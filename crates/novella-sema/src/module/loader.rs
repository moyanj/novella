//! Phase 0 模块装载：worklist 遍历导入闭包，每个文件读入 → 词法 → 语法 → 登记模块。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use novella_ast::program::{ImportItem, Item, Program};
use novella_core::Span;
use novella_core::diagnostic::Diagnostic;
use novella_core::target::RuntimeMode;
use novella_parser::ParserInput;

use super::resolve_path::find_crate_root;
use super::{FileLoader, Module, ModuleId, ResolvedImport, StdManifest, Tier, resolve_import};

/// Phase 0：从入口文件起按 worklist 加载全部可达模块。
///
/// 返回模块表（入口为 id 0）。循环导入安全：路径→id 映射先入队即登记，
/// 二次遇到同一文件直接复用 id；加载中的模块其导入语句在 Phase 2 才绑定，
/// 因此环不会导致无限递归。
pub(crate) fn load_modules(
    loader: &mut dyn FileLoader,
    entry: &Path,
    scopes: &mut crate::symbol::ScopeTree,
    options: &mut crate::AnalysisOptions,
) -> (Vec<Module>, Vec<Diagnostic>, HashMap<PathBuf, ModuleId>) {
    let mut modules: Vec<Module> = Vec::new();
    let mut by_path: HashMap<PathBuf, ModuleId> = HashMap::new();
    let mut global_diagnostics: Vec<Diagnostic> = Vec::new();
    // worklist 元素为 "(调用方路径, 规范化键)"：同一次加载中同一个文件可能先以
    // 相对、后以绝对路径出现，两次的键相同，但只有首次出现决定 `path` 回显值。
    let mut worklist = vec![(entry.to_path_buf(), loader.module_key(entry))];
    // 独立检查裸机依赖模块时继承工程环境，不能因文件未写入口指令而装载宿主标准库。
    let mut no_std = options.module_only && options.build_spec.requires_no_std;
    // `@no_alloc` 只能由入口指令开启；裸机 runtime=none 已在分层判定中拒绝 alloc 层。
    let mut no_alloc = false;
    // 标准库分层清单：决定 @no_std 环境可导入哪些 std: 模块。经 loader 读取，
    // 使清单自动成为构建缓存的输入。
    let (std_manifest, manifest_errors) = match options.std_root.as_deref() {
        Some(root) => {
            let (manifest, errors) = StdManifest::load(loader, root);
            (Some(manifest), errors)
        }
        None => (None, Vec::new()),
    };
    for message in manifest_errors {
        global_diagnostics.push(Diagnostic::error(message));
    }

    while let Some((path, key)) = worklist.pop() {
        if loader.is_cancelled() {
            break;
        }
        if by_path.contains_key(&key) {
            continue;
        }
        let id = modules.len();
        by_path.insert(key.clone(), id);

        // ---- 读文件 ----
        let source = match loader.read_to_string(&path) {
            Ok(source) => source,
            Err(e) => {
                global_diagnostics.push(Diagnostic::error(format!(
                    "无法读取模块文件 '{}'：{e}",
                    path.display()
                )));
                // 占位模块：保留导入目标身份，供导入方抑制读取失败导致的级联诊断
                modules.push(Module {
                    key: key.clone(),
                    path,
                    source: String::new(),
                    line_index: novella_core::line_index::LineIndex::build(""),
                    tokens: vec![],
                    program: Program {
                        items: vec![],
                        span: Span { start: 0, end: 0 },
                    },
                    global_scope: scopes.new_scope(None, crate::symbol::ScopeKind::Module),
                    diagnostics: vec![],
                    syntax_diagnostic_count: 0,
                    syntax_failed: true,
                    load_failed: true,
                    crate_root: None,
                });
                continue;
            }
        };

        // ---- 词法 / 语法分析（全流程唯一一次：下游一律消费本模块产物）----
        let file = path.display().to_string();
        // 缓存命中
        let snapshot = loader
            .parsed_source(&path)
            .filter(|snapshot| snapshot.source == source);
        let (tokens, mut program, diagnostics, syntax_failed) = if let Some(snapshot) = snapshot {
            (
                snapshot.tokens,
                snapshot.program,
                snapshot.diagnostics,
                snapshot.syntax_failed,
            )
        } else {
            let lex = novella_lexer::lexer(&source);
            let mut diagnostics: Vec<Diagnostic> = lex
                .errors
                .iter()
                .map(|error| error.to_diagnostic(Some(&file)))
                .collect();
            let tokens = lex.tokens;
            let mut parsed = novella_parser::Parser::new(ParserInput {
                file: file.clone(),
                tokens: tokens.clone(),
            })
            .parse();
            diagnostics.append(&mut parsed.diagnostics);
            let failed = !diagnostics.is_empty();
            (tokens, parsed.program, diagnostics, failed)
        };
        // 外部缓存可能以编辑器路径装饰诊断；统一为本次模块路径。
        let diagnostics: Vec<_> = diagnostics
            .into_iter()
            .map(|mut diagnostic| {
                if let Some(location) = &mut diagnostic.location {
                    location.file = Some(file.clone());
                }
                diagnostic
            })
            .collect();
        let syntax_diagnostic_count = diagnostics.len();

        // cfg 在声明收集和导入扫描之前生效，关闭的声明不参与名称解析。
        let cfg_errors = crate::cfg::configure(&mut program, &options.cfg);
        // 在入口第一次解析后决定默认环境，避免 @no_std 仍读取标准库文件。
        // `@no_alloc` 比 `@no_std` 更强：隐含 `no_std`，并额外禁止 alloc 层模块。
        let mut directive_errors = Vec::new();
        let mut seen_no_std = false;
        let mut seen_no_alloc = false;
        program.items.retain(|item| {
            let Item::Directive(directive) = item else {
                return true;
            };
            // 只处理入口环境指令；其余指令留给语义检查给出诊断。
            let seen = match directive.name.as_str() {
                "no_std" => &mut seen_no_std,
                "no_alloc" => &mut seen_no_alloc,
                _ => return true,
            };
            let directive_text = format!("@{}", directive.name);
            let error = if id != 0 {
                Some(format!("{directive_text} 只能在入口模块中作为独立指令使用"))
            } else if !directive.tokens.is_empty() {
                Some(format!(
                    "{directive_text} 不接受参数，请使用 {directive_text};"
                ))
            } else if *seen {
                Some(format!("入口模块不能重复声明 {directive_text}"))
            } else {
                *seen = true;
                match directive.name.as_str() {
                    "no_std" => no_std = true,
                    _ => {
                        no_alloc = true;
                        no_std = true;
                    }
                }
                None
            };
            if let Some(message) = error {
                directive_errors.push((directive.span, message));
            }
            // 保留入口标记，供语义产物与 HIR 传递入口模式。
            true
        });
        if id == 0
            && !no_std
            && let Some(std_root) = &options.std_root
        {
            // 预导入 site.nov 模块
            let site = std_root.join("site.nov");
            worklist.push((site.clone(), loader.module_key(&site)));
        }
        let line_index = novella_core::line_index::LineIndex::build(&source);
        let mut module = Module {
            source,
            line_index,
            tokens,
            program,
            key,
            path,
            global_scope: scopes.new_scope(None, crate::symbol::ScopeKind::Module),
            diagnostics,
            syntax_diagnostic_count,
            syntax_failed,
            load_failed: false,
            crate_root: None,
        };

        for (span, message) in cfg_errors {
            module.error(span, message);
        }
        for (span, message) in directive_errors {
            module.error(span, message);
        }
        // ---- 扫描导入语句，扩展 worklist ----
        let imports: Vec<ImportItem> = module
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                novella_ast::program::Item::Import(import) => Some(import.clone()),
                _ => None,
            })
            .collect();
        for import in &imports {
            if no_std
                && import.prefix.as_deref() == Some("std")
                && let Some(message) =
                    std_import_denial(std_manifest.as_ref(), import, options, no_alloc)
            {
                module.error(import.span, message);
                continue;
            }
            // crate 根懒查找（仅 `@:` 需要）；以规范化键为基准，避免相对路径
            // 起点不同导致找不到 nov.toml
            if import.prefix.as_deref() == Some("@") && module.crate_root.is_none() {
                module.crate_root = find_crate_root(loader, &module.key);
            }
            let importer_dir = module
                .key
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            match resolve_import(
                import,
                &importer_dir,
                module.crate_root.as_deref(),
                options.std_root.as_deref(),
                |path| loader.exists(path),
            ) {
                Ok(ResolvedImport::File(target)) => {
                    let target_key = loader.module_key(&target);
                    worklist.push((target, target_key));
                }
                Ok(ResolvedImport::ExceptionBuiltin) => {}
                Err(message) => module.error(import.span, message),
            }
        }
        modules.push(module);
    }

    // 恢复 id 顺序（worklist 为栈，加载顺序与 id 无关，但入口保持 0：
    // 入口先入队，且 by_path 在弹出时登记，故入口必为 id 0）
    (modules, global_diagnostics, by_path)
}

/// `@no_std` / `@no_alloc` 下 `std:` 导入的分层判定；返回 `Some(诊断)` 表示不允许导入。
fn std_import_denial(
    manifest: Option<&StdManifest>,
    import: &ImportItem,
    options: &crate::AnalysisOptions,
    no_alloc: bool,
) -> Option<String> {
    // 诊断文本指出实际生效的那条指令：`@no_alloc` 隐含 `@no_std`，但更严格。
    let env = if no_alloc { "@no_alloc" } else { "@no_std" };
    let allowed = if no_alloc { "core" } else { "core / alloc" };
    // 编译器内建的 Exception 根不属于标准库分层体系，维持既有拒绝语义。
    if import.path == "exception" {
        return Some(format!("{env} 环境不能导入标准库模块 `std:exception`"));
    }
    let Some(manifest) = manifest else {
        return Some(format!(
            "{env} 环境需要标准库清单判断 `std:{}` 的层级，但 std_root 下缺少 modules.toml",
            import.path
        ));
    };
    if !manifest.present() {
        return Some(format!(
            "{env} 环境需要标准库清单判断 `std:{}` 的层级，但 std_root 下缺少 modules.toml",
            import.path
        ));
    }
    let Some(info) = manifest.module(&import.path) else {
        return Some(format!(
            "标准库清单未声明模块 `std:{}`，{env} 环境不能导入未分层的模块",
            import.path
        ));
    };
    match info.tier() {
        Tier::Core => {}
        // `@no_alloc` 不接受任何分配器依赖，运行时是否托管都不放行。
        Tier::Alloc if no_alloc => {
            return Some(format!(
                "`std:{}` 属于 alloc 层，{env} 环境只能导入 {allowed} 层模块",
                import.path
            ));
        }
        Tier::Alloc if options.build_target.runtime != RuntimeMode::None => {}
        Tier::Alloc => {
            return Some(format!(
                "`std:{}` 属于 alloc 层，{env} 环境需要托管运行时（--runtime managed）",
                import.path
            ));
        }
        Tier::Std => {
            return Some(format!(
                "`std:{}` 属于 {} 层，{env} 环境只能导入 {allowed} 层模块",
                import.path,
                Tier::Std.name()
            ));
        }
    }
    if info.requires_float() && !options.build_spec.has_float {
        return Some(format!(
            "`std:{}` 需要支持浮点的目标，当前目标不支持浮点",
            import.path
        ));
    }
    None
}

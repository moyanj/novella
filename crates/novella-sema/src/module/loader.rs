//! Phase 0 模块装载：worklist 遍历导入闭包，每个文件读入 → 词法 → 语法 → 登记模块。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use novella_ast::program::{ImportItem, Program};
use novella_core::Span;
use novella_core::diagnostic::Diagnostic;
use novella_core::line_index::LineIndex;
use novella_core::path::module_key;
use novella_core::token::TokenKind;
use novella_parser::ParserInput;

use super::resolve_path::find_crate_root;
use super::{FileLoader, Module, ModuleId, ResolvedImport, resolve_import};

/// Phase 0：从入口文件起按 worklist 加载全部可达模块。
///
/// 返回模块表（入口为 id 0）。循环导入安全：路径→id 映射先入队即登记，
/// 二次遇到同一文件直接复用 id；加载中的模块其导入语句在 Phase 2 才绑定，
/// 因此环不会导致无限递归。
pub(crate) fn load_modules(
    loader: &mut dyn FileLoader,
    entry: &Path,
    scopes: &mut crate::symbol::ScopeTree,
) -> (Vec<Module>, Vec<Diagnostic>, HashMap<PathBuf, ModuleId>) {
    let mut modules: Vec<Module> = Vec::new();
    let mut by_path: HashMap<PathBuf, ModuleId> = HashMap::new();
    let mut global_diagnostics: Vec<Diagnostic> = Vec::new();
    let mut worklist: Vec<PathBuf> = vec![entry.to_path_buf()];

    while let Some(path) = worklist.pop() {
        let key = module_key(&path);
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
                // 占位模块：保证 by_path 与 modules 下标一致，导入方在绑定阶段报「未找到」
                modules.push(Module {
                    path: key,
                    source: String::new(),
                    program: Program {
                        items: vec![],
                        span: Span { start: 0, end: 0 },
                    },
                    lines: LineIndex::build(""),
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

        // ---- 词法 / 语法分析（CLI 流程的内联复刻：过滤注释后喂给 parser）----
        let lex = novella_lexer::lexer(&source);
        let mut diagnostics: Vec<Diagnostic> = lex
            .errors
            .iter()
            .map(|e| {
                let mut d = e.to_diagnostic(Some(&path.display().to_string()));
                if let Some(loc) = &mut d.location {
                    loc.file = Some(path.display().to_string());
                }
                d
            })
            .collect();
        let tokens: Vec<_> = lex
            .tokens
            .into_iter()
            .filter(|t| t.kind != TokenKind::LineComment)
            .collect();
        let mut parse_result = novella_parser::Parser::new(ParserInput {
            file: path.to_string_lossy().to_string(),
            tokens,
        })
        .parse();
        for diag in &mut parse_result.diagnostics {
            if let Some(loc) = &mut diag.location {
                loc.file = Some(path.display().to_string());
            }
        }
        let syntax_failed = !diagnostics.is_empty() || !parse_result.diagnostics.is_empty();
        diagnostics.append(&mut parse_result.diagnostics);
        let syntax_diagnostic_count = diagnostics.len();

        let mut module = Module {
            lines: LineIndex::build(&source),
            source,
            program: parse_result.program,
            path: key,
            global_scope: scopes.new_scope(None, crate::symbol::ScopeKind::Module),
            diagnostics,
            syntax_diagnostic_count,
            syntax_failed,
            load_failed: false,
            crate_root: None,
        };

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
            // crate 根懒查找（仅 `@:` 需要）
            if import.prefix.as_deref() == Some("@") && module.crate_root.is_none() {
                module.crate_root = find_crate_root(loader, &module.path);
            }
            let importer_dir = module
                .path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            match resolve_import(import, &importer_dir, module.crate_root.as_deref()) {
                Ok(ResolvedImport::File(target)) => worklist.push(target),
                Ok(ResolvedImport::StdExternal) => {}
                Err(message) => module.error(import.span, message),
            }
        }
        modules.push(module);
    }

    // 恢复 id 顺序（worklist 为栈，加载顺序与 id 无关，但入口保持 0：
    // 入口先入队，且 by_path 在弹出时登记，故入口必为 id 0）
    (modules, global_diagnostics, by_path)
}

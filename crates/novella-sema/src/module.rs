//! 模块加载：模块 = 文件，导入路径 → 文件路径映射，worklist 队列保证
//! 循环导入安全（对应 `docs/sema-phase1.md` §1.1–§1.2）。
//!
//! 路径规则（design.md §4.3.1 + 本期修订）：
//! - `:` 仅作**别名分隔符**且最多一个：`"@:sub/mod"` → 前缀 `@` + 路径 `sub/mod`；
//!   路径内再出现 `:` 报「路径段分隔符只支持 `/`」；
//! - 无前缀：相对当前文件所在目录；`@`：crate 根（最近的 `nov.toml` 所在目录）；
//!   `super`：当前文件目录的父目录；`std`：外部符号 stub（库尚未落地）；
//! - 段分隔只认 `/`；空段报「非法模块路径」。

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use novella_ast::program::{ImportItem, Program};
use novella_core::diagnostic::{Diagnostic, DiagnosticLocation};
use novella_core::{Span, token::TokenKind};

/// 模块标识（[`crate::Sema::modules`] 内下标）。
pub type ModuleId = usize;

/// 文件读取抽象（测试可注入内存文件系统）。
pub trait FileLoader {
    fn read_to_string(&mut self, path: &Path) -> io::Result<String>;
    /// 路径是否存在（nov.toml 查找用）
    fn exists(&mut self, path: &Path) -> bool;
}

/// 基于真实文件系统的加载器。
pub struct FsLoader;

impl FileLoader for FsLoader {
    fn read_to_string(&mut self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn exists(&mut self, path: &Path) -> bool {
        path.exists()
    }
}

/// 行号索引：字节偏移 → (1-based 行号, 0-based 行内字节列)。
///
/// 与 lexer 内部实现同源（sema 不依赖 lexer 私有类型，独立维护一份）。
#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn build(input: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in input.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }

    /// 定位偏移所在的行列：`line` 1-based，`column` 为行内字节偏移（0-based）。
    pub fn locate(&self, offset: usize) -> (usize, usize) {
        let idx = self.starts.partition_point(|&s| s <= offset) - 1;
        (idx + 1, offset - self.starts[idx])
    }
}

/// 一个已加载并解析的模块。
pub struct Module {
    /// 规范化后的绝对路径（模块唯一键）
    pub path: PathBuf,
    pub source: String,
    pub program: Program,
    pub lines: LineIndex,
    /// 模块全局作用域（声明收集阶段建立）
    pub global_scope: crate::symbol::ScopeId,
    /// 本模块产出的诊断（词法 / 语法 / 语义，按产生顺序）
    pub diagnostics: Vec<Diagnostic>,
    /// `diagnostics` 前缀中词法 / 语法诊断的条数；其后的才是语义诊断
    ///（CLI 借此只渲染语义部分，避免与自身的 lexer/parser 输出重复）
    pub syntax_diagnostic_count: usize,
    /// 词法或语法阶段已有错误：语义检查跳过该模块（避免误导性级联报错），
    /// 其导入方在导入语句处收到提示
    pub syntax_failed: bool,
    /// 文件读取失败的占位模块（错误已在装载时报告，导入绑定阶段不再重复提示）
    pub load_failed: bool,
    /// crate 根（nov.toml 所在目录），`@:` 解析时懒计算并缓存
    pub crate_root: Option<PathBuf>,
}

impl Module {
    /// 在全局作用域内查找符号（不沿父链——全局作用域即根）。
    pub fn global_scope_symbol<'a>(
        &self,
        scopes: &'a crate::symbol::ScopeTree,
        name: &str,
    ) -> Option<&'a crate::symbol::Symbol> {
        scopes.lookup_local(self.global_scope, name)
    }

    /// 在指定 span 处追加一条 error 诊断（行列由本模块行索引回推）。
    pub fn error(&mut self, span: Span, message: impl Into<String>) {
        let (line, column) = self.lines.locate(span.start);
        self.diagnostics.push(
            Diagnostic::error(message).with_location(DiagnosticLocation {
                file: Some(self.path.display().to_string()),
                line,
                column,
                span: Some(span),
            }),
        );
    }

    /// 在指定 span 处追加一条 warning 诊断。
    pub fn warning(&mut self, span: Span, message: impl Into<String>) {
        let (line, column) = self.lines.locate(span.start);
        self.diagnostics.push(
            Diagnostic::warning(message).with_location(DiagnosticLocation {
                file: Some(self.path.display().to_string()),
                line,
                column,
                span: Some(span),
            }),
        );
    }
}

/// 词法消除路径中的 `.` 与 `..`（不触盘；`super:` 导入的路径含 `..`，
/// 必须先归一化才能作为模块唯一键）。
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // 无法继续上溯时（相对路径开头）保留 `..`
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 模块唯一键：优先 `fs::canonicalize`（解析符号链接），失败退回词法归一化。
fn module_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

/// 从 `file` 所在目录向上查找最近的 `nov.toml`，返回其所在目录（crate 根）。
fn find_crate_root(loader: &mut dyn FileLoader, file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if loader.exists(&dir.join("nov.toml")) {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// 导入路径解析结果。
pub enum ResolvedImport {
    /// 解析到文件路径（待加载 / 已加载模块）
    File(PathBuf),
    /// `std:` 前缀——外部符号 stub
    StdExternal,
}

/// 导入绑定阶段的解析结果（文件路径已映射为模块 id）。
pub enum NsResolution {
    Module(ModuleId),
    StdExternal,
}

/// 解析导入语句到目标模块（加载阶段完成后调用；路径→id 映射见
/// [`crate::Sema::module_by_path`]）。错误与加载阶段同源，调用方一般忽略
/// （诊断已在加载阶段报出）。
pub fn resolve_import_path(
    sema: &crate::Sema,
    module: ModuleId,
    import: &ImportItem,
) -> Result<NsResolution, String> {
    let importer = &sema.modules[module];
    let dir = importer
        .path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    match resolve_import(import, &dir, importer.crate_root.as_deref())? {
        ResolvedImport::File(path) => {
            let key = module_key(&path);
            sema.module_by_path
                .get(&key)
                .copied()
                .map(NsResolution::Module)
                .ok_or_else(|| format!("模块文件未加载：{}", path.display()))
        }
        ResolvedImport::StdExternal => Ok(NsResolution::StdExternal),
    }
}

/// 将导入语句映射到目标（纯路径运算，不触发加载）。
///
/// `crate_root_of` 返回导入方模块的 crate 根（懒查找 nov.toml）。
pub fn resolve_import(
    import: &ImportItem,
    importer_dir: &Path,
    crate_root: Option<&Path>,
) -> Result<ResolvedImport, String> {
    // `:` 仅作别名分隔符且最多一个：parser 已在第一个 `:` 处切分，
    // path 内残留的 `:` 即第二个分隔符
    if import.path.contains(':') {
        return Err("路径段分隔符只支持 `/`（`:` 仅作别名分隔符，最多一个）".to_string());
    }
    let segments: Vec<&str> = import.path.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(format!("非法模块路径 \"{}\"（含空段）", import.raw_path));
    }
    let mut base = match import.prefix.as_deref() {
        None => importer_dir.to_path_buf(),
        Some("@") => match crate_root {
            Some(root) => root.to_path_buf(),
            None => return Err("未找到 nov.toml（crate 根），无法解析 `@:` 导入".to_string()),
        },
        Some("super") => match importer_dir.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return Err("`super:` 导入已超出文件系统根".to_string()),
        },
        Some("std") => return Ok(ResolvedImport::StdExternal),
        Some(other) => return Err(format!("未知路径别名 `{other}`")),
    };
    for segment in &segments {
        base.push(segment);
    }
    base.set_extension("nov");
    Ok(ResolvedImport::File(base))
}

/// Phase 0：从入口文件起按 worklist 加载全部可达模块。
///
/// 返回模块表（入口为 id 0）。循环导入安全：路径→id 映射先入队即登记，
/// 二次遇到同一文件直接复用 id；加载中的模块其导入语句在 Phase 2 才绑定，
/// 因此环不会导致无限递归。
pub fn load_modules(
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
        let mut parse_result = novella_parser::Parser::new(tokens).parse();
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

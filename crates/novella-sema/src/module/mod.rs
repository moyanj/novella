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
use std::path::{Path, PathBuf};

use novella_ast::program::Program;
use novella_core::Span;
use novella_core::diagnostic::{Diagnostic, DiagnosticCode, DiagnosticLocation};
use novella_core::line_index::LineIndex;
use novella_core::path::module_key;
use novella_core::token::Token;

/// 模块标识：语义分析结果中模块列表（[`crate::DiagnosticReport::modules`]）的下标。
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

/// 一个已加载并解析的模块。
pub(crate) struct Module {
    /// 模块唯一键（[`module_key`] 规范化结果），模块去重与路径→id 映射使用。
    ///
    /// 与 [`Module::path`] 分离的原因：`path` 是调用方传入的原始路径，
    /// 诊断 / 产物视图对外回显它；去重却必须用规范化绝对路径（符号链接、
    /// `..`、相对路径都要收敛到同一个键）。
    pub(crate) key: PathBuf,
    /// 调用方传入的原始路径（对外的文件标识；诊断 `location.file` 用它）
    pub(crate) path: PathBuf,
    /// 源码
    pub(crate) source: String,
    /// 词法产物：未经注释过滤的完整 token 流（JSON 输出 / 高亮等下游消费）
    pub(crate) tokens: Vec<Token>,
    /// 抽象语法树
    pub(crate) program: Program,
    /// 源码行索引
    pub(crate) lines: LineIndex,
    /// 模块全局作用域（声明收集阶段建立）
    pub(crate) global_scope: crate::symbol::ScopeId,
    /// 本模块产出的诊断（词法 / 语法 / 语义，按产生顺序）
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// `diagnostics` 前缀中词法 / 语法诊断的条数；其后的才是语义诊断
    ///（CLI 借此只渲染语义部分，避免与自身的 lexer/parser 输出重复）
    pub(crate) syntax_diagnostic_count: usize,
    /// 词法或语法阶段已有错误：语义检查跳过该模块（避免误导性级联报错），
    /// 其导入方在导入语句处收到提示
    pub(crate) syntax_failed: bool,
    /// 文件读取失败的占位模块（错误已在装载时报告，导入绑定阶段不再重复提示）
    pub(crate) load_failed: bool,
    /// crate 根（nov.toml 所在目录），`@:` 解析时懒计算并缓存
    pub(crate) crate_root: Option<PathBuf>,
}

impl Module {
    /// 在全局作用域内查找符号（不沿父链——全局作用域即根）。
    pub(crate) fn global_scope_symbol<'a>(
        &self,
        scopes: &'a crate::symbol::ScopeTree,
        name: &str,
    ) -> Option<&'a crate::symbol::Symbol> {
        scopes.lookup_local(self.global_scope, name)
    }

    /// 在指定 span 处追加一条 error 诊断（行列由本模块行索引回推）。
    pub(crate) fn error(&mut self, span: Span, message: impl Into<String>) {
        self.error_with_code(span, DiagnosticCode::SemanticError, message);
    }

    /// 追加一条带指定错误码的 error 诊断。
    ///
    /// 错误码供编辑器做快速修复与稳定匹配（见 [`DiagnosticCode`]）；阶段内能
    /// 确定归属（名称 / 类型 / 声明 / unsafe）时应显式传入，否则用
    /// [`DiagnosticCode::SemanticError`]。
    pub(crate) fn error_with_code(
        &mut self,
        span: Span,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        let diagnostic = self.decorated(span, Diagnostic::error(message).with_code(code));
        self.diagnostics.push(diagnostic);
    }

    /// 在指定 span 处追加一条 warning 诊断。
    pub(crate) fn warning(&mut self, span: Span, message: impl Into<String>) {
        let diagnostic = self.decorated(
            span,
            Diagnostic::warning(message).with_code(DiagnosticCode::SemanticError),
        );
        self.diagnostics.push(diagnostic);
    }

    /// 把「仅有 span」的诊断补齐成本模块的行列与文件路径。
    ///
    /// 行列换算统一走 [`novella_core::decor::decorate`]，本模块只提供上下文
    /// （路径 + 源码 + 行索引），避免各阶段重复实现 span → 行/列。
    fn decorated(&self, span: Span, diagnostic: Diagnostic) -> Diagnostic {
        let located = diagnostic.with_location(DiagnosticLocation {
            file: None,
            line: 0,
            column: 0,
            span: Some(span),
        });
        novella_core::decor::decorate_one(
            located,
            Some(self.path.to_string_lossy().as_ref()),
            &self.source,
        )
    }
}

/// 叠加内存缓冲的文件加载器（C2）。
///
/// 命中 overlay 的路径直接返回内存文本，其余回落磁盘——供 LSP 把
/// 「编辑器里打开但未保存的文档」喂给语义分析：打开文档以缓冲为准
/// （与诊断推送的文本一致），未打开的依赖模块仍从磁盘读。
///
/// 键的规范化与模块加载一致（[`module_key`]）：构造与查询两侧都做，
/// 因此符号链接 / `..` 等写法不会绕过 overlay。
pub struct OverlayLoader {
    overlay: HashMap<PathBuf, String>,
}

impl OverlayLoader {
    /// 以「路径 → 内存文本」构造（路径在构造时规范化）。
    pub fn new(overlay: HashMap<PathBuf, String>) -> Self {
        let overlay = overlay
            .into_iter()
            .map(|(path, text)| (module_key(&path), text))
            .collect();
        OverlayLoader { overlay }
    }
}

impl FileLoader for OverlayLoader {
    fn read_to_string(&mut self, path: &Path) -> io::Result<String> {
        if let Some(text) = self.overlay.get(&module_key(path)) {
            return Ok(text.clone());
        }
        std::fs::read_to_string(path)
    }

    fn exists(&mut self, path: &Path) -> bool {
        self.overlay.contains_key(&module_key(path)) || path.exists()
    }
}

mod loader;
mod resolve_path;

pub(crate) use loader::load_modules;
pub(crate) use resolve_path::{NsResolution, ResolvedImport, resolve_import, resolve_import_path};

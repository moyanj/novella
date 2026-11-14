//! novella 语义分析（sema 第一期）。
//!
//! 实施范围见 `docs/sema-phase1.md`：完整导入系统（跨文件加载、路径别名
//! `@` / `std` / `super`、nov.toml crate 根标记、循环导入安全）、类型别名、
//! 全程序名称解析、基本类型检查、声明级基础检查与 extern 函数规则。
//!
//! 管线（三遍，对应计划 §1.2）：
//! - Phase 0 [`module::load_modules`]：worklist 加载全部可达模块（循环导入安全）；
//! - Phase 1 [`resolve::collect_module`]：逐模块收集顶层声明到全局作用域；
//! - Phase 2a [`resolve::bind_imports`]：绑定导入符号（具名 / 命名空间）；
//! - Phase 2b [`check::build_type_defs`]：解析字段 / 方法签名建类型注册表；
//! - Phase 2c [`check::resolve_class_parents`]：解析 class 继承链；
//! - Phase 2d [`check::check_module`]：声明级检查 + 函数体 / 顶层语句类型检查。

mod check;
mod module;
mod resolve;
mod symbol;
mod ty;
mod typecheck;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use novella_core::Span;
use novella_core::diagnostic::{Diagnostic, DiagnosticCode, Severity};

pub use module::{FileLoader, FsLoader, ModuleId, OverlayLoader, module_key};

use module::Module;
use symbol::{ScopeTree, Symbol, SymbolKind};
use ty::TypeDef;

/// 单个模块的诊断集合。
pub struct ModuleDiagnostics {
    pub path: PathBuf,
    /// 全部诊断（词法 / 语法 / 语义，按产生顺序）
    pub diagnostics: Vec<Diagnostic>,
    /// 前缀中词法 / 语法诊断的条数；`diagnostics[此值..]` 即纯语义诊断
    pub syntax_diagnostic_count: usize,
}

impl ModuleDiagnostics {
    /// 语义阶段产生的诊断切片（不含词法 / 语法诊断）。
    pub fn semantic_diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics[self.syntax_diagnostic_count..]
    }
}

/// 语义分析结果。
pub struct SemaResult {
    /// 已加载模块的诊断（含词法 / 语法 / 语义，入口为第一个）
    pub modules: Vec<ModuleDiagnostics>,
    /// 全局诊断（入口读取失败等无模块归属的错误）
    pub diagnostics: Vec<Diagnostic>,
    /// 引用 → 定义绑定表：键为引用处的 `(模块 id, span)`，值为定义位置。
    ///
    /// 覆盖值标识符、类型名（含 `new` / struct 初始化）、`ns.成员` 限定访问；
    /// 成员访问（`a.b`）与 import 语句内的名字不在表内（成员定义无 span，
    /// 属已知限制）。具名导入的引用记录的是目标模块的原始定义而非本地绑定。
    pub references: HashMap<(ModuleId, Span), Definition>,
    /// 具名类型摘要（class 继承链，供类型层级查询）
    pub types: Vec<TypeSummary>,
}

/// 定义位置（[`SemaResult::references`] 的值）。
///
/// 故意不暴露内部 [`Symbol`]：LSP 只需要名字、位置与粗粒度种类，
/// 符号表内部结构（作用域 / 可见性）不属于公共 API。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Definition {
    /// 符号名
    pub name: String,
    /// 定义所在模块
    pub module: ModuleId,
    /// 声明处 span
    pub span: Span,
    /// 粗粒度种类
    pub kind: DefinitionKind,
}

/// 定义的粗粒度种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefinitionKind {
    Function,
    Type,
    Variable,
    Parameter,
    /// import 绑定（无法追到目标定义时保留）
    Import,
}

impl Definition {
    fn from_symbol(symbol: &Symbol) -> Self {
        let kind = match &symbol.kind {
            SymbolKind::Function { .. } => DefinitionKind::Function,
            SymbolKind::Type => DefinitionKind::Type,
            SymbolKind::Variable { .. } => DefinitionKind::Variable,
            SymbolKind::Param { .. } => DefinitionKind::Parameter,
            SymbolKind::NamespaceImport { .. } | SymbolKind::NamedImport { .. } => {
                DefinitionKind::Import
            }
        };
        Definition {
            name: symbol.name.clone(),
            module: symbol.module,
            span: symbol.span,
            kind,
        }
    }
}

/// 具名类型摘要（class 继承链）。
#[derive(Debug, Clone)]
pub struct TypeSummary {
    /// 定义所在模块
    pub module: ModuleId,
    /// 类型名
    pub name: String,
    pub kind: TypeSummaryKind,
    /// 声明处 span（取自模块全局符号；取不到时为 `(0, 0)`）
    pub span: Span,
    /// 解析后的父类（仅 class 可能有）
    pub parent: Option<(ModuleId, String)>,
}

/// 类型摘要的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeSummaryKind {
    Struct,
    Class,
    Trait,
    Alias,
}

impl SemaResult {
    /// 是否存在 error 级诊断。
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .chain(self.modules.iter().flat_map(|m| m.diagnostics.iter()))
            .any(|d| d.severity == Severity::Error)
    }
}

/// 以真实文件系统分析入口文件及其全部导入闭包。
pub fn analyze(entry: &Path) -> SemaResult {
    analyze_with(&mut FsLoader, entry)
}

/// 以指定文件加载器分析（测试可注入内存文件系统）。
pub fn analyze_with(loader: &mut dyn FileLoader, entry: &Path) -> SemaResult {
    let mut sema = Sema::new();
    // Phase 0：模块加载（含词法 / 语法分析）
    let (modules, global_diagnostics, module_by_path) =
        module::load_modules(loader, entry, &mut sema.scopes);
    sema.modules = modules;
    sema.global_diagnostics = global_diagnostics;
    sema.module_by_path = module_by_path;

    // Phase 1：全模块顶层声明收集（先于导入绑定，保证跨模块引用可见）
    for id in 0..sema.modules.len() {
        resolve::collect_module(&mut sema, id);
    }
    // Phase 2a：绑定导入符号
    for id in 0..sema.modules.len() {
        resolve::bind_imports(&mut sema, id);
    }
    // Phase 2b：构建类型注册表（字段 / 方法签名）
    for id in 0..sema.modules.len() {
        check::build_type_defs(&mut sema, id);
    }
    // Phase 2c：解析 class 继承链
    check::resolve_class_parents(&mut sema);
    // Phase 2d：声明级检查 + 函数体 / 顶层语句类型检查
    for id in 0..sema.modules.len() {
        check::check_module(&mut sema, id);
    }

    let types = sema.type_summaries();
    SemaResult {
        modules: sema
            .modules
            .into_iter()
            .map(|m| ModuleDiagnostics {
                path: m.path,
                diagnostics: m.diagnostics,
                syntax_diagnostic_count: m.syntax_diagnostic_count,
            })
            .collect(),
        diagnostics: sema.global_diagnostics,
        references: sema.references,
        types,
    }
}

/// 语义分析共享状态。
pub(crate) struct Sema {
    pub modules: Vec<Module>,
    pub scopes: ScopeTree,
    /// 类型定义注册表：`(模块, 类型名) -> 定义实体`
    pub type_defs: HashMap<(ModuleId, String), TypeDef>,
    /// 类型别名展开中的集合（循环检测）
    pub resolving_aliases: HashSet<(ModuleId, String)>,
    /// 规范化路径 → 模块 id（导入绑定阶段解析目标模块）
    pub module_by_path: HashMap<PathBuf, ModuleId>,
    pub global_diagnostics: Vec<Diagnostic>,
    /// 引用 → 定义绑定表（键为引用处的 `(模块, span)`）
    pub references: HashMap<(ModuleId, Span), Definition>,
}

impl Sema {
    fn new() -> Self {
        Sema {
            modules: Vec::new(),
            scopes: ScopeTree::default(),
            type_defs: HashMap::new(),
            resolving_aliases: HashSet::new(),
            module_by_path: HashMap::new(),
            global_diagnostics: Vec::new(),
            references: HashMap::new(),
        }
    }

    /// 记录一次「引用 → 定义」绑定。
    ///
    /// 具名导入符号先追到目标模块的原始定义（跳转直达源头而非 import 行）；
    /// 追不到（目标未导出等，诊断已在绑定阶段报过）时退而记录本地绑定。
    pub(crate) fn record_reference(&mut self, module: ModuleId, span: Span, symbol: &Symbol) {
        // 零长 span 来自占位 / 合成节点，记进引用表会让任意光标偏移误命中
        if span.start >= span.end {
            return;
        }
        let chased = match &symbol.kind {
            SymbolKind::NamedImport {
                target: Some(target),
                imported,
            } => self.global_scope_symbol(*target, imported),
            _ => None,
        };
        let symbol = chased.as_ref().unwrap_or(symbol);
        self.references
            .insert((module, span), Definition::from_symbol(symbol));
    }

    /// 汇总类型注册表为层级摘要（声明 span 取自模块全局符号）。
    fn type_summaries(&self) -> Vec<TypeSummary> {
        let mut summaries = Vec::with_capacity(self.type_defs.len());
        for ((module, name), def) in &self.type_defs {
            let (kind, parent) = match def {
                TypeDef::Struct(_) => (TypeSummaryKind::Struct, None),
                TypeDef::Class(class) => (TypeSummaryKind::Class, class.parent_resolved.clone()),
                TypeDef::Trait(_) => (TypeSummaryKind::Trait, None),
                TypeDef::Alias(_) => (TypeSummaryKind::Alias, None),
            };
            let span = self.modules[*module]
                .global_scope_symbol(&self.scopes, name)
                .map(|symbol| symbol.span)
                .unwrap_or(Span { start: 0, end: 0 });
            summaries.push(TypeSummary {
                module: *module,
                name: name.clone(),
                kind,
                span,
                parent,
            });
        }
        summaries
    }

    /// 在指定模块的 span 处追加 error 诊断（错误码取 [`DiagnosticCode::NameError`]，
    /// 即名称解析 / 导入绑定阶段的语义错误）。
    ///
    /// 类型检查与声明检查等阶段应改用 [`Sema::type_error`] / [`Sema::decl_error`] /
    /// [`Sema::unsafe_error`]，以便编辑器按错误码做快速修复。
    pub fn error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::NameError, message);
    }

    /// 在指定模块的 span 处追加带错误码的 error 诊断。
    pub fn error_with_code(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        self.modules[module].error_with_code(span, code, message);
    }

    /// 类型检查错误（`NVL3002`）。
    pub fn type_error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::TypeError, message);
    }

    /// 声明级检查错误（`NVL3003`）。
    pub fn decl_error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::DeclarationError, message);
    }

    /// unsafe 上下文规则错误（`NVL3004`）。
    pub fn unsafe_error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::UnsafeError, message);
    }

    /// 在指定模块的 span 处追加 warning 诊断。
    pub fn warning(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.modules[module].warning(span, message);
    }

    /// 取目标模块的全局符号（克隆返回，便于跨模块借用）。
    pub(crate) fn global_scope_symbol(
        &self,
        module: ModuleId,
        name: &str,
    ) -> Option<crate::symbol::Symbol> {
        self.modules
            .get(module)?
            .global_scope_symbol(&self.scopes, name)
            .cloned()
    }

    /// class 继承链上的 `is_subclass(ancestor, derived)` 判定（含自身）。
    pub fn is_subclass(&self, ancestor: &ty::Ty, derived: &ty::Ty) -> bool {
        let (ty::Ty::Class(am, an), ty::Ty::Class(derived_module, derived_name)) =
            (ancestor, derived)
        else {
            return false;
        };
        let (mut module, mut name) = (*derived_module, derived_name.clone());
        if *am == module && *an == name {
            return true;
        }
        let mut visited = HashSet::new();
        loop {
            if !visited.insert((module, name.clone())) {
                return false; // 继承环（已在 resolve_class_parents 报过）
            }
            match self.type_defs.get(&(module, name.clone())) {
                Some(TypeDef::Class(def)) => match &def.parent_resolved {
                    Some((pm, pn)) => {
                        module = *pm;
                        name = pn.clone();
                        if *am == module && *an == name {
                            return true;
                        }
                    }
                    None => return false,
                },
                _ => return false,
            }
        }
    }

    /// `target ← source` 可赋值性（注入继承链判定）。
    pub fn is_assignable(&self, target: &ty::Ty, source: &ty::Ty) -> bool {
        let mut is_subclass = |a: &ty::Ty, b: &ty::Ty| self.is_subclass(a, b);
        ty::assignable(target, source, &mut is_subclass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OverlayLoader 的内存文本参与分析：引用表记录跨模块调用与类型引用，
    /// 类型摘要记录 class 继承链（B1/B3/C2 的联合验证）。
    #[test]
    fn references_and_type_summaries_are_collected() {
        let dir = PathBuf::from("/novella-mem-test");
        let main = dir.join("main.nov");
        let helper = dir.join("helper.nov");
        let mut overlay = HashMap::new();
        overlay.insert(
            helper.clone(),
            "pub func f() -> int {\n    return 1;\n}\n".to_owned(),
        );
        overlay.insert(
            main.clone(),
            "import \"helper\";\nclass Base {}\nclass Shape(Base) {}\nfunc main() {\n    let x = helper.f();\n    let b = new Base();\n}\n"
                .to_owned(),
        );
        let mut loader = OverlayLoader::new(overlay);
        let result = analyze_with(&mut loader, &main);
        assert!(
            !result.has_errors(),
            "overlay 分析应无错误：{:?}",
            result
                .modules
                .iter()
                .flat_map(|m| m.diagnostics.iter().map(|d| &d.message))
                .collect::<Vec<_>>()
        );

        // 类型摘要：Base / Shape 都在，Shape 的父类解析为 Base
        let shape = result
            .types
            .iter()
            .find(|t| t.name == "Shape")
            .expect("Shape 应在类型摘要中");
        assert_eq!(shape.kind, TypeSummaryKind::Class);
        let parent = shape.parent.as_ref().expect("Shape 有父类");
        assert_eq!(parent.1, "Base");
        assert_eq!(parent.0, shape.module, "同模块继承");

        // 引用表：class 声明里的父类名 Base、`Base()` 构造、`helper.f()` 成员访问
        assert!(
            result.references.values().any(|d| d.name == "Base"),
            "应记录 Base 的类型引用：{:?}",
            result.references
        );
        assert!(
            result
                .references
                .values()
                .any(|d| d.name == "f" && d.kind == DefinitionKind::Function),
            "应记录 helper.f 的函数引用"
        );
        // 定义均落在声明模块内（helper.f 定义在 helper 模块）
        let f_def = result
            .references
            .values()
            .find(|d| d.name == "f")
            .expect("f 的引用");
        assert_eq!(result.modules[f_def.module].path, module::module_key(&helper));
    }
}

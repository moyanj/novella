//! novella 语义分析（sema 第一期）。
//!
//! 实施范围见 `docs/sema-phase1.md`：完整导入系统（跨文件加载、路径别名
//! `@` / `std` / `super`、nov.toml crate 根标记、循环导入安全）、类型别名、
//! 全程序名称解析、基本类型检查、声明级基础检查与 extern 函数规则。
//!
//! 管线（三遍，对应计划 §1.2；各阶段为内部实现，见 `module` / `resolve` /
//! `check` 模块）：
//! - Phase 0 `module::load_modules`：worklist 加载全部可达模块（循环导入安全）；
//! - Phase 1 `resolve::collect_module`：逐模块收集顶层声明到全局作用域；
//! - Phase 2a `resolve::bind_imports`：绑定导入符号（具名 / 命名空间）；
//! - Phase 2b `check::build_type_defs`：解析字段 / 方法签名建类型注册表；
//! - Phase 2c `check::resolve_class_parents`：解析 class 继承链；
//! - Phase 2d `check::check_module`：声明级检查 + 函数体 / 顶层语句类型检查。
//!
//! 公开面只有三个输入 / 输出类型族：
//! - 入口：[`analyze`] / [`analyze_with`]，加载器 [`FileLoader`] / [`FsLoader`] /
//!   [`OverlayLoader`]；
//! - 结果：[`SemaResult`]，由 [`DiagnosticReport`] / [`BindingTable`] /
//!   [`TypeHierarchy`] / [`ModuleOutput`] 四个只读视图组成；
//! - 视图内的值类型：[`Definition`] / [`DefinitionKind`] / [`TypeSummary`] /
//!   [`TypeSummaryKind`] / [`ModuleDiagnostics`] / [`AnalyzedModule`]。
//!
//! **词法 / 语法只在 `analyze` 内执行一次**：`SemaResult.modules` 给出每个模块
//! 的完整产物（源码、token 流、AST、词法语法诊断），下游（CLI / codegen）直接
//! 消费，不得自行再跑 lexer / parser。
//!
//! 其余（符号表、作用域树、类型注册表、检查函数）一律 `pub(crate)`。

mod check;
mod module;
mod resolve;
mod symbol;
mod ty;
mod typecheck;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use novella_ast::program::Program;
use novella_core::Span;
use novella_core::diagnostic::{Diagnostic, DiagnosticCode, Severity};
use novella_core::token::Token;

pub use module::{FileLoader, FsLoader, ModuleId, OverlayLoader};
pub use novella_core::path::module_key;

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

/// 诊断输出（[`SemaResult::diagnostics`]）。
///
/// 词法 / 语法 / 语义诊断分两层：`modules` 按模块归组，`global` 收集无模块
/// 归属的错误（入口文件读取失败等）。
pub struct DiagnosticReport {
    /// 已加载模块的诊断（含词法 / 语法 / 语义，入口为第一个）
    pub modules: Vec<ModuleDiagnostics>,
    /// 全局诊断（入口读取失败等无模块归属的错误）
    pub global: Vec<Diagnostic>,
}

impl DiagnosticReport {
    /// 是否存在 error 级诊断。
    pub fn has_errors(&self) -> bool {
        self.global
            .iter()
            .chain(self.modules.iter().flat_map(|m| m.diagnostics.iter()))
            .any(|d| d.severity == Severity::Error)
    }
}

/// 引用绑定表（[`SemaResult::bindings`]）：键为引用处的 `(模块 id, span)`，
/// 值为定义位置。
///
/// 覆盖值标识符、类型名（含 `new` / struct 初始化）、`ns.成员` 限定访问；
/// 成员访问（`a.b`）与 import 语句内的名字不在表内（成员定义无 span，
/// 属已知限制）。具名导入的引用记录的是目标模块的原始定义而非本地绑定。
pub struct BindingTable {
    pub references: HashMap<(ModuleId, Span), Definition>,
}

impl BindingTable {
    /// 按键序遍历全部绑定（输出稳定，便于快照与去重）。
    pub fn iter(&self) -> impl Iterator<Item = (&(ModuleId, Span), &Definition)> {
        self.references.iter()
    }
}

/// 类型层级（[`SemaResult::types`]）：具名类型摘要，供父类查找与推导。
pub struct TypeHierarchy {
    /// 具名类型摘要（class 继承链）
    pub summaries: Vec<TypeSummary>,
}

/// 单个模块的编译产物（只读视图）。
///
/// [`SemaResult::modules`] 的下标即 [`ModuleId`]，与 [`BindingTable`] /
/// [`TypeHierarchy`] 中的模块标识一致。**入口模块恒为下标 0**。
///
/// 这是语法阶段产物的唯一出口：CLI 输出 JSON、codegen 取 AST 一律从这里取，
/// 不再自行调用 `novella_lexer` / `novella_parser`。
#[derive(Debug)]
pub struct AnalyzedModule {
    /// 模块标识（= 在 [`SemaResult::modules`] 中的下标）
    pub id: ModuleId,
    /// 调用方传入的原始路径（入口即 `analyze` 的 `entry`）；
    /// 诊断 `location.file` 用同一值，编辑器可直接映射
    pub path: PathBuf,
    /// 源码全文
    pub source: String,
    /// 词法产物：**未经注释过滤**的完整 token 流（含 `LineComment` 与末尾 EOF）
    pub tokens: Vec<Token>,
    /// 抽象语法树
    pub program: Program,
    /// 词法 + 语法诊断（按产生顺序）。语义诊断在 [`DiagnosticReport`]，
    /// 两处不重复——本字段即 `ModuleDiagnostics::syntax_diagnostic_count` 前缀
    pub syntax_diagnostics: Vec<Diagnostic>,
    /// 词法或语法阶段已有错误：语义检查已跳过该模块
    pub syntax_failed: bool,
    /// 文件读取失败（源码为空，诊断已记入 [`DiagnosticReport::global`]）
    pub load_failed: bool,
}

/// 模块产物视图（[`SemaResult::modules`]）。
pub struct ModuleOutput {
    /// 全部已加载模块（入口为下标 0）
    pub modules: Vec<AnalyzedModule>,
}

impl ModuleOutput {
    /// 按 id 取模块产物。
    pub fn get(&self, id: ModuleId) -> Option<&AnalyzedModule> {
        self.modules.get(id)
    }

    /// 入口模块产物（下标 0；加载器保证存在）。
    pub fn entry(&self) -> Option<&AnalyzedModule> {
        self.modules.first()
    }

    /// 按路径查找模块产物（两侧都经 [`module_key`] 规范化，
    /// 因此 `./a.nov`、绝对路径、符号链接写法都能命中同一模块）。
    pub fn find_by_path(&self, path: &Path) -> Option<&AnalyzedModule> {
        let key = module_key(path);
        self.modules
            .iter()
            .find(|module| module_key(&module.path) == key)
    }

    /// 查找入口模块产物：优先按 `entry` 路径匹配，匹配不到时退回下标 0。
    pub fn find_entry(&self, entry: &Path) -> Option<&AnalyzedModule> {
        self.find_by_path(entry).or_else(|| self.entry())
    }
}

/// 语义类型的公开投影（[`Ty`](crate::ty::Ty) 的只读视图）。
///
/// 内部 `Ty` 走 `Rc` / 内部定义类型，不能直接导出；本枚举只带名义键
/// （`Struct(模块, 名)` 等）与结构化子类型，足以供 codegen 做布局 / 调用约定推导。
/// 错误抑制用的 `Unknown` / `External` / `Error` 统一投影为 [`TypeView::Unknown`]。
#[derive(Debug, Clone, PartialEq)]
pub enum TypeView {
    Void,
    Boolean,
    String,
    /// 已定型整数：类型名（`int` 已归一化为 `i32`）
    Int(&'static str),
    /// 已定型浮点：类型名（`float` 已归一化为 `f32`）
    Float(&'static str),
    /// 未定型字面量（`42` / `1.5`），类型检查阶段的临时类型
    IntLiteral,
    FloatLiteral,
    Null,
    Pointer(Box<TypeView>),
    Array(Box<TypeView>),
    Tuple(Vec<TypeView>),
    Union(Vec<TypeView>),
    /// 指向类型注册表中函数签名的键（模块 + 函数名）
    Function(ModuleId, String),
    /// `(定义模块, 类型名)`
    Struct(ModuleId, String),
    Class(ModuleId, String),
    Trait(ModuleId, String),
    /// 不透明泛型形参
    TypeParam(String),
    /// 未知 / 外部 / 错误抑制（调用方不应据它生成代码）
    Unknown,
}

/// 表达式级类型表（[`SemaResult::expression_types`]）。
///
/// 键为 `(模块 id, 表达式 span)`，值为该表达式的语义类型。只覆盖**进入类型检查
/// 的表达式**（函数体 / 方法体 / 字段初始化 / 顶层语句）；语法失败的模块为空。
pub struct ExpressionTypes {
    pub types: HashMap<(ModuleId, Span), TypeView>,
}

impl ExpressionTypes {
    /// 按 span 查询表达式类型。
    pub fn get(&self, module: ModuleId, span: Span) -> Option<&TypeView> {
        self.types.get(&(module, span))
    }
}

/// 语义分析结果。
///
/// 各字段都是**只读视图**：语义阶段内部状态（作用域树、符号表、类型注册表、
/// 加载器）不外泄，下游只能查询模块产物、诊断、绑定与类型层级。
pub struct SemaResult {
    /// 诊断（按模块归组 + 全局）
    pub diagnostics: DiagnosticReport,
    /// 引用 → 定义绑定表
    pub bindings: BindingTable,
    /// 具名类型层级
    pub types: TypeHierarchy,
    /// 模块产物视图（源码 / token 流 / AST / 词法语法诊断）
    pub modules: ModuleOutput,
    /// 表达式级语义类型（键为 `(模块, span)`）
    pub expression_types: ExpressionTypes,
}

/// 定义位置（[`BindingTable::references`] 的值）。
///
/// 故意不暴露内部 `Symbol`：LSP 只需要名字、位置与粗粒度种类，
/// 符号表内部结构（作用域 / 可见性）不属于公共 API。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Definition {
    /// 符号名
    pub name: String,
    /// 定义所在模块
    pub module: ModuleId,
    /// 名字标识符处的 span（跳转 / 高亮目标；整条声明区间另见模块符号）
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
            span: symbol.name_span,
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

    // 模块产物与诊断一次组装：`diagnostics` 按模块重建已能释放源码 / 行表，
    // 这里再额外保留 token 流与 AST（`Program` / `Vec<Token>` 均为深拷贝，
    // 相对符号表开销可忽略），两者都随模块移出，不产生第二份持久状态。
    let module_count = sema.modules.len();
    let mut analyzed_modules = Vec::with_capacity(module_count);
    let mut module_diagnostics = Vec::with_capacity(module_count);
    for (id, module) in std::mem::take(&mut sema.modules).into_iter().enumerate() {
        let split_at = module.syntax_diagnostic_count;
        let mut diagnostics = module.diagnostics;
        let semantic = diagnostics.split_off(split_at);
        let path = module.path;
        analyzed_modules.push(AnalyzedModule {
            id,
            path: path.clone(),
            source: module.source,
            tokens: module.tokens,
            program: module.program,
            syntax_diagnostics: diagnostics,
            syntax_failed: module.syntax_failed,
            load_failed: module.load_failed,
        });
        // 语法诊断已单独放进 `AnalyzedModule.syntax_diagnostics`，
        // 这里只留语义部分（`syntax_diagnostic_count` 归零，二者不重叠）
        module_diagnostics.push(ModuleDiagnostics {
            path,
            diagnostics: semantic,
            syntax_diagnostic_count: 0,
        });
    }

    SemaResult {
        diagnostics: DiagnosticReport {
            modules: module_diagnostics,
            global: sema.global_diagnostics,
        },
        bindings: BindingTable {
            references: sema.references,
        },
        types: TypeHierarchy { summaries: types },
        modules: ModuleOutput {
            modules: analyzed_modules,
        },
        expression_types: ExpressionTypes {
            types: sema
                .expression_types
                .into_iter()
                .map(|(key, ty)| (key, ty.view()))
                .collect(),
        },
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
    /// 表达式 → 语义类型（键为 `(模块, 表达式 span)`），供 codegen 推导布局
    pub expression_types: HashMap<(ModuleId, Span), ty::Ty>,
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
            expression_types: HashMap::new(),
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
                .map(|symbol| symbol.name_span)
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
            !result.diagnostics.has_errors(),
            "overlay 分析应无错误：{:?}",
            result
                .diagnostics
                .modules
                .iter()
                .flat_map(|m| m.diagnostics.iter().map(|d| &d.message))
                .collect::<Vec<_>>()
        );

        // 类型摘要：Base / Shape 都在，Shape 的父类解析为 Base
        let shape = result
            .types
            .summaries
            .iter()
            .find(|t| t.name == "Shape")
            .expect("Shape 应在类型摘要中");
        assert_eq!(shape.kind, TypeSummaryKind::Class);
        let parent = shape.parent.as_ref().expect("Shape 有父类");
        assert_eq!(parent.1, "Base");
        assert_eq!(parent.0, shape.module, "同模块继承");

        // 引用表：class 声明里的父类名 Base、`Base()` 构造、`helper.f()` 成员访问
        assert!(
            result
                .bindings
                .references
                .values()
                .any(|d| d.name == "Base"),
            "应记录 Base 的类型引用：{:?}",
            result.bindings.references
        );
        assert!(
            result
                .bindings
                .references
                .values()
                .any(|d| d.name == "f" && d.kind == DefinitionKind::Function),
            "应记录 helper.f 的函数引用"
        );
        // 定义均落在声明模块内（helper.f 定义在 helper 模块）
        let f_def = result
            .bindings
            .references
            .values()
            .find(|d| d.name == "f")
            .expect("f 的引用");
        assert_eq!(
            result.diagnostics.modules[f_def.module].path,
            module_key(&helper)
        );
    }

    /// 表达式类型表按 `(模块, span)` 导出，且投影为公开的 `TypeView`。
    #[test]
    fn expression_types_are_exported_as_type_views() {
        let dir = PathBuf::from("/novella-mem-types");
        let main = dir.join("main.nov");
        let source = "func main() {\n    let x: int = 1 + 2;\n    let b = true;\n}\n";
        let mut overlay = HashMap::new();
        overlay.insert(main.clone(), source.to_owned());
        let mut loader = OverlayLoader::new(overlay);
        let result = analyze_with(&mut loader, &main);
        assert!(!result.diagnostics.has_errors(), "不应有错误");

        let entry = result.modules.find_entry(&main).expect("入口模块产物");
        let program = &entry.program;
        // 收集所有表达式 span，逐个查类型表，覆盖率必须 100%
        let spans = collect_expression_spans(program);
        assert!(!spans.is_empty(), "示例应含表达式");
        for span in &spans {
            assert!(
                result.expression_types.get(0, *span).is_some(),
                "表达式 span {span:?} 缺少类型"
            );
        }
        // `1`、`2` 与 `1 + 2` 都是未定型字面量类型（`IntLiteral`）；
        // 只有对照标注时才会定型为 `i32`，因此这里断言 `IntLiteral` 而不假定具体类型
        let literals = spans
            .iter()
            .map(|s| result.expression_types.get(0, *s).expect("已校验"))
            .filter(|t| matches!(t, TypeView::IntLiteral))
            .count();
        assert_eq!(literals, 3, "`1`、`2` 与 `1 + 2` 均为未定型整数");
        assert!(
            spans
                .iter()
                .any(|s| matches!(result.expression_types.get(0, *s), Some(TypeView::Boolean))),
            "`true` 应投影为 Boolean"
        );
    }

    /// 递归收集 AST 中全部表达式 span（含嵌套在语句里的）。
    ///
    /// 与 `typecheck` 的遍历保持同一套递归字段（AST 新增递归字段时本函数
    /// 因 `match` 无兜底分支而编译失败，不会静默漏收）。
    fn collect_expression_spans(program: &novella_ast::program::Program) -> Vec<Span> {
        use novella_ast::expr::Expression;
        use novella_ast::program::Item;
        use novella_ast::stmt::{ForInit, ForKind, IfBranch, Statement};

        fn walk_expr(expr: &Expression, out: &mut Vec<Span>) {
            out.push(expr.span());
            match expr {
                Expression::Paren { expr, .. }
                | Expression::Unary { operand: expr, .. }
                | Expression::Await { expr, .. }
                | Expression::Cast { expr, .. } => walk_expr(expr, out),
                Expression::Tuple { elements, .. }
                | Expression::ArrayLiteral { elements, .. }
                | Expression::MacroCall { args: elements, .. } => {
                    for element in elements {
                        walk_expr(element, out);
                    }
                }
                Expression::ObjectLiteral { properties, .. } => {
                    for property in properties {
                        if let Some(value) = &property.value {
                            walk_expr(value, out);
                        }
                    }
                }
                Expression::Binary { left, right, .. } => {
                    walk_expr(left, out);
                    walk_expr(right, out);
                }
                Expression::Ternary {
                    condition,
                    then_expr,
                    else_expr,
                    ..
                } => {
                    walk_expr(condition, out);
                    walk_expr(then_expr, out);
                    walk_expr(else_expr, out);
                }
                Expression::Assignment { target, value, .. } => {
                    walk_expr(target, out);
                    walk_expr(value, out);
                }
                Expression::MemberAccess { target, .. } => walk_expr(target, out),
                Expression::IndexAccess { target, index, .. } => {
                    walk_expr(target, out);
                    walk_expr(index, out);
                }
                Expression::Call { callee, args, .. } => {
                    walk_expr(callee, out);
                    for arg in args {
                        walk_expr(arg, out);
                    }
                }
                Expression::New { args, .. } => {
                    for arg in args {
                        walk_expr(arg, out);
                    }
                }
                Expression::Is { operand, .. } => walk_expr(operand, out),
                Expression::Match(m) => walk_expr(&m.subject, out),
                // lambda 体不在本期类型检查范围内（`check_lambda` 自建上下文）
                Expression::Lambda(_) => {}
                Expression::Literal { .. }
                | Expression::Identifier { .. }
                | Expression::This { .. }
                | Expression::StructInit { .. } => {}
            }
        }

        fn walk_stmt(stmt: &Statement, out: &mut Vec<Span>) {
            match stmt {
                Statement::Block(block) => {
                    for s in &block.stmts {
                        walk_stmt(s, out);
                    }
                }
                Statement::Variable(v) => {
                    if let Some(value) = &v.value {
                        walk_expr(value, out);
                    }
                }
                Statement::VariableList { declarations, .. } => {
                    for v in declarations {
                        if let Some(value) = &v.value {
                            walk_expr(value, out);
                        }
                    }
                }
                Statement::Expression(expr) => walk_expr(expr, out),
                Statement::Return { value, .. } | Statement::Yield { value, .. } => {
                    if let Some(value) = value {
                        walk_expr(value, out);
                    }
                }
                Statement::Throw { expr, .. } => walk_expr(expr, out),
                Statement::If(s) => {
                    for branch in &s.branches {
                        match branch {
                            IfBranch::ElseIf {
                                condition, block, ..
                            } => {
                                walk_expr(condition, out);
                                walk_stmt(block, out);
                            }
                            IfBranch::Else { block, .. } => walk_stmt(block, out),
                        }
                    }
                }
                Statement::While {
                    condition, body, ..
                } => {
                    walk_expr(condition, out);
                    walk_stmt(body, out);
                }
                Statement::For(s) => {
                    match &s.condition {
                        ForKind::ForIn(cond) => walk_expr(&cond.right, out),
                        ForKind::ForLoop(cond) => {
                            if let Some(init) = &cond.init {
                                match init {
                                    ForInit::Declaration { declarations, .. } => {
                                        for v in declarations {
                                            if let Some(value) = &v.value {
                                                walk_expr(value, out);
                                            }
                                        }
                                    }
                                    ForInit::Expression { expr, .. } => walk_expr(expr, out),
                                }
                            }
                            if let Some(condition) = &cond.condition {
                                walk_expr(condition, out);
                            }
                            if let Some(update) = &cond.update {
                                walk_expr(update, out);
                            }
                        }
                    }
                    walk_stmt(&s.block, out);
                }
                Statement::Try(s) => {
                    for s in &s.block.stmts {
                        walk_stmt(s, out);
                    }
                    for catch in &s.catches {
                        for s in &catch.block.stmts {
                            walk_stmt(s, out);
                        }
                    }
                    if let Some(finally) = &s.finally {
                        for s in &finally.stmts {
                            walk_stmt(s, out);
                        }
                    }
                }
                Statement::With(s) => {
                    walk_expr(&s.expr, out);
                    for s in &s.block.stmts {
                        walk_stmt(s, out);
                    }
                }
                Statement::Unsafe { block, .. } => {
                    for s in &block.stmts {
                        walk_stmt(s, out);
                    }
                }
                Statement::Empty { .. } | Statement::Break { .. } | Statement::Continue { .. } => {}
            }
        }

        let mut out = Vec::new();
        for item in &program.items {
            if let Item::Function(function) = item
                && let Some(block) = &function.block
            {
                walk_stmt(block, &mut out);
            }
        }
        out
    }
}

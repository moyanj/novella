//! 语义分析的公开只读数据契约。
use crate::{
    ModuleId, module_key,
    symbol::{Symbol, SymbolKind},
};
use novella_ast::program::Program;
use novella_core::{
    Span,
    diagnostic::{Diagnostic, Severity},
    token::Token,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 单个模块的诊断集合。
pub struct ModuleDiagnostics {
    pub path: PathBuf,
    /// 全部诊断（词法 / 语法 / 语义，按产生顺序）
    pub diagnostics: Vec<Diagnostic>,
    /// 前缀中词法 / 语法诊断的条数；`diagnostics[此值..]` 即纯语义诊断
    pub syntax_diagnostic_count: usize,
}

impl ModuleDiagnostics {
    /// 词法和语法诊断；与语义诊断切片不重叠。
    pub fn syntax_diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics[..self.syntax_diagnostic_count]
    }

    /// 源码顺序的全部诊断；调用方无需合并不同产物。
    pub fn ordered(&self) -> Vec<&Diagnostic> {
        let mut diagnostics: Vec<_> = self.diagnostics.iter().collect();
        diagnostics.sort_by_key(|diagnostic| {
            diagnostic
                .location
                .as_ref()
                .and_then(|loc| loc.span)
                .map(|span| span.start)
                .unwrap_or(0)
        });
        diagnostics
    }
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
/// 字段/方法引用记录真实属主的 name_span，限定类型引用不包含路径与泛型实参。
/// import 语句内的名字不作为值引用；具名导入引用直达目标模块原始定义。
pub struct BindingTable {
    pub references: HashMap<(ModuleId, Span), Definition>,
    /// 本次分析全部定义，独立于是否存在引用；键即分析内定义身份。
    pub definitions: HashMap<(ModuleId, Span), Definition>,
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
    /// 入口经显式导入、名字引用及类型依赖可达；默认环境中未使用的模块仅参与检查。
    pub required_for_codegen: bool,
    /// 调用方传入的原始路径（入口即 `analyze` 的 `entry`）；
    /// 诊断 `location.file` 用同一值，编辑器可直接映射
    pub path: PathBuf,
    /// 源码全文
    pub source: String,
    /// 词法产物：**未经注释过滤**的完整 token 流（含 `LineComment` 与末尾 EOF）
    pub tokens: Vec<Token>,
    /// 抽象语法树
    pub program: Program,
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

/// 语义类型的公开投影（内部 `Ty` 的只读视图）。
///
/// 内部 `Ty` 走 `Rc` / 内部定义类型，不能直接导出；本枚举只带名义键
/// （`Struct(模块, 名)` 等）与结构化子类型，足以供 codegen 做布局 / 调用约定推导。
/// `Unknown` 保留未定型状态，已诊断错误独立投影为 [`TypeView::Error`]。
#[derive(Debug, Clone, PartialEq)]
pub enum TypeView {
    /// 具名泛型实例的基类型与实参，后端不得擦除这些实参。
    Applied(Box<TypeView>, Vec<TypeView>),
    Future(Box<TypeView>),
    Object(std::collections::BTreeMap<String, TypeView>),
    Void,
    Boolean,
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
    /// 函数签名投影（见 [`FunctionView`]）
    Function(Box<FunctionView>),
    /// `(定义模块, 类型名)`
    Struct(ModuleId, String),
    Class(ModuleId, String),
    Trait(ModuleId, String),
    /// 不透明泛型形参
    TypeParam(String),
    /// 未知 / 外部 / 错误抑制（调用方不应据它生成代码）
    Unknown,
    /// 来源处已经报错；区别于语言允许的无注解参数 Unknown。
    Error,
}

/// 函数签名的公开投影（[`TypeView::Function`] 的载荷）。
///
/// 内部 `FunctionSig` 持有 `Ty`，不能直接导出；
/// 本结构只带 codegen 建调用约定所需的最小信息：参数 / 返回 / 标记。
///
/// 注意 `name: None` 的情形（lambda / 占位签名）：名义键缺失时仍保留
/// 完整签名结构，codegen 可据 `params` / `ret` 建 `FunctionType`，
/// 再用自己的稳定键（如模块 + 序号）做符号名。
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionView {
    /// 签名所属模块
    pub module: ModuleId,
    /// 函数名：顶层函数使用原名，方法使用 `类型::方法`；lambda / 占位签名为 `None`
    pub name: Option<String>,
    /// 形参类型（未标注类型的形参为 [`TypeView::Unknown`]）
    pub params: Vec<TypeView>,
    /// 返回类型
    pub ret: Box<TypeView>,
    /// 泛型参数个数（调用检查已完成代入与约束验证）
    pub type_param_count: usize,
    /// 是否 `async` 函数
    pub is_async: bool,
    /// 是否 `extern` 函数（仅有签名、无函数体）
    pub is_extern: bool,
    /// 是否带隐式接收者（实例方法）
    ///
    /// `self` 不占 `params` 位置：语义检查对 `self` 不解析类型，若把它当作
    /// 普通形参投影，`params` 里就会出现 [`TypeView::Unknown`]，下游只能整体拒绝。
    /// 因此投影时略去 `self` 占位形参，改由本标记表达；接收者类型即方法所属的
    /// 具名类型（[`TypeSummary`] 中的定义）。
    pub receiver: bool,
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

/// 已在宿主源码坐标中检查过的格式化字符串片段。
#[derive(Debug, Clone)]
pub enum FStringPart {
    Text(String),
    Expression(novella_ast::expr::Expression),
}

/// 语义分析结果。
///
/// 各字段都是**只读视图**：语义阶段内部状态（作用域树、符号表、类型注册表、
/// 加载器）不外泄，下游只能查询模块产物、诊断、绑定与类型层级。
pub struct SemaResult {
    /// 实际用于平台整数范围检查的分析配置；旧 API 仍允许仅做 32 位静态分析。
    pub analysis_target: crate::AnalysisTarget,
    /// 入口与链接策略由语义阶段传递，后续阶段不得重新猜测。
    pub build_target: novella_core::target::BuildTarget,
    /// 目标描述；HIR 与后端据此决定指针宽度、目标特性与发射后端。
    pub build_spec: novella_target::TargetSpec,
    /// 入口模块显式关闭语言 main 入口检查与包装。
    pub no_main: bool,
    /// 入口模块显式关闭默认标准库环境。
    pub no_std: bool,
    /// 顶层函数声明位置对应的已验证 C 导出符号。
    pub exports: HashMap<(ModuleId, Span), String>,
    /// 由实际 std 根目录解析出的字符串名义身份；下游不得按文件名猜测。
    pub string_type: Option<TypeView>,
    /// f-string 已检查的片段；下游复用 AST，不重复词法/语法分析。
    pub fstrings: HashMap<(ModuleId, Span), Vec<FStringPart>>,
    pub declarations: crate::DeclarationIndex,
    /// 已证明穷尽的 match；下游可据此降级无 default 的值分支。
    pub exhaustive_matches: HashSet<(ModuleId, Span)>,
    /// 诊断（按模块归组 + 全局）
    pub diagnostics: DiagnosticReport,
    /// 引用 → 定义绑定表
    pub bindings: BindingTable,
    /// 具名类型层级
    pub types: TypeHierarchy,
    /// 模块产物视图（源码 / token 流 / AST）；全部诊断见 diagnostics。
    pub modules: ModuleOutput,
    /// 表达式级语义类型（键为 `(模块, span)`）
    pub expression_types: ExpressionTypes,
    /// 类型注解的解析结果（含别名展开与限定名；键为注解 span）。
    pub annotation_types: ExpressionTypes,
    /// 声明的最终类型（函数签名 / 变量绑定；键为声明 span）。
    pub declaration_types: ExpressionTypes,
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
    pub(crate) fn from_symbol(symbol: &Symbol) -> Self {
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
    /// 字段（声明顺序；struct / class 可能有，其余为空）
    pub fields: Vec<FieldSummary>,
    /// 方法（按名字排序，保证下游输出稳定）
    pub methods: Vec<MethodSummary>,
}

/// 字段摘要（[`TypeSummary::fields`] 的元素）。
///
/// `ty` 用 [`TypeView`] 表达：字段类型里出现未知 / 外部 / 联合时会投影为
/// `Unknown` 等值，下游据此 fail-fast，而不是拿到一个「看起来能用」的类型。
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSummary {
    pub name: String,
    pub ty: TypeView,
    /// 是否有声明处默认值
    pub has_default: bool,
    /// `weak` 标记：赋值不 retain、不参与引用计数
    pub is_weak: bool,
    /// `static` 标记：不出现在实例布局里，而是一个全局
    pub is_static: bool,
}

/// 方法摘要（[`TypeSummary::methods`] 的元素）。
#[derive(Debug, Clone, PartialEq)]
pub struct MethodSummary {
    pub name: String,
    /// 方法签名（`this` 不占参数位；`name` 恒为 `Some(方法名)`）
    pub signature: FunctionView,
    pub is_static: bool,
}

/// 类型摘要的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeSummaryKind {
    Struct,
    Class,
    Trait,
    Alias,
}

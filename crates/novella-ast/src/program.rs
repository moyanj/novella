use serde::{Deserialize, Serialize};

use novella_core::{
    Span,
    token::{Token, TokenKind},
};

use crate::{
    Spanned,
    expr::Expression,
    stmt::{Block, Statement},
    types::{FunctionType, Param, TypeExpr, TypeParameter},
};

/// 编译器指令（对应文法 `directive`）：`@name` 或 `@name(...)`
///
/// 这是**编译期**指令，语义等价于 Rust 的 attribute `#[name(...)]`，
/// 不是 Python 那种在语言里另有对象的运行时机制：`@xxx(...)` 由编译器在编译期按 `xxx`
/// 解释并作用于其修饰的声明 / 指令块（可影响布局、代码生成、检查等），
/// 不构造运行时包装。
/// - `name` 为指令名（如 `@cfg(target_os = "linux")` 中的 `cfg`）；
/// - 圆括号内为**原始 token 树**（任意 token，允许嵌套括号与各类字面量），
///   不在 AST 层做结构化解析，由语义分析阶段按 `name` 解释其含义。
///
/// 出于「值语义延后」的项目约定，token 流原样保留（含字符串/进制前缀），
/// 嵌套括号以 `OpenParen`/`CloseParen` 等 token 平铺在序列中，需要时可
/// 按括号配对还原树形。`@name` 无括号的简写形式等价于空 token 流。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct CompilerDirective {
    /// 指令名：如 `@cfg(...)` 中的 `cfg`
    pub name: String,
    /// 圆括号内的原始 token 序列（不含外层括号本身）；`@name` 简写形式为空
    pub tokens: Vec<Token>,
    /// 指令整体（含 `@` 与括号）覆盖的源码区间
    pub span: Span,
}

/// 程序根节点：顶层 item 列表（每个 item 自带源码区间）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct Program {
    pub items: Vec<Item>,
    /// 整个源文件覆盖的源码区间
    pub span: Span,
}

/// 对象可见性
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    Public,
    Private,
    Package,
}
impl Scope {
    pub fn from_token(token: &TokenKind) -> Option<Self> {
        match *token {
            TokenKind::Pub => Some(Self::Public),
            TokenKind::Pri => Some(Self::Private),
            TokenKind::Pkg => Some(Self::Package),
            _ => None,
        }
    }
}

/// 导入对象（对应文法 `fromImport` 的 `identifier (',' identifier)*` 列表中的单项）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct ImportSpecifier {
    /// 本地符号名，None 表示与 imported 同名
    pub local: Option<String>,
    /// 实际符号名
    pub imported: String,
    /// 该项覆盖的源码区间
    pub span: Span,
}

/// 导入的具体形式
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum ImportKind {
    /// 具名导入：from "mod" import a, b as c
    Named {
        /// 具名导入的符号列表
        specifiers: Vec<ImportSpecifier>,
        /// 导入语句覆盖的源码区间
        span: Span,
    },
    /// 命名空间导入：import "mod"  将整个模块绑定到 infered_name
    Namespace {
        /// 本地绑定的符号名
        local_alias: String,
        /// 导入语句覆盖的源码区间
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct ImportItem {
    /// 原始路径字面量
    pub raw_path: String,
    /// 前缀（如 "std"）
    pub prefix: Option<String>,
    /// 逻辑路径（去除协议）
    pub path: String,
    /// 导入变体
    pub kind: ImportKind,
    /// 整条导入语句覆盖的源码区间
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct TypeAliasItem {
    /// 别名名称
    pub name: String,
    /// 泛型参数列表（`type A<T> = ...` 中的 `<T>`）
    pub type_parameters: Vec<TypeParameter>,
    /// 被别名的类型表达式（`=` 右侧）
    pub alias: TypeExpr,
    /// 类型别名声明覆盖的源码区间
    pub span: Span,
}

/// trait 声明（对应文法
/// `traitDeclaration: directive* (Pub | Pri | Pkg)? Trait identifier typeParameters? traitBody`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct TraitItem {
    /// trait 编译器指令列表（对应文法 `traitDeclaration` 的 `directive*`）
    pub directives: Vec<CompilerDirective>,
    /// trait 名
    pub name: String,
    /// 泛型参数列表（对应文法 `typeParameters?`）
    pub type_parameters: Vec<TypeParameter>,
    /// trait 成员列表（对应文法 `traitMember: methodDeclaration | fieldDeclaration`）
    pub members: Vec<ClassMember>,
    /// 可见性（对应文法 `(Pub | Pri | Pkg)?`），None 表示缺省
    pub scope: Option<Scope>,
    /// 整条 trait 声明覆盖的源码区间（含编译器指令与可见性修饰符）
    pub span: Span,
}

/// 类成员修饰符：可见性、静态与弱引用标记（对应文法 `accessModifier? Static? Weak?`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemberModifiers {
    pub scope: Option<Scope>,
    /// 是否为静态成员（对应文法 `Static?`）
    pub is_static: bool,
    /// 是否为弱引用字段（对应文法 `Weak?`，仅字段有意义）。
    /// 赋值不 retain、不参与引用计数；访问过期对象属未定义行为
    /// （无空安全保证，见 `docs/object-model.md`）。
    pub is_weak: bool,
}

/// 类字段成员（对应文法 `fieldDeclaration`：`directive* accessModifier? Static? identifier typeAnnotation? (Assign expression)? SemiColon`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct FieldMember {
    /// 字段标识符位置（成员绑定与重命名使用）。
    pub name_span: Span,
    /// 字段编译器指令列表（对应文法 `fieldDeclaration` 的 `directive*`）
    pub directives: Vec<CompilerDirective>,
    /// 可见性与静态修饰符
    pub modifiers: MemberModifiers,
    /// 字段名
    pub name: String,
    /// 类型注解（对应文法 `typeAnnotation?`），None 表示缺省
    pub ty: Option<TypeExpr>,
    /// 初始化表达式（对应文法 `(Assign expression)?`），None 表示无初始化
    pub initializer: Option<Expression>,
    /// 字段声明覆盖的源码区间
    pub span: Span,
}

/// 类方法成员（对应文法 `methodDeclaration`：`directive* accessModifier? Static? Func identifier typeParameters? '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? (block | SemiColon)`）
///
/// 独立于 [`FunctionItem`]，以表达方法专有的访问修饰符与静态标记。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct MethodMember {
    /// 方法标识符位置（不覆盖参数列表与函数体）。
    pub name_span: Span,
    /// 方法编译器指令列表（对应文法 `methodDeclaration` 的 `directive*`）
    pub directives: Vec<CompilerDirective>,
    /// 可见性与静态修饰符
    pub modifiers: MemberModifiers,
    /// 方法名
    pub name: String,
    /// 泛型参数列表（对应文法 `typeParameters?`）
    pub type_parameters: Vec<TypeParameter>,
    /// 形参列表（对应文法 `parameterList?`）
    pub params: Vec<Param>,
    /// 返回类型注解（对应文法 `typeAnnotation? ReturnArrow? typeExpression?`），None 表示缺省
    pub return_type: Option<TypeExpr>,
    /// 方法体（对应文法 `block`），None 表示纯声明形态（`SemiColon`）
    pub block: Option<Block>,
    /// 方法声明覆盖的源码区间
    pub span: Span,
}

/// 类成员（对应文法 `classMember: methodDeclaration | fieldDeclaration`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum ClassMember {
    /// 字段成员（对应文法 `fieldDeclaration`）
    Field(FieldMember),
    /// 方法成员（对应文法 `methodDeclaration`）
    Method(MethodMember),
}

/// 类声明（对应文法
/// `classDeclaration: directive* Pub? Class identifier typeParameters? classHeritage? classBody`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct ClassItem {
    /// 类编译器指令列表（对应文法 `classDeclaration` 的 `directive*`）
    pub directives: Vec<CompilerDirective>,
    pub name: String,
    pub type_parameters: Vec<TypeParameter>,
    pub members: Vec<ClassMember>,
    /// 继承父类列表（对应文法 `classHeritage`，文法仅允许单个 `typeReference`，此处按多继承预留）
    pub parent: Vec<TypeExpr>,
    /// 可见性（对应文法 `Pub?`）
    pub scope: Option<Scope>,
    /// 整条类声明覆盖的源码区间
    pub span: Span,
}

/// 顶层函数声明（对应文法 `functionDeclaration` / `externFunctionDeclaration`）
///
/// `is_extern` 区分两种形态：`func foo() {...}`（带函数体）与
/// `extern func foo();`（仅签名，`block` 为 None）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct FunctionItem {
    /// 函数编译器指令列表（对应文法 `functionDeclaration` / `methodDeclaration` 的 `directive*`）
    pub directives: Vec<CompilerDirective>,
    pub name: String,
    pub r#type: Option<FunctionType>,
    /// 函数体（对应文法 `block`），None 表示纯声明形态（`SemiColon`）。
    /// `Box` 承载以控制 `FunctionItem` 尺寸，避免触发 clippy `large-enum-variant`
    /// （`Statement` 枚举整体尺寸较大，直接内嵌会让 `Item::Function` 变体膨胀）。
    pub block: Option<Box<Statement>>,
    pub scope: Option<Scope>,
    pub is_extern: bool,
    pub is_async: bool,
    /// 名字标识符的区间（跳转 / 高亮用；`span` 覆盖整条声明，含函数体）
    pub name_span: Span,
    /// 整条函数声明覆盖的源码区间
    pub span: Span,
}

/// struct 声明（对应文法 `structDeclaration`）
///
/// 值语义（design.md §4.7）：赋值/传参逐位拷贝；无继承、无构造器
/// （字段初始化构造 `TypeName{ x: 1 }`，见 `Expression::StructInit`）；
/// 允许方法，方法内 `this` 为编译器注入的可信指针（字段访问无需 unsafe）；
/// 不识别 `__` 前缀特殊方法。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct StructItem {
    /// struct 编译器指令列表（对应文法 `structDeclaration` 的 `directive*`）
    pub directives: Vec<CompilerDirective>,
    pub name: String,
    pub type_parameters: Vec<TypeParameter>,
    /// 成员列表（复用 `ClassMember`：Field / Method）
    pub members: Vec<ClassMember>,
    /// 继承信息（`struct S(A) {}` 的 `(A)`；对应文法 `structDeclaration` 的 `classHeritage?`）。
    ///
    /// design.md 中 struct 无继承语义：parser 只负责**保留**该信息（避免静默丢失），
    /// 是否拒绝由语义分析判定。
    pub heritage: Vec<TypeExpr>,
    /// 可见性（对应文法 `Pub?`）
    pub scope: Option<Scope>,
    /// 整条 struct 声明覆盖的源码区间（含编译器指令与可见性修饰符）
    pub span: Span,
}

/// 顶层 item（对应文法 `statement | importStatement | typeAliasStatement | classDeclaration | functionDeclaration`）。
///
/// 每个变体都携带自身位置，统一经 [`Item::span`] 访问。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum Item {
    /// 导入语句（对应文法 `moduleImport`）
    Import(ImportItem),
    /// 类型别名（对应文法 `typeAliasDeclaration`）
    TypeAlias(TypeAliasItem),
    /// trait 声明（对应文法 `traitDeclaration`）
    Trait(TraitItem),
    /// 类声明（对应文法 `classDeclaration`）
    Class(ClassItem),
    /// struct 声明（对应文法 `structDeclaration`）
    Struct(StructItem),
    /// 函数声明（对应文法 `functionDeclaration` / `externFunctionDeclaration`）
    Function(FunctionItem),
    /// 顶层独立编译器指令语句（对应文法 `directiveStatement` 的 `directive SemiColon` 形态，如 `@no_std;`）
    Directive(CompilerDirective),
    /// 编译器指令块：指令直接作用于一个语句块（对应文法 `directiveStatement` 的 `directive block` 形态，
    /// 如 `@cfg(feature = "limine") { ... }`，语义等价 Rust 的 `#[cfg(...)] { ... }`）
    DirectiveBlock {
        /// 编译器指令
        directive: CompilerDirective,
        /// 被指令修饰的语句块
        block: Option<Block>,
        /// 指令块整体覆盖的源码区间
        span: Span,
    },
    /// 顶层语句（`Box` 承载以控制枚举尺寸，避免 clippy `large-enum-variant`）
    Statement(Box<Statement>),
}

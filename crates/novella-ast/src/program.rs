use serde::{Deserialize, Serialize};

use novella_core::Spanned;

use crate::{
    decorator::Decorator,
    expr::Expression,
    stmt::{Block, Statement},
    types::{FunctionType, Param, TypeExpr, TypeParameter},
};

/// 程序根节点：顶层 item 列表（携带源码位置）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub items: Vec<Spanned<Item>>,
}

/// 对象可见性
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    Pubilc,
    Private,
}

/// 导入对象
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportSpecifier {
    /// 本地符号名，None 表示与 imported 同名
    pub local: Option<String>,
    /// 实际符号名
    pub imported: String,
}

/// 导入的具体形式
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImportKind {
    /// 具名导入：from "mod" import a, b as c
    Named { specifiers: Vec<ImportSpecifier> },
    /// 命名空间导入：import "mod"  将整个模块绑定到 infered_name
    Namespace {
        /// 本地绑定的符号名
        local_alias: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportItem {
    /// 原始路径字面量
    pub raw_path: String,
    /// 前缀（如 "std"）
    pub prefix: Option<String>,
    /// 逻辑路径（去除协议）
    pub path: String,
    /// 导入变体
    pub kind: ImportKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeAliasItem {
    /// 别名名称
    pub name: String,
    /// 泛型参数列表（`type A<T> = ...` 中的 `<T>`）
    pub type_parameters: Vec<TypeParameter>,
    /// 被别名的类型表达式（`=` 右侧）
    pub alias: TypeExpr,
}

// TODO: 实现接口的AST节点
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterfaceItem {
    /// 接口装饰器列表（对应文法 `interfaceDeclaration` 的 `decorator*`）
    pub attributes: Vec<Decorator>,
}

/// 类成员修饰符：可见性与静态标记（对应文法 `accessModifier? Static?`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemberModifiers {
    pub scope: Option<Scope>,
    /// 是否为静态成员（对应文法 `Static?`）
    pub is_static: bool,
}

/// 类字段成员（对应文法 `fieldDeclaration`：`decorator* accessModifier? Static? identifier typeAnnotation? (Assign expression)? SemiColon`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldMember {
    /// 字段装饰器列表（对应文法 `fieldDeclaration` 的 `decorator*`）
    pub attributes: Vec<Decorator>,
    /// 可见性与静态修饰符
    pub modifiers: MemberModifiers,
    /// 字段名
    pub name: String,
    /// 类型注解（对应文法 `typeAnnotation?`），None 表示缺省
    pub ty: Option<TypeExpr>,
    /// 初始化表达式（对应文法 `(Assign expression)?`），None 表示无初始化
    pub initializer: Option<Expression>,
}

/// 类方法成员（对应文法 `methodDeclaration`：`decorator* accessModifier? Static? Func identifier typeParameters? '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? (block | SemiColon)`）
///
/// 不复用 [`FunctionItem`]：其 `r#type: FunctionType` 要求必填返回类型，
/// 无法表达 `func foo() {}` 这类缺省返回注解的形态；且方法独有的
/// `accessModifier?` / `Static?` 与顶层 `is_extern` 语义并不互通。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodMember {
    /// 方法装饰器列表（对应文法 `methodDeclaration` 的 `decorator*`）
    pub attributes: Vec<Decorator>,
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
}

/// 类成员（对应文法 `classMember: methodDeclaration | fieldDeclaration`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClassMember {
    /// 字段成员（对应文法 `fieldDeclaration`）
    Field(FieldMember),
    /// 方法成员（对应文法 `methodDeclaration`）
    Method(MethodMember),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassItem {
    /// 类装饰器列表（对应文法 `classDeclaration` 的 `decorator*`）
    pub attributes: Vec<Decorator>,
    pub name: String,
    pub type_parameters: Vec<TypeParameter>,
    pub members: Vec<ClassMember>,
    pub parent: Vec<TypeExpr>,
    pub scope: Option<Scope>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionItem {
    /// 函数装饰器列表（对应文法 `functionDeclaration` / `methodDeclaration` 的 `decorator*`）
    pub attributes: Vec<Decorator>,
    pub name: String,
    pub r#type: Option<FunctionType>,
    pub block: Option<Block>,
    pub scope: Option<Scope>,
    pub is_extern: bool,
    pub is_async: bool,
}

/// 顶层 item（对应文法 `statement | importStatement | typeAliasStatement | classDeclaration | functionDeclaration`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Item {
    Import(ImportItem),
    TypeAlias(TypeAliasItem),
    Interface(InterfaceItem),
    Class(ClassItem),
    Function(FunctionItem),
    /// 顶层独立装饰器语句（对应文法 `decoratorStatement` 的 `decorator SemiColon` 形态，如 `@foo;`）
    Decorator(Decorator),
    /// 装饰块：装饰器直接修饰一个语句块（对应文法 `decoratorStatement` 的 `decorator block` 形态，
    /// 如 `@cfg(feature = "limine") { ... }`，语义等价 Rust 的 `#[cfg(...)] { ... }`）
    DecoratorBlock {
        /// 装饰器
        decorator: Decorator,
        /// 被装饰的语句块
        block: Option<Block>,
    },
    /// 顶层语句（`Box` 承载以控制枚举尺寸，避免 clippy `large-enum-variant`）
    Statement(Box<Statement>),
}

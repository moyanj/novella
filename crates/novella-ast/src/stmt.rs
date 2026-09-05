use crate::{expr::Expression, program::Scope, types::TypeExpr};

pub struct Block {
    pub stmts: Vec<Statement>,
}

/// 变量声明关键字（对应文法 `variableStatement` 开头的 `const` / `let`）
#[derive(Debug, Clone, PartialEq)]
pub enum VariableType {
    Const,
    Let,
}

/// 变量绑定模式（对应文法 `variableDeclaration` 中的 `identifier | bindingPattern`）。
///
/// 既支持单个标识符绑定（`let a = 1;`），也支持元组解构绑定（`let (a, b) = (0, 1);`）。
/// 设计为递归结构：`Tuple` 的元素仍是绑定模式，与 `TypeExpr::Tuple` 的递归风格一致，
/// 将来可自然扩展为嵌套解构（如 `let ((x, y), z) = ...;`）。
#[derive(Debug, Clone, PartialEq)]
pub enum BindingPattern {
    /// 单个标识符绑定：`a`
    Ident(String),
    /// 元组解构绑定：`(a, b)`
    Tuple(Vec<BindingPattern>),
}

/// 变量声明语句（对应文法的 `variableDeclaration`）
///
/// 该结构同时承载普通变量声明与 for 循环 init 段的声明列表：
/// 后者在文法中不带 `const` / `let` 关键字，按本语言约定创建时
/// `r#type` 一律自动填充为 `VariableType::Let`（见 [`ForInit`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct VariableStmt {
    /// 变量绑定模式（单个标识符或元组解构）
    pub pattern: BindingPattern,
    pub ty: Option<TypeExpr>,
    pub value: Option<Expression>,
    pub scope: Option<Scope>,
    pub r#type: VariableType,
}

pub struct IfCondition {
    pub condition: Option<Expression>, // None 表示 else 分支
    pub block: Block,
}

pub struct IfStmt {
    pub branchs: Vec<IfCondition>,
}

pub struct ForInCond {
    pub left: BindingPattern,
    pub right: Expression,
}

/// for 循环 init 段（对应文法 `forInit: variableDeclarationList | expression`）
///
/// 文法中 init 段声明列表不带 `const` / `let` 关键字，
/// 本语言规定 for 循环 init 中的声明一律按 `let` 语义承载，
/// 即创建 [`VariableStmt`] 时 `r#type` 自动填充为 [`VariableType::Let`]。
#[derive(Debug, Clone, PartialEq)]
pub enum ForInit {
    /// 声明列表形态：`for (i = 0, j = 1; ...)`（对应 `variableDeclarationList`）
    Declaration(Vec<VariableStmt>),
    /// 表达式形态：`for (i = 0; ...)`（对应 `expression`）
    Expression(Expression),
}

/// C 风格 for 循环的三段式条件（对应文法
/// `For '(' forInit? SemiColon expression? SemiColon expression? ')'`）
#[derive(Debug, Clone, PartialEq)]
pub struct ForLoopCond {
    /// init 段（可省略）
    pub init: Option<ForInit>,
    /// 循环继续条件（可省略，省略视为恒真）
    pub condition: Option<Expression>,
    /// 每次迭代后执行的更新表达式（可省略）
    pub update: Option<Expression>,
}

pub enum ForKind {
    ForIn(ForInCond),
    ForLoop(ForLoopCond),
}

pub struct ForStmt {
    pub condition: ForKind,
    pub block: Block,
}

pub enum Statement {
    Variable(VariableStmt),
    Return(Option<Expression>),
    Break,
    Throw(Expression),
    Yield(Option<Expression>),
    Block(Block),
    If(IfStmt),
    While(Expression, Block),
    For(ForStmt),
    Expression(Expression),
}

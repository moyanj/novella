use serde::{Deserialize, Serialize};

use crate::{expr::Expression, program::Scope, types::TypeExpr};

/// 语句块（对应文法的 `statementList`，即 `{ stmt* }`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub stmts: Vec<Statement>,
}

/// 变量声明关键字（对应文法 `variableStatement` 开头的 `const` / `let`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VariableType {
    Const,
    Let,
}

/// 变量绑定模式（对应文法 `variableDeclaration` 中的 `identifier | bindingPattern`）。
///
/// 既支持单个标识符绑定（`let a = 1;`），也支持元组解构绑定（`let (a, b) = (0, 1);`）。
/// 设计为递归结构：`Tuple` 的元素仍是绑定模式，与 `TypeExpr::Tuple` 的递归风格一致，
/// 将来可自然扩展为嵌套解构（如 `let ((x, y), z) = ...;`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariableStmt {
    /// 变量绑定模式（单个标识符或元组解构）
    pub pattern: BindingPattern,
    pub ty: Option<TypeExpr>,
    pub value: Option<Expression>,
    pub scope: Option<Scope>,
    pub r#type: VariableType,
}

/// if / elif / else 的分支（对应文法 `ifStatement` 的 `If` / `Elif` / `Else` 三种形态）。
///
/// 文法中每个分支的 body 都是 `statement`（不限于花括号块），
/// 因此分支体用 `Box<Statement>` 承载任意语句；
/// 递归字段用 `Box` 控制枚举尺寸（避免 clippy `large-enum-variant`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IfBranch {
    /// 有条件的分支：`if (cond) stmt` 与 `elif (cond) stmt`
    ElseIf {
        condition: Expression,
        block: Box<Statement>,
    },
    /// 无条件的兜底分支：`else stmt`
    Else { block: Box<Statement> },
}

/// if 语句（对应文法 `ifStatement`）。
///
/// `branches` 按源码顺序承载条件链：第一个通常是 `ElseIf`（即 `if`），
/// 中间可以有任意个 `ElseIf`（对应 `Elif`），最后一个可以是 `Else`（可省略）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IfStmt {
    pub branches: Vec<IfBranch>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForInCond {
    pub left: BindingPattern,
    pub right: Expression,
}

/// for 循环 init 段（对应文法 `forInit: variableDeclarationList | expression`）
///
/// 文法中 init 段声明列表不带 `const` / `let` 关键字，
/// 本语言规定 for 循环 init 中的声明一律按 `let` 语义承载，
/// 即创建 [`VariableStmt`] 时 `r#type` 自动填充为 [`VariableType::Let`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ForInit {
    /// 声明列表形态：`for (i = 0, j = 1; ...)`（对应 `variableDeclarationList`）
    Declaration(Vec<VariableStmt>),
    /// 表达式形态：`for (i = 0; ...)`（对应 `expression`）
    Expression(Expression),
}

/// C 风格 for 循环的三段式条件（对应文法
/// `For '(' forInit? SemiColon expression? SemiColon expression? ')'`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForLoopCond {
    /// init 段（可省略）
    pub init: Option<ForInit>,
    /// 循环继续条件（可省略，省略视为恒真）
    pub condition: Option<Expression>,
    /// 每次迭代后执行的更新表达式（可省略）
    pub update: Option<Expression>,
}

/// for 循环条件变体：for-in 与 C 风格三段式
///
/// 递归字段用 `Box` 承载以控制枚举尺寸（避免 clippy `large-enum-variant`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ForKind {
    ForIn(ForInCond),
    ForLoop(Box<ForLoopCond>),
}

/// for 语句（对应文法 `forStatement`）
///
/// 文法中循环体是 `statement`（不限于花括号块），
/// 因此用 `Box<Statement>` 承载任意语句（递归字段用 `Box` 控制枚举尺寸）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForStmt {
    pub condition: ForKind,
    pub block: Box<Statement>,
}

/// try 的 catch 分支（对应文法 `catchClause`：
/// `Catch '(' identifier typeAnnotation? ')' block`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatchClause {
    /// 捕获的异常变量名（`catch (e)` 中的 `e`）
    pub name: String,
    /// 可选的异常类型注解（`catch (e: SomeError)` 中的 `SomeError`）
    pub ty: Option<TypeExpr>,
    /// catch 分支体
    pub block: Block,
}

/// try 语句（对应文法 `tryStatement`：
/// `Try block (catchClause+ finallyClause? | finallyClause)`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TryStmt {
    /// try 块
    pub block: Block,
    /// 一个或多个 catch 分支；若无 catch 则必须存在 finally
    pub catches: Vec<CatchClause>,
    /// 可选的 finally 块（对应文法 `finallyClause`）
    pub finally: Option<Block>,
}

/// with 绑定的可选项（对应文法 `withStatement` 的 `As` 子句）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WithBinding {
    /// 解构绑定目标：标识符或元组解构模式（对应 `identifier | bindingPattern`）
    pub pattern: BindingPattern,
    /// 可选的类型注解
    pub ty: Option<TypeExpr>,
}

/// with 语句（对应文法 `withStatement`：
/// `With expression (As (identifier | bindingPattern) typeAnnotation?)? block`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WithStmt {
    /// 被管理的资源表达式（如文件、锁等）
    pub expr: Expression,
    /// 可选的 `As name: Type` 绑定子句
    pub binding: Option<WithBinding>,
    /// with 块
    pub block: Block,
}

/// 语句（对应文法 `statement`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Statement {
    Variable(VariableStmt),
    Return(Option<Expression>),
    Break,
    Throw(Expression),
    Yield(Option<Expression>),
    Block(Block),
    If(IfStmt),
    While(Expression, Box<Statement>),
    For(ForStmt),
    Try(TryStmt),
    With(WithStmt),
    Expression(Expression),
}

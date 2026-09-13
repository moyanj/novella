use serde::{Deserialize, Serialize};

use novella_core::Span;

use crate::{Spanned, expr::Expression, program::Scope, types::TypeExpr};

/// 语句块（对应文法的 `statementList`，即 `{ stmt* }`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct Block {
    pub stmts: Vec<Statement>,
    /// 含花括号的源码区间
    pub span: Span,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum BindingPattern {
    /// 单个标识符绑定：`a`
    Ident {
        /// 绑定名
        name: String,
        /// 标识符的源码区间
        span: Span,
    },
    /// 元组解构绑定：`(a, b)`
    Tuple {
        /// 元素绑定模式列表
        elements: Vec<BindingPattern>,
        /// 含括号的源码区间
        span: Span,
    },
}

/// 变量声明语句（对应文法的 `variableDeclaration`）
///
/// 该结构同时承载普通变量声明与 for 循环 init 段的声明列表：
/// 后者在文法中不带 `const` / `let` 关键字，按本语言约定创建时
/// `r#type` 一律自动填充为 `VariableType::Let`（见 [`ForInit`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct VariableStmt {
    /// 变量绑定模式（单个标识符或元组解构）
    pub pattern: BindingPattern,
    pub ty: Option<TypeExpr>,
    pub value: Option<Expression>,
    pub scope: Option<Scope>,
    pub r#type: VariableType,
    /// 单条声明覆盖的源码区间
    pub span: Span,
}

/// if / elif / else 的分支（对应文法 `ifStatement` 的 `If` / `Elif` / `Else` 三种形态）。
///
/// 文法中每个分支的 body 都是 `statement`（不限于花括号块），
/// 因此分支体用 `Box<Statement>` 承载任意语句；
/// 递归字段用 `Box` 控制枚举尺寸（避免 clippy `large-enum-variant`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum IfBranch {
    /// 有条件的分支：`if (cond) stmt` 与 `elif (cond) stmt`
    ElseIf {
        condition: Expression,
        block: Box<Statement>,
        /// 该分支（条件与分支体）覆盖的源码区间
        span: Span,
    },
    /// 无条件的兜底分支：`else stmt`
    Else {
        block: Box<Statement>,
        /// 该分支覆盖的源码区间
        span: Span,
    },
}

/// if 语句（对应文法 `ifStatement`）。
///
/// `branches` 按源码顺序承载条件链：第一个通常是 `ElseIf`（即 `if`），
/// 中间可以有任意个 `ElseIf`（对应 `Elif`），最后一个可以是 `Else`（可省略）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct IfStmt {
    pub branches: Vec<IfBranch>,
    /// 整条 if / elif / else 链覆盖的源码区间
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct ForInCond {
    pub left: BindingPattern,
    pub right: Expression,
    /// `left in right` 条件覆盖的源码区间
    pub span: Span,
}

/// for 循环 init 段（对应文法 `forInit: variableDeclarationList | expression`）
///
/// 文法中 init 段声明列表不带 `const` / `let` 关键字，
/// 本语言规定 for 循环 init 中的声明一律按 `let` 语义承载，
/// 即创建 [`VariableStmt`] 时 `r#type` 自动填充为 [`VariableType::Let`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum ForInit {
    /// 声明列表形态：`for (i = 0, j = 1; ...)`（对应 `variableDeclarationList`）
    Declaration {
        /// 声明列表
        declarations: Vec<VariableStmt>,
        /// init 段覆盖的源码区间
        span: Span,
    },
    /// 表达式形态：`for (i = 0; ...)`（对应 `expression`）
    Expression {
        /// init 表达式
        expr: Expression,
        /// init 段覆盖的源码区间
        span: Span,
    },
}

/// C 风格 for 循环的三段式条件（对应文法
/// `For '(' forInit? SemiColon expression? SemiColon expression? ')'`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct ForLoopCond {
    /// init 段（可省略）
    pub init: Option<ForInit>,
    /// 循环继续条件（可省略，省略视为恒真）
    pub condition: Option<Expression>,
    /// 每次迭代后执行的更新表达式（可省略）
    pub update: Option<Expression>,
    /// 括号内三段式条件覆盖的源码区间
    pub span: Span,
}

/// for 循环条件变体：for-in 与 C 风格三段式
///
/// 递归字段用 `Box` 承载以控制枚举尺寸（避免 clippy `large-enum-variant`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum ForKind {
    ForIn(Box<ForInCond>),
    ForLoop(Box<ForLoopCond>),
}

/// for 语句（对应文法 `forStatement`）
///
/// 文法中循环体是 `statement`（不限于花括号块），
/// 因此用 `Box<Statement>` 承载任意语句（递归字段用 `Box` 控制枚举尺寸）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct ForStmt {
    pub condition: ForKind,
    pub block: Box<Statement>,
    /// 整条 for 语句覆盖的源码区间
    pub span: Span,
}

/// try 的 catch 分支（对应文法 `catchClause`：
/// `Catch '(' identifier typeAnnotation? ')' block`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct CatchClause {
    /// 捕获的异常变量名（`catch (e)` 中的 `e`）
    pub name: String,
    /// 可选的异常类型注解（`catch (e: SomeError)` 中的 `SomeError`）
    pub ty: Option<TypeExpr>,
    /// catch 分支体
    pub block: Block,
    /// 该 catch 子句覆盖的源码区间
    pub span: Span,
}

/// try 语句（对应文法 `tryStatement`：
/// `Try block (catchClause+ finallyClause? | finallyClause)`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct TryStmt {
    /// try 块
    pub block: Block,
    /// 一个或多个 catch 分支；若无 catch 则必须存在 finally
    pub catches: Vec<CatchClause>,
    /// 可选的 finally 块（对应文法 `finallyClause`）
    pub finally: Option<Block>,
    /// 整条 try 语句覆盖的源码区间
    pub span: Span,
}

/// with 绑定的可选项（对应文法 `withStatement` 的 `As` 子句）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct WithBinding {
    /// 解构绑定目标：标识符或元组解构模式（对应 `identifier | bindingPattern`）
    pub pattern: BindingPattern,
    /// 可选的类型注解
    pub ty: Option<TypeExpr>,
    /// `As ...` 子句覆盖的源码区间
    pub span: Span,
}

/// with 语句（对应文法 `withStatement`：
/// `With expression (As (identifier | bindingPattern) typeAnnotation?)? block`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct WithStmt {
    /// 被管理的资源表达式（如文件、锁等）
    pub expr: Expression,
    /// 可选的 `As name: Type` 绑定子句
    pub binding: Option<WithBinding>,
    /// with 块
    pub block: Block,
    /// 整条 with 语句覆盖的源码区间
    pub span: Span,
}

/// 语句（对应文法 `statement`）。
///
/// 每个变体携带自身位置：变体若内嵌自带 `span` 的节点（如 `Block` / `IfStmt`）
/// 则直接取用，否则由变体自身的 `span` 字段承载，统一经 [`Statement::span`] 访问。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum Statement {
    /// 变量声明语句（对应 `variableStatement`）
    Variable(VariableStmt),
    /// 返回语句：`return expr?;`（对应 `returnStatement`）
    Return {
        /// 返回值，None 表示裸 `return`
        value: Option<Expression>,
        /// 语句覆盖的源码区间
        span: Span,
    },
    /// 跳出循环：`break;`（对应 `breakStatement`）
    Break {
        /// 语句覆盖的源码区间
        span: Span,
    },
    /// 跳过本次迭代：`continue;`（对应 `continueStatement`）
    Continue {
        /// 语句覆盖的源码区间
        span: Span,
    },
    /// 抛出异常：`throw expr;`（对应 `throwStatement`）
    Throw {
        /// 被抛出的表达式
        expr: Expression,
        /// 语句覆盖的源码区间
        span: Span,
    },
    /// 产出值：`yield expr?;`（对应 `yieldStatement`）
    Yield {
        /// 产出的值，None 表示裸 `yield`
        value: Option<Expression>,
        /// 语句覆盖的源码区间
        span: Span,
    },
    /// 语句块（对应 `block`）
    Block(Block),
    /// if 语句（对应 `ifStatement`）
    If(IfStmt),
    /// while 循环（对应 `whileStatement`，条件与循环体）
    While {
        /// 循环条件
        condition: Expression,
        /// 循环体语句
        body: Box<Statement>,
        /// 语句覆盖的源码区间
        span: Span,
    },
    /// for 循环（对应 `forStatement`）
    For(ForStmt),
    /// try 语句（对应 `tryStatement`）
    Try(TryStmt),
    /// with 语句（对应 `withStatement`）
    With(WithStmt),
    /// unsafe 语句块（对应 `unsafeStatement`）。
    ///
    /// 本语言唯一「unsafe 上下文」入口：解引用、指针算术、指针互转、
    /// 指针↔整数转换须位于块内（design.md §6.4）；块内仍是普通 `Statement`，
    /// 语法层不做白名单过滤。
    Unsafe {
        /// 块内语句序列（可嵌套 block / 其他语句）
        block: Box<Block>,
        /// `unsafe` 关键字到块结束的源码区间
        span: Span,
    },
    /// 表达式语句（对应 `expressionStatement`）
    Expression(Expression),
}

//! 表达式节点（对应文法 `expression`），按结构分层：
//!
//! - `literal`：字面量；
//! - `ops`：一元 / 二元 / 赋值运算符枚举；
//! - `lambda`：lambda 表达式（形参、体、属性赋值简写）；
//! - `match`：match 表达式（分支模式、分支体、分支）；
//! - 本文件：[`Expression`]——扁平化后的表达式树节点。
//!
//! 节点扁平化设计：一元 / 二元 / 赋值等运算各自成节点，递归字段用 `Box`
//! 承载以避免无限大小。所有变体都从本模块重新导出，下游继续写
//! `novella_ast::expr::Expression` 即可，无需关心文件划分。

mod lambda;
mod literal;
mod match_node;
mod ops;

pub use lambda::{Lambda, LambdaBody, LambdaParams, PropertyAssignment};
pub use literal::Literal;
pub use match_node::{Match, MatchCase, MatchCaseBody, MatchPattern};
pub use ops::{AssignOp, BinaryOp, UnaryOp};

use serde::{Deserialize, Serialize};

use novella_core::Span;

use crate::{Spanned, types::TypeExpr};

/// 表达式（对应文法 `expression`）。
///
/// 节点扁平化设计：一元 / 二元 / 赋值等运算各自成节点，
/// 递归字段用 `Box` 承载以避免无限大小，运算符以枚举集中定义
/// （与词法 token 一一对应，供 parser 直接映射）。
/// 每个变体携带 `span` 记录表达式覆盖的源码区间（供诊断定位），
/// 由 `Spanned` 派生统一生成 [`Expression::span`]。
#[derive(Debug, novella_derive::GuardedClone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum Expression {
    // —— 原子表达式 ——
    /// 字面量：`42`、`"hello"`、`true`、`null` 等（对应 `LiteralExpr`）
    Literal {
        /// 字面量值（保留源码原文）
        value: Literal,
        /// 字面量的源码区间
        span: Span,
    },
    /// 标识符引用：`foo`（对应 `IdentifierExpr`）
    Identifier {
        /// 标识符名
        name: String,
        /// 标识符的源码区间
        span: Span,
    },
    /// 括号表达式：`(a)`（对应 `ParenExpr`；注意与元组 `(a, b)` 区分）
    Paren {
        /// 括号内的表达式
        expr: Box<Expression>,
        /// 含括号的源码区间
        span: Span,
    },
    /// 元组表达式：`(0, 1)`（对应 `TupleExpr`）
    Tuple {
        /// 元组元素列表
        elements: Vec<Expression>,
        /// 元组的源码区间
        span: Span,
    },
    /// 数组字面量：`[1, 2, 3]`（对应 `ArrayLiteralExpr`）
    ArrayLiteral {
        /// 数组元素列表
        elements: Vec<Expression>,
        /// 数组字面量的源码区间
        span: Span,
    },
    /// 对象字面量：`{ a: 1, b }`（对应 `ObjectLiteralExpr`）
    ObjectLiteral {
        /// 属性赋值列表
        properties: Vec<PropertyAssignment>,
        /// 对象字面量的源码区间
        span: Span,
    },
    /// this 引用（对应 `ThisExpr`）
    This {
        /// `this` 的源码区间
        span: Span,
    },

    // —— 一元运算 ——
    /// 一元运算：`-x`、`!ok`、`i++` 等（对应 `PrefixExpr` / `PostfixExpr`）
    Unary {
        /// 运算符
        op: UnaryOp,
        /// 操作数
        operand: Box<Expression>,
        /// 一元表达式覆盖的源码区间
        span: Span,
    },
    /// await 表达式：`await p`（对应 `AwaitExpr`）
    Await {
        /// 被等待的表达式
        expr: Box<Expression>,
        /// await 表达式覆盖的源码区间
        span: Span,
    },

    // —— 二元运算 ——
    /// 二元运算：`a + b`、`x && y` 等（对应各二元运算产生式）
    Binary {
        /// 运算符
        op: BinaryOp,
        /// 左操作数
        left: Box<Expression>,
        /// 右操作数
        right: Box<Expression>,
        /// 二元表达式覆盖的源码区间
        span: Span,
    },
    /// 类型判断：`x is string`（对应 `IsExpr`；右操作数是类型表达式而非值表达式）
    Is {
        /// 被判断的表达式
        operand: Box<Expression>,
        /// 目标类型（对应 `typeExpression`）
        ty: TypeExpr,
        /// 类型判断表达式覆盖的源码区间
        span: Span,
    },
    /// 类型转换：`expr as Type`（对应 `CastExpr`，复用 `As` token）。
    ///
    /// 指针相关转换（指针↔整数、指针↔指针）须位于 unsafe 上下文，
    /// 纯数值转换不受限（design.md §6.3）。
    Cast {
        /// 被转换的表达式
        expr: Box<Expression>,
        /// 目标类型
        ty: TypeExpr,
        /// 转换表达式覆盖的源码区间
        span: Span,
    },
    /// struct 字段初始化构造：`Point{ x: 1, y: 2 }`（对应 `StructInitExpr`）。
    ///
    /// 复用 `objectLiteral` 的字段赋值形态；每个字段须可初始化（显式给出或
    /// 取字段声明 `initializer` 默认值）；无位置参数构造（design.md §5.4）。
    StructInit {
        /// 被构造的 struct 类型名
        name: String,
        /// 显式泛型实参（空列表表示非泛型类型或已实例化别名）。
        type_args: Vec<TypeExpr>,
        /// 字段初始化列表
        properties: Vec<PropertyAssignment>,
        /// 构造表达式覆盖的源码区间
        span: Span,
    },

    // —— 三元与赋值 ——
    /// 三元条件表达式：`a ? b : c`（对应 `TernaryExpr`）
    Ternary {
        /// 条件
        condition: Box<Expression>,
        /// 条件为真时的值
        then_expr: Box<Expression>,
        /// 条件为假时的值
        else_expr: Box<Expression>,
        /// 三元表达式覆盖的源码区间
        span: Span,
    },
    /// 赋值表达式：`a = 1`、`a += 2` 等（对应 `AssignmentExpr`）
    Assignment {
        /// 赋值目标（通常为标识符 / 成员访问 / 索引访问）
        target: Box<Expression>,
        /// 赋值运算符
        op: AssignOp,
        /// 被赋的值
        value: Box<Expression>,
        /// 赋值表达式覆盖的源码区间
        span: Span,
    },

    // —— 访问与调用 ——
    /// 成员访问：`obj.field`（对应 `MemberAccessExpr`）
    MemberAccess {
        /// 被访问的目标表达式
        target: Box<Expression>,
        /// 成员名
        name: String,
        /// 成员访问表达式覆盖的源码区间
        span: Span,
    },
    /// 索引访问：`arr[0]`（对应 `IndexAccessExpr`）
    IndexAccess {
        /// 被索引的目标表达式
        target: Box<Expression>,
        /// 索引表达式
        index: Box<Expression>,
        /// 索引访问表达式覆盖的源码区间
        span: Span,
    },
    /// 函数调用：`foo<T>(a, b)`（对应 `CallExpr`，含可选的 `typeGeneric?`）
    Call {
        /// 被调用的表达式（通常是标识符或成员访问）
        callee: Box<Expression>,
        /// 泛型实参列表（对应 `typeGeneric` 的 `typeArgumentList`），无泛型时为空
        type_args: Vec<TypeExpr>,
        /// 实参列表（对应 `argumentList`）
        args: Vec<Expression>,
        /// 调用表达式覆盖的源码区间
        span: Span,
    },
    /// 对象构造：`new Foo(1)`（对应 `NewExpr`）。
    ///
    /// 构造目标是 `new` 之后的整条后缀表达式（标识符链 / 括号 / 调用）；
    /// 若该表达式整体是一次调用，则其 callee 作为 `target`、`type_args` / `args`
    /// 提升为构造实参（文法 `NewExpr` 注释的约定），否则实参为空。
    New {
        /// 被构造的目标表达式
        target: Box<Expression>,
        /// 泛型实参列表（`new Foo<int>(1)` 的 `<int>`），无泛型时为空
        type_args: Vec<TypeExpr>,
        /// 构造实参列表
        args: Vec<Expression>,
        /// 构造表达式覆盖的源码区间
        span: Span,
    },
    // —— 高级表达式 ——
    /// match 表达式：`match x { ... }`（对应 `MatchExpr`）
    Match(Match),
    /// lambda 表达式：`(a, b) => a + b`（对应 `LambdaExpr`）
    Lambda(Lambda),
}

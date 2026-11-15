//! lambda 节点：形参形态、体形态与节点本身（对应文法 `lambdaExpression`）。

use serde::{Deserialize, Serialize};

use novella_core::Span;

use crate::{
    Spanned,
    stmt::Block,
    types::{Param, TypeExpr},
};

use super::Expression;

/// 对象字面量的属性赋值（对应文法 `propertyAssignment`）
///
/// `{ a: 1, b }` 中的 `b` 为简写形态：`value` 为 None，
/// 语义等价于 `b: b`，由语义分析阶段展开。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct PropertyAssignment {
    /// 属性名
    pub name: String,
    /// 属性值表达式；None 表示简写形态 `{foo}`（等价 `foo: foo`）
    pub value: Option<Expression>,
    /// 属性赋值覆盖的源码区间
    pub span: Span,
}

/// lambda 参数形态（对应文法 `lambdaSignature`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum LambdaParams {
    /// 单参数简写：`x => ...`
    Single {
        /// 参数名
        name: String,
        /// 参数标识符的源码区间
        span: Span,
    },
    /// 参数列表：`(a, b) => ...`（对应 `parameterList`）
    List {
        /// 形参列表
        params: Vec<Param>,
        /// 括号参数列表的源码区间
        span: Span,
    },
}

/// lambda 函数体（对应文法 `lambdaExpression` 的 `block | expression`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum LambdaBody {
    /// 表达式体：`=> a + b`
    Expr {
        /// 表达式
        expr: Box<Expression>,
        /// 表达式体的源码区间
        span: Span,
    },
    /// 语句块体：`=> { ... }`
    Block {
        /// 语句块
        block: Block,
        /// 语句块体的源码区间
        span: Span,
    },
}

/// lambda 表达式（对应文法
/// `lambdaExpression: Async? lambdaSignature typeAnnotation? Arrow (block | expression)`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct Lambda {
    /// 是否为异步 lambda（对应文法 `Async?`）
    pub is_async: bool,
    /// 参数形态（单标识符简写或参数列表）
    pub params: LambdaParams,
    /// 返回类型注解（对应文法 `typeAnnotation?`），None 表示缺省
    pub return_type: Option<TypeExpr>,
    /// 函数体（表达式或语句块）
    pub body: LambdaBody,
    /// lambda 表达式覆盖的源码区间
    pub span: Span,
}

//! match 节点：分支模式、分支体、分支与 match 表达式本身（对应文法 `matchExpression`）。

use serde::{Deserialize, Serialize};

use novella_core::Span;

use crate::{Spanned, stmt::Block, types::TypeExpr};

use super::Expression;

/// match 分支的模式（对应文法 `matchCase` 的 `expression | typeAnnotation`）
///
/// `typeAnnotation` 即 `: TypeExpr` 形态，故 `case : T => ...` 表示按类型匹配。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum MatchPattern {
    /// 表达式模式：`case value => ...`
    Expr {
        /// 模式表达式
        expr: Box<Expression>,
        /// 模式表达式的源码区间
        span: Span,
    },
    /// 类型注解模式：`case : T => ...`
    Type {
        /// 模式类型
        ty: TypeExpr,
        /// 类型注解的源码区间
        span: Span,
    },
}

/// match 分支的函数体（对应文法 `matchCase` / `matchDefault` 的 `'=>' expression | block`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum MatchCaseBody {
    /// 表达式体：`=> value`
    Expr {
        /// 表达式
        expr: Box<Expression>,
        /// 表达式体的源码区间
        span: Span,
    },
    /// 语句块体：`{ ... }`
    Block {
        /// 语句块
        block: Block,
        /// 语句块体的源码区间
        span: Span,
    },
}

/// match 分支（对应文法 `matchCase`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct MatchCase {
    /// 匹配模式
    pub pattern: MatchPattern,
    /// 命中后的分支体
    pub body: MatchCaseBody,
    /// 分支整体（`case ... => ...`）覆盖的源码区间
    pub span: Span,
}

/// match 表达式（对应文法
/// `matchExpression: Match expression '{' matchCase+ matchDefault? '}'`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct Match {
    /// 被匹配的表达式（`match` 后的 `expression`）
    pub subject: Box<Expression>,
    /// 具名分支列表（对应 `matchCase+`）
    pub cases: Vec<MatchCase>,
    /// 兜底分支（对应 `matchDefault?`），None 表示没有 `default`
    pub default: Option<MatchCaseBody>,
    /// match 表达式覆盖的源码区间
    pub span: Span,
}

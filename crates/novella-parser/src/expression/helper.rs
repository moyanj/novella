//! 表达式解析的共享小工具：二元 / 赋值节点构造、token 种类判定。
//!
//! 这些函数不依赖解析器状态，被优先级阶梯与后缀扫描多处调用。

use novella_ast::expr::{AssignOp, BinaryOp, Expression};
use novella_core::{Span, token::TokenKind};

/// token 种类是否可能出现在类型实参列表中（用于判定 `<` 是否为泛型起点）。
///
/// 须覆盖文法 `typeExpression` 能包含的全部 token 种类：`func (a: int) -> int`
/// 形态需要 `Func` 与 `Colon`（B-07）。
pub(crate) fn is_type_token(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Identifier
            | TokenKind::Null
            | TokenKind::Func
            | TokenKind::Colon
            | TokenKind::OpenParen
            | TokenKind::CloseParen
            | TokenKind::OpenBracket
            | TokenKind::CloseBracket
            | TokenKind::LessThan
            | TokenKind::MoreThan
            | TokenKind::Comma
            | TokenKind::Multiply
            | TokenKind::BitOr
            | TokenKind::ReturnArrow
    )
}

/// token 种类到赋值运算符的映射（非赋值 token 返回 `None`）。
pub(crate) fn assign_op(kind: TokenKind) -> Option<AssignOp> {
    Some(match kind {
        TokenKind::Assign => AssignOp::Assign,
        TokenKind::PlusAssign => AssignOp::PlusAssign,
        TokenKind::MinusAssign => AssignOp::MinusAssign,
        TokenKind::MultiplyAssign => AssignOp::MultiplyAssign,
        TokenKind::DivideAssign => AssignOp::DivideAssign,
        TokenKind::ModulusAssign => AssignOp::ModulusAssign,
        TokenKind::BitAndAssign => AssignOp::BitAndAssign,
        TokenKind::BitOrAssign => AssignOp::BitOrAssign,
        TokenKind::BitXorAssign => AssignOp::BitXorAssign,
        TokenKind::LeftShiftArithmeticAssign => AssignOp::LeftShiftAssign,
        TokenKind::RightShiftArithmeticAssign => AssignOp::RightShiftAssign,
        _ => return None,
    })
}

/// 把二元运算的两侧组装为 [`Expression::Binary`]。
pub(crate) fn binary(op: BinaryOp, left: Expression, right: Expression) -> Expression {
    let span = Span {
        start: left.span().start,
        end: right.span().end,
    };
    Expression::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
        span,
    }
}

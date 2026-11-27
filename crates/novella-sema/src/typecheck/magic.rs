//! 魔法方法协议：声明形状、静态派发与下游调用选择。

use std::rc::Rc;

use novella_ast::expr::{AssignOp, BinaryOp, Expression, Literal, UnaryOp};
use novella_core::Span;

use crate::ty::{FunctionSig, Ty};
use crate::{OperatorCall, Sema};

use super::body::BodyCtx;
use super::call::protocol_method;
use super::expr::{check_expr, check_expr_with_hint};
use super::member::MemberHit;

// 正向、反向、复合赋值。关系比较反向时交换关系；相等与不等是对称协议。
type BinaryProtocol = (
    BinaryOp,
    &'static str,
    &'static str,
    Option<(AssignOp, &'static str)>,
);
const BINARY: &[BinaryProtocol] = &[
    (
        BinaryOp::Plus,
        "__add",
        "__radd",
        Some((AssignOp::PlusAssign, "__iadd")),
    ),
    (
        BinaryOp::Minus,
        "__sub",
        "__rsub",
        Some((AssignOp::MinusAssign, "__isub")),
    ),
    (
        BinaryOp::Multiply,
        "__mul",
        "__rmul",
        Some((AssignOp::MultiplyAssign, "__imul")),
    ),
    (
        BinaryOp::Divide,
        "__div",
        "__rdiv",
        Some((AssignOp::DivideAssign, "__idiv")),
    ),
    (
        BinaryOp::Modulus,
        "__mod",
        "__rmod",
        Some((AssignOp::ModulusAssign, "__imod")),
    ),
    (
        BinaryOp::LeftShift,
        "__lshift",
        "__rlshift",
        Some((AssignOp::LeftShiftAssign, "__ilshift")),
    ),
    (
        BinaryOp::RightShift,
        "__rshift",
        "__rrshift",
        Some((AssignOp::RightShiftAssign, "__irshift")),
    ),
    (
        BinaryOp::BitAnd,
        "__and",
        "__rand",
        Some((AssignOp::BitAndAssign, "__iand")),
    ),
    (
        BinaryOp::BitOr,
        "__or",
        "__ror",
        Some((AssignOp::BitOrAssign, "__ior")),
    ),
    (
        BinaryOp::BitXor,
        "__xor",
        "__rxor",
        Some((AssignOp::BitXorAssign, "__ixor")),
    ),
    (BinaryOp::Equals, "__eq", "__eq", None),
    (BinaryOp::NotEquals, "__ne", "__ne", None),
    (BinaryOp::LessThan, "__lt", "__gt", None),
    (BinaryOp::LessThanEquals, "__le", "__ge", None),
    (BinaryOp::GreaterThan, "__gt", "__lt", None),
    (BinaryOp::GreaterThanEquals, "__ge", "__le", None),
];

const UNARY: &[(UnaryOp, &str)] = &[
    (UnaryOp::Plus, "__pos"),
    (UnaryOp::Minus, "__neg"),
    (UnaryOp::Not, "__not"),
    (UnaryOp::BitNot, "__invert"),
    (UnaryOp::PostIncrement, "__inc"),
    (UnaryOp::PostDecrement, "__dec"),
];

pub(crate) const GET_ITEM: &str = "__getitem";
pub(crate) const SET_ITEM: &str = "__setitem";

/// 返回显式参数数量，以及返回类型是否必须为 `boolean`（`__not`、比较与 `__ne`）。
pub(crate) fn method_shape(name: &str) -> Option<(usize, bool)> {
    if name == GET_ITEM {
        return Some((1, false));
    }
    // 下标写入是过程式协议：返回 void，由返回注解检查单独把关。
    if name == SET_ITEM {
        return Some((2, false));
    }
    if UNARY.iter().any(|(_, method)| *method == name) {
        return Some((0, name == "__not"));
    }
    BINARY.iter().find_map(|(_, direct, reverse, compound)| {
        (*direct == name || *reverse == name || compound.is_some_and(|(_, method)| method == name))
            .then_some((1, compound.is_none()))
    })
}

pub(super) struct Selected {
    call: OperatorCall,
    hit: MemberHit,
    sig: Rc<FunctionSig>,
}

impl Selected {
    fn find(sema: &Sema, receiver: &Ty, method: &'static str, reversed: bool) -> Option<Self> {
        let (hit, sig) = protocol_method(sema, receiver, method)?;
        Some(Self {
            call: OperatorCall {
                method,
                reversed,
                invert: false,
            },
            hit,
            sig,
        })
    }

    pub(super) fn is_reversed(&self) -> bool {
        self.call.reversed
    }

    pub(super) fn finish(
        self,
        sema: &mut Sema,
        ctx: &BodyCtx,
        span: Span,
        operand: Option<(&Ty, Span)>,
    ) -> Ty {
        self.hit.check_visible(sema, ctx, self.call.method, span);
        if let Some((actual, site)) = operand {
            let Some(param) = self.sig.params.get(1) else {
                return Ty::Error;
            };
            if !sema.is_assignable(&param.ty, actual) {
                sema.type_error(
                    ctx.module,
                    site,
                    format!(
                        "运算符方法 `{}` 的操作数需要 `{}`，实为 `{actual}`",
                        self.call.method, param.ty
                    ),
                );
            }
        }
        ctx.record_possible_exception();
        sema.operator_calls.insert((ctx.module, span), self.call);
        self.sig.ret.clone()
    }

    pub(super) fn check_argument(
        &self,
        sema: &mut Sema,
        ctx: &mut BodyCtx,
        expr: &Expression,
    ) -> Ty {
        match self.sig.params.get(1) {
            Some(param) => check_expr_with_hint(sema, ctx, expr, &param.ty),
            None => check_expr(sema, ctx, expr),
        }
    }

    /// 反向调用的左实参已检查过，只补普通调用允许的 trait 装箱，不重复访问 AST。
    pub(super) fn checked_argument(
        &self,
        sema: &mut Sema,
        ctx: &BodyCtx,
        span: Span,
        ty: Ty,
    ) -> Ty {
        match self.sig.params.get(1) {
            Some(param) if sema.record_dyn_conversion(ctx.module, span, &param.ty, &ty) => {
                param.ty.clone()
            }
            _ => ty,
        }
    }
}

pub(super) fn unary(sema: &Sema, op: UnaryOp, ty: &Ty) -> Option<Selected> {
    let (_, method) = UNARY.iter().find(|(item, _)| *item == op)?;
    Selected::find(sema, ty, method, false)
}

pub(super) fn binary(sema: &Sema, op: BinaryOp, ty: &Ty, reversed: bool) -> Option<Selected> {
    let (_, direct, reverse, _) = BINARY.iter().find(|(item, ..)| *item == op)?;
    Selected::find(sema, ty, if reversed { reverse } else { direct }, reversed)
}

pub(super) fn inverted_equality(sema: &Sema, ty: &Ty, reversed: bool) -> Option<Selected> {
    let mut selected = binary(sema, BinaryOp::Equals, ty, reversed)?;
    selected.call.invert = true;
    Some(selected)
}

pub(super) fn compound(op: AssignOp) -> Option<(BinaryOp, &'static str)> {
    BINARY.iter().find_map(|(binary, _, _, compound)| {
        let (assign, method) = (*compound)?;
        (assign == op).then_some((*binary, method))
    })
}

pub(super) fn compound_method(sema: &Sema, ty: &Ty, method: &'static str) -> Option<Selected> {
    Selected::find(sema, ty, method, false)
}

pub(super) fn is_null_literal(mut expr: &Expression) -> bool {
    while let Expression::Paren { expr: inner, .. } = expr {
        expr = inner;
    }
    matches!(
        expr,
        Expression::Literal {
            value: Literal::Null,
            ..
        }
    )
}

pub(super) fn check_writeback(
    sema: &mut Sema,
    ctx: &BodyCtx,
    span: Span,
    target: &Ty,
    result: &Ty,
) {
    if !result.is_opaque() && !sema.is_assignable(target, result) {
        sema.type_error(
            ctx.module,
            span,
            format!("运算结果 `{result}` 不能写回 `{target}`"),
        );
    }
}

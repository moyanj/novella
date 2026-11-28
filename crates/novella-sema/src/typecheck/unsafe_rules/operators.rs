//! 二元运算、类型合一、显式转换与 unsafe。
use super::*;
use crate::module::ModuleId;
use crate::typecheck::magic;

/// S10：指针类操作需要 `unsafe {}`。
pub(crate) fn require_unsafe(sema: &mut Sema, ctx: &BodyCtx, span: Span, what: &str) {
    if !ctx.in_unsafe {
        sema.unsafe_error(
            ctx.module,
            span,
            format!("{what}是不安全操作，需要 `unsafe {{ }}` 块"),
        );
    }
}
pub(crate) fn check_binary(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: BinaryOp,
    left: &Expression,
    right: &Expression,
    span: Span,
) -> Ty {
    check_binary_with_hint(sema, ctx, op, left, right, span, None)
}

/// 带期望类型的二元检查（plan §7 S03）。
///
/// 只有 `&&` / `||` / `??` 需要下推：它们的操作数类型不是「由自身决定」而是由
/// 运算符或另一侧决定——`c && []` 里的空数组要靠 boolean 上下文报「不是 boolean」，
/// `p ?? null` 里的 `null` 靠左操作数类型定型。算术 / 比较等两侧都自查，忽略 `hint`。
pub(crate) fn check_binary_with_hint(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: BinaryOp,
    left: &Expression,
    right: &Expression,
    span: Span,
    hint: Option<&Ty>,
) -> Ty {
    // 逻辑运算两侧都是 boolean（来自运算符本身的约束，与调用方的期望无关）
    if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
        let boolean = Ty::Boolean;
        let lhs = check_expr_with_hint(sema, ctx, left, &boolean);
        let before = ctx.uninitialized.clone();
        let executes = crate::typecheck::flow::constant_bool(left).map(|value| {
            if op == BinaryOp::LogicalAnd {
                value
            } else {
                !value
            }
        });
        let reachable = ctx.reachable;
        ctx.reachable &= executes != Some(false);
        let rhs = check_expr_with_hint(sema, ctx, right, &boolean);
        ctx.reachable = reachable;
        match executes {
            Some(true) => {}
            Some(false) => ctx.uninitialized = before,
            None => ctx.uninitialized.extend(before),
        }
        return check_binary_operands(sema, ctx, op, left, right, span, lhs, rhs);
    }
    let lhs = check_expr(sema, ctx, left);
    // 与 null 字面量的比较继续检测引用是否为空，不调用可能解引用接收者的方法。
    let equality = matches!(op, BinaryOp::Equals | BinaryOp::NotEquals);
    let dispatch = !(equality && (matches!(lhs, Ty::Null) || magic::is_null_literal(right)));
    if dispatch && let Some(selected) = magic::binary(sema, op, &lhs, false) {
        let rhs = selected.check_argument(sema, ctx, right);
        return selected.finish(sema, ctx, span, Some((&rhs, right.span())));
    }
    // `??` 的备用值：优先按左操作数类型，其次按调用方期望类型（左为纯 `null` 时）
    // 借用期：`lhs` 在下方矩阵里仍要用，故克隆成 `Option<Ty>` 而不是借用
    let right_hint: Option<Ty> = match op {
        BinaryOp::NullCoalesce => {
            if lhs.is_opaque() || matches!(lhs, Ty::Null) {
                hint.cloned()
            } else {
                Some(lhs.clone())
            }
        }
        _ => None,
    };
    let before = ctx.uninitialized.clone();
    let rhs = match &right_hint {
        Some(hint) => check_expr_with_hint(sema, ctx, right, hint),
        None => check_expr(sema, ctx, right),
    };
    if op == BinaryOp::NullCoalesce && !matches!(lhs, Ty::Null) {
        ctx.uninitialized.extend(before);
    }
    if dispatch && !(equality && matches!(rhs, Ty::Null)) {
        let selected = magic::binary(sema, op, &rhs, true).or_else(|| {
            (op == BinaryOp::NotEquals)
                .then(|| {
                    magic::inverted_equality(sema, &lhs, false)
                        .or_else(|| magic::inverted_equality(sema, &rhs, true))
                })
                .flatten()
        });
        if let Some(selected) = selected {
            return if selected.is_reversed() {
                let lhs = selected.checked_argument(sema, ctx, left.span(), lhs);
                selected.finish(sema, ctx, span, Some((&lhs, left.span())))
            } else {
                let rhs = selected.checked_argument(sema, ctx, right.span(), rhs);
                selected.finish(sema, ctx, span, Some((&rhs, right.span())))
            };
        }
    }
    check_binary_operands(sema, ctx, op, left, right, span, lhs, rhs)
}

/// 二元运算的操作数已定型后的矩阵判定（`check_binary*` 的公共尾部）。
#[allow(clippy::too_many_arguments)] // 参数是二元检查上下文的自然拆分（同 resolve_type_reference）
pub(super) fn check_binary_operands(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: BinaryOp,
    left: &Expression,
    right: &Expression,
    span: Span,
    lhs: Ty,
    rhs: Ty,
) -> Ty {
    // 一侧不可判定时不做检查（可恢复原则）
    if lhs.is_opaque() || rhs.is_opaque() {
        return match op {
            BinaryOp::LogicalAnd
            | BinaryOp::LogicalOr
            | BinaryOp::Equals
            | BinaryOp::NotEquals
            | BinaryOp::IdentityEquals
            | BinaryOp::IdentityNotEquals
            | BinaryOp::LessThan
            | BinaryOp::LessThanEquals
            | BinaryOp::GreaterThan
            | BinaryOp::GreaterThanEquals => Ty::Boolean,
            _ => Ty::Unknown,
        };
    }
    // 投影可作为不透明值传递，但未固定其实际类型时不能用于算术。
    if matches!(
        op,
        BinaryOp::Plus
            | BinaryOp::Minus
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Modulus
    ) {
        let mut invalid = false;
        for (operand, ty) in [(left, &lhs), (right, &rhs)] {
            if matches!(ty, Ty::Associated { .. }) {
                sema.type_error(
                    ctx.module,
                    operand.span(),
                    format!(
                        "未约束的关联类型 `{ty}` 不能当作具体类型参与 `{}` 运算",
                        binary_op_text(op)
                    ),
                );
                invalid = true;
            }
        }
        if invalid {
            return Ty::Error;
        }
    }
    match op {
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
            if !matches!(lhs, Ty::Boolean) || !matches!(rhs, Ty::Boolean) {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的两个操作数都必须是 `boolean`（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
            }
            Ty::Boolean
        }
        BinaryOp::NullCoalesce => {
            if !lhs.is_nullable() {
                sema.type_error(
                    ctx.module,
                    left.span(),
                    format!("`??` 的左操作数必须可空，实为 `{lhs}`"),
                );
            }
            // 空合并只选择备用值；指针非空时不隐式解引用。
            match &lhs {
                ty if ty.is_nullable() && !matches!(ty, Ty::Null) => {
                    if !sema.is_assignable(&lhs, &rhs) {
                        sema.type_error(
                            ctx.module,
                            right.span(),
                            format!("空合并的备用值需要 `{lhs}`，实为 `{rhs}`"),
                        );
                    }
                    lhs
                }
                Ty::Null => rhs,
                _ => rhs,
            }
        }
        BinaryOp::Equals
        | BinaryOp::NotEquals
        | BinaryOp::IdentityEquals
        | BinaryOp::IdentityNotEquals => {
            // struct 没有内建值相等，也没有引用身份。
            for (side, ty) in [("左", &lhs), ("右", &rhs)] {
                if matches!(crate::traits::nominal_base(ty), Ty::Struct(..)) {
                    sema.type_error(
                        ctx.module,
                        span,
                        format!("struct 类型 `{ty}` 没有该比较运算的定义（{side}操作数）"),
                    );
                }
            }
            let width = sema.target.pointer_width;
            if let Some(common) = unify_numeric(&lhs, &rhs, width) {
                if check_numeric_operands(
                    sema,
                    ctx.module,
                    &common,
                    [(&lhs, left.span()), (&rhs, right.span())],
                ) {
                    sema.record_binary_numeric(ctx.module, span, common);
                }
            } else if !comparable(&lhs, &rhs, width) {
                sema.type_error(ctx.module, span, numeric_mismatch_text(&lhs, &rhs, "比较"));
            }
            Ty::Boolean
        }
        BinaryOp::LessThan
        | BinaryOp::LessThanEquals
        | BinaryOp::GreaterThan
        | BinaryOp::GreaterThanEquals => {
            check_numeric_binary(
                sema,
                ctx.module,
                op,
                span,
                [(&lhs, left.span()), (&rhs, right.span())],
            );
            Ty::Boolean
        }
        BinaryOp::Plus | BinaryOp::Minus | BinaryOp::Multiply | BinaryOp::Divide => {
            check_arithmetic(
                sema,
                ctx,
                op,
                [(&lhs, left.span()), (&rhs, right.span())],
                span,
            )
        }
        BinaryOp::Modulus | BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr => {
            check_numeric_binary(
                sema,
                ctx.module,
                op,
                span,
                [(&lhs, left.span()), (&rhs, right.span())],
            )
        }
        BinaryOp::LeftShift | BinaryOp::RightShift => {
            if !lhs.is_integer() || !rhs.is_integer() {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的两个操作数都必须是整数（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
                Ty::Unknown
            } else {
                concrete(&lhs)
            }
        }
    }
}
type NumericOperands<'a> = [(&'a Ty, Span); 2];

/// 加减乘除：数值提升 + S7 指针算术。
pub(super) fn check_arithmetic(
    sema: &mut Sema,
    ctx: &BodyCtx,
    op: BinaryOp,
    operands: NumericOperands<'_>,
    span: Span,
) -> Ty {
    let [(lhs, _), (rhs, _)] = operands;
    // S7：指针 ± 整数 → 同型指针（unsafe）
    if [lhs, rhs]
        .iter()
        .any(|ty| matches!(ty, Ty::Pointer(inner) if matches!(**inner, Ty::Void)))
    {
        sema.type_error(
            ctx.module,
            span,
            "void* 不支持指针算术，请先转换为具体指针类型",
        );
        return Ty::Error;
    }
    // 数值路径才有公共类型可记录；指针分支单独返回，不进入下文。
    if matches!(op, BinaryOp::Plus | BinaryOp::Minus) {
        if let (Ty::Pointer(_), b) = (lhs, rhs)
            && b.is_integer()
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return lhs.clone();
        }
        if matches!(op, BinaryOp::Plus)
            && let (Ty::Pointer(_), a) = (rhs, lhs)
            && a.is_integer()
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return rhs.clone();
        }
        // 指针 - 指针 → isize（S7，要求同 pointee）
        if matches!(op, BinaryOp::Minus)
            && let (Ty::Pointer(a), Ty::Pointer(b)) = (lhs, rhs)
            && (ty_eq(a, b) || matches!(**a, Ty::Void) || matches!(**b, Ty::Void))
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return Ty::Int(crate::ty::IntKind::Isize);
        }
    }
    if matches!(op, BinaryOp::LeftShift | BinaryOp::RightShift) {
        if lhs.is_integer() && rhs.is_integer() {
            if !sema.is_assignable(lhs, rhs) {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("移位量 `{rhs}` 超出 `{lhs}` 的范围"),
                );
                return Ty::Error;
            }
            return concrete(lhs);
        }
        sema.type_error(
            ctx.module,
            span,
            format!("移位运算需要整数（实为 `{lhs}` 与 `{rhs}`）"),
        );
        return Ty::Error;
    }
    check_numeric_binary(sema, ctx.module, op, span, operands)
}

pub(super) fn check_numeric_binary(
    sema: &mut Sema,
    module: ModuleId,
    op: BinaryOp,
    span: Span,
    operands: NumericOperands<'_>,
) -> Ty {
    let [(lhs, _), (rhs, _)] = operands;
    if matches!(op, BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr)
        && (!lhs.is_integer() || !rhs.is_integer())
    {
        sema.type_error(
            module,
            span,
            format!(
                "`{}` 只能用于整数类型（实为 `{lhs}` 与 `{rhs}`）",
                binary_op_text(op)
            ),
        );
        return Ty::Error;
    }
    let Some(common) = unify_numeric(lhs, rhs, sema.target.pointer_width) else {
        sema.type_error(
            module,
            span,
            numeric_mismatch_text(lhs, rhs, &format!("`{}`", binary_op_text(op))),
        );
        return Ty::Error;
    };
    if !check_numeric_operands(sema, module, &common, operands) {
        return Ty::Error;
    }
    sema.record_binary_numeric(module, span, common.clone());
    common
}

fn check_numeric_operands(
    sema: &mut Sema,
    module: ModuleId,
    common: &Ty,
    operands: NumericOperands<'_>,
) -> bool {
    // 两侧都是字面量时留给外层上下文定型，不在此套用默认位宽。
    if !matches!(common, Ty::Int(_) | Ty::Float(_)) {
        return true;
    }
    let mut valid = true;
    for (operand, span) in operands {
        if !sema.is_assignable(common, operand) {
            sema.type_error(
                module,
                span,
                format!("操作数 `{operand}` 超出运算类型 `{common}` 的范围；请显式 `as`"),
            );
            valid = false;
        }
    }
    valid
}

/// 数值统一：字面量折叠 + 已定型数值求最小无损公共类型。
///
/// 字面量优先落到另一侧的具体类型，再由调用方按目标范围检查；两个字面量
/// 保持字面量态，交给上下文定型。int/float 两族不互相提升。
pub(crate) fn unify_numeric(a: &Ty, b: &Ty, width: crate::PointerWidth) -> Option<Ty> {
    match (a, b) {
        (Ty::IntLiteral(..), Ty::Int(k)) | (Ty::Int(k), Ty::IntLiteral(..)) => Some(Ty::Int(*k)),
        (Ty::IntLiteral(an, a), Ty::IntLiteral(bn, b)) => {
            Some(Ty::IntLiteral(*an || *bn, *a.max(b)))
        }
        (Ty::FloatLiteral(_), Ty::Float(k)) | (Ty::Float(k), Ty::FloatLiteral(_)) => {
            Some(Ty::Float(*k))
        }
        (Ty::FloatLiteral(a), Ty::FloatLiteral(b)) => Some(Ty::FloatLiteral(a.max(*b))),
        _ => crate::ty::numeric_common(a, b, width),
    }
}
/// 二元数值运算无公共类型时的可操作文案。
fn numeric_mismatch_text(lhs: &Ty, rhs: &Ty, what: &str) -> String {
    if lhs.is_numeric() && rhs.is_numeric() {
        if (lhs.is_integer() && rhs.is_float()) || (lhs.is_float() && rhs.is_integer()) {
            return format!(
                "{what} 的整数与浮点操作数不能隐式混合（`{lhs}` 与 `{rhs}`）；请显式 `as`"
            );
        }
        return format!(
            "{what} 的操作数 `{lhs}` 与 `{rhs}` 没有能同时无损容纳二者的公共类型；请显式 `as`"
        );
    }
    format!("{what} 的操作数类型不兼容（`{lhs}` 与 `{rhs}`）")
}
/// 相等比较的可比性：同型 / 字面量折叠 / null 与可空。
pub(crate) fn comparable(a: &Ty, b: &Ty, width: crate::PointerWidth) -> bool {
    if a.is_opaque() || b.is_opaque() {
        return true;
    }
    if ty_eq(a, b) || unify_numeric(a, b, width).is_some() {
        return true;
    }
    (matches!(a, Ty::Null) && b.is_nullable()) || (matches!(b, Ty::Null) && a.is_nullable())
}
/// 通用类型合一（三元/数组元素用）：等价 / 数值统一 / 单向可赋值。
pub(crate) fn unify_types(a: &Ty, b: &Ty, width: crate::PointerWidth) -> Option<Ty> {
    if a.is_opaque() {
        return Some(b.clone());
    }
    if b.is_opaque() {
        return Some(a.clone());
    }
    if ty_eq(a, b) {
        return Some(a.clone());
    }
    if let Some(ty) = unify_numeric(a, b, width) {
        return Some(ty);
    }
    if matches!(a, Ty::Null) && b.is_nullable() {
        return Some(b.clone());
    }
    if matches!(b, Ty::Null) && a.is_nullable() {
        return Some(a.clone());
    }
    None
}
/// `as` 显式转换（design.md §6.3）：数值互转安全；涉指针的转换 unsafe。
pub(crate) fn check_cast(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    expr: &Expression,
    ty: &TypeExpr,
    span: Span,
) -> Ty {
    let source = check_expr(sema, ctx, expr);
    let target = sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
    if source.is_opaque() || target.is_opaque() {
        return target;
    }
    if !ty_eq(&source, &target) && sema.record_dyn_conversion(ctx.module, span, &target, &source) {
        return target;
    }
    // 数值互转（含 int→float）本身安全；null 可显式转任意可空类型
    let ok = if ty_eq(&source, &target)
        || (source.is_numeric() && target.is_numeric())
        || (matches!(source, Ty::Null) && target.is_nullable())
    {
        true
    } else if (matches!(target, Ty::Pointer(_))
        && (matches!(source, Ty::Pointer(_)) || source.is_integer()))
        || (matches!(source, Ty::Pointer(_)) && target.is_integer())
    {
        require_unsafe(sema, ctx, span, "指针显式转换");
        true
    } else if reference_type(&source) && reference_type(&target) {
        if sema.is_subclass(&target, &source) {
            true // 向上转型（实际可隐式）
        } else if sema.is_subclass(&source, &target) {
            require_unsafe(sema, ctx, span, "向下转型");
            true
        } else {
            false
        }
    } else {
        false
    };
    if !ok {
        sema.type_error(
            ctx.module,
            span,
            format!("无法把 `{source}` 转换为 `{target}`"),
        );
    }
    target
}

fn reference_type(ty: &Ty) -> bool {
    match ty {
        Ty::Applied(base, _) => reference_type(base),
        Ty::Class(..) | Ty::Trait(..) => true,
        _ => false,
    }
}

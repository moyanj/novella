//! 一元运算、指针和局部槽位取址。
use super::*;

pub(crate) fn check_unary(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: UnaryOp,
    operand: &Expression,
    span: Span,
) -> Ty {
    // 对局部槽位取址不读取其当前值；索引/成员定位仍照常检查所需的基座值。
    fn local_slot(sema: &Sema, ctx: &BodyCtx, expr: &Expression) -> Option<Span> {
        match expr {
            Expression::Paren { expr, .. } => local_slot(sema, ctx, expr),
            Expression::Identifier { name, .. } => {
                sema.scopes.lookup(ctx.scope, name).map(|s| s.name_span)
            }
            _ => None,
        }
    }
    let address_slot = if op == UnaryOp::AddressOf {
        local_slot(sema, ctx, operand).filter(|span| ctx.uninitialized.remove(span))
    } else {
        None
    };
    let operand_ty = check_expr(sema, ctx, operand);
    if let Some(slot) = address_slot {
        ctx.uninitialized.insert(slot);
    }
    match op {
        // `-128` / `+5` 这类「字面量取符号」要保留未定型态：字面量的实际位宽由
        // 使用它的上下文（标注 / 形参 / 返回类型）决定，提前 concrete 成 `i32`
        // 会让 `let x: i8 = -128;` 误报（i32 不可赋给 i8，见矩阵 L04）。
        UnaryOp::Plus | UnaryOp::Minus => {
            if operand_ty.is_numeric() {
                match (op, operand_ty) {
                    // 字面量保持未定型态，实际位宽由使用它的上下文决定。
                    // `-x` 记下取负态（载荷仍是绝对值），负数下限在使用点判定（L04）。
                    (UnaryOp::Minus, Ty::IntLiteral(negative, value)) => {
                        Ty::IntLiteral(!negative, value)
                    }
                    (UnaryOp::Plus, Ty::IntLiteral(negative, value)) => {
                        Ty::IntLiteral(negative, value)
                    }
                    (UnaryOp::Minus, Ty::FloatLiteral(value)) => Ty::FloatLiteral(-value),
                    (UnaryOp::Plus, Ty::FloatLiteral(value)) => Ty::FloatLiteral(value),
                    (_, other) => concrete(&other),
                }
            } else if operand_ty.is_opaque() {
                operand_ty
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "一元 `{}` 只能用于数值类型，实为 `{operand_ty}`",
                        unary_op_text(op)
                    ),
                );
                Ty::Error
            }
        }
        UnaryOp::Not => {
            if !matches!(operand_ty, Ty::Boolean) && !operand_ty.is_opaque() {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("`not` 只能用于 `boolean`，实为 `{operand_ty}`"),
                );
            }
            Ty::Boolean
        }
        UnaryOp::BitNot => {
            if operand_ty.is_integer() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                operand_ty
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("`~` 只能用于整数类型，实为 `{operand_ty}`"),
                );
                Ty::Error
            }
        }
        UnaryOp::PostIncrement | UnaryOp::PostDecrement => {
            use crate::typecheck::call::{check_index_signature, index_parts, is_custom_index};
            let overload = index_parts(operand).and_then(|(receiver, _)| {
                sema.expression_types
                    .get(&(ctx.module, receiver.span()))
                    .filter(|ty| is_custom_index(sema, ty))
                    .cloned()
            });
            if let Some(receiver) = overload {
                if let Some(setter) = check_index_signature(sema, ctx, &receiver, "__setitem", span)
                {
                    crate::typecheck::unsafe_rules::check_index_update_types(
                        sema, ctx, &receiver, &setter, span,
                    );
                }
            } else if !is_lvalue(sema, ctx, operand) {
                sema.type_error(ctx.module, operand.span(), "自增/自减需要可写的左值");
            }
            if let Ty::Pointer(inner) = &operand_ty {
                require_unsafe(sema, ctx, span, "指针自增/自减");
                if matches!(**inner, Ty::Void) {
                    sema.type_error(
                        ctx.module,
                        span,
                        "void* 不支持指针自增/自减，请先转换为具体指针类型",
                    );
                    Ty::Error
                } else {
                    operand_ty
                }
            } else if operand_ty.is_numeric() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                operand_ty
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("自增/自减只能用于数值类型，实为 `{operand_ty}`"),
                );
                Ty::Error
            }
        }
        UnaryOp::AddressOf => {
            // weak 槽位保存控制块句柄，不能作为载荷地址暴露给裸指针操作。
            fn weak_field(sema: &Sema, ctx: &BodyCtx, expr: &Expression) -> bool {
                match expr {
                    Expression::Paren { expr, .. } => weak_field(sema, ctx, expr),
                    Expression::MemberAccess { target, name, .. } => {
                        let Some(receiver) =
                            sema.expression_types.get(&(ctx.module, target.span()))
                        else {
                            return false;
                        };
                        let base = match receiver {
                            Ty::Pointer(inner) => &**inner,
                            other => other,
                        };
                        if !matches!(base, Ty::Struct(..) | Ty::Class(..) | Ty::Applied(..)) {
                            return false;
                        }
                        let key = crate::typecheck::this_ty_key(base);
                        crate::typecheck::member::lookup_member_chain(sema, &key, name, false)
                            .is_some_and(|hit| {
                                matches!(hit.info,
                                crate::typecheck::member::MemberInfo::Field(field) if field.is_weak)
                            })
                    }
                    _ => false,
                }
            }
            if weak_field(sema, ctx, operand) {
                sema.type_error(ctx.module, operand.span(), "weak 字段不能取裸地址");
                return Ty::Error;
            }
            // 取址是安全操作（design.md §6），但需要左值；
            // 操作数类型未知时错误已报过，跳过左值检查避免级联
            if !operand_ty.is_opaque() && !is_lvalue(sema, ctx, operand) {
                sema.type_error(ctx.module, operand.span(), "`&` 取址需要左值");
            }
            Ty::Pointer(Box::new(operand_ty))
        }
        UnaryOp::DerefPointer => {
            // S2：解引用必须 unsafe
            require_unsafe(sema, ctx, span, "指针解引用");
            match operand_ty {
                Ty::Pointer(inner) if matches!(*inner, Ty::Void) => {
                    sema.type_error(ctx.module, span, "不能解引用 void*，请先转换为具体指针类型");
                    Ty::Error
                }
                Ty::Pointer(inner) => *inner,
                Ty::Null => {
                    sema.type_error(ctx.module, span, "不能解引用 `null` 字面量");
                    Ty::Error
                }
                ty if ty.is_opaque() => ty,
                ty => {
                    sema.type_error(ctx.module, span, format!("只能解引用指针，实为 `{ty}`"));
                    Ty::Error
                }
            }
        }
    }
}

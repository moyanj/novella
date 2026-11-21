//! 表达式分派与类型登记。
use super::*;

/// 表达式检查，返回表达式类型（出错时返回 Error 并记录诊断，保证可恢复）。
///
/// 顺带把「表达式 span → 类型」登记进 [`Sema::expression_types`]，供 codegen
/// 使用。登记点必须放在**唯一的函数出口**（而不是各分支里），否则新增分支
/// 时极易漏记；`check_expr_with_hint` 等其它入口最终也会回到这里。
pub(crate) fn check_expr(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> Ty {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        check_expr_recorded(sema, ctx, expr)
    })
}

pub(super) fn check_expr_recorded(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> Ty {
    let ty = check_expr_inner(sema, ctx, expr);
    if matches!(
        expr,
        Expression::Call { .. } | Expression::New { .. } | Expression::Await { .. }
    ) {
        ctx.record_possible_exception();
    }
    sema.expression_types
        .insert((ctx.module, expr.span()), ty.clone());
    ty
}

/// `check_expr` 的实体：只做分派与推导，不登记类型表。
/// 递归调用仍走 `check_expr`，因此子表达式同样会被登记。
pub(super) fn check_expr_inner(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> Ty {
    match expr {
        Expression::Literal { value, span } => check_literal(sema, ctx, value, *span),
        Expression::Identifier { name, span } => check_identifier(sema, ctx, name, *span),
        Expression::Paren { expr, .. } => check_expr(sema, ctx, expr),
        Expression::Tuple { elements, .. } => {
            let tys = elements.iter().map(|e| check_expr(sema, ctx, e)).collect();
            Ty::Tuple(tys)
        }
        Expression::ArrayLiteral { elements, span } => {
            check_array_literal(sema, ctx, elements, *span)
        }
        Expression::ObjectLiteral { properties, .. } => check_object_literal(sema, ctx, properties),
        Expression::This { span } => match &ctx.this_ty {
            Some(ty) => ty.clone(),
            None => {
                sema.type_error(ctx.module, *span, "`this` 只能在方法内使用");
                Ty::Error
            }
        },
        Expression::Unary { op, operand, span } => check_unary(sema, ctx, *op, operand, *span),
        Expression::Await { expr, span } => {
            if !ctx.in_async {
                sema.type_error(ctx.module, *span, "`await` 只能在 async 函数内使用");
            }
            match check_expr(sema, ctx, expr) {
                Ty::Future(inner) => *inner,
                ty if ty.is_opaque() => ty,
                ty => {
                    sema.type_error(
                        ctx.module,
                        *span,
                        format!("await 需要 Future<T>，实为 `{ty}`"),
                    );
                    Ty::Error
                }
            }
        }
        Expression::Binary {
            op,
            left,
            right,
            span,
        } => check_binary(sema, ctx, *op, left, right, *span),
        Expression::Is { operand, ty, .. } => {
            check_expr(sema, ctx, operand);
            sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
            Ty::Boolean
        }
        Expression::Cast { expr, ty, span } => check_cast(sema, ctx, expr, ty, *span),
        Expression::StructInit {
            name,
            type_args,
            properties,
            span,
        } => check_struct_init(sema, ctx, name, type_args, properties, *span),
        Expression::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            expect_boolean(sema, ctx, condition, "三元表达式的条件");
            let before = ctx.uninitialized.clone();
            let reachable = ctx.reachable;
            let condition = crate::typecheck::flow::constant_bool(condition);
            ctx.reachable &= condition != Some(false);
            let then_ty = check_expr(sema, ctx, then_expr);
            let after_then = ctx.uninitialized.clone();
            ctx.uninitialized = before;
            ctx.reachable = reachable && condition != Some(true);
            let else_ty = check_expr(sema, ctx, else_expr);
            ctx.reachable = reachable;
            match condition {
                Some(true) => ctx.uninitialized = after_then,
                Some(false) => {}
                None => ctx.uninitialized.extend(after_then),
            }
            unify_types(&then_ty, &else_ty).unwrap_or_else(|| {
                sema.type_error(
                    ctx.module,
                    *span,
                    format!("三元表达式分支类型 `{then_ty}` 与 `{else_ty}` 不一致"),
                );
                Ty::Error
            })
        }
        Expression::Assignment {
            target,
            op,
            value,
            span,
        } => check_assignment(sema, ctx, target, *op, value, *span),
        Expression::MemberAccess { target, name, span } => {
            check_member_access(sema, ctx, target, name, *span)
        }
        Expression::IndexAccess {
            target,
            index,
            span,
        } => check_index_access(sema, ctx, target, index, *span),
        Expression::Call {
            callee,
            type_args,
            args,
            span,
        } => check_call(sema, ctx, callee, type_args, args, *span),
        Expression::New {
            target,
            type_args,
            args,
            span,
        } => check_new(sema, ctx, target, type_args, args, *span),
        Expression::Match(m) => crate::typecheck::matching::check_match(sema, ctx, m, None),
        Expression::Lambda(lambda) => check_lambda(sema, ctx, lambda),
    }
}

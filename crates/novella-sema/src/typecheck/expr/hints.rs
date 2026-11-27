//! 期望类型的结构递归下推。
use super::*;

/// 带期望类型的表达式检查（design.md §5.2.1「字面量归化」、plan §7 S03）。
///
/// 期望类型沿**结构**下推，而不是只在顶层做几种特例，因此括号 / 元组 /
/// 数组 / 三元 / `??` / `as` / lambda 里的空数组与 `null` 也能被上下文定型：
///
/// | 表达式 | 下推规则 |
/// | --- | --- |
/// | `Paren` | 期望类型传给内层（本层登记同样类型） |
/// | `ArrayLiteral` | `T[]` → 元素一律按 `T` 检查（空数组直接得 `T[]`） |
/// | `Tuple` | `(T1, T2)` → 逐位下推 |
/// | `Ternary` | 两个分支都按期望类型检查，分支合并失败时用期望类型收敛 |
/// | `Binary ??` | 备用值（右操作数）按左操作数类型或期望类型检查 |
/// | `Binary &&/\|\|` | 两侧都是 `boolean`，无需下推 |
/// | `Cast` | 转换结果自身决定类型，不下推 |
/// | 空数组 / `null` | 无上下文时由 [`check_array_literal`] 报错；`null` 适配可空目标 |
///
/// 其余表达式（调用、成员访问、标识符等）类型由自身决定，直接 [`check_expr`]。
/// 本函数是登记类型表的**第二个**出口，所有分支都会落到
/// [`record_expression_type`]，避免「新增一个提示分支却忘了登记」。
pub(crate) fn check_expr_with_hint(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    expr: &Expression,
    hint: &Ty,
) -> Ty {
    let ty = stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        check_expr_with_hint_inner(sema, ctx, expr, hint)
    });
    record_expression_type(sema, ctx.module, expr.span(), ty.clone());
    ty
}

/// 把「表达式 span → 类型」写进类型表；与 [`check_expr`] 共用同一键空间。
pub(crate) fn record_expression_type(sema: &mut Sema, module: ModuleId, span: Span, ty: Ty) {
    sema.expression_types.insert((module, span), ty);
}

/// 期望类型下推的实体：只做结构分派，登记由 [`check_expr_with_hint`] 统一完成。
pub(super) fn check_expr_with_hint_inner(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    expr: &Expression,
    hint: &Ty,
) -> Ty {
    match (expr, hint) {
        (Expression::ObjectLiteral { properties, .. }, Ty::Object(fields)) => {
            super::aggregates::check_object_literal_with_hint(sema, ctx, properties, fields)
        }
        (Expression::Match(node), hint) => {
            crate::typecheck::matching::check_match(sema, ctx, node, Some(hint))
        }
        // 括号：期望类型穿透（括号不改变表达式类型，只影响优先级）
        (Expression::Paren { expr, .. }, hint) => check_expr_with_hint(sema, ctx, expr, hint),
        (
            Expression::ArrayLiteral { elements, span },
            Ty::Array(element, _) | Ty::Pointer(element),
        ) => check_array_literal_with_hint(sema, ctx, elements, *span, element),
        // 元组：逐位下推。位数不同属结构错误，报错后按元素各自定型（不再为
        // 对接不上的位置伪造期望类型——「不用 Unknown 伪装具体类型」的反面同理）
        (Expression::Tuple { elements, span }, Ty::Tuple(element_tys)) => {
            if element_tys.len() != elements.len() {
                sema.type_error(
                    ctx.module,
                    *span,
                    format!(
                        "元组字面量有 {} 个元素，期望类型是 {} 位的 `{hint}`",
                        elements.len(),
                        element_tys.len()
                    ),
                );
                let tys: Vec<Ty> = elements.iter().map(|e| check_expr(sema, ctx, e)).collect();
                Ty::Tuple(tys)
            } else {
                let tys: Vec<Ty> = elements
                    .iter()
                    .zip(element_tys)
                    .map(|(element, element_ty)| {
                        check_expr_with_hint(sema, ctx, element, element_ty)
                    })
                    .collect();
                unify_tuple(sema, ctx, *span, &tys, hint)
            }
        }
        // 三元：两个分支共享期望类型；分支无法合并时收敛到期望类型（标注最权威）
        (
            Expression::Ternary {
                condition,
                then_expr,
                else_expr,
                span,
            },
            hint,
        ) => {
            expect_boolean(sema, ctx, condition, "三元表达式的条件");
            let before = ctx.uninitialized.clone();
            let reachable = ctx.reachable;
            let condition = crate::typecheck::flow::constant_bool(condition);
            ctx.reachable &= condition != Some(false);
            let then_ty = check_expr_with_hint(sema, ctx, then_expr, hint);
            let after_then = ctx.uninitialized.clone();
            ctx.uninitialized = before;
            ctx.reachable = reachable && condition != Some(true);
            let else_ty = check_expr_with_hint(sema, ctx, else_expr, hint);
            ctx.reachable = reachable;
            match condition {
                Some(true) => ctx.uninitialized = after_then,
                Some(false) => {}
                None => ctx.uninitialized.extend(after_then),
            }
            unify_types(&then_ty, &else_ty).unwrap_or_else(|| {
                if sema.is_assignable(hint, &then_ty) && sema.is_assignable(hint, &else_ty) {
                    // 两侧都能赋给期望类型：保留期望类型，不报错（如 u8 字面量与变量混用）
                    hint.clone()
                } else {
                    sema.type_error(
                        ctx.module,
                        *span,
                        format!("三元表达式分支类型 `{then_ty}` 与 `{else_ty}` 不一致"),
                    );
                    Ty::Error
                }
            })
        }
        // `??`：备用值按「左操作数类型，否则期望类型」检查（左操作数为纯 null 时用期望）
        (
            Expression::Binary {
                op: BinaryOp::NullCoalesce,
                left,
                right,
                span,
            },
            hint,
        ) => {
            let fallback_hint = if hint.is_opaque() { None } else { Some(hint) };
            check_binary_with_hint(
                sema,
                ctx,
                BinaryOp::NullCoalesce,
                left,
                right,
                *span,
                fallback_hint,
            )
        }
        // `&&` / `||`：两侧都为 boolean，下推 boolean 可避免空数组被误判
        (
            Expression::Binary {
                op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr),
                left,
                right,
                span,
            },
            hint,
        ) => check_binary_with_hint(sema, ctx, *op, left, right, *span, Some(hint)),
        // 空数组 / `null` 的兜底：期望类型适配可空目标
        (
            Expression::Literal {
                value: Literal::Null,
                ..
            },
            hint,
        ) if hint.is_nullable() => hint.clone(),
        // 其余表达式类型由自身决定
        _ => check_expr(sema, ctx, expr),
    }
}

/// 元组下推后的收尾：逐位类型必须与期望逐位兼容，否则报错；结构不符走通用路径。
pub(super) fn unify_tuple(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    span: Span,
    tys: &[Ty],
    hint: &Ty,
) -> Ty {
    let actual = Ty::Tuple(tys.to_vec());
    if !sema.is_assignable(hint, &actual) {
        sema.type_error(
            ctx.module,
            span,
            format!("元组类型 `{actual}` 与期望 `{hint}` 不兼容"),
        );
        return hint.clone();
    }
    concrete(&actual)
}

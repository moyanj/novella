//! 数组和匿名结构对象。
use super::*;

pub(crate) fn check_array_literal(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    elements: &[Expression],
    span: Span,
) -> Ty {
    if elements.is_empty() {
        sema.type_error(
            ctx.module,
            span,
            "空数组字面量无法推断元素类型，请显式写出元素或改用类型标注",
        );
        return Ty::Error;
    }
    let mut element_ty = check_expr(sema, ctx, &elements[0]);
    let mut actuals = vec![(elements[0].span(), element_ty.clone())];
    for element in &elements[1..] {
        let ty = check_expr(sema, ctx, element);
        actuals.push((element.span(), ty.clone()));
        element_ty = match unify_types(&element_ty, &ty) {
            Some(unified) => unified,
            None => {
                sema.type_error(
                    ctx.module,
                    element.span(),
                    format!("数组元素类型 `{ty}` 与首个元素 `{element_ty}` 不一致"),
                );
                element_ty
            }
        };
    }
    let inferred = concrete(&element_ty);
    for (span, ty) in actuals {
        if !sema.is_assignable(&inferred, &ty) {
            sema.type_error(
                ctx.module,
                span,
                format!("数组元素 `{ty}` 超出推导元素类型 `{inferred}` 的范围"),
            );
        }
    }
    Ty::Array(Box::new(inferred))
}

/// 带元素类型提示的数组字面量检查（`check_expr_with_hint` 的 `T[]` 分支）。
///
/// 空数组直接定型为 `T[]`——这是让 `[]` 合法的唯一上下文；非空时每个元素都按
/// `T` 下推，使 `[[], [1]]` 这类嵌套空数组同样能定型。元素与 `T` 不兼容时按
/// 首个不匹配的元素报错，数组类型仍按标注给出，避免上层再报「无法推断元素类型」。
pub(super) fn check_array_literal_with_hint(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    elements: &[Expression],
    _span: Span,
    element_hint: &Ty,
) -> Ty {
    if elements.is_empty() {
        return Ty::Array(Box::new(element_hint.clone()));
    }
    let mut mismatched = None;
    for element in elements {
        let ty = check_expr_with_hint(sema, ctx, element, element_hint);
        if mismatched.is_none() && !sema.is_assignable(element_hint, &ty) {
            mismatched = Some((element.span(), ty));
        }
    }
    if let Some((element_span, ty)) = mismatched {
        sema.type_error(
            ctx.module,
            element_span,
            format!("数组元素类型 `{ty}` 与标注的元素类型 `{element_hint}` 不兼容"),
        );
    }
    Ty::Array(Box::new(element_hint.clone()))
}
/// 对象字面量（追加项 E）：重复键检查 + 简写 `{b}` 展开校验。
pub(crate) fn check_object_literal(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    properties: &[novella_ast::expr::PropertyAssignment],
) -> Ty {
    check_object_literal_with_hint(sema, ctx, properties, &std::collections::BTreeMap::new())
}

/// 已知对象结构时逐字段下推，允许空数组等依赖上下文的字段值定型。
pub(super) fn check_object_literal_with_hint(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    properties: &[novella_ast::expr::PropertyAssignment],
    hints: &std::collections::BTreeMap<String, Ty>,
) -> Ty {
    let mut seen = HashSet::new();
    let mut fields = std::collections::BTreeMap::new();
    for property in properties {
        if !seen.insert(property.name.clone()) {
            sema.type_error(
                ctx.module,
                property.span,
                format!("对象字面量的键 `{}` 重复", property.name),
            );
        }
        match &property.value {
            Some(value) => {
                let ty = match hints.get(&property.name) {
                    Some(hint) => check_expr_with_hint(sema, ctx, value, hint),
                    None => check_expr(sema, ctx, value),
                };
                fields.insert(property.name.clone(), concrete(&ty));
            }
            None => {
                let ty = check_identifier(sema, ctx, &property.name, property.span);
                fields.insert(property.name.clone(), concrete(&ty));
            }
        }
    }
    Ty::Object(fields)
}

//! 局部声明、模式绑定与默认字面量类型。
use super::*;
// ---------- 变量声明与绑定模式 ----------
pub(crate) fn check_variable_decl(sema: &mut Sema, ctx: &mut BodyCtx, decl: &VariableStmt) {
    let is_const = matches!(decl.r#type, VariableType::Const);
    let annotated_ty = decl
        .ty
        .as_ref()
        .map(|ty| sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env));
    // 带类型标注时把标注作为期望类型下推（双向检查）：空数组字面量、未定型
    // 字面量等需要上下文才能定型的表达式（design.md §5.2「数组字面量取首元素
    // 为基准」的空情形）由标注给出元素类型，而不是直接报「无法推断」。
    let value_ty = decl.value.as_ref().map(|value| match &annotated_ty {
        Some(annotated) => check_expr_with_hint(sema, ctx, value, annotated),
        None => check_expr(sema, ctx, value),
    });
    let bound_ty = match (&annotated_ty, &value_ty) {
        (Some(annotated), Some(value_ty)) => {
            if !sema.is_assignable(annotated, value_ty) {
                sema.type_error(
                    ctx.module,
                    decl.span,
                    sema.assign_mismatch_reason(annotated, value_ty)
                        .unwrap_or_else(|| {
                            format!("初始化类型 `{value_ty}` 与标注 `{annotated}` 不兼容")
                        }),
                );
            }
            annotated.clone()
        }
        (Some(annotated), None) => {
            if is_const {
                sema.type_error(ctx.module, decl.span, "`const` 声明必须初始化");
            }
            annotated.clone()
        }
        (None, Some(value_ty)) => {
            if matches!(value_ty, Ty::Null) {
                sema.type_error(
                    ctx.module,
                    decl.span,
                    "无法用 `null` 推断变量类型，请添加类型标注",
                );
                Ty::Unknown
            } else {
                let inferred = concrete(value_ty);
                if !sema.is_assignable(&inferred, value_ty) {
                    sema.type_error(
                        ctx.module,
                        decl.span,
                        format!("初始值 `{value_ty}` 超出默认类型 `{inferred}` 的范围"),
                    );
                }
                inferred
            }
        }
        (None, None) => {
            sema.type_error(
                ctx.module,
                decl.span,
                "变量声明需要类型标注或初始值（无法推断类型）",
            );
            Ty::Unknown
        }
    };
    sema.declaration_types
        .insert((ctx.module, decl.span), bound_ty.clone());
    bind_pattern_typed(sema, ctx, &decl.pattern, &bound_ty, is_const);
    if decl.value.is_none() && !ctx.fill_existing {
        let mut names = Vec::new();
        crate::resolve::collect_pattern_names(&decl.pattern, &mut names);
        ctx.uninitialized
            .extend(names.into_iter().map(|(_, span)| span));
    }
}
/// 按类型绑定模式（元组解构校验结构与 arity）。
pub(crate) fn bind_pattern(sema: &mut Sema, ctx: &mut BodyCtx, pattern: &BindingPattern, ty: &Ty) {
    bind_pattern_impl(sema, ctx, pattern, ty, false);
}
pub(crate) fn bind_pattern_typed(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    pattern: &BindingPattern,
    ty: &Ty,
    is_const: bool,
) {
    bind_pattern_impl(sema, ctx, pattern, ty, is_const);
}
pub(crate) fn bind_pattern_impl(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    pattern: &BindingPattern,
    ty: &Ty,
    is_const: bool,
) {
    match pattern {
        BindingPattern::Ident { name, span } => {
            let kind = SymbolKind::Variable {
                is_const,
                ty: Some(ty.clone()),
            };
            if ctx.fill_existing {
                // 顶层变量：回填声明收集阶段登记的符号
                if let Some(symbol) = sema.scopes.lookup_mut(ctx.scope, name) {
                    symbol.kind = kind;
                    return;
                }
            }
            define_local(sema, ctx, name, kind, *span);
        }
        BindingPattern::Tuple { elements, span } => {
            let element_tys: Vec<Ty> = match ty {
                Ty::Tuple(elements_ty) if elements_ty.len() == elements.len() => {
                    elements_ty.clone()
                }
                Ty::Tuple(_) => {
                    sema.type_error(ctx.module, *span, "元组解构的元素个数不匹配");
                    vec![Ty::Unknown; elements.len()]
                }
                _ if ty.is_opaque() => vec![Ty::Unknown; elements.len()],
                _ => {
                    sema.type_error(ctx.module, *span, format!("不能用 `{ty}` 做元组解构"));
                    vec![Ty::Unknown; elements.len()]
                }
            };
            for (element, element_ty) in elements.iter().zip(element_tys) {
                bind_pattern_impl(sema, ctx, element, &element_ty, is_const);
            }
        }
    }
}
/// 在当前作用域定义局部符号（重定义报诊断）。
pub(crate) fn define_local(
    sema: &mut Sema,
    ctx: &BodyCtx,
    name: &str,
    kind: SymbolKind,
    span: Span,
) {
    let symbol = Symbol {
        name: name.to_string(),
        kind,
        visibility: None,
        span,
        name_span: Span {
            start: span.start,
            end: span.start + name.len(),
        },
        module: ctx.module,
    };
    if sema.scopes.define(ctx.scope, symbol).is_err() {
        sema.type_error(ctx.module, span, format!("重复定义 `{name}`"));
    }
}
/// 字面量归一化：未定型字面量落到默认类型（`int` ≡ `i32`，`float` ≡ `f32`）。
pub(crate) fn concrete(ty: &Ty) -> Ty {
    match ty {
        Ty::IntLiteral(negative, value) => {
            // 定型时取回符号：`-128` 的具体值才能落到 `i8` 上
            let _ = (negative, value);
            Ty::Int(crate::ty::IntKind::I32)
        }
        Ty::FloatLiteral(_) => Ty::Float(crate::ty::FloatKind::F32),
        Ty::Tuple(elements) => Ty::Tuple(elements.iter().map(concrete).collect()),
        Ty::Array(element) => Ty::Array(Box::new(concrete(element))),
        other => other.clone(),
    }
}

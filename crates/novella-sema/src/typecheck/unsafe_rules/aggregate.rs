//! struct 字段初始化及泛型代入。
use super::*;

/// struct 初始化 `Point{ x: 1 }`：字段存在性/重复/缺失检查。
pub(crate) fn check_struct_init(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    name: &str,
    type_args: &[TypeExpr],
    properties: &[novella_ast::expr::PropertyAssignment],
    span: Span,
) -> Ty {
    let struct_key = match type_key_of_name(sema, ctx.scope, name) {
        Some(key) => {
            if let Some(symbol) = sema.scopes.lookup(ctx.scope, name).cloned() {
                sema.record_reference(
                    ctx.module,
                    Span {
                        start: span.start,
                        end: span.start + name.len(),
                    },
                    &symbol,
                );
            }
            key
        }
        None if sema.scopes.lookup(ctx.scope, name).is_some() => {
            sema.type_error(
                ctx.module,
                span,
                format!("`{name}` 不是类型，不能用于初始化"),
            );
            return Ty::Unknown;
        }
        None => {
            sema.type_error(ctx.module, span, format!("未定义的类型 `{name}`"));
            return Ty::Error;
        }
    };
    let resolved = sema.resolve_type(
        ctx.module,
        ctx.scope,
        &novella_ast::types::TypeExpr::Reference {
            qualifier: vec![],
            name: name.to_owned(),
            generic_args: type_args.to_vec(),
            span,
        },
        &ctx.type_env,
    );
    let base = match &resolved {
        Ty::Applied(base, _) => &**base,
        ty => ty,
    };
    let struct_key = match base {
        Ty::Struct(module, name) => (*module, name.clone()),
        _ => struct_key,
    };
    let subst = crate::ty::instance_substitution(sema, &resolved);
    match sema.type_defs.get(&struct_key) {
        Some(TypeDef::Struct(def)) => {
            let def = def.clone();
            let mut seen = HashSet::new();
            for property in properties {
                if !seen.insert(property.name.clone()) {
                    sema.type_error(
                        ctx.module,
                        property.span,
                        format!("字段 `{}` 重复初始化", property.name),
                    );
                    continue;
                }
                let Some(field_def) = def.fields.iter().find(|f| f.name == property.name) else {
                    sema.type_error(
                        ctx.module,
                        property.span,
                        format!("struct `{name}` 没有字段 `{}`", property.name),
                    );
                    continue;
                };
                let mut field_def = field_def.clone();
                field_def.ty = crate::ty::substitute_type_params(
                    &crate::check::fields::field_type(sema, &struct_key, &field_def.name),
                    &subst,
                );
                // 字段类型已知：按它下推（`P { xs: [] }` 里的空数组由此定型）
                let value_ty = match &property.value {
                    Some(value) => check_expr_with_hint(sema, ctx, value, &field_def.ty),
                    None => {
                        // 简写 {x} ≡ {x: x}
                        check_expr(
                            sema,
                            ctx,
                            &Expression::Identifier {
                                name: property.name.clone(),
                                span: property.span,
                            },
                        )
                    }
                };
                if !sema.is_assignable(&field_def.ty, &value_ty) {
                    sema.type_error(
                        ctx.module,
                        property.span,
                        format!(
                            "字段 `{}` 需要 `{}`，实为 `{value_ty}`",
                            property.name, field_def.ty
                        ),
                    );
                }
            }
            // 缺失字段（无默认值的必须提供）
            for field_def in &def.fields {
                if !field_def.has_default && !seen.contains(&field_def.name) {
                    sema.type_error(
                        ctx.module,
                        span,
                        format!("struct `{name}` 的字段 `{}` 未初始化", field_def.name),
                    );
                }
            }
            resolved
        }
        Some(TypeDef::Class(_)) => {
            sema.type_error(
                ctx.module,
                span,
                format!("class `{name}` 不能用 `{{...}}` 初始化，请用 `new {name}(...)`"),
            );
            Ty::Unknown
        }
        Some(_) => {
            sema.type_error(ctx.module, span, format!("`{name}` 不是 struct/class 类型"));
            Ty::Error
        }
        None => Ty::Unknown, // 语法失败的模块，注册阶段已报过
    }
}

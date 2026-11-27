//! 先收集关联类型名称与完整定义，再定型普通成员签名。
use super::*;
use crate::traits::{nominal_base, self_key};
use crate::ty::ty_eq;

pub(crate) fn register_trait_items(sema: &mut Sema, module: ModuleId) {
    if sema.modules[module].syntax_failed {
        return;
    }
    for item in sema.modules[module].program.items.clone() {
        let Item::Trait(item) = item else {
            continue;
        };
        let mut names = HashMap::new();
        for member in item.members {
            let ClassMember::AssociatedType(assoc) = member else {
                continue;
            };
            if assoc.qualifier.is_some() || assoc.ty.is_some() {
                sema.decl_error(module, assoc.span, "trait 中的关联类型须写为 `type Name;`");
            } else if names.insert(assoc.name.clone(), assoc.name_span).is_some() {
                sema.decl_error(
                    module,
                    assoc.name_span,
                    format!("重复声明关联类型 `{}`", assoc.name),
                );
            }
        }
        if let Some(TypeDef::Trait(def)) = sema.type_defs.get_mut(&(module, item.name)) {
            def.associated = names;
        }
    }
}

pub(crate) fn register_owner_bounds(sema: &mut Sema, module: ModuleId) {
    if sema.modules[module].syntax_failed {
        return;
    }
    let scope = sema.modules[module].global_scope;
    for item in &sema.modules[module].program.items.clone() {
        let params = match item {
            Item::Trait(item) => {
                let ty = sema.identity_instance(Ty::Trait(module, item.name.clone()));
                sema.type_param_bounds
                    .insert(self_key(module, &item.name), vec![ty]);
                &item.type_parameters
            }
            Item::Class(item) => &item.type_parameters,
            Item::Struct(item) => &item.type_parameters,
            Item::TypeAlias(item) => &item.type_parameters,
            _ => continue,
        };
        let env = registration::identity_env(module, params);
        sema.register_bounds(module, scope, params, &env);
    }
}

pub(crate) fn register_associated_definitions(sema: &mut Sema) {
    let mut definitions = Vec::new();
    // 解析各 RHS 时暂不归约；跨模块、同一类型内的前向引用均在全部登记后处理。
    for module in 0..sema.modules.len() {
        if sema.modules[module].syntax_failed {
            continue;
        }
        let scope = sema.modules[module].global_scope;
        for item in sema.modules[module].program.items.clone() {
            let (name, base, members, params) = match item {
                Item::Class(item) => (
                    item.name.clone(),
                    Ty::Class(module, item.name),
                    item.members,
                    item.type_parameters,
                ),
                Item::Struct(item) => (
                    item.name.clone(),
                    Ty::Struct(module, item.name),
                    item.members,
                    item.type_parameters,
                ),
                _ => continue,
            };
            let receiver = sema.identity_instance(base);
            let key = (module, name);
            let traits = match sema.type_defs.get(&key) {
                Some(TypeDef::Class(d)) => d.trait_types.clone(),
                Some(TypeDef::Struct(d)) => d.trait_types.clone(),
                _ => continue,
            };
            let mut env = registration::identity_env(module, &params);
            env.insert("Self".into(), receiver.clone());
            let mut values: Vec<(Ty, String, Ty)> = Vec::new();
            for member in members {
                let ClassMember::AssociatedType(assoc) = member else {
                    continue;
                };
                let (Some(qualifier), Some(expr)) = (&assoc.qualifier, &assoc.ty) else {
                    sema.decl_error(
                        module,
                        assoc.span,
                        "关联类型实现须写为 `type Trait.Name = Type;`",
                    );
                    continue;
                };
                let applied = sema.resolve_type(module, scope, qualifier, &env);
                if !matches!(nominal_base(&applied), Ty::Trait(..))
                    || !traits.iter().any(|t| ty_eq(t, &applied))
                {
                    sema.decl_error(
                        module,
                        qualifier.span(),
                        format!("关联类型需要显式实现 trait `{applied}`"),
                    );
                    continue;
                }
                if sema.associated_declaration(&applied, &assoc.name).is_none() {
                    sema.decl_error(
                        module,
                        assoc.name_span,
                        format!("trait `{applied}` 未声明关联类型 `{}`", assoc.name),
                    );
                    continue;
                }
                if values
                    .iter()
                    .any(|(t, n, _)| n == &assoc.name && ty_eq(t, &applied))
                {
                    sema.decl_error(
                        module,
                        assoc.name_span,
                        format!("重复实现关联类型 `{applied}.{}`", assoc.name),
                    );
                    continue;
                }
                let ty = sema.resolve_type(module, scope, expr, &env);
                definitions.push((
                    key.clone(),
                    receiver.clone(),
                    applied.clone(),
                    assoc.name.clone(),
                    expr.span(),
                ));
                values.push((applied, assoc.name, ty));
            }
            match sema.type_defs.get_mut(&key) {
                Some(TypeDef::Class(d)) => d.associated = values,
                Some(TypeDef::Struct(d)) => d.associated = values,
                _ => {}
            }
        }
    }
    for (key, receiver, trait_ref, name, span) in definitions {
        let projection = Ty::Associated {
            receiver: Box::new(receiver),
            trait_ref: Box::new(trait_ref),
            name,
        };
        let ty = sema.normalize_associated(&projection);
        let mut invalid = false;
        crate::ty::rewrite_type(&ty, &mut |t| {
            invalid |= matches!(t, Ty::Associated { receiver, .. } if matches!(nominal_base(receiver), Ty::Class(..) | Ty::Struct(..)));
            None
        });
        if invalid
            || matches!(ty, Ty::Error)
                && !sema
                    .annotation_types
                    .get(&(key.0, span))
                    .is_some_and(Ty::is_opaque)
        {
            sema.type_error(
                key.0,
                span,
                format!("关联类型 `{projection}` 循环或依赖未定义的关联类型"),
            );
        }
        sema.annotation_types.insert((key.0, span), ty);
    }
}

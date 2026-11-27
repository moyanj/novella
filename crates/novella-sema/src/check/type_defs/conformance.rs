//! 显式 trait 实现的一致性与下游见证表。
use super::*;
use crate::ty::{instance_substitution, ty_eq};
use crate::{TraitImplementation, TraitMethodImplementation};

impl Sema {
    /// 从接收者到真实声明的应用属主；最近的声明（包括抽象）遮蔽父类声明。
    pub(crate) fn nominal_method(&self, receiver: &Ty, name: &str) -> Option<(Ty, MethodSig)> {
        self.nominal_lineage(receiver)
            .into_iter()
            .find_map(|owner| {
                let methods = match self.type_defs.get(&crate::typecheck::this_ty_key(&owner)) {
                    Some(TypeDef::Struct(d)) => &d.methods,
                    Some(TypeDef::Class(d)) => &d.methods,
                    _ => return None,
                };
                methods.get(name).cloned().map(|method| (owner, method))
            })
    }

    pub(crate) fn trait_implementations(&self) -> Vec<TraitImplementation> {
        effective_pairs(self)
            .into_iter()
            .map(|(_, _, receiver, trait_ref)| {
                let (_, mut names, methods) = requirements(self, &trait_ref);
                let mut bindings = names
                    .drain(..)
                    .filter_map(|name| {
                        let projection = Ty::Associated {
                            receiver: Box::new(receiver.clone()),
                            trait_ref: Box::new(trait_ref.clone()),
                            name: name.clone(),
                        };
                        self.associated_value(&receiver, &trait_ref, &name)?;
                        Some((name, self.normalize_associated(&projection).view()))
                    })
                    .collect::<Vec<_>>();
                bindings.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                let methods = methods
                    .into_iter()
                    .filter_map(|required| {
                        let (owner, method, is_default) = match self
                            .nominal_method(&receiver, &required.name)
                        {
                            Some((owner, method))
                                if !method.is_abstract
                                    && method.is_static == required.is_static =>
                            {
                                (owner, method, false)
                            }
                            None if !required.is_abstract => (trait_ref.clone(), required, true),
                            _ => return None,
                        };
                        Some(TraitMethodImplementation {
                            name: method.name,
                            module: method.sig.module,
                            symbol: method.sig.name.expect("成员方法包含声明符号"),
                            owner: owner.view(),
                            is_default,
                        })
                    })
                    .collect();
                TraitImplementation {
                    receiver: receiver.view(),
                    trait_ref: trait_ref.view(),
                    bindings,
                    methods,
                }
            })
            .collect()
    }
}

fn nominal_definitions(sema: &Sema) -> Vec<(ModuleId, Span, Ty)> {
    let mut definitions = sema.type_defs.iter().collect::<Vec<_>>();
    definitions.sort_unstable_by_key(|(key, _)| *key);
    definitions
        .into_iter()
        .filter_map(|((module, name), def)| {
            let (base, span) = match def {
                TypeDef::Class(d) => (Ty::Class(*module, name.clone()), d.span),
                TypeDef::Struct(d) => (Ty::Struct(*module, name.clone()), d.span),
                _ => return None,
            };
            Some((*module, span, sema.identity_instance(base)))
        })
        .collect()
}

/// 为每个 class 的静态接收者单独发布有效见证，保证继承实现仍选择其最近覆写。
fn effective_pairs(sema: &Sema) -> Vec<(ModuleId, Span, Ty, Ty)> {
    nominal_definitions(sema)
        .into_iter()
        .flat_map(|(module, span, receiver)| {
            sema.implemented_traits(&receiver)
                .into_iter()
                .map(move |trait_ref| (module, span, receiver.clone(), trait_ref))
        })
        .collect()
}

fn check_overlaps(sema: &mut Sema) {
    for (module, span, receiver) in nominal_definitions(sema) {
        let mut earlier = sema
            .nominal_lineage(&receiver)
            .iter()
            .skip(1)
            .flat_map(|parent| sema.direct_traits(parent))
            .collect::<Vec<_>>();
        for trait_ref in sema.direct_traits(&receiver) {
            if let Some(prior) = earlier
                .iter()
                .find(|other| super::overlap::overlap(other, &trait_ref))
            {
                sema.decl_error(
                    module,
                    span,
                    format!("`{receiver}` 的 trait 实现 `{trait_ref}` 与 `{prior}` 重叠"),
                );
            }
            earlier.push(trait_ref);
        }
    }
}

fn requirements(sema: &Sema, trait_ref: &Ty) -> (String, Vec<String>, Vec<MethodSig>) {
    let key = crate::typecheck::this_ty_key(trait_ref);
    let Some(TypeDef::Trait(def)) = sema.type_defs.get(&key) else {
        return (key.1, Vec::new(), Vec::new());
    };
    let mut names = def.associated.keys().cloned().collect::<Vec<_>>();
    names.sort_unstable();
    let mut methods = def.methods.values().cloned().collect::<Vec<_>>();
    methods.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (def.name.clone(), names, methods)
}

pub(super) fn check_trait_impls(sema: &mut Sema) {
    check_overlaps(sema);
    for (module, span, receiver, trait_ref) in effective_pairs(sema) {
        let (trait_name, names, methods) = requirements(sema, &trait_ref);
        for name in names {
            if sema
                .associated_value(&receiver, &trait_ref, &name)
                .is_none()
            {
                sema.decl_error(
                    module,
                    span,
                    format!(
                        "`{receiver}` 声明实现 trait `{trait_ref}`，但缺少关联类型 `{name}` 的实现"
                    ),
                );
            }
        }
        for required in methods {
            let name = &required.name;
            let Some((owner, actual)) = sema.nominal_method(&receiver, name) else {
                if required.is_abstract {
                    sema.decl_error(
                        module,
                        span,
                        format!(
                            "`{receiver}` 声明实现 trait `{trait_ref}`，但缺少方法 `{name}` 的实现"
                        ),
                    );
                }
                continue;
            };
            let required_ty = sema.instantiate_trait_member(
                &Ty::Function(std::rc::Rc::new(required.sig.clone())),
                &trait_ref,
                &receiver,
            );
            let actual_ty = sema.instantiate_type(
                &Ty::Function(std::rc::Rc::new(actual.sig.clone())),
                &instance_substitution(sema, &owner),
            );
            let (Ty::Function(required_sig), Ty::Function(actual_sig)) = (required_ty, actual_ty)
            else {
                unreachable!()
            };
            if actual.is_abstract
                || actual.is_static != required.is_static
                || !inheritance::trait_signature_compatible(sema, &required_sig, &actual_sig)
                || !compatible_bounds(
                    sema,
                    &trait_ref,
                    &receiver,
                    &owner,
                    &required_sig,
                    &actual_sig,
                )
            {
                let location = if actual.sig.module == module && ty_eq(&owner, &receiver) {
                    actual.span
                } else {
                    span
                };
                sema.decl_error(
                    module,
                    location,
                    format!("方法 `{name}` 的实现与 trait `{trait_name}` 声明的签名不一致"),
                );
            }
        }
    }
}

fn compatible_bounds(
    sema: &Sema,
    trait_ref: &Ty,
    receiver: &Ty,
    owner: &Ty,
    required: &FunctionSig,
    actual: &FunctionSig,
) -> bool {
    let mut subst = instance_substitution(sema, owner);
    subst.extend(
        actual
            .type_params
            .iter()
            .cloned()
            .zip(required.type_params.iter().cloned().map(Ty::TypeParam)),
    );
    required
        .type_params
        .iter()
        .zip(&actual.type_params)
        .all(|(want, have)| {
            let expected = sema
                .type_param_bounds
                .get(want)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let given = sema
                .type_param_bounds
                .get(have)
                .map(Vec::as_slice)
                .unwrap_or_default();
            // 实现不能要求接口声明之外的约束。
            given.iter().all(|bound| {
                let bound = sema.instantiate_type(bound, &subst);
                expected.iter().any(|expected| {
                    ty_eq(
                        &bound,
                        &sema.instantiate_trait_member(expected, trait_ref, receiver),
                    )
                })
            })
        })
}

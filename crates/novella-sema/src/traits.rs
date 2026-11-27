//! 显式 trait 实现、关联类型投影与拥有型动态对象的共同查询。

use std::collections::{HashMap, HashSet};

use novella_core::Span;

use crate::ty::{Ty, TypeDef, instance_substitution, rewrite_type, substitute_type_params, ty_eq};
use crate::{DynConversion, ModuleId, Sema};

pub(crate) fn nominal_base(ty: &Ty) -> &Ty {
    match ty {
        Ty::Applied(base, _) => base,
        ty => ty,
    }
}

pub(crate) fn self_key(module: ModuleId, name: &str) -> String {
    format!("Self@trait:m{module}:{name}")
}

impl Sema {
    /// 实例化类型声明本身，保留定义处的形式参数身份。
    pub(crate) fn identity_instance(&self, base: Ty) -> Ty {
        let key = crate::typecheck::this_ty_key(&base);
        let args = self
            .type_defs
            .get(&key)
            .map_or_else(Vec::new, |definition| {
                definition
                    .type_params()
                    .iter()
                    .cloned()
                    .map(Ty::TypeParam)
                    .collect()
            });
        if args.is_empty() {
            base
        } else {
            Ty::Applied(Box::new(base), args)
        }
    }

    /// class 父链上每个节点的实际应用类型；struct 仅有自身。
    pub(crate) fn nominal_lineage(&self, start: &Ty) -> Vec<Ty> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut current = start.clone();
        while let Ty::Struct(module, name) | Ty::Class(module, name) = nominal_base(&current) {
            let key = (*module, name.clone());
            if !visited.insert(key.clone()) {
                break;
            }
            result.push(current.clone());
            let Some(TypeDef::Class(def)) = self.type_defs.get(&key) else {
                break;
            };
            let Some(parent) = &def.parent_type else {
                break;
            };
            current = substitute_type_params(parent, &instance_substitution(self, &current));
        }
        result
    }

    pub(crate) fn implemented_traits(&self, receiver: &Ty) -> Vec<Ty> {
        match nominal_base(receiver) {
            Ty::TypeParam(param) => {
                return self
                    .type_param_bounds
                    .get(param)
                    .cloned()
                    .unwrap_or_default();
            }
            Ty::Trait(..) => return vec![receiver.clone()],
            Ty::Dynamic { trait_ref, .. } => return vec![(**trait_ref).clone()],
            _ => {}
        }
        let mut result = Vec::new();
        for owner in self.nominal_lineage(receiver) {
            for ty in self.direct_traits(&owner) {
                if !result.iter().any(|prior| ty_eq(prior, &ty)) {
                    result.push(ty);
                }
            }
        }
        result
    }

    /// 单个名义宿主自己列出的应用 trait，不包含父类声明。
    pub(crate) fn direct_traits(&self, owner: &Ty) -> Vec<Ty> {
        let key = crate::typecheck::this_ty_key(owner);
        let traits = match self.type_defs.get(&key) {
            Some(TypeDef::Class(d)) => &d.trait_types,
            Some(TypeDef::Struct(d)) => &d.trait_types,
            _ => return Vec::new(),
        };
        let subst = instance_substitution(self, owner);
        traits
            .iter()
            .map(|t| self.instantiate_type(t, &subst))
            .collect()
    }

    pub(crate) fn associated_declaration(
        &self,
        trait_ref: &Ty,
        name: &str,
    ) -> Option<(ModuleId, Span)> {
        let Ty::Trait(module, trait_name) = nominal_base(trait_ref) else {
            return None;
        };
        let Some(TypeDef::Trait(def)) = self.type_defs.get(&(*module, trait_name.clone())) else {
            return None;
        };
        def.associated
            .get(name)
            .copied()
            .map(|span| (*module, span))
    }

    pub(crate) fn associated_value(&self, receiver: &Ty, trait_ref: &Ty, name: &str) -> Option<Ty> {
        self.associated_value_active(receiver, trait_ref, name, &mut Vec::new())
    }

    fn associated_value_active(
        &self,
        receiver: &Ty,
        trait_ref: &Ty,
        name: &str,
        active: &mut Vec<(Ty, Ty, String)>,
    ) -> Option<Ty> {
        if let Ty::Dynamic {
            trait_ref: actual,
            bindings,
        } = receiver
        {
            return ty_eq(actual, trait_ref)
                .then(|| {
                    bindings
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, value)| value.clone())
                })
                .flatten();
        }
        for owner in self.nominal_lineage(receiver) {
            let assoc = match self.type_defs.get(&crate::typecheck::this_ty_key(&owner)) {
                Some(TypeDef::Struct(def)) => &def.associated,
                Some(TypeDef::Class(def)) => &def.associated,
                _ => continue,
            };
            let subst = instance_substitution(self, &owner);
            if let Some((_, _, value)) = assoc.iter().find(|(applied, n, _)| {
                n == name
                    && ty_eq(
                        trait_ref,
                        &self.normalize_projections(
                            &substitute_type_params(applied, &subst),
                            active,
                        ),
                    )
            }) {
                return Some(substitute_type_params(value, &subst));
            }
        }
        None
    }

    pub(crate) fn instantiate_type(&self, ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
        let ty = substitute_type_params(ty, subst);
        self.normalize_associated(&ty)
    }

    /// 先求投影的接收者和 trait 应用实参，再查显式实现；未定型参数保留投影。
    pub(crate) fn normalize_associated(&self, ty: &Ty) -> Ty {
        self.normalize_projections(ty, &mut Vec::new())
    }

    fn normalize_projections(&self, ty: &Ty, active: &mut Vec<(Ty, Ty, String)>) -> Ty {
        stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
            rewrite_type(ty, &mut |ty| {
                let Ty::Associated {
                    receiver,
                    trait_ref,
                    name,
                } = ty
                else {
                    return None;
                };
                let receiver = self.normalize_projections(receiver, active);
                let trait_ref = self.normalize_projections(trait_ref, active);
                let projection = || Ty::Associated {
                    receiver: Box::new(receiver.clone()),
                    trait_ref: Box::new(trait_ref.clone()),
                    name: name.clone(),
                };
                // 以名义属主与关联名追踪递归；即使每次递归生成新泛型实参也能终止。
                if active.iter().any(|(r, t, n)| {
                    n == name
                        && ty_eq(nominal_base(r), nominal_base(&receiver))
                        && ty_eq(nominal_base(t), nominal_base(&trait_ref))
                }) {
                    return Some(Ty::Error);
                }
                active.push((receiver.clone(), trait_ref.clone(), name.clone()));
                let result = self
                    .associated_value_active(&receiver, &trait_ref, name, active)
                    .map_or_else(projection, |value| {
                        self.normalize_projections(&value, active)
                    });
                active.pop();
                Some(result)
            })
        })
    }

    /// 在具体调用或 dyn 接口中，用实际接收者替换 trait 方法模板的 Self。
    pub(crate) fn instantiate_trait_member(&self, ty: &Ty, trait_ref: &Ty, receiver: &Ty) -> Ty {
        let (module, name) = crate::typecheck::this_ty_key(trait_ref);
        let mut subst = instance_substitution(self, trait_ref);
        subst.insert(self_key(module, &name), receiver.clone());
        self.instantiate_type(ty, &subst)
    }

    /// 静态和泛型方法不进入 vtable。非泛型实例方法的显式参数与返回值不能依赖裸 Self。
    pub(crate) fn dyn_dispatchable(&self, trait_ref: &Ty, method: &crate::ty::MethodSig) -> bool {
        let sig = &method.sig;
        if method.is_static || sig.type_param_count != 0 || sig.is_async {
            return false;
        }
        let (module, name) = crate::typecheck::this_ty_key(trait_ref);
        let template = self.identity_instance(nominal_base(trait_ref).clone());
        let self_ty = Ty::TypeParam(self_key(module, &name));
        let mentions_self = |ty: &Ty| {
            let mut found = false;
            rewrite_type(ty, &mut |part| {
                // 这个投影在构造 dyn 类型时有必填的绑定，不是裸 Self。
                if matches!(part, Ty::Associated { receiver, trait_ref: owner, name }
                    if ty_eq(receiver, &self_ty) && ty_eq(owner, &template)
                        && self.associated_declaration(owner, name).is_some())
                {
                    return Some(Ty::Unknown);
                }
                found |= ty_eq(part, &self_ty);
                None
            });
            found
        };
        !mentions_self(&sig.ret)
            && !sig.params.iter().enumerate().any(|(index, param)| {
                (index != 0 || param.name != "self") && mentions_self(&param.ty)
            })
    }

    /// 返回类型推导与方法签名注册结束后再判定对象安全，支持前向及跨模块引用。
    pub(crate) fn check_dyn_safety(&mut self) {
        let mut uses = self
            .annotation_types
            .iter()
            .filter_map(|(&(module, span), ty)| {
                let Ty::Dynamic { trait_ref, .. } = ty else {
                    return None;
                };
                Some((module, span, (**trait_ref).clone()))
            })
            .collect::<Vec<_>>();
        uses.sort_unstable_by_key(|(module, span, _)| (*module, span.start, span.end));
        for (module, span, trait_ref) in uses {
            let key = crate::typecheck::this_ty_key(&trait_ref);
            let Some(TypeDef::Trait(def)) = self.type_defs.get(&key) else {
                continue;
            };
            // 对比声明中的 Self 与投影；trait 的应用类型实参不会改变槽集合。
            let template = self.identity_instance(nominal_base(&trait_ref).clone());
            let mut unsafe_methods = def
                .methods
                .values()
                .filter(|method| {
                    !method.is_static
                        && method.sig.type_param_count == 0
                        && !self.dyn_dispatchable(&template, method)
                })
                .map(|method| method.name.clone())
                .collect::<Vec<_>>();
            unsafe_methods.sort_unstable();
            for name in unsafe_methods {
                self.type_error(module, span, format!("`dyn {trait_ref}` 不是对象安全的：方法 `{name}` 为异步或在签名中使用裸 `Self`"));
            }
        }
    }

    /// 允许拥有型转换的唯一入口。保留原表达式类型供后端装箱 / 保留引用。
    pub(crate) fn record_dyn_conversion(
        &mut self,
        module: ModuleId,
        span: Span,
        target: &Ty,
        source: &Ty,
    ) -> bool {
        let Ty::Dynamic {
            trait_ref,
            bindings,
        } = target
        else {
            return false;
        };
        if !matches!(
            nominal_base(source),
            Ty::Class(..) | Ty::Struct(..) | Ty::TypeParam(_)
        ) || !self.satisfies_bound(trait_ref, source)
        {
            return false;
        }
        if !bindings.iter().all(|(name, want)| {
            let have = Ty::Associated {
                receiver: Box::new(source.clone()),
                trait_ref: trait_ref.clone(),
                name: name.clone(),
            };
            ty_eq(&self.normalize_associated(&have), want)
        }) {
            return false;
        }
        self.dyn_conversions.insert(
            (module, span),
            DynConversion {
                source: source.view(),
                target: target.view(),
            },
        );
        true
    }
}

//! 消费 sema 的调用实例化结果，选择单态实体或动态槽位。

use super::*;

pub(super) enum CallTarget {
    Expression,
    Direct { return_type: HirType },
    Dynamic(u32),
}

impl BodyBuilder<'_> {
    pub(super) fn resolve_call_instance(
        &self,
        instance: &novella_sema::CallInstance,
        subst: &mut Substitution,
        span: Span,
    ) -> Result<(FunctionSignature, CallTarget), BuildError> {
        let view = &instance.signature;
        // 匿名签名表示函数值而非可实例化声明；其中自由形参只采用外层代入。
        if view.name.is_none() {
            return Ok((
                mono::concrete_signature(self.sema, &self.instances, view, &self.subst)?,
                CallTarget::Expression,
            ));
        }
        let receiver = instance
            .receiver
            .as_ref()
            .map(|ty| {
                mono::native_view(
                    self.sema,
                    &self.instances,
                    &mono::substitute(ty, &self.subst),
                    &self.subst,
                    0,
                )
            })
            .transpose()?;
        if let Some(receiver @ TypeView::Dynamic { trait_ref, .. }) = &receiver {
            let ty = self.concrete_type(receiver, None)?;
            let (_, method) = view
                .name
                .as_deref()
                .and_then(|name| name.split_once("::"))
                .ok_or_else(|| {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "动态派发方法",
                    })
                })?;
            let slot = self.dynamic_slot(&ty, method, span)?;
            mono::bind_trait_receiver(self.sema, subst, trait_ref, receiver)?;
            return Ok((
                mono::concrete_signature(self.sema, &self.instances, view, &self.subst)?,
                CallTarget::Dynamic(slot),
            ));
        }
        if let Some((owner, method_name)) =
            view.name.as_deref().and_then(|name| name.split_once("::"))
            && let Some(definition) = mono::summary(self.sema, view.module, owner)
        {
            if definition.kind == novella_sema::TypeSummaryKind::Trait {
                let missing = || {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "trait 静态派发实现",
                    })
                };
                let receiver = receiver.as_ref().ok_or_else(missing)?;
                let trait_ref = if let Some(trait_ref) = &instance.trait_ref {
                    mono::native_view(
                        self.sema,
                        &self.instances,
                        &mono::substitute(trait_ref, &self.subst),
                        &self.subst,
                        0,
                    )?
                } else {
                    TypeView::Trait(view.module, owner.to_owned())
                };
                let (implementation, impl_subst) = mono::resolve_trait_implementation(
                    self.sema,
                    &self.instances,
                    receiver,
                    &trait_ref,
                )?
                .ok_or_else(missing)?;
                let method = implementation
                    .methods
                    .iter()
                    .find(|method| method.name == method_name)
                    .ok_or_else(missing)?;
                let arguments = mono::trait_method_arguments(self.sema, view, method, subst)?;
                mono::bind_trait_receiver(self.sema, subst, &trait_ref, receiver)?;
                let target = mono::instantiate_trait_method(
                    self.sema,
                    &self.instances,
                    implementation,
                    &impl_subst,
                    method,
                    &arguments,
                )?;
                // 默认实参、参数类型和静态返回类型仍由调用点看见的 trait 契约决定。
                let mut signature =
                    mono::concrete_signature(self.sema, &self.instances, view, &self.subst)?;
                signature.id = target.id;
                return Ok((
                    signature,
                    CallTarget::Direct {
                        return_type: target.ret,
                    },
                ));
            }
            // 裸成员调用没有目标表达式；从已具体化的 self 取得声明属主代入。
            if definition
                .type_parameters
                .iter()
                .any(|key| !subst.iter().any(|(present, _)| present == key))
            {
                let receiver = receiver.as_ref().and_then(mono::nominal_key).or_else(|| {
                    self.lookup_local("self")
                        .and_then(|id| self.local_types.get(id.0 as usize))
                        .and_then(hir_nominal_key)
                });
                if let Some((module, name)) = receiver {
                    let chain =
                        layout::instance_lineage(self.sema, &self.instances, module, name, span)?;
                    if let Some((_, owner_subst)) = chain
                        .into_iter()
                        .find(|(item, _)| item.module == view.module && item.name == owner)
                    {
                        for pair in owner_subst {
                            if !subst.iter().any(|(key, _)| key == &pair.0) {
                                subst.push(pair);
                            }
                        }
                    }
                }
                subst.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            }
        }
        if subst.is_empty() {
            return Ok((
                mono::concrete_signature(self.sema, &self.instances, view, &self.subst)?,
                CallTarget::Expression,
            ));
        }
        let call_view = mono::substitute(&TypeView::Function(Box::new(view.clone())), &self.subst);
        let TypeView::Function(call_view) = call_view else {
            unreachable!()
        };
        let signature =
            mono::instantiate_function(self.sema, &self.instances, &call_view, subst.clone())?;
        let target = CallTarget::Direct {
            return_type: signature.ret.clone(),
        };
        Ok((signature, target))
    }
}

fn hir_nominal_key(ty: &HirType) -> Option<(ModuleId, &str)> {
    match ty {
        HirType::Class(module, name) | HirType::Struct(module, name) => Some((*module, name)),
        HirType::Pointer(inner) => hir_nominal_key(inner),
        _ => None,
    }
}

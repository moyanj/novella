//! 类型参数身份、实例环境、代入与调用推导。
use super::*;
/// 源码声明身份参与泛型形参判等，避免不同声明中的同名 T 串用。
pub(crate) fn param_key(module: ModuleId, param: &novella_ast::types::TypeParameter) -> String {
    format!("{}@m{}:{}", param.name, module, param.span.start)
}

/// 编译器内建标准异常根的稳定名义身份，不占用用户模块编号。
pub(crate) fn exception_type() -> Ty {
    Ty::Class(ModuleId::MAX, "Exception".to_owned())
}

pub(crate) fn instance_substitution(sema: &crate::Sema, ty: &Ty) -> HashMap<String, Ty> {
    if let Ty::Applied(base, args) = ty {
        let key = crate::typecheck::this_ty_key(base);
        sema.type_defs
            .get(&key)
            .map(|def| {
                def.type_params()
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        HashMap::new()
    }
}

/// 沿实际实例的父链代入类型实参，返回成员定义环境中的替换表。
pub(crate) fn member_substitution(
    sema: &crate::Sema,
    ty: &Ty,
    owner: &(ModuleId, String),
) -> HashMap<String, Ty> {
    if matches!(sema.type_defs.get(owner), Some(TypeDef::Trait(_))) {
        let Some(trait_ref) = sema
            .implemented_traits(ty)
            .into_iter()
            .find(|t| crate::typecheck::this_ty_key(t) == *owner)
        else {
            return HashMap::new();
        };
        let mut subst = instance_substitution(sema, &trait_ref);
        subst.insert(crate::traits::self_key(owner.0, &owner.1), ty.clone());
        return subst;
    }
    if !matches!(
        crate::traits::nominal_base(ty),
        Ty::Struct(..) | Ty::Class(..) | Ty::Trait(..)
    ) {
        return HashMap::new();
    }
    let mut current = ty.clone();
    let mut visited = std::collections::HashSet::new();
    loop {
        let key = crate::typecheck::this_ty_key(&current);
        let subst = instance_substitution(sema, &current);
        if key == *owner || !visited.insert(key.clone()) {
            return subst;
        }
        let Some(TypeDef::Class(def)) = sema.type_defs.get(&key) else {
            return subst;
        };
        let Some(parent) = &def.parent_type else {
            return subst;
        };
        current = substitute_type_params(parent, &subst);
    }
}

/// 泛型形参替换：把 `ty` 里出现的 [`Ty::TypeParam`] 按 `subst` 换成实参类型（D05）。
///
/// 浅泛型——只替换 `T` 本身；`Array<T>` / `Pointer<T>` / `Tuple` / `Union` /
/// `Function` 里的 `T` 一并递归替换，保证 `func f(items: T[])` 这类签名在代入后
/// 仍是数组形状。`subst` 里没有的形参原样保留（调用方据此判定「推导不出」）。
pub(crate) fn substitute_type_params(ty: &Ty, subst: &std::collections::HashMap<String, Ty>) -> Ty {
    if subst.is_empty() {
        return ty.clone();
    }
    rewrite_type(ty, &mut |ty| match ty {
        Ty::TypeParam(name) => subst.get(name).cloned(),
        _ => None,
    })
}

/// 逐层重写类型；命中的节点由调用者完全替换，不再递归进入替换值。
pub(crate) fn rewrite_type(ty: &Ty, rewrite: &mut impl FnMut(&Ty) -> Option<Ty>) -> Ty {
    if let Some(ty) = rewrite(ty) {
        return ty;
    }
    match ty {
        Ty::Applied(base, args) => Ty::Applied(
            base.clone(),
            args.iter().map(|t| rewrite_type(t, rewrite)).collect(),
        ),
        Ty::Future(inner) => Ty::Future(Box::new(rewrite_type(inner, rewrite))),
        Ty::Associated {
            receiver,
            trait_ref,
            name,
        } => Ty::Associated {
            receiver: Box::new(rewrite_type(receiver, rewrite)),
            trait_ref: Box::new(rewrite_type(trait_ref, rewrite)),
            name: name.clone(),
        },
        Ty::Dynamic {
            trait_ref,
            bindings,
        } => Ty::Dynamic {
            trait_ref: Box::new(rewrite_type(trait_ref, rewrite)),
            bindings: bindings
                .iter()
                .map(|(n, t)| (n.clone(), rewrite_type(t, rewrite)))
                .collect(),
        },
        Ty::Object(fields) => Ty::Object(
            fields
                .iter()
                .map(|(n, t)| (n.clone(), rewrite_type(t, rewrite)))
                .collect(),
        ),
        Ty::Array(element, length) => Ty::Array(Box::new(rewrite_type(element, rewrite)), *length),
        Ty::Pointer(element) => Ty::Pointer(Box::new(rewrite_type(element, rewrite))),
        Ty::Tuple(elements) => {
            Ty::Tuple(elements.iter().map(|e| rewrite_type(e, rewrite)).collect())
        }
        Ty::Union(members) => Ty::Union(members.iter().map(|m| rewrite_type(m, rewrite)).collect()),
        Ty::Function(sig) => {
            let params = sig
                .params
                .iter()
                .map(|p| ParamSig {
                    name: p.name.clone(),
                    ty: rewrite_type(&p.ty, rewrite),
                    has_default: p.has_default,
                })
                .collect();
            Ty::Function(std::rc::Rc::new(FunctionSig {
                module: sig.module,
                name: sig.name.clone(),
                params,
                ret: rewrite_type(&sig.ret, rewrite),
                type_param_count: sig.type_param_count,
                type_params: sig.type_params.clone(),
                is_async: sig.is_async,
                is_extern: sig.is_extern,
                is_method: sig.is_method,
            }))
        }
        // 其余变体（含 Struct / Class / Trait 的泛型实参，见 S05 未闭环项）不含可代换形参
        _ => ty.clone(),
    }
}

/// 从「形参类型 ← 实参类型」推出一条形参绑定；不一致或有冲突则返回冲突说明。
///
/// 只处理浅层位置：直接 `T`、以及 `T[]` / `T*` / `(T, U)` / 联合里的 `T`。
/// 已绑定过的形参再次出现时必须**完全相等**（不同则推导失败）。
pub(crate) fn infer_type_param(
    param: &Ty,
    actual: &Ty,
    subst: &mut std::collections::HashMap<String, Ty>,
) -> Result<(), String> {
    match param {
        Ty::Applied(base, args) => {
            if let Ty::Applied(other, actuals) = actual
                && ty_eq(base, other)
                && args.len() == actuals.len()
            {
                for (param, actual) in args.iter().zip(actuals) {
                    infer_type_param(param, actual, subst)?;
                }
            }
            Ok(())
        }
        Ty::Future(inner) => {
            if let Ty::Future(actual) = actual {
                infer_type_param(inner, actual, subst)?;
            }
            Ok(())
        }
        Ty::TypeParam(name) => {
            // 不透明实参（未知 / 外部 / 错误）不能作为推导依据
            if actual.is_opaque()
                || matches!(actual, Ty::Null | Ty::IntLiteral(..) | Ty::FloatLiteral(_))
            {
                return Ok(());
            }
            match subst.get(name) {
                Some(existing) if !ty_eq(existing, actual) => Err(format!(
                    "泛型实参推导冲突：形参 `{name}` 先推出 `{existing}`，后又推出 `{actual}`"
                )),
                Some(_) => Ok(()),
                None => {
                    subst.insert(name.clone(), actual.clone());
                    Ok(())
                }
            }
        }
        Ty::Array(element, length) => match actual {
            Ty::Array(actual_element, actual_length) if length == actual_length => {
                infer_type_param(element, actual_element, subst)
            }
            _ => Ok(()),
        },
        Ty::Pointer(element) => match actual {
            Ty::Pointer(actual_element) => infer_type_param(element, actual_element, subst),
            _ => Ok(()),
        },
        Ty::Tuple(elements) => match actual {
            Ty::Tuple(actuals) if actuals.len() == elements.len() => {
                for (p, a) in elements.iter().zip(actuals.iter()) {
                    infer_type_param(p, a, subst)?;
                }
                Ok(())
            }
            _ => Ok(()),
        },
        Ty::Function(sig) => {
            if let Ty::Function(other) = actual
                && sig.params.len() == other.params.len()
            {
                for (param, actual) in sig.params.iter().zip(&other.params) {
                    infer_type_param(&param.ty, &actual.ty, subst)?;
                }
                infer_type_param(&sig.ret, &other.ret, subst)?;
            }
            Ok(())
        }
        Ty::Union(members) => {
            // 先移除已有具体分支；仅在剩余形状唯一时推导，避免依赖成员顺序。
            let actuals = match actual {
                Ty::Union(items) => items.as_slice(),
                _ => std::slice::from_ref(actual),
            };
            for actual in actuals {
                if members.iter().any(|member| ty_eq(member, actual)) {
                    continue;
                }
                let mut candidates = Vec::new();
                for member in members {
                    let mut trial = subst.clone();
                    if infer_type_param(member, actual, &mut trial).is_ok()
                        && trial
                            .iter()
                            .any(|(name, ty)| subst.get(name).is_none_or(|prior| !ty_eq(prior, ty)))
                    {
                        candidates.push(trial);
                    }
                }
                if candidates.len() == 1 {
                    *subst = candidates.pop().expect("唯一候选");
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

//! 检查同一接收者上的 trait 应用是否可能在实例化后重合。
use std::collections::HashMap;

use crate::ty::{Ty, rewrite_type, substitute_type_params, ty_eq};

pub(super) fn overlap(left: &Ty, right: &Ty) -> bool {
    unify(left, right, &mut HashMap::new())
}

fn unify(left: &Ty, right: &Ty, subst: &mut HashMap<String, Ty>) -> bool {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        unify_inner(left, right, subst)
    })
}

fn unify_inner(left: &Ty, right: &Ty, subst: &mut HashMap<String, Ty>) -> bool {
    if ty_eq(left, right) {
        return true;
    }
    match (left, right) {
        // 未归约的投影没有固定形状，不能用发生检查证明不相交。
        (Ty::Associated { .. }, _) | (_, Ty::Associated { .. }) => true,
        (Ty::TypeParam(name), ty) | (ty, Ty::TypeParam(name)) => {
            if let Some(assigned) = subst.get(name).cloned() {
                return unify(&assigned, ty, subst);
            }
            let value = rewrite_type(ty, &mut |part| match part {
                Ty::TypeParam(key) => subst.get(key).cloned(),
                _ => None,
            });
            if matches!(&value, Ty::TypeParam(key) if key == name) {
                return true;
            }
            let mut occurs = false;
            rewrite_type(&value, &mut |part| {
                occurs |= matches!(part, Ty::TypeParam(key) if key == name);
                None
            });
            if occurs {
                return false;
            }
            // 保持映射已代入，隐式类型变量循环不能蒙混通过发生检查。
            let replacement = HashMap::from([(name.clone(), value.clone())]);
            for old in subst.values_mut() {
                *old = substitute_type_params(old, &replacement);
            }
            subst.insert(name.clone(), value);
            true
        }
        (Ty::Applied(base, left), Ty::Applied(other, right)) => {
            ty_eq(base, other) && unify_list(left, right, subst)
        }
        (Ty::Pointer(left), Ty::Pointer(right)) | (Ty::Future(left), Ty::Future(right)) => {
            unify(left, right, subst)
        }
        (Ty::Array(left, a), Ty::Array(right, b)) => a == b && unify(left, right, subst),
        (Ty::Tuple(left), Ty::Tuple(right)) => unify_list(left, right, subst),
        (Ty::Function(left), Ty::Function(right)) => {
            left.params.len() == right.params.len()
                && left
                    .params
                    .iter()
                    .zip(&right.params)
                    .all(|(a, b)| unify(&a.ty, &b.ty, subst))
                && unify(&left.ret, &right.ret, subst)
        }
        (
            Ty::Dynamic {
                trait_ref: a,
                bindings: left,
            },
            Ty::Dynamic {
                trait_ref: b,
                bindings: right,
            },
        ) => {
            left.len() == right.len()
                && unify(a, b, subst)
                && left
                    .iter()
                    .zip(right)
                    .all(|((name, a), (other, b))| name == other && unify(a, b, subst))
        }
        (Ty::Object(left), Ty::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|((name, a), (other, b))| name == other && unify(a, b, subst))
        }
        // 联合没有顺序，未归约的投影没有固定形状。无法证明不相交时保守拒绝，
        // 但完全定型、元素不等的联合可证明不相交。
        (Ty::Union(left), Ty::Union(right)) => {
            left.len() == right.len() && left.iter().chain(right).any(has_unresolved)
        }
        _ => false,
    }
}

fn unify_list(left: &[Ty], right: &[Ty], subst: &mut HashMap<String, Ty>) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| unify(a, b, subst))
}

fn has_unresolved(ty: &Ty) -> bool {
    let mut unresolved = false;
    rewrite_type(ty, &mut |ty| {
        unresolved |= matches!(ty, Ty::TypeParam(_) | Ty::Associated { .. });
        None
    });
    unresolved
}

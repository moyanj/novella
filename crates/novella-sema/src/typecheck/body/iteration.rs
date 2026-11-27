//! 数组与 `iter` / `hasNext` / `next` 协议的静态检查。

use novella_core::Span;

use crate::Sema;
use crate::ty::{Ty, member_substitution, substitute_type_params};
use crate::typecheck::member::{MemberInfo, lookup_typed_member_chain, type_key_of};

use super::BodyCtx;

pub(super) fn element_type(sema: &mut Sema, ctx: &BodyCtx, source: Ty, span: Span) -> Ty {
    let source = match source {
        Ty::Array(element, _) => return *element,
        ty if ty.is_opaque() => return ty,
        ty => ty,
    };

    let iterator = protocol_method_return(sema, ctx, &source, "iter", span);
    if iterator.is_opaque() {
        return iterator;
    }
    let condition = protocol_method_return(sema, ctx, &iterator, "hasNext", span);
    if !matches!(condition, Ty::Boolean) && !condition.is_opaque() {
        sema.type_error(
            ctx.module,
            span,
            format!("迭代器 `{iterator}` 的 `hasNext()` 必须返回 `boolean`，实为 `{condition}`"),
        );
    }
    let element = protocol_method_return(sema, ctx, &iterator, "next", span);
    if matches!(element, Ty::Void) {
        sema.type_error(
            ctx.module,
            span,
            format!("迭代器 `{iterator}` 的 `next()` 必须返回元素，不能返回 `void`"),
        );
        return Ty::Error;
    }
    element
}

fn protocol_method_return(
    sema: &mut Sema,
    ctx: &BodyCtx,
    receiver: &Ty,
    name: &str,
    span: Span,
) -> Ty {
    let lookup_ty = match receiver {
        Ty::TypeParam(param) => sema
            .type_param_bounds
            .get(param)
            .unwrap_or(receiver)
            .clone(),
        _ => receiver.clone(),
    };
    if lookup_ty.is_opaque() {
        return lookup_ty;
    }
    let base = match &lookup_ty {
        Ty::Applied(base, _) => &**base,
        other => other,
    };
    let hit = match base {
        Ty::Struct(..) | Ty::Class(..) | Ty::Trait(..) => {
            lookup_typed_member_chain(sema, &type_key_of(&lookup_ty), name, false)
        }
        _ => None,
    };
    let Some(hit) = hit else {
        sema.type_error(
            ctx.module,
            span,
            format!("`for ... in` 的迭代协议要求类型 `{receiver}` 提供实例方法 `{name}()`"),
        );
        return Ty::Error;
    };
    let MemberInfo::Method(method) = &hit.info else {
        sema.type_error(
            ctx.module,
            span,
            format!("迭代协议的 `{receiver}.{name}` 必须是实例方法"),
        );
        return Ty::Error;
    };
    let params = &method.sig.params;
    let has_explicit_params = match params.as_slice() {
        [] => false,
        [receiver] => receiver.name != "self",
        _ => true,
    };
    if method.is_static || method.sig.type_param_count != 0 || has_explicit_params {
        sema.type_error(
            ctx.module,
            span,
            format!("迭代协议的 `{receiver}.{name}()` 必须是无显式参数、无泛型的实例方法"),
        );
        return Ty::Error;
    }
    let subst = member_substitution(sema, &lookup_ty, &hit.owner);
    let result = substitute_type_params(&method.sig.ret, &subst);
    hit.check_visible(sema, ctx, name, span);
    ctx.record_possible_exception();
    result
}

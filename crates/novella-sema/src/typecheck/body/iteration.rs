//! 数组与 `iter` / `hasNext` / `next` 协议的静态检查。

use novella_core::Span;

use crate::Sema;
use crate::ty::{Ty, member_substitution};
use crate::typecheck::member::{
    MemberInfo, lookup_typed_member_chain, trait_method_hit, type_key_of,
};

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
    if receiver.is_opaque() {
        return receiver.clone();
    }
    let base = crate::traits::nominal_base(receiver);
    let nominal = matches!(base, Ty::Struct(..) | Ty::Class(..) | Ty::Trait(..));
    let mut hit = nominal
        .then(|| lookup_typed_member_chain(sema, &type_key_of(receiver), name, false))
        .flatten();
    let mut applied_trait = None;
    if hit.is_none() {
        match trait_method_hit(sema, ctx, receiver, name, span, nominal) {
            Some(Ok((trait_ref, found))) => {
                applied_trait = Some(trait_ref);
                hit = Some(found);
            }
            Some(Err(())) => return Ty::Error,
            None => {}
        }
    }
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
    let result = if let Some(trait_ref) = applied_trait {
        sema.instantiate_trait_member(&method.sig.ret, &trait_ref, receiver)
    } else {
        sema.instantiate_type(
            &method.sig.ret,
            &member_substitution(sema, receiver, &hit.owner),
        )
    };
    hit.check_visible(sema, ctx, name, span);
    ctx.record_possible_exception();
    result
}

//! 成员访问检查：实例成员 `a.b`、静态成员 `Type.member`、成员链推导。

use std::collections::HashSet;
use std::rc::Rc;

use novella_ast::expr::Expression;
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::NsTarget;
use crate::ty::{Ty, TypeDef};

use super::body::BodyCtx;
use super::expr::{
    IdentTarget, check_expr, classify_identifier, type_key_of_name, value_type_of_symbol,
};
use super::unsafe_rules::require_unsafe;

/// 成员访问 `a.b`：模块成员 / 静态成员 / 实例成员（class 沿继承链）/ 穿指针成员。
pub(crate) fn check_member_access(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    name: &str,
    span: Span,
) -> Ty {
    // 特殊路径 1：`ns.member`（命名空间导入）
    if let Expression::Identifier {
        name: target_name, ..
    } = target
    {
        match classify_identifier(sema, ctx, target_name) {
            Some(IdentTarget::Namespace(NsTarget::StdExternal)) => {
                return Ty::External; // std 外部符号不做成员检查
            }
            Some(IdentTarget::Namespace(NsTarget::Module(target_module))) => {
                return match sema.global_scope_symbol(target_module, name) {
                    None => {
                        sema.type_error(
                            ctx.module,
                            span,
                            format!("模块 `{target_name}` 中没有 `{name}`"),
                        );
                        Ty::Unknown
                    }
                    Some(symbol) => {
                        if !symbol.is_importable() {
                            sema.type_error(
                                ctx.module,
                                span,
                                format!("`{name}` 在模块 `{target_name}` 中不是 pub"),
                            );
                        }
                        sema.record_reference(ctx.module, span, &symbol);
                        value_type_of_symbol(sema, ctx, symbol, name, span)
                    }
                };
            }
            Some(IdentTarget::Type) => {
                // 特殊路径 2：`Type.member` → 仅静态成员
                return check_static_member(sema, ctx, target_name, name, span);
            }
            _ => {}
        }
    }
    let target_ty = check_expr(sema, ctx, target);
    member_type_of(sema, ctx, &target_ty, name, span)
}
/// `Type.member`：仅静态成员（含继承链）。
pub(crate) fn check_static_member(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    type_name: &str,
    name: &str,
    span: Span,
) -> Ty {
    // 还原类型键（普通类型符号在定义模块；具名导入的在目标模块）
    let type_key = type_key_of_name(sema, ctx.scope, type_name);
    let Some(type_key) = type_key else {
        sema.type_error(ctx.module, span, format!("`{type_name}` 不是类型"));
        return Ty::Unknown;
    };
    match lookup_member_chain(sema, &type_key, name, true) {
        Some(MemberInfo::Field(field)) => {
            let _ = field;
            sema.type_error(
                ctx.module,
                span,
                format!("字段 `{name}` 不是静态成员（struct/class 字段随实例）"),
            );
            Ty::Unknown
        }
        Some(MemberInfo::Method(method)) => {
            if !method.is_static {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("方法 `{name}` 不是静态方法，不能通过类型名访问"),
                );
            }
            Ty::Function(Rc::new(method.sig))
        }
        None => {
            sema.type_error(
                ctx.module,
                span,
                format!("类型 `{type_name}` 没有成员 `{name}`"),
            );
            Ty::Unknown
        }
    }
}
/// 值成员访问：struct/class 字段与方法（class 沿继承链），穿指针成员须 unsafe。
pub(crate) fn member_type_of(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target_ty: &Ty,
    name: &str,
    span: Span,
) -> Ty {
    match target_ty {
        Ty::Struct(..) | Ty::Class(..) => {
            let key = type_key_of(target_ty);
            match lookup_member_chain(sema, &key, name, false) {
                Some(MemberInfo::Field(field)) => field.ty,
                Some(MemberInfo::Method(method)) => Ty::Function(Rc::new(method.sig)),
                None => {
                    sema.type_error(
                        ctx.module,
                        span,
                        format!("类型 `{target_ty}` 没有成员 `{name}`"),
                    );
                    Ty::Unknown
                }
            }
        }
        Ty::Pointer(inner) => {
            // S3：穿指针的成员访问须 unsafe
            require_unsafe(sema, ctx, span, "通过指针访问成员");
            match &**inner {
                Ty::Struct(..) | Ty::Class(..) => member_type_of(sema, ctx, inner, name, span),
                ty if ty.is_opaque() => Ty::Unknown,
                ty => {
                    sema.type_error(ctx.module, span, format!("`{ty}` 没有成员 `{name}`"));
                    Ty::Unknown
                }
            }
        }
        ty if ty.is_opaque() => Ty::Unknown,
        ty => {
            sema.type_error(ctx.module, span, format!("`{ty}` 没有成员 `{name}`"));
            Ty::Unknown
        }
    }
}
pub(crate) fn type_key_of(ty: &Ty) -> (ModuleId, String) {
    match ty {
        Ty::Struct(m, n) | Ty::Class(m, n) => (*m, n.clone()),
        _ => unreachable!("仅 Struct/Class 调用"),
    }
}
pub(crate) enum MemberInfo {
    Field(crate::ty::FieldDef),
    Method(crate::ty::MethodSig),
}
/// 沿 class 继承链查找成员（struct 只看自身；static_only 时只看静态方法）。
pub(crate) fn lookup_member_chain(
    sema: &Sema,
    key: &(ModuleId, String),
    name: &str,
    static_only: bool,
) -> Option<MemberInfo> {
    let mut current = key.clone();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return None; // 继承环（注册阶段已报过诊断）
        }
        match sema.type_defs.get(&current)? {
            TypeDef::Struct(def) => {
                if !static_only && let Some(field) = def.fields.iter().find(|f| f.name == name) {
                    return Some(MemberInfo::Field(field.clone()));
                }
                if let Some(method) = def.methods.get(name)
                    && (!static_only || method.is_static)
                {
                    return Some(MemberInfo::Method(method.clone()));
                }
                return None; // struct 无继承
            }
            TypeDef::Class(def) => {
                if !static_only && let Some(field) = def.fields.iter().find(|f| f.name == name) {
                    return Some(MemberInfo::Field(field.clone()));
                }
                if let Some(method) = def.methods.get(name)
                    && (!static_only || method.is_static)
                {
                    return Some(MemberInfo::Method(method.clone()));
                }
                // 继续向父类查找
                match &def.parent_resolved {
                    Some((parent_module, parent_name)) => {
                        current = (*parent_module, parent_name.clone());
                    }
                    None => return None,
                }
            }
            _ => return None,
        }
    }
}

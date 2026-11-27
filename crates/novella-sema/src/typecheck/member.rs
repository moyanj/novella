//! 成员访问检查：实例成员 `a.b`、静态成员 `Type.member`、成员链推导。

use std::collections::HashSet;
use std::rc::Rc;

use novella_ast::expr::Expression;
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::resolve::{ExportTarget, ReexportError, resolve_exported};
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
            Some(IdentTarget::Namespace(NsTarget::ExceptionBuiltin)) => {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "std:exception 没有可用的值成员 `{name}`；Exception 仅可作类型或构造目标"
                    ),
                );
                return Ty::Error;
            }
            Some(IdentTarget::Namespace(NsTarget::Module(target_module))) => {
                if sema.modules[target_module].load_failed {
                    return Ty::Error;
                }
                // 跨模块成员访问同样走再导出链（`pub` 重导入可多层）
                return match resolve_exported(sema, target_module, name) {
                    Ok(ExportTarget::Symbol(symbol)) => {
                        sema.record_reference(
                            ctx.module,
                            Span {
                                start: span.end.saturating_sub(name.len()),
                                end: span.end,
                            },
                            &symbol,
                        );
                        value_type_of_symbol(sema, ctx, *symbol, name, span)
                    }
                    Ok(ExportTarget::Namespace(ns_target)) => {
                        let _ = ns_target;
                        sema.type_error(
                            ctx.module,
                            span,
                            format!("`{name}` 是模块 `{target_name}` 的命名空间，不能作为值使用"),
                        );
                        Ty::Error
                    }
                    Ok(ExportTarget::ExceptionRoot) => {
                        sema.type_error(
                            ctx.module,
                            span,
                            format!("`{name}` 是标准库内建类型，不能作为普通值使用"),
                        );
                        Ty::Error
                    }
                    Err(ReexportError::Missing { .. }) => {
                        sema.type_error(
                            ctx.module,
                            span,
                            format!("模块 `{target_name}` 中没有 `{name}`"),
                        );
                        Ty::Error
                    }
                    Err(error) => {
                        sema.type_error(ctx.module, span, error.message());
                        Ty::Error
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
    // 访问成员只借用接收者，不建立新的对象所有权。
    ctx.borrowed_receiver = true;
    let target_ty = check_expr(sema, ctx, target);
    let ty = member_type_of(sema, ctx, &target_ty, name, span);
    // 尚无借用效果分析，任何自身实例方法都可能把析构接收者转为强引用。
    // 只检查直接 self/this（及括号），字段对象的方法具有独立所有权，仍可调用。
    let mut receiver = target;
    while let Expression::Paren { expr, .. } = receiver {
        receiver = expr;
    }
    let destructor_receiver = ctx.destructor_self.is_some_and(|parameter_span| {
        matches!(receiver, Expression::This { .. })
            || matches!(receiver, Expression::Identifier { name, .. }
                if name == "self" && sema.scopes.lookup(ctx.scope, name).is_some_and(|symbol| symbol.span == parameter_span))
    });
    if destructor_receiver && matches!(&ty, Ty::Function(signature) if signature.is_method) {
        sema.type_error(
            ctx.module,
            span,
            "析构接收者不能调用或提取自身实例方法；该方法可能间接建立 self 的强引用",
        );
    }
    ty
}
/// 解析静态成员属主，读写共用别名展开规则。
pub(crate) fn static_receiver(sema: &mut Sema, ctx: &BodyCtx, type_name: &str, span: Span) -> Ty {
    // 还原类型键（普通类型符号在定义模块；具名导入的在目标模块）
    let type_key = type_key_of_name(sema, ctx.scope, type_name);
    let Some(type_key) = type_key else {
        sema.type_error(ctx.module, span, format!("`{type_name}` 不是类型"));
        return Ty::Error;
    };
    match sema.type_defs.get(&type_key) {
        Some(TypeDef::Alias(_)) => sema.resolve_type(
            ctx.module,
            ctx.scope,
            &novella_ast::types::TypeExpr::Reference {
                qualifier: vec![],
                name: type_name.to_owned(),
                generic_args: vec![],
                span: Span {
                    start: span.start,
                    end: span.start + type_name.len(),
                },
            },
            &ctx.type_env,
        ),
        Some(TypeDef::Struct(_)) => Ty::Struct(type_key.0, type_key.1.clone()),
        Some(TypeDef::Class(_)) => Ty::Class(type_key.0, type_key.1.clone()),
        Some(TypeDef::Trait(_)) => Ty::Trait(type_key.0, type_key.1.clone()),
        None => Ty::Error,
    }
}
/// `Type.member`：仅静态成员（含继承链）。
pub(crate) fn check_static_member(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    type_name: &str,
    name: &str,
    span: Span,
) -> Ty {
    let receiver = static_receiver(sema, ctx, type_name, span);
    if receiver.is_opaque() {
        return receiver;
    }
    let base = match &receiver {
        Ty::Applied(base, _) => &**base,
        ty => ty,
    };
    if !matches!(base, Ty::Struct(..) | Ty::Class(..) | Ty::Trait(..)) {
        sema.type_error(
            ctx.module,
            span,
            format!("类型 `{receiver}` 没有静态成员 `{name}`"),
        );
        return Ty::Error;
    }
    let type_key = type_key_of(&receiver);
    match lookup_typed_member_chain(sema, &type_key, name, true) {
        Some(hit) => {
            let subst = crate::ty::member_substitution(sema, &receiver, &hit.owner);
            hit.record(sema, ctx, name, span);
            match &hit.info {
                MemberInfo::Field(field) => {
                    if !field.is_static {
                        sema.type_error(
                            ctx.module,
                            span,
                            format!("字段 `{name}` 不是静态成员（struct/class 字段随实例）"),
                        );
                    }
                    hit.check_visible(sema, ctx, name, span);
                    let ty = crate::check::fields::field_type(sema, &hit.owner, name);
                    crate::ty::substitute_type_params(&ty, &subst)
                }
                MemberInfo::Method(method) => {
                    if !method.is_static {
                        sema.type_error(
                            ctx.module,
                            span,
                            format!("方法 `{name}` 不是静态方法，不能通过类型名访问"),
                        );
                    }
                    hit.check_visible(sema, ctx, name, span);
                    crate::ty::substitute_type_params(
                        &Ty::Function(Rc::new(method.sig.clone())),
                        &subst,
                    )
                }
            }
        }
        None => {
            sema.type_error(
                ctx.module,
                span,
                format!("类型 `{type_name}` 没有成员 `{name}`"),
            );
            Ty::Error
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
        Ty::TypeParam(param) => {
            if let Some(bound) = sema.type_param_bounds.get(param).cloned() {
                member_type_of(sema, ctx, &bound, name, span)
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("无约束类型参数 `{param}` 没有已知成员 `{name}`"),
                );
                Ty::Error
            }
        }
        Ty::Object(fields) => fields.get(name).cloned().unwrap_or_else(|| {
            sema.type_error(ctx.module, span, format!("对象没有字段 `{name}`"));
            Ty::Error
        }),
        Ty::Applied(..) | Ty::Trait(..) | Ty::Struct(..) | Ty::Class(..) => {
            let key = type_key_of(target_ty);
            match lookup_typed_member_chain(sema, &key, name, false) {
                Some(hit) => {
                    hit.record(sema, ctx, name, span);
                    hit.check_visible(sema, ctx, name, span);
                    let subst = crate::ty::member_substitution(sema, target_ty, &hit.owner);
                    let result = match hit.info {
                        MemberInfo::Field(field) => {
                            check_static_access(sema, ctx, &field, name, span);
                            crate::check::fields::field_type(sema, &hit.owner, name)
                        }
                        MemberInfo::Method(method) => {
                            check_static_access(sema, ctx, &method, name, span);
                            Ty::Function(Rc::new(method.sig))
                        }
                    };
                    crate::ty::substitute_type_params(&result, &subst)
                }
                None => {
                    sema.type_error(
                        ctx.module,
                        span,
                        format!("类型 `{target_ty}` 没有成员 `{name}`"),
                    );
                    Ty::Error
                }
            }
        }
        Ty::Pointer(inner) => {
            // S3：穿指针的成员访问须 unsafe
            require_unsafe(sema, ctx, span, "通过指针访问成员");
            match &**inner {
                Ty::Struct(..) | Ty::Class(..) | Ty::Applied(..) => {
                    member_type_of(sema, ctx, inner, name, span)
                }
                ty if ty.is_opaque() => ty.clone(),
                ty => {
                    sema.type_error(ctx.module, span, format!("`{ty}` 没有成员 `{name}`"));
                    Ty::Error
                }
            }
        }
        ty if ty.is_opaque() => ty.clone(),
        ty => {
            sema.type_error(ctx.module, span, format!("`{ty}` 没有成员 `{name}`"));
            Ty::Error
        }
    }
}
pub(crate) fn type_key_of(ty: &Ty) -> (ModuleId, String) {
    match ty {
        Ty::Applied(base, _) => type_key_of(base),
        Ty::Struct(m, n) | Ty::Class(m, n) | Ty::Trait(m, n) => (*m, n.clone()),
        _ => unreachable!("仅 Struct/Class 调用"),
    }
}
pub(crate) enum MemberInfo {
    Field(crate::ty::FieldDef),
    Method(crate::ty::MethodSig),
}

/// 成员命中：成员本身 + 它被声明在哪个类型上（可见性判定用）。
pub(crate) struct MemberHit {
    pub(crate) info: MemberInfo,
    /// 声明该成员的类型键（继承链上可能不是查询起点）
    pub(crate) owner: (ModuleId, String),
}

impl MemberHit {
    /// 引用只覆盖末尾成员名；定义来自实际命中的继承声明。
    fn record(&self, sema: &mut Sema, ctx: &BodyCtx, name: &str, span: Span) {
        let (definition_span, kind) = match &self.info {
            MemberInfo::Field(field) => (field.name_span, crate::DefinitionKind::Variable),
            MemberInfo::Method(method) => (method.name_span, crate::DefinitionKind::Function),
        };
        let reference = Span {
            start: span.end.saturating_sub(name.len()),
            end: span.end,
        };
        sema.references.insert(
            (ctx.module, reference),
            crate::Definition {
                name: name.to_owned(),
                module: self.owner.0,
                span: definition_span,
                kind,
            },
        );
    }
    /// 成员可见性检查（D02）。
    ///
    /// 规则：`pub` 成员任意模块可访问；非 `pub` 成员仅当访问点位于
    /// **声明类型内部**（`ctx.this_ty` 是声明类型或其子类）或与声明同模块时可见。
    pub(crate) fn check_visible(&self, sema: &mut Sema, ctx: &BodyCtx, name: &str, span: Span) {
        let (is_public, module) = match &self.info {
            MemberInfo::Field(field) => (field.is_public, field.module),
            MemberInfo::Method(method) => (method.is_public, method.sig.module),
        };
        if is_public || module == ctx.module {
            return;
        }
        // 声明类型内部（含子类方法体）视为可见：方法是声明类型的子类即可
        if let Some(this_ty) = &ctx.this_ty {
            let owner_ty = Ty::Class(self.owner.0, self.owner.1.clone());
            if sema.is_subclass(&owner_ty, this_ty) {
                return;
            }
        }
        sema.type_error(
            ctx.module,
            span,
            format!("成员 `{name}` 不是 `pub`，无法从类型外部访问"),
        );
    }
}
/// 读取可调用成员前完成返回类型推导，保留纯查询入口供继承校验和左值提示使用。
pub(crate) fn lookup_typed_member_chain(
    sema: &mut Sema,
    key: &(ModuleId, String),
    name: &str,
    static_only: bool,
) -> Option<MemberHit> {
    let hit = lookup_member_chain(sema, key, name, static_only)?;
    if let MemberInfo::Method(method) = &hit.info
        && let Some(signature_name) = &method.sig.name
        && crate::check::returns::resolve(sema, method.sig.module, signature_name)
    {
        return lookup_member_chain(sema, key, name, static_only);
    }
    Some(hit)
}

/// 沿 class 继承链查找成员（struct 只看自身；static_only 时只看静态方法）。
pub(crate) fn lookup_member_chain(
    sema: &Sema,
    key: &(ModuleId, String),
    name: &str,
    _static_only: bool,
) -> Option<MemberHit> {
    let mut current = key.clone();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return None; // 继承环（注册阶段已报过诊断）
        }
        match sema.type_defs.get(&current)? {
            TypeDef::Trait(def) => {
                return def.methods.get(name).map(|method| MemberHit {
                    info: MemberInfo::Method(method.clone()),
                    owner: current,
                });
            }
            TypeDef::Struct(def) => {
                if let Some(field) = def.fields.iter().find(|f| f.name == name) {
                    return Some(MemberHit {
                        info: MemberInfo::Field(field.clone()),
                        owner: current,
                    });
                }
                if let Some(method) = def.methods.get(name) {
                    return Some(MemberHit {
                        info: MemberInfo::Method(method.clone()),
                        owner: current,
                    });
                }
                return None; // struct 无继承
            }
            TypeDef::Class(def) => {
                if let Some(field) = def.fields.iter().find(|f| f.name == name) {
                    return Some(MemberHit {
                        info: MemberInfo::Field(field.clone()),
                        owner: current.clone(),
                    });
                }
                if let Some(method) = def.methods.get(name) {
                    return Some(MemberHit {
                        info: MemberInfo::Method(method.clone()),
                        owner: current.clone(),
                    });
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

/// D02：静态成员不得经实例访问（`a.make()` / `a.count` 均非法）。
///
/// 反向规则（实例成员经类型名访问）由 [`check_static_member`] 负责；两条合起来
/// 保证「成员的种类看声明处，不看调用点写法」。
fn check_static_access(
    sema: &mut Sema,
    ctx: &BodyCtx,
    member: &impl StaticMarked,
    name: &str,
    span: Span,
) {
    if member.is_static() {
        sema.type_error(
            ctx.module,
            span,
            format!("`{name}` 是静态成员，不能通过实例访问，请改用类型名"),
        );
    }
}

/// 静态成员标记（字段与方法共用）。
trait StaticMarked {
    fn is_static(&self) -> bool;
}

impl StaticMarked for crate::ty::FieldDef {
    fn is_static(&self) -> bool {
        self.is_static
    }
}

impl StaticMarked for crate::ty::MethodSig {
    fn is_static(&self) -> bool {
        self.is_static
    }
}

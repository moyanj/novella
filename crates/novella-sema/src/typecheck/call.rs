//! 调用相关检查：索引、函数 / 方法调用、`new` 构造、`match`、lambda。

use std::collections::HashMap;
use std::rc::Rc;

use novella_ast::expr::{Expression, Literal, UnaryOp};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::symbol::SymbolKind;
use crate::ty::{FunctionSig, Ty, TypeDef};

use super::body::{BodyCtx, concrete};
use super::expr::{IdentTarget, check_expr, check_expr_with_hint, classify_identifier};
use super::unsafe_rules::require_unsafe;

/// 索引访问 `a[i]`：数组、指针（unsafe）及零基字面量下标的元组。
pub(crate) fn check_index_access(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    index: &Expression,
    span: Span,
) -> Ty {
    let target_ty = check_expr(sema, ctx, target);
    check_index_on(sema, ctx, target_ty, index, span)
}

pub(crate) fn is_custom_index(sema: &Sema, ty: &Ty) -> bool {
    matches!(ty, Ty::Class(..) | Ty::Struct(..) | Ty::Applied(..)) && !sema.is_string_type(ty)
}

pub(crate) fn index_parts(expr: &Expression) -> Option<(&Expression, &Expression)> {
    match expr {
        Expression::Paren { expr, .. } => index_parts(expr),
        Expression::IndexAccess { target, index, .. } => Some((target, index)),
        _ => None,
    }
}

pub(crate) fn index_signature(
    sema: &Sema,
    target: &Ty,
    name: &str,
) -> Option<(super::member::MemberHit, Rc<FunctionSig>)> {
    let key = super::member::type_key_of(target);
    let hit = super::member::lookup_member_chain(sema, &key, name, false)?;
    let super::member::MemberInfo::Method(method) = &hit.info else {
        return None;
    };
    let subst = crate::ty::member_substitution(sema, target, &hit.owner);
    let Ty::Function(sig) =
        crate::ty::substitute_type_params(&Ty::Function(Rc::new(method.sig.clone())), &subst)
    else {
        return None;
    };
    Some((hit, sig))
}

pub(crate) fn check_index_signature(
    sema: &mut Sema,
    ctx: &BodyCtx,
    target: &Ty,
    name: &str,
    span: Span,
) -> Option<Rc<FunctionSig>> {
    let Some((hit, sig)) = index_signature(sema, target, name) else {
        sema.type_error(ctx.module, span, format!("类型 `{target}` 未实现 `{name}`"));
        return None;
    };
    hit.check_visible(sema, ctx, name, span);
    Some(sig)
}

pub(crate) fn check_index_operand(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    index: &Expression,
    expected: &Ty,
) {
    let actual = check_expr_with_hint(sema, ctx, index, expected);
    if !sema.is_assignable(expected, &actual) {
        sema.type_error(
            ctx.module,
            index.span(),
            format!("下标需要 `{expected}`，实为 `{actual}`"),
        );
    }
}

pub(crate) fn check_index_on(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target_ty: Ty,
    index: &Expression,
    span: Span,
) -> Ty {
    if is_custom_index(sema, &target_ty) {
        if let Some(sig) = check_index_signature(sema, ctx, &target_ty, "__getitem", span)
            && let Some(param) = sig.params.get(1)
        {
            check_index_operand(sema, ctx, index, &param.ty);
            return sig.ret.clone();
        }
        check_expr(sema, ctx, index);
        return Ty::Error;
    }
    let index_ty = check_expr(sema, ctx, index);
    if !index_ty.is_integer() && !index_ty.is_opaque() {
        sema.type_error(
            ctx.module,
            index.span(),
            format!("索引必须是整数，实为 `{index_ty}`"),
        );
        return Ty::Error;
    }
    if sema.is_string_type(&target_ty) {
        return Ty::Int(crate::ty::IntKind::U16);
    }
    match target_ty {
        Ty::Array(element, _) => *element,
        Ty::Pointer(inner) => {
            // S4：指针下标须 unsafe
            require_unsafe(sema, ctx, span, "指针下标访问");
            *inner
        }
        Ty::Tuple(elements) => match index_ty {
            Ty::IntLiteral(negative, value) if signed_literal(index).is_some() => {
                if let Some(element) = tuple_element(&elements, negative, value) {
                    return element.clone();
                }
                let sign = if negative && value != 0 { "-" } else { "" };
                sema.type_error(
                    ctx.module,
                    index.span(),
                    format!(
                        "元组下标 `{sign}{value}` 越界，元组长度为 {}",
                        elements.len()
                    ),
                );
                Ty::Error
            }
            Ty::Error => Ty::Error,
            _ => {
                sema.type_error(ctx.module, index.span(), "元组下标必须是整数字面量");
                Ty::Error
            }
        },
        ty if ty.is_opaque() => Ty::Unknown,
        ty => {
            sema.type_error(ctx.module, span, format!("`{ty}` 不支持下标访问"));
            Ty::Unknown
        }
    }
}

fn tuple_element(elements: &[Ty], negative: bool, value: u128) -> Option<&Ty> {
    if negative && value != 0 {
        return None;
    }
    elements.get(usize::try_from(value).ok()?)
}

/// 普通赋值先检查右侧，因此只按下标语法查询元组槽位，不读取左侧表达式。
pub(super) fn tuple_element_hint<'a>(elements: &'a [Ty], index: &Expression) -> Option<&'a Ty> {
    let (negative, raw) = signed_literal(index)?;
    let Ty::IntLiteral(_, value) = crate::ty::classify_number_literal(raw)? else {
        return None;
    };
    tuple_element(elements, negative, value)
}

fn signed_literal(mut index: &Expression) -> Option<(bool, &str)> {
    let mut negative = false;
    loop {
        match index {
            Expression::Paren { expr, .. } => index = expr,
            Expression::Unary {
                op: op @ (UnaryOp::Plus | UnaryOp::Minus),
                operand,
                ..
            } => {
                negative ^= *op == UnaryOp::Minus;
                index = operand;
            }
            Expression::Literal {
                value: Literal::Number(raw),
                ..
            } => return Some((negative, raw)),
            _ => return None,
        }
    }
}

/// 函数调用：签名检查（个数/默认值/逐参兼容）+ E7 extern 调用须 unsafe + 泛型实参 arity。
pub(crate) fn check_call(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    callee: &Expression,
    type_args: &[TypeExpr],
    args: &[Expression],
    span: Span,
) -> Ty {
    // `Type(...)`：struct 无构造器 / class 用 new
    if let Expression::Identifier { name, .. } = callee {
        if matches!(
            classify_identifier(sema, ctx, name),
            Some(IdentTarget::Type)
        ) {
            for arg in args {
                check_expr(sema, ctx, arg);
            }
            sema.type_error(
                ctx.module,
                span,
                format!("类型 `{name}` 不能直接调用：class 请用 `new {name}(...)`，struct 请用 `{name}{{ ... }}`"),
            );
            return Ty::Unknown;
        }
        if matches!(
            classify_identifier(sema, ctx, name),
            Some(IdentTarget::Namespace(_))
        ) {
            for arg in args {
                check_expr(sema, ctx, arg);
            }
            sema.type_error(ctx.module, span, format!("命名空间 `{name}` 不可调用"));
            return Ty::Unknown;
        }
    }
    let callee_ty = check_expr(sema, ctx, callee);
    // 先取被调签名，按形参类型下推实参（`takes([])` 里的空数组靠形参定型）；
    // 签名未知（不透明 / 非函数）时按无上下文检查，诊断由各自分支给出。
    let type_arg_tys: Vec<Ty> = type_args
        .iter()
        .map(|t| sema.resolve_type(ctx.module, ctx.scope, t, &ctx.type_env))
        .collect();
    let explicit: HashMap<String, Ty> = match &callee_ty {
        Ty::Function(sig) => sig
            .type_params
            .iter()
            .cloned()
            .zip(type_arg_tys.iter().cloned())
            .collect(),
        _ => HashMap::new(),
    };
    let param_tys = match &callee_ty {
        Ty::Function(sig) => Some(
            instance_param_tys(sig)
                .iter()
                .map(|ty| sema.instantiate_type(ty, &explicit))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };
    let arg_tys: Vec<Ty> = args
        .iter()
        .enumerate()
        .map(
            |(index, arg)| match param_tys.as_ref().and_then(|tys| tys.get(index)) {
                Some(param_ty) => check_expr_with_hint(sema, ctx, arg, param_ty),
                None => check_expr(sema, ctx, arg),
            },
        )
        .collect();
    match &callee_ty {
        Ty::Function(sig) => {
            let subst = build_substitution(
                sema,
                ctx,
                sig,
                &type_arg_tys,
                &instance_param_tys(sig),
                &arg_tys,
                span,
            );
            // 方法约束可能同时引用属主与方法本身的形参。
            let (mut bound_subst, _) = sema.call_owner_substitution(ctx.module, callee, sig);
            bound_subst.extend(subst.iter().map(|(name, ty)| (name.clone(), ty.clone())));
            for param in &sig.type_params {
                if !subst.contains_key(param) {
                    sema.type_error(
                        ctx.module,
                        span,
                        format!("无法推导泛型参数 `{param}`，请显式提供类型实参"),
                    );
                }
                if let (Some(bounds), Some(actual)) =
                    (sema.type_param_bounds.get(param).cloned(), subst.get(param))
                {
                    for bound in bounds {
                        let bound = sema.instantiate_type(&bound, &bound_subst);
                        if !sema.satisfies_bound(&bound, actual) {
                            sema.type_error(
                                ctx.module,
                                span,
                                format!("泛型实参 `{actual}` 不满足约束 `{bound}`"),
                            );
                        }
                    }
                }
            }
            let instantiated = sema.instantiate_type(&Ty::Function(sig.clone()), &subst);
            let Ty::Function(instantiated) = instantiated else {
                unreachable!()
            };
            sema.record_call_instance(ctx.module, span, callee, &instantiated, &subst);
            check_call_against_sig(
                sema,
                ctx,
                &instantiated,
                &type_arg_tys,
                args,
                &arg_tys,
                span,
            );
            let ret = instantiated.ret.clone();
            if sig.is_async {
                Ty::Future(Box::new(ret))
            } else {
                ret
            }
        }
        ty if ty.is_opaque() => Ty::Unknown,
        ty => {
            sema.type_error(ctx.module, span, format!("`{ty}` 不可调用"));
            Ty::Unknown
        }
    }
}
/// D05：按「显式实参优先、缺失则从实参推导」建立泛型替换表。
///
/// - 显式给了实参：按声明顺序 `T1 → 实参1`，个数已在调用点校验；
/// - 没给：逐个形参从对应实参类型推导（`infer_type_param`），冲突即报错；
/// - 推导不出的形参不进替换表，签名中的 `T` 保持 [`Ty::TypeParam`]（沿用浅泛型容错）。
#[allow(clippy::too_many_arguments)]
fn build_substitution(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    sig: &FunctionSig,
    type_arg_tys: &[Ty],
    param_tys: &[Ty],
    arg_tys: &[Ty],
    span: Span,
) -> HashMap<String, Ty> {
    let mut subst: HashMap<String, Ty> = HashMap::new();
    if !type_arg_tys.is_empty() {
        for (name, arg) in sig.type_params.iter().zip(type_arg_tys.iter()) {
            subst.insert(name.clone(), arg.clone());
        }
        return subst;
    }
    for (param, actual) in param_tys.iter().zip(arg_tys.iter()) {
        let actual = match (param, actual) {
            (Ty::Pointer(_), Ty::Array(element, _)) => Ty::Pointer(Box::new(concrete(element))),
            _ => concrete(actual),
        };
        if let Err(message) = crate::ty::infer_type_param(param, &actual, &mut subst) {
            sema.type_error(ctx.module, span, message);
        }
    }
    subst
}

/// 用签名检查一次调用（check_call 与 check_new 共用）。
pub(crate) fn check_call_against_sig(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    sig: &FunctionSig,
    type_arg_tys: &[Ty],
    args: &[Expression],
    arg_tys: &[Ty],
    span: Span,
) {
    // 泛型实参 arity：给了就必须与形参个数一致；没给则浅泛型放行
    if !type_arg_tys.is_empty() && type_arg_tys.len() != sig.type_param_count {
        sema.type_error(
            ctx.module,
            span,
            format!(
                "泛型实参个数不匹配：需要 {} 个，给了 {} 个",
                sig.type_param_count,
                type_arg_tys.len()
            ),
        );
    }
    // E7：extern 调用必须在 unsafe 内
    if sig.is_extern {
        require_unsafe(sema, ctx, span, "调用 extern 函数");
    }
    // 实例接收者由成员访问绑定，不属于调用点显式实参。
    let offset = usize::from(sig.is_method && sig.params.first().is_some_and(|p| p.name == "self"));
    let params = &sig.params[offset..];
    let required = params.iter().filter(|p| !p.has_default).count();
    if arg_tys.len() < required
        || arg_tys.len() > params.len()
        || params
            .iter()
            .skip(arg_tys.len())
            .any(|param| !param.has_default)
    {
        sema.type_error(
            ctx.module,
            span,
            format!(
                "实参个数不匹配：需要 {}–{} 个，给了 {} 个",
                required,
                params.len(),
                arg_tys.len()
            ),
        );
        return;
    }
    for (arg, (param, arg_ty)) in args.iter().zip(params.iter().zip(arg_tys.iter())) {
        let array_decay = matches!((&param.ty, arg_ty), (Ty::Pointer(pointee), Ty::Array(element, _))
            if crate::ty::ty_eq(pointee, element));
        if array_decay {
            if readonly_decay_source(sema, ctx, arg) {
                sema.type_error(ctx.module, arg.span(), "只读数组不能退化为可写指针");
            }
        } else if !sema.is_assignable(&param.ty, arg_ty)
            && !sema.record_dyn_conversion(ctx.module, arg.span(), &param.ty, arg_ty)
        {
            sema.type_error(
                ctx.module,
                arg.span(),
                sema.assign_mismatch_reason(&param.ty, arg_ty)
                    .unwrap_or_else(|| {
                        format!("参数 `{}` 需要 `{}`，实为 `{arg_ty}`", param.name, param.ty)
                    }),
            );
        }
    }
}

fn readonly_decay_source(sema: &mut Sema, ctx: &mut BodyCtx, source: &Expression) -> bool {
    match source {
        Expression::Paren { expr, .. } => readonly_decay_source(sema, ctx, expr),
        Expression::Identifier { name, .. } => matches!(
            sema.scopes
                .lookup(ctx.scope, name)
                .map(|symbol| &symbol.kind),
            Some(SymbolKind::Variable { is_const: true, .. })
        ),
        Expression::MemberAccess { target, .. } => readonly_decay_source(sema, ctx, target),
        Expression::IndexAccess { target, .. } => {
            let is_array = matches!(
                sema.expression_types.get(&(ctx.module, target.span())),
                Some(Ty::Array(..))
            );
            is_array && readonly_decay_source(sema, ctx, target)
        }
        _ => false,
    }
}

/// `new Class(args)`：必须是 class，按 `__init` 检查实参。
pub(crate) fn check_new(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    type_args: &[TypeExpr],
    args: &[Expression],
    span: Span,
) -> Ty {
    fn path(expr: &Expression, parts: &mut Vec<String>) -> bool {
        match expr {
            Expression::Identifier { name, .. } => {
                parts.push(name.clone());
                true
            }
            Expression::MemberAccess { target, name, .. } => {
                if !path(target, parts) {
                    return false;
                }
                parts.push(name.clone());
                true
            }
            _ => false,
        }
    }
    let mut parts = Vec::new();
    if !path(target, &mut parts) {
        check_expr(sema, ctx, target);
        for arg in args {
            check_expr(sema, ctx, arg);
        }
        sema.type_error(ctx.module, span, "new 的目标必须是类的类型路径");
        return Ty::Error;
    }
    let name = parts.pop().expect("非空类型路径");
    let resolved = sema.resolve_type(
        ctx.module,
        ctx.scope,
        &TypeExpr::Reference {
            qualifier: parts,
            name: name.clone(),
            generic_args: type_args.to_vec(),
            span: target.span(),
        },
        &ctx.type_env,
    );
    let (base, type_arg_tys) = match &resolved {
        Ty::Applied(base, args) => (&**base, args.clone()),
        other => (other, Vec::new()),
    };
    let type_key = match base {
        Ty::Class(module, name) | Ty::Struct(module, name) => Some((*module, name.clone())),
        _ => None,
    };
    let unresolved_new = resolved.is_opaque();
    let class_subst: HashMap<String, Ty> = type_key
        .as_ref()
        .and_then(|key| sema.type_defs.get(key))
        .map(|def| {
            def.type_params()
                .iter()
                .cloned()
                .zip(type_arg_tys.iter().cloned())
                .collect()
        })
        .unwrap_or_default();
    let init_params = type_key
        .as_ref()
        .and_then(|key| match sema.type_defs.get(key) {
            Some(TypeDef::Class(def)) => def.methods.get("__init").map(|method| {
                instance_param_tys(&method.sig)
                    .iter()
                    .map(|ty| crate::ty::substitute_type_params(ty, &class_subst))
                    .collect::<Vec<_>>()
            }),
            _ => None,
        });
    let arg_tys: Vec<Ty> = args
        .iter()
        .enumerate()
        .map(
            |(index, arg)| match init_params.as_ref().and_then(|tys| tys.get(index)) {
                Some(param_ty) => check_expr_with_hint(sema, ctx, arg, param_ty),
                None => check_expr(sema, ctx, arg),
            },
        )
        .collect();
    let Some(type_key) = type_key else {
        // 未定义类型已有诊断：返回 Error 阻止上层重复报错
        return if unresolved_new {
            Ty::Error
        } else {
            Ty::Unknown
        };
    };
    match sema.type_defs.get(&type_key) {
        Some(TypeDef::Class(def)) => {
            let def = def.clone();
            // 泛型类浅检查
            if type_arg_tys.len() != def.type_param_count {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "泛型实参个数不匹配：需要 {} 个，给了 {} 个",
                        def.type_param_count,
                        type_arg_tys.len()
                    ),
                );
            }
            // D02：含未实现抽象方法的类（抽象类）不可实例化
            if crate::check::is_abstract_class(sema, &type_key) {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("class `{name}` 是抽象类（含未实现的抽象方法），不能 `new`"),
                );
            }
            // 按 __init 检查实参；未定义 __init 时要求零实参
            match def.methods.get("__init") {
                Some(init) => {
                    let ty = crate::ty::substitute_type_params(
                        &Ty::Function(Rc::new(init.sig.clone())),
                        &class_subst,
                    );
                    let Ty::Function(init_sig) = ty else {
                        unreachable!()
                    };
                    check_call_against_sig(sema, ctx, &init_sig, &[], args, &arg_tys, span);
                }
                None => {
                    if !args.is_empty() {
                        sema.type_error(
                            ctx.module,
                            span,
                            format!("class `{name}` 没有 `__init`，`new` 不能传实参"),
                        );
                    }
                }
            }
            let base = Ty::Class(type_key.0, type_key.1);
            if type_arg_tys.is_empty() {
                base
            } else {
                let ty = Ty::Applied(Box::new(base), type_arg_tys);
                sema.generic_uses.insert((ctx.module, span), ty.clone());
                ty
            }
        }
        Some(TypeDef::Struct(_)) => {
            sema.type_error(
                ctx.module,
                span,
                format!("struct `{name}` 不能用 `new` 构造，请用 `{name}{{ ... }}`"),
            );
            Ty::Error
        }
        Some(_) => {
            sema.type_error(
                ctx.module,
                span,
                format!("`{name}` 不是 class，不能用 `new` 构造"),
            );
            Ty::Error
        }
        None if crate::ty::ty_eq(&resolved, &crate::ty::exception_type()) => {
            if !args.is_empty() {
                sema.type_error(ctx.module, span, "Exception 根构造器不接受实参");
            }
            resolved
        }
        None => Ty::Error, // 注册阶段已报过
    }
}
pub(crate) use super::lambda::check_lambda;

/// 取一次调用可见的形参类型（实例方法跳过 index 0 的 `self`，与
/// [`crate::ty::FunctionSig::view`] 的接收者规则一致）。
fn instance_param_tys(sig: &FunctionSig) -> Vec<Ty> {
    let skip_self = sig.is_method && sig.params.first().is_some_and(|p| p.name == "self");
    sig.params
        .iter()
        .skip(usize::from(skip_self))
        .map(|param| param.ty.clone())
        .collect()
}

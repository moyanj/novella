//! 调用相关检查：索引、函数 / 方法调用、`new` 构造、`match`、lambda。

use std::collections::HashSet;
use std::rc::Rc;

use novella_ast::expr::{Expression, LambdaBody, MatchCaseBody, MatchPattern};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::symbol::{ScopeKind, SymbolKind};
use crate::ty::{FunctionSig, ParamSig, Ty, TypeDef};

use super::body::{BodyCtx, check_block, concrete, define_local, fork_scope};
use super::expr::{IdentTarget, check_expr, classify_identifier, type_key_of_name};
use super::unsafe_rules::{comparable, require_unsafe, unify_types};

/// 索引访问 `a[i]`：数组 / 指针（S4，unsafe）/ 元组（需常量索引，暂不支持）。
pub(crate) fn check_index_access(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    index: &Expression,
    span: Span,
) -> Ty {
    let target_ty = check_expr(sema, ctx, target);
    let index_ty = check_expr(sema, ctx, index);
    if !index_ty.is_integer() && !index_ty.is_opaque() {
        sema.type_error(
            ctx.module,
            index.span(),
            format!("索引必须是整数，实为 `{index_ty}`"),
        );
    }
    match target_ty {
        Ty::Array(element) => *element,
        Ty::Pointer(inner) => {
            // S4：指针下标须 unsafe
            require_unsafe(sema, ctx, span, "指针下标访问");
            *inner
        }
        Ty::Tuple(_) => {
            sema.type_error(
                ctx.module,
                span,
                "元组索引需要常量下标，本期暂不支持（请用解构）",
            );
            Ty::Unknown
        }
        ty if ty.is_opaque() => Ty::Unknown,
        ty => {
            sema.type_error(ctx.module, span, format!("`{ty}` 不支持下标访问"));
            Ty::Unknown
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
    let arg_tys: Vec<Ty> = args.iter().map(|a| check_expr(sema, ctx, a)).collect();
    let type_arg_tys: Vec<Ty> = type_args
        .iter()
        .map(|t| sema.resolve_type(ctx.module, ctx.scope, t, &ctx.type_env))
        .collect();
    match &callee_ty {
        Ty::Function(sig) => {
            check_call_against_sig(sema, ctx, sig, &type_arg_tys, args, &arg_tys, span);
            sig.ret.clone()
        }
        Ty::External => {
            // std 外部符号：参数照常检查（上面已做），返回类型未知
            Ty::Unknown
        }
        ty if ty.is_opaque() => Ty::Unknown,
        ty => {
            sema.type_error(ctx.module, span, format!("`{ty}` 不可调用"));
            Ty::Unknown
        }
    }
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
    let required = sig.params.iter().filter(|p| !p.has_default).count();
    if arg_tys.len() < required || arg_tys.len() > sig.params.len() {
        sema.type_error(
            ctx.module,
            span,
            format!(
                "实参个数不匹配：需要 {}–{} 个，给了 {} 个",
                required,
                sig.params.len(),
                arg_tys.len()
            ),
        );
        return;
    }
    for (arg, (param, arg_ty)) in args.iter().zip(sig.params.iter().zip(arg_tys.iter())) {
        if !sema.is_assignable(&param.ty, arg_ty) {
            sema.type_error(
                ctx.module,
                arg.span(),
                format!("参数 `{}` 需要 `{}`，实为 `{arg_ty}`", param.name, param.ty),
            );
        }
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
    // 目标通常是 Identifier（也可能是成员路径 Type 访问，本期只支持标识符）
    let Expression::Identifier {
        name,
        span: name_span,
    } = target
    else {
        check_expr(sema, ctx, target);
        for arg in args {
            check_expr(sema, ctx, arg);
        }
        sema.type_error(ctx.module, span, "`new` 的目标目前只支持类名标识符");
        return Ty::Unknown;
    };
    let type_key = match type_key_of_name(sema, ctx.scope, name) {
        Some(key) => {
            if let Some(symbol) = sema.scopes.lookup(ctx.scope, name).cloned() {
                sema.record_reference(ctx.module, *name_span, &symbol);
            }
            Some(key)
        }
        None if sema.scopes.lookup(ctx.scope, name).is_some() => {
            sema.type_error(
                ctx.module,
                *name_span,
                format!("`{name}` 不是类型，不能用 `new` 构造"),
            );
            None
        }
        None => {
            sema.type_error(ctx.module, *name_span, format!("未定义的类型 `{name}`"));
            None
        }
    };
    let unresolved_new = type_key.is_none() && sema.scopes.lookup(ctx.scope, name).is_none();
    let arg_tys: Vec<Ty> = args.iter().map(|a| check_expr(sema, ctx, a)).collect();
    let type_arg_tys: Vec<Ty> = type_args
        .iter()
        .map(|t| sema.resolve_type(ctx.module, ctx.scope, t, &ctx.type_env))
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
            if !type_arg_tys.is_empty() && type_arg_tys.len() != def.type_param_count {
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
            // 按 __init 检查实参；未定义 __init 时要求零实参
            match def.methods.get("__init") {
                Some(init) => {
                    let init_sig = init.sig.clone();
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
            Ty::Class(type_key.0, type_key.1)
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
        None => Ty::Unknown, // 注册阶段已报过
    }
}
/// match 表达式：case 模式与 subject 的兼容性 + 分支结果类型合一。
pub(crate) fn check_match(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    subject: &Expression,
    cases: &[novella_ast::expr::MatchCase],
    default: Option<&MatchCaseBody>,
) -> Ty {
    let subject_ty = check_expr(sema, ctx, subject);
    let mut result_ty = Ty::Unknown;
    let mut merge = |sema: &mut Sema, ctx: &mut BodyCtx, body: &MatchCaseBody| match body {
        MatchCaseBody::Expr { expr, .. } => {
            let ty = check_expr(sema, ctx, expr);
            if result_ty.is_opaque() {
                result_ty = ty;
            } else if let Some(unified) = unify_types(&result_ty, &ty) {
                result_ty = unified;
            } else if !ty.is_opaque() {
                sema.type_error(
                    ctx.module,
                    expr.span(),
                    format!("match 分支类型 `{ty}` 与先前分支 `{result_ty}` 不一致"),
                );
            }
        }
        MatchCaseBody::Block { block, .. } => {
            check_block(sema, ctx, block);
        }
    };
    for case in cases {
        match &case.pattern {
            MatchPattern::Expr { expr, .. } => {
                let pattern_ty = check_expr(sema, ctx, expr);
                if !comparable(&subject_ty, &pattern_ty) {
                    sema.type_error(
                        ctx.module,
                        expr.span(),
                        format!("case 值 `{pattern_ty}` 与 match 目标 `{subject_ty}` 不可比较"),
                    );
                }
            }
            MatchPattern::Type { ty, .. } => {
                sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
            }
        }
        merge(sema, ctx, &case.body);
    }
    if let Some(body) = default {
        merge(sema, ctx, body);
    }
    result_ty
}
/// lambda：新作用域 + 形参绑定 + 返回类型（标注或推导）。
pub(crate) fn check_lambda(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    lambda: &novella_ast::expr::Lambda,
) -> Ty {
    let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Function);
    let mut lambda_ctx = fork_scope(ctx, scope);
    lambda_ctx.in_async = lambda.is_async;
    lambda_ctx.in_loop = false;
    lambda_ctx.this_ty = None; // lambda 不捕获 this（箭头函数语义属后续期）
    let mut param_sigs = Vec::new();
    match &lambda.params {
        novella_ast::expr::LambdaParams::Single { name, span } => {
            // 无标注单参数：类型 Unknown（参数推导属后续期）
            define_local(
                sema,
                &lambda_ctx,
                name,
                SymbolKind::Param { ty: Ty::Unknown },
                *span,
            );
            param_sigs.push(ParamSig {
                name: name.clone(),
                ty: Ty::Unknown,
                has_default: false,
            });
        }
        novella_ast::expr::LambdaParams::List { params, .. } => {
            let mut seen = HashSet::new();
            for param in params {
                if !seen.insert(param.name.clone()) {
                    sema.type_error(
                        ctx.module,
                        param.span,
                        format!("lambda 形参 `{}` 重复", param.name),
                    );
                }
                let ty = param
                    .ty
                    .as_ref()
                    .map(|t| sema.resolve_type(ctx.module, scope, t, &ctx.type_env))
                    .unwrap_or(Ty::Unknown);
                define_local(
                    sema,
                    &lambda_ctx,
                    &param.name,
                    SymbolKind::Param { ty: ty.clone() },
                    param.span,
                );
                param_sigs.push(ParamSig {
                    name: param.name.clone(),
                    ty,
                    has_default: param.default.is_some(),
                });
            }
        }
    }
    let annotated_ret = lambda
        .return_type
        .as_ref()
        .map(|t| sema.resolve_type(ctx.module, scope, t, &ctx.type_env));
    let ret_ty = match &lambda.body {
        LambdaBody::Expr { expr, .. } => {
            // 表达式体：返回类型 = 表达式类型（标注时校验兼容）
            let body_ty = check_expr(sema, &mut lambda_ctx, expr);
            match &annotated_ret {
                Some(annotated) => {
                    if !sema.is_assignable(annotated, &body_ty) {
                        sema.type_error(
                            ctx.module,
                            expr.span(),
                            format!("lambda 体类型 `{body_ty}` 与返回标注 `{annotated}` 不兼容"),
                        );
                    }
                    annotated.clone()
                }
                None => concrete(&body_ty),
            }
        }
        LambdaBody::Block { block, .. } => {
            lambda_ctx.expected_return = annotated_ret.clone().or(Some(Ty::Unknown));
            check_block(sema, &mut lambda_ctx, block);
            annotated_ret.clone().unwrap_or(Ty::Unknown)
        }
    };
    Ty::Function(Rc::new(FunctionSig {
        params: param_sigs,
        ret: ret_ty,
        type_param_count: 0,
        is_async: lambda.is_async,
        is_extern: false,
    }))
}

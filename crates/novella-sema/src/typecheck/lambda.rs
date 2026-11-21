//! lambda 检查与块体返回类型合并。
use super::body::{BodyCtx, check_block, concrete, define_local, fork_scope};
use super::expr::{check_expr, check_expr_with_hint};
use crate::{
    Sema,
    symbol::{ScopeKind, SymbolKind},
    ty::{FunctionSig, ParamSig, Ty},
};
use novella_ast::{expr::LambdaBody, stmt::Block};
use std::collections::HashSet;
use std::rc::Rc;

/// lambda：新作用域 + 形参绑定 + 返回类型（标注或推导）。
pub(crate) fn check_lambda(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    lambda: &novella_ast::expr::Lambda,
) -> Ty {
    let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Function);
    let mut lambda_ctx = fork_scope(ctx, scope);
    lambda_ctx.destructor_capture = ctx.destructor_self.is_some();
    lambda_ctx.in_async = lambda.is_async;
    lambda_ctx.in_loop = false;
    lambda_ctx.loop_assignments = None;
    lambda_ctx.return_states = None;
    lambda_ctx.exception_states = None;
    lambda_ctx.reachable = true;
    lambda_ctx.in_catch = false;
    lambda_ctx.in_unsafe = false;
    lambda_ctx.inferred_returns = None;
    // 箭头函数继承词法 this；运行时闭包环境由 HIR/后端另行处理。
    let mut param_sigs = Vec::new();
    match &lambda.params {
        novella_ast::expr::LambdaParams::Single { name, span } => {
            if name == "self" {
                sema.decl_error(ctx.module, *span, "lambda 不能声明实例接收者 self");
            }
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
                if param.name == "self" {
                    sema.decl_error(ctx.module, param.span, "lambda 不能声明实例接收者 self");
                }
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
                if let Some(default) = &param.default {
                    let actual = check_expr_with_hint(sema, &mut lambda_ctx, default, &ty);
                    if !sema.is_assignable(&ty, &actual) {
                        sema.type_error(
                            ctx.module,
                            default.span(),
                            format!("lambda 默认值需要 `{ty}`，实为 `{actual}`"),
                        );
                    }
                }
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
            let body_ty = match &annotated_ret {
                Some(ret) => check_expr_with_hint(sema, &mut lambda_ctx, expr, ret),
                None => check_expr(sema, &mut lambda_ctx, expr),
            };
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
            if annotated_ret.is_none() {
                lambda_ctx.inferred_returns = Some(Default::default());
            }
            check_block(sema, &mut lambda_ctx, block);
            if let Some(ret) = &annotated_ret {
                super::flow::check_block(sema, ctx.module, ret, block);
            }
            annotated_ret
                .clone()
                .unwrap_or_else(|| infer_block_return(sema, &lambda_ctx, block))
        }
    };
    Ty::Function(Rc::new(FunctionSig {
        // 匿名 lambda 不在类型注册表内，没有名义键
        module: lambda_ctx.module,
        name: None,
        params: param_sigs,
        ret: ret_ty,
        type_param_count: 0,
        // lambda 不支持泛型形参
        type_params: vec![],
        is_async: lambda.is_async,
        is_extern: false,
        is_method: false,
    }))
}

/// 只合并自身的可达返回；落尾等价于 void，不从调用点反向猜测。
fn infer_block_return(sema: &mut Sema, ctx: &BodyCtx, block: &Block) -> Ty {
    let mut returns = ctx
        .inferred_returns
        .as_ref()
        .expect("lambda 返回收集器")
        .borrow()
        .clone();
    if super::flow::block_falls_through(sema, ctx.module, block) {
        returns.push((block.span, Ty::Void));
    }
    let Some((_, first)) = returns.first() else {
        return Ty::Void;
    };
    let mut result = concrete(first);
    for (span, actual) in &returns {
        let actual = concrete(actual);
        if let Some(joined) = super::unsafe_rules::unify_types(&result, &actual) {
            result = joined;
        } else {
            sema.type_error(
                ctx.module,
                *span,
                format!("lambda 返回类型不一致：{result} 与 {actual}，请统一返回值或显式标注"),
            );
            return Ty::Error;
        }
    }
    result
}

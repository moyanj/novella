//! match 的模式覆盖、结果合一与期望类型下推。

use super::{
    body::{BodyCtx, check_block, fork_scope},
    expr::{check_expr, check_expr_with_hint},
    unsafe_rules::{comparable, unify_types},
};
use crate::{
    Sema,
    ty::{Ty, ty_eq},
};
use novella_ast::expr::{Expression, Literal, Match, MatchCaseBody, MatchPattern};

/// 括号和字面量符号不改变常量身份；调用等动态模式不参与重复常量判定。
fn constant_literal(expr: &Expression) -> Option<&Literal> {
    match expr {
        Expression::Literal { value, .. } => Some(value),
        Expression::Paren { expr, .. } => constant_literal(expr),
        Expression::Unary {
            op: novella_ast::expr::UnaryOp::Plus | novella_ast::expr::UnaryOp::Minus,
            operand,
            ..
        } => constant_literal(operand).filter(|value| matches!(value, Literal::Number(_))),
        _ => None,
    }
}

pub(crate) fn check_match(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    node: &Match,
    hint: Option<&Ty>,
) -> Ty {
    let subject = check_expr(sema, ctx, &node.subject);
    let mut constants = Vec::new();
    let mut types = Vec::new();
    let mut booleans = [false; 2];
    for case in &node.cases {
        match &case.pattern {
            MatchPattern::Expr { expr, .. } => {
                let ty = check_expr(sema, ctx, expr);
                if !comparable(&subject, &ty) {
                    sema.type_error(
                        ctx.module,
                        expr.span(),
                        format!("case 值 `{ty}` 与 match 目标 `{subject}` 不可比较"),
                    );
                }
                if let Some(value) = constant_literal(expr) {
                    let key = match value {
                        Literal::Number(_) => match &ty {
                            Ty::IntLiteral(negative, value) => {
                                Some(format!("i:{}:{value}", *negative && *value != 0))
                            }
                            Ty::FloatLiteral(value) => {
                                Some(format!("f:{}", if *value == 0.0 { 0.0 } else { *value }))
                            }
                            _ => None,
                        },
                        Literal::String(raw) => novella_core::string::decode_string_literal(raw)
                            .ok()
                            .map(|v| format!("s:{v}")),
                        Literal::Boolean(value) => {
                            booleans[usize::from(*value)] = true;
                            Some(format!("b:{value}"))
                        }
                        Literal::Null => Some("null".to_owned()),
                    };
                    if let Some(key) = key {
                        if constants.contains(&key) {
                            sema.type_error(ctx.module, case.span, "重复的 case 常量");
                        }
                        constants.push(key);
                    }
                }
                if types.iter().any(|prior| sema.is_assignable(prior, &ty)) {
                    sema.type_error(ctx.module, case.span, "case 已被先前类型模式覆盖");
                }
            }
            MatchPattern::Type { ty, .. } => {
                let ty = sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
                if types.iter().any(|prior| sema.is_assignable(prior, &ty)) {
                    sema.type_error(ctx.module, case.span, "类型模式已被先前分支覆盖");
                }
                let possible = match &subject {
                    Ty::Union(members) => members
                        .iter()
                        .any(|m| sema.is_assignable(&ty, m) || sema.is_assignable(m, &ty)),
                    other => sema.is_assignable(&ty, other) || sema.is_assignable(other, &ty),
                };
                if !possible {
                    sema.type_error(ctx.module, case.span, "类型模式不可能匹配目标类型");
                }
                types.push(ty);
            }
        }
    }
    let exhaustive = node.default.is_some()
        || matches!(subject, Ty::Boolean) && booleans.iter().all(|b| *b)
        || match &subject {
            Ty::Union(members) => members
                .iter()
                .all(|member| types.iter().any(|t| sema.is_assignable(t, member))),
            other => types.iter().any(|t| ty_eq(t, other)),
        };
    if exhaustive {
        sema.exhaustive_matches.insert((ctx.module, node.span));
    }
    let mut result: Option<Ty> = None;
    let mut has_value = false;
    let mut has_block = false;
    let before = ctx.uninitialized.clone();
    let mut merged = std::collections::HashSet::new();
    for body in node
        .cases
        .iter()
        .map(|c| &c.body)
        .chain(node.default.iter())
    {
        let scope = sema
            .scopes
            .new_scope(Some(ctx.scope), crate::symbol::ScopeKind::Block);
        let mut branch = fork_scope(ctx, scope);
        let ty = match body {
            MatchCaseBody::Expr { expr, .. } => match hint {
                Some(hint) => check_expr_with_hint(sema, &mut branch, expr, hint),
                None => check_expr(sema, &mut branch, expr),
            },
            MatchCaseBody::Block { block, .. } => {
                check_block(sema, &mut branch, block);
                Ty::Void
            }
        };
        if matches!(ty, Ty::Void) {
            has_block = true;
        } else if !ty.is_opaque() {
            has_value = true;
        }
        let reaches_join = match body {
            MatchCaseBody::Expr { .. } => true,
            MatchCaseBody::Block { block, .. } => {
                super::flow::block_falls_through(sema, ctx.module, block)
            }
        };
        if reaches_join {
            merged.extend(branch.uninitialized);
        }
        result = Some(match result {
            None => ty,
            Some(previous) => unify_types(&previous, &ty).unwrap_or_else(|| {
                if hint
                    .is_some_and(|h| sema.is_assignable(h, &previous) && sema.is_assignable(h, &ty))
                {
                    hint.expect("已有期望类型").clone()
                } else {
                    sema.type_error(
                        ctx.module,
                        body.span(),
                        format!("match 分支类型 `{ty}` 与 `{previous}` 不一致"),
                    );
                    Ty::Error
                }
            }),
        });
    }
    if has_value && has_block {
        sema.type_error(ctx.module, node.span, "match 不能混合值分支与 void 块分支");
    }
    if has_value && !exhaustive {
        sema.type_error(
            ctx.module,
            node.span,
            "值 match 必须穷尽目标类型或提供 default",
        );
    }
    if !exhaustive {
        merged.extend(before);
    }
    ctx.uninitialized = merged;
    result.unwrap_or(Ty::Void)
}

//! 返回路径检查：退出集合的顺序组合与分支合并，不把 break 当作返回。

use novella_ast::expr::{AssignOp, BinaryOp, Expression, Literal, MatchCaseBody, MatchPattern};
use novella_ast::stmt::{Block, ForInit, ForKind, IfBranch, Statement, VariableStmt};
use novella_core::Span;

use crate::{Sema, module::ModuleId, ty::Ty};

#[derive(Clone, Copy, Default)]
struct Exits {
    normal: bool,
    returns: bool,
    throws: bool,
    breaks: bool,
    continues: bool,
}

impl Exits {
    fn normal() -> Self {
        Self {
            normal: true,
            ..Self::default()
        }
    }
    fn union(self, other: Self) -> Self {
        Self {
            normal: self.normal || other.normal,
            returns: self.returns || other.returns,
            throws: self.throws || other.throws,
            breaks: self.breaks || other.breaks,
            continues: self.continues || other.continues,
        }
    }
    fn then(mut self, next: Self) -> Self {
        if self.normal {
            self.normal = false;
            self = self.union(next);
        }
        self
    }
}

pub(crate) fn check_statement(sema: &mut Sema, module: ModuleId, ret: &Ty, body: &Statement) {
    check_exit(
        sema,
        module,
        ret,
        body.span(),
        statement(body, &|span| {
            sema.exhaustive_matches.contains(&(module, span))
        }),
    );
}

pub(crate) fn check_block(sema: &mut Sema, module: ModuleId, ret: &Ty, body: &Block) {
    check_exit(
        sema,
        module,
        ret,
        body.span,
        block(body, &|span| {
            sema.exhaustive_matches.contains(&(module, span))
        }),
    );
}

fn check_exit(sema: &mut Sema, module: ModuleId, ret: &Ty, span: Span, exits: Exits) {
    if !matches!(ret, Ty::Void) && !ret.is_opaque() && exits.normal {
        sema.type_error(
            module,
            span,
            format!("返回类型为 `{ret}` 的函数存在正常到达末尾而未返回值的路径"),
        );
    }
}

fn block(body: &Block, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    body.stmts.iter().fold(Exits::normal(), |exits, stmt| {
        exits.then(statement(stmt, exhaustive))
    })
}

pub(super) fn constant_bool(expr: &Expression) -> Option<bool> {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || constant_bool_inner(expr))
}

fn constant_bool_inner(expr: &Expression) -> Option<bool> {
    match expr {
        Expression::Literal {
            value: Literal::Boolean(value),
            ..
        } => Some(*value),
        Expression::Paren { expr, .. } => constant_bool(expr),
        _ => None,
    }
}

fn loop_exits(
    body: &Statement,
    condition: Option<bool>,
    update: Option<&Expression>,
    exhaustive: &dyn Fn(Span) -> bool,
) -> Exits {
    if condition == Some(false) {
        return Exits::normal();
    }
    let inner = statement(body, exhaustive);
    let update = if inner.normal || inner.continues {
        expressions(update, exhaustive)
    } else {
        Exits::default()
    };
    Exits {
        normal: condition != Some(true) || inner.breaks || update.breaks,
        returns: inner.returns || update.returns,
        throws: inner.throws || update.throws,
        ..Exits::default()
    }
}

fn statement(stmt: &Statement, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        statement_inner(stmt, exhaustive)
    })
}

pub(super) fn falls_through(sema: &Sema, module: ModuleId, stmt: &Statement) -> bool {
    statement(stmt, &|span| {
        sema.exhaustive_matches.contains(&(module, span))
    })
    .normal
}

pub(super) fn block_falls_through(sema: &Sema, module: ModuleId, body: &Block) -> bool {
    block(body, &|span| {
        sema.exhaustive_matches.contains(&(module, span))
    })
    .normal
}

pub(super) fn expr_falls_through(sema: &Sema, module: ModuleId, expr: &Expression) -> bool {
    expression(expr, &|span| {
        sema.exhaustive_matches.contains(&(module, span))
    })
    .normal
}

fn declarations(decls: &[VariableStmt], exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    expressions(
        decls.iter().filter_map(|decl| decl.value.as_ref()),
        exhaustive,
    )
}

fn statement_inner(stmt: &Statement, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    match stmt {
        Statement::Return { value, .. } => expressions(value, exhaustive).then(Exits {
            returns: true,
            ..Exits::default()
        }),
        Statement::Throw { expr, .. } => expressions(expr, exhaustive).then(Exits {
            throws: true,
            ..Exits::default()
        }),
        Statement::Break { .. } => Exits {
            breaks: true,
            ..Exits::default()
        },
        Statement::Continue { .. } => Exits {
            continues: true,
            ..Exits::default()
        },
        Statement::Block(body) => block(body, exhaustive),
        Statement::Unsafe { block: body, .. } => block(body, exhaustive),
        Statement::If(value) => {
            let mut exits = Exits::default();
            let mut remaining = true;
            for branch in &value.branches {
                if !remaining {
                    break;
                }
                match branch {
                    IfBranch::ElseIf {
                        condition, block, ..
                    } => {
                        let condition_exits = expression(condition, exhaustive);
                        exits = exits.union(Exits {
                            normal: false,
                            ..condition_exits
                        });
                        if !condition_exits.normal {
                            remaining = false;
                            continue;
                        }
                        if constant_bool(condition) != Some(false) {
                            exits = exits.union(statement(block, exhaustive));
                        }
                        if constant_bool(condition) == Some(true) {
                            remaining = false;
                        }
                    }
                    IfBranch::Else { block, .. } => {
                        exits = exits.union(statement(block, exhaustive));
                        remaining = false;
                    }
                }
            }
            exits.normal |= remaining;
            exits
        }
        Statement::While {
            condition, body, ..
        } => expression(condition, exhaustive).then(loop_exits(
            body,
            constant_bool(condition),
            None,
            exhaustive,
        )),
        Statement::For(value) => match &value.condition {
            ForKind::ForIn(cond) => {
                let iter = expression(&cond.right, exhaustive);
                finish_for_header(iter, loop_exits(&value.block, None, None, exhaustive))
            }
            ForKind::ForLoop(cond) => {
                let init = match &cond.init {
                    Some(ForInit::Declaration {
                        declarations: decls,
                        ..
                    }) => declarations(decls, exhaustive),
                    Some(ForInit::Expression { expr, .. }) => expression(expr, exhaustive),
                    None => Exits::normal(),
                };
                let header = init.then(expressions(&cond.condition, exhaustive));
                finish_for_header(
                    header,
                    loop_exits(
                        &value.block,
                        cond.condition.as_ref().map_or(Some(true), constant_bool),
                        cond.update.as_ref(),
                        exhaustive,
                    ),
                )
            }
        },
        Statement::Try(value) => {
            let mut exits = block(&value.block, exhaustive);
            // 任一执行中的表达式都可能抛出；catch 分支须保守计入可达出口。
            for catch in &value.catches {
                exits = exits.union(block(&catch.block, exhaustive));
            }
            if let Some(finally) = &value.finally {
                let cleanup = block(finally, exhaustive);
                let reachable = exits.normal
                    || exits.returns
                    || exits.throws
                    || exits.breaks
                    || exits.continues;
                exits = if cleanup.normal {
                    exits
                } else {
                    Exits::default()
                };
                if reachable {
                    exits = exits.union(Exits {
                        normal: false,
                        ..cleanup
                    });
                }
            }
            exits
        }
        Statement::With(value) => {
            expression(&value.expr, exhaustive).then(block(&value.block, exhaustive))
        }
        Statement::Expression(expr) => expression(expr, exhaustive),
        Statement::Variable(decl) => expressions(&decl.value, exhaustive),
        Statement::VariableList {
            declarations: decls,
            ..
        } => declarations(decls, exhaustive),
        Statement::Yield { value, .. } => expressions(value, exhaustive),
        Statement::Empty { .. } => Exits::normal(),
    }
}

fn finish_for_header(header: Exits, inner: Exits) -> Exits {
    let mut exits = header.then(inner);
    exits.normal |= header.breaks;
    exits.breaks = false;
    exits.continues = false;
    exits
}

fn expressions<'a>(
    values: impl IntoIterator<Item = &'a Expression>,
    exhaustive: &dyn Fn(Span) -> bool,
) -> Exits {
    let mut exits = Exits::normal();
    for value in values {
        if !exits.normal {
            break;
        }
        exits = exits.then(expression(value, exhaustive));
    }
    exits
}

/// match 的已证明穷尽性属于 sema 事实，返回路径分析与 HIR 共用它。
fn expression(expr: &Expression, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        expression_inner(expr, exhaustive)
    })
}

fn expression_inner(expr: &Expression, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    match expr {
        Expression::Paren { expr, .. }
        | Expression::Await { expr, .. }
        | Expression::Cast { expr, .. } => expression(expr, exhaustive),
        Expression::Unary { operand, .. } | Expression::Is { operand, .. } => {
            expression(operand, exhaustive)
        }
        Expression::MemberAccess { target, .. } => expression(target, exhaustive),
        Expression::Tuple { elements, .. } | Expression::ArrayLiteral { elements, .. } => {
            expressions(elements, exhaustive)
        }
        Expression::ObjectLiteral { properties, .. }
        | Expression::StructInit { properties, .. } => expressions(
            properties
                .iter()
                .filter_map(|property| property.value.as_ref()),
            exhaustive,
        ),
        Expression::Call { callee, args, .. }
        | Expression::New {
            target: callee,
            args,
            ..
        } => expressions(std::iter::once(callee.as_ref()).chain(args), exhaustive),
        Expression::IndexAccess { target, index, .. } => {
            expressions([target.as_ref(), index.as_ref()], exhaustive)
        }
        Expression::Assignment {
            target, op, value, ..
        } => {
            let (first, second) = if *op == AssignOp::Assign {
                (value, target)
            } else {
                (target, value)
            };
            expressions([first.as_ref(), second.as_ref()], exhaustive)
        }
        Expression::Binary {
            left, op, right, ..
        } => {
            let required = match op {
                BinaryOp::LogicalAnd => constant_bool(left),
                BinaryOp::LogicalOr => constant_bool(left).map(|value| !value),
                BinaryOp::NullCoalesce => None,
                _ => Some(true),
            };
            let left = expression(left, exhaustive);
            if required == Some(false) || !left.normal {
                return left;
            }
            let right = expression(right, exhaustive);
            left.then(if required == Some(true) {
                right
            } else {
                right.union(Exits::normal())
            })
        }
        Expression::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            let branches = match constant_bool(condition) {
                Some(true) => expression(then_expr, exhaustive),
                Some(false) => expression(else_expr, exhaustive),
                None => expression(then_expr, exhaustive).union(expression(else_expr, exhaustive)),
            };
            expression(condition, exhaustive).then(branches)
        }
        Expression::Match(node) => {
            let subject = expression(&node.subject, exhaustive);
            let mut exits = Exits {
                normal: false,
                ..subject
            };
            let mut remaining = subject.normal;
            for case in &node.cases {
                if !remaining {
                    break;
                }
                if let MatchPattern::Expr { expr, .. } = &case.pattern {
                    let pattern = expression(expr, exhaustive);
                    exits = exits.union(Exits {
                        normal: false,
                        ..pattern
                    });
                    remaining = pattern.normal;
                }
                if remaining {
                    exits = exits.union(match_body(&case.body, exhaustive));
                }
            }
            if remaining {
                if let Some(default) = &node.default {
                    exits = exits.union(match_body(default, exhaustive));
                } else {
                    exits.normal |= !exhaustive(node.span);
                }
            }
            exits
        }
        Expression::Literal { .. }
        | Expression::Identifier { .. }
        | Expression::This { .. }
        | Expression::Lambda(_) => Exits::normal(),
    }
}

fn match_body(body: &MatchCaseBody, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    match body {
        MatchCaseBody::Block { block: body, .. } => block(body, exhaustive),
        MatchCaseBody::Expr { expr, .. } => expression(expr, exhaustive),
    }
}

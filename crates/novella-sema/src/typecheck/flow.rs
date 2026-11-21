//! 返回路径检查：退出集合的顺序组合与分支合并，不把 break 当作返回。

use novella_ast::expr::{Expression, Literal};
use novella_ast::stmt::{Block, ForKind, IfBranch, Statement};
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
    exhaustive: &dyn Fn(Span) -> bool,
) -> Exits {
    if condition == Some(false) {
        return Exits::normal();
    }
    let inner = statement(body, exhaustive);
    Exits {
        normal: condition != Some(true) || inner.breaks,
        returns: inner.returns,
        throws: inner.throws,
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

fn statement_inner(stmt: &Statement, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    match stmt {
        Statement::Return { .. } => Exits {
            returns: true,
            ..Exits::default()
        },
        Statement::Throw { .. } => Exits {
            throws: true,
            ..Exits::default()
        },
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
        } => loop_exits(body, constant_bool(condition), exhaustive),
        Statement::For(value) => match &value.condition {
            ForKind::ForIn(_) => loop_exits(&value.block, None, exhaustive),
            ForKind::ForLoop(cond) => loop_exits(
                &value.block,
                cond.condition.as_ref().map_or(Some(true), constant_bool),
                exhaustive,
            ),
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
        Statement::With(value) => block(&value.block, exhaustive),
        Statement::Expression(expr) => expression(expr, exhaustive),
        Statement::Variable(_)
        | Statement::VariableList { .. }
        | Statement::Empty { .. }
        | Statement::Yield { .. } => Exits::normal(),
    }
}

/// match 的已证明穷尽性属于 sema 事实，返回路径分析与 HIR 共用它。
fn expression(expr: &Expression, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        expression_inner(expr, exhaustive)
    })
}

fn expression_inner(expr: &Expression, exhaustive: &dyn Fn(Span) -> bool) -> Exits {
    match expr {
        Expression::Paren { expr, .. } => expression(expr, exhaustive),
        Expression::Match(node) => {
            let mut exits = if exhaustive(node.span) {
                Exits::default()
            } else {
                Exits::normal()
            };
            for body in node
                .cases
                .iter()
                .map(|case| &case.body)
                .chain(node.default.iter())
            {
                let branch = match body {
                    novella_ast::expr::MatchCaseBody::Block { block: body, .. } => {
                        block(body, exhaustive)
                    }
                    novella_ast::expr::MatchCaseBody::Expr { expr, .. } => {
                        expression(expr, exhaustive)
                    }
                };
                exits = exits.union(branch);
            }
            exits
        }
        _ => Exits::normal(),
    }
}

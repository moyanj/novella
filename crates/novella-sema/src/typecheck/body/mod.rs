//! 语句与声明检查：控制流语句、变量声明与绑定模式、局部符号定义；
//! 以及所有检查共享的上下文 [`BodyCtx`]。

use std::collections::{HashMap, HashSet};

use novella_ast::expr::{BinaryOp, Expression, UnaryOp};
use novella_ast::stmt::{
    BindingPattern, Block, ForInit, ForKind, Statement, VariableStmt, VariableType,
};
use novella_core::Span;

type InferredReturns = std::rc::Rc<std::cell::RefCell<Vec<(Span, Ty)>>>;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::{ScopeId, ScopeKind, Symbol, SymbolKind};
use crate::ty::Ty;

use super::expr::{check_expr, check_expr_with_hint};

/// 函数体 / 语句序列的检查上下文。
#[derive(Default)]
pub(crate) struct LoopAssignments {
    breaks: Vec<HashSet<Span>>,
    continues: Vec<HashSet<Span>>,
}

pub(crate) struct BodyCtx {
    /// 仅无返回注解的块体 lambda 收集自身可达 return 的类型。
    pub inferred_returns: Option<InferredReturns>,
    pub return_states: Option<std::rc::Rc<std::cell::RefCell<Vec<HashSet<Span>>>>>,
    pub exception_states: Option<std::rc::Rc<std::cell::RefCell<Vec<HashSet<Span>>>>>,
    pub reachable: bool,
    pub loop_assignments: Option<std::rc::Rc<std::cell::RefCell<LoopAssignments>>>,
    /// 按定义位置追踪尚未赋值的局部，避免同名遮蔽污染外层。
    pub uninitialized: HashSet<Span>,
    pub(crate) module: ModuleId,
    pub scope: ScopeId,
    pub in_unsafe: bool,
    pub in_async: bool,
    pub in_loop: bool,
    pub in_catch: bool,
    /// 方法内的 `this` 类型（非方法上下文为 None）
    pub this_ty: Option<Ty>,
    /// 析构接收者只允许借用；其参数定义位置用于区分局部同名遮蔽。
    pub destructor_self: Option<Span>,
    /// lambda 对接收者字段的读取同样需要捕获自身，不能视为当前栈内借用。
    pub destructor_capture: bool,
    /// 仅对当前一次表达式检查有效，不传播到调用实参等拥有位置。
    pub borrowed_receiver: bool,
    /// 期望返回类型（顶层语句为 None，`return` 即报错）
    pub expected_return: Option<Ty>,
    /// 泛型形参环境（`T` → `TypeParam("T")`）
    pub type_env: HashMap<String, Ty>,
    /// 顶层变量声明复用声明收集阶段登记的符号（回填类型而非重定义）
    pub fill_existing: bool,
}
impl BodyCtx {
    pub(crate) fn base(module: ModuleId, scope: ScopeId) -> Self {
        BodyCtx {
            inferred_returns: None,
            return_states: None,
            exception_states: None,
            reachable: true,
            loop_assignments: None,
            uninitialized: HashSet::new(),
            module,
            scope,
            in_unsafe: false,
            in_async: false,
            in_loop: false,
            in_catch: false,
            this_ty: None,
            destructor_self: None,
            destructor_capture: false,
            borrowed_receiver: false,
            expected_return: None,
            type_env: HashMap::new(),
            fill_existing: false,
        }
    }
    pub(crate) fn record_possible_exception(&self) {
        if self.reachable
            && let Some(states) = &self.exception_states
        {
            states.borrow_mut().push(self.uninitialized.clone());
        }
    }
}

// ---------- 语句 ----------
pub(crate) fn check_stmt(sema: &mut Sema, ctx: &mut BodyCtx, stmt: &Statement) {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        check_stmt_inner(sema, ctx, stmt);
        if ctx.reachable {
            if matches!(stmt, Statement::Return { .. })
                && let Some(states) = &ctx.return_states
            {
                states.borrow_mut().push(ctx.uninitialized.clone());
            }
            if matches!(stmt, Statement::Throw { .. }) {
                ctx.record_possible_exception();
            }
        }
    })
}

fn check_stmt_inner(sema: &mut Sema, ctx: &mut BodyCtx, stmt: &Statement) {
    match stmt {
        Statement::Variable(decl) => check_variable_decl(sema, ctx, decl),
        Statement::VariableList { declarations, .. } => {
            for decl in declarations {
                check_variable_decl(sema, ctx, decl);
            }
        }
        Statement::Empty { .. } => {}
        Statement::Return { value, span } => {
            let expected_return = ctx.expected_return.clone();
            match expected_return {
                None => sema.type_error(ctx.module, *span, "`return` 只能出现在函数内"),
                Some(expected) => match value {
                    None => {
                        if ctx.reachable
                            && let Some(returns) = &ctx.inferred_returns
                        {
                            returns.borrow_mut().push((*span, Ty::Void));
                        }
                        if !matches!(expected, Ty::Void) && !expected.is_opaque() {
                            sema.type_error(
                                ctx.module,
                                *span,
                                format!("函数返回类型为 `{expected}`，不能只写 `return;`"),
                            );
                        }
                    }
                    Some(value) => {
                        // 期望类型下推：`return [];` 在 `-> i32[]` 里合法
                        let value_ty = check_expr_with_hint(sema, ctx, value, &expected);
                        if ctx.reachable
                            && let Some(returns) = &ctx.inferred_returns
                        {
                            returns.borrow_mut().push((value.span(), value_ty.clone()));
                        }
                        if !sema.is_assignable(&expected, &value_ty) {
                            sema.type_error(
                                ctx.module,
                                value.span(),
                                format!("返回类型 `{value_ty}` 与函数声明的 `{expected}` 不兼容"),
                            );
                        }
                    }
                },
            }
        }
        Statement::Break { span } => {
            if ctx.reachable
                && let Some(flow) = &ctx.loop_assignments
            {
                flow.borrow_mut().breaks.push(ctx.uninitialized.clone());
            }
            if !ctx.in_loop {
                sema.type_error(ctx.module, *span, "`break` 只能出现在循环内");
            }
        }
        Statement::Continue { span } => {
            if ctx.reachable
                && let Some(flow) = &ctx.loop_assignments
            {
                flow.borrow_mut().continues.push(ctx.uninitialized.clone());
            }
            if !ctx.in_loop {
                sema.type_error(ctx.module, *span, "`continue` 只能出现在循环内");
            }
        }
        Statement::Throw { expr: None, span } => {
            if !ctx.in_catch {
                sema.type_error(ctx.module, *span, "throw; 仅能在 catch 内重抛");
            }
        }
        Statement::Throw {
            expr: Some(expr), ..
        } => {
            let ty = check_expr(sema, ctx, expr);
            // 异常对象使用编译器提供的 Exception 名义身份。
            if !sema.is_subclass(&crate::ty::exception_type(), &ty) && !ty.is_opaque() {
                sema.type_error(
                    ctx.module,
                    expr.span(),
                    format!("`throw` 的操作数必须是 Exception 子类，实为 `{ty}`"),
                );
            }
        }
        Statement::Yield { span, value, .. } => {
            // 语法保留但尚无 yield 语义，明确报错并检查子表达式。
            sema.type_error(
                ctx.module,
                *span,
                "`yield` 语义尚未支持（语法保留，检查跳过）",
            );
            if let Some(value) = value {
                check_expr(sema, ctx, value);
            }
        }
        Statement::Block(block) => check_block(sema, ctx, block),
        Statement::If(if_stmt) => {
            let mut merged = HashSet::new();
            let mut has_else = false;
            let mut remaining = true;
            for branch in &if_stmt.branches {
                match branch {
                    novella_ast::stmt::IfBranch::ElseIf {
                        condition, block, ..
                    } => {
                        let reachable = ctx.reachable;
                        ctx.reachable &= remaining;
                        expect_boolean(sema, ctx, condition, "if 条件");
                        ctx.reachable = reachable;
                        let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
                        let mut branch_ctx = fork_scope(ctx, scope);
                        branch_ctx.reachable &=
                            remaining && super::flow::constant_bool(condition) != Some(false);
                        check_stmt(sema, &mut branch_ctx, block);
                        if remaining
                            && super::flow::constant_bool(condition) != Some(false)
                            && super::flow::falls_through(sema, ctx.module, block)
                        {
                            merged.extend(branch_ctx.uninitialized);
                        }
                        remaining &= super::flow::constant_bool(condition) != Some(true);
                    }
                    novella_ast::stmt::IfBranch::Else { block, .. } => {
                        has_else = true;
                        let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
                        let mut branch_ctx = fork_scope(ctx, scope);
                        branch_ctx.reachable &= remaining;
                        check_stmt(sema, &mut branch_ctx, block);
                        if remaining && super::flow::falls_through(sema, ctx.module, block) {
                            merged.extend(branch_ctx.uninitialized);
                        }
                    }
                }
            }
            if !has_else && remaining {
                // 未命中任何分支也已经求值了所有条件，保留条件中的赋值。
                merged.extend(ctx.uninitialized.iter().copied());
            }
            ctx.uninitialized = merged;
        }
        Statement::While {
            condition, body, ..
        } => {
            expect_boolean(sema, ctx, condition, "while 条件");
            let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
            let mut body_ctx = fork_scope(ctx, scope);
            body_ctx.in_loop = true;
            body_ctx.loop_assignments = Some(Default::default());
            body_ctx.reachable &= super::flow::constant_bool(condition) != Some(false);
            check_stmt(sema, &mut body_ctx, body);
            if super::flow::constant_bool(condition) == Some(true) {
                ctx.uninitialized = body_ctx
                    .loop_assignments
                    .as_ref()
                    .expect("循环上下文")
                    .borrow()
                    .breaks
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
            }
        }
        Statement::For(for_stmt) => {
            let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
            let mut loop_ctx = fork_scope(ctx, scope);
            loop_ctx.in_loop = true;
            loop_ctx.loop_assignments = Some(Default::default());
            match &for_stmt.condition {
                ForKind::ForIn(cond) => {
                    let iter_ty = check_expr(sema, &mut loop_ctx, &cond.right);
                    let element_ty =
                        iteration::element_type(sema, &loop_ctx, iter_ty, cond.right.span());
                    bind_pattern(sema, &mut loop_ctx, &cond.left, &element_ty);
                }
                ForKind::ForLoop(cond) => {
                    if let Some(init) = &cond.init {
                        match init {
                            ForInit::Declaration { declarations, .. } => {
                                for decl in declarations {
                                    check_variable_decl(sema, &mut loop_ctx, decl);
                                }
                            }
                            ForInit::Expression { expr, .. } => {
                                check_expr(sema, &mut loop_ctx, expr);
                            }
                        }
                    }
                    if let Some(condition) = &cond.condition {
                        expect_boolean(sema, &mut loop_ctx, condition, "for 条件");
                        loop_ctx.reachable &= super::flow::constant_bool(condition) != Some(false);
                    }
                }
            }
            let entry = loop_ctx.uninitialized.clone();
            check_stmt(sema, &mut loop_ctx, &for_stmt.block);
            if let ForKind::ForLoop(cond) = &for_stmt.condition {
                let flow = loop_ctx
                    .loop_assignments
                    .as_ref()
                    .expect("循环上下文")
                    .clone();
                if !super::flow::falls_through(sema, ctx.module, &for_stmt.block) {
                    loop_ctx.uninitialized.clear();
                }
                for state in &flow.borrow().continues {
                    loop_ctx.uninitialized.extend(state.iter().copied());
                }
                // 更新段只由正常尾部或 continue 到达，return/break 不经过这里。
                loop_ctx.reachable &= super::flow::falls_through(sema, ctx.module, &for_stmt.block)
                    || !flow.borrow().continues.is_empty();
                if let Some(update) = &cond.update {
                    check_expr(sema, &mut loop_ctx, update);
                }
                let always = cond
                    .condition
                    .as_ref()
                    .is_none_or(|c| super::flow::constant_bool(c) == Some(true));
                ctx.uninitialized = flow.borrow().breaks.iter().flatten().copied().collect();
                if !always {
                    ctx.uninitialized.extend(entry);
                }
            } else {
                // for-in 的迭代表达式必执行一次；循环体可能执行零次。
                ctx.uninitialized = entry;
            }
        }
        Statement::Try(try_stmt) => check_try(sema, ctx, try_stmt),
        Statement::With(with_stmt) => {
            let resource_ty = check_expr(sema, ctx, &with_stmt.expr);
            let base = match &resource_ty {
                Ty::Applied(base, _) => &**base,
                ty => ty,
            };
            if matches!(base, Ty::Class(..) | Ty::Struct(..)) {
                let key = crate::typecheck::this_ty_key(base);
                for name in ["__enter", "__exit"] {
                    if let Some(hit) =
                        crate::typecheck::member::lookup_member_chain(sema, &key, name, false)
                    {
                        hit.check_visible(sema, ctx, name, with_stmt.expr.span());
                    }
                }
            }
            let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
            let mut with_ctx = fork_scope(ctx, scope);
            if let Some(binding) = &with_stmt.binding {
                let bound_ty = match &binding.ty {
                    Some(ty) => {
                        let annotated = sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
                        if !sema.is_assignable(&annotated, &resource_ty) {
                            sema.type_error(
                                ctx.module,
                                binding.span,
                                format!(
                                    "with 绑定类型 `{annotated}` 与资源类型 `{resource_ty}` 不兼容"
                                ),
                            );
                        }
                        annotated
                    }
                    None => resource_ty,
                };
                bind_pattern(sema, &mut with_ctx, &binding.pattern, &bound_ty);
            }
            check_block(sema, &mut with_ctx, &with_stmt.block);
            ctx.uninitialized = with_ctx.uninitialized;
        }
        Statement::Unsafe { block, .. } => {
            let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
            let mut unsafe_ctx = fork_scope(ctx, scope);
            unsafe_ctx.in_unsafe = true;
            check_block(sema, &mut unsafe_ctx, block);
            ctx.uninitialized = unsafe_ctx.uninitialized;
        }
        Statement::Expression(expr) => {
            check_expr(sema, ctx, expr);
        }
    }
}
/// 以新作用域派生上下文（其余标志继承）。
pub(crate) fn fork_scope(ctx: &BodyCtx, scope: ScopeId) -> BodyCtx {
    BodyCtx {
        inferred_returns: ctx.inferred_returns.clone(),
        return_states: ctx.return_states.clone(),
        exception_states: ctx.exception_states.clone(),
        reachable: ctx.reachable,
        loop_assignments: ctx.loop_assignments.clone(),
        uninitialized: ctx.uninitialized.clone(),
        module: ctx.module,
        scope,
        in_unsafe: ctx.in_unsafe,
        in_async: ctx.in_async,
        in_loop: ctx.in_loop,
        in_catch: ctx.in_catch,
        this_ty: ctx.this_ty.clone(),
        destructor_self: ctx.destructor_self,
        destructor_capture: ctx.destructor_capture,
        borrowed_receiver: false,
        expected_return: ctx.expected_return.clone(),
        type_env: ctx.type_env.clone(),
        fill_existing: false,
    }
}
pub(crate) fn check_block(sema: &mut Sema, ctx: &mut BodyCtx, block: &Block) {
    let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
    let mut block_ctx = fork_scope(ctx, scope);
    for stmt in &block.stmts {
        check_stmt(sema, &mut block_ctx, stmt);
        block_ctx.reachable &= super::flow::falls_through(sema, ctx.module, stmt);
    }
    ctx.uninitialized = block_ctx.uninitialized;
}
/// 一元运算符的源码文本（诊断消息用；UnaryOp 不实现 Display）。
pub(crate) fn unary_op_text(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Plus => "+",
        UnaryOp::Minus => "-",
        UnaryOp::Not => "not",
        UnaryOp::BitNot => "~",
        UnaryOp::PostIncrement => "++",
        UnaryOp::PostDecrement => "--",
        UnaryOp::AddressOf => "&",
        UnaryOp::DerefPointer => "*",
    }
}
/// 二元运算符的源码文本（诊断消息用；BinaryOp 不实现 Display）。
pub(crate) fn binary_op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Multiply => "*",
        BinaryOp::Divide => "/",
        BinaryOp::Modulus => "%",
        BinaryOp::Plus => "+",
        BinaryOp::Minus => "-",
        BinaryOp::LeftShift => "<<",
        BinaryOp::RightShift => ">>",
        BinaryOp::LessThan => "<",
        BinaryOp::GreaterThan => ">",
        BinaryOp::LessThanEquals => "<=",
        BinaryOp::GreaterThanEquals => ">=",
        BinaryOp::Equals => "==",
        BinaryOp::NotEquals => "!=",
        BinaryOp::IdentityEquals => "===",
        BinaryOp::IdentityNotEquals => "!==",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::LogicalAnd => "and",
        BinaryOp::LogicalOr => "or",
        BinaryOp::NullCoalesce => "??",
    }
}
/// 条件位置的 boolean 检查。
pub(crate) fn expect_boolean(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression, what: &str) {
    let ty = check_expr(sema, ctx, expr);
    if !matches!(ty, Ty::Boolean) && !ty.is_opaque() {
        sema.type_error(
            ctx.module,
            expr.span(),
            format!("{what}必须是 `boolean`，实为 `{ty}`"),
        );
    }
}

mod bindings;
pub(crate) use bindings::*;

mod iteration;
mod try_flow;
use try_flow::check_try;

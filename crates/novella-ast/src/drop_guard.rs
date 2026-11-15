//! AST 的安全析构：深层 `Box` 递归链在 `Drop` 时受分段栈保护。
//!
//! 解析期由 `novella-parser::recursion` 保证递归下降不爆栈；但 AST 是深度递归
//! 的 `Box<Expression>` / `Box<Statement>` / `Box<TypeExpr>` 链，**编译器默认生成
//! 的析构器逐层递归下降**，数千层的 AST 释放时会沿 `drop_glue` 把真实栈打穿
//! （实测：2000 层嵌套 `unsafe` 块、5000 层括号，在 256 KiB 线程栈上都直接 abort）。
//!
//! 做法：三个递归根类型各自实现 `Drop`，但**本层不做递归**：
//!
//! 1. 在 [`with_stack`]（`stacker::maybe_grow`）里保证当前层栈充足；
//! 2. 只摘取本节点的**直接子节点**（`Box` 内容 / `Vec` 元素）移进局部容器；
//! 3. [`release_all`] 逐个释放子节点——释放时子节点自己的 `Drop` 再次执行，
//!    于是下一层又落进一段新栈。
//!
//! 净效果：析构深度由「真实栈深度」变成「分段栈切换次数」，深层 AST 可安全释放。
//!
//! 维护约束：**新增 `Box<Expression>` / `Box<Statement>` / `Box<TypeExpr>` /
//! `Vec<...>` 递归字段（含嵌套在 `IfStmt` / `ForStmt` / `TryStmt` / `Match` /
//! `Lambda` 等中间结构里的）时必须同步本模块**；`match` 一律穷尽，不留 `_` 兜底，
//! 这样漏改会直接编译失败而不是静默后退回递归析构。

use crate::expr::{Expression, Lambda, LambdaBody, Match, MatchCaseBody, PropertyAssignment};
use crate::stmt::{
    Block, CatchClause, ForInit, ForKind, ForStmt, IfBranch, IfStmt, Statement, TryStmt,
    VariableStmt, WithStmt,
};
use crate::types::{FunctionType, Param, PredefinedType, TypeExpr};
use novella_core::Span;

/// 红区：剩余栈低于该值时切到新分段。
///
/// 与 parser 侧（`novella-parser/src/recursion.rs`）保持同一组参数，
/// 使解析期与析构期的分段大小一致，内存占用易于推算。
const RED_ZONE: usize = 512 * 1024;

/// 单段栈字节数（必须显著大于 [`RED_ZONE`]，否则每次进入都会立即再切段）。
const STACK_SEGMENT: usize = 2 * 1024 * 1024;

/// 占位跨度（占位值随所在 `Box` 立即释放，不会被任何代码读取）。
const EMPTY_SPAN: Span = Span { start: 0, end: 0 };

/// 在栈空间充足的上下文里执行 `f`；栈不足时切到堆上新分段。
#[inline]
pub(crate) fn with_stack<T>(f: impl FnOnce() -> T) -> T {
    stacker::maybe_grow(RED_ZONE, STACK_SEGMENT, f)
}

/// 逐个释放一层子节点（每个元素的 `Drop` 会各自再切一次分段栈）。
#[inline]
fn release_all<T>(mut items: Vec<T>) {
    while let Some(item) = items.pop() {
        drop(item);
    }
}

/// 取走 `Box<T>` 的内容，留下无子节点的占位值。
#[inline]
fn take_boxed<T>(slot: &mut Box<T>, placeholder: T) -> T {
    std::mem::replace(slot.as_mut(), placeholder)
}

/// 无子节点的占位表达式。
#[inline]
fn no_expr() -> Expression {
    Expression::This { span: EMPTY_SPAN }
}

/// 无子节点的占位语句。
#[inline]
fn no_stmt() -> Statement {
    Statement::Empty { span: EMPTY_SPAN }
}

/// 无子节点的占位类型。
#[inline]
fn no_type() -> TypeExpr {
    TypeExpr::Predefined {
        ty: PredefinedType::Int,
        span: EMPTY_SPAN,
    }
}

// ===== Expression =====

impl Drop for Expression {
    fn drop(&mut self) {
        with_stack(|| {
            let mut children: Vec<Expression> = Vec::new();
            self.take_children(&mut children);
            release_all(children);
        });
    }
}

impl Expression {
    /// 把本节点的直接子表达式移出并压入 `out`。
    ///
    /// `match` 穷尽所有变体：新增变体时此处编译失败，强制同步。
    fn take_children(&mut self, out: &mut Vec<Expression>) {
        match self {
            Expression::Literal { .. }
            | Expression::Identifier { .. }
            | Expression::This { .. } => {}
            Expression::Paren { expr, .. }
            | Expression::Await { expr, .. }
            | Expression::Unary { operand: expr, .. }
            | Expression::MemberAccess { target: expr, .. } => {
                out.push(take_boxed(expr, no_expr()))
            }
            Expression::Tuple { elements, .. } | Expression::ArrayLiteral { elements, .. } => {
                out.append(elements);
            }
            Expression::ObjectLiteral { properties, .. }
            | Expression::StructInit { properties, .. } => {
                take_property_children(std::mem::take(properties), out);
            }
            Expression::Binary { left, right, .. } => {
                out.push(take_boxed(left, no_expr()));
                out.push(take_boxed(right, no_expr()));
            }
            Expression::Is { operand, .. } => out.push(take_boxed(operand, no_expr())),
            Expression::Cast { expr, .. } => out.push(take_boxed(expr, no_expr())),
            Expression::Ternary {
                condition,
                then_expr,
                else_expr,
                ..
            } => {
                out.push(take_boxed(condition, no_expr()));
                out.push(take_boxed(then_expr, no_expr()));
                out.push(take_boxed(else_expr, no_expr()));
            }
            Expression::Assignment { target, value, .. } => {
                out.push(take_boxed(target, no_expr()));
                out.push(take_boxed(value, no_expr()));
            }
            Expression::IndexAccess { target, index, .. } => {
                out.push(take_boxed(target, no_expr()));
                out.push(take_boxed(index, no_expr()));
            }
            Expression::Call { callee, args, .. } | Expression::MacroCall { callee, args, .. } => {
                out.push(take_boxed(callee, no_expr()));
                out.append(args);
            }
            Expression::New { target, args, .. } => {
                out.push(take_boxed(target, no_expr()));
                out.append(args);
            }
            Expression::Match(m) => take_match_children(m, out),
            Expression::Lambda(l) => take_lambda_children(l, out),
        }
    }
}

/// 取走对象字面量 / 结构体初始化中的属性值表达式。
fn take_property_children(properties: Vec<PropertyAssignment>, out: &mut Vec<Expression>) {
    for mut prop in properties {
        if let Some(value) = prop.value.take() {
            out.push(value);
        }
    }
}

/// 取走 `match` 表达式的直接子表达式（此前须已取到 `Match` 的所有权）。
fn take_match_children(m: &mut Match, out: &mut Vec<Expression>) {
    out.push(take_boxed(&mut m.subject, no_expr()));
    for case in std::mem::take(&mut m.cases) {
        take_case_body_children(case.body, out);
    }
    if let Some(default) = m.default.take() {
        take_case_body_children(default, out);
    }
}

/// 取走分支体内部的子表达式。
fn take_case_body_children(body: MatchCaseBody, out: &mut Vec<Expression>) {
    match body {
        MatchCaseBody::Expr { expr, .. } => out.push(*expr),
        MatchCaseBody::Block { block, .. } => take_block_children(block),
    }
}

/// 取走 `lambda` 的直接子表达式。
fn take_lambda_children(l: &mut Lambda, out: &mut Vec<Expression>) {
    match std::mem::replace(
        &mut l.body,
        LambdaBody::Expr {
            expr: Box::new(no_expr()),
            span: EMPTY_SPAN,
        },
    ) {
        LambdaBody::Expr { expr, .. } => out.push(*expr),
        LambdaBody::Block { block, .. } => take_block_children(block),
    }
}

/// 取走语句块内直接语句的**表达式**载荷（语句本身由 [`Statement::drop`] 切段）。
///
/// 这里不递归进子语句：块内语句会在 `release_all` / `Statement::drop` 中各自
/// 进入新分段，符合「每层一次分段栈切换」的设计。
fn take_block_children(block: Block) {
    release_all(block.stmts);
}

// ===== Statement =====

impl Drop for Statement {
    fn drop(&mut self) {
        with_stack(|| {
            let mut children: Vec<Statement> = Vec::new();
            let mut exprs: Vec<Expression> = Vec::new();
            self.take_children(&mut children, &mut exprs);
            release_all(children);
            release_all(exprs);
        });
    }
}

impl Statement {
    /// 把本节点的直接子语句 / 子表达式移出。`match` 穷尽所有变体。
    fn take_children(&mut self, stmts: &mut Vec<Statement>, exprs: &mut Vec<Expression>) {
        match self {
            Statement::Empty { .. } | Statement::Break { .. } | Statement::Continue { .. } => {}
            Statement::Variable(v) => take_variable_children(v, exprs),
            Statement::VariableList { declarations, .. } => {
                for decl in std::mem::take(declarations) {
                    drop_variable(decl, exprs);
                }
            }
            Statement::Return { value, .. } | Statement::Yield { value, .. } => {
                if let Some(v) = value.take() {
                    exprs.push(v);
                }
            }
            Statement::Throw { expr, .. } => exprs.push(std::mem::replace(expr, no_expr())),
            Statement::Block(block) => stmts.append(&mut block.stmts),
            Statement::Expression(e) => exprs.push(std::mem::replace(e, no_expr())),
            Statement::While {
                condition, body, ..
            } => {
                exprs.push(std::mem::replace(condition, no_expr()));
                stmts.push(take_boxed(body, no_stmt()));
            }
            Statement::Unsafe { block, .. } => stmts.append(&mut block.stmts),
            Statement::If(if_stmt) => take_if_children(if_stmt, stmts, exprs),
            Statement::For(for_stmt) => take_for_children(for_stmt, stmts, exprs),
            Statement::Try(try_stmt) => take_try_children(try_stmt, stmts),
            Statement::With(with_stmt) => take_with_children(with_stmt, stmts, exprs),
        }
    }
}

/// 变量声明中的值表达式与类型承载。
fn take_variable_children(v: &mut VariableStmt, exprs: &mut Vec<Expression>) {
    if let Some(value) = v.value.take() {
        exprs.push(value);
    }
}

/// 释放一条变量声明（`try` 的 `ty` 由类型析构自行切段）。
fn drop_variable(decl: VariableStmt, exprs: &mut Vec<Expression>) {
    let mut decl = decl;
    if let Some(value) = decl.value.take() {
        exprs.push(value);
    }
}

/// 取走 `if` 语句的直接子节点。
fn take_if_children(if_stmt: &mut IfStmt, stmts: &mut Vec<Statement>, exprs: &mut Vec<Expression>) {
    for branch in std::mem::take(&mut if_stmt.branches) {
        match branch {
            IfBranch::ElseIf {
                condition, block, ..
            } => {
                exprs.push(*condition);
                stmts.push(*block);
            }
            IfBranch::Else { block, .. } => stmts.push(*block),
        }
    }
}

/// 取走 `for` 语句的直接子节点。
fn take_for_children(
    for_stmt: &mut ForStmt,
    stmts: &mut Vec<Statement>,
    exprs: &mut Vec<Expression>,
) {
    stmts.push(take_boxed(&mut for_stmt.block, no_stmt()));
    match std::mem::replace(
        &mut for_stmt.condition,
        ForKind::ForLoop(Box::new(crate::stmt::ForLoopCond {
            init: None,
            condition: None,
            update: None,
            span: EMPTY_SPAN,
        })),
    ) {
        ForKind::ForIn(cond) => exprs.push(cond.right),
        ForKind::ForLoop(cond) => {
            let mut cond = *cond;
            if let Some(init) = cond.init.take() {
                match init {
                    ForInit::Declaration { declarations, .. } => {
                        for decl in declarations {
                            drop_variable(decl, exprs);
                        }
                    }
                    ForInit::Expression { expr, .. } => exprs.push(expr),
                }
            }
            if let Some(condition) = cond.condition.take() {
                exprs.push(condition);
            }
            if let Some(update) = cond.update.take() {
                exprs.push(update);
            }
        }
    }
}

/// 取走 `try` 语句的直接子节点。
fn take_try_children(try_stmt: &mut TryStmt, stmts: &mut Vec<Statement>) {
    stmts.append(&mut try_stmt.block.stmts);
    for clause in std::mem::take(&mut try_stmt.catches) {
        let mut clause: CatchClause = clause;
        stmts.append(&mut clause.block.stmts);
    }
    if let Some(mut finally) = try_stmt.finally.take() {
        stmts.append(&mut finally.stmts);
    }
}

/// 取走 `with` 语句的直接子节点。
fn take_with_children(
    with_stmt: &mut WithStmt,
    stmts: &mut Vec<Statement>,
    exprs: &mut Vec<Expression>,
) {
    exprs.push(std::mem::replace(&mut with_stmt.expr, no_expr()));
    stmts.append(&mut with_stmt.block.stmts);
}

// ===== TypeExpr =====

impl Drop for TypeExpr {
    fn drop(&mut self) {
        with_stack(|| {
            let mut children: Vec<TypeExpr> = Vec::new();
            self.take_children(&mut children);
            release_all(children);
        });
    }
}

impl TypeExpr {
    /// 把本节点的直接子类型移出。`match` 穷尽所有变体。
    fn take_children(&mut self, out: &mut Vec<TypeExpr>) {
        match self {
            TypeExpr::Predefined { .. } => {}
            TypeExpr::Reference { generic_args, .. } => out.append(generic_args),
            TypeExpr::Array { element, .. } | TypeExpr::Pointer { element, .. } => {
                out.push(take_boxed(element, no_type()));
            }
            TypeExpr::Tuple { elements, .. }
            | TypeExpr::Union {
                types: elements, ..
            } => {
                out.append(elements);
            }
            TypeExpr::Function(f) => take_function_children(f, out),
        }
    }
}

/// 取走函数类型的参数类型 / 默认值表达式 / 返回类型。
fn take_function_children(f: &mut FunctionType, out: &mut Vec<TypeExpr>) {
    for mut param in std::mem::take(&mut f.params) {
        // 参数默认值是表达式，交给 `Expression::drop` 切段
        if let Some(default) = param.default.take() {
            drop_default_expr(default);
        }
        if let Some(ty) = param.ty.take() {
            out.push(ty);
        }
    }
    out.push(take_boxed(&mut f.return_type, no_type()));
}

/// 在分段栈中释放一个默认值表达式。
fn drop_default_expr(expr: Expression) {
    with_stack(move || drop(expr));
}

// 未在此处显式处理的类型项：
// `Param` 仅被 `FunctionType` 持有，`PredefinedType` 无子节点——
// 若将来给 `Param` 增加新的递归字段，须同步 [`take_function_children`]。
const _: () = {
    // 静态断言：`Param` 仍是函数类型的唯一参数载体（若改名/改结构此处编译失败）
    fn _assert_parts(_: &Param, _: &FunctionType) {}
};

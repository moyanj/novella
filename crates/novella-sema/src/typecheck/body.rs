//! 语句与声明检查：控制流语句、变量声明与绑定模式、局部符号定义；
//! 以及所有检查共享的上下文 [`BodyCtx`]。

use std::collections::HashMap;

use novella_ast::expr::{BinaryOp, Expression, UnaryOp};
use novella_ast::stmt::{
    BindingPattern, Block, ForInit, ForKind, Statement, VariableStmt, VariableType,
};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::{ScopeId, ScopeKind, Symbol, SymbolKind};
use crate::ty::Ty;

use super::expr::{check_expr, check_expr_with_hint};

/// 函数体 / 语句序列的检查上下文。
pub(crate) struct BodyCtx {
    pub(crate) module: ModuleId,
    pub scope: ScopeId,
    pub in_unsafe: bool,
    pub in_async: bool,
    pub in_loop: bool,
    /// 方法内的 `this` 类型（非方法上下文为 None）
    pub this_ty: Option<Ty>,
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
            module,
            scope,
            in_unsafe: false,
            in_async: false,
            in_loop: false,
            this_ty: None,
            expected_return: None,
            type_env: HashMap::new(),
            fill_existing: false,
        }
    }
}

// ---------- 语句 ----------
pub(crate) fn check_stmt(sema: &mut Sema, ctx: &mut BodyCtx, stmt: &Statement) {
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
                        if !matches!(expected, Ty::Void) && !expected.is_opaque() {
                            sema.type_error(
                                ctx.module,
                                *span,
                                format!("函数返回类型为 `{expected}`，不能只写 `return;`"),
                            );
                        }
                    }
                    Some(value) => {
                        let value_ty = check_expr(sema, ctx, value);
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
            if !ctx.in_loop {
                sema.type_error(ctx.module, *span, "`break` 只能出现在循环内");
            }
        }
        Statement::Continue { span } => {
            if !ctx.in_loop {
                sema.type_error(ctx.module, *span, "`continue` 只能出现在循环内");
            }
        }
        Statement::Throw { expr, .. } => {
            let ty = check_expr(sema, ctx, expr);
            // design.md §9.1.1：throw 的操作数必须是 class 引用
            // （Exception 子类校验待 std:exception 落地后收紧）
            if !matches!(ty, Ty::Class(..)) && !ty.is_opaque() {
                sema.type_error(
                    ctx.module,
                    expr.span(),
                    format!("`throw` 的操作数必须是 class 引用，实为 `{ty}`"),
                );
            }
        }
        Statement::Yield { span, value, .. } => {
            // design.md §9.4：语法保留，语义未支持。语法展示文件需要完整覆盖
            // 语句族，故降为 warning（可编译通过），值照常检查避免漏查副作用。
            sema.warning(
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
            for branch in &if_stmt.branches {
                match branch {
                    novella_ast::stmt::IfBranch::ElseIf {
                        condition, block, ..
                    } => {
                        expect_boolean(sema, ctx, condition, "if 条件");
                        let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
                        let mut branch_ctx = fork_scope(ctx, scope);
                        check_stmt(sema, &mut branch_ctx, block);
                    }
                    novella_ast::stmt::IfBranch::Else { block, .. } => {
                        let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
                        let mut branch_ctx = fork_scope(ctx, scope);
                        check_stmt(sema, &mut branch_ctx, block);
                    }
                }
            }
        }
        Statement::While {
            condition, body, ..
        } => {
            expect_boolean(sema, ctx, condition, "while 条件");
            let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
            let mut body_ctx = fork_scope(ctx, scope);
            body_ctx.in_loop = true;
            check_stmt(sema, &mut body_ctx, body);
        }
        Statement::For(for_stmt) => {
            let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
            let mut loop_ctx = fork_scope(ctx, scope);
            loop_ctx.in_loop = true;
            match &for_stmt.condition {
                ForKind::ForIn(cond) => {
                    let iter_ty = check_expr(sema, &mut loop_ctx, &cond.right);
                    let element_ty = match &iter_ty {
                        Ty::Array(element) => (**element).clone(),
                        _ if iter_ty.is_opaque() => Ty::Unknown,
                        _ => {
                            sema.type_error(
                                ctx.module,
                                cond.right.span(),
                                format!("`for ... in` 只能遍历数组，实为 `{iter_ty}`"),
                            );
                            Ty::Unknown
                        }
                    };
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
                    }
                    if let Some(update) = &cond.update {
                        check_expr(sema, &mut loop_ctx, update);
                    }
                }
            }
            check_stmt(sema, &mut loop_ctx, &for_stmt.block);
        }
        Statement::Try(try_stmt) => {
            check_block(sema, ctx, &try_stmt.block);
            for catch in &try_stmt.catches {
                let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
                let mut catch_ctx = fork_scope(ctx, scope);
                let catch_ty = match &catch.ty {
                    Some(ty) => {
                        let ty = sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
                        if !matches!(ty, Ty::Class(..)) && !ty.is_opaque() {
                            sema.type_error(
                                ctx.module,
                                ty_span(ty_expr_of(catch)),
                                format!("catch 类型必须是 class，实为 `{ty}`"),
                            );
                        }
                        ty
                    }
                    None => Ty::Unknown,
                };
                define_local(
                    sema,
                    &catch_ctx,
                    &catch.name,
                    SymbolKind::Variable {
                        is_const: true,
                        ty: Some(catch_ty),
                    },
                    catch.span,
                );
                check_block(sema, &mut catch_ctx, &catch.block);
            }
            if let Some(finally) = &try_stmt.finally {
                check_block(sema, ctx, finally);
            }
        }
        Statement::With(with_stmt) => {
            let resource_ty = check_expr(sema, ctx, &with_stmt.expr);
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
        }
        Statement::Unsafe { block, .. } => {
            let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
            let mut unsafe_ctx = fork_scope(ctx, scope);
            unsafe_ctx.in_unsafe = true;
            check_block(sema, &mut unsafe_ctx, block);
        }
        Statement::Expression(expr) => {
            check_expr(sema, ctx, expr);
        }
    }
}
pub(crate) fn ty_expr_of(catch: &novella_ast::stmt::CatchClause) -> &TypeExpr {
    catch.ty.as_ref().expect("仅在 Some 时调用")
}
pub(crate) fn ty_span(ty: &TypeExpr) -> Span {
    ty.span()
}
/// 以新作用域派生上下文（其余标志继承）。
pub(crate) fn fork_scope(ctx: &BodyCtx, scope: ScopeId) -> BodyCtx {
    BodyCtx {
        module: ctx.module,
        scope,
        in_unsafe: ctx.in_unsafe,
        in_async: ctx.in_async,
        in_loop: ctx.in_loop,
        this_ty: ctx.this_ty.clone(),
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
    }
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
// ---------- 变量声明与绑定模式 ----------
pub(crate) fn check_variable_decl(sema: &mut Sema, ctx: &mut BodyCtx, decl: &VariableStmt) {
    let is_const = matches!(decl.r#type, VariableType::Const);
    let annotated_ty = decl
        .ty
        .as_ref()
        .map(|ty| sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env));
    // 带类型标注时把标注作为期望类型下推（双向检查）：空数组字面量、未定型
    // 字面量等需要上下文才能定型的表达式（design.md §5.2「数组字面量取首元素
    // 为基准」的空情形）由标注给出元素类型，而不是直接报「无法推断」。
    let value_ty = decl.value.as_ref().map(|value| match &annotated_ty {
        Some(annotated) => check_expr_with_hint(sema, ctx, value, annotated),
        None => check_expr(sema, ctx, value),
    });
    let bound_ty = match (&annotated_ty, &value_ty) {
        (Some(annotated), Some(value_ty)) => {
            if !sema.is_assignable(annotated, value_ty) {
                sema.type_error(
                    ctx.module,
                    decl.span,
                    format!("初始化类型 `{value_ty}` 与标注 `{annotated}` 不兼容"),
                );
            }
            annotated.clone()
        }
        (Some(annotated), None) => {
            if is_const {
                sema.type_error(ctx.module, decl.span, "`const` 声明必须初始化");
            }
            annotated.clone()
        }
        (None, Some(value_ty)) => {
            if matches!(value_ty, Ty::Null) {
                sema.type_error(
                    ctx.module,
                    decl.span,
                    "无法用 `null` 推断变量类型，请添加类型标注",
                );
                Ty::Unknown
            } else {
                concrete(value_ty)
            }
        }
        (None, None) => {
            sema.type_error(
                ctx.module,
                decl.span,
                "变量声明需要类型标注或初始值（无法推断类型）",
            );
            Ty::Unknown
        }
    };
    sema.declaration_types
        .insert((ctx.module, decl.span), bound_ty.clone());
    bind_pattern_typed(sema, ctx, &decl.pattern, &bound_ty, is_const);
}
/// 按类型绑定模式（元组解构校验结构与 arity）。
pub(crate) fn bind_pattern(sema: &mut Sema, ctx: &mut BodyCtx, pattern: &BindingPattern, ty: &Ty) {
    bind_pattern_impl(sema, ctx, pattern, ty, false);
}
pub(crate) fn bind_pattern_typed(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    pattern: &BindingPattern,
    ty: &Ty,
    is_const: bool,
) {
    bind_pattern_impl(sema, ctx, pattern, ty, is_const);
}
pub(crate) fn bind_pattern_impl(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    pattern: &BindingPattern,
    ty: &Ty,
    is_const: bool,
) {
    match pattern {
        BindingPattern::Ident { name, span } => {
            let kind = SymbolKind::Variable {
                is_const,
                ty: Some(ty.clone()),
            };
            if ctx.fill_existing {
                // 顶层变量：回填声明收集阶段登记的符号
                if let Some(symbol) = sema.scopes.lookup_mut(ctx.scope, name) {
                    symbol.kind = kind;
                    return;
                }
            }
            define_local(sema, ctx, name, kind, *span);
        }
        BindingPattern::Tuple { elements, span } => {
            let element_tys: Vec<Ty> = match ty {
                Ty::Tuple(elements_ty) if elements_ty.len() == elements.len() => {
                    elements_ty.clone()
                }
                Ty::Tuple(_) => {
                    sema.type_error(ctx.module, *span, "元组解构的元素个数不匹配");
                    vec![Ty::Unknown; elements.len()]
                }
                _ if ty.is_opaque() => vec![Ty::Unknown; elements.len()],
                _ => {
                    sema.type_error(ctx.module, *span, format!("不能用 `{ty}` 做元组解构"));
                    vec![Ty::Unknown; elements.len()]
                }
            };
            for (element, element_ty) in elements.iter().zip(element_tys) {
                bind_pattern_impl(sema, ctx, element, &element_ty, is_const);
            }
        }
    }
}
/// 在当前作用域定义局部符号（重定义报诊断）。
pub(crate) fn define_local(
    sema: &mut Sema,
    ctx: &BodyCtx,
    name: &str,
    kind: SymbolKind,
    span: Span,
) {
    let symbol = Symbol {
        name: name.to_string(),
        kind,
        visibility: None,
        span,
        name_span: span,
        module: ctx.module,
    };
    if sema.scopes.define(ctx.scope, symbol).is_err() {
        sema.type_error(ctx.module, span, format!("重复定义 `{name}`"));
    }
}
/// 字面量归一化：未定型字面量落到默认类型（`int` ≡ `i32`，`float` ≡ `f32`）。
pub(crate) fn concrete(ty: &Ty) -> Ty {
    match ty {
        Ty::IntLiteral => Ty::Int(crate::ty::IntKind::I32),
        Ty::FloatLiteral => Ty::Float(crate::ty::FloatKind::F32),
        Ty::Tuple(elements) => Ty::Tuple(elements.iter().map(concrete).collect()),
        Ty::Array(element) => Ty::Array(Box::new(concrete(element))),
        other => other.clone(),
    }
}

//! 基本类型检查（对应 `docs/sema-phase1.md` §5 与 §7 追加项 A–E）。
//!
//! 覆盖：字面量值解析、运算符矩阵、赋值兼容、调用 / 成员 / 构造检查、
//! return 匹配、unsafe 上下文规则（S1–S10 子集 + extern 调用 E7）、
//! const / 左值检查、await / this 上下文、throw 类型、对象字面量简写展开、
//! f-string 插值检查。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use novella_ast::expr::{
    AssignOp, BinaryOp, Expression, LambdaBody, Literal, MatchCaseBody, MatchPattern, UnaryOp,
};
use novella_ast::program::{FieldMember, FunctionItem, MethodMember};
use novella_ast::stmt::{
    BindingPattern, Block, ForInit, ForKind, Statement, VariableStmt, VariableType,
};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::{NsTarget, ScopeId, ScopeKind, Symbol, SymbolKind};
use crate::ty::{
    ClassDef, FunctionSig, ParamSig, StructDef, Ty, TypeDef, classify_number_literal, ty_eq,
};

/// 函数体 / 语句序列的检查上下文。
pub(crate) struct BodyCtx {
    pub module: ModuleId,
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
    fn base(module: ModuleId, scope: ScopeId) -> Self {
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

// ---------- 入口：函数体 / 方法体 / 字段初始化 / 顶层语句 ----------

/// 检查顶层函数体。
pub fn check_function_body(
    sema: &mut Sema,
    module: ModuleId,
    function: &FunctionItem,
    block: &Statement,
) {
    let global = sema.modules[module].global_scope;
    // 取 Phase 2b 回填的签名（重定义冲突导致缺失时跳过体检查）
    let sig = match sema.scopes.lookup_local(global, &function.name) {
        Some(Symbol {
            kind: SymbolKind::Function { sig },
            ..
        }) => sig.clone(),
        _ => return,
    };
    let fn_scope = sema.scopes.new_scope(Some(global), ScopeKind::Function);
    let mut ctx = BodyCtx::base(module, fn_scope);
    ctx.in_async = sig.is_async;
    ctx.expected_return = Some(sig.ret.clone());
    if let Some(function_type) = &function.r#type {
        for param in &function_type.type_parameters {
            ctx.type_env
                .insert(param.name.clone(), Ty::TypeParam(param.name.clone()));
        }
    }
    for param in &sig.params {
        define_local(
            sema,
            &ctx,
            &param.name,
            SymbolKind::Param {
                ty: param.ty.clone(),
            },
            function.span,
        );
    }
    check_stmt(sema, &mut ctx, block);
}

/// 检查方法体（`this` 绑定为属主类型）。
pub fn check_method_body(
    sema: &mut Sema,
    module: ModuleId,
    this_ty: &Ty,
    method: &MethodMember,
    block: &Block,
) {
    let global = sema.modules[module].global_scope;
    let fn_scope = sema.scopes.new_scope(Some(global), ScopeKind::Function);
    let mut ctx = BodyCtx::base(module, fn_scope);
    ctx.this_ty = Some(this_ty.clone());
    for param in &method.type_parameters {
        ctx.type_env
            .insert(param.name.clone(), Ty::TypeParam(param.name.clone()));
    }
    // 属主类型的泛型形参同样可见（class<T> 的方法内可用 T）
    if let Some(def) = sema
        .type_defs
        .get(&(this_ty_key(this_ty).0, this_ty_key(this_ty).1))
    {
        let _ = def; // 形参名未存注册表，由调用方保证方法体内 T 已覆盖（见下）
    }
    // 从注册表读取方法签名以绑定形参
    let method_sig = lookup_method_def(sema, this_ty, &method.name).map(|m| m.sig.clone());
    if let Some(sig) = &method_sig {
        ctx.expected_return = Some(sig.ret.clone());
        for param in &sig.params {
            define_local(
                sema,
                &ctx,
                &param.name,
                SymbolKind::Param {
                    ty: param.ty.clone(),
                },
                method.span,
            );
        }
    }
    check_block(sema, &mut ctx, block);
}

fn this_ty_key(ty: &Ty) -> (ModuleId, String) {
    match ty {
        Ty::Struct(m, n) | Ty::Class(m, n) => (*m, n.clone()),
        _ => unreachable!("this_ty 恒为 Struct/Class"),
    }
}

fn lookup_method_def(sema: &Sema, this_ty: &Ty, name: &str) -> Option<crate::ty::MethodSig> {
    let key = this_ty_key(this_ty);
    match sema.type_defs.get(&key) {
        Some(TypeDef::Struct(StructDef { methods, .. }))
        | Some(TypeDef::Class(ClassDef { methods, .. })) => methods.get(name).cloned(),
        _ => None,
    }
}

/// 检查字段初始化表达式；无标注字段在此推导类型并回填注册表。
pub fn check_field_initializer(
    sema: &mut Sema,
    module: ModuleId,
    this_ty: &Ty,
    field: &FieldMember,
    initializer: &Expression,
) {
    let global = sema.modules[module].global_scope;
    let scope = sema.scopes.new_scope(Some(global), ScopeKind::Block);
    let mut ctx = BodyCtx::base(module, scope);
    let value_ty = check_expr(sema, &mut ctx, initializer);
    let key = this_ty_key(this_ty);
    let annotated = field.ty.is_some();
    // 先读出现有字段类型，再做检查 / 回填，避免同时可变借用注册表与 sema
    let existing_ty = match sema.type_defs.get(&key) {
        Some(TypeDef::Struct(StructDef { fields, .. }))
        | Some(TypeDef::Class(ClassDef { fields, .. })) => fields
            .iter()
            .find(|f| f.name == field.name)
            .map(|f| f.ty.clone()),
        _ => None,
    };
    let Some(expected) = existing_ty else { return };
    if !annotated {
        // 无标注：推导回填（字面量归一化为默认类型）
        let inferred = concrete(&value_ty);
        if let Some(def) = sema.type_defs.get_mut(&key) {
            let fields = match def {
                TypeDef::Struct(StructDef { fields, .. })
                | TypeDef::Class(ClassDef { fields, .. }) => fields,
                _ => return,
            };
            if let Some(field_def) = fields.iter_mut().find(|f| f.name == field.name) {
                field_def.ty = inferred;
            }
        }
    } else if !sema.is_assignable(&expected, &value_ty) {
        sema.error(
            module,
            field.span,
            format!(
                "字段 `{}` 的初始值类型 `{value_ty}` 与标注 `{expected}` 不兼容",
                field.name
            ),
        );
    }
}

/// 检查顶层语句（变量声明回填全局符号类型；`return` 非法）。
pub fn check_toplevel_statement(sema: &mut Sema, module: ModuleId, stmt: &Statement) {
    let global = sema.modules[module].global_scope;
    let mut ctx = BodyCtx::base(module, global);
    ctx.fill_existing = true;
    check_stmt(sema, &mut ctx, stmt);
}

// ---------- 语句 ----------

fn check_stmt(sema: &mut Sema, ctx: &mut BodyCtx, stmt: &Statement) {
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
                None => sema.error(ctx.module, *span, "`return` 只能出现在函数内"),
                Some(expected) => match value {
                    None => {
                        if !matches!(expected, Ty::Void) && !expected.is_opaque() {
                            sema.error(
                                ctx.module,
                                *span,
                                format!("函数返回类型为 `{expected}`，不能只写 `return;`"),
                            );
                        }
                    }
                    Some(value) => {
                        let value_ty = check_expr(sema, ctx, value);
                        if !sema.is_assignable(&expected, &value_ty) {
                            sema.error(
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
                sema.error(ctx.module, *span, "`break` 只能出现在循环内");
            }
        }
        Statement::Continue { span } => {
            if !ctx.in_loop {
                sema.error(ctx.module, *span, "`continue` 只能出现在循环内");
            }
        }
        Statement::Throw { expr, .. } => {
            let ty = check_expr(sema, ctx, expr);
            // design.md §9.1.1：throw 的操作数必须是 class 引用
            // （Exception 子类校验待 std:exception 落地后收紧）
            if !matches!(ty, Ty::Class(..)) && !ty.is_opaque() {
                sema.error(
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
                            sema.error(
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
                            sema.error(
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
                            sema.error(
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

fn ty_expr_of(catch: &novella_ast::stmt::CatchClause) -> &TypeExpr {
    catch.ty.as_ref().expect("仅在 Some 时调用")
}

fn ty_span(ty: &TypeExpr) -> Span {
    ty.span()
}

/// 以新作用域派生上下文（其余标志继承）。
fn fork_scope(ctx: &BodyCtx, scope: ScopeId) -> BodyCtx {
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

fn check_block(sema: &mut Sema, ctx: &mut BodyCtx, block: &Block) {
    let scope = sema.scopes.new_scope(Some(ctx.scope), ScopeKind::Block);
    let mut block_ctx = fork_scope(ctx, scope);
    for stmt in &block.stmts {
        check_stmt(sema, &mut block_ctx, stmt);
    }
}

/// 一元运算符的源码文本（诊断消息用；UnaryOp 不实现 Display）。
fn unary_op_text(op: UnaryOp) -> &'static str {
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
fn binary_op_text(op: BinaryOp) -> &'static str {
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
fn expect_boolean(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression, what: &str) {
    let ty = check_expr(sema, ctx, expr);
    if !matches!(ty, Ty::Boolean) && !ty.is_opaque() {
        sema.error(
            ctx.module,
            expr.span(),
            format!("{what}必须是 `boolean`，实为 `{ty}`"),
        );
    }
}

// ---------- 变量声明与绑定模式 ----------

fn check_variable_decl(sema: &mut Sema, ctx: &mut BodyCtx, decl: &VariableStmt) {
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
                sema.error(
                    ctx.module,
                    decl.span,
                    format!("初始化类型 `{value_ty}` 与标注 `{annotated}` 不兼容"),
                );
            }
            annotated.clone()
        }
        (Some(annotated), None) => {
            if is_const {
                sema.error(ctx.module, decl.span, "`const` 声明必须初始化");
            }
            annotated.clone()
        }
        (None, Some(value_ty)) => {
            if matches!(value_ty, Ty::Null) {
                sema.error(
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
            sema.error(
                ctx.module,
                decl.span,
                "变量声明需要类型标注或初始值（无法推断类型）",
            );
            Ty::Unknown
        }
    };
    bind_pattern_typed(sema, ctx, &decl.pattern, &bound_ty, is_const);
}

/// 按类型绑定模式（元组解构校验结构与 arity）。
fn bind_pattern(sema: &mut Sema, ctx: &mut BodyCtx, pattern: &BindingPattern, ty: &Ty) {
    bind_pattern_impl(sema, ctx, pattern, ty, false);
}

fn bind_pattern_typed(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    pattern: &BindingPattern,
    ty: &Ty,
    is_const: bool,
) {
    bind_pattern_impl(sema, ctx, pattern, ty, is_const);
}

fn bind_pattern_impl(
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
                    sema.error(ctx.module, *span, "元组解构的元素个数不匹配");
                    vec![Ty::Unknown; elements.len()]
                }
                _ if ty.is_opaque() => vec![Ty::Unknown; elements.len()],
                _ => {
                    sema.error(ctx.module, *span, format!("不能用 `{ty}` 做元组解构"));
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
fn define_local(sema: &mut Sema, ctx: &BodyCtx, name: &str, kind: SymbolKind, span: Span) {
    let symbol = Symbol {
        name: name.to_string(),
        kind,
        visibility: None,
        span,
        module: ctx.module,
    };
    if sema.scopes.define(ctx.scope, symbol).is_err() {
        sema.error(ctx.module, span, format!("重复定义 `{name}`"));
    }
}

/// 字面量归一化：未定型字面量落到默认类型（`int` ≡ `i32`，`float` ≡ `f32`）。
fn concrete(ty: &Ty) -> Ty {
    match ty {
        Ty::IntLiteral => Ty::Int(crate::ty::IntKind::I32),
        Ty::FloatLiteral => Ty::Float(crate::ty::FloatKind::F32),
        other => other.clone(),
    }
}

// ---------- 表达式 ----------

/// 表达式检查，返回表达式类型（出错时返回 Unknown 并记录诊断，保证可恢复）。
pub(crate) fn check_expr(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> Ty {
    match expr {
        Expression::Literal { value, span } => check_literal(sema, ctx, value, *span),
        Expression::Identifier { name, span } => check_identifier(sema, ctx, name, *span),
        Expression::Paren { expr, .. } => check_expr(sema, ctx, expr),
        Expression::Tuple { elements, .. } => {
            let tys = elements.iter().map(|e| check_expr(sema, ctx, e)).collect();
            Ty::Tuple(tys)
        }
        Expression::ArrayLiteral { elements, span } => {
            check_array_literal(sema, ctx, elements, *span)
        }
        Expression::ObjectLiteral { properties, .. } => check_object_literal(sema, ctx, properties),
        Expression::This { span } => match &ctx.this_ty {
            Some(ty) => ty.clone(),
            None => {
                sema.error(ctx.module, *span, "`this` 只能在方法内使用");
                Ty::Unknown
            }
        },
        Expression::Unary { op, operand, span } => check_unary(sema, ctx, *op, operand, *span),
        Expression::Await { expr, span } => {
            if !ctx.in_async {
                sema.error(ctx.module, *span, "`await` 只能在 async 函数内使用");
            }
            // Promise 类型属后续期，本期透传操作数类型
            check_expr(sema, ctx, expr)
        }
        Expression::Binary {
            op,
            left,
            right,
            span,
        } => check_binary(sema, ctx, *op, left, right, *span),
        Expression::Is { operand, ty, .. } => {
            check_expr(sema, ctx, operand);
            sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
            Ty::Boolean
        }
        Expression::Cast { expr, ty, span } => check_cast(sema, ctx, expr, ty, *span),
        Expression::StructInit {
            name,
            properties,
            span,
        } => check_struct_init(sema, ctx, name, properties, *span),
        Expression::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            expect_boolean(sema, ctx, condition, "三元表达式的条件");
            let then_ty = check_expr(sema, ctx, then_expr);
            let else_ty = check_expr(sema, ctx, else_expr);
            unify_types(&then_ty, &else_ty).unwrap_or_else(|| {
                sema.error(
                    ctx.module,
                    *span,
                    format!("三元表达式分支类型 `{then_ty}` 与 `{else_ty}` 不一致"),
                );
                Ty::Unknown
            })
        }
        Expression::Assignment {
            target,
            op,
            value,
            span,
        } => check_assignment(sema, ctx, target, *op, value, *span),
        Expression::MemberAccess { target, name, span } => {
            check_member_access(sema, ctx, target, name, *span)
        }
        Expression::IndexAccess {
            target,
            index,
            span,
        } => check_index_access(sema, ctx, target, index, *span),
        Expression::Call {
            callee,
            type_args,
            args,
            span,
        } => check_call(sema, ctx, callee, type_args, args, *span),
        Expression::New {
            target,
            type_args,
            args,
            span,
        } => check_new(sema, ctx, target, type_args, args, *span),
        Expression::MacroCall { args, .. } => {
            // 宏展开属后续期；参数照常检查，返回类型视为未知
            for arg in args {
                check_expr(sema, ctx, arg);
            }
            Ty::Unknown
        }
        Expression::Match(m) => check_match(sema, ctx, &m.subject, &m.cases, m.default.as_ref()),
        Expression::Lambda(lambda) => check_lambda(sema, ctx, lambda),
    }
}

fn check_literal(sema: &mut Sema, ctx: &mut BodyCtx, value: &Literal, span: Span) -> Ty {
    match value {
        Literal::Null => Ty::Null,
        Literal::Boolean(_) => Ty::Boolean,
        Literal::String(raw) => {
            check_fstring(sema, ctx, raw, span);
            Ty::String
        }
        Literal::Number(raw) => classify_number_literal(raw).unwrap_or_else(|| {
            sema.error(ctx.module, span, format!("非法数字字面量 `{raw}`"));
            Ty::Unknown
        }),
    }
}

/// f-string（`f"..."` / `f'...'`）：校验插值表达式的语法与类型。
///
/// 子表达式独立 lex+parse，其内部诊断的 span 是相对于插值文本的，
/// 检查完成后统一平移到源文件坐标（`adjust_diagnostics`）。
fn check_fstring(sema: &mut Sema, ctx: &mut BodyCtx, raw: &str, span: Span) {
    if !raw.starts_with("f\"") && !raw.starts_with("f'") {
        return;
    }
    // raw 形如 f"..."/f'...'（首尾分别为 f 前缀与引号）
    let inner = &raw[2..raw.len().saturating_sub(1)];
    let inner_base = span.start + 2; // 插值文本在源文件中的起始偏移
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        match ch {
            b'\\' => escaped = true,
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'{' if !in_single && !in_double => {
                if bytes.get(i + 1) == Some(&b'{') {
                    i += 2; // {{ 转义
                    continue;
                }
                // 找配对的 }（不允许嵌套 {}，插值为单层表达式）
                let mut j = i + 1;
                let mut inner_single = false;
                let mut inner_double = false;
                while j < bytes.len() {
                    let c = bytes[j];
                    match c {
                        b'\'' if !inner_double => inner_single = !inner_single,
                        b'"' if !inner_single => inner_double = !inner_double,
                        b'}' if !inner_single && !inner_double => break,
                        _ => {}
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    sema.error(ctx.module, span, "f-string 插值缺少配对的 `}`");
                    return;
                }
                let text = &inner[i + 1..j];
                let expr_span = Span {
                    start: inner_base + i,
                    end: inner_base + j + 1,
                };
                check_fstring_interpolation(sema, ctx, text, expr_span);
                i = j + 1;
            }
            _ => i += 1,
        }
    }
}

/// 检查单个 f-string 插值 `{expr}`：独立 lex+parse 后类型检查。
fn check_fstring_interpolation(sema: &mut Sema, ctx: &mut BodyCtx, text: &str, span: Span) {
    let text = text.trim();
    if text.is_empty() {
        sema.error(ctx.module, span, "f-string 插值不能为空");
        return;
    }
    let lex_result = novella_lexer::lexer(text);
    if !lex_result.errors.is_empty() {
        sema.error(ctx.module, span, "f-string 插值存在词法错误");
        return;
    }
    let tokens: Vec<_> = lex_result
        .tokens
        .into_iter()
        .filter(|t| t.kind != novella_core::token::TokenKind::LineComment)
        .collect();
    let mut parser = novella_parser::Parser::new(tokens);
    let program = parser.parse();
    if !program.diagnostics.is_empty() {
        sema.error(ctx.module, span, "f-string 插值不是合法表达式");
        return;
    }
    // 只接受单个表达式语句
    let expression = match program.program.items.as_slice() {
        [novella_ast::program::Item::Statement(stmt)] => match &**stmt {
            Statement::Expression(expr) => expr.clone(),
            _ => {
                sema.error(ctx.module, span, "f-string 插值必须是单个表达式");
                return;
            }
        },
        _ => {
            sema.error(ctx.module, span, "f-string 插值必须是单个表达式");
            return;
        }
    };
    let diag_before = sema.modules[ctx.module].diagnostics.len();
    check_expr(sema, ctx, &expression);
    adjust_diagnostics(sema, ctx.module, diag_before, span.start + 1, text);
}

/// 把 [from_idx..) 的诊断平移：span 加 offset，并按新 span 重算行列。
fn adjust_diagnostics(
    sema: &mut Sema,
    module: ModuleId,
    from_idx: usize,
    offset: usize,
    _text: &str,
) {
    let source_len = sema.modules[module].source.len();
    let lines = sema.modules[module].lines.clone();
    for diag in sema.modules[module].diagnostics.iter_mut().skip(from_idx) {
        let Some(location) = &mut diag.location else {
            continue;
        };
        let Some(old_span) = location.span else {
            continue;
        };
        let shifted = Span {
            start: (old_span.start + offset).min(source_len),
            end: (old_span.end + offset).min(source_len),
        };
        let (line, col) = lines.locate(shifted.start);
        location.span = Some(shifted);
        location.line = line;
        location.column = col;
    }
}

/// 标识符：查作用域链，取符号的值类型。
fn check_identifier(sema: &mut Sema, ctx: &mut BodyCtx, name: &str, span: Span) -> Ty {
    let Some(symbol) = sema.scopes.lookup(ctx.scope, name).cloned() else {
        sema.error(ctx.module, span, format!("未定义的标识符 `{name}`"));
        return Ty::Error;
    };
    value_type_of_symbol(sema, ctx, symbol, name, span)
}

/// 符号 → 值类型（供标识符/导入成员访问共用）。
fn value_type_of_symbol(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    symbol: Symbol,
    name: &str,
    span: Span,
) -> Ty {
    match symbol.kind {
        SymbolKind::Variable { ty, .. } => ty.unwrap_or(Ty::Unknown),
        SymbolKind::Param { ty } => ty,
        SymbolKind::Function { sig } => Ty::Function(Rc::new(sig)),
        SymbolKind::NamedImport { target, imported } => match target {
            Some(target_module) => {
                // 沿导入链取目标符号（不支持再导出链，直接取目标模块全局符号）
                match sema.global_scope_symbol(target_module, &imported) {
                    Some(target_symbol) => {
                        value_type_of_symbol(sema, ctx, target_symbol, name, span)
                    }
                    None => Ty::Unknown, // 绑定阶段已报过诊断
                }
            }
            None => Ty::External, // std 外部符号
        },
        SymbolKind::NamespaceImport { .. } => {
            sema.error(
                ctx.module,
                span,
                format!("命名空间 `{name}` 不能作为值使用，请用 `{name}.成员`"),
            );
            Ty::Unknown
        }
        SymbolKind::Type => {
            sema.error(ctx.module, span, format!("类型 `{name}` 不能作为值使用"));
            Ty::Unknown
        }
    }
}

/// 将「类型位置的标识符」解析为注册表键（具名导入转发到目标模块的原始名）。
fn type_key_of_name(sema: &Sema, scope: ScopeId, name: &str) -> Option<(ModuleId, String)> {
    match sema.scopes.lookup(scope, name) {
        Some(Symbol {
            kind: SymbolKind::Type,
            module,
            ..
        }) => Some((*module, name.to_string())),
        Some(Symbol {
            kind:
                SymbolKind::NamedImport {
                    target: Some(target),
                    imported,
                },
            ..
        }) => Some((*target, imported.clone())),
        _ => None,
    }
}

/// 判定标识符指向的符号种类（成员访问/赋值的特殊路径用）。
enum IdentTarget {
    Value,
    Type,
    Namespace(NsTarget),
}

fn classify_identifier(sema: &Sema, ctx: &BodyCtx, name: &str) -> Option<IdentTarget> {
    let symbol = sema.scopes.lookup(ctx.scope, name)?.clone();
    Some(match symbol.kind {
        SymbolKind::Type => IdentTarget::Type,
        SymbolKind::NamespaceImport { target } => IdentTarget::Namespace(target),
        SymbolKind::NamedImport {
            target,
            ref imported,
        } => {
            // 具名导入的类型也可用于静态成员路径
            if let Some(target_module) = target
                && matches!(
                    sema.global_scope_symbol(target_module, imported),
                    Some(Symbol {
                        kind: SymbolKind::Type,
                        ..
                    })
                )
            {
                return Some(IdentTarget::Type);
            }
            IdentTarget::Value
        }
        _ => IdentTarget::Value,
    })
}

/// 带期望类型的表达式检查（当前只对能由标注定型的几种表达式生效，其余回退
/// 到普通 [`check_expr`]）。返回表达式自身的类型，兼容性由调用方判定。
pub(crate) fn check_expr_with_hint(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    expr: &Expression,
    hint: &Ty,
) -> Ty {
    match (expr, hint) {
        // 空数组字面量：元素类型取自标注 `T[]`
        (Expression::ArrayLiteral { elements, .. }, Ty::Array(element)) if elements.is_empty() => {
            Ty::Array(element.clone())
        }
        // `null` → 标注为可空类型时即为该类型
        (
            Expression::Literal {
                value: Literal::Null,
                ..
            },
            hint,
        ) if hint.is_nullable() => hint.clone(),
        _ => check_expr(sema, ctx, expr),
    }
}

fn check_array_literal(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    elements: &[Expression],
    span: Span,
) -> Ty {
    if elements.is_empty() {
        sema.error(
            ctx.module,
            span,
            "空数组字面量无法推断元素类型，请显式写出元素或改用类型标注",
        );
        return Ty::Unknown;
    }
    let mut element_ty = check_expr(sema, ctx, &elements[0]);
    for element in &elements[1..] {
        let ty = check_expr(sema, ctx, element);
        element_ty = match unify_types(&element_ty, &ty) {
            Some(unified) => unified,
            None => {
                sema.error(
                    ctx.module,
                    element.span(),
                    format!("数组元素类型 `{ty}` 与首个元素 `{element_ty}` 不一致"),
                );
                element_ty
            }
        };
    }
    Ty::Array(Box::new(concrete(&element_ty)))
}

/// 对象字面量（追加项 E）：重复键检查 + 简写 `{b}` 展开校验。
fn check_object_literal(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    properties: &[novella_ast::expr::PropertyAssignment],
) -> Ty {
    let mut seen = HashSet::new();
    for property in properties {
        if !seen.insert(property.name.clone()) {
            sema.error(
                ctx.module,
                property.span,
                format!("对象字面量的键 `{}` 重复", property.name),
            );
        }
        match &property.value {
            Some(value) => {
                check_expr(sema, ctx, value);
            }
            None => {
                // 简写 {b} ≡ {b: b}：校验标识符存在且是值
                if sema.scopes.lookup(ctx.scope, &property.name).is_none() {
                    sema.error(
                        ctx.module,
                        property.span,
                        format!("对象简写键 `{}` 没有可展开的同名变量", property.name),
                    );
                }
            }
        }
    }
    // 匿名对象的结构类型属后续期
    Ty::Unknown
}

fn check_unary(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: UnaryOp,
    operand: &Expression,
    span: Span,
) -> Ty {
    let operand_ty = check_expr(sema, ctx, operand);
    match op {
        UnaryOp::Plus | UnaryOp::Minus => {
            if operand_ty.is_numeric() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                Ty::Unknown
            } else {
                sema.error(
                    ctx.module,
                    span,
                    format!(
                        "一元 `{}` 只能用于数值类型，实为 `{operand_ty}`",
                        unary_op_text(op)
                    ),
                );
                Ty::Unknown
            }
        }
        UnaryOp::Not => {
            if !matches!(operand_ty, Ty::Boolean) && !operand_ty.is_opaque() {
                sema.error(
                    ctx.module,
                    span,
                    format!("`not` 只能用于 `boolean`，实为 `{operand_ty}`"),
                );
            }
            Ty::Boolean
        }
        UnaryOp::BitNot => {
            if operand_ty.is_integer() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                Ty::Unknown
            } else {
                sema.error(
                    ctx.module,
                    span,
                    format!("`~` 只能用于整数类型，实为 `{operand_ty}`"),
                );
                Ty::Unknown
            }
        }
        UnaryOp::PostIncrement | UnaryOp::PostDecrement => {
            if !is_lvalue(sema, ctx, operand) {
                sema.error(ctx.module, operand.span(), "自增/自减需要可写的左值");
            }
            if operand_ty.is_numeric() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                Ty::Unknown
            } else {
                sema.error(
                    ctx.module,
                    span,
                    format!("自增/自减只能用于数值类型，实为 `{operand_ty}`"),
                );
                Ty::Unknown
            }
        }
        UnaryOp::AddressOf => {
            // 取址是安全操作（design.md §6），但需要左值；
            // 操作数类型未知时错误已报过，跳过左值检查避免级联
            if !operand_ty.is_opaque() && !is_lvalue(sema, ctx, operand) {
                sema.error(ctx.module, operand.span(), "`&` 取址需要左值");
            }
            Ty::Pointer(Box::new(operand_ty))
        }
        UnaryOp::DerefPointer => {
            // S2：解引用必须 unsafe
            require_unsafe(sema, ctx, span, "指针解引用");
            match operand_ty {
                Ty::Pointer(inner) => *inner,
                Ty::Null => {
                    sema.error(ctx.module, span, "不能解引用 `null` 字面量");
                    Ty::Unknown
                }
                ty if ty.is_opaque() => Ty::Unknown,
                ty => {
                    sema.error(ctx.module, span, format!("只能解引用指针，实为 `{ty}`"));
                    Ty::Unknown
                }
            }
        }
    }
}

/// S10：指针类操作需要 `unsafe {}`。
fn require_unsafe(sema: &mut Sema, ctx: &BodyCtx, span: Span, what: &str) {
    if !ctx.in_unsafe {
        sema.error(
            ctx.module,
            span,
            format!("{what}是不安全操作，需要 `unsafe {{ }}` 块"),
        );
    }
}

fn check_binary(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: BinaryOp,
    left: &Expression,
    right: &Expression,
    span: Span,
) -> Ty {
    let lhs = check_expr(sema, ctx, left);
    let rhs = check_expr(sema, ctx, right);
    // 一侧不可判定时不做检查（可恢复原则）
    if lhs.is_opaque() || rhs.is_opaque() {
        return match op {
            BinaryOp::LogicalAnd
            | BinaryOp::LogicalOr
            | BinaryOp::Equals
            | BinaryOp::NotEquals
            | BinaryOp::IdentityEquals
            | BinaryOp::IdentityNotEquals
            | BinaryOp::LessThan
            | BinaryOp::LessThanEquals
            | BinaryOp::GreaterThan
            | BinaryOp::GreaterThanEquals => Ty::Boolean,
            _ => Ty::Unknown,
        };
    }
    match op {
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
            if !matches!(lhs, Ty::Boolean) || !matches!(rhs, Ty::Boolean) {
                sema.error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的两个操作数都必须是 `boolean`（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
            }
            Ty::Boolean
        }
        BinaryOp::NullCoalesce => {
            if !lhs.is_nullable() {
                sema.error(
                    ctx.module,
                    left.span(),
                    format!("`??` 的左操作数必须可空，实为 `{lhs}`"),
                );
            }
            // 结果类型 = 左操作数去空；无法表达时退化为右操作数类型
            match &lhs {
                Ty::Pointer(inner) => (**inner).clone(),
                Ty::Class(..) => lhs,
                Ty::Null => rhs,
                _ => rhs,
            }
        }
        BinaryOp::Equals
        | BinaryOp::NotEquals
        | BinaryOp::IdentityEquals
        | BinaryOp::IdentityNotEquals => {
            // §8.5：struct 不允许 ==
            for (side, ty) in [("左", &lhs), ("右", &rhs)] {
                if matches!(ty, Ty::Struct(..)) {
                    sema.error(
                        ctx.module,
                        span,
                        format!("struct 类型不支持 `==`/`!=` 比较（{side}操作数为 `{ty}`）"),
                    );
                }
            }
            if !comparable(&lhs, &rhs) {
                sema.error(ctx.module, span, format!("`{lhs}` 与 `{rhs}` 不可比较"));
            }
            Ty::Boolean
        }
        BinaryOp::LessThan
        | BinaryOp::LessThanEquals
        | BinaryOp::GreaterThan
        | BinaryOp::GreaterThanEquals => {
            let ok = (lhs.is_numeric() && rhs.is_numeric() && unify_numeric(&lhs, &rhs).is_some())
                || (matches!(lhs, Ty::String) && matches!(rhs, Ty::String));
            if !ok {
                sema.error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的操作数类型不兼容（`{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
            }
            Ty::Boolean
        }
        BinaryOp::Plus => {
            if matches!(lhs, Ty::String) && matches!(rhs, Ty::String) {
                return Ty::String;
            }
            check_arithmetic(sema, ctx, op, &lhs, &rhs, span)
        }
        BinaryOp::Minus | BinaryOp::Multiply | BinaryOp::Divide => {
            check_arithmetic(sema, ctx, op, &lhs, &rhs, span)
        }
        BinaryOp::Modulus => match unify_numeric(&lhs, &rhs) {
            Some(ty) if ty.is_integer() => ty,
            Some(_) => {
                sema.error(ctx.module, span, "`%` 只能用于整数类型");
                Ty::Unknown
            }
            None => {
                sema.error(
                    ctx.module,
                    span,
                    format!("`%` 的操作数类型不兼容（`{lhs}` 与 `{rhs}`）"),
                );
                Ty::Unknown
            }
        },
        BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr => {
            if lhs.is_integer() && rhs.is_integer() {
                match unify_numeric(&lhs, &rhs) {
                    Some(ty) if ty.is_integer() => ty,
                    _ => {
                        sema.error(
                            ctx.module,
                            span,
                            format!(
                                "`{}` 的整数操作数位宽不一致（`{lhs}` 与 `{rhs}`）",
                                binary_op_text(op)
                            ),
                        );
                        Ty::Unknown
                    }
                }
            } else {
                sema.error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 只能用于整数类型（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
                Ty::Unknown
            }
        }
        BinaryOp::LeftShift | BinaryOp::RightShift => {
            if !lhs.is_integer() || !rhs.is_integer() {
                sema.error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的两个操作数都必须是整数（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
                Ty::Unknown
            } else {
                concrete(&lhs)
            }
        }
    }
}

/// 加减乘除：数值同型运算 + S7 指针算术。
fn check_arithmetic(
    sema: &mut Sema,
    ctx: &BodyCtx,
    op: BinaryOp,
    lhs: &Ty,
    rhs: &Ty,
    span: Span,
) -> Ty {
    // S7：指针 ± 整数 → 同型指针（unsafe）
    if matches!(op, BinaryOp::Plus | BinaryOp::Minus) {
        if let (Ty::Pointer(_), b) = (lhs, rhs)
            && b.is_integer()
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return lhs.clone();
        }
        if matches!(op, BinaryOp::Plus)
            && let (Ty::Pointer(_), a) = (rhs, lhs)
            && a.is_integer()
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return rhs.clone();
        }
        // 指针 - 指针 → isize（S7，要求同 pointee）
        if matches!(op, BinaryOp::Minus)
            && let (Ty::Pointer(a), Ty::Pointer(b)) = (lhs, rhs)
            && (ty_eq(a, b) || matches!(**a, Ty::Void) || matches!(**b, Ty::Void))
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return Ty::Int(crate::ty::IntKind::Isize);
        }
    }
    match unify_numeric(lhs, rhs) {
        Some(ty) => ty,
        None => {
            sema.error(
                ctx.module,
                span,
                format!(
                    "`{}` 的操作数类型不兼容（`{lhs}` 与 `{rhs}`）",
                    binary_op_text(op)
                ),
            );
            Ty::Unknown
        }
    }
}

/// 数值统一：字面量折叠 + 同 kind 检查。不做隐式数值转换。
fn unify_numeric(a: &Ty, b: &Ty) -> Option<Ty> {
    match (a, b) {
        (Ty::IntLiteral, Ty::Int(k)) | (Ty::Int(k), Ty::IntLiteral) => Some(Ty::Int(*k)),
        (Ty::IntLiteral, Ty::IntLiteral) => Some(Ty::IntLiteral),
        (Ty::FloatLiteral, Ty::Float(k)) | (Ty::Float(k), Ty::FloatLiteral) => Some(Ty::Float(*k)),
        (Ty::FloatLiteral, Ty::FloatLiteral) => Some(Ty::FloatLiteral),
        (Ty::Int(k1), Ty::Int(k2)) if k1 == k2 => Some(Ty::Int(*k1)),
        (Ty::Float(k1), Ty::Float(k2)) if k1 == k2 => Some(Ty::Float(*k1)),
        _ => None,
    }
}

/// 相等比较的可比性：同型 / 字面量折叠 / null 与可空。
fn comparable(a: &Ty, b: &Ty) -> bool {
    if a.is_opaque() || b.is_opaque() {
        return true;
    }
    if ty_eq(a, b) || unify_numeric(a, b).is_some() {
        return true;
    }
    (matches!(a, Ty::Null) && b.is_nullable()) || (matches!(b, Ty::Null) && a.is_nullable())
}

/// 通用类型合一（三元/数组元素用）：等价 / 数值统一 / 单向可赋值。
fn unify_types(a: &Ty, b: &Ty) -> Option<Ty> {
    if a.is_opaque() {
        return Some(b.clone());
    }
    if b.is_opaque() {
        return Some(a.clone());
    }
    if ty_eq(a, b) {
        return Some(a.clone());
    }
    if let Some(ty) = unify_numeric(a, b) {
        return Some(ty);
    }
    if matches!(a, Ty::Null) && b.is_nullable() {
        return Some(b.clone());
    }
    if matches!(b, Ty::Null) && a.is_nullable() {
        return Some(a.clone());
    }
    None
}

/// `as` 显式转换（design.md §6.3）：数值互转安全；涉指针的转换 unsafe。
fn check_cast(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    expr: &Expression,
    ty: &TypeExpr,
    span: Span,
) -> Ty {
    let source = check_expr(sema, ctx, expr);
    let target = sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
    if source.is_opaque() || target.is_opaque() {
        return target;
    }
    // 数值互转（含 int→float）本身安全；null 可显式转任意可空类型
    let ok = if ty_eq(&source, &target)
        || (source.is_numeric() && target.is_numeric())
        || (matches!(source, Ty::Null) && target.is_nullable())
    {
        true
    } else if (matches!(target, Ty::Pointer(_))
        && (matches!(source, Ty::Pointer(_)) || source.is_integer()))
        || (matches!(source, Ty::Pointer(_)) && target.is_integer())
    {
        require_unsafe(sema, ctx, span, "指针显式转换");
        true
    } else if let (Ty::Class(..), Ty::Class(..)) = (&source, &target) {
        if sema.is_subclass(&source, &target) {
            true // 向上转型（实际可隐式）
        } else if sema.is_subclass(&target, &source) {
            require_unsafe(sema, ctx, span, "向下转型");
            true
        } else {
            false
        }
    } else {
        false
    };
    if !ok {
        sema.error(
            ctx.module,
            span,
            format!("无法把 `{source}` 转换为 `{target}`"),
        );
    }
    target
}

/// struct 初始化 `Point{ x: 1 }`：字段存在性/重复/缺失检查。
fn check_struct_init(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    name: &str,
    properties: &[novella_ast::expr::PropertyAssignment],
    span: Span,
) -> Ty {
    let struct_key = match type_key_of_name(sema, ctx.scope, name) {
        Some(key) => key,
        None if sema.scopes.lookup(ctx.scope, name).is_some() => {
            sema.error(
                ctx.module,
                span,
                format!("`{name}` 不是类型，不能用于初始化"),
            );
            return Ty::Unknown;
        }
        None => {
            sema.error(ctx.module, span, format!("未定义的类型 `{name}`"));
            return Ty::Error;
        }
    };
    match sema.type_defs.get(&struct_key) {
        Some(TypeDef::Struct(def)) => {
            let def = def.clone();
            let mut seen = HashSet::new();
            for property in properties {
                if !seen.insert(property.name.clone()) {
                    sema.error(
                        ctx.module,
                        property.span,
                        format!("字段 `{}` 重复初始化", property.name),
                    );
                    continue;
                }
                let Some(field_def) = def.fields.iter().find(|f| f.name == property.name) else {
                    sema.error(
                        ctx.module,
                        property.span,
                        format!("struct `{name}` 没有字段 `{}`", property.name),
                    );
                    continue;
                };
                let value_ty = match &property.value {
                    Some(value) => check_expr(sema, ctx, value),
                    None => {
                        // 简写 {x} ≡ {x: x}
                        check_identifier(sema, ctx, &property.name, property.span)
                    }
                };
                if !sema.is_assignable(&field_def.ty, &value_ty) {
                    sema.error(
                        ctx.module,
                        property.span,
                        format!(
                            "字段 `{}` 需要 `{}`，实为 `{value_ty}`",
                            property.name, field_def.ty
                        ),
                    );
                }
            }
            // 缺失字段（无默认值的必须提供）
            for field_def in &def.fields {
                if !field_def.has_default && !seen.contains(&field_def.name) {
                    sema.error(
                        ctx.module,
                        span,
                        format!("struct `{name}` 的字段 `{}` 未初始化", field_def.name),
                    );
                }
            }
            Ty::Struct(struct_key.0, struct_key.1)
        }
        Some(TypeDef::Class(_)) => {
            sema.error(
                ctx.module,
                span,
                format!("class `{name}` 不能用 `{{...}}` 初始化，请用 `new {name}(...)`"),
            );
            Ty::Unknown
        }
        Some(_) => {
            sema.error(ctx.module, span, format!("`{name}` 不是 struct/class 类型"));
            Ty::Error
        }
        None => Ty::Unknown, // 语法失败的模块，注册阶段已报过
    }
}

/// 赋值：左值检查（追加项 B：const 与导入符号只读）+ 兼容检查。
fn check_assignment(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    op: AssignOp,
    value: &Expression,
    span: Span,
) -> Ty {
    check_lvalue_assignment(sema, ctx, target);
    let target_ty = check_expr(sema, ctx, target);
    let value_ty = check_expr(sema, ctx, value);
    match op {
        AssignOp::Assign => {
            if !sema.is_assignable(&target_ty, &value_ty) {
                sema.error(
                    ctx.module,
                    span,
                    format!("不能把 `{value_ty}` 赋给 `{target_ty}`"),
                );
            }
        }
        _ => {
            // 复合赋值 ≡ target = target <op> value，结果须能赋回
            let result_ty = check_arithmetic_compound(sema, ctx, op, &target_ty, &value_ty, span);
            if !result_ty.is_opaque() && !sema.is_assignable(&target_ty, &result_ty) {
                sema.error(
                    ctx.module,
                    span,
                    format!("复合赋值结果 `{result_ty}` 不能赋回 `{target_ty}`"),
                );
            }
        }
    }
    target_ty
}

/// 复合赋值的运算检查（复用二元运算符矩阵）。
fn check_arithmetic_compound(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: AssignOp,
    lhs: &Ty,
    rhs: &Ty,
    span: Span,
) -> Ty {
    if lhs.is_opaque() || rhs.is_opaque() {
        return Ty::Unknown;
    }
    match op {
        AssignOp::PlusAssign => {
            if matches!(lhs, Ty::String) && matches!(rhs, Ty::String) {
                return Ty::String;
            }
            check_arithmetic(sema, ctx, BinaryOp::Plus, lhs, rhs, span)
        }
        AssignOp::MinusAssign => check_arithmetic(sema, ctx, BinaryOp::Minus, lhs, rhs, span),
        AssignOp::MultiplyAssign => check_arithmetic(sema, ctx, BinaryOp::Multiply, lhs, rhs, span),
        AssignOp::DivideAssign => check_arithmetic(sema, ctx, BinaryOp::Divide, lhs, rhs, span),
        AssignOp::ModulusAssign => check_arithmetic(sema, ctx, BinaryOp::Modulus, lhs, rhs, span),
        AssignOp::BitAndAssign => check_arithmetic(sema, ctx, BinaryOp::BitAnd, lhs, rhs, span),
        AssignOp::BitXorAssign => check_arithmetic(sema, ctx, BinaryOp::BitXor, lhs, rhs, span),
        AssignOp::BitOrAssign => check_arithmetic(sema, ctx, BinaryOp::BitOr, lhs, rhs, span),
        AssignOp::LeftShiftAssign => {
            check_arithmetic(sema, ctx, BinaryOp::LeftShift, lhs, rhs, span)
        }
        AssignOp::RightShiftAssign => {
            check_arithmetic(sema, ctx, BinaryOp::RightShift, lhs, rhs, span)
        }
        AssignOp::Assign => unreachable!("Assign 已在调用点处理"),
    }
}

/// 左值校验：标识符 / 成员访问 / 索引访问 / 解引用为合法目标。
/// 追加项 B：const 变量、具名导入、函数、命名空间不可写。
fn is_lvalue(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> bool {
    match expr {
        Expression::Paren { expr, .. } => is_lvalue(sema, ctx, expr),
        Expression::Identifier { name, .. } => match sema.scopes.lookup(ctx.scope, name) {
            Some(Symbol {
                kind: SymbolKind::Variable { is_const, .. },
                ..
            }) => !is_const,
            Some(Symbol {
                kind: SymbolKind::Param { .. },
                ..
            }) => true,
            _ => false,
        },
        Expression::MemberAccess { target, .. } => {
            // 模块/静态成员的写入不合法（导入符号只读）
            if let Expression::Identifier { name, .. } = &**target
                && matches!(
                    classify_identifier(sema, ctx, name),
                    Some(IdentTarget::Namespace(_) | IdentTarget::Type)
                )
            {
                return false;
            }
            is_lvalue(sema, ctx, target)
        }
        Expression::IndexAccess { target, .. } => is_lvalue(sema, ctx, target),
        // `this` 作为成员访问/索引的基座是左值（直接给 `this` 赋值由
        // check_lvalue_assignment 单独拒绝）
        Expression::This { .. } => true,
        Expression::Unary {
            op: UnaryOp::DerefPointer,
            ..
        } => true,
        _ => false,
    }
}

/// 赋值目标的左值检查并给出诊断。
fn check_lvalue_assignment(sema: &mut Sema, ctx: &mut BodyCtx, target: &Expression) {
    // 先给出针对性诊断
    if let Expression::Identifier { name, span } = target {
        match sema.scopes.lookup(ctx.scope, name) {
            Some(Symbol {
                kind: SymbolKind::Variable { is_const: true, .. },
                ..
            }) => {
                sema.error(
                    ctx.module,
                    *span,
                    format!("不能给 `const` 变量 `{name}` 赋值"),
                );
                return;
            }
            Some(Symbol {
                kind: SymbolKind::NamedImport { .. },
                ..
            }) => {
                sema.error(ctx.module, *span, format!("导入的 `{name}` 是只读的"));
                return;
            }
            Some(Symbol {
                kind: SymbolKind::Function { .. },
                ..
            }) => {
                sema.error(ctx.module, *span, format!("不能给函数 `{name}` 赋值"));
                return;
            }
            _ => {}
        }
    }
    if let Expression::This { span, .. } = target {
        sema.error(ctx.module, *span, "不能给 `this` 赋值");
        return;
    }
    if !is_lvalue(sema, ctx, target) {
        sema.error(ctx.module, target.span(), "赋值目标不是可写的左值");
    }
}

/// 成员访问 `a.b`：模块成员 / 静态成员 / 实例成员（class 沿继承链）/ 穿指针成员。
fn check_member_access(
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
            Some(IdentTarget::Namespace(NsTarget::StdExternal)) => {
                return Ty::External; // std 外部符号不做成员检查
            }
            Some(IdentTarget::Namespace(NsTarget::Module(target_module))) => {
                return match sema.global_scope_symbol(target_module, name) {
                    None => {
                        sema.error(
                            ctx.module,
                            span,
                            format!("模块 `{target_name}` 中没有 `{name}`"),
                        );
                        Ty::Unknown
                    }
                    Some(symbol) => {
                        if !symbol.is_importable() {
                            sema.error(
                                ctx.module,
                                span,
                                format!("`{name}` 在模块 `{target_name}` 中不是 pub"),
                            );
                        }
                        value_type_of_symbol(sema, ctx, symbol, name, span)
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
    let target_ty = check_expr(sema, ctx, target);
    member_type_of(sema, ctx, &target_ty, name, span)
}

/// `Type.member`：仅静态成员（含继承链）。
fn check_static_member(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    type_name: &str,
    name: &str,
    span: Span,
) -> Ty {
    // 还原类型键（普通类型符号在定义模块；具名导入的在目标模块）
    let type_key = type_key_of_name(sema, ctx.scope, type_name);
    let Some(type_key) = type_key else {
        sema.error(ctx.module, span, format!("`{type_name}` 不是类型"));
        return Ty::Unknown;
    };
    match lookup_member_chain(sema, &type_key, name, true) {
        Some(MemberInfo::Field(field)) => {
            let _ = field;
            sema.error(
                ctx.module,
                span,
                format!("字段 `{name}` 不是静态成员（struct/class 字段随实例）"),
            );
            Ty::Unknown
        }
        Some(MemberInfo::Method(method)) => {
            if !method.is_static {
                sema.error(
                    ctx.module,
                    span,
                    format!("方法 `{name}` 不是静态方法，不能通过类型名访问"),
                );
            }
            Ty::Function(Rc::new(method.sig))
        }
        None => {
            sema.error(
                ctx.module,
                span,
                format!("类型 `{type_name}` 没有成员 `{name}`"),
            );
            Ty::Unknown
        }
    }
}

/// 值成员访问：struct/class 字段与方法（class 沿继承链），穿指针成员须 unsafe。
fn member_type_of(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target_ty: &Ty,
    name: &str,
    span: Span,
) -> Ty {
    match target_ty {
        Ty::Struct(..) | Ty::Class(..) => {
            let key = type_key_of(target_ty);
            match lookup_member_chain(sema, &key, name, false) {
                Some(MemberInfo::Field(field)) => field.ty,
                Some(MemberInfo::Method(method)) => Ty::Function(Rc::new(method.sig)),
                None => {
                    sema.error(
                        ctx.module,
                        span,
                        format!("类型 `{target_ty}` 没有成员 `{name}`"),
                    );
                    Ty::Unknown
                }
            }
        }
        Ty::Pointer(inner) => {
            // S3：穿指针的成员访问须 unsafe
            require_unsafe(sema, ctx, span, "通过指针访问成员");
            match &**inner {
                Ty::Struct(..) | Ty::Class(..) => member_type_of(sema, ctx, inner, name, span),
                ty if ty.is_opaque() => Ty::Unknown,
                ty => {
                    sema.error(ctx.module, span, format!("`{ty}` 没有成员 `{name}`"));
                    Ty::Unknown
                }
            }
        }
        ty if ty.is_opaque() => Ty::Unknown,
        ty => {
            sema.error(ctx.module, span, format!("`{ty}` 没有成员 `{name}`"));
            Ty::Unknown
        }
    }
}

fn type_key_of(ty: &Ty) -> (ModuleId, String) {
    match ty {
        Ty::Struct(m, n) | Ty::Class(m, n) => (*m, n.clone()),
        _ => unreachable!("仅 Struct/Class 调用"),
    }
}

enum MemberInfo {
    Field(crate::ty::FieldDef),
    Method(crate::ty::MethodSig),
}

/// 沿 class 继承链查找成员（struct 只看自身；static_only 时只看静态方法）。
fn lookup_member_chain(
    sema: &Sema,
    key: &(ModuleId, String),
    name: &str,
    static_only: bool,
) -> Option<MemberInfo> {
    let mut current = key.clone();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return None; // 继承环（注册阶段已报过诊断）
        }
        match sema.type_defs.get(&current)? {
            TypeDef::Struct(def) => {
                if !static_only && let Some(field) = def.fields.iter().find(|f| f.name == name) {
                    return Some(MemberInfo::Field(field.clone()));
                }
                if let Some(method) = def.methods.get(name)
                    && (!static_only || method.is_static)
                {
                    return Some(MemberInfo::Method(method.clone()));
                }
                return None; // struct 无继承
            }
            TypeDef::Class(def) => {
                if !static_only && let Some(field) = def.fields.iter().find(|f| f.name == name) {
                    return Some(MemberInfo::Field(field.clone()));
                }
                if let Some(method) = def.methods.get(name)
                    && (!static_only || method.is_static)
                {
                    return Some(MemberInfo::Method(method.clone()));
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

/// 索引访问 `a[i]`：数组 / 指针（S4，unsafe）/ 元组（需常量索引，暂不支持）。
fn check_index_access(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    index: &Expression,
    span: Span,
) -> Ty {
    let target_ty = check_expr(sema, ctx, target);
    let index_ty = check_expr(sema, ctx, index);
    if !index_ty.is_integer() && !index_ty.is_opaque() {
        sema.error(
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
            sema.error(
                ctx.module,
                span,
                "元组索引需要常量下标，本期暂不支持（请用解构）",
            );
            Ty::Unknown
        }
        ty if ty.is_opaque() => Ty::Unknown,
        ty => {
            sema.error(ctx.module, span, format!("`{ty}` 不支持下标访问"));
            Ty::Unknown
        }
    }
}

/// 函数调用：签名检查（个数/默认值/逐参兼容）+ E7 extern 调用须 unsafe + 泛型实参 arity。
fn check_call(
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
            sema.error(
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
            sema.error(ctx.module, span, format!("命名空间 `{name}` 不可调用"));
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
            sema.error(ctx.module, span, format!("`{ty}` 不可调用"));
            Ty::Unknown
        }
    }
}

/// 用签名检查一次调用（check_call 与 check_new 共用）。
fn check_call_against_sig(
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
        sema.error(
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
        sema.error(
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
            sema.error(
                ctx.module,
                arg.span(),
                format!("参数 `{}` 需要 `{}`，实为 `{arg_ty}`", param.name, param.ty),
            );
        }
    }
}

/// `new Class(args)`：必须是 class，按 `__init` 检查实参。
fn check_new(
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
        sema.error(ctx.module, span, "`new` 的目标目前只支持类名标识符");
        return Ty::Unknown;
    };
    let type_key = match type_key_of_name(sema, ctx.scope, name) {
        Some(key) => Some(key),
        None if sema.scopes.lookup(ctx.scope, name).is_some() => {
            sema.error(
                ctx.module,
                *name_span,
                format!("`{name}` 不是类型，不能用 `new` 构造"),
            );
            None
        }
        None => {
            sema.error(ctx.module, *name_span, format!("未定义的类型 `{name}`"));
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
                sema.error(
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
                        sema.error(
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
            sema.error(
                ctx.module,
                span,
                format!("struct `{name}` 不能用 `new` 构造，请用 `{name}{{ ... }}`"),
            );
            Ty::Error
        }
        Some(_) => {
            sema.error(
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
fn check_match(
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
                sema.error(
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
                    sema.error(
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
fn check_lambda(sema: &mut Sema, ctx: &mut BodyCtx, lambda: &novella_ast::expr::Lambda) -> Ty {
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
                    sema.error(
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
                        sema.error(
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

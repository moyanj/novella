//! 类型检查（规则索引见 `docs/sema-coverage.md`），按职责分文件：
//!
//! - `mod`（本文件）：检查入口——函数体 / 方法体 / 字段初始化 / 顶层语句，
//!   以及语句序列的检查上下文 [`BodyCtx`]；
//! - `body`：语句与变量声明（`if` / `for` / `while` / 解构绑定、局部符号定义）；
//! - `expr`：表达式检查与字面量值解析（标识符分类、f-string 插值、数组 / 对象字面量）；
//! - `unsafe_rules`：运算符矩阵、赋值兼容、cast、struct 初始化、unsafe 上下文规则
//!   （S1–S10 子集 + extern 调用 E7）、左值检查；
//! - `member`：成员访问 `a.b` 与 `Type.member` 静态成员；
//! - `call`：索引、调用、`new` 构造、`match`、lambda。
//!
//! 全部子模块为同一逻辑单元，只在本 crate 内使用（`pub(crate)` 即可，模块本身私有）。

mod body;
mod call;
mod expr;
mod flow;
mod lambda;
mod matching;
pub(crate) mod member;
mod unsafe_rules;

use novella_ast::expr::Expression;
use novella_ast::program::{FieldMember, FunctionItem, MethodMember};
use novella_ast::stmt::{Block, Statement};

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::{ScopeKind, Symbol, SymbolKind};
use crate::ty::{ClassDef, StructDef, Ty, TypeDef};

use body::{BodyCtx, check_block, check_stmt, concrete, define_local};
use expr::check_expr;

/// 默认实参在声明处检查，按参数顺序只开放此前已经绑定的形参。
fn check_defaults(
    sema: &mut Sema,
    module: ModuleId,
    params: &[novella_ast::types::Param],
    sig: &crate::ty::FunctionSig,
    this_ty: Option<Ty>,
) {
    let global = sema.modules[module].global_scope;
    let scope = sema.scopes.new_scope(Some(global), ScopeKind::Function);
    let mut ctx = BodyCtx::base(module, scope);
    ctx.this_ty = this_ty;
    for id in &sig.type_params {
        ctx.type_env.insert(
            id.split('@').next().unwrap_or(id).to_owned(),
            Ty::TypeParam(id.clone()),
        );
    }
    if let Some(owner) = &ctx.this_ty
        && let Some(def) = sema.type_defs.get(&this_ty_key(owner))
    {
        for id in def.type_params() {
            ctx.type_env
                .entry(id.split('@').next().unwrap_or(id).to_owned())
                .or_insert(Ty::TypeParam(id.clone()));
        }
    }
    for (param, resolved) in params.iter().zip(&sig.params) {
        if let Some(expr) = &param.default {
            let actual = expr::check_expr_with_hint(sema, &mut ctx, expr, &resolved.ty);
            if !sema.is_assignable(&resolved.ty, &actual) {
                sema.type_error(
                    module,
                    expr.span(),
                    format!("默认值需要 `{}`，实为 `{actual}`", resolved.ty),
                );
            }
        }
        define_local(
            sema,
            &ctx,
            &param.name,
            SymbolKind::Param {
                ty: resolved.ty.clone(),
            },
            param.span,
        );
    }
}

pub(crate) fn check_function_defaults(sema: &mut Sema, module: ModuleId, function: &FunctionItem) {
    let global = sema.modules[module].global_scope;
    if let Some(Symbol {
        kind: SymbolKind::Function { sig },
        ..
    }) = sema.scopes.lookup_local(global, &function.name).cloned()
        && let Some(ty) = &function.r#type
    {
        check_defaults(sema, module, &ty.params, &sig, None);
    }
}

pub(crate) fn check_method_defaults(
    sema: &mut Sema,
    module: ModuleId,
    owner: &Ty,
    method: &MethodMember,
) {
    if let Some(sig) = lookup_method_def(sema, owner, &method.name) {
        check_defaults(
            sema,
            module,
            &method.params,
            &sig.sig,
            (!method.modifiers.is_static).then(|| owner.clone()),
        );
    }
}

// ---------- 入口：函数体 / 方法体 / 字段初始化 / 顶层语句 ----------
/// 检查顶层函数体。
pub(crate) fn check_function_body(
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
            ctx.type_env.insert(
                param.name.clone(),
                Ty::TypeParam(crate::ty::param_key(module, param)),
            );
        }
    }
    // 重定义已报错时，注册签名可能来自另一条声明；仅配对实际存在的形参，
    // 不用签名下标强行索引当前 AST，避免错误恢复路径 panic。
    if let Some(syntax) = &function.r#type {
        for (syntax, param) in syntax.params.iter().zip(&sig.params) {
            define_local(
                sema,
                &ctx,
                &syntax.name,
                SymbolKind::Param {
                    ty: param.ty.clone(),
                },
                syntax.span,
            );
        }
    }
    check_stmt(sema, &mut ctx, block);
    flow::check_statement(sema, module, &sig.ret, block);
}
/// 检查方法体（`this` 绑定为属主类型）。
pub(crate) fn check_method_body(
    sema: &mut Sema,
    module: ModuleId,
    this_ty: &Ty,
    method: &MethodMember,
    block: &Block,
) {
    let global = sema.modules[module].global_scope;
    let fn_scope = sema.scopes.new_scope(Some(global), ScopeKind::Function);
    let mut ctx = BodyCtx::base(module, fn_scope);
    ctx.this_ty = (!method.modifiers.is_static).then(|| this_ty.clone());
    if method.name == "__destroy" && matches!(this_ty, Ty::Class(..)) {
        ctx.destructor_self = method.params.first().map(|param| param.span);
    }
    for param in &method.type_parameters {
        ctx.type_env.insert(
            param.name.clone(),
            Ty::TypeParam(crate::ty::param_key(module, param)),
        );
    }
    // 属主类型的泛型形参同样可见（class<T> 的方法内可用 T）
    if let Some(def) = sema
        .type_defs
        .get(&(this_ty_key(this_ty).0, this_ty_key(this_ty).1))
    {
        for id in def.type_params() {
            ctx.type_env
                .entry(id.split('@').next().unwrap_or(id).to_owned())
                .or_insert(Ty::TypeParam(id.clone()));
        }
    }
    // 从注册表读取方法签名以绑定形参
    let method_sig = lookup_method_def(sema, this_ty, &method.name).map(|m| m.sig.clone());
    if let Some(sig) = &method_sig {
        ctx.expected_return = Some(sig.ret.clone());
        for (syntax, param) in method.params.iter().zip(&sig.params) {
            define_local(
                sema,
                &ctx,
                &syntax.name,
                SymbolKind::Param {
                    ty: param.ty.clone(),
                },
                syntax.span,
            );
        }
    }

    check_block(sema, &mut ctx, block);
    if let Some(sig) = &method_sig {
        flow::check_block(sema, module, &sig.ret, block);
    }
}
pub(crate) fn this_ty_key(ty: &Ty) -> (ModuleId, String) {
    match ty {
        Ty::Applied(base, _) => this_ty_key(base),
        Ty::Struct(m, n) | Ty::Class(m, n) | Ty::Trait(m, n) => (*m, n.clone()),
        _ => unreachable!("this_ty 恒为 Struct/Class"),
    }
}
pub(crate) fn lookup_method_def(
    sema: &Sema,
    this_ty: &Ty,
    name: &str,
) -> Option<crate::ty::MethodSig> {
    let key = this_ty_key(this_ty);
    match sema.type_defs.get(&key) {
        Some(TypeDef::Struct(StructDef { methods, .. }))
        | Some(TypeDef::Class(ClassDef { methods, .. })) => methods
            .get(crate::check::canonical_method_name(name))
            .cloned(),
        _ => None,
    }
}
/// 检查字段初始化表达式；无标注字段在此推导类型并回填注册表。
pub(crate) fn check_field_initializer(
    sema: &mut Sema,
    module: ModuleId,
    this_ty: &Ty,
    field: &FieldMember,
    initializer: &Expression,
    env: std::collections::HashMap<String, Ty>,
) {
    let global = sema.modules[module].global_scope;
    let scope = sema.scopes.new_scope(Some(global), ScopeKind::Block);
    let mut ctx = BodyCtx::base(module, scope);
    ctx.type_env = env;
    ctx.this_ty = (!field.modifiers.is_static).then(|| this_ty.clone());
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
    let value_ty = if annotated {
        expr::check_expr_with_hint(sema, &mut ctx, initializer, &expected)
    } else {
        check_expr(sema, &mut ctx, initializer)
    };
    if !annotated {
        // 无标注：推导回填（字面量归一化为默认类型）
        let inferred = concrete(&value_ty);
        if !sema.is_assignable(&inferred, &value_ty) {
            sema.type_error(
                module,
                field.span,
                format!("字段初始值 `{value_ty}` 超出默认类型 `{inferred}` 的范围"),
            );
        }
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
        sema.type_error(
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
pub(crate) fn register_global_annotation(
    sema: &mut Sema,
    module: ModuleId,
    declaration: &novella_ast::stmt::VariableStmt,
) {
    let scope = sema.modules[module].global_scope;
    let mut ctx = BodyCtx::base(module, scope);
    ctx.fill_existing = true;
    let ty = sema.resolve_type(
        module,
        scope,
        declaration.ty.as_ref().expect("有注解声明"),
        &ctx.type_env,
    );
    body::bind_pattern_typed(
        sema,
        &mut ctx,
        &declaration.pattern,
        &ty,
        matches!(declaration.r#type, novella_ast::stmt::VariableType::Const),
    );
}

/// 检查顶层语句（变量声明回填全局符号类型；`return` 非法）。
pub(crate) fn check_toplevel_statement(sema: &mut Sema, module: ModuleId, stmt: &Statement) {
    let global = sema.modules[module].global_scope;
    let mut ctx = BodyCtx::base(module, global);
    ctx.fill_existing = true;
    check_stmt(sema, &mut ctx, stmt);
}

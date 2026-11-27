//! Phase 2d 声明级检查：main 入口、struct / class 命名规则、extern 规则 E2–E5，
//! 并驱动函数体 / 顶层语句的类型检查。

use novella_ast::program::{ClassMember, Item};
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::ty::Ty;
use crate::typecheck;

/// 对一个模块执行声明级检查并驱动全部函数体 / 顶层语句的类型检查。
pub(crate) fn check_module(sema: &mut Sema, module: ModuleId) {
    if sema.modules[module].syntax_failed {
        return;
    }
    let items = sema.modules[module].program.items.clone();

    // main 入口检查（仅入口模块，design.md §4.8.1）
    if module == 0 && !sema.module_only && !sema.no_main {
        check_main_entry(sema, &items);
    }

    for item in &items {
        let directives = match item {
            Item::Function(f) => f.directives.as_slice(),
            Item::Class(c) => c.directives.as_slice(),
            Item::Struct(s) => s.directives.as_slice(),
            Item::Trait(t) => t.directives.as_slice(),
            _ => &[],
        };
        for directive in directives {
            if !matches!(item, Item::Function(_)) || directive.name != "export" {
                sema.type_error(module, directive.span, "不合理的编译器指令位置");
            }
        }
        match item {
            Item::Function(function) => {
                typecheck::check_function_defaults(sema, module, function);
                if !super::returns::resolve(sema, module, &function.name)
                    && let Some(block) = &function.block
                {
                    typecheck::check_function_body(sema, module, function, block);
                }
            }
            Item::Class(class) => {
                let this_ty = Ty::Class(module, class.name.clone());
                for member in &class.members {
                    check_member_body(sema, module, member, &this_ty);
                }
            }
            Item::Struct(struct_item) => {
                let this_ty = Ty::Struct(module, struct_item.name.clone());
                for member in &struct_item.members {
                    check_member_body(sema, module, member, &this_ty);
                }
            }
            Item::Statement(stmt) => {
                if !matches!(
                    &**stmt,
                    novella_ast::stmt::Statement::Variable(_)
                        | novella_ast::stmt::Statement::VariableList { .. }
                ) {
                    typecheck::check_toplevel_statement(sema, module, stmt);
                }
            }
            // 指令块：@cfg 已在 cfg 阶段消费，其余指令的处理属后续期，块内跳过检查并提示
            Item::DirectiveBlock {
                directive, span, ..
            } => {
                let message = match directive.name.as_str() {
                    "no_std" => "@no_std 不能用于指令块，请在入口模块中使用 @no_std;",
                    "no_alloc" => "@no_alloc 不能用于指令块，请在入口模块中使用 @no_alloc;",
                    _ => "该编译器指令尚未实现，不能完成语义检查",
                };
                sema.type_error(module, *span, message);
            }
            Item::Directive(directive)
                if matches!(directive.name.as_str(), "no_main" | "no_std" | "no_alloc") => {}
            Item::Directive(directive) => {
                sema.type_error(module, directive.span, "该编译器指令尚未实现")
            }
            _ => {}
        }
    }
}

/// 字段初始化 / 方法体检查。
fn check_member_body(sema: &mut Sema, module: ModuleId, member: &ClassMember, this_ty: &Ty) {
    let directives = match member {
        ClassMember::Field(f) => &f.directives,
        ClassMember::Method(m) => &m.directives,
    };
    for directive in directives {
        sema.type_error(module, directive.span, "成员上的编译器指令尚未实现");
    }
    match member {
        ClassMember::Field(_) => {} // 已在字段定型阶段检查，不能再次登记初值。
        ClassMember::Method(method) => {
            typecheck::check_method_defaults(sema, module, this_ty, method);
            let (_, owner) = typecheck::this_ty_key(this_ty);
            if !super::returns::resolve(sema, module, &format!("{owner}::{}", method.name))
                && let Some(block) = &method.block
            {
                typecheck::check_method_body(sema, module, this_ty, method, block);
            }
        }
    }
}

/// main 入口检查：顶层 `func main`、无参、无泛型、返回 `void` / `int`、非 extern。
fn check_main_entry(sema: &mut Sema, items: &[Item]) {
    let main = items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "main" => Some(function),
        _ => None,
    });
    let Some(main) = main else {
        sema.decl_error(
            0,
            Span { start: 0, end: 0 },
            "缺少程序入口：顶层 `func main`",
        );
        return;
    };
    if main.is_extern {
        return; // E5 已报「`main` 不能声明为 extern」
    }
    if let Some(function_type) = &main.r#type {
        if !function_type.params.is_empty() {
            sema.decl_error(0, main.span, "`main` 不能接受参数");
        }
        if !function_type.type_parameters.is_empty() {
            sema.decl_error(0, main.span, "`main` 不能带泛型参数");
        }
        super::returns::resolve(sema, 0, &main.name);
        let ret = sema
            .global_scope_symbol(0, &main.name)
            .and_then(|symbol| match symbol.kind {
                crate::symbol::SymbolKind::Function { sig } => Some(sig.ret),
                _ => None,
            })
            .unwrap_or(Ty::Error);
        // design.md §4.8.1：退出码语义只定义在 `void` / `int` 上；`int` 已归一化为 i32，
        // 别名（`type Code = int;`）经 expand_alias 展开后同样命中，其余整数宽度一律拒绝。
        if !is_main_return_type(&ret) {
            sema.decl_error(
                0,
                main.span,
                format!("`main` 的返回类型必须是 `void` 或 `int`，实为 `{ret}`"),
            );
        }
    }
    if main.is_async {
        sema.decl_error(0, main.span, "`main` 不能声明为 async");
    }
}

/// `main` 允许的返回类型：`void` 或 `int`（≡ `i32`，design.md §4.8.1 / §5.2）。
///
/// 不用 `Ty::Int(_)` 通配——那样 `i64` / `u8` / `usize` 也会被放行，而退出码
/// 语义只定义在 32 位有符号整数上；`Ty::Unknown`（已由别处报错）放行不追加诊断。
fn is_main_return_type(ty: &Ty) -> bool {
    match ty {
        Ty::Void => true,
        Ty::Int(kind) => *kind == crate::ty::IntKind::I32,
        Ty::Unknown | Ty::Error => true,
        _ => false,
    }
}

//! Phase 2d 声明级检查：main 入口、struct / class 命名规则、extern 规则 E2–E5，
//! 并驱动函数体 / 顶层语句的类型检查。

use std::collections::HashMap;

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
    if module == 0 {
        check_main_entry(sema, &items);
    }

    for item in &items {
        match item {
            Item::Function(function) => {
                if let Some(block) = &function.block {
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
            Item::Statement(stmt) => typecheck::check_toplevel_statement(sema, module, stmt),
            // 装饰块：指令解释（@cfg 等）属后续期，块内跳过检查并提示
            Item::DecoratorBlock { span, .. } => {
                sema.warning(module, *span, "装饰块暂不展开，块内代码未做语义检查");
            }
            _ => {}
        }
    }
}

/// 字段初始化 / 方法体检查。
fn check_member_body(sema: &mut Sema, module: ModuleId, member: &ClassMember, this_ty: &Ty) {
    match member {
        ClassMember::Field(field) => {
            if let Some(initializer) = &field.initializer {
                typecheck::check_field_initializer(sema, module, this_ty, field, initializer);
            }
        }
        ClassMember::Method(method) => {
            if let Some(block) = &method.block {
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
        let global_scope = sema.modules[0].global_scope;
        let ret = sema.resolve_type(0, global_scope, &function_type.return_type, &HashMap::new());
        if !matches!(ret, Ty::Void | Ty::Int(_)) {
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

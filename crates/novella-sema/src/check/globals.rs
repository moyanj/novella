//! 顶层变量按声明依赖定型，不重复检查函数体或顶层初始化表达式。

use novella_ast::program::Item;
use novella_ast::stmt::{Statement, VariableStmt};

use crate::{Sema, module::ModuleId, ty::Ty};

#[derive(Clone)]
pub(crate) struct GlobalTask {
    declaration: VariableStmt,
    state: u8,
}

pub(crate) fn collect(sema: &mut Sema) {
    for module in 0..sema.modules.len() {
        if sema.modules[module].syntax_failed {
            continue;
        }
        for item in sema.modules[module].program.items.clone() {
            let Item::Statement(stmt) = item else {
                continue;
            };
            let declarations = match &*stmt {
                Statement::Variable(decl) => vec![decl.clone()],
                Statement::VariableList { declarations, .. } => declarations.clone(),
                _ => continue,
            };
            for declaration in declarations {
                let mut names = Vec::new();
                crate::resolve::collect_pattern_names(&declaration.pattern, &mut names);
                let span = declaration.span;
                for (name, _) in names {
                    sema.global_tasks_by_name.insert((module, name), span);
                }
                sema.global_tasks.insert(
                    (module, span),
                    GlobalTask {
                        declaration,
                        state: 0,
                    },
                );
            }
        }
    }
    // 显式注解先登记，与初值求值分开；有注解的环不属于类型推导环。
    let annotated: Vec<_> = sema
        .global_tasks
        .iter()
        .filter(|(_, task)| task.declaration.ty.is_some())
        .map(|((module, _), task)| (*module, task.declaration.clone()))
        .collect();
    for (module, declaration) in annotated {
        crate::typecheck::register_global_annotation(sema, module, &declaration);
    }
}

pub(crate) fn check_all(sema: &mut Sema) {
    let mut keys: Vec<_> = sema.global_tasks.keys().copied().collect();
    keys.sort_by_key(|(module, span)| (*module, span.start));
    for (module, span) in keys {
        resolve(sema, module, span);
    }
}

pub(crate) fn resolve_name(sema: &mut Sema, module: ModuleId, name: &str) {
    if let Some(span) = sema
        .global_tasks_by_name
        .get(&(module, name.to_owned()))
        .copied()
    {
        resolve(sema, module, span);
    }
}

fn resolve(sema: &mut Sema, module: ModuleId, span: novella_core::Span) {
    let Some(task) = sema.global_tasks.get(&(module, span)).cloned() else {
        return;
    };
    match task.state {
        2 => return,
        1 => {
            sema.type_error(module, span, "顶层变量的类型推导存在循环依赖");
            let mut names = Vec::new();
            crate::resolve::collect_pattern_names(&task.declaration.pattern, &mut names);
            let scope = sema.modules[module].global_scope;
            for (name, _) in names {
                if let Some(symbol) = sema.scopes.lookup_mut(scope, &name)
                    && let crate::symbol::SymbolKind::Variable { ty, .. } = &mut symbol.kind
                {
                    *ty = Some(Ty::Error);
                }
            }
            return;
        }
        _ => {}
    }
    sema.global_tasks
        .get_mut(&(module, span))
        .expect("已收集变量")
        .state = 1;
    let statement = Statement::Variable(task.declaration);
    crate::typecheck::check_toplevel_statement(sema, module, &statement);
    sema.global_tasks
        .get_mut(&(module, span))
        .expect("已收集变量")
        .state = 2;
}

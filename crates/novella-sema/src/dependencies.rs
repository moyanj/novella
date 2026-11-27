//! 发射模块闭包：显式导入必须保留，默认环境只在实际引用时参与代码生成。

use std::collections::HashSet;

use novella_ast::program::Item;

use crate::module::{NsResolution, resolve_import_path};
use crate::ty::Ty;
use crate::{ModuleId, Sema};

pub(crate) fn codegen_modules(sema: &Sema) -> HashSet<ModuleId> {
    let mut edges = vec![HashSet::new(); sema.modules.len()];
    for (module, output) in sema.modules.iter().enumerate() {
        for item in &output.program.items {
            if let Item::Import(import) = item
                && let Ok(NsResolution::Module(target)) = resolve_import_path(sema, module, import)
            {
                edges[module].insert(target);
            }
        }
    }
    for ((module, _), definition) in &sema.references {
        edges[*module].insert(definition.module);
    }
    // 字面量、推导类型和默认实参可能没有名字引用，仍必须保留它们的类型属主。
    for ((module, _), ty) in sema
        .expression_types
        .iter()
        .chain(&sema.annotation_types)
        .chain(&sema.declaration_types)
    {
        type_dependencies(ty, &mut edges[*module]);
    }
    let mut required = HashSet::new();
    let mut pending = if edges.is_empty() { vec![] } else { vec![0] };
    while let Some(module) = pending.pop() {
        if required.insert(module) {
            pending.extend(edges[module].iter().copied());
        }
    }
    required
}

fn type_dependencies(ty: &Ty, targets: &mut HashSet<ModuleId>) {
    match ty {
        Ty::Struct(module, _) | Ty::Class(module, _) | Ty::Trait(module, _) => {
            // `Exception` 根使用 `ModuleId::MAX` 作为编译器内建名义身份，没有对应模块。
            if *module != ModuleId::MAX {
                targets.insert(*module);
            }
        }
        Ty::Pointer(inner) | Ty::Array(inner, _) | Ty::Future(inner) => {
            type_dependencies(inner, targets);
        }
        Ty::Tuple(members) | Ty::Union(members) => {
            for member in members {
                type_dependencies(member, targets);
            }
        }
        Ty::Applied(base, arguments) => {
            type_dependencies(base, targets);
            for argument in arguments {
                type_dependencies(argument, targets);
            }
        }
        Ty::Function(signature) => {
            if signature.name.is_some() {
                targets.insert(signature.module);
            }
            for parameter in &signature.params {
                type_dependencies(&parameter.ty, targets);
            }
            type_dependencies(&signature.ret, targets);
        }
        Ty::Associated {
            receiver,
            trait_ref,
            ..
        } => {
            type_dependencies(receiver, targets);
            type_dependencies(trait_ref, targets);
        }
        Ty::Dynamic {
            trait_ref,
            bindings,
        } => {
            type_dependencies(trait_ref, targets);
            for (_, ty) in bindings {
                type_dependencies(ty, targets);
            }
        }
        Ty::Object(fields) => {
            for ty in fields.values() {
                type_dependencies(ty, targets);
            }
        }
        Ty::Unknown
        | Ty::Void
        | Ty::Null
        | Ty::Boolean
        | Ty::Int(_)
        | Ty::Float(_)
        | Ty::IntLiteral(..)
        | Ty::FloatLiteral(_)
        | Ty::TypeParam(_)
        | Ty::Error => {}
    }
}

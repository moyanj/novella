//! Phase 2b 类型注册表与 Phase 2c class 继承链。
//!
//! - [`build_type_defs`]：解析字段 / 方法 / 函数签名，填充类型注册表；
//! - [`resolve_class_parents`]：注册 class 父类和 trait 关系并检测继承环；
//! - [`check_inherited_signatures`]：返回类型定型后校验覆写和 trait 实现。

use std::collections::{HashMap, HashSet};

use novella_ast::program::{ClassMember, FieldMember, FunctionItem, Item, MethodMember};
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::SymbolKind;
use crate::ty::{FieldDef, FunctionSig, MethodSig, Ty, TypeDef};
use crate::typecheck::member::{MemberInfo, lookup_member_chain};

use super::CLASS_SPECIAL_METHODS;

mod registration;
pub(crate) use registration::*;
mod inheritance;
pub(crate) use inheritance::*;

pub(crate) fn check_value_layout_cycles(sema: &mut Sema) {
    let mut edges = Vec::new();
    for ((module, name), definition) in &sema.type_defs {
        let fields = match definition {
            crate::ty::TypeDef::Struct(definition) => &definition.fields,
            _ => continue,
        };
        for field in fields.iter().filter(|field| !field.is_static) {
            let mut targets = Vec::new();
            collect_value_types(&field.ty, &mut targets);
            for target in targets {
                if matches!(
                    sema.type_defs.get(&target),
                    Some(crate::ty::TypeDef::Struct(_))
                ) {
                    edges.push((*module, name.clone(), target, field.name_span));
                }
            }
        }
    }
    let snapshot = edges.clone();
    for (module, owner, target, span) in edges {
        if reaches(
            &snapshot,
            target,
            (module, owner.clone()),
            &mut HashSet::new(),
        ) {
            sema.type_error(
                module,
                span,
                format!("struct `{owner}` 含直接或间接递归值布局"),
            );
        }
    }
}

fn collect_value_types(ty: &Ty, output: &mut Vec<(ModuleId, String)>) {
    match ty {
        Ty::Struct(module, name) => output.push((*module, name.clone())),
        Ty::Applied(base, _) => collect_value_types(base, output),
        Ty::Array(element, _) => collect_value_types(element, output),
        Ty::Tuple(elements) => {
            for element in elements {
                collect_value_types(element, output);
            }
        }
        _ => {}
    }
}

fn reaches(
    edges: &[(ModuleId, String, (ModuleId, String), Span)],
    from: (ModuleId, String),
    target: (ModuleId, String),
    visited: &mut HashSet<(ModuleId, String)>,
) -> bool {
    if from == target {
        return true;
    }
    if !visited.insert(from.clone()) {
        return false;
    }
    edges
        .iter()
        .filter(|edge| (edge.0, edge.1.clone()) == from)
        .any(|edge| reaches(edges, edge.2.clone(), target.clone(), visited))
}

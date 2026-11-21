//! 字段初值按依赖定型；每个初值只检查一次，避免源码顺序影响方法体。

use std::collections::HashMap;

use novella_ast::program::{ClassMember, FieldMember, Item};

use crate::Sema;
use crate::module::ModuleId;
use crate::ty::{Ty, TypeDef};

pub(crate) type FieldKey = (ModuleId, String, String);

#[derive(Clone)]
pub(crate) struct FieldTask {
    owner: Ty,
    field: FieldMember,
    env: HashMap<String, Ty>,
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Resolving,
    Done,
}

pub(crate) fn check_fields(sema: &mut Sema) {
    let mut keys = Vec::new();
    let mut weak_fields = Vec::new();
    for module in 0..sema.modules.len() {
        if sema.modules[module].syntax_failed {
            continue;
        }
        for item in sema.modules[module].program.items.clone() {
            let (owner, members, params) = match item {
                Item::Class(c) => (Ty::Class(module, c.name), c.members, c.type_parameters),
                Item::Struct(s) => (Ty::Struct(module, s.name), s.members, s.type_parameters),
                _ => continue,
            };
            let (_, name) = crate::typecheck::this_ty_key(&owner);
            let env: HashMap<_, _> = params
                .into_iter()
                .map(|p| {
                    (
                        p.name.clone(),
                        Ty::TypeParam(crate::ty::param_key(module, &p)),
                    )
                })
                .collect();
            for member in members {
                if let ClassMember::Field(field) = member {
                    if field.modifiers.is_weak {
                        weak_fields.push((
                            module,
                            name.clone(),
                            field.name.clone(),
                            field.name_span,
                        ));
                    }
                    if field.initializer.is_none() {
                        continue;
                    }
                    let key = (module, name.clone(), field.name.clone());
                    keys.push(key.clone());
                    sema.field_tasks.entry(key).or_insert(FieldTask {
                        owner: owner.clone(),
                        field,
                        env: env.clone(),
                        state: State::Pending,
                    });
                }
            }
        }
    }
    for key in keys {
        resolve_field(sema, &key);
    }
    // 必须等初值推导结束再检查，避免无注解的 weak 字段绕过类型约束。
    for (module, owner, name, span) in weak_fields {
        let ty = field_type(sema, &(module, owner), &name);
        let base = match &ty {
            Ty::Applied(base, _) => base.as_ref(),
            other => other,
        };
        if !base.is_opaque() && !matches!(base, Ty::Class(..)) {
            sema.type_error(
                module,
                span,
                format!("weak 字段 `{name}` 必须为 class 引用类型，实际为 `{ty}`"),
            );
        }
    }
}

/// 只在字段无标注时沿类型依赖递归；显式标注打断推导依赖环。
pub(crate) fn field_type(sema: &mut Sema, owner: &(ModuleId, String), name: &str) -> Ty {
    let key = (owner.0, owner.1.clone(), name.to_owned());
    if sema
        .field_tasks
        .get(&key)
        .is_some_and(|task| task.field.ty.is_none())
    {
        resolve_field(sema, &key);
    }
    match sema.type_defs.get(owner) {
        Some(TypeDef::Struct(def)) => &def.fields,
        Some(TypeDef::Class(def)) => &def.fields,
        _ => return Ty::Error,
    }
    .iter()
    .find(|field| field.name == name)
    .map_or(Ty::Error, |field| field.ty.clone())
}

fn resolve_field(sema: &mut Sema, key: &FieldKey) {
    let Some(task) = sema.field_tasks.get(key).cloned() else {
        return;
    };
    match task.state {
        State::Done => return,
        State::Resolving => {
            sema.type_error(
                key.0,
                task.field.span,
                format!(
                    "字段 `{}.{}` 的类型推导存在循环依赖，请添加类型标注",
                    key.1, key.2
                ),
            );
            set_field_type(sema, key, Ty::Error);
            return;
        }
        State::Pending => {}
    }
    sema.field_tasks.get_mut(key).expect("字段任务已登记").state = State::Resolving;
    let initializer = task.field.initializer.as_ref().expect("字段任务必须有初值");
    crate::typecheck::check_field_initializer(
        sema,
        key.0,
        &task.owner,
        &task.field,
        initializer,
        task.env,
    );
    sema.field_tasks.get_mut(key).expect("字段任务已登记").state = State::Done;
}

pub(crate) fn set_field_type(sema: &mut Sema, key: &FieldKey, ty: Ty) {
    let fields = match sema.type_defs.get_mut(&(key.0, key.1.clone())) {
        Some(TypeDef::Struct(def)) => &mut def.fields,
        Some(TypeDef::Class(def)) => &mut def.fields,
        _ => return,
    };
    if let Some(field) = fields.iter_mut().find(|field| field.name == key.2) {
        field.ty = ty;
    }
}

//! 无返回注解的函数按依赖检查一次，并将推导结果回填签名与声明类型。
use novella_ast::program::{ClassMember, FunctionItem, Item, MethodMember};
use novella_core::Span;

use crate::{
    Sema,
    module::ModuleId,
    symbol::SymbolKind,
    ty::{Ty, TypeDef},
};

#[derive(Clone)]
enum Body {
    Function(FunctionItem),
    Method { owner: Ty, method: MethodMember },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Resolving,
    Done,
}

#[derive(Clone)]
pub(crate) struct ReturnTask {
    body: Body,
    state: State,
    span: Span,
}

pub(crate) fn collect(sema: &mut Sema) {
    for module in 0..sema.modules.len() {
        if sema.modules[module].syntax_failed {
            continue;
        }
        for item in sema.modules[module].program.items.clone() {
            match item {
                Item::Function(function)
                    if !function.is_extern
                        && function.block.is_some()
                        && function
                            .r#type
                            .as_ref()
                            .is_some_and(|sig| sig.return_type.is_none()) =>
                {
                    let key = (module, function.name.clone());
                    // 重名声明的诊断由收集阶段负责，不覆盖首个定义的任务。
                    sema.return_tasks.entry(key).or_insert(ReturnTask {
                        span: function.span,
                        body: Body::Function(function),
                        state: State::Pending,
                    });
                }
                Item::Struct(item) => {
                    collect_methods(sema, module, Ty::Struct(module, item.name), item.members)
                }
                Item::Class(item) => {
                    collect_methods(sema, module, Ty::Class(module, item.name), item.members)
                }
                _ => {}
            }
        }
    }
}

fn collect_methods(sema: &mut Sema, module: ModuleId, owner: Ty, members: Vec<ClassMember>) {
    let (_, owner_name) = crate::typecheck::this_ty_key(&owner);
    for member in members {
        let ClassMember::Method(method) = member else {
            continue;
        };
        // 特殊方法的返回类型由协议固定，不能由函数体改变。
        if method.return_type.is_some()
            || method.block.is_none()
            || super::CLASS_SPECIAL_METHODS.contains(&super::canonical_method_name(&method.name))
        {
            continue;
        }
        sema.return_tasks
            .entry((module, format!("{owner_name}::{}", method.name)))
            .or_insert(ReturnTask {
                span: method.span,
                body: Body::Method {
                    owner: owner.clone(),
                    method,
                },
                state: State::Pending,
            });
    }
}

pub(crate) fn check_all(sema: &mut Sema) {
    let mut keys: Vec<_> = sema.return_tasks.keys().cloned().collect();
    keys.sort();
    for (module, name) in keys {
        resolve(sema, module, &name);
    }
}

/// 返回是否存在推导任务；声明检查据此避免再次检查已经定型的函数体。
pub(crate) fn resolve(sema: &mut Sema, module: ModuleId, name: &str) -> bool {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        resolve_inner(sema, module, name)
    })
}

fn resolve_inner(sema: &mut Sema, module: ModuleId, name: &str) -> bool {
    let key = (module, name.to_owned());
    let Some(task) = sema.return_tasks.get(&key).cloned() else {
        return false;
    };
    match task.state {
        State::Done => return true,
        State::Resolving => {
            sema.type_error(
                module,
                task.span,
                format!("函数 `{name}` 的返回类型推导存在循环依赖，请显式标注返回类型"),
            );
            set_return(sema, module, name, &task, Ty::Error);
            sema.return_tasks.get_mut(&key).expect("已收集函数").state = State::Done;
            return true;
        }
        State::Pending => {}
    }
    sema.return_tasks.get_mut(&key).expect("已收集函数").state = State::Resolving;
    let ret = match &task.body {
        Body::Function(function) => crate::typecheck::check_function_body(
            sema,
            module,
            function,
            function.block.as_ref().expect("函数体"),
        ),
        Body::Method { owner, method } => crate::typecheck::check_method_body(
            sema,
            module,
            owner,
            method,
            method.block.as_ref().expect("方法体"),
        ),
    };
    // 循环已经固定为 Error，不允许外层返回继续覆盖为貌似成功的类型。
    if sema.return_tasks[&key].state != State::Done {
        set_return(sema, module, name, &task, ret);
        sema.return_tasks.get_mut(&key).expect("已收集函数").state = State::Done;
    }
    true
}

/// 组合类型里的占位同样不能成为推导结果；Error 只传播，Unknown 要求补充注解。
pub(crate) fn validate_type(sema: &mut Sema, module: ModuleId, span: Span, ty: &Ty) -> bool {
    match inference_state(ty) {
        InferenceState::Complete => true,
        InferenceState::Error => false,
        InferenceState::Unknown => {
            sema.type_error(
                module,
                span,
                "无法确定返回值类型，请显式标注返回类型及相关参数类型",
            );
            false
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum InferenceState {
    Complete,
    Unknown,
    Error,
}

fn inference_state(ty: &Ty) -> InferenceState {
    let merge = |types: &[Ty]| {
        types
            .iter()
            .map(inference_state)
            .max()
            .unwrap_or(InferenceState::Complete)
    };
    match ty {
        Ty::Unknown => InferenceState::Unknown,
        Ty::Error => InferenceState::Error,
        Ty::Pointer(inner) | Ty::Array(inner) | Ty::Future(inner) => inference_state(inner),
        Ty::Tuple(types) | Ty::Union(types) => merge(types),
        Ty::Applied(base, args) => inference_state(base).max(merge(args)),
        Ty::Object(fields) => fields
            .values()
            .map(inference_state)
            .max()
            .unwrap_or(InferenceState::Complete),
        Ty::Function(sig) => sig
            .params
            .iter()
            .map(|param| inference_state(&param.ty))
            .chain(std::iter::once(inference_state(&sig.ret)))
            .max()
            .unwrap_or(InferenceState::Complete),
        Ty::Void
        | Ty::Null
        | Ty::Boolean
        | Ty::Int(_)
        | Ty::Float(_)
        | Ty::IntLiteral(..)
        | Ty::FloatLiteral(_)
        | Ty::Struct(..)
        | Ty::Class(..)
        | Ty::Trait(..)
        | Ty::TypeParam(_) => InferenceState::Complete,
    }
}

fn set_return(sema: &mut Sema, module: ModuleId, name: &str, task: &ReturnTask, ret: Ty) {
    let signature = match &task.body {
        Body::Function(_) => {
            let scope = sema.modules[module].global_scope;
            match sema
                .scopes
                .lookup_mut(scope, name)
                .map(|symbol| &mut symbol.kind)
            {
                Some(SymbolKind::Function { sig }) => {
                    sig.ret = ret;
                    Some(sig.clone())
                }
                _ => None,
            }
        }
        Body::Method { owner, method } => {
            let key = crate::typecheck::this_ty_key(owner);
            let methods = match sema.type_defs.get_mut(&key) {
                Some(TypeDef::Struct(def)) => &mut def.methods,
                Some(TypeDef::Class(def)) => &mut def.methods,
                _ => return,
            };
            methods
                .get_mut(super::canonical_method_name(&method.name))
                .map(|method| {
                    method.sig.ret = ret;
                    method.sig.clone()
                })
        }
    };
    if let Some(signature) = signature {
        sema.declaration_types.insert(
            (module, task.span),
            Ty::Function(std::rc::Rc::new(signature)),
        );
    }
}

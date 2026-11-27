//! 在选定 HIR 布局和 ABI 前展开语义阶段提供的泛型代入。
use super::*;
use novella_sema::{FunctionView, TypeSummary};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

pub(super) type Substitution = Vec<(String, TypeView)>;
pub(super) type Instances = Rc<RefCell<InstanceTable>>;

#[derive(Clone)]
pub(super) struct AggregateInstance {
    pub module: ModuleId,
    pub base: String,
    pub name: String,
    pub args: Vec<TypeView>,
    pub is_class: bool,
}
pub(super) struct FunctionInstance {
    pub view: FunctionView,
    pub subst: Substitution,
    pub signature: FunctionSignature,
}
pub(super) enum Instance {
    Aggregate(AggregateInstance),
    Function(FunctionInstance),
}
#[derive(Default)]
pub(super) struct InstanceTable {
    pending: VecDeque<Instance>,
    aggregates: HashMap<(ModuleId, String), Vec<AggregateInstance>>,
    functions: HashMap<(ModuleId, String), Vec<(Substitution, HirFunctionId)>>,
    names: HashMap<(ModuleId, String), AggregateInstance>,
    lambda_environments: Vec<Substitution>,
    next_id: usize,
}
impl InstanceTable {
    pub fn next(&mut self) -> Option<Instance> {
        self.pending.pop_front()
    }
    fn fresh(&mut self) -> Result<usize, BuildError> {
        // 多态递归可能不断产生新的实例；不可让队列无限增长。
        if self.next_id >= (1 << 14) {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "单态化实例数超过工程上限",
            }));
        }
        let id = self.next_id;
        self.next_id += 1;
        Ok(id)
    }
}

pub(super) fn substitute(view: &TypeView, subst: &Substitution) -> TypeView {
    let recurse = |ty: &TypeView| substitute(ty, subst);
    match view {
        TypeView::TypeParam(name) => subst
            .iter()
            .find(|(key, _)| key == name)
            .map_or_else(|| view.clone(), |(_, value)| value.clone()),
        TypeView::Applied(base, args) => {
            TypeView::Applied(Box::new(recurse(base)), args.iter().map(recurse).collect())
        }
        TypeView::Associated {
            receiver,
            trait_ref,
            name,
        } => TypeView::Associated {
            receiver: Box::new(recurse(receiver)),
            trait_ref: Box::new(recurse(trait_ref)),
            name: name.clone(),
        },
        TypeView::Dynamic {
            trait_ref,
            bindings,
        } => TypeView::Dynamic {
            trait_ref: Box::new(recurse(trait_ref)),
            bindings: bindings
                .iter()
                .map(|(name, ty)| (name.clone(), recurse(ty)))
                .collect(),
        },
        TypeView::Future(inner) => TypeView::Future(Box::new(recurse(inner))),
        TypeView::Pointer(inner) => TypeView::Pointer(Box::new(recurse(inner))),
        TypeView::Array(inner, len) => TypeView::Array(Box::new(recurse(inner)), *len),
        TypeView::Tuple(parts) => TypeView::Tuple(parts.iter().map(recurse).collect()),
        TypeView::Union(parts) => TypeView::Union(parts.iter().map(recurse).collect()),
        TypeView::Object(fields) => TypeView::Object(
            fields
                .iter()
                .map(|(key, ty)| (key.clone(), recurse(ty)))
                .collect(),
        ),
        TypeView::Function(view) => {
            let mut result = (**view).clone();
            result.params = view.params.iter().map(recurse).collect();
            result.ret = Box::new(recurse(&view.ret));
            TypeView::Function(Box::new(result))
        }
        _ => view.clone(),
    }
}

pub(super) fn summary<'a>(
    sema: &'a SemaResult,
    module: ModuleId,
    name: &str,
) -> Option<&'a TypeSummary> {
    sema.types
        .summaries
        .iter()
        .find(|summary| summary.module == module && summary.name == name)
}

/// 将布局实例的名字还原为语义声明和定义处代入。
pub(super) fn aggregate_definition<'a>(
    sema: &'a SemaResult,
    instances: &Instances,
    module: ModuleId,
    name: &str,
) -> Option<(&'a TypeSummary, Substitution)> {
    let table = instances.borrow();
    let instance = table.names.get(&(module, name.to_owned()));
    let definition = summary(sema, module, instance.map_or(name, |entry| &entry.base))?;
    let subst = instance.map_or_else(Vec::new, |entry| {
        definition
            .type_parameters
            .iter()
            .cloned()
            .zip(entry.args.iter().cloned())
            .collect()
    });
    Some((definition, subst))
}

pub(super) fn concrete_type(
    sema: &SemaResult,
    instances: &Instances,
    view: &TypeView,
    subst: &Substitution,
    preferred: Option<&HirType>,
) -> Result<HirType, BuildError> {
    let view = native_view(sema, instances, &substitute(view, subst), subst, 0)?;
    HirType::from_view_with(&view, &mut function_id_of, preferred)
}

pub(super) fn native_view(
    sema: &SemaResult,
    instances: &Instances,
    view: &TypeView,
    subst: &Substitution,
    depth: usize,
) -> Result<TypeView, BuildError> {
    if depth > 256 {
        return Err(BuildError::at(BuildErrorKind::Unimplemented {
            node: "单态化类型嵌套超过工程上限",
        }));
    }
    let recurse = |ty: &TypeView| native_view(sema, instances, ty, subst, depth + 1);
    Ok(match view {
        TypeView::Applied(base, args) => {
            let (module, base, is_class) = match base.as_ref() {
                TypeView::Struct(module, name) => (*module, name, false),
                TypeView::Class(module, name) => (*module, name, true),
                TypeView::Trait(..) => {
                    return Ok(TypeView::Applied(
                        base.clone(),
                        args.iter().map(recurse).collect::<Result<_, _>>()?,
                    ));
                }
                _ => return Err(BuildError::at(BuildErrorKind::GenericUnsupported)),
            };
            let args = args.iter().map(recurse).collect::<Result<Vec<_>, _>>()?;
            let name = aggregate_name(instances, module, base, args, is_class)?;
            if is_class {
                TypeView::Class(module, name)
            } else {
                TypeView::Struct(module, name)
            }
        }
        TypeView::Struct(module, name) | TypeView::Class(module, name) => {
            let Some(def) =
                summary(sema, *module, name).filter(|def| !def.type_parameters.is_empty())
            else {
                return Ok(view.clone());
            };
            let args = def
                .type_parameters
                .iter()
                .map(|param| {
                    let ty = subst
                        .iter()
                        .find(|(key, _)| key == param)
                        .map(|(_, ty)| ty)
                        .ok_or_else(|| BuildError::at(BuildErrorKind::GenericUnsupported))?;
                    recurse(ty)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let is_class = matches!(view, TypeView::Class(..));
            let name = aggregate_name(instances, *module, name, args, is_class)?;
            if is_class {
                TypeView::Class(*module, name)
            } else {
                TypeView::Struct(*module, name)
            }
        }
        TypeView::Pointer(inner) => TypeView::Pointer(Box::new(recurse(inner)?)),
        TypeView::Array(inner, len) => TypeView::Array(Box::new(recurse(inner)?), *len),
        TypeView::Tuple(parts) => {
            TypeView::Tuple(parts.iter().map(recurse).collect::<Result<_, _>>()?)
        }
        TypeView::Function(view) => {
            let mut result = (**view).clone();
            result.params = view.params.iter().map(recurse).collect::<Result<_, _>>()?;
            result.ret = Box::new(recurse(&view.ret)?);
            TypeView::Function(Box::new(result))
        }
        TypeView::Dynamic {
            trait_ref,
            bindings,
        } => TypeView::Dynamic {
            trait_ref: Box::new(recurse(trait_ref)?),
            bindings: bindings
                .iter()
                .map(|(name, ty)| Ok((name.clone(), recurse(ty)?)))
                .collect::<Result<_, BuildError>>()?,
        },
        TypeView::Associated {
            receiver,
            trait_ref,
            name,
        } => {
            let receiver = recurse(receiver)?;
            let trait_ref = recurse(trait_ref)?;
            let missing = || {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "关联类型实现",
                })
            };
            if let TypeView::Dynamic { bindings, .. } = &receiver {
                let (_, ty) = bindings
                    .iter()
                    .find(|(key, _)| key == name)
                    .ok_or_else(missing)?;
                return recurse(ty);
            }
            let (implementation, subst) =
                resolve_trait_implementation(sema, instances, &receiver, &trait_ref)?
                    .ok_or_else(missing)?;
            let (_, value) = implementation
                .bindings
                .iter()
                .find(|(key, _)| key == name)
                .ok_or_else(missing)?;
            return recurse(&substitute(value, &subst));
        }
        TypeView::TypeParam(_) => return Err(BuildError::at(BuildErrorKind::GenericUnsupported)),
        _ => view.clone(),
    })
}

fn aggregate_name(
    instances: &Instances,
    module: ModuleId,
    base: &str,
    args: Vec<TypeView>,
    is_class: bool,
) -> Result<String, BuildError> {
    let mut table = instances.borrow_mut();
    let key = (module, base.to_owned());
    if let Some(instance) = table
        .aggregates
        .get(&key)
        .and_then(|entries| entries.iter().find(|entry| entry.args == args))
    {
        return Ok(instance.name.clone());
    }
    let name = format!("{base}#mono{}", table.fresh()?);
    let instance = AggregateInstance {
        module,
        base: base.to_owned(),
        name: name.clone(),
        args,
        is_class,
    };
    table
        .aggregates
        .entry(key)
        .or_default()
        .push(instance.clone());
    table.names.insert((module, name.clone()), instance.clone());
    table.pending.push_back(Instance::Aggregate(instance));
    Ok(name)
}

pub(super) fn concrete_signature(
    sema: &SemaResult,
    instances: &Instances,
    view: &FunctionView,
    subst: &Substitution,
) -> Result<FunctionSignature, BuildError> {
    let ty = concrete_type(
        sema,
        instances,
        &TypeView::Function(Box::new(view.clone())),
        subst,
        None,
    )?;
    let HirType::Function(signature) = ty else {
        unreachable!()
    };
    Ok(*signature)
}

/// 先登记签名，再把函数体放入全局队列，支持递归和跨模块实例化。
pub(super) fn instantiate_function(
    sema: &SemaResult,
    instances: &Instances,
    view: &FunctionView,
    subst: Substitution,
) -> Result<FunctionSignature, BuildError> {
    if subst.is_empty() {
        return concrete_signature(sema, instances, view, &subst);
    }
    let name = view
        .name
        .as_ref()
        .ok_or_else(|| BuildError::at(BuildErrorKind::GenericUnsupported))?;
    let key = (view.module, name.clone());
    let mut view = view.clone();
    view.type_param_count = 0;
    let mut signature = concrete_signature(sema, instances, &view, &subst)?;
    let mut table = instances.borrow_mut();
    if let Some((_, id)) = table
        .functions
        .get(&key)
        .and_then(|entries| entries.iter().find(|(args, _)| args == &subst))
    {
        signature.id = id.clone();
        return Ok(signature);
    }
    signature.id.0.push_str(&format!("#mono{}", table.fresh()?));
    table
        .functions
        .entry(key)
        .or_default()
        .push((subst.clone(), signature.id.clone()));
    table
        .pending
        .push_back(Instance::Function(FunctionInstance {
            view,
            subst,
            signature: signature.clone(),
        }));
    Ok(signature)
}

pub(super) fn concrete_substitution(
    sema: &SemaResult,
    instances: &Instances,
    source: &Substitution,
    ambient: &Substitution,
) -> Result<Substitution, BuildError> {
    let mut subst = source
        .iter()
        .map(|(key, ty)| {
            let ty = substitute(ty, ambient);
            native_view(sema, instances, &ty, ambient, 0).map(|ty| (key.clone(), ty))
        })
        .collect::<Result<Vec<_>, _>>()?;
    subst.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(subst)
}

pub(super) fn lambda_suffix(
    instances: &Instances,
    subst: &Substitution,
) -> Result<String, BuildError> {
    let mut table = instances.borrow_mut();
    let envs = &mut table.lambda_environments;
    let id = envs
        .iter()
        .position(|existing| existing == subst)
        .unwrap_or_else(|| {
            envs.push(subst.clone());
            envs.len() - 1
        });
    Ok(format!("#mono{id}"))
}

pub(super) fn nominal_key(view: &TypeView) -> Option<(ModuleId, &str)> {
    match view {
        TypeView::Applied(base, _) | TypeView::Pointer(base) => nominal_key(base),
        TypeView::Struct(module, name)
        | TypeView::Class(module, name)
        | TypeView::Trait(module, name) => Some((*module, name)),
        _ => None,
    }
}

/// 从最近的 class 父链节点（或者 struct 自身）选择显式实现。
pub(super) fn resolve_trait_implementation<'a>(
    sema: &'a SemaResult,
    instances: &Instances,
    receiver: &TypeView,
    trait_ref: &TypeView,
) -> Result<Option<(&'a novella_sema::TraitImplementation, Substitution)>, BuildError> {
    let receiver = native_view(sema, instances, receiver, &Vec::new(), 0)?;
    let Some((module, name)) = nominal_key(&receiver) else {
        return Ok(None);
    };
    let trait_ref = native_view(sema, instances, trait_ref, &Vec::new(), 0)?;
    let lineage =
        layout::instance_lineage(sema, instances, module, name, Span { start: 0, end: 0 })?;
    for (definition, subst) in lineage.into_iter().rev() {
        for implementation in &sema.trait_implementations {
            if nominal_key(&implementation.receiver)
                != Some((definition.module, definition.name.as_str()))
            {
                continue;
            }
            if nominal_key(&implementation.trait_ref) != nominal_key(&trait_ref) {
                continue;
            }
            let applied = native_view(
                sema,
                instances,
                &substitute(&implementation.trait_ref, &subst),
                &subst,
                0,
            )?;
            if applied == trait_ref {
                return Ok(Some((implementation, subst)));
            }
        }
    }
    Ok(None)
}

fn default_self_key(module: ModuleId, name: &str) -> String {
    format!("Self@trait:m{module}:{name}")
}

/// 默认实参在接口声明环境展开，其 Self 与类型实参取调用点接收者。
pub(super) fn bind_trait_receiver(
    sema: &SemaResult,
    subst: &mut Substitution,
    trait_ref: &TypeView,
    receiver: &TypeView,
) -> Result<(), BuildError> {
    let missing = || {
        BuildError::at(BuildErrorKind::MissingType {
            what: "trait 默认实参定义",
        })
    };
    let (module, name) = nominal_key(trait_ref).ok_or_else(missing)?;
    let definition = summary(sema, module, name).ok_or_else(missing)?;
    let self_key = default_self_key(module, name);
    subst.retain(|(key, _)| *key != self_key && !definition.type_parameters.contains(key));
    subst.push((self_key, receiver.clone()));
    if let TypeView::Applied(_, args) = trait_ref {
        subst.extend(
            definition
                .type_parameters
                .iter()
                .cloned()
                .zip(args.iter().cloned()),
        );
    }
    subst.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(())
}

pub(super) fn method_receiver(
    sema: &SemaResult,
    instances: &Instances,
    module: ModuleId,
    owner: &str,
    subst: &Substitution,
) -> Result<HirType, BuildError> {
    let definition = summary(sema, module, owner).ok_or_else(|| {
        BuildError::at(BuildErrorKind::MissingType {
            what: "方法接收者定义",
        })
    })?;
    let view = match definition.kind {
        novella_sema::TypeSummaryKind::Struct => TypeView::Struct(module, owner.to_owned()),
        novella_sema::TypeSummaryKind::Class => TypeView::Class(module, owner.to_owned()),
        novella_sema::TypeSummaryKind::Trait => {
            TypeView::TypeParam(default_self_key(module, owner))
        }
        novella_sema::TypeSummaryKind::Alias => {
            return Err(BuildError::at(BuildErrorKind::GenericUnsupported));
        }
    };
    concrete_type(sema, instances, &view, subst, None)
}

/// 具名方法的签名和默认方法均以真实定义环境为准；类接收者 ABI 总是引用。
pub(super) fn instantiate_trait_method(
    sema: &SemaResult,
    instances: &Instances,
    implementation: &novella_sema::TraitImplementation,
    impl_subst: &Substitution,
    method: &novella_sema::TraitMethodImplementation,
    method_arguments: &Substitution,
) -> Result<FunctionSignature, BuildError> {
    let missing = || {
        BuildError::at(BuildErrorKind::MissingType {
            what: "trait 方法定义",
        })
    };
    let (owner_name, _) = method.symbol.split_once("::").ok_or_else(missing)?;
    let definition = summary(sema, method.module, owner_name).ok_or_else(missing)?;
    let view = &definition
        .methods
        .iter()
        .find(|candidate| candidate.name == method.name)
        .ok_or_else(missing)?
        .signature;
    let owner = native_view(
        sema,
        instances,
        &substitute(&method.owner, impl_subst),
        impl_subst,
        0,
    )?;
    let mut subst = if method.is_default {
        let mut subst: Substitution = if let TypeView::Applied(_, args) = owner {
            definition
                .type_parameters
                .iter()
                .cloned()
                .zip(args)
                .collect()
        } else {
            Vec::new()
        };
        let receiver = native_view(
            sema,
            instances,
            &substitute(&implementation.receiver, impl_subst),
            impl_subst,
            0,
        )?;
        subst.push((default_self_key(method.module, owner_name), receiver));
        subst
    } else {
        let (module, name) = nominal_key(&owner).ok_or_else(missing)?;
        aggregate_definition(sema, instances, module, name)
            .ok_or_else(missing)?
            .1
    };
    subst.extend(method_arguments.iter().cloned());
    subst.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    instantiate_function(sema, instances, view, subst)
}

/// 实现和接口的形参身份不同；以各自声明顺序映射实参。
pub(super) fn trait_method_arguments(
    sema: &SemaResult,
    trait_view: &FunctionView,
    method: &novella_sema::TraitMethodImplementation,
    arguments: &Substitution,
) -> Result<Substitution, BuildError> {
    let missing = || {
        BuildError::at(BuildErrorKind::MissingType {
            what: "trait 泛型方法实参",
        })
    };
    let (owner, _) = method.symbol.split_once("::").ok_or_else(missing)?;
    let target = &summary(sema, method.module, owner)
        .and_then(|item| {
            item.methods
                .iter()
                .find(|candidate| candidate.name == method.name)
        })
        .ok_or_else(missing)?
        .signature;
    if target.type_parameters.len() != trait_view.type_parameters.len() {
        return Err(missing());
    }
    trait_view
        .type_parameters
        .iter()
        .zip(&target.type_parameters)
        .map(|(source, target)| {
            let (_, ty) = arguments
                .iter()
                .find(|(key, _)| key == source)
                .ok_or_else(missing)?;
            Ok((target.clone(), ty.clone()))
        })
        .collect()
}

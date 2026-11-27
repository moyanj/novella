//! 聚合实例布局：跨模块继承按父类到子类展开，字段索引只有一个来源。

use super::*;

/// 返回从根到具体实例的继承链，并保留每一层声明的代入。
pub(super) fn instance_lineage<'a>(
    sema: &'a SemaResult,
    instances: &Instances,
    module: ModuleId,
    name: &str,
    span: Span,
) -> Result<Vec<(&'a novella_sema::TypeSummary, Substitution)>, BuildError> {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut current = Some((module, name.to_owned()));
    while let Some((owner, ref name)) = current {
        let (summary, subst) = mono::aggregate_definition(sema, instances, owner, name)
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "实例布局"
                })
                .with_site(module, span)
            })?;
        if !visited.insert((owner, summary.name.clone())) {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "cyclic inheritance",
            })
            .with_site(module, span));
        }
        current = instance_parent(sema, instances, summary, &subst)?;
        chain.push((summary, subst));
    }
    chain.reverse();
    Ok(chain)
}

pub(super) fn instance_parent(
    sema: &SemaResult,
    instances: &Instances,
    summary: &novella_sema::TypeSummary,
    subst: &Substitution,
) -> Result<Option<(ModuleId, String)>, BuildError> {
    let Some(parent) = &summary.applied_parent else {
        return Ok(summary.parent.clone());
    };
    let HirType::Class(module, name) = mono::concrete_type(sema, instances, parent, subst, None)?
    else {
        return Err(BuildError::at(BuildErrorKind::MissingType {
            what: "父类布局",
        }));
    };
    Ok(Some((module, name)))
}

pub(super) fn instance_fields(
    sema: &SemaResult,
    instances: &Instances,
    module: ModuleId,
    name: &str,
    span: Span,
) -> Result<Vec<HirField>, BuildError> {
    let mut fields = Vec::new();
    for (summary, subst) in instance_lineage(sema, instances, module, name, span)? {
        for field in &summary.fields {
            if field.is_static {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "static field storage",
                })
                .with_site(module, span));
            }
            let ty = mono::concrete_type(sema, instances, &field.ty, &subst, None)
                .map_err(|error| error.with_site(module, span))?;
            fields.push(HirField {
                name: field.name.clone(),
                ty,
                is_static: false,
                is_weak: field.is_weak,
            });
        }
    }
    Ok(fields)
}

/// 只有内联字段参与循环；类、指针、函数和动态接口的布局都经过间接层。
pub(super) fn validate_value_layouts(
    sema: &SemaResult,
    instances: &Instances,
    modules: &[HirModule],
) -> Result<(), BuildError> {
    let mut structs = HashMap::new();
    for module in modules {
        for ty in &module.types {
            if let HirTypeDef::Struct { name, fields, .. } = ty {
                let id = structs.len();
                structs.insert((module.id, name.as_str()), (id, fields));
            }
        }
    }
    // 状态：0 未访问、1 当前路径、2 已验证。显式栈避免深布局耗尽编译器的调用栈。
    let mut state = vec![0; structs.len()];
    for &root in structs.keys() {
        let mut pending = vec![(root, false)];
        while let Some((key, finished)) = pending.pop() {
            let Some((id, fields)) = structs.get(&key) else {
                continue;
            };
            if finished {
                state[*id] = 2;
                continue;
            }
            match state[*id] {
                2 => continue,
                1 => {
                    let error = BuildError::at(BuildErrorKind::InfiniteValueLayout);
                    if let Some((summary, _)) =
                        mono::aggregate_definition(sema, instances, key.0, key.1)
                    {
                        return Err(error.with_site(key.0, summary.span));
                    }
                    return Err(error);
                }
                _ => {}
            }
            state[*id] = 1;
            pending.push((key, true));
            let mut types: Vec<_> = fields.iter().map(|field| &field.ty).collect();
            while let Some(ty) = types.pop() {
                match ty {
                    HirType::Struct(module, name) => {
                        pending.push(((*module, name.as_str()), false))
                    }
                    HirType::Array(inner, _) => types.push(inner),
                    HirType::Tuple(parts) => types.extend(parts),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

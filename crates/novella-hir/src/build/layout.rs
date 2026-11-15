//! 聚合实例布局：跨模块继承按父类到子类展开，字段索引只有一个来源。

use super::*;

pub(super) fn instance_fields(
    sema: &SemaResult,
    module: ModuleId,
    name: &str,
    span: Span,
) -> Result<Vec<HirField>, BuildError> {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut current = Some((module, name.to_string()));
    while let Some(key) = current {
        if !visited.insert(key.clone()) {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "cyclic inheritance",
            })
            .with_site(module, span));
        }
        let summary = sema
            .types
            .summaries
            .iter()
            .find(|summary| summary.module == key.0 && summary.name == key.1)
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "实例布局"
                })
                .with_site(module, span)
            })?;
        current = summary.parent.clone();
        chain.push(summary);
    }
    let mut fields = Vec::new();
    for summary in chain.into_iter().rev() {
        for field in &summary.fields {
            if field.is_static {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "static field storage",
                })
                .with_site(module, span));
            }
            let ty = HirType::from_view(&field.ty, &mut function_id_of)
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

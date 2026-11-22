//! 对已降级操作再次校验目标约束，并据实际操作计算运行时需求。
use super::*;
use novella_core::target::{RuntimeMode, RuntimeRequirements};

pub(super) fn managed(sema: &SemaResult, ty: &HirType) -> bool {
    fn visit(sema: &SemaResult, ty: &HirType, seen: &mut HashSet<(ModuleId, String)>) -> bool {
        match ty {
            HirType::Class(..) | HirType::Trait(..) | HirType::Array(_) => true,
            HirType::Tuple(items) => items.iter().any(|ty| visit(sema, ty, seen)),
            HirType::Struct(module, name) => {
                if !seen.insert((*module, name.clone())) {
                    return false;
                }
                sema.types
                    .summaries
                    .iter()
                    .find(|summary| summary.module == *module && summary.name == *name)
                    .is_some_and(|summary| {
                        summary.fields.iter().filter(|f| !f.is_static).any(|field| {
                            field.is_weak
                                || HirType::from_view(&field.ty, &mut function_id_of)
                                    .is_ok_and(|ty| visit(sema, &ty, seen))
                        })
                    })
            }
            _ => false,
        }
    }
    visit(sema, ty, &mut HashSet::new())
}

fn floating(ty: &HirType) -> bool {
    match ty {
        HirType::Float(_) => true,
        HirType::Pointer(ty) | HirType::Array(ty) => floating(ty),
        HirType::Tuple(items) => items.iter().any(floating),
        HirType::Function(signature) => {
            signature.params.iter().any(floating) || floating(&signature.ret)
        }
        _ => false,
    }
}

pub(super) fn validate(
    sema: &SemaResult,
    modules: &[HirModule],
) -> Result<RuntimeRequirements, BuildError> {
    if sema.build_target.validate().is_err()
        || sema.build_target.is_bare_metal() && (!sema.no_main || !sema.no_std)
    {
        return Err(BuildError::at(BuildErrorKind::InvalidInput));
    }
    let mut required = false;
    for module in modules {
        if sema.build_target.is_bare_metal() {
            // 浮点在所有声明中都禁用，但纯类型中的托管字段仍不构成实际运行时依赖。
            for definition in &module.types {
                let (fields, methods) = match definition {
                    crate::item::HirTypeDef::Struct {
                        fields, methods, ..
                    }
                    | crate::item::HirTypeDef::Class {
                        fields, methods, ..
                    } => (fields.as_slice(), methods),
                    crate::item::HirTypeDef::Trait { methods, .. } => (&[][..], methods),
                };
                if fields.iter().any(|field| floating(&field.ty))
                    || methods.iter().any(|method| {
                        method.signature.params.iter().any(floating)
                            || floating(&method.signature.ret)
                    })
                {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "bare-metal floating-point declaration",
                    })
                    .with_site(module.id, Span { start: 0, end: 0 }));
                }
            }
        }
        for item in &module.items {
            match item {
                HirItem::Function(function) => {
                    for ty in function
                        .signature
                        .params
                        .iter()
                        .chain(std::iter::once(&function.signature.ret))
                    {
                        check_type(sema, ty, module.id, &mut required)?;
                    }
                    block(sema, &function.body, module.id, &mut required)?;
                }
                HirItem::ExternFunction(function) => {
                    if sema.build_target.is_bare_metal()
                        && (function.signature.params.iter().any(floating)
                            || floating(&function.signature.ret))
                    {
                        return Err(BuildError::at(BuildErrorKind::Unimplemented {
                            node: "bare-metal floating-point ABI",
                        })
                        .with_site(module.id, Span { start: 0, end: 0 }));
                    }
                }
                HirItem::Global(global) => check_type(sema, &global.ty, module.id, &mut required)?,
            }
        }
    }
    Ok(RuntimeRequirements { managed: required })
}

fn check_type(
    sema: &SemaResult,
    ty: &HirType,
    module: ModuleId,
    required: &mut bool,
) -> Result<(), BuildError> {
    if sema.build_target.is_bare_metal() && floating(ty) {
        return Err(BuildError::at(BuildErrorKind::Unimplemented {
            node: "bare-metal floating point",
        })
        .with_site(module, Span { start: 0, end: 0 }));
    }
    if managed(sema, ty) {
        require(sema, module, required)?;
    }
    Ok(())
}

fn require(sema: &SemaResult, module: ModuleId, required: &mut bool) -> Result<(), BuildError> {
    if sema.build_target.runtime == RuntimeMode::None {
        return Err(BuildError::at(BuildErrorKind::Unimplemented {
            node: "managed operation without runtime",
        })
        .with_site(module, Span { start: 0, end: 0 }));
    }
    *required = true;
    Ok(())
}

fn block(
    sema: &SemaResult,
    body: &HirBlock,
    module: ModuleId,
    required: &mut bool,
) -> Result<(), BuildError> {
    for statement in &body.stmts {
        match statement {
            HirStmt::Block(body) => block(sema, body, module, required)?,
            HirStmt::If {
                cond,
                then_block,
                else_block,
            } => {
                block(sema, &cond.prelude, module, required)?;
                block(sema, then_block, module, required)?;
                if let Some(body) = else_block {
                    block(sema, body, module, required)?;
                }
            }
            HirStmt::Loop { cond, body, step } => {
                if let Some(cond) = cond {
                    block(sema, &cond.prelude, module, required)?;
                }
                block(sema, body, module, required)?;
                block(sema, step, module, required)?;
            }
            HirStmt::Instr(instruction) => {
                let types: Vec<&HirType> = match instruction {
                    HirInstr::StringLiteral { .. }
                    | HirInstr::Array { .. }
                    | HirInstr::New { .. }
                    | HirInstr::ArrayLength { .. } => {
                        require(sema, module, required)?;
                        Vec::new()
                    }
                    HirInstr::LocalAddress { .. } => Vec::new(),
                    HirInstr::FieldAddress { target_ty, .. } => vec![target_ty],
                    HirInstr::IndexAddress { element_ty, .. }
                    | HirInstr::IndexLoad { element_ty, .. }
                    | HirInstr::IndexStore { element_ty, .. } => vec![element_ty],
                    HirInstr::StoreIndirect { ty, .. }
                    | HirInstr::Tuple { ty, .. }
                    | HirInstr::TupleExtract { ty, .. }
                    | HirInstr::Alloc { ty, .. } => vec![ty],
                    HirInstr::Call { dst_ty, .. } => dst_ty.iter().collect(),
                    HirInstr::Unary { dst_ty, .. } => vec![dst_ty],
                    HirInstr::Binary {
                        lhs_ty,
                        rhs_ty,
                        dst_ty,
                        ..
                    } => vec![lhs_ty, rhs_ty, dst_ty],
                    HirInstr::Cast { from, to, .. } => vec![from, to],
                    HirInstr::TypeTest {
                        operand_ty, target, ..
                    } => vec![operand_ty, target],
                    HirInstr::FieldLoad { target_ty, ty, .. } => vec![target_ty, ty],
                    HirInstr::FieldStore {
                        target_ty, is_weak, ..
                    } => {
                        if *is_weak {
                            require(sema, module, required)?;
                        }
                        vec![target_ty]
                    }
                    HirInstr::Store { .. } | HirInstr::Load { .. } | HirInstr::Return(_) => {
                        Vec::new()
                    }
                };
                for ty in types {
                    check_type(sema, ty, module, required)?;
                }
            }
            HirStmt::Break
            | HirStmt::Continue
            | HirStmt::ReleaseTemps(_)
            | HirStmt::EnterScope
            | HirStmt::LeaveScope
            | HirStmt::Nop => {}
        }
    }
    Ok(())
}

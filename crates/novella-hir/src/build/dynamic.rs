//! 保留 sema 验证过的转换和固定槽位，构造拥有型动态 trait 值。
use super::{mono, *};
use novella_sema::TypeSummary;

fn dynamic_trait<'a>(sema: &'a SemaResult, ty: &HirType) -> Option<&'a TypeSummary> {
    let HirType::Dynamic(dynamic) = ty else {
        return None;
    };
    mono::summary(sema, dynamic.module, &dynamic.name)
}

impl BodyBuilder<'_> {
    pub(super) fn dynamic_slot(
        &self,
        ty: &HirType,
        method: &str,
        span: Span,
    ) -> Result<u32, BuildError> {
        dynamic_trait(self.sema, ty)
            .and_then(|definition| {
                definition
                    .methods
                    .iter()
                    .filter(|m| m.is_dyn_dispatchable)
                    .position(|m| m.name == method)
            })
            .and_then(|slot| u32::try_from(slot).ok())
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::InvalidInput).with_site(self.module, span)
            })
    }
}

impl BodyBuilder<'_> {
    pub(super) fn pack_dynamic(
        &mut self,
        operand: HirOperand,
        source: &HirType,
        target: &HirType,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let invalid = || BuildError::at(BuildErrorKind::InvalidInput).with_site(self.module, span);
        let conversion = self
            .sema
            .dyn_conversions
            .get(&(self.module, span))
            .ok_or_else(invalid)?;
        let source_view = mono::substitute(&conversion.source, &self.subst);
        let target_view = mono::substitute(&conversion.target, &self.subst);
        let TypeView::Dynamic { trait_ref, .. } = &target_view else {
            return Err(invalid());
        };
        let (implementation, impl_subst) = mono::resolve_trait_implementation(
            self.sema,
            &self.instances,
            &source_view,
            trait_ref,
        )?
        .ok_or_else(invalid)?;
        let definition = dynamic_trait(self.sema, target).ok_or_else(invalid)?;
        // 与 dynamic_slot 共用 sema 排好的方法顺序；无需在运行时查找方法。
        let mut witness = Vec::new();
        for slot in definition
            .methods
            .iter()
            .filter(|method| method.is_dyn_dispatchable)
        {
            let method = implementation
                .methods
                .iter()
                .find(|method| method.name == slot.name)
                .ok_or_else(invalid)?;
            let sig = mono::instantiate_trait_method(
                self.sema,
                &self.instances,
                implementation,
                &impl_subst,
                method,
                &Vec::new(),
            )?;
            witness.push(sig.id);
        }
        let dst = self.fresh_local(target.clone());
        block.push(HirStmt::Instr(HirInstr::DynPack {
            operand,
            source: source.clone(),
            witness,
            ty: target.clone(),
            dst,
        }));
        Ok(HirOperand::Local(dst))
    }
}

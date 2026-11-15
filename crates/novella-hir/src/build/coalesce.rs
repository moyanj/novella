//! 空合并的短路降级：左值求一次，仅空分支执行备用表达式。

use super::*;

impl BodyBuilder<'_> {
    pub(super) fn build_coalesce(
        &mut self,
        left: &Expression,
        right: &Expression,
        span: Span,
        preferred: Option<&HirType>,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let ty = self.expr_type_pref(span, preferred)?;
        if matches!(
            self.sema.expression_types.get(self.module, left.span()),
            Some(TypeView::Null)
        ) {
            // 纯 null 没有副作用，但括号等语法仍通过正常降级入口校验。
            self.build_expression(left, block)?;
            return self.build_typed_expression(right, &ty, block);
        }
        let value = self.build_typed_expression(left, &ty, block)?;
        let test = self.fresh_local(HirType::Boolean);
        block.push(HirStmt::Instr(HirInstr::Binary {
            op: HirBinaryOp::Ne,
            lhs_ty: ty.clone(),
            rhs_ty: ty.clone(),
            lhs: value.clone(),
            rhs: HirOperand::Null,
            dst: test,
            dst_ty: HirType::Boolean,
        }));
        let dst = self.fresh_local(ty.clone());
        block.push(HirStmt::Instr(HirInstr::Alloc {
            id: dst,
            ty: ty.clone(),
            init: None,
        }));
        let mut present = HirBlock::new();
        present.push(HirStmt::Instr(HirInstr::Store { id: dst, value }));
        let mut absent = HirBlock::new();
        let fallback = self.build_typed_expression(right, &ty, &mut absent)?;
        absent.push(HirStmt::Instr(HirInstr::Store {
            id: dst,
            value: fallback,
        }));
        block.push(HirStmt::If {
            cond: HirCond::new(HirOperand::Local(test)),
            then_block: present,
            else_block: Some(absent),
        });
        Ok(HirOperand::Local(dst))
    }
}

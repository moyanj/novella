//! 定长数组遍历：数组求值一次，continue 统一跳到循环 step。

use super::*;

impl BodyBuilder<'_> {
    pub(super) fn emit_for_in(
        &mut self,
        cond: &novella_ast::stmt::ForInCond,
        body: &Statement,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        self.with_scope(|this| {
            let HirType::Array(element_ty, length) = this.expr_type(cond.right.span())? else {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "for-in of non-array",
                })
                .with_site(this.module, cond.span));
            };
            let array = this.build_expression(&cond.right, block)?;
            let usize_ty = HirType::Int(IntTy::Usize);
            let index = this.fresh_local(usize_ty.clone());
            block.push(HirStmt::Instr(HirInstr::Alloc {
                id: index,
                ty: usize_ty.clone(),
                init: Some(HirOperand::IntConst(0)),
            }));
            let mut prelude = HirBlock::new();
            let test = this.fresh_local(HirType::Boolean);
            prelude.push(HirStmt::Instr(HirInstr::Binary {
                op: HirBinaryOp::Lt,
                lhs_ty: usize_ty.clone(),
                rhs_ty: usize_ty.clone(),
                lhs: HirOperand::Local(index),
                rhs: HirOperand::IntConst(i128::from(length)),
                dst: test,
                dst_ty: HirType::Boolean,
            }));
            let mut loop_body = HirBlock::new();
            let element = this.fresh_local((*element_ty).clone());
            loop_body.push(HirStmt::Instr(HirInstr::IndexLoad {
                target: array,
                element_ty: (*element_ty).clone(),
                index: HirOperand::Local(index),
                dst: element,
            }));
            this.bind_pattern(
                &cond.left,
                &element_ty,
                Some(HirOperand::Local(element)),
                &mut loop_body,
            )?;
            this.loop_depth += 1;
            let result = this.emit_statement(body, &mut loop_body);
            this.loop_depth -= 1;
            result?;
            let mut step = HirBlock::new();
            let next = this.fresh_local(usize_ty.clone());
            step.push(HirStmt::Instr(HirInstr::Binary {
                op: HirBinaryOp::Add,
                lhs_ty: usize_ty.clone(),
                rhs_ty: usize_ty.clone(),
                lhs: HirOperand::Local(index),
                rhs: HirOperand::IntConst(1),
                dst: next,
                dst_ty: usize_ty,
            }));
            step.push(HirStmt::Instr(HirInstr::Store {
                id: index,
                value: HirOperand::Local(next),
            }));
            block.push(HirStmt::Loop {
                cond: Some(HirCond {
                    prelude,
                    value: HirOperand::Local(test),
                }),
                body: loop_body,
                step,
            });
            Ok(())
        })
    }
}

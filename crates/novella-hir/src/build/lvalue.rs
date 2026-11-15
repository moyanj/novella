//! 左值地址：位置先求一次，再读取 / 修改，保证复合赋值和自增的求值顺序。

use super::*;

impl BodyBuilder<'_> {
    /// 把局部变成存储地址，供字段链、取地址与赋值共用。
    fn local_address(&mut self, local: LocalId, block: &mut HirBlock) -> HirOperand {
        let ty = self.local_type(local);
        let dst = self.fresh_local(HirType::Pointer(Box::new(ty.clone())));
        block.push(HirStmt::Instr(HirInstr::LocalAddress { local, ty, dst }));
        HirOperand::Local(dst)
    }

    /// 取得 struct 存储地址；右值先物化为临时槽位，避免把值误作地址。
    pub(super) fn aggregate_address(
        &mut self,
        expr: &Expression,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        match expr {
            Expression::Identifier { .. }
            | Expression::MemberAccess { .. }
            | Expression::IndexAccess { .. }
            | Expression::This { .. }
            | Expression::Unary {
                op: UnaryOp::DerefPointer,
                ..
            } => self.build_address(expr, block),
            Expression::Paren { expr, .. } => self.aggregate_address(expr, block),
            _ => {
                let ty = self.expr_type(expr.span())?;
                let value = self.build_expression(expr, block)?;
                let local = self.fresh_local(ty.clone());
                block.push(HirStmt::Instr(HirInstr::Alloc {
                    id: local,
                    ty,
                    init: Some(value),
                }));
                Ok(self.local_address(local, block))
            }
        }
    }

    pub(super) fn build_address(
        &mut self,
        expr: &Expression,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        match expr {
            Expression::Paren { expr, .. } => self.build_address(expr, block),
            Expression::Identifier { name, span } => {
                if let Some(local) = self.lookup_local(name) {
                    if name == "self" && matches!(self.local_type(local), HirType::Pointer(_)) {
                        return Ok(HirOperand::Local(local));
                    }
                    return Ok(self.local_address(local, block));
                }
                // with 接收者与 self 成员都走与显式字段相同的布局和地址指令。
                if let Some((ty, value)) = self.with_receiver.clone()
                    && let Ok((index, field_ty)) = self.field_slot(&ty, name, *span)
                {
                    return Ok(self.field_address(value, ty, index, field_ty, block));
                }
                if let Some(local) = self.lookup_local("self") {
                    let ty = self.local_type(local);
                    let (index, field_ty) = self.field_slot(&ty, name, *span)?;
                    let base = if matches!(ty, HirType::Struct(..)) {
                        self.local_address(local, block)
                    } else {
                        HirOperand::Local(local)
                    };
                    return Ok(self.field_address(base, ty, index, field_ty, block));
                }
                Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "non-local assignment target",
                })
                .with_site(self.module, *span))
            }
            Expression::This { span } => {
                let local = self.lookup_local("self").ok_or_else(|| {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "this 接收者",
                    })
                    .with_site(self.module, *span)
                })?;
                if matches!(self.local_type(local), HirType::Pointer(_)) {
                    Ok(HirOperand::Local(local))
                } else {
                    Ok(self.local_address(local, block))
                }
            }
            Expression::MemberAccess { target, name, span } => {
                let target_ty = self.expr_type(target.span())?;
                let (index, field_ty) = self.field_slot(&target_ty, name, *span)?;
                let base = if matches!(target_ty, HirType::Struct(..)) {
                    self.aggregate_address(target, block)?
                } else {
                    self.build_expression(target, block)?
                };
                Ok(self.field_address(base, target_ty, index, field_ty, block))
            }
            Expression::IndexAccess {
                target,
                index,
                span,
            } => {
                let element_ty = self.expr_type(*span)?;
                let value = self.build_expression(target, block)?;
                let index =
                    self.build_typed_expression(index, &HirType::Int(IntTy::Usize), block)?;
                let dst = self.fresh_local(HirType::Pointer(Box::new(element_ty.clone())));
                block.push(HirStmt::Instr(HirInstr::IndexAddress {
                    target: value,
                    index,
                    element_ty,
                    dst,
                }));
                Ok(HirOperand::Local(dst))
            }
            Expression::Unary {
                op: UnaryOp::DerefPointer,
                operand,
                ..
            } => self.build_expression(operand, block),
            _ => Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "non-lvalue address",
            })
            .with_site(self.module, expr.span())),
        }
    }

    fn field_address(
        &mut self,
        target: HirOperand,
        target_ty: HirType,
        index: usize,
        ty: HirType,
        block: &mut HirBlock,
    ) -> HirOperand {
        let dst = self.fresh_local(HirType::Pointer(Box::new(ty.clone())));
        block.push(HirStmt::Instr(HirInstr::FieldAddress {
            target,
            target_ty,
            index,
            ty,
            dst,
        }));
        HirOperand::Local(dst)
    }

    pub(super) fn read_address(
        &mut self,
        address: HirOperand,
        ty: &HirType,
        block: &mut HirBlock,
    ) -> HirOperand {
        let dst = self.fresh_local(ty.clone());
        block.push(HirStmt::Instr(HirInstr::Unary {
            op: HirUnaryOp::Deref,
            operand: address,
            dst,
            dst_ty: ty.clone(),
        }));
        HirOperand::Local(dst)
    }
}

//! 左值地址：位置先求一次，再读取 / 修改，保证复合赋值和自增的求值顺序。

use super::*;

type WeakFieldTarget = (HirOperand, HirType, String, usize);

/// 下标读改写共用同一个接收者和下标实参。
pub(super) struct IndexPlaceTarget {
    pub(super) receiver: HirOperand,
    pub(super) receiver_ty: HirType,
    pub(super) key: HirOperand,
    span: Span,
}

/// 定位结果来自一次求值；weak 字段必须走字段指令，不能按裸载荷地址读写。
pub(super) enum UpdatePlace {
    Address(HirOperand),
    WeakField(WeakFieldTarget),
    Index(IndexPlaceTarget),
}

impl UpdatePlace {
    pub(super) fn read(
        &self,
        builder: &mut BodyBuilder<'_>,
        ty: &HirType,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        Ok(match self {
            Self::Address(address) => builder.read_address(address.clone(), ty, block),
            Self::WeakField((target, target_ty, field, index)) => {
                let dst = builder.fresh_local(ty.clone());
                block.push(HirStmt::Instr(HirInstr::FieldLoad {
                    target: target.clone(),
                    target_ty: target_ty.clone(),
                    field: field.clone(),
                    index: *index,
                    ty: ty.clone(),
                    borrow: false,
                    dst,
                }));
                HirOperand::Local(dst)
            }
            Self::Index(target) => {
                let signature =
                    builder.protocol_signature(&target.receiver_ty, "__getitem", target.span)?;
                builder.emit_protocol_call(
                    signature,
                    vec![target.receiver.clone(), target.key.clone()],
                    false,
                    block,
                )
            }
        })
    }

    pub(super) fn write(
        self,
        builder: &mut BodyBuilder<'_>,
        value: HirOperand,
        ty: HirType,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        block.push(HirStmt::Instr(match self {
            // 地址落点的类型由左值本身决定，写入与运算符无关。
            Self::Address(address) => HirInstr::StoreIndirect { address, value, ty },
            Self::WeakField((target, target_ty, field, index)) => HirInstr::FieldStore {
                target,
                target_ty,
                field,
                index,
                value,
                is_weak: true,
            },
            Self::Index(target) => {
                // 写入是过程式协议，没有返回值。
                let signature =
                    builder.protocol_signature(&target.receiver_ty, "__setitem", target.span)?;
                builder.emit_protocol_call(
                    signature,
                    vec![target.receiver, target.key, value],
                    false,
                    block,
                );
                return Ok(());
            }
        }));
        Ok(())
    }
}

impl BodyBuilder<'_> {
    pub(super) fn update_place(
        &mut self,
        target: &Expression,
        borrow: bool,
        block: &mut HirBlock,
    ) -> Result<UpdatePlace, BuildError> {
        if let Some(receiver_ty) = self.overloaded_index_receiver(target)? {
            let mut expr = target;
            while let Expression::Paren { expr: inner, .. } = expr {
                expr = inner;
            }
            let Expression::IndexAccess {
                target: receiver,
                index,
                span,
            } = expr
            else {
                unreachable!("已检查下标表达式")
            };
            let method = if borrow { "__setitem" } else { "__getitem" };
            let signature = self.protocol_signature(&receiver_ty, method, *span)?;
            return self
                .build_protocol_index_place(
                    receiver,
                    index,
                    receiver_ty,
                    &signature.params[0],
                    *span,
                    block,
                )
                .map(UpdatePlace::Index);
        }
        match self.weak_assignment_field(target, block)? {
            Some(field) => Ok(UpdatePlace::WeakField(field)),
            None => self
                .build_address_for_use(target, borrow, block)
                .map(UpdatePlace::Address),
        }
    }

    /// 基座与键各求值一次；struct 方法接收原槽位地址，以便 setter 修改它。
    pub(super) fn build_protocol_index_place(
        &mut self,
        receiver: &Expression,
        index: &Expression,
        receiver_ty: HirType,
        key_ty: &HirType,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<IndexPlaceTarget, BuildError> {
        let receiver = if matches!(receiver_ty, HirType::Struct(..)) {
            self.aggregate_address(receiver, block)?
        } else {
            self.build_expression(receiver, block)?
        };
        let key = self.build_call_argument(index, key_ty, block)?;
        Ok(IndexPlaceTarget {
            receiver,
            receiver_ty,
            key,
            span,
        })
    }

    /// 这两种地址路径不会执行用户代码，也不会改写 RHS 的来源槽位。
    pub(super) fn noninterfering_assignment_target(&self, expr: &Expression) -> bool {
        match expr {
            Expression::Paren { expr, .. } => self.noninterfering_assignment_target(expr),
            Expression::Identifier { name, .. } => self.lookup_local(name).is_some(),
            Expression::MemberAccess { target, .. } => match target.as_ref() {
                Expression::Identifier { name, .. } => self.lookup_local(name).is_some(),
                Expression::This { .. } => true,
                _ => false,
            },
            _ => false,
        }
    }

    pub(super) fn check_index_target(
        &self,
        target: &Expression,
        span: Span,
    ) -> Result<(), BuildError> {
        if matches!(self.expr_type(target.span())?, HirType::Tuple(_)) {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "tuple indexing",
            })
            .with_site(self.module, span));
        }
        Ok(())
    }

    /// 把局部变成存储地址，供字段链、取地址与赋值共用。
    pub(super) fn local_address(&mut self, local: LocalId, block: &mut HirBlock) -> HirOperand {
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
            | Expression::This { .. }
            | Expression::Unary {
                op: UnaryOp::DerefPointer,
                ..
            } => self.build_address(expr, block),
            Expression::IndexAccess { .. } if self.overloaded_index_receiver(expr)?.is_none() => {
                self.build_address(expr, block)
            }
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

    /// weak 字段没有可暴露的 payload 指针槽位；赋值必须保留字段元数据。
    fn weak_assignment_field(
        &mut self,
        expr: &Expression,
        block: &mut HirBlock,
    ) -> Result<Option<WeakFieldTarget>, BuildError> {
        match expr {
            Expression::Paren { expr, .. } => self.weak_assignment_field(expr, block),
            Expression::MemberAccess { target, name, span } => {
                let ty = self.expr_type(target.span())?;
                let (index, _) = self.field_slot(&ty, name, *span)?;
                if !self.is_weak_field(&ty, index, *span)? {
                    return Ok(None);
                }
                let receiver = self.build_expression(target, block)?;
                Ok(Some((receiver, ty, name.clone(), index)))
            }
            Expression::Identifier { name, span } if self.lookup_local(name).is_none() => {
                let receiver = self.with_receiver.clone().or_else(|| {
                    self.lookup_local("self")
                        .map(|id| (self.local_type(id), HirOperand::Local(id)))
                });
                let Some((ty, receiver)) = receiver else {
                    return Ok(None);
                };
                let (index, _) = self.field_slot(&ty, name, *span)?;
                Ok(self
                    .is_weak_field(&ty, index, *span)?
                    .then(|| (receiver, ty, name.clone(), index)))
            }
            _ => Ok(None),
        }
    }

    fn is_weak_field(&self, ty: &HirType, index: usize, span: Span) -> Result<bool, BuildError> {
        let ty = match ty {
            HirType::Pointer(inner) => inner.as_ref(),
            ty => ty,
        };
        let (HirType::Class(module, name) | HirType::Struct(module, name)) = ty else {
            return Ok(false);
        };
        Ok(instance_fields(self.sema, &self.instances, *module, name, span)?[index].is_weak)
    }

    pub(super) fn build_address(
        &mut self,
        expr: &Expression,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        self.build_address_for_use(expr, false, block)
    }

    pub(super) fn build_address_for_use(
        &mut self,
        expr: &Expression,
        immediate: bool,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        match expr {
            Expression::Paren { expr, .. } => self.build_address_for_use(expr, immediate, block),
            Expression::Identifier { name, span } => {
                if let Some(local) = self.lookup_local(name) {
                    if name == "self" && matches!(self.local_type(local), HirType::Pointer(_)) {
                        return Ok(HirOperand::Local(local));
                    }
                    return Ok(self.local_address(local, block));
                }
                if let Some(address) = super::globals::address(self.sema, self.module, *span) {
                    return Ok(address);
                }
                // with 接收者与 self 成员都走与显式字段相同的布局和地址指令。
                if let Some((ty, value)) = self.with_receiver.clone()
                    && let Ok((index, field_ty)) = self.field_slot(&ty, name, *span)
                {
                    return self.field_address(value, ty, index, field_ty, *span, block);
                }
                if let Some(local) = self.lookup_local("self") {
                    let ty = self.local_type(local);
                    let (index, field_ty) = self.field_slot(&ty, name, *span)?;
                    let base = if matches!(ty, HirType::Struct(..)) {
                        self.local_address(local, block)
                    } else {
                        HirOperand::Local(local)
                    };
                    return self.field_address(base, ty, index, field_ty, *span, block);
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
                if let Some(address) = super::globals::address(self.sema, self.module, *span) {
                    return Ok(address);
                }
                let target_ty = self.expr_type(target.span())?;
                let (index, field_ty) = self.field_slot(&target_ty, name, *span)?;
                let base = if matches!(target_ty, HirType::Struct(..)) {
                    self.aggregate_address(target, block)?
                } else if immediate {
                    self.build_immediate_value(target, &target_ty, block)?
                } else {
                    self.build_expression(target, block)?
                };
                self.field_address(base, target_ty, index, field_ty, *span, block)
            }
            Expression::IndexAccess {
                target,
                index,
                span,
            } => {
                self.check_index_target(target, *span)?;
                let element_ty = self.expr_type(*span)?;
                let value = if matches!(self.expr_type(target.span())?, HirType::Array(..)) {
                    self.aggregate_address(target, block)?
                } else {
                    self.build_expression(target, block)?
                };
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
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        if self.is_weak_field(&target_ty, index, span)? {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "weak 字段取址",
            })
            .with_site(self.module, span));
        }
        let dst = self.fresh_local(HirType::Pointer(Box::new(ty.clone())));
        block.push(HirStmt::Instr(HirInstr::FieldAddress {
            target,
            target_ty,
            index,
            ty,
            dst,
        }));
        Ok(HirOperand::Local(dst))
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

//! 运算符降级：短路、定型，以及先定位再读写的赋值。

use super::*;

fn is_float_literal_expression(expr: &Expression) -> bool {
    match expr {
        Expression::Literal {
            value: novella_ast::expr::Literal::Number(raw),
            ..
        } => raw.contains('.') || raw.contains(['e', 'E']),
        Expression::Paren { expr, .. } | Expression::Unary { operand: expr, .. } => {
            is_float_literal_expression(expr)
        }
        _ => false,
    }
}

impl BodyBuilder<'_> {
    pub(super) fn build_unary(
        &mut self,
        op: UnaryOp,
        operand: &Expression,
        span: Span,
        preferred: Option<&HirType>,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        if op == UnaryOp::AddressOf {
            return self.build_address(operand, block);
        }
        let ty = self.expr_type_pref(span, preferred)?;
        if matches!(op, UnaryOp::PostIncrement | UnaryOp::PostDecrement) {
            let address = self.build_address(operand, block)?;
            let old = self.read_address(address.clone(), &ty, block);
            let one = if matches!(ty, HirType::Float(_)) {
                HirOperand::FloatConst(1.0)
            } else {
                HirOperand::IntConst(1)
            };
            let dst = self.fresh_local(ty.clone());
            block.push(HirStmt::Instr(HirInstr::Binary {
                op: if op == UnaryOp::PostIncrement {
                    HirBinaryOp::Add
                } else {
                    HirBinaryOp::Sub
                },
                lhs: old.clone(),
                rhs: one,
                lhs_ty: ty.clone(),
                rhs_ty: if matches!(ty, HirType::Pointer(_)) {
                    HirType::Int(IntTy::Isize)
                } else {
                    ty.clone()
                },
                dst,
                dst_ty: ty.clone(),
            }));
            block.push(HirStmt::Instr(HirInstr::StoreIndirect {
                address,
                value: HirOperand::Local(dst),
                ty,
            }));
            return Ok(old);
        }
        let operand_ty = if op == UnaryOp::DerefPointer {
            self.expr_type(operand.span())?
        } else {
            ty.clone()
        };
        let value = self.build_typed_expression(operand, &operand_ty, block)?;
        let hir_op = match op {
            UnaryOp::Plus => return Ok(value),
            UnaryOp::Minus => HirUnaryOp::Neg,
            UnaryOp::Not => HirUnaryOp::Not,
            UnaryOp::BitNot => HirUnaryOp::BitNot,
            UnaryOp::DerefPointer => HirUnaryOp::Deref,
            UnaryOp::AddressOf | UnaryOp::PostIncrement | UnaryOp::PostDecrement => unreachable!(),
        };
        let dst = self.fresh_local(ty.clone());
        block.push(HirStmt::Instr(HirInstr::Unary {
            op: hir_op,
            operand: value,
            dst,
            dst_ty: ty,
        }));
        Ok(HirOperand::Local(dst))
    }

    pub(super) fn build_binary(
        &mut self,
        op: BinaryOp,
        left: &Expression,
        right: &Expression,
        span: Span,
        preferred: Option<&HirType>,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            return self.build_short_circuit(op, left, right, block);
        }
        if op == BinaryOp::NullCoalesce {
            return self.build_coalesce(left, right, span, preferred, block);
        }
        let left_type = self.expr_type(left.span()).ok();
        let right_type = self.expr_type(right.span()).ok();
        let common = [left_type.as_ref(), right_type.as_ref()]
            .into_iter()
            .flatten()
            .find(|ty| matches!(ty, HirType::Float(_)))
            .cloned()
            .or_else(|| {
                (is_float_literal_expression(left) && is_float_literal_expression(right))
                    .then_some(HirType::Float(crate::FloatTy::F32))
            });
        if let Some(common) = common {
            let lhs = self.build_typed_expression(left, &common, block)?;
            let rhs = self.build_typed_expression(right, &common, block)?;
            let result_ty = self.expr_type_pref(span, preferred)?;
            let dst = self.fresh_local(result_ty.clone());
            block.push(HirStmt::Instr(HirInstr::Binary {
                op: binary_op_of(op).ok_or_else(|| {
                    BuildError::at(BuildErrorKind::Unimplemented {
                        node: binary_node_name(op),
                    })
                    .with_site(self.module, span)
                })?,
                lhs,
                rhs,
                lhs_ty: common.clone(),
                rhs_ty: common,
                dst,
                dst_ty: result_ty,
            }));
            return Ok(HirOperand::Local(dst));
        }
        if let (Some(left_type), Some(right_type)) = (&left_type, &right_type)
            && same_string_class(self.sema, left_type)
            && left_type == right_type
        {
            let method_name = match op {
                BinaryOp::Plus => Some("concat"),
                BinaryOp::Equals | BinaryOp::NotEquals => Some("equals"),
                _ => None,
            };
            if let Some(method_name) = method_name {
                let signature = self
                    .class_method_signature(
                        class_identity(left_type).unwrap().0,
                        "String",
                        method_name,
                        span,
                    )?
                    .ok_or_else(|| {
                        BuildError::at(BuildErrorKind::MissingType {
                            what: "std:String 运算方法",
                        })
                        .with_site(self.module, span)
                    })?;
                let receiver = self.build_typed_expression(left, left_type, block)?;
                let argument = self.build_typed_expression(right, right_type, block)?;
                let result_ty = self.expr_type_pref(span, preferred)?;
                let dst = self.fresh_local(signature.ret.clone());
                block.push(HirStmt::Instr(HirInstr::Call {
                    callee: HirOperand::FunctionPtr(signature.id),
                    args: vec![receiver, argument],
                    dst: Some(dst),
                    dst_ty: Some(signature.ret),
                }));
                if op == BinaryOp::NotEquals {
                    let inverted = self.fresh_local(result_ty);
                    block.push(HirStmt::Instr(HirInstr::Unary {
                        op: HirUnaryOp::Not,
                        operand: HirOperand::Local(dst),
                        dst: inverted,
                        dst_ty: HirType::Boolean,
                    }));
                    return Ok(HirOperand::Local(inverted));
                }
                return Ok(HirOperand::Local(dst));
            }
        }
        let mut hir_op = binary_op_of(op).ok_or_else(|| {
            BuildError::at(BuildErrorKind::Unimplemented {
                node: binary_node_name(op),
            })
            .with_site(self.module, span)
        })?;
        let result_ty = self.expr_type_pref(span, preferred)?;
        // 比较的 bool 结果不能定型操作数；优先选已定型的一侧。
        let left_ty = self.expr_type(left.span()).ok();
        let right_ty = self.expr_type(right.span()).ok();
        let pointer_math = !is_comparison(hir_op)
            && (matches!(left_ty, Some(HirType::Pointer(_)))
                || matches!(right_ty, Some(HirType::Pointer(_))));
        let value_ty = if pointer_math {
            left_ty.clone().unwrap_or(HirType::Int(IntTy::Isize))
        } else if is_comparison(hir_op) {
            match (self.expr_type(left.span()), self.expr_type(right.span())) {
                (Ok(ty), _) | (_, Ok(ty)) => ty,
                (Err(error), _) => return Err(error),
            }
        } else {
            result_ty.clone()
        };
        if matches!(op, BinaryOp::IdentityEquals | BinaryOp::IdentityNotEquals)
            && !matches!(
                value_ty,
                HirType::Boolean
                    | HirType::Int(_)
                    | HirType::Float(_)
                    | HirType::Pointer(_)
                    | HirType::Class(..)
            )
        {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "identity comparison of aggregates / strings",
            })
            .with_site(self.module, span));
        }
        let lhs = self.build_typed_expression(left, &value_ty, block)?;
        // 指针算术保留整数位移；不能把整数强转为指针。
        let rhs_ty = if pointer_math {
            if matches!(left_ty, Some(HirType::Pointer(_)))
                && matches!(right_ty, Some(HirType::Pointer(_)))
            {
                hir_op = HirBinaryOp::PtrDiff;
            }
            right_ty.unwrap_or(HirType::Int(IntTy::Isize))
        } else {
            value_ty.clone()
        };
        let rhs = self.build_typed_expression(right, &rhs_ty, block)?;
        let dst = self.fresh_local(result_ty.clone());
        block.push(HirStmt::Instr(HirInstr::Binary {
            op: hir_op,
            lhs,
            rhs,
            lhs_ty: value_ty,
            rhs_ty,
            dst,
            dst_ty: result_ty,
        }));
        Ok(HirOperand::Local(dst))
    }

    pub(super) fn build_short_circuit(
        &mut self,
        op: BinaryOp,
        left: &Expression,
        right: &Expression,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let dst = self.fresh_local(HirType::Boolean);
        block.push(HirStmt::Instr(HirInstr::Alloc {
            id: dst,
            ty: HirType::Boolean,
            init: None,
        }));
        let lhs = self.build_expression(left, block)?;
        let mut rhs_block = HirBlock::new();
        let rhs = self.build_expression(right, &mut rhs_block)?;
        rhs_block.push(HirStmt::Instr(HirInstr::Store {
            id: dst,
            value: rhs,
        }));
        let mut stop = HirBlock::new();
        stop.push(HirStmt::Instr(HirInstr::Store {
            id: dst,
            value: HirOperand::BoolConst(op == BinaryOp::LogicalOr),
        }));
        let (then_block, else_block) = if op == BinaryOp::LogicalAnd {
            (rhs_block, stop)
        } else {
            (stop, rhs_block)
        };
        block.push(HirStmt::If {
            cond: HirCond::new(lhs),
            then_block,
            else_block: Some(else_block),
        });
        Ok(HirOperand::Local(dst))
    }

    /// 左值先定位一次，复合赋值在右值副作用发生前读取旧值。
    pub(super) fn build_assignment(
        &mut self,
        target: &Expression,
        op: AssignOp,
        value: &Expression,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let ty = self.expr_type(target.span())?;
        // 普通赋值先完成 RHS；复合赋值先定位并读取旧值。
        let early_rhs = if op == AssignOp::Assign {
            Some(self.build_typed_expression(value, &ty, block)?)
        } else {
            None
        };
        let weak_field = self.weak_assignment_field(target, block)?;
        let address = if weak_field.is_some() {
            None
        } else {
            Some(self.build_address(target, block)?)
        };
        let previous = if op == AssignOp::Assign {
            None
        } else if let Some((receiver, target_ty, field, index)) = &weak_field {
            let dst = self.fresh_local(ty.clone());
            block.push(HirStmt::Instr(HirInstr::FieldLoad {
                target: receiver.clone(),
                target_ty: target_ty.clone(),
                field: field.clone(),
                index: *index,
                ty: ty.clone(),
                dst,
            }));
            Some(HirOperand::Local(dst))
        } else {
            Some(self.read_address(address.clone().expect("普通左值有地址"), &ty, block))
        };
        let rhs_ty = if op != AssignOp::Assign && matches!(ty, HirType::Pointer(_)) {
            self.expr_type_pref(value.span(), Some(&HirType::Int(IntTy::Isize)))?
        } else {
            ty.clone()
        };
        let rhs = match early_rhs {
            Some(rhs) => rhs,
            None => self.build_typed_expression(value, &rhs_ty, block)?,
        };
        let stored = if let Some(lhs) = previous {
            if op == AssignOp::PlusAssign && same_string_class(self.sema, &ty) {
                self.string_method_call(&ty, "concat", vec![lhs, rhs], span, block)?
            } else {
                let hir_op = compound_op_of(op).ok_or_else(|| {
                    BuildError::at(BuildErrorKind::Unimplemented {
                        node: assign_node_name(op),
                    })
                    .with_site(self.module, span)
                })?;
                let dst = self.fresh_local(ty.clone());
                block.push(HirStmt::Instr(HirInstr::Binary {
                    op: hir_op,
                    lhs,
                    rhs,
                    lhs_ty: ty.clone(),
                    rhs_ty,
                    dst,
                    dst_ty: ty.clone(),
                }));
                HirOperand::Local(dst)
            }
        } else {
            rhs
        };
        if let Some((receiver, target_ty, field, index)) = weak_field {
            block.push(HirStmt::Instr(HirInstr::FieldStore {
                target: receiver,
                target_ty,
                field,
                index,
                value: stored.clone(),
                is_weak: true,
            }));
        } else {
            block.push(HirStmt::Instr(HirInstr::StoreIndirect {
                address: address.expect("普通左值有地址"),
                value: stored.clone(),
                ty,
            }));
        }
        Ok(stored)
    }

    /// 具名类型的字段槽位：`(下标, 字段类型)`。
    ///
    /// 沿 `sema.types.summaries` 做继承链查找（子类字段按声明顺序追加在父类之后），
    /// 找不到即 fail-fast —— 猜字段偏移会让 codegen 写坏内存。
    pub(super) fn field_slot(
        &self,
        target_ty: &HirType,
        field: &str,
        span: Span,
    ) -> Result<(usize, HirType), BuildError> {
        let base_ty = match target_ty {
            HirType::Pointer(inner) => inner.as_ref(),
            ty => ty,
        };
        let (module, name) = match base_ty {
            HirType::Struct(module, name) | HirType::Class(module, name) => (*module, name),
            _ => {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "field access on non-aggregate",
                })
                .with_site(self.module, span));
            }
        };
        let fields = instance_fields(self.sema, module, name, span)?;
        fields
            .iter()
            .enumerate()
            .rev()
            .find(|(_, candidate)| candidate.name == field)
            .map(|(index, field)| (index, field.ty.clone()))
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "字段布局"
                })
                .with_site(self.module, span)
            })
    }

    /// 方法体内裸写成员名（`self.x` / `with` 接收者字段）的隐式解析。
    pub(super) fn implicit_member(
        &mut self,
        name: &str,
        block: &mut HirBlock,
    ) -> Result<Option<HirOperand>, BuildError> {
        // `with e { x }`：接收者优先（与解析期 `with_stack` 的遮蔽顺序一致）
        if let Some((ty, receiver)) = self.with_receiver.clone()
            && let Ok((index, field_ty)) = self.field_slot(&ty, name, Span { start: 0, end: 0 })
        {
            let dst = self.fresh_local(field_ty.clone());
            block.push(HirStmt::Instr(HirInstr::FieldLoad {
                target: receiver,
                target_ty: ty,
                field: name.to_string(),
                index,
                ty: field_ty,
                dst,
            }));
            return Ok(Some(HirOperand::Local(dst)));
        }
        // 实例方法内的 `self.x`
        let Some(self_id) = self.lookup_local("self") else {
            return Ok(None);
        };
        let self_ty = self.local_type(self_id);
        let Ok((index, field_ty)) = self.field_slot(&self_ty, name, Span { start: 0, end: 0 })
        else {
            return Ok(None);
        };
        let dst = self.fresh_local(field_ty.clone());
        block.push(HirStmt::Instr(HirInstr::FieldLoad {
            target: HirOperand::Local(self_id),
            target_ty: self_ty,
            field: name.to_string(),
            index,
            ty: field_ty,
            dst,
        }));
        Ok(Some(HirOperand::Local(dst)))
    }
}

fn class_identity(ty: &HirType) -> Option<(novella_sema::ModuleId, &str)> {
    match ty {
        HirType::Class(module, name) => Some((*module, name)),
        _ => None,
    }
}

pub(super) fn same_string_class(sema: &SemaResult, ty: &HirType) -> bool {
    matches!((&sema.string_type, ty),
        (Some(TypeView::Class(expected_module, expected_name)), HirType::Class(module, name))
        if expected_module == module && expected_name == name)
}

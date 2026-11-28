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
            let place = self.update_place(operand, false, block)?;
            let old = place.read(self, &ty, block)?;
            if let Some(call) = self.sema.operator_calls.get(&(self.module, span)).copied() {
                let signature = self.protocol_signature(&ty, call.method, span)?;
                let from = signature.ret.clone();
                let receiver = self.protocol_receiver_value(old.clone(), &ty, block);
                let updated = self.emit_protocol_call(signature, vec![receiver], false, block);
                let value = self.coerce(updated, &from, &ty, span, block)?;
                place.write(self, value, ty, block)?;
                return Ok(old);
            }
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
            place.write(self, HirOperand::Local(dst), ty, block)?;
            return Ok(old);
        }
        if let Some(call) = self.sema.operator_calls.get(&(self.module, span)).copied() {
            let receiver_ty = self.expr_type(operand.span())?;
            let signature = self.protocol_signature(&receiver_ty, call.method, span)?;
            let value = self.build_expression(operand, block)?;
            let receiver = self.protocol_receiver_value(value, &receiver_ty, block);
            return Ok(self.emit_protocol_call(signature, vec![receiver], false, block));
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
        if let Some(call) = self.sema.operator_calls.get(&(self.module, span)).copied() {
            let receiver_expr = if call.reversed { right } else { left };
            let receiver_ty = self.expr_type(receiver_expr.span())?;
            let signature = self.protocol_signature(&receiver_ty, call.method, span)?;
            // 反向派发只交换传参，不交换源码左右操作数的求值顺序。
            let (receiver, argument) = if call.reversed {
                let argument = self.build_call_argument(left, &signature.params[0], block)?;
                let value = self.build_expression(right, block)?;
                (
                    self.protocol_receiver_value(value, &receiver_ty, block),
                    argument,
                )
            } else {
                let value = self.build_expression(left, block)?;
                let receiver = self.protocol_receiver_value(value, &receiver_ty, block);
                let argument = self.build_call_argument(right, &signature.params[0], block)?;
                (receiver, argument)
            };
            return Ok(self.emit_protocol_call(
                signature,
                vec![receiver, argument],
                call.invert,
                block,
            ));
        }
        // 数值比较的结果是 boolean，不能用它反推操作数位宽。
        let common = self.binary_numeric(span)?.or_else(|| {
            // 两个未定型浮点字面量用于比较时没有外层数值上下文。
            (binary_op_of(op).is_some_and(is_comparison)
                && is_float_literal_expression(left)
                && is_float_literal_expression(right))
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
        let lhs = if matches!(
            right,
            Expression::Literal {
                value: novella_ast::expr::Literal::Null,
                ..
            }
        ) {
            self.build_immediate_value(left, &value_ty, block)?
        } else {
            self.build_typed_expression(left, &value_ty, block)?
        };
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
        discarded: bool,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let ty = self.expr_type(target.span())?;
        // 普通赋值先完成 RHS；复合赋值先定位并读取旧值。
        let early_rhs = if op == AssignOp::Assign {
            let can_borrow = discarded && self.noninterfering_assignment_target(target);
            Some(if can_borrow {
                self.build_immediate_value(value, &ty, block)?
            } else {
                self.build_typed_expression(value, &ty, block)?
            })
        } else {
            None
        };
        let place = self.update_place(target, op == AssignOp::Assign, block)?;
        let previous = (op != AssignOp::Assign)
            .then(|| place.read(self, &ty, block))
            .transpose()?;
        let stored = if let Some(rhs) = early_rhs {
            rhs
        } else if let Some(lhs) = previous {
            if let Some(call) = self.sema.operator_calls.get(&(self.module, span)).copied() {
                let receiver_ty = if call.reversed {
                    self.expr_type(value.span())?
                } else {
                    ty.clone()
                };
                let signature = self.protocol_signature(&receiver_ty, call.method, span)?;
                let (receiver, argument) = if call.reversed {
                    let argument =
                        self.coerce(lhs, &ty, &signature.params[0], target.span(), block)?;
                    let value = self.build_expression(value, block)?;
                    (
                        self.protocol_receiver_value(value, &receiver_ty, block),
                        argument,
                    )
                } else {
                    let receiver = self.protocol_receiver_value(lhs, &receiver_ty, block);
                    let argument = self.build_call_argument(value, &signature.params[0], block)?;
                    (receiver, argument)
                };
                let from = signature.ret.clone();
                let result =
                    self.emit_protocol_call(signature, vec![receiver, argument], false, block);
                self.coerce(result, &from, &ty, span, block)?
            } else {
                let rhs_ty = if matches!(ty, HirType::Pointer(_)) {
                    self.expr_type_pref(value.span(), Some(&HirType::Int(IntTy::Isize)))?
                } else {
                    ty.clone()
                };
                let rhs = self.build_typed_expression(value, &rhs_ty, block)?;
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
            unreachable!("复合赋值必须先读取旧值")
        };
        place.write(self, stored.clone(), ty, block)?;
        Ok(stored)
    }

    /// 运算符按值读取 struct；其方法 ABI 需要指针，因此给快照单独建槽。
    /// 这也隔离后缀方法对 self 的修改，保住表达式要返回的旧值。
    fn protocol_receiver_value(
        &mut self,
        value: HirOperand,
        ty: &HirType,
        block: &mut HirBlock,
    ) -> HirOperand {
        if !matches!(ty, HirType::Struct(..)) {
            return value;
        }
        let id = self.fresh_local(ty.clone());
        block.push(HirStmt::Instr(HirInstr::Alloc {
            id,
            ty: ty.clone(),
            init: Some(value),
        }));
        self.local_address(id, block)
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
        let fields = instance_fields(self.sema, &self.instances, module, name, span)?;
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
                borrow: false,
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
            borrow: false,
            dst,
        }));
        Ok(Some(HirOperand::Local(dst)))
    }
}

pub(super) fn same_string_class(sema: &SemaResult, ty: &HirType) -> bool {
    matches!((&sema.string_type, ty),
        (Some(TypeView::Class(expected_module, expected_name)), HirType::Class(module, name))
        if expected_module == module && expected_name == name)
}

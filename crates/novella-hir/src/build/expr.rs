//! HIR 降级：expr 职责实现。

use super::*;

impl<'a> BodyBuilder<'a> {
    // ---------- 表达式 ----------

    /// 降级表达式：把求值顺序展开成指令，返回结果操作数。
    ///
    /// 所有 `dst` 的类型都来自 sema 表达式类型表，codegen 不再推导。
    pub(super) fn build_expression(
        &mut self,
        expr: &Expression,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        self.build_expression_inner(expr, None, block)
    }

    /// 把上下文期望类型下推到复合表达式，并在出口完成定型 / 转换。
    pub(super) fn build_typed_expression(
        &mut self,
        expr: &Expression,
        expected: &HirType,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let value = self.build_expression_inner(expr, Some(expected), block)?;
        let actual = self.expr_type_pref(expr.span(), Some(expected))?;
        self.coerce(value, &actual, expected, expr.span(), block)
    }

    fn build_expression_inner(
        &mut self,
        expr: &Expression,
        preferred: Option<&HirType>,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        match expr {
            Expression::Literal { value, span } => self.build_literal(value, *span, block),
            Expression::Identifier { name, span } => {
                if let Some(id) = self.lookup_local(name) {
                    if name == "self"
                        && let HirType::Pointer(inner) = self.local_type(id)
                        && matches!(inner.as_ref(), HirType::Struct(..))
                    {
                        return Ok(self.read_address(HirOperand::Local(id), &inner, block));
                    }
                    // 先保存值快照，后续实参 / 子表达式的修改不会改变此前读值。
                    let dst = self.fresh_local(self.local_type(id));
                    block.push(HirStmt::Instr(HirInstr::Load { dst, src: id }));
                    return Ok(HirOperand::Local(dst));
                }
                if let Some(address) = super::globals::address(self.sema, self.module, *span) {
                    let ty = self.expr_type(*span)?;
                    return Ok(self.read_address(address, &ty, block));
                }
                if self.capture_names.contains(name) {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "lambda capture",
                    })
                    .with_site(self.module, *span));
                }
                if let Some((_, id)) = self.functions.iter().find(|(n, _)| n == name) {
                    return Ok(HirOperand::FunctionPtr(id.clone()));
                }
                if let Some(TypeView::Function(view)) =
                    self.sema.expression_types.get(self.module, *span)
                    && view.name.is_some()
                    && !view.receiver
                {
                    return Ok(HirOperand::FunctionPtr(function_id_of(view)));
                }
                // 隐式字段与 self 共享布局。
                if let Some(operand) = self.implicit_member(name, block)? {
                    return Ok(operand);
                }
                Err(
                    BuildError::at(BuildErrorKind::Unimplemented { node: "identifier" })
                        .with_site(self.module, *span),
                )
            }
            Expression::Paren { expr, .. } => self.build_expression_inner(expr, preferred, block),
            Expression::Unary { op, operand, span } => {
                self.build_unary(*op, operand, *span, preferred, block)
            }
            Expression::Binary {
                op,
                left,
                right,
                span,
            } => self.build_binary(*op, left, right, *span, preferred, block),
            Expression::Assignment {
                target,
                op,
                value,
                span,
            } => self.build_assignment(target, *op, value, *span, block),
            Expression::Call {
                callee,
                args,
                span,
                type_args,
            } => {
                if !type_args.is_empty() {
                    return Err(BuildError::at(BuildErrorKind::GenericUnsupported)
                        .with_site(self.module, *span));
                }
                self.build_call(callee, args, *span, block)
            }
            Expression::Ternary {
                condition,
                then_expr,
                else_expr,
                span,
            } => {
                let result_ty = self.expr_type_pref(*span, preferred)?;
                let dst = self.fresh_local(result_ty.clone());
                block.push(HirStmt::Instr(HirInstr::Alloc {
                    id: dst,
                    ty: result_ty.clone(),
                    init: None,
                }));
                let cond = self.build_expression(condition, block)?;
                let mut then_block = HirBlock::new();
                let then_value =
                    self.build_typed_expression(then_expr, &result_ty, &mut then_block)?;
                then_block.push(HirStmt::Instr(HirInstr::Store {
                    id: dst,
                    value: then_value,
                }));
                let mut else_block = HirBlock::new();
                let else_value =
                    self.build_typed_expression(else_expr, &result_ty, &mut else_block)?;
                else_block.push(HirStmt::Instr(HirInstr::Store {
                    id: dst,
                    value: else_value,
                }));
                block.push(HirStmt::If {
                    cond: HirCond::new(cond),
                    then_block,
                    else_block: Some(else_block),
                });
                Ok(HirOperand::Local(dst))
            }
            Expression::Is { operand, ty, .. } => {
                let target = annotation_type(self.sema, self.module, ty)?;
                let operand_ty = self.expr_type(operand.span())?;
                let value = self.build_typed_expression(operand, &operand_ty, block)?;
                let dst = self.fresh_local(HirType::Boolean);
                block.push(HirStmt::Instr(HirInstr::TypeTest {
                    operand: value,
                    operand_ty,
                    target,
                    dst,
                }));
                Ok(HirOperand::Local(dst))
            }
            Expression::Cast { expr, ty, span } => {
                let target = annotation_type(self.sema, self.module, ty)?;
                let from = self.expr_type_pref(expr.span(), Some(&target))?;
                let value = self.build_expression_inner(expr, Some(&from), block)?;
                self.coerce(value, &from, &target, *span, block)
            }
            Expression::Match(node) => self.build_match(node, preferred, block),
            Expression::MemberAccess { target, name, span } => {
                if let Some(address) = super::globals::address(self.sema, self.module, *span) {
                    let ty = self.expr_type(*span)?;
                    return Ok(self.read_address(address, &ty, block));
                }
                // 方法当作值：`obj.m` / `Type.static` 取函数指针
                if let Ok(HirType::Function(signature)) = self.expr_type_pref(*span, None) {
                    if signature.receiver {
                        return Err(BuildError::at(BuildErrorKind::Unimplemented {
                            node: "bound method value",
                        })
                        .with_site(self.module, *span));
                    }
                    return Ok(HirOperand::FunctionPtr(signature.id.clone()));
                }
                let value = self.build_expression(target, block)?;
                let target_ty = self.expr_type(target.span())?;
                let (index, ty) = self.field_slot(&target_ty, name, *span)?;
                let dst = self.fresh_local(ty.clone());
                block.push(HirStmt::Instr(HirInstr::FieldLoad {
                    target: value,
                    target_ty,
                    field: name.clone(),
                    index,
                    ty,
                    dst,
                }));
                Ok(HirOperand::Local(dst))
            }
            Expression::IndexAccess {
                target,
                index,
                span,
            } => {
                let target_ty = self.expr_type(target.span())?;
                if operators::same_string_class(self.sema, &target_ty) {
                    let receiver = self.build_expression(target, block)?;
                    let index =
                        self.build_typed_expression(index, &HirType::Int(IntTy::Usize), block)?;
                    return self.string_method_call(
                        &target_ty,
                        "codeUnitAt",
                        vec![receiver, index],
                        *span,
                        block,
                    );
                }
                self.check_index_target(target, *span)?;
                let value = self.build_expression(target, block)?;
                // 元素类型 = 表达式本身的类型；目标类型用于 codegen 选 GEP 的步长
                let element_ty = self.expr_type_pref(*span, None)?;
                let index_value =
                    self.build_typed_expression(index, &HirType::Int(IntTy::Usize), block)?;
                let dst = self.fresh_local(element_ty.clone());
                block.push(HirStmt::Instr(HirInstr::IndexLoad {
                    target: value,
                    element_ty,
                    index: index_value,
                    dst,
                }));
                Ok(HirOperand::Local(dst))
            }
            Expression::StructInit {
                name,
                properties,
                span,
                ..
            } => self.build_struct_init(name, properties, *span, block),
            Expression::Lambda(node) => self.build_lambda(node),
            Expression::New {
                target,
                type_args,
                args,
                span,
            } => self.build_new(target, type_args, args, *span, block),
            Expression::Await { span, .. } => {
                Err(
                    BuildError::at(BuildErrorKind::Unimplemented { node: "await" })
                        .with_site(self.module, *span),
                )
            }
            Expression::Tuple { elements, span } => {
                let ty = self.expr_type_pref(*span, preferred)?;
                let HirType::Tuple(types) = &ty else {
                    unreachable!("元组表达式的类型是 Tuple")
                };
                let values = elements
                    .iter()
                    .zip(types)
                    .map(|(expr, ty)| self.build_typed_expression(expr, ty, block))
                    .collect::<Result<Vec<_>, _>>()?;
                let dst = self.fresh_local(ty.clone());
                block.push(HirStmt::Instr(HirInstr::Tuple {
                    elements: values,
                    ty,
                    dst,
                }));
                Ok(HirOperand::Local(dst))
            }
            Expression::ArrayLiteral { elements, span } => {
                let HirType::Array(element_ty) = self.expr_type_pref(*span, preferred)? else {
                    unreachable!("数组表达式的类型是 Array")
                };
                let values = elements
                    .iter()
                    .map(|expr| self.build_typed_expression(expr, &element_ty, block))
                    .collect::<Result<Vec<_>, _>>()?;
                let dst = self.fresh_local(HirType::Array(element_ty.clone()));
                block.push(HirStmt::Instr(HirInstr::Array {
                    elements: values,
                    element_ty: *element_ty,
                    dst,
                }));
                Ok(HirOperand::Local(dst))
            }
            Expression::ObjectLiteral { span, .. } => {
                Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "object literal",
                })
                .with_site(self.module, *span))
            }
            Expression::This { span } => {
                let id = self.lookup_local("self").ok_or_else(|| {
                    BuildError::at(BuildErrorKind::Unimplemented {
                        node: "this outside method",
                    })
                    .with_site(self.module, *span)
                })?;
                match self.local_type(id) {
                    HirType::Pointer(inner) if matches!(inner.as_ref(), HirType::Struct(..)) => {
                        Ok(self.read_address(HirOperand::Local(id), &inner, block))
                    }
                    _ => Ok(HirOperand::Local(id)),
                }
            }
        }
    }
}

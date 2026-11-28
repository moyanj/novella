//! HIR 指令发射。

use super::*;

impl<'ctx> Body<'ctx, '_> {
    pub(super) fn instruction(&mut self, instruction: &HirInstr) -> Result<bool> {
        match instruction {
            HirInstr::Adopt { id, ty, init } => {
                let value = self.operand(&HirOperand::Local(*init), ty)?;
                if let Some(index) = self.owned.iter().rposition(|active| active == init) {
                    self.owned.remove(index);
                } else if self.is_managed(ty) {
                    self.reference_value("novella_rt_retain", value, ty)?;
                }
                self.save(*id, ty, value)?;
            }
            HirInstr::Alloc { id, ty, init } => {
                let value = init
                    .as_ref()
                    .map(|operand| self.operand(operand, ty))
                    .transpose()?;
                let pointer = self.allocate(*id, ty)?;
                if let Some(value) = value {
                    if self.is_managed(ty) {
                        self.reference_value("novella_rt_retain", value, ty)?;
                    }
                    self.builder.build_store(pointer, value)?;
                } else if self.is_managed(ty) {
                    self.builder.build_store(
                        pointer,
                        value_type(self.context, self.types, ty)?.const_zero(),
                    )?;
                }
            }
            HirInstr::DynPack {
                operand,
                source,
                witness,
                ty,
                dst,
            } => {
                self.pack_dynamic(operand, source, witness, ty, *dst)?;
            }
            HirInstr::DynCall {
                receiver,
                receiver_ty,
                signature,
                slot,
                args,
                dst,
            } => {
                self.call_dynamic((receiver, receiver_ty), signature, *slot, args, *dst)?;
            }
            HirInstr::StringLiteral { value, ty, dst } => {
                let object = self.string_literal(value, ty)?;
                self.save(*dst, ty, object.into())?;
            }
            HirInstr::Load { dst, src } => {
                let ty = self.slot(*src)?.1.clone();
                let value = self.operand(&HirOperand::Local(*src), &ty)?;
                if self.borrowed.contains(src) {
                    self.borrowed.insert(*dst);
                } else if self.is_managed(&ty) {
                    self.reference_value("novella_rt_retain", value, &ty)?;
                }
                self.save(*dst, &ty, value)?;
            }
            HirInstr::Store { id, value } => {
                let (pointer, ty) = self.slot(*id)?;
                let pointer = *pointer;
                let ty = ty.clone();
                let new_value = self.operand(value, &ty)?;
                if self.is_managed(&ty) {
                    self.reference_value("novella_rt_retain", new_value, &ty)?;
                    let old_value = self.builder.build_load(
                        value_type(self.context, self.types, &ty)?,
                        pointer,
                        "previous_local",
                    )?;
                    self.builder.build_store(pointer, new_value)?;
                    self.reference_value("novella_rt_release", old_value, &ty)?;
                } else {
                    self.builder.build_store(pointer, new_value)?;
                }
            }
            HirInstr::LocalAddress { local, ty, dst } => {
                let (pointer, actual) = self.slot(*local)?;
                if !actual.same_value_type(ty) {
                    return Err(CodegenError("局部变量取址类型不匹配".into()));
                }
                self.save(
                    *dst,
                    &HirType::Pointer(Box::new(ty.clone())),
                    (*pointer).into(),
                )?;
            }
            HirInstr::StoreIndirect { address, value, ty } => {
                let address = self
                    .operand(address, &HirType::Pointer(Box::new(ty.clone())))?
                    .into_pointer_value();
                let new_value = self.operand(value, ty)?;
                if self.is_managed(ty) {
                    self.reference_value("novella_rt_retain", new_value, ty)?;
                    let old_value = self.builder.build_load(
                        value_type(self.context, self.types, ty)?,
                        address,
                        "previous_indirect",
                    )?;
                    self.builder.build_store(address, new_value)?;
                    self.reference_value("novella_rt_release", old_value, ty)?;
                } else {
                    self.builder.build_store(address, new_value)?;
                }
            }
            HirInstr::New {
                ty,
                init,
                args,
                dst,
            } => {
                let object = self.runtime_alloc(ty)?;
                self.save(*dst, ty, object.into())?;
                if let Some(signature) = init {
                    let id = &signature.id;
                    let function = self
                        .functions
                        .get(id)
                        .ok_or_else(|| CodegenError(format!("未定义构造函数 `{id}`")))?;
                    let mut values: Vec<BasicMetadataValueEnum<'ctx>> =
                        Vec::with_capacity(args.len() + 1);
                    values.push(object.into());
                    values.extend(
                        args.iter()
                            .zip(&signature.params)
                            .map(|(arg, arg_ty)| {
                                self.operand(arg, arg_ty).map(BasicMetadataValueEnum::from)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                    let call = self.builder.build_call(function.0, &values, "init")?;
                    if !call.try_as_basic_value().basic().is_none() {
                        return Err(CodegenError("class __init 必须返回 void".into()));
                    }
                }
            }
            HirInstr::Unary {
                op: HirUnaryOp::Deref,
                operand,
                dst,
                dst_ty,
            } => {
                let address = self
                    .operand(operand, &HirType::Pointer(Box::new(dst_ty.clone())))?
                    .into_pointer_value();
                let value = self.builder.build_load(
                    value_type(self.context, self.types, dst_ty)?,
                    address,
                    "deref",
                )?;
                if self.is_managed(dst_ty) {
                    self.reference_value("novella_rt_retain", value, dst_ty)?;
                }
                self.save(*dst, dst_ty, value)?;
            }
            HirInstr::Unary {
                op,
                operand,
                dst,
                dst_ty,
            } => {
                let value = self.unary(*op, operand, dst_ty)?;
                self.save(*dst, dst_ty, value)?;
            }
            HirInstr::Binary {
                op,
                lhs,
                rhs,
                lhs_ty,
                rhs_ty,
                dst,
                dst_ty,
            } => {
                let value = self.binary(*op, (lhs, lhs_ty), (rhs, rhs_ty))?;
                self.save(*dst, dst_ty, value)?;
            }
            HirInstr::Cast {
                operand,
                from,
                to,
                dst,
            } => {
                let value = self.cast(operand, from, to)?;
                if self.is_managed(to) {
                    self.reference_value("novella_rt_retain", value, to)?;
                }
                self.save(*dst, to, value)?;
            }
            HirInstr::Call {
                callee,
                args,
                dst,
                dst_ty,
            } => {
                self.call(callee, args, *dst, dst_ty.as_ref())?;
            }
            HirInstr::Return(value) => {
                let keep = value.as_ref().and_then(|operand| match operand {
                    HirOperand::Local(id) if self.is_managed(&self.slot(*id).ok()?.1) => Some(*id),
                    _ => None,
                });
                let value = value
                    .as_ref()
                    .map(|operand| self.operand(operand, self.ret))
                    .transpose()?;
                if value.is_none() != (*self.ret == HirType::Void) {
                    return Err(CodegenError("返回值与函数签名不一致".into()));
                }
                if keep.is_some_and(|id| self.borrowed.contains(&id)) {
                    // 返回借用接收者时建立返回值所有权；普通局部则直接转移。
                    self.reference_value(
                        "novella_rt_retain",
                        value.expect("已有返回值"),
                        self.ret,
                    )?;
                }
                self.release_locals(keep)?;
                self.builder.build_return(
                    value
                        .as_ref()
                        .map(|value| value as &dyn inkwell::values::BasicValue),
                )?;
                return Ok(false);
            }
            HirInstr::FieldAddress {
                target,
                target_ty,
                index,
                ty,
                dst,
            } => {
                if self.types.field_is_weak(target_ty, *index)? {
                    return Err(CodegenError("weak 字段不能取裸地址".into()));
                }
                let pointer = self.field_pointer(target, target_ty, *index)?;
                self.save(
                    *dst,
                    &HirType::Pointer(Box::new(ty.clone())),
                    pointer.into(),
                )?;
            }
            HirInstr::FieldLoad {
                target,
                target_ty,
                index,
                ty,
                borrow,
                dst,
                ..
            } => {
                let pointer = self.field_pointer(target, target_ty, *index)?;
                let value = self.builder.build_load(
                    value_type(self.context, self.types, ty)?,
                    pointer,
                    "field_load",
                )?;
                let value = if self.types.field_is_weak(target_ty, *index)? {
                    self.runtime_weak_value("novella_rt_weak_upgrade", value.into_pointer_value())?
                        .into()
                } else {
                    if *borrow {
                        self.borrowed.insert(*dst);
                    } else if self.is_managed(ty) {
                        self.reference_value("novella_rt_retain", value, ty)?;
                    }
                    value
                };
                self.save(*dst, ty, value)?;
            }
            HirInstr::FieldStore {
                target,
                target_ty,
                index,
                value,
                is_weak,
                ..
            } => {
                let pointer = self.field_pointer(target, target_ty, *index)?;
                let field_ty = self.types.field_type(target_ty, *index)?;
                let value = self.operand(value, &field_ty)?;
                if *is_weak != self.types.field_is_weak(target_ty, *index)? {
                    return Err(CodegenError("HIR weak 字段属性与布局不一致".into()));
                }
                if *is_weak {
                    let handle = self
                        .runtime_weak_value("novella_rt_weak_create", value.into_pointer_value())?;
                    let previous = self.builder.build_load(
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        pointer,
                        "previous_weak",
                    )?;
                    self.builder.build_store(pointer, handle)?;
                    self.runtime_reference_op(
                        "novella_rt_weak_release",
                        previous.into_pointer_value(),
                    )?;
                } else if self.is_managed(&field_ty) {
                    self.reference_value("novella_rt_retain", value, &field_ty)?;
                    let previous = self.builder.build_load(
                        value_type(self.context, self.types, &field_ty)?,
                        pointer,
                        "previous_field_value",
                    )?;
                    self.builder.build_store(pointer, value)?;
                    self.reference_value("novella_rt_release", previous, &field_ty)?;
                } else {
                    self.builder.build_store(pointer, value)?;
                }
            }
            HirInstr::IndexLoad {
                target,
                element_ty,
                index,
                dst,
            } => {
                let address = self.index_address(target, index, element_ty)?;
                let element = value_type(self.context, self.types, element_ty)?;
                let value = self.builder.build_load(element, address, "index_load")?;
                if self.is_managed(element_ty) {
                    self.reference_value("novella_rt_retain", value, element_ty)?;
                }
                self.save(*dst, element_ty, value)?;
            }
            HirInstr::IndexAddress {
                target,
                index,
                element_ty,
                dst,
            } => {
                let address = self.index_address(target, index, element_ty)?;
                self.save(
                    *dst,
                    &HirType::Pointer(Box::new(element_ty.clone())),
                    address.into(),
                )?;
            }
            HirInstr::Array {
                elements,
                element_ty,
                dst,
            } => {
                let length = u32::try_from(elements.len())
                    .map_err(|_| CodegenError("数组元素数量超出 u32 范围".into()))?;
                let ty = HirType::Array(Box::new(element_ty.clone()), length);
                let value = self.array_literal(elements, element_ty, length)?;
                self.save(*dst, &ty, value.into())?;
            }
            HirInstr::Tuple { elements, ty, dst } => {
                let HirType::Tuple(element_types) = ty else {
                    return Err(CodegenError("Tuple 指令的类型不是元组".into()));
                };
                if elements.len() != element_types.len() {
                    return Err(CodegenError("元组元素数与类型不一致".into()));
                }
                let mut value = value_type(self.context, self.types, ty)?
                    .into_struct_type()
                    .const_zero();
                for (index, (operand, element_ty)) in elements.iter().zip(element_types).enumerate()
                {
                    let element = self.operand(operand, element_ty)?;
                    self.reference_value("novella_rt_retain", element, element_ty)?;
                    value = self
                        .builder
                        .build_insert_value(value, element, index as u32, "tuple")?
                        .into_struct_value();
                }
                self.save(*dst, ty, value.into())?;
            }
            HirInstr::TupleExtract {
                target,
                index,
                ty,
                dst,
            } => {
                let HirOperand::Local(id) = target else {
                    return Err(CodegenError("元组提取目标必须已定型".into()));
                };
                let tuple_ty = self.slot(*id)?.1.clone();
                let value = self.operand(target, &tuple_ty)?.into_struct_value();
                let element =
                    self.builder
                        .build_extract_value(value, *index as u32, "tuple_element")?;
                self.reference_value("novella_rt_retain", element, ty)?;
                self.save(*dst, ty, element)?;
            }
            HirInstr::TypeTest { .. } => {
                return Err(CodegenError(format!("暂不支持 HIR 指令 {instruction:?}")));
            }
        }
        Ok(true)
    }
}

//! HIR 指令发射。

use super::*;

impl<'ctx> Body<'ctx, '_> {
    pub(super) fn instruction(&mut self, instruction: &HirInstr) -> Result<bool> {
        match instruction {
            HirInstr::Alloc { id, ty, init } => {
                let value = init
                    .as_ref()
                    .map(|operand| self.operand(operand, ty))
                    .transpose()?;
                let pointer = self.allocate(*id, ty)?;
                if let Some(value) = value {
                    if Self::is_managed(ty) {
                        self.reference_value("novella_rt_retain", value, ty)?;
                    }
                    self.builder.build_store(pointer, value)?;
                } else if Self::is_managed(ty) {
                    self.builder.build_store(
                        pointer,
                        value_type(self.context, self.types, ty)?.const_zero(),
                    )?;
                }
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
                } else if Self::is_managed(&ty) {
                    self.reference_value("novella_rt_retain", value, &ty)?;
                }
                self.save(*dst, &ty, value)?;
            }
            HirInstr::Store { id, value } => {
                let (pointer, ty) = self.slot(*id)?;
                let pointer = *pointer;
                let ty = ty.clone();
                let new_value = self.operand(value, &ty)?;
                if Self::is_managed(&ty) {
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
                if actual != ty {
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
                if Self::is_managed(ty) {
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
                        return Err(CodegenError("class __init__ 必须返回 void".into()));
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
                if Self::is_managed(dst_ty) {
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
                if Self::is_managed(to) {
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
                let (HirOperand::Function(id) | HirOperand::FunctionPtr(id)) = callee else {
                    return Err(CodegenError("暂不支持间接调用".into()));
                };
                let &(callee, signature) = self
                    .functions
                    .get(id)
                    .ok_or_else(|| CodegenError(format!("未定义 HIR 函数 `{id}`")))?;
                let receiver_count = usize::from(signature.receiver);
                if args.len() != signature.params.len() + receiver_count {
                    return Err(CodegenError(format!("`{id}` 实参个数与签名不一致")));
                }
                let mut values: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(args.len());
                if signature.receiver {
                    let receiver_ty = match args.first() {
                        Some(HirOperand::Local(id)) => self.slot(*id)?.1.clone(),
                        _ => return Err(CodegenError(format!("`{id}` 接收者不是已定型局部值"))),
                    };
                    if !matches!(receiver_ty, HirType::Class(..) | HirType::Pointer(_)) {
                        return Err(CodegenError(format!(
                            "`{id}` 接收者类型不合法：{receiver_ty:?}"
                        )));
                    }
                    values.push(self.operand(&args[0], &receiver_ty)?.into());
                }
                values.extend(
                    args[receiver_count..]
                        .iter()
                        .zip(&signature.params)
                        .map(|(operand, ty)| {
                            self.operand(operand, ty).map(BasicMetadataValueEnum::from)
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                let call = self.builder.build_call(callee, &values, "call")?;
                for (location, attribute) in crate::abi::extensions(self.context, signature) {
                    call.add_attribute(location, attribute);
                }
                if let Some(dst) = dst {
                    let value = call
                        .try_as_basic_value()
                        .basic()
                        .filter(|_| dst_ty.as_ref() == Some(&signature.ret))
                        .ok_or_else(|| {
                            CodegenError(format!("`{id}` 调用的返回类型与 HIR 不一致"))
                        })?;
                    self.save(*dst, &signature.ret, value)?;
                }
            }
            HirInstr::Return(value) => {
                let keep = value.as_ref().and_then(|operand| match operand {
                    HirOperand::Local(id) if Self::is_managed(&self.slot(*id).ok()?.1) => Some(*id),
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
                    if Self::is_managed(ty) {
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
                } else if Self::is_managed(&field_ty) {
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
                if Self::is_managed(element_ty) {
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
                let object = self.array_literal(elements, element_ty)?;
                self.save(
                    *dst,
                    &HirType::Array(Box::new(element_ty.clone())),
                    object.into(),
                )?;
            }
            HirInstr::ArrayLength { target, dst } => {
                let length = self.array_length(target)?;
                self.save(
                    *dst,
                    &HirType::Int(novella_hir::IntTy::Usize),
                    length.into(),
                )?;
            }
            HirInstr::IndexStore {
                target,
                index,
                element_ty,
                value,
            } => {
                let slot = self.index_address(target, index, element_ty)?;
                let value = self.operand(value, element_ty)?;
                self.reference_value("novella_rt_retain", value, element_ty)?;
                let previous = self.builder.build_load(
                    value_type(self.context, self.types, element_ty)?,
                    slot,
                    "previous_element",
                )?;
                self.builder.build_store(slot, value)?;
                self.reference_value("novella_rt_release", previous, element_ty)?;
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

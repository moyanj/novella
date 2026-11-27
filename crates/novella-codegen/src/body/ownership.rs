//! 局部所有权、对象分配与字符串物化。

use super::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

impl<'ctx> Body<'ctx, '_> {
    pub(super) fn is_managed(&self, ty: &HirType) -> bool {
        crate::ownership::has_owned_members(self.types, ty)
    }

    pub(super) fn save_parameter(
        &mut self,
        id: LocalId,
        ty: &HirType,
        value: BasicValueEnum<'ctx>,
    ) -> Result<()> {
        self.save(id, ty, value)?;
        if self.is_managed(ty) {
            self.reference_value("novella_rt_retain", value, ty)?;
        }
        Ok(())
    }

    pub(super) fn release_locals(&mut self, keep: Option<LocalId>) -> Result<()> {
        self.release_owned(0, keep)
    }

    pub(super) fn release_owned(&mut self, start: usize, keep: Option<LocalId>) -> Result<()> {
        let ids: Vec<_> = self.owned[start..].iter().rev().copied().collect();
        for id in ids {
            if keep == Some(id) {
                continue;
            }
            self.release_slot(id)?;
        }
        Ok(())
    }

    pub(super) fn release_temps(&mut self, ids: &[LocalId]) -> Result<()> {
        for id in ids.iter().rev().copied() {
            if let Some(index) = self.owned.iter().rposition(|active| *active == id) {
                self.release_slot(id)?;
                self.owned.remove(index);
            }
        }
        Ok(())
    }

    fn release_slot(&mut self, id: LocalId) -> Result<()> {
        let (pointer, ty) = self.slot(id)?.clone();
        let value_ty = value_type(self.context, self.types, &ty)?;
        let value = self
            .builder
            .build_load(value_ty, pointer, "release_local")?;
        // 清空后再析构，后续合流及循环再次进入时不会重复释放旧值。
        self.builder.build_store(pointer, value_ty.const_zero())?;
        self.reference_value("novella_rt_release", value, &ty)
    }

    pub(super) fn aggregate_type(&self, ty: &HirType) -> Result<inkwell::types::StructType<'ctx>> {
        let (module, name) = match ty {
            HirType::Struct(module, name) | HirType::Class(module, name) => (*module, name),
            HirType::Pointer(inner) => return self.aggregate_type(inner),
            _ => return Err(CodegenError(format!("不是可寻址聚合类型：{ty:?}"))),
        };
        self.types.aggregate(module, name)
    }

    pub(super) fn field_pointer(
        &self,
        target: &HirOperand,
        target_ty: &HirType,
        index: usize,
    ) -> Result<PointerValue<'ctx>> {
        let aggregate = self.aggregate_type(target_ty)?;
        let pointer_ty = HirType::Pointer(Box::new(target_ty.clone()));
        let value = match target_ty {
            HirType::Class(..) => self.operand(target, target_ty)?,
            HirType::Struct(..) => match target {
                HirOperand::Local(id) if self.slot(*id)?.1 == *target_ty => {
                    self.slot(*id)?.0.into()
                }
                _ => self.operand(target, &pointer_ty)?,
            },
            HirType::Pointer(_) => self.operand(target, target_ty)?,
            _ => unreachable!(),
        };
        let pointer = value.into_pointer_value();
        let pointer_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let pointer = if pointer.get_type() == pointer_type {
            pointer
        } else {
            self.builder
                .build_pointer_cast(pointer, pointer_type, "aggregate_ptr")?
        };
        aggregate
            .get_field_type_at_index(index as u32)
            .ok_or_else(|| CodegenError(format!("字段下标越界：{index}")))?;
        Ok(self
            .builder
            .build_struct_gep(aggregate, pointer, index as u32, "field")?)
    }

    pub(super) fn runtime_alloc(&mut self, ty: &HirType) -> Result<PointerValue<'ctx>> {
        let aggregate = self.aggregate_type(ty)?;
        let layout = self.module.get_data_layout();
        let target = inkwell::targets::TargetData::create(
            layout
                .as_str()
                .to_str()
                .map_err(|_| CodegenError("目标布局不是有效 UTF-8".into()))?,
        );
        let size = self
            .context
            .i64_type()
            .const_int(target.get_abi_size(&aggregate), false);
        let alloc_ty = self
            .context
            .ptr_type(inkwell::AddressSpace::default())
            .fn_type(
                &[
                    self.context.i64_type().into(),
                    self.context.i64_type().into(),
                    self.context
                        .ptr_type(inkwell::AddressSpace::default())
                        .into(),
                ],
                false,
            );
        let alloc = self
            .module
            .get_function("novella_rt_alloc")
            .unwrap_or_else(|| self.module.add_function("novella_rt_alloc", alloc_ty, None));
        let size = self
            .builder
            .build_int_cast(size, self.context.i64_type(), "object_size")?;
        let alignment = self
            .context
            .i64_type()
            .const_int(u64::from(target.get_abi_alignment(&aggregate)), false);
        // 实际分配才声明析构包装，未使用的 class 不产生 ARC 依赖。
        let name = crate::ownership::destructor_name(ty)?;
        let pointer = self.context.ptr_type(inkwell::AddressSpace::default());
        let destroy = self
            .module
            .get_function(&name)
            .unwrap_or_else(|| {
                self.module.add_function(
                    &name,
                    self.context.void_type().fn_type(&[pointer.into()], false),
                    Some(inkwell::module::Linkage::Internal),
                )
            })
            .as_global_value()
            .as_pointer_value();
        let call = self.builder.build_call(
            alloc,
            &[size.into(), alignment.into(), destroy.into()],
            "new_object",
        )?;
        call.try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError("runtime alloc 没有返回对象指针".into()))
            .map(|value| value.into_pointer_value())
    }

    pub(super) fn reference_value(
        &self,
        operation: &str,
        value: BasicValueEnum<'ctx>,
        ty: &HirType,
    ) -> Result<()> {
        crate::ownership::reference_op(
            self.context,
            self.module,
            self.types,
            &self.builder,
            operation,
            value,
            ty,
        )
    }

    pub(super) fn runtime_reference_op(
        &mut self,
        name: &str,
        object: PointerValue<'ctx>,
    ) -> Result<()> {
        crate::ownership::reference_call(self.context, self.module, &self.builder, name, object)
    }

    pub(super) fn runtime_weak_value(
        &mut self,
        name: &str,
        value: PointerValue<'ctx>,
    ) -> Result<PointerValue<'ctx>> {
        let pointer = self.context.ptr_type(inkwell::AddressSpace::default());
        let signature = pointer.fn_type(&[pointer.into()], false);
        let function = self
            .module
            .get_function(name)
            .unwrap_or_else(|| self.module.add_function(name, signature, None));
        self.builder
            .build_call(function, &[value.into()], "weak_value")?
            .try_as_basic_value()
            .basic()
            .map(|value| value.into_pointer_value())
            .ok_or_else(|| CodegenError(format!("{name} 没有返回引用")))
    }

    pub(super) fn string_literal(
        &mut self,
        value: &[u16],
        ty: &HirType,
    ) -> Result<PointerValue<'ctx>> {
        let static_string_ty = HirType::Pointer(Box::new(HirType::Int(novella_hir::IntTy::U16)));
        if *ty == static_string_ty {
            let pointer_type = self.context.ptr_type(inkwell::AddressSpace::default());
            if value.is_empty() {
                return Ok(pointer_type.const_null());
            }
            let mut hasher = DefaultHasher::new();
            value.hash(&mut hasher);
            let name = format!("__novella_static_string_{:x}", hasher.finish());
            let global = if let Some(global) = self.module.get_global(&name) {
                global
            } else {
                let elements = value
                    .iter()
                    .map(|unit| self.context.i16_type().const_int(u64::from(*unit), false))
                    .collect::<Vec<_>>();
                let array = self.context.i16_type().const_array(&elements);
                let global = self.module.add_global(array.get_type(), None, &name);
                global.set_initializer(&array);
                global.set_constant(true);
                global
            };
            let zero = self.context.i64_type().const_zero();
            let data = unsafe {
                self.builder.build_gep(
                    self.context.i16_type(),
                    global.as_pointer_value(),
                    &[zero, zero],
                    "static_string",
                )?
            };
            return Ok(data);
        }
        let HirType::Class(_, name) = ty else {
            return Err(CodegenError(format!(
                "字符串字面量的目标类型不是 class：{ty:?}"
            )));
        };
        if name != "String" {
            return Err(CodegenError(format!(
                "字符串字面量的目标 class 不是 std:String：{name}"
            )));
        }
        let expected_data = HirType::Pointer(Box::new(HirType::Int(novella_hir::IntTy::U16)));
        if self.types.field_type(ty, 0)? != expected_data
            || self.types.field_type(ty, 1)? != HirType::Int(novella_hir::IntTy::Usize)
        {
            return Err(CodegenError(
                "std:String 必须以 UTF-16 数据指针和 usize 长度开头".into(),
            ));
        }
        let object = self.runtime_alloc(ty)?;
        let aggregate = self.aggregate_type(ty)?;
        let data_field = self
            .builder
            .build_struct_gep(aggregate, object, 0, "string_data")?;
        let len_field = self
            .builder
            .build_struct_gep(aggregate, object, 1, "string_len")?;
        let pointer_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let data = if value.is_empty() {
            pointer_type.const_null()
        } else {
            let malloc_ty = pointer_type.fn_type(&[self.context.i64_type().into()], false);
            let malloc = self
                .module
                .get_function("novella_rt_malloc")
                .unwrap_or_else(|| {
                    self.module
                        .add_function("novella_rt_malloc", malloc_ty, None)
                });
            let bytes = value
                .len()
                .checked_mul(std::mem::size_of::<u16>())
                .ok_or_else(|| CodegenError("字符串字面量缓冲区大小溢出".into()))?;
            let bytes = self.context.i64_type().const_int(bytes as u64, false);
            self.builder
                .build_call(malloc, &[bytes.into()], "string_bytes")?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CodegenError("novella_rt_malloc 没有返回字符串缓冲区".into()))?
                .into_pointer_value()
        };
        if !value.is_empty() {
            self.trap_if(
                self.builder
                    .build_is_null(data, "string_allocation_failed")?,
            )?;
        }
        self.builder.build_store(data_field, data)?;
        self.builder.build_store(
            len_field,
            self.context.i64_type().const_int(value.len() as u64, false),
        )?;
        if !value.is_empty() {
            let data_type = self.context.i16_type();
            for (index, code_unit) in value.iter().copied().enumerate() {
                // LLVM 的 opaque pointer 允许直接以元素类型进行索引。
                let index = self.context.i64_type().const_int(index as u64, false);
                let slot = unsafe {
                    self.builder
                        .build_gep(data_type, data, &[index], "string_code_unit")?
                };
                self.builder
                    .build_store(slot, data_type.const_int(u64::from(code_unit), false))?;
            }
        }
        Ok(object)
    }
}

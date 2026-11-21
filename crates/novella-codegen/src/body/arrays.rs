//! 托管数组的载荷布局、元素生命周期与边界检查。

use super::*;
use inkwell::{
    IntPredicate,
    module::Linkage,
    targets::TargetData,
    types::{BasicType, StructType},
    values::IntValue,
};
use novella_hir::IntTy;

impl<'ctx> Body<'ctx, '_> {
    // 零长度尾数组与定长载荷共享元素偏移，由 LLVM 插入元素对齐所需的填充。
    fn array_layout(&self, element_ty: &HirType, count: u32) -> Result<StructType<'ctx>> {
        let element = value_type(self.context, self.types, element_ty)?;
        Ok(self.context.struct_type(
            &[
                self.context.i64_type().into(),
                element.array_type(count).into(),
            ],
            false,
        ))
    }

    pub(super) fn array_literal(
        &self,
        elements: &[HirOperand],
        element_ty: &HirType,
    ) -> Result<PointerValue<'ctx>> {
        let count = u32::try_from(elements.len())
            .map_err(|_| CodegenError("数组字面量元素数量超出 LLVM 布局上限".into()))?;
        let layout = self.array_layout(element_ty, count)?;
        let layout_string = self.module.get_data_layout();
        let target = TargetData::create(
            layout_string
                .as_str()
                .to_str()
                .map_err(|_| CodegenError("目标数据布局不是有效 UTF-8".into()))?,
        );
        let usize_ty = self.context.i64_type();
        let pointer_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let signature = pointer_ty.fn_type(
            &[usize_ty.into(), usize_ty.into(), pointer_ty.into()],
            false,
        );
        let alloc = self
            .module
            .get_function("novella_rt_alloc")
            .unwrap_or_else(|| {
                self.module
                    .add_function("novella_rt_alloc", signature, None)
            });
        let destroy = self.array_destructor(element_ty)?;
        let object = self
            .builder
            .build_call(
                alloc,
                &[
                    usize_ty
                        .const_int(target.get_abi_size(&layout), false)
                        .into(),
                    usize_ty
                        .const_int(u64::from(target.get_abi_alignment(&layout)), false)
                        .into(),
                    destroy.as_global_value().as_pointer_value().into(),
                ],
                "new_array",
            )?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError("runtime alloc 没有返回数组指针".into()))?
            .into_pointer_value();
        let length = self
            .builder
            .build_struct_gep(layout, object, 0, "array_length_slot")?;
        self.builder
            .build_store(length, usize_ty.const_int(u64::from(count), false))?;
        let data = self
            .builder
            .build_struct_gep(layout, object, 1, "array_data")?;
        let element = value_type(self.context, self.types, element_ty)?;
        for (index, operand) in elements.iter().enumerate() {
            let value = self.operand(operand, element_ty)?;
            crate::ownership::reference_op(
                self.context,
                self.module,
                self.types,
                &self.builder,
                "novella_rt_retain",
                value,
                element_ty,
            )?;
            let slot = unsafe {
                self.builder.build_gep(
                    element,
                    data,
                    &[usize_ty.const_int(index as u64, false)],
                    "array_element",
                )?
            };
            self.builder.build_store(slot, value)?;
        }
        Ok(object)
    }

    pub(super) fn array_length(&self, target: &HirOperand) -> Result<IntValue<'ctx>> {
        let HirOperand::Local(id) = target else {
            return Err(CodegenError("数组长度的目标必须是已定型局部值".into()));
        };
        let ty = &self.slot(*id)?.1;
        let HirType::Array(element_ty) = ty else {
            return Err(CodegenError(format!("不能读取非数组类型的长度：{ty:?}")));
        };
        let object = self.operand(target, ty)?.into_pointer_value();
        let slot = self.builder.build_struct_gep(
            self.array_layout(element_ty, 0)?,
            object,
            0,
            "array_length_slot",
        )?;
        Ok(self
            .builder
            .build_load(self.context.i64_type(), slot, "array_length")?
            .into_int_value())
    }

    pub(super) fn index_address(
        &self,
        target: &HirOperand,
        index: &HirOperand,
        element_ty: &HirType,
    ) -> Result<PointerValue<'ctx>> {
        let HirOperand::Local(id) = target else {
            return Err(CodegenError("索引目标必须是已定型局部值".into()));
        };
        let ty = &self.slot(*id)?.1;
        let element = value_type(self.context, self.types, element_ty)?;
        let index = self
            .operand(index, &HirType::Int(IntTy::Usize))?
            .into_int_value();
        let object = self.operand(target, ty)?.into_pointer_value();
        let data = match ty {
            HirType::Array(actual) if actual.as_ref() == element_ty => {
                let invalid = self.builder.build_int_compare(
                    IntPredicate::UGE,
                    index,
                    self.array_length(target)?,
                    "array_out_of_bounds",
                )?;
                self.trap_if(invalid)?;
                self.builder.build_struct_gep(
                    self.array_layout(element_ty, 0)?,
                    object,
                    1,
                    "array_data",
                )?
            }
            HirType::Pointer(actual) if actual.as_ref() == element_ty => object,
            _ => {
                return Err(CodegenError(format!(
                    "索引目标与元素类型不匹配：{ty:?} / {element_ty:?}"
                )));
            }
        };
        Ok(unsafe {
            self.builder
                .build_gep(element, data, &[index], "index_address")?
        })
    }

    fn array_destructor(&self, element_ty: &HirType) -> Result<FunctionValue<'ctx>> {
        // 模块内递增序号避免不同函数和元素类型之间符号碰撞。
        let name = format!(
            "__novella_drop_array_{}",
            self.module.get_functions().count()
        );
        let pointer = self.context.ptr_type(inkwell::AddressSpace::default());
        let function = self.module.add_function(
            &name,
            self.context.void_type().fn_type(&[pointer.into()], false),
            Some(Linkage::Internal),
        );
        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(function, "entry");
        let check = self.context.append_basic_block(function, "check");
        let body = self.context.append_basic_block(function, "element");
        let done = self.context.append_basic_block(function, "done");
        builder.position_at_end(entry);
        let object = function
            .get_first_param()
            .expect("数组析构有载荷参数")
            .into_pointer_value();
        let layout = self.array_layout(element_ty, 0)?;
        let length_slot = builder.build_struct_gep(layout, object, 0, "length_slot")?;
        let usize_ty = self.context.i64_type();
        let length = builder
            .build_load(usize_ty, length_slot, "length")?
            .into_int_value();
        let data = builder.build_struct_gep(layout, object, 1, "data")?;
        builder.build_unconditional_branch(check)?;
        builder.position_at_end(check);
        let remaining = builder.build_phi(usize_ty, "remaining")?;
        remaining.add_incoming(&[(&length, entry)]);
        let has_element = builder.build_int_compare(
            IntPredicate::NE,
            remaining.as_basic_value().into_int_value(),
            usize_ty.const_zero(),
            "has_element",
        )?;
        builder.build_conditional_branch(has_element, body, done)?;
        builder.position_at_end(body);
        let index = builder.build_int_sub(
            remaining.as_basic_value().into_int_value(),
            usize_ty.const_int(1, false),
            "index",
        )?;
        let element = value_type(self.context, self.types, element_ty)?;
        let slot = unsafe { builder.build_gep(element, data, &[index], "element_slot")? };
        let value = builder.build_load(element, slot, "element")?;
        crate::ownership::reference_op(
            self.context,
            self.module,
            self.types,
            &builder,
            "novella_rt_release",
            value,
            element_ty,
        )?;
        // reference_op 可以拆分基本块，回边应使用实际结束的位置。
        let tail = builder.get_insert_block().expect("数组元素析构有基本块");
        builder.build_unconditional_branch(check)?;
        remaining.add_incoming(&[(&index, tail)]);
        builder.position_at_end(done);
        builder.build_return(None)?;
        Ok(function)
    }
}

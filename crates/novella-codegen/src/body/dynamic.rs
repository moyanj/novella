//! 动态 trait 值为 `{payload, witness}`：只有 payload 参与 ARC。

use super::*;
use inkwell::AddressSpace;
use inkwell::module::Linkage;

impl<'ctx> Body<'ctx, '_> {
    pub(super) fn pack_dynamic(
        &mut self,
        operand: &HirOperand,
        source: &HirType,
        witness: &[HirFunctionId],
        ty: &HirType,
        dst: LocalId,
    ) -> Result<()> {
        if !matches!(ty, HirType::Dynamic(_)) {
            return Err(CodegenError("动态包装需要 dyn 目标类型".into()));
        }
        let value = self.operand(operand, source)?;
        let table = self.witness_table(witness)?;
        let data = match source {
            HirType::Class(..) => {
                self.reference_value("novella_rt_retain", value, source)?;
                value.into_pointer_value()
            }
            HirType::Struct(..) => {
                let layout = value_type(self.context, self.types, source)?;
                let destroy = self.box_destructor(source, layout)?;
                let data = self.runtime_alloc_layout(layout, destroy)?;
                self.reference_value("novella_rt_retain", value, source)?;
                self.builder.build_store(data, value)?;
                data
            }
            _ => return Err(CodegenError(format!("不能包装成 dyn：{source:?}"))),
        };
        let layout = value_type(self.context, self.types, ty)?.into_struct_type();
        let composite = self
            .builder
            .build_insert_value(layout.get_undef(), data, 0, "dyn.data")?;
        let composite = self
            .builder
            .build_insert_value(composite, table, 1, "dyn.table")?;
        self.save(dst, ty, composite.into_struct_value().into())
    }

    fn witness_table(&self, witness: &[HirFunctionId]) -> Result<PointerValue<'ctx>> {
        // 长度前缀确保任意合法符号组合（包括实例化名称）都不会发生拼接碰撞。
        use std::fmt::Write as _;
        let mut name = String::from("__novella_witness");
        for id in witness {
            let _ = write!(name, ".{}:{}", id.0.len(), id.0);
        }
        if let Some(global) = self.module.get_global(&name) {
            return Ok(global.as_pointer_value());
        }
        let mut slots = Vec::with_capacity(witness.len());
        for id in witness {
            let (function, sig) = self
                .functions
                .get(id)
                .ok_or_else(|| CodegenError(format!("witness 实例 `{id}` 不存在")))?;
            if !sig.receiver || sig.is_async || sig.type_param_count != 0 {
                return Err(CodegenError(format!("非法的 witness 方法 `{id}`")));
            }
            slots.push(function.as_global_value().as_pointer_value());
        }
        let pointer = self.context.ptr_type(AddressSpace::default());
        let value = pointer.const_array(&slots);
        let global = self.module.add_global(value.get_type(), None, &name);
        global.set_linkage(Linkage::Private);
        global.set_constant(true);
        global.set_initializer(&value);
        Ok(global.as_pointer_value())
    }

    fn box_destructor(
        &self,
        ty: &HirType,
        layout: inkwell::types::BasicTypeEnum<'ctx>,
    ) -> Result<PointerValue<'ctx>> {
        let HirType::Struct(module, name) = ty else {
            return Err(CodegenError("dyn box 析构需要 struct 类型".into()));
        };
        let pointer = self.context.ptr_type(AddressSpace::default());
        if !self.is_managed(ty) {
            return Ok(pointer.const_null());
        }
        let symbol = format!("__novella_dyn_box_drop_m{module}_{name}");
        if let Some(function) = self.module.get_function(&symbol) {
            return Ok(function.as_global_value().as_pointer_value());
        }
        let signature = self.context.void_type().fn_type(&[pointer.into()], false);
        let function = self
            .module
            .add_function(&symbol, signature, Some(Linkage::Private));
        let builder = self.context.create_builder();
        builder.position_at_end(self.context.append_basic_block(function, "entry"));
        let payload =
            builder.build_load(layout, function.get_params()[0].into_pointer_value(), "box")?;
        crate::ownership::reference_op(
            self.context,
            self.module,
            self.types,
            &builder,
            "novella_rt_release",
            payload,
            ty,
        )?;
        builder.build_return(None)?;
        Ok(function.as_global_value().as_pointer_value())
    }

    pub(super) fn call_dynamic(
        &mut self,
        (receiver, ty): (&HirOperand, &HirType),
        signature: &FunctionSignature,
        slot: u32,
        args: &[HirOperand],
        dst: Option<LocalId>,
    ) -> Result<()> {
        if !matches!(ty, HirType::Dynamic(_))
            || !signature.receiver
            || args.len() != signature.params.len()
        {
            return Err(CodegenError("动态调用接收者或实参与签名不匹配".into()));
        }
        let value = self.operand(receiver, ty)?.into_struct_value();
        let data = self.builder.build_extract_value(value, 0, "dyn.data")?;
        let table = self
            .builder
            .build_extract_value(value, 1, "dyn.table")?
            .into_pointer_value();
        let pointer = self.context.ptr_type(AddressSpace::default());
        let offset = self.context.i64_type().const_int(u64::from(slot), false);
        // HIR 用同一个 trait 槽顺序生成表和调用；表是连续的 opaque-pointer 数组。
        let address = unsafe {
            self.builder
                .build_gep(pointer, table, &[offset], "witness.slot")?
        };
        let callee = self
            .builder
            .build_load(pointer, address, "witness.method")?
            .into_pointer_value();
        let mut values = vec![data.into()];
        for (operand, ty) in args.iter().zip(&signature.params) {
            values.push(self.operand(operand, ty)?.into());
        }
        let function_type = crate::function_type(self.context, self.types, signature)?;
        let call = self
            .builder
            .build_indirect_call(function_type, callee, &values, "dyn.call")?;
        self.finish_call(call, signature, dst)
    }
}

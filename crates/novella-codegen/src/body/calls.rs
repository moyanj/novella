//! 直接调用与定型函数指针调用共享参数、ABI 扩展及返回值所有权契约。

use super::*;

enum Callee<'ctx> {
    Direct(FunctionValue<'ctx>),
    Indirect(PointerValue<'ctx>),
}

impl<'ctx> Body<'ctx, '_> {
    pub(super) fn call(
        &mut self,
        operand: &HirOperand,
        args: &[HirOperand],
        dst: Option<LocalId>,
        dst_ty: Option<&HirType>,
    ) -> Result<()> {
        let (callee, signature) = match operand {
            HirOperand::Function(id) | HirOperand::FunctionPtr(id) => {
                let (function, signature) = self
                    .functions
                    .get(id)
                    .ok_or_else(|| CodegenError(format!("未定义 HIR 函数 `{id}`")))?;
                (Callee::Direct(*function), (*signature).clone())
            }
            HirOperand::Local(id) => {
                let ty = self.slot(*id)?.1.clone();
                let HirType::Function(signature) = &ty else {
                    return Err(CodegenError(format!(
                        "间接调用槽位 {} 不是定型函数指针",
                        id.0
                    )));
                };
                let pointer = self.operand(operand, &ty)?.into_pointer_value();
                (Callee::Indirect(pointer), (**signature).clone())
            }
            _ => {
                return Err(CodegenError(
                    "调用目标不是已定义函数或定型函数指针局部值".into(),
                ));
            }
        };
        // 从完整 HIR 签名构造类型，裸机浮点、异步和泛型限制同直接调用。
        let function_type = crate::function_type(self.context, self.types, &signature)?;
        let id = &signature.id;
        let receiver_count = usize::from(signature.receiver);
        if args.len() != signature.params.len() + receiver_count {
            return Err(CodegenError(format!("`{id}` 实参个数与签名不一致")));
        }
        if dst.is_some() && dst_ty != Some(&signature.ret) {
            return Err(CodegenError(format!("`{id}` 调用的返回类型与 HIR 不一致")));
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
                .map(|(operand, ty)| self.operand(operand, ty).map(BasicMetadataValueEnum::from))
                .collect::<Result<Vec<_>>>()?,
        );
        let call = match callee {
            Callee::Direct(function) => self.builder.build_call(function, &values, "call")?,
            Callee::Indirect(pointer) => {
                self.builder
                    .build_indirect_call(function_type, pointer, &values, "call")?
            }
        };
        for (location, attribute) in crate::abi::extensions(self.context, &signature) {
            call.add_attribute(location, attribute);
        }
        if let Some(dst) = dst {
            let value = call
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CodegenError(format!("`{id}` 的 void 调用不能保存结果")))?;
            // 返回值已经拥有引用；槽位接管它，不能重复 retain。
            self.save(dst, &signature.ret, value)?;
        } else if self.is_managed(&signature.ret) {
            let value = call
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CodegenError(format!("`{id}` 缺少拥有返回值")))?;
            self.reference_value("novella_rt_release", value, &signature.ret)?;
        }
        Ok(())
    }
}

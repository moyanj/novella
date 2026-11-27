//! 内联数组的构造与下标；裸指针仍按其元素类型寻址。

use inkwell::{
    IntPredicate,
    types::BasicType,
    values::{ArrayValue, PointerValue},
};
use novella_hir::{HirOperand, HirType, IntTy};

use super::Body;
use crate::{CodegenError, Result, value_type};

impl<'ctx> Body<'ctx, '_> {
    pub(super) fn array_literal(
        &self,
        elements: &[HirOperand],
        element_ty: &HirType,
        length: u32,
    ) -> Result<ArrayValue<'ctx>> {
        let mut value = value_type(self.context, self.types, element_ty)?
            .array_type(length)
            .const_zero();
        for (index, operand) in (0..length).zip(elements) {
            let element = self.operand(operand, element_ty)?;
            self.reference_value("novella_rt_retain", element, element_ty)?;
            value = self
                .builder
                .build_insert_value(value, element, index, "array")?
                .into_array_value();
        }
        Ok(value)
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
        let (slot, ty) = self.slot(*id)?;
        let index = self
            .operand(index, &HirType::Int(IntTy::Usize))?
            .into_int_value();
        let (base, array) = match ty {
            HirType::Array(actual, length) if actual.as_ref() == element_ty => {
                (*slot, Some(*length))
            }
            HirType::Pointer(actual) => {
                let base = self.operand(target, ty)?.into_pointer_value();
                match actual.as_ref() {
                    actual if actual == element_ty => (base, None),
                    HirType::Array(element, length) if element.as_ref() == element_ty => {
                        (base, Some(*length))
                    }
                    _ => {
                        return Err(CodegenError(format!(
                            "索引目标与元素类型不匹配：{ty:?} / {element_ty:?}"
                        )));
                    }
                }
            }
            _ => {
                return Err(CodegenError(format!(
                    "索引目标与元素类型不匹配：{ty:?} / {element_ty:?}"
                )));
            }
        };
        let element = value_type(self.context, self.types, element_ty)?;
        if let Some(length) = array {
            let word = index.get_type();
            let invalid = self.builder.build_int_compare(
                IntPredicate::UGE,
                index,
                word.const_int(u64::from(length), false),
                "array_out_of_bounds",
            )?;
            self.trap_if(invalid)?;
            Ok(unsafe {
                self.builder.build_gep(
                    element.array_type(length),
                    base,
                    &[word.const_zero(), index],
                    "index_address",
                )?
            })
        } else {
            Ok(unsafe {
                self.builder
                    .build_gep(element, base, &[index], "index_address")?
            })
        }
    }
}

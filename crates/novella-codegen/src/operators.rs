//! 标量运算与整数转换。

use inkwell::IntPredicate;
use inkwell::intrinsics::Intrinsic;
use inkwell::values::{BasicValueEnum, IntValue};
use novella_hir::{HirBinaryOp, HirOperand, HirType, HirUnaryOp};

use crate::body::Body;
use crate::{CodegenError, Result, value_type};

impl<'ctx> Body<'ctx, '_> {
    pub(crate) fn unary(
        &self,
        op: HirUnaryOp,
        operand: &HirOperand,
        ty: &HirType,
    ) -> Result<BasicValueEnum<'ctx>> {
        let value = self.operand(operand, ty)?;
        match (op, value) {
            (HirUnaryOp::Identity, _) => Ok(value),
            (HirUnaryOp::Neg, BasicValueEnum::IntValue(value)) => {
                Ok(self.builder.build_int_neg(value, "neg")?.into())
            }
            (HirUnaryOp::Not | HirUnaryOp::BitNot, BasicValueEnum::IntValue(value)) => {
                Ok(self.builder.build_not(value, "not")?.into())
            }
            _ => Err(CodegenError(format!("暂不支持一元运算 {op:?} / {ty:?}"))),
        }
    }

    pub(crate) fn cast(
        &self,
        operand: &HirOperand,
        from: &HirType,
        to: &HirType,
    ) -> Result<BasicValueEnum<'ctx>> {
        let value = self.operand(operand, from)?;
        if from == to {
            return Ok(value);
        }
        if let (HirType::Int(kind), HirType::Int(_)) = (from, to) {
            let ty = value_type(self.context, to)?.into_int_type();
            return Ok(self
                .builder
                .build_int_cast_sign_flag(value.into_int_value(), ty, kind.is_signed(), "cast")?
                .into());
        }
        Err(CodegenError(format!("暂不支持转换 {from:?} → {to:?}")))
    }

    pub(crate) fn binary(
        &self,
        op: HirBinaryOp,
        (lhs, lhs_ty): (&HirOperand, &HirType),
        (rhs, rhs_ty): (&HirOperand, &HirType),
    ) -> Result<BasicValueEnum<'ctx>> {
        use HirBinaryOp::*;
        use IntPredicate::*;

        let unsupported =
            || CodegenError(format!("暂不支持二元运算 {op:?} / {lhs_ty:?} / {rhs_ty:?}"));
        if lhs_ty != rhs_ty {
            return Err(unsupported());
        }
        let lhs = self.operand(lhs, lhs_ty)?;
        let rhs = self.operand(rhs, rhs_ty)?;
        if let (BasicValueEnum::PointerValue(lhs), BasicValueEnum::PointerValue(rhs)) = (lhs, rhs) {
            let predicate = match op {
                Eq => EQ,
                Ne => NE,
                _ => return Err(unsupported()),
            };
            return Ok(self
                .builder
                .build_int_compare(predicate, lhs, rhs, "ptr.cmp")?
                .into());
        }
        let (BasicValueEnum::IntValue(lhs), BasicValueEnum::IntValue(rhs)) = (lhs, rhs) else {
            return Err(unsupported());
        };
        let signed = matches!(lhs_ty, HirType::Int(kind) if kind.is_signed());
        let builder = &self.builder;
        let result = match op {
            Add => builder.build_int_add(lhs, rhs, "add")?,
            Sub => builder.build_int_sub(lhs, rhs, "sub")?,
            Mul => builder.build_int_mul(lhs, rhs, "mul")?,
            Div | Rem => {
                let ty = rhs.get_type();
                let zero = builder.build_int_compare(EQ, rhs, ty.const_zero(), "zero")?;
                let invalid = if signed {
                    let minimum = ty.const_int(1_u64 << (ty.get_bit_width() - 1), false);
                    let min = builder.build_int_compare(EQ, lhs, minimum, "min")?;
                    let minus_one =
                        builder.build_int_compare(EQ, rhs, ty.const_all_ones(), "minus.one")?;
                    let overflow = builder.build_and(min, minus_one, "overflow")?;
                    builder.build_or(zero, overflow, "invalid.divisor")?
                } else {
                    zero
                };
                self.trap_if(invalid)?;
                match (op, signed) {
                    (Div, true) => builder.build_int_signed_div(lhs, rhs, "div")?,
                    (Div, false) => builder.build_int_unsigned_div(lhs, rhs, "div")?,
                    (Rem, true) => builder.build_int_signed_rem(lhs, rhs, "rem")?,
                    _ => builder.build_int_unsigned_rem(lhs, rhs, "rem")?,
                }
            }
            Shl | Shr => {
                let ty = rhs.get_type();
                let invalid = builder.build_int_compare(
                    UGE,
                    rhs,
                    ty.const_int(u64::from(ty.get_bit_width()), false),
                    "invalid.shift",
                )?;
                self.trap_if(invalid)?;
                if op == Shl {
                    builder.build_left_shift(lhs, rhs, "shl")?
                } else {
                    builder.build_right_shift(lhs, rhs, signed, "shr")?
                }
            }
            BitAnd | LogAnd => builder.build_and(lhs, rhs, "and")?,
            BitOr | LogOr => builder.build_or(lhs, rhs, "or")?,
            BitXor => builder.build_xor(lhs, rhs, "xor")?,
            Eq | Ne | Lt | Le | Gt | Ge => {
                let predicate = match (op, signed) {
                    (Eq, _) => EQ,
                    (Ne, _) => NE,
                    (Lt, true) => SLT,
                    (Lt, false) => ULT,
                    (Le, true) => SLE,
                    (Le, false) => ULE,
                    (Gt, true) => SGT,
                    (Gt, false) => UGT,
                    (Ge, true) => SGE,
                    (Ge, false) => UGE,
                    _ => return Err(unsupported()),
                };
                builder.build_int_compare(predicate, lhs, rhs, "cmp")?
            }
            PtrDiff => return Err(unsupported()),
        };
        Ok(result.into())
    }

    fn trap_if(&self, invalid: IntValue<'ctx>) -> Result<()> {
        let trap = Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(self.module, &[]))
            .ok_or_else(|| CodegenError("LLVM 未提供 llvm.trap".into()))?;
        let failure = self
            .context
            .append_basic_block(self.function, "arithmetic.trap");
        let next = self
            .context
            .append_basic_block(self.function, "arithmetic.ok");
        self.builder
            .build_conditional_branch(invalid, failure, next)?;
        self.builder.position_at_end(failure);
        self.builder.build_call(trap, &[], "")?;
        self.builder.build_unreachable()?;
        self.builder.position_at_end(next);
        Ok(())
    }
}

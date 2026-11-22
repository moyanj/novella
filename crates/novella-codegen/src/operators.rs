//! 标量运算与整数转换。

use inkwell::intrinsics::Intrinsic;
use inkwell::values::{BasicValueEnum, IntValue};
use inkwell::{FloatPredicate, IntPredicate};
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
            (HirUnaryOp::Neg, BasicValueEnum::FloatValue(value)) => {
                Ok(self.builder.build_float_neg(value, "neg")?.into())
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
        if matches!(
            (from, to),
            (
                HirType::Pointer(_) | HirType::Class(..),
                HirType::Pointer(_) | HirType::Class(..)
            )
        ) {
            return Ok(value);
        }
        if let (HirType::Int(kind), HirType::Int(_)) = (from, to) {
            let ty = value_type(self.context, self.types, to)?.into_int_type();
            return Ok(self
                .builder
                .build_int_cast_sign_flag(value.into_int_value(), ty, kind.is_signed(), "cast")?
                .into());
        }
        match (from, to) {
            (HirType::Float(_), HirType::Float(_)) => {
                return Ok(self
                    .builder
                    .build_float_cast(
                        value.into_float_value(),
                        value_type(self.context, self.types, to)?.into_float_type(),
                        "float_cast",
                    )?
                    .into());
            }
            (HirType::Int(kind), HirType::Float(_)) => {
                let ty = value_type(self.context, self.types, to)?.into_float_type();
                return Ok(if kind.is_signed() {
                    self.builder.build_signed_int_to_float(
                        value.into_int_value(),
                        ty,
                        "int_float",
                    )?
                } else {
                    self.builder.build_unsigned_int_to_float(
                        value.into_int_value(),
                        ty,
                        "int_float",
                    )?
                }
                .into());
            }
            // 浮点 → 整数：按目标整型的符号性选择有/无符号转换。
            // LLVM 的 fptosi/fptoui 对超出目标范围或 NaN 的值是毒值
            // （undef），因此这里先判定可转换区间，越界即 trap，
            // 与整数除零 / 移位越界的处理保持一致（不静默兜底）。
            (HirType::Float(_), HirType::Int(target)) => {
                let float = value.into_float_value();
                let ty = value_type(self.context, self.types, to)?.into_int_type();
                let float_ty = value_type(self.context, self.types, from)?.into_float_type();
                let bits = i32::try_from(ty.get_bit_width())
                    .map_err(|_| CodegenError(format!("浮点转整数：位宽超出可表示范围 {ty:?}")))?;
                // 有符号下界 -2^(bits-1) 含端点，上界 2^(bits-1) 开区间；
                // 无符号区间为 [0, 2^bits)。用 f64 计算边界：bits ≤ 64 时
                // 边界值本身都可被 f64 精确表示。
                let (min, max) = if target.is_signed() {
                    (-2_f64.powi(bits - 1), 2_f64.powi(bits - 1))
                } else {
                    (0.0, 2_f64.powi(bits))
                };
                // LLVM 的 fptosi / fptoui 对 NaN 或越界输入产生毒值，
                // 因此先判区间，越界与 NaN 一律 trap（与整数除零同口径）。
                let below = self.builder.build_float_compare(
                    FloatPredicate::OLT,
                    float,
                    float_ty.const_float(min),
                    "float_to_int.below",
                )?;
                let above = self.builder.build_float_compare(
                    FloatPredicate::OGE,
                    float,
                    float_ty.const_float(max),
                    "float_to_int.above",
                )?;
                let invalid = self
                    .builder
                    .build_or(below, above, "float_to_int.invalid")?;
                self.trap_if(invalid)?;
                return Ok(if target.is_signed() {
                    self.builder
                        .build_float_to_signed_int(float, ty, "float_int")?
                } else {
                    self.builder
                        .build_float_to_unsigned_int(float, ty, "float_int")?
                }
                .into());
            }
            _ => {}
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
        if let (BasicValueEnum::FloatValue(lhs), BasicValueEnum::FloatValue(rhs)) = (lhs, rhs) {
            let result: BasicValueEnum = match op {
                Add => self.builder.build_float_add(lhs, rhs, "add")?.into(),
                Sub => self.builder.build_float_sub(lhs, rhs, "sub")?.into(),
                Mul => self.builder.build_float_mul(lhs, rhs, "mul")?.into(),
                Div => self.builder.build_float_div(lhs, rhs, "div")?.into(),
                Rem => self.builder.build_float_rem(lhs, rhs, "rem")?.into(),
                Eq | Ne | Lt | Le | Gt | Ge => {
                    let predicate = match op {
                        Eq => FloatPredicate::OEQ,
                        Ne => FloatPredicate::UNE,
                        Lt => FloatPredicate::OLT,
                        Le => FloatPredicate::OLE,
                        Gt => FloatPredicate::OGT,
                        Ge => FloatPredicate::OGE,
                        _ => unreachable!(),
                    };
                    self.builder
                        .build_float_compare(predicate, lhs, rhs, "fcmp")?
                        .into()
                }
                _ => return Err(unsupported()),
            };
            return Ok(result);
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

    pub(crate) fn trap_if(&self, invalid: IntValue<'ctx>) -> Result<()> {
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

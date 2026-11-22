//! 数值字面量、预定义类型与目标范围。
use super::*;

/// 数字字面量解析（design.md §2「值语义延后」：AST 保留原文，此处归类）。
///
/// 返回 `IntLiteral` / `FloatLiteral`；非法数字文本返回 None（调用方报诊断）。
/// 支持进制前缀（`0x` / `0o` / `0b`）与下划线分隔；具体值与溢出检查属后续期。
pub(crate) fn classify_number_literal(raw: &str) -> Option<Ty> {
    let cleaned: String = raw.chars().filter(|&c| c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    // 浮点：含小数点或指数（注意 0x 前缀的十六进制不允许小数）
    let lower = cleaned.to_ascii_lowercase();
    let is_hex_or_based =
        lower.starts_with("0x") || lower.starts_with("0o") || lower.starts_with("0b");
    if !is_hex_or_based && (cleaned.contains('.') || lower.contains('e')) {
        // 粗略校验：应为合法浮点文本
        // 解析成 f64 才能判定目标类型能否有限表示（`1e999` → inf，L05）
        return cleaned.parse::<f64>().ok().map(Ty::FloatLiteral);
    }
    // 整数：按进制校验数字合法性
    let (digits, radix) = if let Some(rest) = lower.strip_prefix("0x") {
        (rest, 16)
    } else if let Some(rest) = lower.strip_prefix("0o") {
        (rest, 8)
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (rest, 2)
    } else {
        (lower.as_str(), 10)
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    // 数值只用于目标位宽范围判定；超出 u128 的极端字面量按「无法确定」处理，
    // 由调用方报非法数字文本，而不是静默截断。
    let value = u128::from_str_radix(digits, radix).ok()?;
    Some(Ty::IntLiteral(false, value))
}

/// 预定义类型名归类（design.md §5.1）。
///
/// 返回 None 表示非预定义名，调用方继续走符号查找。
pub(crate) fn predefined_from_name(name: &str) -> Option<Ty> {
    if let Some(kind) = IntKind::from_name(name) {
        return Some(Ty::Int(kind));
    }
    if let Some(kind) = FloatKind::from_name(name) {
        return Some(Ty::Float(kind));
    }
    match name {
        "boolean" => Some(Ty::Boolean),
        "void" => Some(Ty::Void),
        "null" => Some(Ty::Null),
        _ => None,
    }
}

/// `PredefinedType` 枚举 → `Ty`（parser 仅对缺省 `void` 返回等少数位置产出）。
pub(crate) fn predefined_from_enum(ty: PredefinedType) -> Ty {
    match ty {
        PredefinedType::Null => Ty::Null,
        PredefinedType::Boolean => Ty::Boolean,
        PredefinedType::Int => Ty::Int(IntKind::I32),
        PredefinedType::Float => Ty::Float(FloatKind::F32),
        PredefinedType::Void => Ty::Void,
        PredefinedType::U8 => Ty::Int(IntKind::U8),
        PredefinedType::U16 => Ty::Int(IntKind::U16),
        PredefinedType::U32 => Ty::Int(IntKind::U32),
        PredefinedType::U64 => Ty::Int(IntKind::U64),
        PredefinedType::I8 => Ty::Int(IntKind::I8),
        PredefinedType::I16 => Ty::Int(IntKind::I16),
        PredefinedType::I32 => Ty::Int(IntKind::I32),
        PredefinedType::I64 => Ty::Int(IntKind::I64),
        PredefinedType::F16 => Ty::Float(FloatKind::F16),
        PredefinedType::F32 => Ty::Float(FloatKind::F32),
        PredefinedType::F64 => Ty::Float(FloatKind::F64),
        PredefinedType::Usize => Ty::Int(IntKind::Usize),
        PredefinedType::Isize => Ty::Int(IntKind::Isize),
    }
}

/// 未取负的整数字面量 `value` 能否被 `kind` 表示（design.md §5.2.1）。
///
/// 正字面量按类型的**有符号上限**判定，因此 `let x: i8 = 128;` 非法、
/// `let x: i8 = 127;` 合法。`-128` 是 `Unary(Minus, 128)`，由
/// [`int_negated_literal_fits`] 在取负处单独判定——两者合起来才是完整范围。
/// `usize` / `isize` 的位宽取决于目标平台，sema 阶段按 64 位判定（见矩阵 T15）。
pub(crate) fn int_literal_fits_non_negative(kind: IntKind, value: u128) -> bool {
    match kind {
        IntKind::U8 => value <= u8::MAX as u128,
        IntKind::U16 => value <= u16::MAX as u128,
        IntKind::U32 => value <= u32::MAX as u128,
        IntKind::U64 | IntKind::Usize => value <= u64::MAX as u128,
        IntKind::I8 => value <= i8::MAX as u128,
        IntKind::I16 => value <= i16::MAX as u128,
        IntKind::I32 => value <= i32::MAX as u128,
        IntKind::I64 | IntKind::Isize => value <= i64::MAX as u128,
    }
}

/// 取负后的整数字面量 `value`（`-value`）能否被有符号类型 `kind` 表示。
///
/// 只有有符号类型能表示负数；判定按 `|MIN|`，即 `i8` 允许 `-128`。
pub(crate) fn int_negated_literal_fits(kind: IntKind, value: u128) -> bool {
    match kind {
        IntKind::I8 => value <= 128,
        IntKind::I16 => value <= 32768,
        IntKind::I32 => value <= i32::MAX as u128 + 1,
        IntKind::I64 | IntKind::Isize => value <= i64::MAX as u128 + 1,
        // 无符号类型不能表示负数
        IntKind::U8 | IntKind::U16 | IntKind::U32 | IntKind::U64 | IntKind::Usize => false,
    }
}

/// 整数类型的取值范围文本（诊断用，含最小负数与最大正数）。
pub(crate) fn int_range_text(kind: IntKind) -> String {
    let (min, max): (i128, i128) = match kind {
        IntKind::U8 => (0, u8::MAX as i128),
        IntKind::U16 => (0, u16::MAX as i128),
        IntKind::U32 => (0, u32::MAX as i128),
        IntKind::U64 => (0, u64::MAX as i128),
        IntKind::Usize => (0, u64::MAX as i128),
        IntKind::I8 => (i8::MIN as i128, i8::MAX as i128),
        IntKind::I16 => (i16::MIN as i128, i16::MAX as i128),
        IntKind::I32 => (i32::MIN as i128, i32::MAX as i128),
        IntKind::I64 => (i64::MIN as i128, i64::MAX as i128),
        IntKind::Isize => (i64::MIN as i128, i64::MAX as i128),
    };
    format!("{min}..={max}")
}

/// 浮点字面量 `value` 能否被浮点类型 `kind` **有限**表示（design.md §5.2.1）。
///
/// - `f64`：字面量本身是 `f64`，只拒绝解析后非有限的值（`1e999` → `inf`）；
/// - `f32`：须落在 `f32` 的有限范围内（最大值之外即溢出，`f32::MAX as f64` 上界）；
/// - `f16`：按 IEEE binary16 的舍入溢出阈值检查；
pub(crate) fn float_literal_fits(kind: FloatKind, value: f64) -> bool {
    if !value.is_finite() {
        return false;
    }
    match kind {
        FloatKind::F64 => true,
        // 判据是「按 f32 舍入后仍有限」，而不是「绝对值 ≤ f32::MAX」：
        // `3.4028235e38` 比 `f32::MAX` 略大，但舍入后正好落在 `f32::MAX` 上，合法。
        FloatKind::F32 => (value as f32).is_finite(),
        // IEEE binary16 的最近偶数舍入溢出阈值为 65520；最大有限值为 65504。
        FloatKind::F16 => value.abs() < 65520.0,
    }
}

/// 浮点类型的可表示范围文本（诊断用，短格式，避免默认 `Display` 展开上百位数字）。
pub(crate) fn float_range_text(kind: FloatKind) -> String {
    match kind {
        FloatKind::F64 => "≈±1.7976931348623157e308（f64）".to_string(),
        FloatKind::F32 => "≈±3.4028235e38（f32）".to_string(),
        // f16 使用 IEEE binary16 表示
        FloatKind::F16 => "±65504（IEEE binary16，按最近偶数舍入）".to_string(),
    }
}

/// 平台整数仅在范围判定时选位宽，不能改变其名义类型相等性。
pub(crate) fn target_int_kind(kind: IntKind, width: crate::PointerWidth) -> IntKind {
    match (kind, width) {
        (IntKind::Usize, crate::PointerWidth::Bits32) => IntKind::U32,
        (IntKind::Isize, crate::PointerWidth::Bits32) => IntKind::I32,
        _ => kind,
    }
}

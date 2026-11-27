//! HIR 降级：helpers 职责实现。

use super::*;

/// 函数可见性映射（缺省可见性按模块内可见处理）。
pub(super) fn visibility_of(scope: Option<Scope>) -> HirVisibility {
    match scope {
        Some(Scope::Public) => HirVisibility::Public,
        Some(Scope::Private) => HirVisibility::Private,
        Some(Scope::Package) => HirVisibility::Package,
        None => HirVisibility::Private,
    }
}

// ---------- 助手 ----------

/// 比较类二元运算（结果为 bool）。
pub(super) fn is_comparison(op: HirBinaryOp) -> bool {
    matches!(
        op,
        HirBinaryOp::Eq
            | HirBinaryOp::Ne
            | HirBinaryOp::Lt
            | HirBinaryOp::Le
            | HirBinaryOp::Gt
            | HirBinaryOp::Ge
    )
}

/// 两种类型之间是否有可用的转换指令（codegen 侧据此选 `sext` / `trunc` / `sitofp` 等）。
pub(super) fn castable(from: &HirType, to: &HirType) -> bool {
    match (from, to) {
        (HirType::Int(_), HirType::Int(_)) => true,
        (HirType::Float(_), HirType::Float(_)) => true,
        (HirType::Int(_), HirType::Float(_)) | (HirType::Float(_), HirType::Int(_)) => true,
        (HirType::Int(_), HirType::Pointer(_)) | (HirType::Pointer(_), HirType::Int(_)) => true,
        (HirType::Boolean, HirType::Int(_)) | (HirType::Int(_), HirType::Boolean) => true,
        (HirType::Pointer(_), HirType::Pointer(_)) | (HirType::Class(..), HirType::Class(..)) => {
            true
        }
        (a, b) => same_value_type(a, b),
    }
}

/// 类型的简短标签（错误消息里用）。
pub(super) fn type_label(ty: &HirType) -> &'static str {
    match ty {
        HirType::Void => "void",
        HirType::Boolean => "boolean",
        HirType::Int(_) => "int",
        HirType::Float(_) => "float",
        HirType::Pointer(_) => "pointer",
        HirType::Array(..) => "array",
        HirType::Tuple(_) => "tuple",
        HirType::Function(_) => "function",
        HirType::Struct(..) => "struct",
        HirType::Class(..) => "class",
        HirType::Trait(..) => "trait",
    }
}

/// AST 二元运算符 → HIR 二元运算（不支持者返回 `None`）。
///
/// `&&` / `||` 由短路展开单独处理，不在此表内。
pub(super) fn binary_op_of(op: BinaryOp) -> Option<HirBinaryOp> {
    Some(match op {
        BinaryOp::Plus => HirBinaryOp::Add,
        BinaryOp::Minus => HirBinaryOp::Sub,
        BinaryOp::Multiply => HirBinaryOp::Mul,
        BinaryOp::Divide => HirBinaryOp::Div,
        BinaryOp::Modulus => HirBinaryOp::Rem,
        BinaryOp::LeftShift => HirBinaryOp::Shl,
        BinaryOp::RightShift => HirBinaryOp::Shr,
        BinaryOp::LessThan => HirBinaryOp::Lt,
        BinaryOp::LessThanEquals => HirBinaryOp::Le,
        BinaryOp::GreaterThan => HirBinaryOp::Gt,
        BinaryOp::GreaterThanEquals => HirBinaryOp::Ge,
        BinaryOp::Equals | BinaryOp::IdentityEquals => HirBinaryOp::Eq,
        BinaryOp::NotEquals | BinaryOp::IdentityNotEquals => HirBinaryOp::Ne,
        BinaryOp::BitAnd => HirBinaryOp::BitAnd,
        BinaryOp::BitOr => HirBinaryOp::BitOr,
        BinaryOp::BitXor => HirBinaryOp::BitXor,
        // 严格相等与空合并需要值表示（本期未定）
        BinaryOp::NullCoalesce | BinaryOp::LogicalAnd | BinaryOp::LogicalOr => return None,
    })
}

/// 复合赋值运算符 → 对应的二元运算（简单赋值返回 `None`）。
pub(super) fn compound_op_of(op: AssignOp) -> Option<HirBinaryOp> {
    Some(match op {
        AssignOp::PlusAssign => HirBinaryOp::Add,
        AssignOp::MinusAssign => HirBinaryOp::Sub,
        AssignOp::MultiplyAssign => HirBinaryOp::Mul,
        AssignOp::DivideAssign => HirBinaryOp::Div,
        AssignOp::ModulusAssign => HirBinaryOp::Rem,
        AssignOp::BitAndAssign => HirBinaryOp::BitAnd,
        AssignOp::BitOrAssign => HirBinaryOp::BitOr,
        AssignOp::BitXorAssign => HirBinaryOp::BitXor,
        AssignOp::LeftShiftAssign => HirBinaryOp::Shl,
        AssignOp::RightShiftAssign => HirBinaryOp::Shr,
        AssignOp::Assign => return None,
    })
}

/// 整数文本解析：支持 `0x` / `0o` / `0b` 前缀与负号（下划线已在调用方剥离）。
pub(super) fn parse_int_literal(text: &str) -> Option<i128> {
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let lower = body.to_ascii_lowercase();
    let value = if let Some(hex) = lower.strip_prefix("0x") {
        i128::from_str_radix(hex, 16).ok()?
    } else if let Some(oct) = lower.strip_prefix("0o") {
        i128::from_str_radix(oct, 8).ok()?
    } else if let Some(bin) = lower.strip_prefix("0b") {
        i128::from_str_radix(bin, 2).ok()?
    } else {
        lower.parse::<i128>().ok()?
    };
    Some(if negative { -value } else { value })
}

/// 普通字符串解码；格式化和字节串需要专属降级，不得剥前缀冒充字符串。
pub(super) fn binary_node_name(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Multiply => "multiply",
        BinaryOp::Divide => "divide",
        BinaryOp::Modulus => "modulus",
        BinaryOp::Plus => "plus",
        BinaryOp::Minus => "minus",
        BinaryOp::LeftShift => "left shift",
        BinaryOp::RightShift => "right shift",
        BinaryOp::LessThan => "less than",
        BinaryOp::GreaterThan => "greater than",
        BinaryOp::LessThanEquals => "less than equals",
        BinaryOp::GreaterThanEquals => "greater than equals",
        BinaryOp::Equals => "equals",
        BinaryOp::NotEquals => "not equals",
        BinaryOp::IdentityEquals => "identity equals",
        BinaryOp::IdentityNotEquals => "identity not equals",
        BinaryOp::BitAnd => "bit and",
        BinaryOp::BitXor => "bit xor",
        BinaryOp::BitOr => "bit or",
        BinaryOp::LogicalAnd => "logical and",
        BinaryOp::LogicalOr => "logical or",
        BinaryOp::NullCoalesce => "null coalesce",
    }
}

pub(super) fn assign_node_name(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "assign",
        AssignOp::PlusAssign => "plus assign",
        AssignOp::MinusAssign => "minus assign",
        AssignOp::MultiplyAssign => "multiply assign",
        AssignOp::DivideAssign => "divide assign",
        AssignOp::ModulusAssign => "modulus assign",
        AssignOp::BitAndAssign => "bit and assign",
        AssignOp::BitOrAssign => "bit or assign",
        AssignOp::BitXorAssign => "bit xor assign",
        AssignOp::LeftShiftAssign => "left shift assign",
        AssignOp::RightShiftAssign => "right shift assign",
    }
}

/// 具名实体键与匿名函数类型键分开；匿名键不标识实体。
pub(super) fn function_id_of(view: &novella_sema::FunctionView) -> HirFunctionId {
    match &view.name {
        Some(name) => HirFunctionId(format!("m{}::{}", view.module, name)),
        None => HirFunctionId::anonymous(view.module),
    }
}

/// 函数类型按签名兼容，实体 id 不参与值类型判等。
pub(super) fn same_value_type(a: &HirType, b: &HirType) -> bool {
    match (a, b) {
        (HirType::Function(a), HirType::Function(b)) => {
            a.receiver == b.receiver
                && a.params.len() == b.params.len()
                && a.params
                    .iter()
                    .zip(&b.params)
                    .all(|(a, b)| same_value_type(a, b))
                && same_value_type(&a.ret, &b.ret)
        }
        (HirType::Tuple(a), HirType::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value_type(a, b))
        }
        (HirType::Array(a, left), HirType::Array(b, right)) if left == right => {
            same_value_type(a, b)
        }
        (HirType::Pointer(a), HirType::Pointer(b)) => same_value_type(a, b),
        _ => a == b,
    }
}

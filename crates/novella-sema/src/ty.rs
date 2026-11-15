//! 语义类型 [`Ty`]：类型表达式的解析结果、可赋值性判定与字面量类型解析。
//!
//! 设计要点（对应 `docs/sema-phase1.md` §4–§5）：
//! - `int` ≡ `i32`、`float` ≡ `f32`（design.md 既定等价），解析时直接归一化；
//! - 数值字面量先记为未定型的 [`Ty::IntLiteral`] / [`Ty::FloatLiteral`]，
//!   可赋给任意同族数值类型（按目标类型的溢出检查属后续期）；
//! - 不做隐式数值转换：已定型值跨数值种类赋值一律报错；
//! - 隐式转换仅 `null → 任意指针/class`、`void* ↔ 任意 T*`、子类 → 父类。

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use novella_ast::types::{PredefinedType, TypeExpr};

use crate::module::ModuleId;

/// 整数种类（`int` 已归一化为 `I32`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IntKind {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    Usize,
    Isize,
}

impl IntKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            IntKind::U8 => "u8",
            IntKind::U16 => "u16",
            IntKind::U32 => "u32",
            IntKind::U64 => "u64",
            IntKind::I8 => "i8",
            IntKind::I16 => "i16",
            IntKind::I32 => "i32",
            IntKind::I64 => "i64",
            IntKind::Usize => "usize",
            IntKind::Isize => "isize",
        }
    }

    /// 按预定义类型名归类（design.md §5.1：类型名在词法层是普通标识符）。
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "u8" => IntKind::U8,
            "u16" => IntKind::U16,
            "u32" => IntKind::U32,
            "u64" => IntKind::U64,
            "i8" => IntKind::I8,
            "i16" => IntKind::I16,
            // `int` 等价于 `i32`
            "i32" | "int" => IntKind::I32,
            "i64" => IntKind::I64,
            "usize" => IntKind::Usize,
            "isize" => IntKind::Isize,
            _ => return None,
        })
    }

    /// 是否有符号（移位 / 取模诊断暂不使用，供 codegen 与后续期）
    #[allow(dead_code)]
    pub(crate) fn is_signed(self) -> bool {
        matches!(
            self,
            IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::I64 | IntKind::Isize
        )
    }
}

/// 浮点种类（`float` 已归一化为 `F32`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FloatKind {
    F8,
    F16,
    F32,
    F64,
}

impl FloatKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            FloatKind::F8 => "f8",
            FloatKind::F16 => "f16",
            FloatKind::F32 => "f32",
            FloatKind::F64 => "f64",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "f8" => FloatKind::F8,
            "f16" => FloatKind::F16,
            // `float` 等价于 `f32`
            "f32" | "float" => FloatKind::F32,
            "f64" => FloatKind::F64,
            _ => return None,
        })
    }
}

/// 函数签名（参数类型在声明收集后解析；extern 与普通函数共用）。
#[derive(Debug, Clone)]
pub(crate) struct FunctionSig {
    pub params: Vec<ParamSig>,
    pub ret: Ty,
    /// 泛型参数个数（本期浅泛型：调用点只查 arity）
    pub type_param_count: usize,
    pub is_async: bool,
    pub is_extern: bool,
}

/// 形参签名。
#[derive(Debug, Clone)]
pub(crate) struct ParamSig {
    pub(crate) name: String,
    /// 未标注类型时为 [`Ty::Unknown`]（参数类型推导属后续期，调用点跳过该参检查）
    pub ty: Ty,
    pub has_default: bool,
}

/// 语义类型。
#[derive(Debug, Clone)]
pub(crate) enum Ty {
    /// 类型未知（前面已报过诊断，用于抑制级联错误）
    Unknown,
    Void,
    /// `null` 字面量的类型（可隐式赋给任意指针 / class 引用）
    Null,
    Boolean,
    /// `string`（按名归类的内置类型；design.md 已将其移出 `PredefinedType`）
    String,
    Int(IntKind),
    Float(FloatKind),
    /// 未定型整数字面量（如 `42`），可赋给任意整数类型
    IntLiteral,
    /// 未定型浮点字面量（如 `1.5`），可赋给任意浮点类型
    FloatLiteral,
    Pointer(Box<Ty>),
    Array(Box<Ty>),
    Tuple(Vec<Ty>),
    Function(Rc<FunctionSig>),
    Union(Vec<Ty>),
    /// struct 类型（按模块 + 名名义判等）
    Struct(ModuleId, String),
    /// class 引用类型
    Class(ModuleId, String),
    /// trait 类型（仅 `is` / `as` / 约束位置有意义）
    Trait(ModuleId, String),
    /// 泛型类型参数（浅泛型：不透明）
    TypeParam(String),
    /// std 导入的不透明外部符号（调用 / 访问不做检查）
    External,
    /// 错误类型（产生处已报过明确诊断）：向下传播时不产生级联诊断，
    /// 但不参与任何兼容判定之外的推导（见 [`Ty::is_opaque`]）。
    Error,
}

impl Ty {
    pub(crate) fn is_numeric(&self) -> bool {
        matches!(
            self,
            Ty::Int(_) | Ty::Float(_) | Ty::IntLiteral | Ty::FloatLiteral
        )
    }

    pub(crate) fn is_integer(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::IntLiteral)
    }

    /// 是否可参与判空（指针 / class / null 自身）
    pub(crate) fn is_nullable(&self) -> bool {
        matches!(self, Ty::Pointer(_) | Ty::Class(..) | Ty::Null)
    }

    /// 是否是不透明占位（Unknown / External / Error）：参与检查时一律放行，抑制级联。
    /// `Error` 是 `Unknown` 以外的另一档：来源处已带明确诊断（如演示文件中的
    /// `yield`、语义未实现的写法），后续依赖它的位置不再重复报错。
    pub(crate) fn is_opaque(&self) -> bool {
        matches!(self, Ty::Unknown | Ty::External | Ty::Error)
    }
}

/// 结构化判等（`Function` 变体比较签名内容而非 `Rc` 指针）。
pub(crate) fn ty_eq(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Unknown, Ty::Unknown)
        | (Ty::Void, Ty::Void)
        | (Ty::Null, Ty::Null)
        | (Ty::Boolean, Ty::Boolean)
        | (Ty::String, Ty::String)
        | (Ty::IntLiteral, Ty::IntLiteral)
        | (Ty::FloatLiteral, Ty::FloatLiteral) => true,
        (Ty::Int(x), Ty::Int(y)) => x == y,
        (Ty::Float(x), Ty::Float(y)) => x == y,
        (Ty::Pointer(x), Ty::Pointer(y)) => ty_eq(x, y),
        (Ty::Array(x), Ty::Array(y)) => ty_eq(x, y),
        (Ty::Tuple(x), Ty::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| ty_eq(a, b))
        }
        (Ty::Function(x), Ty::Function(y)) => sig_eq(x, y),
        (Ty::Union(x), Ty::Union(y)) => {
            x.len() == y.len() && x.iter().all(|m| y.iter().any(|n| ty_eq(m, n)))
        }
        (Ty::Struct(m1, n1), Ty::Struct(m2, n2))
        | (Ty::Class(m1, n1), Ty::Class(m2, n2))
        | (Ty::Trait(m1, n1), Ty::Trait(m2, n2)) => m1 == m2 && n1 == n2,
        (Ty::TypeParam(x), Ty::TypeParam(y)) => x == y,
        (Ty::External, Ty::External) => true,
        (Ty::Error, Ty::Error) => true,
        _ => false,
    }
}

/// 函数签名判等（忽略 async / extern 标记）。
pub(crate) fn sig_eq(a: &FunctionSig, b: &FunctionSig) -> bool {
    a.params.len() == b.params.len()
        && a.params
            .iter()
            .zip(&b.params)
            .all(|(x, y)| ty_eq(&x.ty, &y.ty))
        && ty_eq(&a.ret, &b.ret)
}

/// `target ← source` 可赋值性（不引入隐式数值转换；`sema` 需继承链信息，
/// class 父子判定由调用方通过 `is_subclass` 闭包注入）。
pub(crate) fn assignable(
    target: &Ty,
    source: &Ty,
    is_subclass: &mut dyn FnMut(&Ty, &Ty) -> bool,
) -> bool {
    // 任一侧不透明：放行（诊断已在产生 Unknown 时报出）
    if target.is_opaque() || source.is_opaque() {
        return true;
    }
    if ty_eq(target, source) {
        return true;
    }
    match (target, source) {
        // 未定型字面量归化到具体数值类型
        (Ty::Int(_), Ty::IntLiteral) | (Ty::Float(_), Ty::FloatLiteral) => true,
        // null → 任意指针 / class 引用（design.md §6.5 S5）
        (Ty::Pointer(_), Ty::Null) | (Ty::Class(..), Ty::Null) => true,
        // void* ↔ 任意指针（design.md §6.5 S6）
        (Ty::Pointer(t), Ty::Pointer(s)) if matches!(**t, Ty::Void) || matches!(**s, Ty::Void) => {
            true
        }
        // 子类引用 → 父类引用
        (Ty::Class(..), Ty::Class(..)) => is_subclass(target, source),
        // 联合目标：source 可赋给任一成员即可
        (Ty::Union(members), _) => members.iter().any(|m| assignable(m, source, is_subclass)),
        // 联合源：全部成员都可赋给目标才可赋（窄化前的保守规则）
        (_, Ty::Union(members)) => members.iter().all(|m| assignable(target, m, is_subclass)),
        // 元组 / 数组：同形后逐元素可赋（字面量归化等规则随递归生效）
        (Ty::Tuple(t), Ty::Tuple(s)) if t.len() == s.len() => {
            t.iter().zip(s).all(|(t, s)| assignable(t, s, is_subclass))
        }
        (Ty::Array(t), Ty::Array(s)) => assignable(t, s, is_subclass),
        _ => false,
    }
}

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
        return cleaned.parse::<f64>().ok().map(|_| Ty::FloatLiteral);
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
    Some(Ty::IntLiteral)
}

// ---------- 类型定义注册表 ----------

/// 字段定义（struct / class 成员）。
#[derive(Debug, Clone)]
pub(crate) struct FieldDef {
    pub(crate) name: String,
    pub ty: Ty,
    /// 是否有声明处默认值（StructInit 可省略该字段）
    pub has_default: bool,
}

/// 方法签名（含静态标记；`this` 不占用参数位）。
#[derive(Debug, Clone)]
pub(crate) struct MethodSig {
    pub sig: FunctionSig,
    pub is_static: bool,
}

/// struct 定义。
#[derive(Debug, Clone)]
pub(crate) struct StructDef {
    pub fields: Vec<FieldDef>,
    pub methods: HashMap<String, MethodSig>,
    pub type_param_count: usize,
}

/// class 定义。
#[derive(Debug, Clone)]
pub(crate) struct ClassDef {
    pub fields: Vec<FieldDef>,
    pub methods: HashMap<String, MethodSig>,
    pub type_param_count: usize,
    /// 继承父类的原始类型表达式（`classHeritage`，文法预留多继承——本期仅取首个）
    pub(crate) parent: Option<TypeExpr>,
    /// 解析后的父类（模块, 名）；在全部模块声明收集完成后解析
    pub(crate) parent_resolved: Option<(ModuleId, String)>,
}

/// trait 定义（本期仅登记，不做实现一致性检查）。
#[derive(Debug, Clone)]
pub(crate) struct TraitDef {
    pub type_param_count: usize,
}

/// 类型别名定义（目标类型表达式按需展开，循环展开在使用点检测）。
#[derive(Debug, Clone)]
pub(crate) struct AliasDef {
    /// 泛型形参名列表（`type Pair<T> = (T, T)` 中的 `["T"]`）
    pub type_params: Vec<String>,
    pub target: TypeExpr,
}

/// 类型定义实体（注册表 [`crate::Sema::type_defs`] 的值）。
#[derive(Debug, Clone)]
pub(crate) enum TypeDef {
    Struct(StructDef),
    Class(ClassDef),
    Trait(TraitDef),
    Alias(AliasDef),
}

impl TypeDef {
    /// 泛型参数个数（浅泛型 arity 检查用）
    pub(crate) fn type_param_count(&self) -> usize {
        match self {
            TypeDef::Struct(d) => d.type_param_count,
            TypeDef::Class(d) => d.type_param_count,
            TypeDef::Trait(d) => d.type_param_count,
            TypeDef::Alias(d) => d.type_params.len(),
        }
    }
}

/// 预定义类型名归类（design.md §5.1）。
///
/// `string` 已由设计移出 `PredefinedType`，与 `PredefinedType` 枚举一并在此按名处理；
/// 返回 None 表示非预定义名，调用方继续走符号查找。
pub(crate) fn predefined_from_name(name: &str) -> Option<Ty> {
    if let Some(kind) = IntKind::from_name(name) {
        return Some(Ty::Int(kind));
    }
    if let Some(kind) = FloatKind::from_name(name) {
        return Some(Ty::Float(kind));
    }
    match name {
        "string" => Some(Ty::String),
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
        PredefinedType::F8 => Ty::Float(FloatKind::F8),
        PredefinedType::F16 => Ty::Float(FloatKind::F16),
        PredefinedType::F32 => Ty::Float(FloatKind::F32),
        PredefinedType::F64 => Ty::Float(FloatKind::F64),
        PredefinedType::Usize => Ty::Int(IntKind::Usize),
        PredefinedType::Isize => Ty::Int(IntKind::Isize),
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Unknown => write!(f, "<未知>"),
            Ty::Void => write!(f, "void"),
            Ty::Null => write!(f, "null"),
            Ty::Boolean => write!(f, "boolean"),
            Ty::String => write!(f, "string"),
            Ty::Int(k) => write!(f, "{}", k.name()),
            Ty::Float(k) => write!(f, "{}", k.name()),
            Ty::IntLiteral => write!(f, "<整数字面量>"),
            Ty::FloatLiteral => write!(f, "<浮点字面量>"),
            Ty::Pointer(t) => write!(f, "{t}*"),
            Ty::Array(t) => write!(f, "{t}[]"),
            Ty::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            Ty::Function(_) => write!(f, "<函数>"),
            Ty::Union(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{t}")?;
                }
                Ok(())
            }
            Ty::Struct(_, n) | Ty::Class(_, n) | Ty::Trait(_, n) => write!(f, "{n}"),
            Ty::TypeParam(n) => write!(f, "{n}"),
            Ty::External => write!(f, "<外部符号>"),
            Ty::Error => write!(f, "<错误类型>"),
        }
    }
}

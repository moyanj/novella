use serde::{Deserialize, Serialize};

use novella_core::Span;

use crate::{Spanned, expr::Expression};

/// 类型参数：`T` 或 `T: A + B`（对应文法 `typeParameter`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct TypeParameter {
    /// 类型参数名称
    pub name: String,
    /// 上界约束（`T: Bound` 中的 `Bound`），无约束时为 None
    pub bound: Option<TypeExpr>,
    /// `+` 分隔的后续上界，按源码顺序保存。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_bounds: Vec<TypeExpr>,
    /// 类型参数覆盖的源码区间
    pub span: Span,
}

/// 预定义的基础类型
///
/// u/i/f16/32/64 宽度类型、usize/isize、boolean 均为特殊的基础类型，
/// 在语义/代码生成阶段直接映射到目标平台的原生类型，而不是普通的类型引用。
/// 该枚举是纯值，其源码区间由承载它的 `TypeExpr::Predefined` 提供。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PredefinedType {
    /// null
    Null,
    /// boolean（原生布尔类型）
    Boolean,
    /// int (等价于i32)
    Int,
    /// float(等价于f32)
    Float,
    /// void
    Void,
    /// 8 位无符号整数
    U8,
    /// 16 位无符号整数
    U16,
    /// 32 位无符号整数
    U32,
    /// 64 位无符号整数
    U64,
    /// 8 位有符号整数
    I8,
    /// 16 位有符号整数
    I16,
    /// 32 位有符号整数
    I32,
    /// 64 位有符号整数
    I64,
    /// 16 位浮点数
    F16,
    /// 32 位浮点数
    F32,
    /// 64 位浮点数
    F64,
    /// 平台相关的无符号整数（指针宽度）
    Usize,
    /// 平台相关的有符号整数（指针宽度）
    Isize,
}

impl PredefinedType {
    /// 全部预定义类型（源码拼写顺序，与枚举声明一致）。
    ///
    /// 供补全、文档等需要枚举「全部基础类型」的消费者使用；新增变体时
    /// 编译器不会强制此处同步，但 `spellings_cover_enum` 测试会按数量比对。
    pub const ALL: [PredefinedType; 18] = [
        PredefinedType::Null,
        PredefinedType::Boolean,
        PredefinedType::Int,
        PredefinedType::Float,
        PredefinedType::Void,
        PredefinedType::U8,
        PredefinedType::U16,
        PredefinedType::U32,
        PredefinedType::U64,
        PredefinedType::I8,
        PredefinedType::I16,
        PredefinedType::I32,
        PredefinedType::I64,
        PredefinedType::F16,
        PredefinedType::F32,
        PredefinedType::F64,
        PredefinedType::Usize,
        PredefinedType::Isize,
    ];

    /// 该类型在源码中的拼写（小写关键字形）。
    ///
    /// 与 `novella_core::keywords::KEYWORDS` 共用同一批字面量，是补全/高亮的
    /// 事实来源；新增预定义类型必须同时进关键字表，否则词法无法识别。
    pub fn spelling(self) -> &'static str {
        match self {
            PredefinedType::Null => "null",
            PredefinedType::Boolean => "boolean",
            PredefinedType::Int => "int",
            PredefinedType::Float => "float",
            PredefinedType::Void => "void",
            PredefinedType::U8 => "u8",
            PredefinedType::U16 => "u16",
            PredefinedType::U32 => "u32",
            PredefinedType::U64 => "u64",
            PredefinedType::I8 => "i8",
            PredefinedType::I16 => "i16",
            PredefinedType::I32 => "i32",
            PredefinedType::I64 => "i64",
            PredefinedType::F16 => "f16",
            PredefinedType::F32 => "f32",
            PredefinedType::F64 => "f64",
            PredefinedType::Usize => "usize",
            PredefinedType::Isize => "isize",
        }
    }
}

/// 函数参数：`name`、`name: TypeExpr` 或带默认值（对应 `parameter`）
///
/// 文法的 `parameter: identifier typeAnnotation? (Assign expression)?`：
/// 默认值表达式在表达式 AST 落地后由本字段承载。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct Param {
    /// 参数名称
    pub name: String,
    /// 参数类型标注，缺失时（无 `: TypeExpr`）为 None
    pub ty: Option<TypeExpr>,
    /// 参数默认值（对应 `(Assign expression)?`），None 表示无默认值
    pub default: Option<Expression>,
    /// 参数声明覆盖的源码区间
    pub span: Span,
}

/// 函数类型（对应 `functionType` / `arrowFunctionType`，两者合并）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct FunctionType {
    /// 泛型参数（`<T>`）
    pub type_parameters: Vec<TypeParameter>,
    /// 参数列表
    pub params: Vec<Param>,
    /// 显式返回类型。函数声明省略返回注解时为 `None`，函数类型表达式始终为 `Some`。
    pub return_type: Option<Box<TypeExpr>>,
    /// 函数类型覆盖的源码区间
    pub span: Span,
}

/// 动态 trait 类型的关联类型绑定（对应文法 `associatedTypeBinding`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct AssociatedTypeBinding {
    /// 关联类型名。
    pub name: String,
    /// 关联类型标识符位置。
    pub name_span: Span,
    /// 等号右侧的类型。
    pub ty: TypeExpr,
    /// 整个绑定的源码区间。
    pub span: Span,
}

/// 类型表达式（对应 `typeExpression` / `primaryType`）。
///
/// 每个变体携带 `span` 记录类型表达式覆盖的源码区间，统一经 [`TypeExpr::span`] 访问。
#[derive(Debug, novella_derive::GuardedClone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum TypeExpr {
    /// 预定义基础类型
    Predefined {
        /// 预定义类型种类
        ty: PredefinedType,
        /// 源码区间
        span: Span,
    },
    /// 类型引用（可能带命名空间限定与泛型实参）：`Foo` / `mod.Foo<T>`
    Reference {
        /// 命名空间限定段（`mod.Foo` 中的 `["mod"]`）；非限定引用时为空。
        /// 由语义分析按命名空间导入（`import "mod";`）解析到目标模块的类型符号。
        qualifier: Vec<String>,
        /// 被引用类型的名称
        name: String,
        /// 泛型实参列表，非泛型引用时为空
        generic_args: Vec<TypeExpr>,
        /// 源码区间
        span: Span,
    },
    /// 动态 trait 类型：`dyn Iter<i32, Item = i32>`（对应文法 `dynamicType`）。
    Dynamic {
        /// trait 引用，携带位置泛型实参。
        trait_ref: Box<TypeExpr>,
        /// 同一实参列表中后置的关联类型绑定。
        bindings: Vec<AssociatedTypeBinding>,
        /// 整个动态类型的源码区间。
        span: Span,
    },
    /// 数组类型 `T[N]`，或仅用于形参的 `T[]`。
    Array {
        /// 元素类型
        element: Box<TypeExpr>,
        /// 定长数组的元素个数；省略时在形参位置退化为指针。
        length: Option<u32>,
        /// 源码区间
        span: Span,
    },
    /// 指针类型：`T*`（对应文法的 `primaryType '*'` 后缀形态）。
    ///
    /// 可嵌套 `T**`，也允许 `void*`（元素为 `Predefined::Void`）；
    /// `null` 可隐式赋给任意指针类型。指针与 `usize` 同宽（语义阶段定型）；
    /// 解引用 / 指针算术 / 指针互转均须位于 unsafe 上下文（design.md §6）。
    Pointer {
        /// 被指向的类型（可为任意类型表达式，包括指针 / 数组 / 联合）
        element: Box<TypeExpr>,
        /// 源码区间
        span: Span,
    },
    /// 元组类型：`(T1, T2)`
    Tuple {
        /// 元素类型列表
        elements: Vec<TypeExpr>,
        /// 源码区间
        span: Span,
    },
    /// 函数类型：`(T1, T2) -> U`（箭头形式）/ `func (a: int) -> int`（具名形式）
    Function(FunctionType),
    /// 联合类型：`A | B | C`（扁平存储，集合语义）
    Union {
        /// 联合成员类型列表
        types: Vec<TypeExpr>,
        /// 源码区间
        span: Span,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 预定义类型的拼写表必须覆盖枚举全部变体（`ALL` 数组长度与拼写无遗漏）。
    ///
    /// 注意：除 `null` 外，类型名在词法层都是普通 `Identifier`（见
    /// `crates/novella-parser/src/types.rs` 模块文档），因此不能断言它们在关键字表里。
    #[test]
    fn spellings_cover_enum() {
        let spellings = PredefinedType::ALL.map(PredefinedType::spelling);
        assert_eq!(spellings.len(), 18);
        let mut unique = spellings.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), spellings.len(), "拼写不得重复");
        assert_eq!(PredefinedType::Null.spelling(), "null");
        assert_eq!(PredefinedType::I32.spelling(), "i32");
    }
}

use serde::{Deserialize, Serialize};

use novella_core::Span;

use crate::{Spanned, expr::Expression};

/// 类型参数：`T` 或 `T: Bound`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct TypeParameter {
    /// 类型参数名称
    pub name: String,
    /// 上界约束（`T: Bound` 中的 `Bound`），无约束时为 None
    pub bound: Option<TypeExpr>,
    /// 类型参数覆盖的源码区间
    pub span: Span,
}

/// 预定义的基础类型
///
/// u/i/f8/16/32/64 宽度类型、usize/isize、boolean 均为特殊的基础类型，
/// 在语义/代码生成阶段直接映射到目标平台的原生类型，而不是普通的类型引用。
/// 该枚举是纯值，其源码区间由承载它的 `TypeExpr::Predefined` 提供。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PredefinedType {
    /// null
    Null,
    /// boolean（原生布尔类型）
    Boolean,
    /// string
    String,
    /// int
    Int,
    /// float
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
    /// 8 位浮点数
    F8,
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

/// 对象类型成员：`name: TypeExpr`（对应 `objectTypeMember`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct ObjectMember {
    /// 成员名称
    pub name: String,
    /// 成员类型
    pub ty: TypeExpr,
    /// 成员声明覆盖的源码区间
    pub span: Span,
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
    /// 返回类型
    pub return_type: Box<TypeExpr>,
    /// 函数类型覆盖的源码区间
    pub span: Span,
}

/// 类型表达式（对应 `typeExpression` / `primaryType`）。
///
/// 每个变体携带 `span` 记录类型表达式覆盖的源码区间，统一经 [`TypeExpr::span`] 访问。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum TypeExpr {
    /// 预定义基础类型
    Predefined {
        /// 预定义类型种类
        ty: PredefinedType,
        /// 源码区间
        span: Span,
    },
    /// 类型引用（可能带泛型实参）：`Foo` / `Foo<T>`
    Reference {
        /// 被引用类型的名称
        name: String,
        /// 泛型实参列表，非泛型引用时为空
        generic_args: Vec<TypeExpr>,
        /// 源码区间
        span: Span,
    },
    /// 数组类型：`T[]`
    Array {
        /// 元素类型
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
    /// 函数类型：`(a: T) => U` / `fn (a: T) -> U`
    Function(FunctionType),
    /// 联合类型：`A | B | C`（扁平存储，集合语义）
    Union {
        /// 联合成员类型列表
        types: Vec<TypeExpr>,
        /// 源码区间
        span: Span,
    },
}

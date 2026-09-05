/// 类型参数：`T` 或 `T: Bound`
#[derive(Debug, Clone, PartialEq)]
pub struct TypeParameter {
    /// 类型参数名称
    pub name: String,
    /// 上界约束（`T: Bound` 中的 `Bound`），无约束时为 None
    pub bound: Option<TypeExpr>,
}

/// 预定义的基础类型
///
/// u/i/f8/16/32/64 宽度类型、usize/isize、boolean 均为特殊的基础类型，
/// 在语义/代码生成阶段直接映射到目标平台的原生类型，而不是普通的类型引用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectMember {
    /// 成员名称
    pub name: String,
    /// 成员类型
    pub ty: TypeExpr,
}

/// 函数参数：`name` 或 `name: TypeExpr`（对应 `parameter`）
///
/// 文法的 `parameter` 还允许默认值 `(Assign expression)?`，
/// 但 Expression AST 尚未实现，故暂不承载默认值，待表达式节点落地后再补充。
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// 参数名称
    pub name: String,
    /// 参数类型标注，缺失时（无 `: TypeExpr`）为 None
    pub ty: Option<TypeExpr>,
}

/// 函数类型（对应 `functionType` / `arrowFunctionType`，两者合并）
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionType {
    /// 泛型参数（`<T>`）
    pub type_parameters: Vec<TypeParameter>,
    /// 参数列表
    pub params: Vec<Param>,
    /// 返回类型
    pub return_type: Box<TypeExpr>,
}

/// 类型表达式（对应 `typeExpression` / `primaryType`）
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// 预定义基础类型
    Predefined(PredefinedType),
    /// 类型引用（可能带泛型实参）：`Foo` / `Foo<T>`
    Reference {
        /// 被引用类型的名称
        name: String,
        /// 泛型实参列表，非泛型引用时为空
        generic_args: Vec<TypeExpr>,
    },
    /// 数组类型：`T[]`
    Array(Box<TypeExpr>),
    /// 元组类型：`(T1, T2)`
    Tuple(Vec<TypeExpr>),
    /// 函数类型：`(a: T) => U` / `fn (a: T) -> U`
    Function(FunctionType),
    /// 联合类型：`A | B | C`（扁平存储，集合语义）
    Union(Vec<TypeExpr>),
}

//! HIR 类型层：后端可见的 [`HirType`]、种类枚举与 `TypeView → HirType` 转换。
//!
//! 语义阶段的 [`novella_sema::TypeView`] 是**容错投影**（`Unknown` / `Null` /
//! 未定型字面量等都合法出现，供 LSP 一类只读工具使用）；HIR 是**可生成代码**的
//! 类型，二者不是同一集合。因此本模块的转换对无法生成代码的投影一律返回
//! [`BuildError`]，且 `match` 穷尽所有 `TypeView` 变体——新增投影会编译失败，
//! 而不是静默落到某个默认分支。

use novella_sema::{FunctionView, ModuleId, TypeView};

use crate::build::{BuildError, BuildErrorKind};

/// 后端可见的类型。
///
/// 与 [`novella_sema::TypeView`] 一对一映射，但与 `TypeView` 的差异：
/// - 不含 `Unknown` / `Null` / `IntLiteral` / `FloatLiteral` / `Union` / `TypeParam`
///   这些「尚不是具体类型」的投影；
/// - `Function` 携带完整的 [`FunctionSignature`]（而不是裸名义键），
///   供 codegen 直接构造 LLVM 函数类型、不做二次查表。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum HirType {
    /// 无返回值（`void`）
    Void,
    /// `bool`
    Boolean,
    /// 已定型整数
    Int(IntTy),
    /// 已定型浮点
    Float(FloatTy),
    /// 指针 `T*`（被指类型保留，供 codegen 决定布局 / 对齐）
    Pointer(Box<HirType>),
    /// 内联定长数组 `T[N]`。
    Array(Box<HirType>, u32),
    /// 元组
    Tuple(Vec<HirType>),
    /// 函数值：稳定键 + 完整签名
    Function(Box<FunctionSignature>),
    /// struct 值（名义键：所属模块 + 类型名）
    Struct(ModuleId, String),
    /// class 引用（hir 层按指针表示，保留名义键供虚调用 / 继承链查询）
    Class(ModuleId, String),
    /// 拥有型 trait 对象；名义 trait 与关联类型决定调用 ABI。
    Dynamic(Box<DynamicType>),
    /// trait 引用（仅 `is` / `as` / 约束位置有意义）
    Trait(ModuleId, String),
}

/// 动态值的具体 trait 身份，独立存储以避免膨胀普通 HIR 类型与指令。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DynamicType {
    pub module: ModuleId,
    pub name: String,
    pub args: Vec<HirType>,
    pub bindings: Vec<(String, HirType)>,
}

/// 整数种类：与 [`novella_sema`] 内部 `IntKind` 一一对应（`int` 已归一化为 `I32`）。
///
/// 名字通过 `TypeView::Int(&'static str)` 传进来，`from_name` 是**唯一**解析入口；
/// 名字集合由 sema 的 `IntKind::name` 决定，两边任何一侧增删都会在此暴露。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum IntTy {
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

impl IntTy {
    /// 按 sema 给出的类型名解析；未知名返回 `None`（由调用方转成诊断）。
    /// 按类型名识别。`i32` / `int` 等价（design.md 既定等价，语义层同口径），
    /// 因此 `TypeExpr::Reference { name: "i32" }` 也能被正确识别。
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "int" => IntTy::I32,
            "u8" => IntTy::U8,
            "u16" => IntTy::U16,
            "u32" => IntTy::U32,
            "u64" => IntTy::U64,
            "i8" => IntTy::I8,
            "i16" => IntTy::I16,
            "i32" => IntTy::I32,
            "i64" => IntTy::I64,
            "usize" => IntTy::Usize,
            "isize" => IntTy::Isize,
            _ => return None,
        })
    }

    /// 位宽（`usize` / `isize` 按目标平台 x86-64 记 64 位）。
    ///
    /// 注意：跨平台目标（32 位）需改为由 codegen 从目标数据布局取值，
    /// 这里只服务于 x86-64 的默认情形。
    pub fn bit_width(self) -> u32 {
        match self {
            IntTy::U8 | IntTy::I8 => 8,
            IntTy::U16 | IntTy::I16 => 16,
            IntTy::U32 | IntTy::I32 => 32,
            IntTy::U64 | IntTy::I64 | IntTy::Usize | IntTy::Isize => 64,
        }
    }

    /// 是否有符号（codegen 据此选 `sdiv` / `udiv` 一类指令）。
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Isize
        )
    }
}

/// 浮点种类：与 sema 内部 `FloatKind` 一一对应（`float` 已归一化为 `F32`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FloatTy {
    F16,
    F32,
    F64,
}

impl FloatTy {
    /// 按 sema 给出的类型名解析；未知名返回 `None`。
    /// 按类型名识别。`f32` / `float` 等价（同 [`IntTy::from_name`]）。
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "float" => FloatTy::F32,
            "f16" => FloatTy::F16,
            "f32" => FloatTy::F32,
            "f64" => FloatTy::F64,
            _ => return None,
        })
    }

    /// 位宽。
    pub fn bit_width(self) -> u32 {
        match self {
            FloatTy::F16 => 16,
            FloatTy::F32 => 32,
            FloatTy::F64 => 64,
        }
    }
}

/// 函数稳定键：`模块::名字`（顶层函数）/ `模块::类型::方法` / `模块::lambda#序号`。
///
/// 顶层函数与方法沿用 sema 的名义键；匿名签名只表示函数类型，
/// lambda 实体仍需构造期分配唯一键，不能把匿名类型键直接当作实体键。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct HirFunctionId(pub String);

impl HirFunctionId {
    /// 匿名签名的占位键：方法 / lambda 尚无稳定键时使用，仅用于保持结构完整，
    /// 不参与 `HirModule` 的函数查找。
    pub fn anonymous(module: ModuleId) -> Self {
        HirFunctionId(format!("m{module}::anon"))
    }
}

impl std::fmt::Display for HirFunctionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 函数签名（HIR 版）：codegen 用 `params + ret` 建 LLVM 函数类型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FunctionSignature {
    /// 稳定符号键
    pub id: HirFunctionId,
    /// 形参类型（按声明顺序）
    pub params: Vec<HirType>,
    /// 返回类型
    pub ret: HirType,
    /// 泛型参数个数（浅泛型：仅 arity 有意义）
    pub type_param_count: usize,
    /// `async` 标记（`await` 的降级策略确定前，codegen 应拒绝生成）
    pub is_async: bool,
    /// `extern` 标记（只有声明、没有函数体）
    pub is_extern: bool,
    /// 是否带隐式接收者（实例方法）
    ///
    /// `self` 不在 `params` 里：接收者类型由方法所属的具名类型决定，
    /// codegen 生成方法符号时按需把它插到参数表最前面。
    pub receiver: bool,
}

impl HirType {
    /// 比较值类型：函数实体名、外部链接属性不属于函数值的签名。
    pub fn same_value_type(&self, other: &Self) -> bool {
        let same_list = |a: &[Self], b: &[Self]| {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same_value_type(b))
        };
        match (self, other) {
            (Self::Function(a), Self::Function(b)) => {
                a.receiver == b.receiver
                    && a.type_param_count == b.type_param_count
                    && a.is_async == b.is_async
                    && same_list(&a.params, &b.params)
                    && a.ret.same_value_type(&b.ret)
            }
            (Self::Tuple(a), Self::Tuple(b)) => same_list(a, b),
            (Self::Array(a, left), Self::Array(b, right)) => left == right && a.same_value_type(b),
            (Self::Pointer(a), Self::Pointer(b)) => a.same_value_type(b),
            (Self::Dynamic(a), Self::Dynamic(b)) => {
                a.module == b.module
                    && a.name == b.name
                    && same_list(&a.args, &b.args)
                    && a.bindings.len() == b.bindings.len()
                    && a.bindings
                        .iter()
                        .zip(&b.bindings)
                        .all(|((ak, av), (bk, bv))| ak == bk && av.same_value_type(bv))
            }
            _ => self == other,
        }
    }

    /// 从语义投影降级为 HIR 类型。
    ///
    /// `function_id`：遇到 `TypeView::Function` 时用于合成 / 取用稳定键的回调。
    /// 顶层函数由调用方提供真实名字，方法 / lambda 提供合成键。
    ///
    /// 失败情形见 [`BuildErrorKind`]；`match` 穷尽 `TypeView` 全部变体，
    /// sema 新增投影会在此处编译失败（与项目对穷尽映射的要求一致）。
    /// 投影为 HIR 类型。
    ///
    /// `preferred`（可选）：上下文期望类型。它只用于把 sema 的**未定型字面量**
    /// 落地成具体类型（`TypeView::IntLiteral` / `FloatLiteral`）—— 这类表达式
    /// 的类型表里永远是字面量态，但它的实际类型由使用它的上下文决定。
    /// 没有上下文时仍然 fail-fast。
    /// 带上下文期望类型的投影。
    pub(crate) fn from_view_with(
        view: &TypeView,
        function_id: &mut dyn FnMut(&FunctionView) -> HirFunctionId,
        preferred: Option<&HirType>,
    ) -> Result<Self, BuildError> {
        match view {
            TypeView::Applied(_, _) => Err(BuildError::at(BuildErrorKind::GenericUnsupported)),
            TypeView::Future(_) => Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "Future 类型",
            })),
            TypeView::Object(_) => Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "匿名对象类型",
            })),
            TypeView::Void => Ok(HirType::Void),
            TypeView::Boolean => Ok(HirType::Boolean),
            TypeView::Int(name) => IntTy::from_name(name)
                .map(HirType::Int)
                .ok_or_else(|| BuildError::at(BuildErrorKind::UnknownTypeName { name })),
            TypeView::Float(name) => FloatTy::from_name(name)
                .map(HirType::Float)
                .ok_or_else(|| BuildError::at(BuildErrorKind::UnknownTypeName { name })),
            TypeView::Pointer(inner) => Ok(HirType::Pointer(Box::new(Self::from_view_with(
                inner,
                function_id,
                None,
            )?))),
            TypeView::Array(element, length) => {
                let element_hint = match preferred {
                    Some(HirType::Array(ty, _) | HirType::Pointer(ty)) => Some(ty.as_ref()),
                    _ => None,
                };
                Ok(HirType::Array(
                    Box::new(Self::from_view_with(element, function_id, element_hint)?),
                    *length,
                ))
            }
            TypeView::Tuple(elements) => elements
                .iter()
                .enumerate()
                .map(|(index, element)| {
                    let hint = match preferred {
                        Some(HirType::Tuple(types)) => types.get(index),
                        _ => None,
                    };
                    Self::from_view_with(element, function_id, hint)
                })
                .collect::<Result<Vec<_>, _>>()
                .map(HirType::Tuple),
            TypeView::Function(sig) => {
                let signature = FunctionSignature::from_view_with(sig, function_id)?;
                Ok(HirType::Function(Box::new(signature)))
            }
            TypeView::Struct(module, name) => Ok(HirType::Struct(*module, name.clone())),
            TypeView::Class(module, name) => Ok(HirType::Class(*module, name.clone())),
            TypeView::Trait(module, name) => Ok(HirType::Trait(*module, name.clone())),
            TypeView::Dynamic {
                trait_ref,
                bindings,
            } => {
                let (base, args) = match trait_ref.as_ref() {
                    TypeView::Applied(base, args) => (base.as_ref(), args.as_slice()),
                    base => (base, &[][..]),
                };
                let TypeView::Trait(module, name) = base else {
                    return Err(BuildError::at(BuildErrorKind::InvalidInput));
                };
                let args = args
                    .iter()
                    .map(|ty| Self::from_view_with(ty, function_id, None))
                    .collect::<Result<_, _>>()?;
                let bindings = bindings
                    .iter()
                    .map(|(name, ty)| {
                        Self::from_view_with(ty, function_id, None).map(|ty| (name.clone(), ty))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Self::Dynamic(Box::new(DynamicType {
                    module: *module,
                    name: name.clone(),
                    args,
                    bindings,
                })))
            }
            TypeView::Associated { .. } => Err(BuildError::at(BuildErrorKind::GenericUnsupported)),
            // —— 以下投影不能作为 codegen 输入，一律 fail-fast ——
            TypeView::Unknown => Err(BuildError::at(BuildErrorKind::OpaqueType)),
            TypeView::Error => Err(BuildError::at(BuildErrorKind::InvalidInput)),
            TypeView::Null => match preferred {
                Some(ty @ (HirType::Pointer(_) | HirType::Class(..))) => Ok(ty.clone()),
                _ => Err(BuildError::at(BuildErrorKind::UntypedLiteral {
                    what: "null",
                })),
            },
            // 未定型字面量：只有上下文能定型时才算合法
            TypeView::IntLiteral => match preferred {
                Some(ty @ HirType::Int(_)) => Ok(ty.clone()),
                _ => Err(BuildError::at(BuildErrorKind::UntypedLiteral {
                    what: "int literal",
                })),
            },
            TypeView::FloatLiteral => match preferred {
                Some(ty @ HirType::Float(_)) => Ok(ty.clone()),
                _ => Err(BuildError::at(BuildErrorKind::UntypedLiteral {
                    what: "float literal",
                })),
            },
            TypeView::Union(_) => Err(BuildError::at(BuildErrorKind::UnionUnsupported)),
            TypeView::TypeParam(_) => Err(BuildError::at(BuildErrorKind::GenericUnsupported)),
        }
    }
}

impl FunctionSignature {
    /// 从语义签名投影构造；形参 / 返回类型同样走 [`HirType::from_view_with`]。
    ///
    /// 形参中出现 `TypeView::Unknown` 的情况（未标注类型的形参）会 fail-fast：
    /// 这类签名无法确定调用约定，不能进入后端。
    /// 返回值 / 形参不得为未定型字面量。
    fn from_view_with(
        view: &FunctionView,
        function_id: &mut dyn FnMut(&FunctionView) -> HirFunctionId,
    ) -> Result<Self, BuildError> {
        if view.type_param_count != 0 {
            return Err(BuildError::at(BuildErrorKind::GenericUnsupported));
        }
        if view.is_async {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "async function",
            }));
        }
        let params = view
            .params
            .iter()
            .map(|param| HirType::from_view_with(param, function_id, None))
            .collect::<Result<Vec<_>, _>>()?;
        let ret = HirType::from_view_with(&view.ret, function_id, None)?;
        Ok(FunctionSignature {
            id: function_id(view),
            params,
            ret,
            type_param_count: view.type_param_count,
            is_async: view.is_async,
            is_extern: view.is_extern,
            receiver: view.receiver,
        })
    }
}

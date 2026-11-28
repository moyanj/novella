//! 语义类型 [`Ty`]：类型表达式的解析结果、可赋值性判定与字面量类型解析。
//!
//! 设计要点（规则索引见 `docs/sema-coverage.md`）：
//! - `int` ≡ `i32`、`float` ≡ `f32`（design.md 既定等价），解析时直接归一化；
//! - 数值字面量先记为未定型的 [`Ty::IntLiteral`] / [`Ty::FloatLiteral`]，
//!   可赋给任意同族数值类型（按目标类型的溢出检查属后续期）；
//! - 数值隐式转换只允许同符号无损扩宽（`i32 → i64`）；窄化与跨符号一律报错；
//! - 隐式转换仅 `null → 任意指针/class`、`void* ↔ 任意 T*`、子类 → 父类。

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use novella_ast::types::{PredefinedType, TypeExpr};
use novella_core::Span;

use crate::module::ModuleId;

mod generics;
mod literals;
mod relations;
pub(crate) use generics::*;
pub(crate) use literals::target_int_kind;
pub(crate) use literals::*;
pub(crate) use relations::*;

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

    /// 是否有符号（供无损扩宽判定 [`IntKind::widens_to`] 使用）。
    pub(crate) fn is_signed(self) -> bool {
        matches!(
            self,
            IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::I64 | IntKind::Isize
        )
    }

    /// 位宽（位）。
    ///
    /// `usize` / `isize` 的位宽取决于目标平台：sema 阶段按 64 位判定，与
    /// [`int_literal_fits_non_negative`] 的范围判定口径一致。
    fn bit_width(self) -> u32 {
        match self {
            IntKind::U8 | IntKind::I8 => 8,
            IntKind::U16 | IntKind::I16 => 16,
            IntKind::U32 | IntKind::I32 => 32,
            IntKind::U64 | IntKind::I64 | IntKind::Usize | IntKind::Isize => 64,
        }
    }

    /// `self → target` 是否为同符号整数的**无损扩宽**（design.md §5.2.1）。
    ///
    /// 只有「同符号且目标位宽更大」才允许：`i32 → i64`、`u8 → u16` 可行，
    /// `i64 → i32`（窄化）、`i32 → u64`（跨符号）、`u32 → u64` 之外的任意组合
    /// 一律不可。窄化与跨符号必须显式 `as`。
    pub(crate) fn widens_to(self, target: IntKind) -> bool {
        self.is_signed() == target.is_signed() && self.bit_width() < target.bit_width()
    }
}

/// 浮点种类（`float` 已归一化为 `F32`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FloatKind {
    F16,
    F32,
    F64,
}

impl FloatKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            FloatKind::F16 => "f16",
            FloatKind::F32 => "f32",
            FloatKind::F64 => "f64",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
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
    /// 所属模块（名义键的一半）
    pub module: ModuleId,
    /// 函数名（名义键另一半）：顶层函数使用原名，方法使用 `类型::方法`；
    /// lambda / 占位签名为 `None`，投影仍保留完整参数与返回结构。
    pub name: Option<String>,
    pub params: Vec<ParamSig>,
    pub ret: Ty,
    /// 泛型参数个数（调用点按 arity 校验显式实参）
    pub type_param_count: usize,
    /// 泛型形参名（按声明顺序；D05 的显式实参代入与实参推导都据此建立替换表）
    pub type_params: Vec<String>,
    pub is_async: bool,
    pub is_extern: bool,
    /// 实例方法（隐式接收 `this`）：首个无注解的 `self` 绑定为属主类型。
    /// 投影时该形参整体略去，
    /// 接收者改由 [`FunctionView::receiver`] 显式表达。
    pub is_method: bool,
}

impl FunctionSig {
    /// 公开投影（携带完整参数 / 返回结构，含匿名 lambda）。
    ///
    /// 使上游能据它构造类型与签名，而不是只拿到一个名义键。
    /// `self` 占位形参不进入 `params`，改由 `receiver` 标记，接收者类型由
    /// 方法所属的类型定义（[`crate::TypeSummary::name`]）决定。
    pub(crate) fn view(&self) -> crate::FunctionView {
        let mut params = Vec::with_capacity(self.params.len());
        let receiver = self.is_method;
        for (index, param) in self.params.iter().enumerate() {
            if self.is_method && param.name == "self" && index == 0 {
                continue;
            }
            params.push(param.ty.view());
        }
        crate::FunctionView {
            module: self.module,
            name: self.name.clone(),
            params,
            ret: Box::new(self.ret.view()),
            type_param_count: self.type_param_count,
            type_parameters: self.type_params.clone(),
            is_async: self.is_async,
            is_extern: self.is_extern,
            receiver,
        }
    }
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
    Int(IntKind),
    Float(FloatKind),
    /// 未定型整数字面量（如 `42`），可赋给任意整数类型。
    ///
    /// 载荷是 `(是否取负, 绝对值)`：`42` → `(false, 42)`，`-128` → `(true, 128)`。
    /// 数值只参与目标位宽的范围判定（见 [`int_literal_fits_non_negative`] /
    /// [`int_negated_literal_fits`]），公开投影 [`Ty::view`] 始终是
    /// `TypeView::IntLiteral`——下游看到的仍是「未定型字面量」，HIR / CLI / LSP 无需变动。
    IntLiteral(bool, u128),
    /// 未定型浮点字面量（如 `1.5`），可赋给任意浮点类型。
    ///
    /// 载荷是字面量按 `f64` 解析出的值，只用于目标类型的**有限性 / 范围**判定
    /// （L05–L06）；公开投影 [`Ty::view`] 仍是 `TypeView::FloatLiteral`，
    /// HIR / CLI / LSP 无需变动。
    FloatLiteral(f64),
    Pointer(Box<Ty>),
    Array(Box<Ty>, u32),
    Tuple(Vec<Ty>),
    Function(Rc<FunctionSig>),
    Union(Vec<Ty>),
    /// 具名泛型实例保留实参；不与未实例化的类型定义混同。
    Applied(Box<Ty>, Vec<Ty>),
    Future(Box<Ty>),
    Dynamic {
        trait_ref: Box<Ty>,
        bindings: Vec<(String, Ty)>,
    },
    Associated {
        receiver: Box<Ty>,
        trait_ref: Box<Ty>,
        name: String,
    },
    Object(std::collections::BTreeMap<String, Ty>),
    /// struct 类型（按模块 + 名名义判等）
    Struct(ModuleId, String),
    /// class 引用类型
    Class(ModuleId, String),
    /// trait 类型（仅 `is` / `as` / 约束位置有意义）
    Trait(ModuleId, String),
    /// 泛型类型参数（浅泛型：不透明）
    TypeParam(String),
    /// 错误类型（产生处已报过明确诊断）：向下传播时不产生级联诊断，
    /// 但不参与任何兼容判定之外的推导（见 [`Ty::is_opaque`]）。
    Error,
}

impl Ty {
    /// 投影为公开视图 [`crate::TypeView`]（供 codegen 消费）。
    ///
    /// 未完成定型与已诊断错误分别保留 Unknown / Error，下游均拒绝生成代码。
    pub(crate) fn view(&self) -> crate::TypeView {
        use crate::TypeView;
        match self {
            Ty::Unknown => TypeView::Unknown,
            Ty::Error => TypeView::Error,
            Ty::Void => TypeView::Void,
            Ty::Null => TypeView::Null,
            Ty::Boolean => TypeView::Boolean,
            Ty::Int(kind) => TypeView::Int(kind.name()),
            Ty::Float(kind) => TypeView::Float(kind.name()),
            Ty::IntLiteral(..) => TypeView::IntLiteral,
            Ty::FloatLiteral(_) => TypeView::FloatLiteral,
            Ty::Pointer(inner) => TypeView::Pointer(Box::new(inner.view())),
            Ty::Array(element, length) => TypeView::Array(Box::new(element.view()), *length),
            Ty::Tuple(elements) => TypeView::Tuple(elements.iter().map(Ty::view).collect()),
            Ty::Union(members) => TypeView::Union(members.iter().map(Ty::view).collect()),
            Ty::Applied(base, args) => {
                TypeView::Applied(Box::new(base.view()), args.iter().map(Ty::view).collect())
            }
            Ty::Future(inner) => TypeView::Future(Box::new(inner.view())),
            Ty::Dynamic {
                trait_ref,
                bindings,
            } => TypeView::Dynamic {
                trait_ref: Box::new(trait_ref.view()),
                bindings: bindings
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.view()))
                    .collect(),
            },
            Ty::Associated {
                receiver,
                trait_ref,
                name,
            } => TypeView::Associated {
                receiver: Box::new(receiver.view()),
                trait_ref: Box::new(trait_ref.view()),
                name: name.clone(),
            },
            Ty::Object(fields) => {
                TypeView::Object(fields.iter().map(|(n, t)| (n.clone(), t.view())).collect())
            }
            // 函数签名：公开投影携带完整参数 / 返回结构（含匿名 lambda），
            // 使 codegen 能据此建 FunctionType，而不是只能拿到名义键
            Ty::Function(sig) => TypeView::Function(Box::new(sig.view())),
            Ty::Struct(module, name) => TypeView::Struct(*module, name.clone()),
            Ty::Class(module, name) => TypeView::Class(*module, name.clone()),
            Ty::Trait(module, name) => TypeView::Trait(*module, name.clone()),
            Ty::TypeParam(name) => TypeView::TypeParam(name.clone()),
        }
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(
            self,
            Ty::Int(_) | Ty::Float(_) | Ty::IntLiteral(..) | Ty::FloatLiteral(_)
        )
    }

    pub(crate) fn is_integer(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::IntLiteral(..))
    }

    /// 是否可参与判空（指针 / class / null 自身）
    pub(crate) fn is_nullable(&self) -> bool {
        matches!(self, Ty::Pointer(_) | Ty::Class(..) | Ty::Null)
            || matches!(self, Ty::Applied(base, _) if matches!(**base, Ty::Class(..)))
    }

    /// 是否是不透明占位（Unknown / Error）：参与检查时一律放行，抑制级联。
    /// `Error` 是 `Unknown` 以外的另一档：来源处已带明确诊断（如演示文件中的
    /// `yield`、语义未实现的写法），后续依赖它的位置不再重复报错。
    pub(crate) fn is_opaque(&self) -> bool {
        matches!(self, Ty::Unknown | Ty::Error)
    }
}
// ---------- 类型定义注册表 ----------

/// 字段定义（struct / class 成员）。
#[derive(Debug, Clone)]
pub(crate) struct FieldDef {
    pub name_span: Span,
    pub(crate) name: String,
    pub ty: Ty,
    /// 是否有声明处默认值（StructInit 可省略该字段）
    pub has_default: bool,
    /// 是否为弱引用字段（`weak` 修饰，仅 class 字段有意义）
    pub is_weak: bool,
    /// 是否为静态字段（`static` 修饰：不出现在实例布局里，而是一个全局）
    pub is_static: bool,
    /// 成员可见性（D02：缺省 `pri`，即仅声明类型内部可见）
    pub(crate) is_public: bool,
    /// 声明所在的模块（`pub` 之外的成员只在同模块内可见，D02）
    pub(crate) module: ModuleId,
}

/// 方法签名（含静态标记；`this` 不占用参数位）。
#[derive(Debug, Clone)]
pub(crate) struct MethodSig {
    pub name_span: Span,
    pub sig: FunctionSig,
    pub is_static: bool,
    /// 成员可见性（D02：缺省 `pri`，即仅声明类型内部可见）
    pub(crate) is_public: bool,
    /// 方法名（不含 `类型::` 前缀；成员查找与覆写校验用）
    pub(crate) name: String,
    /// 是否只有声明没有函数体（D02：抽象方法，`func f(self) -> T;`）
    pub(crate) is_abstract: bool,
    /// 方法声明的位置（诊断 / 覆写校验定位用）
    pub(crate) span: Span,
}

/// struct 定义。
#[derive(Debug, Clone)]
pub(crate) struct StructDef {
    pub trait_types: Vec<Ty>,
    pub heritage: Vec<TypeExpr>,
    pub span: Span,
    pub associated: Vec<(Ty, String, Ty)>,
    pub type_params: Vec<String>,
    pub fields: Vec<FieldDef>,
    pub methods: HashMap<String, MethodSig>,
    pub type_param_count: usize,
}

/// class 定义。
#[derive(Debug, Clone)]
pub(crate) struct ClassDef {
    pub associated: Vec<(Ty, String, Ty)>,
    pub parent_type: Option<Ty>,
    pub trait_types: Vec<Ty>,
    pub type_params: Vec<String>,
    pub fields: Vec<FieldDef>,
    pub methods: HashMap<String, MethodSig>,
    pub type_param_count: usize,
    /// 解析后的父类（模块, 名）；在全部模块声明收集完成后解析
    pub(crate) parent_resolved: Option<(ModuleId, String)>,
    /// 类名（诊断用；类型键已带名，保留供错误消息直接引用）
    #[allow(dead_code)]
    pub name: String,
    /// class 声明覆盖的源码区间（诊断定位用，类定义节点本身不入注册表）
    pub span: Span,
    /// 声明的 trait 实现列表（`class C (P, Trait1, Trait2)` 中除首项外的类型表达式）
    pub trait_impls: Vec<TypeExpr>,
    /// 解析后的 trait 实现列表；在全部模块声明收集完成后解析
    pub trait_impls_resolved: Vec<(ModuleId, String)>,
}

/// trait 定义（D04：登记成员签名，供 class 实现一致性检查）。
#[derive(Debug, Clone)]
pub(crate) struct TraitDef {
    pub associated: HashMap<String, Span>,
    pub type_params: Vec<String>,
    pub type_param_count: usize,
    /// 要求实现的方法（按名索引；抽象 trait 方法的签名）
    pub methods: std::collections::HashMap<String, MethodSig>,
    /// 声明的类型名（诊断用）
    pub name: String,
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
    pub(crate) fn type_params(&self) -> &[String] {
        match self {
            Self::Struct(d) => &d.type_params,
            Self::Class(d) => &d.type_params,
            Self::Trait(d) => &d.type_params,
            Self::Alias(d) => &d.type_params,
        }
    }
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
impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Unknown => write!(f, "<未知>"),
            Ty::Void => write!(f, "void"),
            Ty::Null => write!(f, "null"),
            Ty::Boolean => write!(f, "boolean"),
            Ty::Int(k) => write!(f, "{}", k.name()),
            Ty::Float(k) => write!(f, "{}", k.name()),
            Ty::IntLiteral(negative, value) => {
                write!(f, "{}{value}", if *negative { "-" } else { "" })
            }
            Ty::FloatLiteral(value) => write!(f, "{value:?}"),
            Ty::Pointer(t) => write!(f, "{t}*"),
            Ty::Array(t, length) => write!(f, "{t}[{length}]"),
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
            Ty::Future(inner) => write!(f, "Future<{inner}>"),
            Ty::Associated { receiver, name, .. } => write!(f, "{receiver}.{name}"),
            Ty::Dynamic {
                trait_ref,
                bindings,
            } => {
                write!(f, "dyn {trait_ref}")?;
                if !bindings.is_empty() {
                    write!(
                        f,
                        "<{}>",
                        bindings
                            .iter()
                            .map(|(name, ty)| format!("{name} = {ty}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
                Ok(())
            }
            Ty::Object(fields) => write!(
                f,
                "{{{}}}",
                fields
                    .iter()
                    .map(|(n, t)| format!("{n}: {t}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Applied(base, args) => write!(
                f,
                "{base}<{}>",
                args.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
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
            Ty::Error => write!(f, "<错误类型>"),
        }
    }
}

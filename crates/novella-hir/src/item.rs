//! HIR 顶层结构：程序 → 模块 → 项（函数 / extern / 全局）。
//!
//! 与 AST 的 `Program` 对应，但去掉了对后端无意义的语法信息
//! （编译器指令 token 流、导入语句、类型别名、可见性修饰符的原始拼写等）；
//! 同时把「类型注册表」需要的东西（struct 字段布局、class 继承）显式列出，
//! 使 codegen 不必回头查 `TypeHierarchy`。

use serde::{Deserialize, Serialize};

use novella_sema::ModuleId;

use crate::body::HirBlock;
use crate::ty::{HirFunctionId, HirType};

/// 整个程序（仅保留需要发射的模块；模块 id 沿用 sema，可能不连续）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HirProgram {
    pub modules: Vec<HirModule>,
}

impl HirProgram {
    /// 按 [`ModuleId`] 取模块。
    pub fn module(&self, id: ModuleId) -> Option<&HirModule> {
        self.modules.iter().find(|module| module.id == id)
    }
}

/// 单个模块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HirModule {
    /// 模块 id（沿用 sema 的模块身份，不一定等于 HIR 列表下标）
    pub id: ModuleId,
    /// 调用方传入的原始路径（与 `AnalyzedModule::path` 同源，供符号名 / 调试信息）
    pub path: String,
    /// 顶层项（函数 / extern / 全局变量）
    pub items: Vec<HirItem>,
    /// 具名聚合类型定义（struct / class / trait）
    pub types: Vec<HirTypeDef>,
}

/// 顶层项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HirItem {
    /// 有函数体的函数
    Function(HirFunction),
    /// 仅声明（`extern func`）：codegen 只产出 declaration，不建基本块
    ExternFunction(HirFunction),
    /// 全局变量
    Global(HirGlobal),
}

/// 函数：稳定键 + 已解析签名 + 函数体。
///
/// 签名同时出现在 `HirItem` 与 `HirType::Function` 中：前者是**定义**，
/// 后者是**值类型**，二者共用 [`crate::FunctionSignature`] 以免两套结构走散。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HirFunction {
    /// 定义模块；lambda 在默认表达式展开后仍归属此模块。
    pub source_module: ModuleId,
    pub is_lambda: bool,
    /// 稳定符号键（codegen 用它作 LLVM 符号名）
    pub id: HirFunctionId,
    /// extern 的真实链接符号；普通函数和 lambda 使用内部名义键。
    pub link_name: Option<String>,
    /// 完整签名
    pub signature: crate::ty::FunctionSignature,
    /// 形参局部（按声明顺序，codegen 按 ABI 从 `get_nth_param` 拷入）
    pub params: Vec<LocalSlot>,
    /// 函数体（`ExternFunction` 恒为空块）
    pub body: HirBlock,
    /// 可见性（决定 LLVM linkage；`extern` 恒为 `Public`）
    pub visibility: HirVisibility,
}

/// 形参对应的局部槽位。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalSlot {
    pub id: crate::body::LocalId,
    /// 形参名（仅用于调试信息；空名由构造期合成 `arg0`…）
    pub name: String,
    pub ty: HirType,
}

/// 全局变量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HirGlobal {
    pub name: String,
    pub ty: HirType,
    /// 初值（`None` 表示零初始化）
    pub init: Option<crate::body::HirOperand>,
    pub is_mutable: bool,
    pub visibility: HirVisibility,
}

/// 可见性 → LLVM linkage 的映射在 codegen 决定，这里只记录语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HirVisibility {
    Public,
    Private,
    Package,
}

/// 具名聚合类型定义。
///
/// struct 值语义（`docs/design.md` §4.7）：字段按声明顺序紧排；
/// class 为引用语义，字段在堆对象内布局，`parent` 决定前缀布局（父字段在前）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HirTypeDef {
    /// 值类型：`fields` 顺序即 LLVM struct 元素顺序
    Struct {
        name: String,
        fields: Vec<HirField>,
        /// 方法（struct 方法同样登记：调用点据 `is_static` 决定是否带接收者）
        methods: Vec<HirMethod>,
    },
    /// 引用类型
    Class {
        name: String,
        /// 父类（`None` 表示无继承）
        parent: Option<(ModuleId, String)>,
        fields: Vec<HirField>,
        /// 方法（codegen 据此建虚表；非虚方法同样登记，由调用点决定直接调用）
        methods: Vec<HirMethod>,
    },
    /// 接口：只有方法签名，没有布局
    Trait {
        name: String,
        methods: Vec<HirMethod>,
    },
}

/// 方法声明：名字 + 已解析签名 + 静态标记（`this` 不占参数位）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HirMethod {
    pub name: String,
    pub signature: crate::ty::FunctionSignature,
    /// 静态方法：无 `this`，可作直接调用；实例方法带隐藏的接收者
    pub is_static: bool,
}

/// 字段（struct / class 共用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HirField {
    pub name: String,
    pub ty: HirType,
    /// `weak` 标记：赋值与销毁不 retain/release，读取不提升为临时强引用
    /// （`docs/design.md` §7.4）；布局与普通 class 引用一致，不自动置空。
    pub is_weak: bool,
    /// `static` 标记：不出现在实例布局里，而是一个全局
    pub is_static: bool,
}

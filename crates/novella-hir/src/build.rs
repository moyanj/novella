//! HIR 构造：从 [`novella_sema::SemaResult`] 降级出 [`HirProgram`]。
//!
//! # 类型来源（关键设计）
//!
//! 指令、注解和声明类型来自 **sema 的三份类型投影表**
//! （expression_types / annotation_types / declaration_types，键 `(ModuleId, Span)`），而不是 HIR 自己
//! 重推。这正是引入 HIR 的目的：语义阶段的类型结论被固化进结构，codegen
//! 只需读字段，不再回头查表、不再重建推导上下文。
//!
//! 查询失败（sema 未登记该表达式）一律 fail-fast，不做「猜一个类型」。
//!
//! # 失败策略
//!
//! **整体 fail-fast**：遇到第一个无法降级的构造就返回 [`BuildError`]，不收集多个、
//! 不做部分输出。HIR 是「可生成代码」的表示，任何一处不确定都意味着后端无法
//! 保证正确性；部分降级只会把错误推迟到 LLVM 侧（更难定位）。
//!
//! 调用方（CLI）负责前置判断：`diagnostics.has_errors()` 为真时不应进入此处。
//!
//! # 覆盖与边界
//!
//! 支持定型标量、元组与数组、递归解构、数组 for-in、C-for 与 while、
//! with 绑定、短路（含指针空合并）、复杂左值、match、无捕获 lambda、
//! 默认实参、自由/静态函数与 struct 直接实例调用、跨模块继承字段布局。
//! 循环条件 prelude 每轮执行，continue 先执行 Loop.step 再检查条件。
//!
//! 异常、异步、class 构造/动态派发、闭包捕获、非 cfg 装饰器、动态对象、
//! 静态/弱字段及 trait 成员明确拒绝；完整契约与验证见 docs/hir-handover.md。

use std::collections::{HashMap, HashSet};

use novella_ast::expr::{
    AssignOp, BinaryOp, Expression, Lambda, Literal, Match, MatchCaseBody, MatchPattern, UnaryOp,
};
use novella_ast::program::{ClassMember, FunctionItem, Item, Program, Scope};
use novella_ast::stmt::{
    BindingPattern, Block, ForInit, ForKind, IfBranch, IfStmt, Statement, VariableStmt,
};
use novella_ast::types::TypeExpr;
use novella_core::Span;
use novella_sema::{AnalyzedModule, ModuleId, SemaResult, TypeView};

use crate::body::{
    HirBinaryOp, HirBlock, HirCond, HirInstr, HirOperand, HirStmt, HirUnaryOp, LocalId,
};
use crate::item::{
    HirField, HirFunction, HirItem, HirMethod, HirModule, HirProgram, HirTypeDef, HirVisibility,
    LocalSlot,
};
use crate::ty::{FunctionSignature, HirFunctionId, HirType, IntTy};

mod aggregate;
mod call;
mod coalesce;
mod context;
mod error;
mod expr;
mod helpers;
mod layout;
mod loops;
mod lvalue;
mod module;
mod operators;
mod stmt;
mod types;

pub use error::{BuildError, BuildErrorKind};
use helpers::*;
use layout::instance_fields;
pub use module::build_hir;
use types::annotation_type;

/// 单个模块的构造状态。
struct ModuleBuilder<'a> {
    sema: &'a SemaResult,
    module: ModuleId,
    program: &'a Program,
    /// 已登记的函数签名
    signatures: HashMap<HirFunctionId, FunctionSignature>,
    /// 顶层函数名 → 稳定键（标识符解析用）
    functions: Vec<(String, HirFunctionId)>,
    /// 顶层函数签名（方法体构造时复用，避免重复解析类型注解）
    top_signatures: HashMap<String, FunctionSignature>,
    /// 已降级的 lambda 函数（挂在模块函数项末尾）
    pending: Vec<Vec<HirFunction>>,
    /// 具名类型定义（延后到 `finish_types`）
    struct_items: Vec<&'a Item>,
    class_items: Vec<&'a Item>,
    trait_items: Vec<&'a Item>,
}

/// 函数体构造器：把 AST 语句 / 表达式展开成「块 → 语句 / 指令」。
struct BodyBuilder<'a> {
    sema: &'a SemaResult,
    module: ModuleId,
    /// 本模块的顶层函数（标识符解析时按名字匹配）
    ///
    /// 持有副本而非借用：`ModuleBuilder` 在函数体降级期间还要把 lambda
    /// 推进 `pending`，借用会让 `&mut self` 无法使用。
    functions: Vec<(String, HirFunctionId)>,
    /// 局部槽位分配（形参与临时值共用）
    next_local: u32,
    /// 局部类型表（`LocalId` → `HirType`），供隐式转换与左值运算使用
    local_types: Vec<HirType>,
    /// 名字 → 局部（作用域链，嵌套块离开时截断）
    locals: Vec<(String, LocalId)>,
    /// 外层局部名仅用于检测尚未支持的闭包捕获。
    capture_names: HashSet<String>,
    /// 循环嵌套深度：`break` / `continue` 只允许出现在循环内
    loop_depth: usize,
    /// 当前函数的返回类型（`return` 的期望类型，用于定型返回的字面量）
    return_type: Option<HirType>,
    /// 已声明的形参槽位（按声明顺序，codegen 据此从 ABI 参数拷入）
    params: Vec<LocalSlot>,
    /// `with` 绑定的接收者类型（成员名隐式解析到该类型的字段）
    with_receiver: Option<(HirType, HirOperand)>,
    /// 本模块内待生成的 lambda 函数（模块循环里逐个降级）
    lambdas: Vec<HirFunction>,
}

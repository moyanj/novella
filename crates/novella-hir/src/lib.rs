//! HIR：语义分析之后、codegen 之前的降级中间表示。
//!
//! # 设计约束（改动前先读）
//!
//! - 只依赖 `novella-core` + `novella-ast` + `novella-sema`，**不出现 inkwell / LLVM 类型**：
//!   LLVM 版本绑定留在 codegen 一处，前端不随 LLVM 版本浮动；
//! - 语法层上下文（`with_stack` / `in_match_subject` / `as` 消歧等解析期标记）
//!   在此全部消除。下游只面对「块 → 语句/指令」两级结构，不需要重建解析状态；
//! - 类型一律为已解析的 [`HirType`]：语义阶段的容错投影（`Unknown` /
//!   `External` / `Error`）、未定型字面量（`IntLiteral` / `FloatLiteral`）、
//!   联合类型与泛型形参**不得**进入 HIR，构造期即 fail-fast 进 [`BuildError`]；
//! - 函数统一获得稳定 [`HirFunctionId`]（顶层函数用名字，方法 / lambda 由构造期
//!   使用属主 / 源码位置键），使 `FunctionSig.name: None` 不再退化为「没有签名」。
//!
//! # 模块划分
//!
//! - [`ty`]：后端可见类型 [`HirType`] 与 `TypeView → HirType` 的穷尽转换（叶子层）；
//! - [`body`]：函数体结构 [`HirBlock`] / [`HirStmt`] / [`HirOperand`]；
//! - [`item`]：模块与顶层项 [`HirProgram`] / [`HirModule`] / [`HirFunction`]；
//! - [`build`]：唯一入口 [`build_hir`]，从 [`novella_sema::SemaResult`] 降级。

pub mod body;
pub mod build;
pub mod item;
pub mod ty;

pub use body::{
    HirBinaryOp, HirBlock, HirCond, HirInstr, HirOperand, HirStmt, HirUnaryOp, LocalId,
};
pub use build::{BuildError, BuildErrorKind, build_hir};
pub use item::{
    HirField, HirFunction, HirGlobal, HirItem, HirMethod, HirModule, HirProgram, HirTypeDef,
    HirVisibility, LocalSlot,
};
pub use ty::{FloatTy, FunctionSignature, HirFunctionId, HirType, IntTy};

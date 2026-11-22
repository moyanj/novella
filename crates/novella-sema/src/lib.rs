//! novella 静态语义分析。
//!
//! 当前范围与验收见 `docs/sema-coverage.md`：完整导入系统（跨文件加载、路径别名
//! `@` / `std` / `super`、nov.toml crate 根标记、循环导入安全）、类型别名、
//! 全程序名称解析、基本类型检查、声明级基础检查与 extern 函数规则。
//!
//! 分阶段管线（含字段/顶层依赖定型；各阶段为内部实现，见 `analysis` / `resolve` /
//! `check` 模块）：
//! - Phase 0 `module::load_modules`：worklist 加载全部可达模块（循环导入安全）；
//! - Phase 1 `resolve::collect_module`：逐模块收集顶层声明到全局作用域；
//! - Phase 2a `resolve::bind_imports`：绑定导入符号（具名 / 命名空间）；
//! - Phase 2b `check::build_type_defs`：解析字段 / 方法签名建类型注册表；
//! - Phase 2c `check::resolve_class_parents`：解析 class 继承链；
//! - Phase 2d `check::check_module`：声明级检查 + 函数体 / 顶层语句类型检查。
//!
//! 公开面只有三个输入 / 输出类型族：
//! - 入口：[`analyze`] / [`analyze_with`] / [`analyze_with_target`]，加载器 [`FileLoader`] / [`FsLoader`] /
//!   [`OverlayLoader`]；
//! - 结果：[`SemaResult`]，由 [`DiagnosticReport`] / [`BindingTable`] /
//!   [`TypeHierarchy`] / [`ModuleOutput`] 四个只读视图组成；
//! - 视图内的值类型：[`Definition`] / [`DefinitionKind`] / [`TypeSummary`] /
//!   [`TypeSummaryKind`] / [`ModuleDiagnostics`] / [`AnalyzedModule`]。
//!
//! **词法 / 语法只在 `analyze` 内执行一次**：`SemaResult.modules` 给出每个模块
//! 的完整产物（源码、token 流、AST；诊断统一在 DiagnosticReport），下游（CLI / codegen）直接
//! 消费，不得自行再跑 lexer / parser。
//!
//! 其余（符号表、作用域树、类型注册表、检查函数）一律 `pub(crate)`。

mod analysis;
mod api;
mod cfg;
mod declarations;
pub use declarations::{DeclarationId, DeclarationIndex};
mod check;
mod dependencies;
mod freestanding;
mod module;
mod resolve;
mod state;
mod symbol;
#[cfg(test)]
mod tests;
mod ty;
mod typecheck;

pub use analysis::{
    AnalysisOptions, AnalysisTarget, PointerWidth, analyze, analyze_with, analyze_with_options,
    analyze_with_target,
};
pub use api::*;
pub use cfg::CfgOptions;
pub use module::{FileLoader, FsLoader, ModuleId, OverlayLoader, ParsedSource};
pub use novella_core::path::module_key;
pub(crate) use state::Sema;

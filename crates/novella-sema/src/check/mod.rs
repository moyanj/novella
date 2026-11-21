//! 声明级检查与类型注册表构建（对应 `docs/sema-phase1.md` §6–§6.1），按职责分文件：
//!
//! - `type_defs`：Phase 2b 类型注册表（字段 / 方法 / 函数签名）与 Phase 2c class 继承链；
//! - `decl`：Phase 2d 声明级检查（main 入口、struct / class 命名规则、extern 规则 E2–E5）
//!   并驱动函数体 / 顶层语句的类型检查（[`crate::typecheck`]）。
//!
//! 三个 Phase 在 [`crate::analyze_with`] 中按序调用，本模块只做组织与再导出。

mod decl;
pub(crate) mod fields;
pub(crate) mod globals;
mod type_defs;

pub(crate) use decl::check_module;
pub(crate) use type_defs::{build_type_defs, is_abstract_class, resolve_class_parents};

/// class 特殊方法清单（design.md §4.6.1：以 `__` 开头的特殊方法由 sema 登记）。
const CLASS_SPECIAL_METHODS: &[&str] = &["__init", "__destroy", "__str", "__enter", "__exit"];

/// 特殊方法允许可选结尾 `__`，注册与协议查询共用同一语义名字。
pub(crate) fn canonical_method_name(name: &str) -> &str {
    match name.strip_suffix("__") {
        Some(short) if CLASS_SPECIAL_METHODS.contains(&short) => short,
        _ => name,
    }
}

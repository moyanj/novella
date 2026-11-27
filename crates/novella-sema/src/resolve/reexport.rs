//! 再导出链解析：`pub from "m" import a;` 让下游模块也能看到 `a`。
//!
//! 语言允许重导入（等价 Rust 的 `pub use`），因此「某模块的全局作用域里有
//! 某个名字」与「这个名字对外导出到哪」并不相同：导出的名字可能又是另一层
//! 导入。所有跨模块查找都必须经过本模块，否则只能看到一跳、看不到链。

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::{NsTarget, Symbol, SymbolKind};

/// 再导出链解析失败的原因。
#[derive(Debug)]
pub(crate) enum ReexportError {
    /// 链上某一层没有该名字
    Missing { module_path: String, name: String },
    /// 链上某一层不是 `pub`，对外不可见
    NotPublic { name: String },
    /// 再导出链成环（`a` 重导出 `b`，`b` 又重导出 `a`）
    Cycle { name: String },
}

/// 链的终点种类（值的类型分派由调用方完成）。
pub(crate) enum ExportTarget {
    /// 真实定义符号（函数 / 类型 / 变量）。
    /// `Box` 只为压平枚举尺寸（`Symbol` 远大于其他变体）。
    Symbol(Box<Symbol>),
    /// 命名空间导入绑定（`ns.member` 使用）
    Namespace(NsTarget),
    /// `std:exception` 的编译器静态名义根
    ExceptionRoot,
}

/// 沿再导出链寻找 `module` 中名字 `name` 的终极目标。
///
/// 可见性检查覆盖**除第一层外的每一跳**：`module` 自身的可见性由调用方语境
/// 决定（本模块内部引用私有导入合法，从外部导入才要求 `pub`），但链上后续
/// 每一层都必须 `pub`，否则整条链对外不可见。
///
/// 成环时返回 [`ReexportError::Cycle`]，不 panic、不挂起。
pub(crate) fn resolve_exported(
    sema: &Sema,
    module: ModuleId,
    name: &str,
) -> Result<ExportTarget, ReexportError> {
    let mut current = sema
        .global_scope_symbol(module, name)
        .ok_or_else(|| missing(sema, module, name))?;

    // 已访问的 `(模块, 名字)`：出现重复即说明链成环
    let mut visited = std::collections::HashSet::new();
    visited.insert((module, name.to_owned()));

    loop {
        let next = match &current.kind {
            SymbolKind::NamedImport {
                target: Some(target),
                imported,
            } => (*target, imported.clone()),
            // Exception 内建根：链到这里结束
            SymbolKind::NamedImport { target: None, .. } => return Ok(ExportTarget::ExceptionRoot),
            SymbolKind::NamespaceImport { target } => {
                return Ok(ExportTarget::Namespace(*target));
            }
            // 真实定义与不可导入的局部变量都由调用方判定用途
            SymbolKind::Function { .. }
            | SymbolKind::Type
            | SymbolKind::Variable { .. }
            | SymbolKind::Param { .. } => return Ok(ExportTarget::Symbol(Box::new(current))),
        };
        let (target_module, imported) = next;
        if !visited.insert((target_module, imported.clone())) {
            return Err(ReexportError::Cycle { name: imported });
        }
        // 读失败的模块不再追索：使用点按 Error 类型静默传播
        if sema.modules[target_module].load_failed {
            return Ok(ExportTarget::Symbol(Box::new(current)));
        }
        let Some(next_symbol) = sema.global_scope_symbol(target_module, &imported) else {
            return Err(missing(sema, target_module, &imported));
        };
        if !next_symbol.is_importable() {
            return Err(ReexportError::NotPublic { name: imported });
        }
        current = next_symbol;
    }
}

impl ReexportError {
    /// 诊断文案（定位点由调用方给出）。
    pub(crate) fn message(&self) -> String {
        match self {
            ReexportError::Missing { module_path, name } => {
                format!("模块 '{module_path}' 中不存在符号 `{name}`")
            }
            ReexportError::NotPublic { name } => {
                format!("符号 `{name}` 不是 `pub`，无法从该模块导入")
            }
            ReexportError::Cycle { name } => {
                format!("导入的再导出链存在循环（涉及符号 `{name}`）")
            }
        }
    }
}

fn missing(sema: &Sema, module: ModuleId, name: &str) -> ReexportError {
    ReexportError::Missing {
        module_path: module_path(sema, module),
        name: name.to_owned(),
    }
}

fn module_path(sema: &Sema, module: ModuleId) -> String {
    sema.modules
        .get(module)
        .map(|m| m.path.display().to_string())
        .unwrap_or_else(|| format!("<模块 {module}>"))
}

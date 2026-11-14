//! novella 语义分析（sema 第一期）。
//!
//! 实施范围见 `docs/sema-phase1.md`：完整导入系统（跨文件加载、路径别名
//! `@` / `std` / `super`、nov.toml crate 根标记、循环导入安全）、类型别名、
//! 全程序名称解析、基本类型检查、声明级基础检查与 extern 函数规则。
//!
//! 管线（三遍，对应计划 §1.2）：
//! - Phase 0 [`module::load_modules`]：worklist 加载全部可达模块（循环导入安全）；
//! - Phase 1 [`resolve::collect_module`]：逐模块收集顶层声明到全局作用域；
//! - Phase 2a [`resolve::bind_imports`]：绑定导入符号（具名 / 命名空间）；
//! - Phase 2b [`check::build_type_defs`]：解析字段 / 方法签名建类型注册表；
//! - Phase 2c [`check::resolve_class_parents`]：解析 class 继承链；
//! - Phase 2d [`check::check_module`]：声明级检查 + 函数体 / 顶层语句类型检查。

mod check;
mod module;
mod resolve;
mod symbol;
mod ty;
mod typecheck;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use novella_core::diagnostic::{Diagnostic, Severity};

pub use module::{FileLoader, FsLoader, ModuleId};

use module::Module;
use symbol::ScopeTree;
use ty::TypeDef;

/// 单个模块的诊断集合。
pub struct ModuleDiagnostics {
    pub path: PathBuf,
    /// 全部诊断（词法 / 语法 / 语义，按产生顺序）
    pub diagnostics: Vec<Diagnostic>,
    /// 前缀中词法 / 语法诊断的条数；`diagnostics[此值..]` 即纯语义诊断
    pub syntax_diagnostic_count: usize,
}

impl ModuleDiagnostics {
    /// 语义阶段产生的诊断切片（不含词法 / 语法诊断）。
    pub fn semantic_diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics[self.syntax_diagnostic_count..]
    }
}

/// 语义分析结果。
pub struct SemaResult {
    /// 已加载模块的诊断（含词法 / 语法 / 语义，入口为第一个）
    pub modules: Vec<ModuleDiagnostics>,
    /// 全局诊断（入口读取失败等无模块归属的错误）
    pub diagnostics: Vec<Diagnostic>,
}

impl SemaResult {
    /// 是否存在 error 级诊断。
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .chain(self.modules.iter().flat_map(|m| m.diagnostics.iter()))
            .any(|d| d.severity == Severity::Error)
    }
}

/// 以真实文件系统分析入口文件及其全部导入闭包。
pub fn analyze(entry: &Path) -> SemaResult {
    analyze_with(&mut FsLoader, entry)
}

/// 以指定文件加载器分析（测试可注入内存文件系统）。
pub fn analyze_with(loader: &mut dyn FileLoader, entry: &Path) -> SemaResult {
    let mut sema = Sema::new();
    // Phase 0：模块加载（含词法 / 语法分析）
    let (modules, global_diagnostics, module_by_path) =
        module::load_modules(loader, entry, &mut sema.scopes);
    sema.modules = modules;
    sema.global_diagnostics = global_diagnostics;
    sema.module_by_path = module_by_path;

    // Phase 1：全模块顶层声明收集（先于导入绑定，保证跨模块引用可见）
    for id in 0..sema.modules.len() {
        resolve::collect_module(&mut sema, id);
    }
    // Phase 2a：绑定导入符号
    for id in 0..sema.modules.len() {
        resolve::bind_imports(&mut sema, id);
    }
    // Phase 2b：构建类型注册表（字段 / 方法签名）
    for id in 0..sema.modules.len() {
        check::build_type_defs(&mut sema, id);
    }
    // Phase 2c：解析 class 继承链
    check::resolve_class_parents(&mut sema);
    // Phase 2d：声明级检查 + 函数体 / 顶层语句类型检查
    for id in 0..sema.modules.len() {
        check::check_module(&mut sema, id);
    }

    SemaResult {
        modules: sema
            .modules
            .into_iter()
            .map(|m| ModuleDiagnostics {
                path: m.path,
                diagnostics: m.diagnostics,
                syntax_diagnostic_count: m.syntax_diagnostic_count,
            })
            .collect(),
        diagnostics: sema.global_diagnostics,
    }
}

/// 语义分析共享状态。
pub(crate) struct Sema {
    pub modules: Vec<Module>,
    pub scopes: ScopeTree,
    /// 类型定义注册表：`(模块, 类型名) -> 定义实体`
    pub type_defs: HashMap<(ModuleId, String), TypeDef>,
    /// 类型别名展开中的集合（循环检测）
    pub resolving_aliases: HashSet<(ModuleId, String)>,
    /// 规范化路径 → 模块 id（导入绑定阶段解析目标模块）
    pub module_by_path: HashMap<PathBuf, ModuleId>,
    pub global_diagnostics: Vec<Diagnostic>,
}

impl Sema {
    fn new() -> Self {
        Sema {
            modules: Vec::new(),
            scopes: ScopeTree::default(),
            type_defs: HashMap::new(),
            resolving_aliases: HashSet::new(),
            module_by_path: HashMap::new(),
            global_diagnostics: Vec::new(),
        }
    }

    /// 在指定模块的 span 处追加 error 诊断。
    pub fn error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.modules[module].error(span, message);
    }

    /// 在指定模块的 span 处追加 warning 诊断。
    pub fn warning(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.modules[module].warning(span, message);
    }

    /// 取目标模块的全局符号（克隆返回，便于跨模块借用）。
    pub(crate) fn global_scope_symbol(
        &self,
        module: ModuleId,
        name: &str,
    ) -> Option<crate::symbol::Symbol> {
        self.modules
            .get(module)?
            .global_scope_symbol(&self.scopes, name)
            .cloned()
    }

    /// class 继承链上的 `is_subclass(ancestor, derived)` 判定（含自身）。
    pub fn is_subclass(&self, ancestor: &ty::Ty, derived: &ty::Ty) -> bool {
        let (ty::Ty::Class(am, an), ty::Ty::Class(derived_module, derived_name)) =
            (ancestor, derived)
        else {
            return false;
        };
        let (mut module, mut name) = (*derived_module, derived_name.clone());
        if *am == module && *an == name {
            return true;
        }
        let mut visited = HashSet::new();
        loop {
            if !visited.insert((module, name.clone())) {
                return false; // 继承环（已在 resolve_class_parents 报过）
            }
            match self.type_defs.get(&(module, name.clone())) {
                Some(TypeDef::Class(def)) => match &def.parent_resolved {
                    Some((pm, pn)) => {
                        module = *pm;
                        name = pn.clone();
                        if *am == module && *an == name {
                            return true;
                        }
                    }
                    None => return false,
                },
                _ => return false,
            }
        }
    }

    /// `target ← source` 可赋值性（注入继承链判定）。
    pub fn is_assignable(&self, target: &ty::Ty, source: &ty::Ty) -> bool {
        let mut is_subclass = |a: &ty::Ty, b: &ty::Ty| self.is_subclass(a, b);
        ty::assignable(target, source, &mut is_subclass)
    }
}

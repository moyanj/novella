//! 符号表与作用域树。
//!
//! 作用域按树组织（模块全局作用域为根，块 / 函数体 / 类体逐层下挂），
//! 名称查找沿父链逐级向上。类型符号的实体（字段表、方法表等）集中存放在
//! [`crate::Sema::type_defs`] 注册表，符号表中只存引用键。

use std::collections::HashMap;

use novella_ast::program::Scope as Visibility;
use novella_core::Span;

use crate::module::ModuleId;
use crate::ty::{FunctionSig, Ty};

/// 作用域标识（[`ScopeTree`] 内下标）。
pub(crate) type ScopeId = usize;

/// 作用域种类（仅作调试与判定用途）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// 模块全局作用域
    Module,
    /// 语句块
    Block,
    /// 函数 / 方法 / lambda 体
    Function,
}

/// 命名空间导入的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NsTarget {
    /// 已解析到具体模块
    Module(ModuleId),
    /// std:exception 的编译器静态名义根；不代表任意 std 成员
    ExceptionBuiltin,
}

/// 符号种类。
#[derive(Debug, Clone)]
pub(crate) enum SymbolKind {
    /// 顶层函数（含 extern；以 `sig.is_extern` 区分）
    Function { sig: FunctionSig },
    /// 类型符号（struct / class / trait / 类型别名），实体见
    /// [`crate::Sema::type_defs`]，键为 `(module, name)`
    Type,
    /// 变量（`let` / `const`，含顶层与局部）；`ty` 在类型检查后回填
    Variable { is_const: bool, ty: Option<Ty> },
    /// 函数 / 方法 / lambda 形参
    Param { ty: Ty },
    /// 命名空间导入绑定（`import "mod";`）
    NamespaceImport { target: NsTarget },
    /// 具名导入绑定（`from "mod" import a as b;`）：
    /// `target` 为 None 只用于编译器 Exception 根，值访问和未知成员仍报错
    NamedImport {
        target: Option<ModuleId>,
        imported: String,
    },
}

/// 一个符号表条目。
#[derive(Debug, Clone)]
pub(crate) struct Symbol {
    pub(crate) name: String,
    pub(crate) kind: SymbolKind,
    /// 声明处可见性（`pub` / `pri` / `pkg`），None 为缺省（模块私有）
    pub visibility: Option<Visibility>,
    /// 声明位置（诊断用）：函数 / 类型为整条声明的区间
    pub(crate) span: Span,
    /// 名字标识符的区间（跳转 / 高亮 / 重命名用）。
    ///
    /// 与 `span` 分离的原因：声明区间覆盖整个函数体，编辑器拿它做跳转目标会
    /// 选中 `() -> int { ... }` 而非名字；`name_span` 只覆盖标识符本身。
    pub(crate) name_span: Span,
    /// 所属模块
    pub(crate) module: ModuleId,
}

impl Symbol {
    /// 是否可被其他模块导入（仅显式 `pub`）
    pub(crate) fn is_importable(&self) -> bool {
        self.visibility == Some(Visibility::Public)
    }
}

/// 单个作用域。
#[derive(Debug)]
pub(crate) struct Scope {
    pub(crate) parent: Option<ScopeId>,
    /// 作用域类别（本期仅调试与后续期的上下文检查用，暂不参与查找逻辑）
    #[allow(dead_code)]
    pub(crate) kind: ScopeKind,
    pub symbols: HashMap<String, Symbol>,
}

/// 作用域树（全模块共用一个 arena，模块间以 `NsTarget::Module` 互查）。
#[derive(Debug, Default)]
pub(crate) struct ScopeTree {
    pub(crate) scopes: Vec<Scope>,
}

impl ScopeTree {
    /// 新建作用域，返回其标识。
    pub(crate) fn new_scope(&mut self, parent: Option<ScopeId>, kind: ScopeKind) -> ScopeId {
        self.scopes.push(Scope {
            parent,
            kind,
            symbols: HashMap::new(),
        });
        self.scopes.len() - 1
    }

    pub(crate) fn set_parent(&mut self, scope: ScopeId, parent: Option<ScopeId>) {
        self.scopes[scope].parent = parent;
    }

    /// 在指定作用域定义符号；重名时返回已存在的符号（供诊断引用）。
    pub(crate) fn define(&mut self, scope: ScopeId, symbol: Symbol) -> Result<(), Box<Symbol>> {
        let table = &mut self.scopes[scope].symbols;
        if let Some(existing) = table.get(&symbol.name) {
            return Err(Box::new(existing.clone()));
        }
        table.insert(symbol.name.clone(), symbol);
        Ok(())
    }

    /// 仅在当前作用域查找。
    pub(crate) fn lookup_local(&self, scope: ScopeId, name: &str) -> Option<&Symbol> {
        self.scopes[scope].symbols.get(name)
    }

    /// 沿父链逐级查找。
    pub(crate) fn lookup(&self, mut scope: ScopeId, name: &str) -> Option<&Symbol> {
        loop {
            let current = &self.scopes[scope];
            if let Some(symbol) = current.symbols.get(name) {
                return Some(symbol);
            }
            {
                let parent = current.parent?;
                scope = parent
            }
        }
    }

    /// 沿父链逐级查找（可变）。
    pub(crate) fn lookup_mut(&mut self, mut scope: ScopeId, name: &str) -> Option<&mut Symbol> {
        // 先用不可变遍历定位目标作用域，再单次可变索引，绕开循环内多次可变借用的限制
        loop {
            if self.scopes[scope].symbols.contains_key(name) {
                break;
            }
            {
                let parent = self.scopes[scope].parent?;
                scope = parent
            }
        }
        self.scopes[scope].symbols.get_mut(name)
    }
}

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
pub type ScopeId = usize;

/// 作用域种类（仅作调试与判定用途）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// 模块全局作用域
    Module,
    /// 语句块
    Block,
    /// 函数 / 方法 / lambda 体
    Function,
}

/// 命名空间导入的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsTarget {
    /// 已解析到具体模块
    Module(ModuleId),
    /// `std:` 前缀——std 库尚未落地，成员一律为不透明外部符号
    StdExternal,
}

/// 符号种类。
#[derive(Debug, Clone)]
pub enum SymbolKind {
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
    /// `target` 为 None 表示 std 外部符号（不透明，调用不查）
    NamedImport {
        target: Option<ModuleId>,
        imported: String,
    },
}

/// 一个符号表条目。
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// 声明处可见性（`pub` / `pri` / `pkg`），None 为缺省（模块私有）
    pub visibility: Option<Visibility>,
    /// 声明位置（诊断用）
    pub span: Span,
    /// 所属模块
    pub module: ModuleId,
}

impl Symbol {
    /// 是否可被其他模块导入（仅显式 `pub`）
    pub fn is_importable(&self) -> bool {
        self.visibility == Some(Visibility::Public)
    }
}

/// 单个作用域。
#[derive(Debug)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    /// 作用域类别（本期仅调试与后续期的上下文检查用，暂不参与查找逻辑）
    #[allow(dead_code)]
    pub kind: ScopeKind,
    pub symbols: HashMap<String, Symbol>,
}

/// 作用域树（全模块共用一个 arena，模块间以 `NsTarget::Module` 互查）。
#[derive(Debug, Default)]
pub struct ScopeTree {
    pub scopes: Vec<Scope>,
}

impl ScopeTree {
    /// 新建作用域，返回其标识。
    pub fn new_scope(&mut self, parent: Option<ScopeId>, kind: ScopeKind) -> ScopeId {
        self.scopes.push(Scope {
            parent,
            kind,
            symbols: HashMap::new(),
        });
        self.scopes.len() - 1
    }

    /// 在指定作用域定义符号；重名时返回已存在的符号（供诊断引用）。
    pub fn define(&mut self, scope: ScopeId, symbol: Symbol) -> Result<(), Box<Symbol>> {
        let table = &mut self.scopes[scope].symbols;
        if let Some(existing) = table.get(&symbol.name) {
            return Err(Box::new(existing.clone()));
        }
        table.insert(symbol.name.clone(), symbol);
        Ok(())
    }

    /// 仅在当前作用域查找。
    pub fn lookup_local(&self, scope: ScopeId, name: &str) -> Option<&Symbol> {
        self.scopes[scope].symbols.get(name)
    }

    /// 沿父链逐级查找。
    pub fn lookup(&self, mut scope: ScopeId, name: &str) -> Option<&Symbol> {
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
    pub fn lookup_mut(&mut self, mut scope: ScopeId, name: &str) -> Option<&mut Symbol> {
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

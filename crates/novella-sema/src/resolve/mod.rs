//! 名称解析：Phase 1 顶层声明收集、Phase 2a 导入绑定，以及
//! `TypeExpr -> Ty` 的类型解析（含类型别名展开与循环检测）。

use std::collections::HashMap;
use std::rc::Rc;

use novella_ast::program::{ImportKind, Item, Scope as Visibility};
use novella_ast::stmt::{BindingPattern, Statement};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::module::{ModuleId, NsResolution, resolve_import_path};
use crate::symbol::{NsTarget, ScopeId, Symbol, SymbolKind};
use crate::ty::{
    AliasDef, ClassDef, FunctionSig, ParamSig, StructDef, TraitDef, Ty, TypeDef,
    predefined_from_enum, predefined_from_name,
};

mod collect;
mod imports;
mod types;
pub(crate) use collect::*;
pub(crate) use imports::*;

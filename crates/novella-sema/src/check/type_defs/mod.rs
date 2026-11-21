//! Phase 2b 类型注册表与 Phase 2c class 继承链。
//!
//! - [`build_type_defs`]：解析字段 / 方法 / 函数签名，填充类型注册表；
//! - [`resolve_class_parents`]：解析 class 继承链并检测继承环。

use std::collections::{HashMap, HashSet};

use novella_ast::program::{ClassMember, FieldMember, FunctionItem, Item, MethodMember};
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::SymbolKind;
use crate::ty::{FieldDef, FunctionSig, MethodSig, Ty, TypeDef};
use crate::typecheck::member::{MemberInfo, lookup_member_chain};

use super::CLASS_SPECIAL_METHODS;

mod registration;
pub(crate) use registration::*;
mod inheritance;
pub(crate) use inheritance::*;

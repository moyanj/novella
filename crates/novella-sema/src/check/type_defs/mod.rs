//! Phase 2b 类型注册表与 Phase 2c class 继承链。
//!
//! - [`build_type_defs`]：解析字段 / 方法 / 函数签名，填充类型注册表；
//! - [`resolve_class_parents`]：注册 class 父类和 trait 关系并检测继承环；
//! - [`check_inherited_signatures`]：返回类型定型后校验覆写和 trait 实现。

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

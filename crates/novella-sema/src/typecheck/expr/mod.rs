//! 表达式检查：字面量值解析、标识符解析、数组 / 对象字面量、一元运算。

use std::collections::HashSet;
use std::rc::Rc;

use novella_ast::expr::{BinaryOp, Expression, Literal, UnaryOp};
use novella_ast::stmt::Statement;
use novella_core::Span;
use novella_parser::ParserInput;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::{NsTarget, ScopeId, Symbol, SymbolKind};
use crate::ty::{Ty, classify_number_literal};

use super::body::{BodyCtx, concrete, expect_boolean, unary_op_text};
use super::call::{check_call, check_index_access, check_lambda, check_new};
use super::member::check_member_access;
use super::unsafe_rules::{
    check_assignment, check_binary, check_binary_with_hint, check_cast, check_struct_init,
    is_lvalue, require_unsafe, unify_types,
};

// ---------- 表达式 ----------

mod dispatch;
pub(crate) use dispatch::*;

mod strings;
pub(crate) use strings::*;

mod names;
pub(crate) use names::*;

mod hints;
pub(crate) use hints::*;

mod aggregates;
pub(crate) use aggregates::*;

mod unary;
pub(crate) use unary::*;

//! 运算符与不变式检查：二元 / 算术矩阵、赋值兼容、cast、struct 初始化、
//! unsafe 上下文规则（S1–S10 子集 + extern 调用 E7）、左值判定。

use std::collections::HashSet;

use novella_ast::expr::{AssignOp, BinaryOp, Expression, UnaryOp};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::symbol::{Symbol, SymbolKind};
use crate::ty::{Ty, TypeDef, ty_eq};

use super::body::{BodyCtx, binary_op_text, concrete};
use super::expr::{
    IdentTarget, check_expr, check_expr_with_hint, classify_identifier, type_key_of_name,
};

mod operators;
pub(crate) use operators::*;

mod aggregate;
pub(crate) use aggregate::*;

mod assignment;
pub(crate) use assignment::*;

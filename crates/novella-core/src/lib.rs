use serde::{Deserialize, Serialize};

pub mod decor;
pub mod diagnostic;
pub mod keywords;
pub mod line_index;
pub mod path;
pub mod string;
pub mod token;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

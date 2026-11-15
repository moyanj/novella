//! 表达式解析（对应文法 `expression` 的全部产生式），按职责分文件：
//!
//! - `stairs`：入口 [`crate::Parser::parse_expression`] 与优先级阶梯（逐层折叠）；
//! - `postfix`：后缀与调用（成员 / 索引 / 调用 / 宏调用 / `new` / 后缀自增）；
//! - `primary`：原子表达式（字面量 / 标识符 / 括号与元组 / 数组与对象 / 块等）；
//! - `lambda`：lambda 表达式；
//! - `match`：match 表达式与错误恢复。
//!
//! 各子模块都是 `impl Parser` 的一块，方法之间互相调用（跨文件调用需 `pub(crate)`），
//! 文件划分只表达职责，不引入新的抽象。

mod helper;
mod lambda;
mod match_expr;
mod postfix;
mod primary;
mod stairs;

//! 装饰器（attribute）节点。

use serde::{Deserialize, Serialize};

use novella_core::token::Token;

/// 装饰器（对应文法 `decorator`）：`@name` 或 `@name(...)`
///
/// 语义等价于 Rust 的 attribute `#[name(...)]`：
/// - `name` 为装饰器名（如 `@derive(Clone)` 中的 `derive`）；
/// - 圆括号内为**原始 token 树**（任意 token，允许嵌套括号与各类字面量），
///   不在 AST 层做结构化解析，由语义分析阶段按 `name` 解释其含义
///   （如 `@cfg(target_os = "linux")`、`@derive(Clone, Debug)`）。
///
/// 出于「值语义延后」的项目约定，token 流原样保留（含字符串/进制前缀），
/// 嵌套括号以 `OpenParen`/`CloseParen` 等 token 平铺在序列中，需要时可
/// 按括号配对还原树形。`@name` 无括号的简写形式等价于空 token 流。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decorator {
    /// 装饰器名：如 `@derive(Clone)` 中的 `derive`
    pub name: String,
    /// 圆括号内的原始 token 序列（不含外层括号本身）；`@name` 简写形式为空
    pub tokens: Vec<Token>,
}

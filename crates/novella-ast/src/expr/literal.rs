//! 字面量节点（对应文法 `literal`）。
//!
//! 字符串与数字均保留源码原文（含 `f`/`r`/`b` 前缀与进制前缀），
//! 具体值语义在语义分析阶段解析。字面量是纯值不携带位置，
//! 其源码区间由外层 `Expression::Literal` 的 `span` 承载。

use serde::{Deserialize, Serialize};

/// 字面量（对应文法 `literal`）。
///
/// 字符串与数字均保留源码原文（含 `f`/`r`/`b` 前缀与进制前缀），
/// 具体值语义在语义分析阶段解析。字面量是纯值不携带位置，
/// 其源码区间由外层 `Expression::Literal` 的 `span` 承载。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Literal {
    /// `null`
    Null,
    /// `true` / `false`
    Boolean(bool),
    /// 字符串字面量（普通 / 格式化 / 原始 / 字节串，保留原文）
    String(String),
    /// 数字字面量（十进制 / 十六进制 / 八进制 / 二进制，保留原文，如 `0`、`1.5`、`0xFF`）
    Number(String),
}

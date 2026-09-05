/// 字面量（对应文法 `literal`）。
///
/// 字符串与数字均保留源码原文（含 `f`/`r`/`b` 前缀与进制前缀），
/// 具体值语义在语义分析阶段解析。
#[derive(Debug, Clone, PartialEq)]
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

/// 表达式（对应文法 `expression`）。
///
/// 目前仅落地了元组表达式与字面量两类节点，用于支撑变量声明的
/// 元组解构（`let (a, b) = (0, 1);`）；其余运算符、调用、访问等
/// 表达式节点待后续逐步补充。
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    /// 元组表达式：`(0, 1)`
    Tuple(Vec<Expression>),
    /// 字面量：`42`、`"hello"`、`true`、`null` 等
    Literal(Literal),
}

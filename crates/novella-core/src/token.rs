//! 词法单元种类定义。

use serde::{Deserialize, Serialize};

/// 词法单元的种类(不含位置信息)。
///
/// 这是文法「词法面」的纯数据投影,与 `novella-lexer` 内部由 logos 生成的
/// 私有 `RawToken` 枚举一一对应。lexer 负责把 logos 的产物转换成本类型,
/// parser 等下游阶段只依赖本类型,从而不被 logos 绑定。
///
/// 各变体的 doc 注释标注了它对应的源码字面量,便于转换层逐一对号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TokenKind {
    // ---------- 多字符运算符 ----------
    /// `++`
    PlusPlus,
    /// `--`
    MinusMinus,
    /// `**`
    Power,
    /// `**=`
    PowerAssign,
    /// `<<`
    LeftShiftArithmetic,
    /// `>>`
    RightShiftArithmetic,
    /// `<<=`
    LeftShiftArithmeticAssign,
    /// `>>=`
    RightShiftArithmeticAssign,
    /// `==`
    Equals,
    /// `!=`
    NotEquals,
    /// `===`
    IdentityEquals,
    /// `!==`
    IdentityNotEquals,
    /// `<=`
    LessThanEquals,
    /// `>=`
    GreaterThanEquals,
    /// `&&`
    And,
    /// `||`
    Or,
    /// `??`
    NullCoalesce,
    /// `=>`
    Arrow,
    /// `->`
    ReturnArrow,
    /// `*=`
    MultiplyAssign,
    /// `/=`
    DivideAssign,
    /// `%=`
    ModulusAssign,
    /// `+=`
    PlusAssign,
    /// `-=`
    MinusAssign,
    /// `&=`
    BitAndAssign,
    /// `^=`
    BitXorAssign,
    /// `|=`
    BitOrAssign,

    // ---------- 单字符运算符 / 分隔符 ----------
    /// `[`
    OpenBracket,
    /// `]`
    CloseBracket,
    /// `(`
    OpenParen,
    /// `)`
    CloseParen,
    /// `{`
    OpenBrace,
    /// `}`
    CloseBrace,
    /// `;`
    SemiColon,
    /// `,`
    Comma,
    /// `=`
    Assign,
    /// `?`
    QuestionMark,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `~`
    BitNot,
    /// `!`
    Not,
    /// `*`
    Multiply,
    /// `/`
    Divide,
    /// `%`
    Modulus,
    /// `<`
    LessThan,
    /// `>`
    MoreThan,
    /// `&`
    BitAnd,
    /// `^`
    BitXOr,
    /// `|`
    BitOr,
    /// `@`
    At,

    // ---------- 关键字 ----------
    /// `from`
    From,
    /// `import`
    Import,
    /// `pub`
    Pub,
    /// `pri`
    Pri,
    /// `const`
    Const,
    /// `let`
    Let,
    /// `var`
    Var,
    /// `static`
    Static,
    /// `func`
    Func,
    /// `class`
    Class,
    /// `interface`
    Interface,
    /// `new`
    New,
    /// `this`
    This,
    /// `if`
    If,
    /// `else`
    Else,
    /// `elif`
    Elif,
    /// `while`
    While,
    /// `for`
    For,
    /// `in`
    In,
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// `return`
    Return,
    /// `default`
    Default,
    /// `match`
    Match,
    /// `case`
    Case,
    /// `and`
    KeyAnd,
    /// `or`
    KeyOr,
    /// `not`
    KeyNot,
    /// `try`
    Try,
    /// `catch`
    Catch,
    /// `finally`
    Finally,
    /// `throw`
    Throw,
    /// `with`
    With,
    /// `as`
    As,
    /// `type`
    Type,
    /// `is`
    Is,
    /// `async`
    Async,
    /// `await`
    Await,
    /// `stream`
    Stream,
    /// `yield`
    Yield,
    /// `macro`
    Macro,
    /// `extern`
    Extern,

    // ---------- 字面量 ----------
    /// `null`
    Null,
    /// `true` / `false`
    BooleanLiteral,
    /// 十六进制整数字面量,如 `0xFF`
    HexIntegerLiteral,
    /// 八进制整数字面量,如 `0o17`
    OctalIntegerLiteral,
    /// 二进制整数字面量,如 `0b1010`
    BinaryIntegerLiteral,
    /// 十进制字面量(整数 / 小数 / 指数),如 `42`、`1.5`、`.5`、`1e10`
    DecimalLiteral,
    /// 普通字符串字面量(双引号 / 单引号)
    StringLiteral,
    /// 格式化字符串字面量,如 `f"..."`
    FormatStringLiteral,
    /// 原始字符串字面量,如 `r"..."`
    RawStringLiteral,
    /// 字节字符串字面量,如 `b"..."`
    ByteStringLiteral,

    // ---------- 标识符 ----------
    /// 标识符,如 `foo`、`_private`、`中文名`
    Identifier,

    // EOF
    Eof,
    // 错误
    Error,
}

impl std::fmt::Display for TokenKind {
    /// 输出变体名(与 `Debug` 一致),用于诊断与错误报告;
    /// 变体若携带数据,`Debug` 会自动附带输出。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub kind: TokenKind, // 词法单元的种类
    pub span: Span,      // 词法单元的起始位置和结束位置
    pub slice: String,   // 词法单元的字符串切片
    pub line: usize,     // 词法单元的行号
    pub column: usize,   // 词法单元的列号
}

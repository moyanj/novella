use logos::{Lexer, Logos};
use novella_core::{
    Span,
    diagnostic::{Diagnostic, DiagnosticLocation, Severity},
    token::{Token, TokenKind},
};
use std::fmt;

#[derive(Logos, Debug, PartialEq, Clone, Copy)]
#[logos(skip r#"[ \t\x0B\x0C\xA0]+"#)]
#[logos(skip r#"[\r\n\u2028\u2029]"#)]
pub enum RawToken {
    // ---------- 多字符运算符（优先于单字符） ----------
    #[token("++")]
    PlusPlus,
    #[token("--")]
    MinusMinus,
    #[token("**")]
    Power,
    #[token("**=")]
    PowerAssign,
    #[token("<<")]
    LeftShiftArithmetic,
    #[token(">>")]
    RightShiftArithmetic,
    #[token("<<=")]
    LeftShiftArithmeticAssign,
    #[token(">>=")]
    RightShiftArithmeticAssign,
    #[token("==")]
    Equals,
    #[token("!=")]
    NotEquals,
    #[token("===")]
    IdentityEquals,
    #[token("!==")]
    IdentityNotEquals,
    #[token("<=")]
    LessThanEquals,
    #[token(">=")]
    GreaterThanEquals,
    #[token("&&")]
    And,
    #[token("||")]
    Or,
    #[token("??")]
    NullCoalesce,
    #[token("=>")]
    Arrow,
    #[token("->")]
    ReturnArrow,
    #[token("*=")]
    MultiplyAssign,
    #[token("/=")]
    DivideAssign,
    #[token("%=")]
    ModulusAssign,
    #[token("+=")]
    PlusAssign,
    #[token("-=")]
    MinusAssign,
    #[token("&=")]
    BitAndAssign,
    #[token("^=")]
    BitXorAssign,
    #[token("|=")]
    BitOrAssign,

    // ---------- 单字符运算符 / 分隔符 ----------
    #[token("[")]
    OpenBracket,
    #[token("]")]
    CloseBracket,
    #[token("(")]
    OpenParen,
    #[token(")")]
    CloseParen,
    #[token("{")]
    OpenBrace,
    #[token("}")]
    CloseBrace,
    #[token(";")]
    SemiColon,
    #[token(",")]
    Comma,
    #[token("=")]
    Assign,
    #[token("?")]
    QuestionMark,
    #[token(":")]
    Colon,
    #[token(".")]
    Dot,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("~")]
    BitNot,
    #[token("!")]
    Not,
    #[token("*")]
    Multiply,
    #[token("/")]
    Divide,
    #[token("%")]
    Modulus,
    #[token("<")]
    LessThan,
    #[token(">")]
    MoreThan,
    #[token("&")]
    BitAnd,
    #[token("^")]
    BitXOr,
    #[token("|")]
    BitOr,
    #[token("@")]
    At,

    // ---------- 关键字 ----------
    #[token("from")]
    From,
    #[token("import")]
    Import,
    #[token("pub")]
    Pub,
    #[token("pri")]
    Pri,
    #[token("const")]
    Const,
    #[token("let")]
    Let,
    #[token("static")]
    Static,
    #[token("func")]
    Func,
    #[token("class")]
    Class,
    #[token("trait")]
    Trait,
    #[token("new")]
    New,
    #[token("this")]
    This,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("elif")]
    Elif,
    #[token("while")]
    While,
    #[token("for")]
    For,
    #[token("in")]
    In,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("return")]
    Return,
    #[token("default")]
    Default,
    #[token("match")]
    Match,
    #[token("case")]
    Case,
    #[token("and")]
    KeyAnd,
    #[token("or")]
    KeyOr,
    #[token("not")]
    KeyNot,
    #[token("try")]
    Try,
    #[token("catch")]
    Catch,
    #[token("finally")]
    Finally,
    #[token("throw")]
    Throw,
    #[token("with")]
    With,
    #[token("as")]
    As,
    #[token("type")]
    Type,
    #[token("is")]
    Is,
    #[token("async")]
    Async,
    #[token("await")]
    Await,
    #[token("stream")]
    Stream,
    #[token("yield")]
    Yield,
    #[token("macro")]
    Macro,
    #[token("extern")]
    Extern,

    #[token("null")]
    Null,
    #[token("true")]
    #[token("false")]
    BooleanLiteral,

    // 数字字面量（不同进制）
    #[regex(r#"0[xX][0-9a-fA-F][0-9a-fA-F_]*"#)]
    HexIntegerLiteral,
    #[regex(r#"0[oO][0-7][0-7_]*"#)]
    OctalIntegerLiteral,
    #[regex(r#"0[bB][01][01_]*"#)]
    BinaryIntegerLiteral,
    // 十进制（整数 / 小数 / 指数）
    #[regex(r#"[0-9][0-9_]*(?:\.[0-9][0-9_]*)?(?:[eE][+-]?[0-9][0-9_]*)?|\.[0-9][0-9_]*(?:[eE][+-]?[0-9][0-9_]*)?"#)]
    DecimalLiteral,

    // ---------- 字符串 ----------
    // 普通字符串（双引号 / 单引号）
    #[regex(r#""([^"\\\r\n]|\\.)*""#)]
    #[regex(r#"'([^'\\\r\n]|\\.)*'"#)]
    StringLiteral,

    // 格式化字符串（f / F 前缀）
    #[regex(r#"[fF]"([^"\\\r\n]|\\.)*""#)]
    #[regex(r#"[fF]'([^'\\\r\n]|\\.)*'"#)]
    FormatStringLiteral,

    // 原始字符串（r / R 前缀）
    #[regex(r#"[rR]"([^"\\\r\n]|\\.)*""#)]
    #[regex(r#"[rR]'([^'\\\r\n]|\\.)*'"#)]
    RawStringLiteral,

    // 字节字符串（b / B 前缀）
    #[regex(r#"[bB]"([^"\\\r\n]|\\.)*""#)]
    #[regex(r#"[bB]'([^'\\\r\n]|\\.)*'"#)]
    ByteStringLiteral,

    // ---------- 标识符 ----------
    #[regex(r#"[_\p{L}$][_\p{L}\d$]*"#)]
    Identifier,

    // ---------- 注释 ----------
    // 行注释：`//` 到行尾（不含换行符本身，换行仍由 skip 规则吞掉）
    #[regex(r#"//[^\r\n\u2028\u2029]*"#, allow_greedy = true)]
    LineComment,
}

/// 把 logos 生成的 [`Token`] 转换为下游共享的 [`TokenKind`]。
///
/// 转换是纯种类映射(不携带位置与原文),104 个变体一一对应;
/// 由 match 的穷尽性保证 logos 侧新增变体时此处编译报错、不会漏转。
impl From<RawToken> for TokenKind {
    fn from(token: RawToken) -> TokenKind {
        use TokenKind::*;
        match token {
            // ---------- 多字符运算符 ----------
            RawToken::PlusPlus => PlusPlus,
            RawToken::MinusMinus => MinusMinus,
            RawToken::Power => Power,
            RawToken::PowerAssign => PowerAssign,
            RawToken::LeftShiftArithmetic => LeftShiftArithmetic,
            RawToken::RightShiftArithmetic => RightShiftArithmetic,
            RawToken::LeftShiftArithmeticAssign => LeftShiftArithmeticAssign,
            RawToken::RightShiftArithmeticAssign => RightShiftArithmeticAssign,
            RawToken::Equals => Equals,
            RawToken::NotEquals => NotEquals,
            RawToken::IdentityEquals => IdentityEquals,
            RawToken::IdentityNotEquals => IdentityNotEquals,
            RawToken::LessThanEquals => LessThanEquals,
            RawToken::GreaterThanEquals => GreaterThanEquals,
            RawToken::And => And,
            RawToken::Or => Or,
            RawToken::NullCoalesce => NullCoalesce,
            RawToken::Arrow => Arrow,
            RawToken::ReturnArrow => ReturnArrow,
            RawToken::MultiplyAssign => MultiplyAssign,
            RawToken::DivideAssign => DivideAssign,
            RawToken::ModulusAssign => ModulusAssign,
            RawToken::PlusAssign => PlusAssign,
            RawToken::MinusAssign => MinusAssign,
            RawToken::BitAndAssign => BitAndAssign,
            RawToken::BitXorAssign => BitXorAssign,
            RawToken::BitOrAssign => BitOrAssign,

            // ---------- 单字符运算符 / 分隔符 ----------
            RawToken::OpenBracket => OpenBracket,
            RawToken::CloseBracket => CloseBracket,
            RawToken::OpenParen => OpenParen,
            RawToken::CloseParen => CloseParen,
            RawToken::OpenBrace => OpenBrace,
            RawToken::CloseBrace => CloseBrace,
            RawToken::SemiColon => SemiColon,
            RawToken::Comma => Comma,
            RawToken::Assign => Assign,
            RawToken::QuestionMark => QuestionMark,
            RawToken::Colon => Colon,
            RawToken::Dot => Dot,
            RawToken::Plus => Plus,
            RawToken::Minus => Minus,
            RawToken::BitNot => BitNot,
            RawToken::Not => Not,
            RawToken::Multiply => Multiply,
            RawToken::Divide => Divide,
            RawToken::Modulus => Modulus,
            RawToken::LessThan => LessThan,
            RawToken::MoreThan => MoreThan,
            RawToken::BitAnd => BitAnd,
            RawToken::BitXOr => BitXOr,
            RawToken::BitOr => BitOr,
            RawToken::At => At,

            // ---------- 关键字 ----------
            RawToken::From => From,
            RawToken::Import => Import,
            RawToken::Pub => Pub,
            RawToken::Pri => Pri,
            RawToken::Const => Const,
            RawToken::Let => Let,
            RawToken::Static => Static,
            RawToken::Func => Func,
            RawToken::Class => Class,
            RawToken::Trait => Trait,
            RawToken::New => New,
            RawToken::This => This,
            RawToken::If => If,
            RawToken::Else => Else,
            RawToken::Elif => Elif,
            RawToken::While => While,
            RawToken::For => For,
            RawToken::In => In,
            RawToken::Break => Break,
            RawToken::Continue => Continue,
            RawToken::Return => Return,
            RawToken::Default => Default,
            RawToken::Match => Match,
            RawToken::Case => Case,
            RawToken::KeyAnd => KeyAnd,
            RawToken::KeyOr => KeyOr,
            RawToken::KeyNot => KeyNot,
            RawToken::Try => Try,
            RawToken::Catch => Catch,
            RawToken::Finally => Finally,
            RawToken::Throw => Throw,
            RawToken::With => With,
            RawToken::As => As,
            RawToken::Type => Type,
            RawToken::Is => Is,
            RawToken::Async => Async,
            RawToken::Await => Await,
            RawToken::Stream => Stream,
            RawToken::Yield => Yield,
            RawToken::Macro => Macro,
            RawToken::Extern => Extern,

            // ---------- 字面量 ----------
            RawToken::Null => Null,
            RawToken::BooleanLiteral => BooleanLiteral,
            RawToken::HexIntegerLiteral => HexIntegerLiteral,
            RawToken::OctalIntegerLiteral => OctalIntegerLiteral,
            RawToken::BinaryIntegerLiteral => BinaryIntegerLiteral,
            RawToken::DecimalLiteral => DecimalLiteral,
            RawToken::StringLiteral => StringLiteral,
            RawToken::FormatStringLiteral => FormatStringLiteral,
            RawToken::RawStringLiteral => RawStringLiteral,
            RawToken::ByteStringLiteral => ByteStringLiteral,

            // ---------- 标识符 ----------
            RawToken::Identifier => Identifier,

            // ---------- 注释 ----------
            RawToken::LineComment => LineComment,
        }
    }
}

/// 词法错误种类（结构化，消息由 [`Display`] 生成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexError {
    /// 非法字符（无法匹配任何词法规则）。
    InvalidCharacter { ch: char },
    /// 未闭合的字符串字面量：以 `quote` 开头，缺少闭合引号。
    UnterminatedString { quote: char },
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexError::InvalidCharacter { ch } => {
                write!(f, "非法字符 '{ch}' (U+{:04X})", u32::from(*ch))
            }
            LexError::UnterminatedString { quote } => {
                write!(f, "未闭合的字符串字面量：以 '{quote}' 开头，缺少闭合引号")
            }
        }
    }
}

#[derive(Debug)]
pub struct TokenError {
    pub token: Token,
    pub error: LexError,
}

impl TokenError {
    /// 转换为通用诊断，供 CLI 统一渲染。
    ///
    /// 列号沿用 token 的 0-based 字节偏移（与 ANTLR `getCharPositionInLine()` 对齐），
    /// 渲染层自行转字符列与 1-based 展示。
    pub fn to_diagnostic(&self, file: Option<&str>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            message: self.error.to_string(),
            location: Some(DiagnosticLocation {
                file: file.map(str::to_owned),
                line: self.token.line,
                column: self.token.column,
                span: Some(self.token.span),
            }),
            note: None,
        }
    }
}

pub struct LexerResult {
    pub tokens: Vec<Token>,
    pub errors: Vec<TokenError>,
}

/// 一次性扫描建立的行索引，用于 O(log n) 定位 token 的行列。
///
/// 行号按 `\n` 划分（与 `str::lines()` 语义一致：`\r\n` 中 `\r` 归上一行），
/// 列号是行内字节偏移（0-based）。相比旧的 `input[..start].lines().count()`
/// 每个 token 全量重扫的 O(n²) 做法，这里只扫描一次输入。
struct LineIndex {
    /// 每行起始字节偏移（首行恒为 0），严格递增。
    starts: Vec<usize>,
}

impl LineIndex {
    /// 扫描输入建立行起点表。
    fn build(input: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in input.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }

    /// 定位偏移量所在的行列位置。
    ///
    /// 返回 `(line, column)`：`line` 为 1-based 行号，`column` 为行内字节偏移
    /// （0-based），语义对齐 ANTLR 的 `getLine()` / `getCharPositionInLine()`。
    #[inline]
    fn locate(&self, offset: usize) -> (usize, usize) {
        // partition_point 依赖 starts 严格递增；starts 恒非空（首行 0），不会下溢
        let idx = self.starts.partition_point(|&s| s <= offset) - 1;
        (idx + 1, offset - self.starts[idx])
    }
}

#[inline]
fn to_token(t: Option<RawToken>, lexer: &Lexer<'_, RawToken>, lines: &LineIndex) -> Token {
    let span = Span {
        start: lexer.span().start,
        end: lexer.span().end,
    };
    let (line, column) = lines.locate(span.start);
    let kind = match t {
        Some(t) => t.into(),
        None => TokenKind::Error,
    };
    Token {
        kind,
        span,
        slice: lexer.slice().to_string(),
        line,
        column,
    }
}

/// 词法分析入口：产出含 EOF 的 token 流与词法错误列表。
///
/// - 可恢复错误（非法字符、未闭合字符串）就近记录进 `errors`，不中断分析；
/// - 错误 token 只进 `errors`，不进 `tokens`（与以往行为一致）；
/// - EOF token 的 span 指向输入末尾，line/column 为末尾处的行列；
/// - 行号 1-based、列号 0-based（字节偏移）。
pub fn lexer(input: &str) -> LexerResult {
    let lines = LineIndex::build(input);
    let mut lexer = RawToken::lexer(input);
    let mut tokens = vec![];
    let mut errors = vec![];
    while let Some(t) = lexer.next() {
        match t {
            Ok(token) => {
                tokens.push(to_token(Some(token), &lexer, &lines));
            }
            Err(_) => {
                let slice = lexer.slice();
                // 引号开头说明是字符串正则因缺闭合引号而失败
                // （`"abc` 在引号处失败；`f"abc` 中前缀 f 先成 Identifier，同样在引号处失败）
                let error = match slice.chars().next() {
                    // slice 恒非空（logos 在无法匹配处至少前进一个字符），None 分支仅防御
                    Some('"' | '\'') => LexError::UnterminatedString {
                        quote: slice.chars().next().unwrap(),
                    },
                    Some(ch) => LexError::InvalidCharacter { ch },
                    None => LexError::InvalidCharacter { ch: '\0' },
                };
                errors.push(TokenError {
                    token: to_token(None, &lexer, &lines),
                    error,
                });
            }
        }
    }
    let eof_offset = input.len();
    let (line, column) = lines.locate(eof_offset);
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span {
            start: eof_offset,
            end: eof_offset,
        },
        slice: String::new(),
        line,
        column,
    });
    LexerResult { tokens, errors }
}

use logos::{Lexer, Logos};
use novella_core::{
    Span,
    token::{Token, TokenKind},
};

#[derive(Logos, Debug, PartialEq, Clone, Copy)]
#[logos(skip(r#"//[^\r\n]*"#, allow_greedy = true))]
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
    #[token("interface")]
    Interface,
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
            RawToken::Interface => Interface,
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
        }
    }
}

pub struct TokenError {
    pub token: Token,
    pub error: String,
}

pub struct LexerResult {
    pub tokens: Vec<Token>,
    pub errors: Vec<TokenError>,
}

#[inline]
fn to_token(input: &str, t: Option<RawToken>, lexer: &Lexer<'_, RawToken>) -> Token {
    let span = Span {
        start: lexer.span().start,
        end: lexer.span().end,
    };
    let line = input[0..span.start].lines().count();
    let column = input[span.start..span.end]
        .chars()
        .position(|c| c == '\n')
        .unwrap_or(span.end);
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

pub fn lexer(input: &str) -> Vec<Token> {
    let mut lexer = RawToken::lexer(input);
    let mut tokens = vec![];
    let mut errors = vec![];
    while let Some(t) = lexer.next() {
        match t {
            Ok(token) => {
                tokens.push(to_token(input, Some(token), &lexer));
            }
            Err(_) => errors.push(TokenError {
                token: to_token(input, None, &lexer),
                error: format!(
                    "Invalid Character '{}' (U+{:04X})",
                    lexer.slice(),
                    lexer.slice().bytes().next().unwrap() as u32
                ),
            }),
        }
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span { start: 0, end: 0 },
        slice: "".to_string(),
        line: 0,
        column: 0,
    });
    tokens
}

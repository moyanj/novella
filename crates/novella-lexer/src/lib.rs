use logos::Logos;
use novella_core::token::TokenKind;

#[derive(Logos, Debug, PartialEq, Clone, Copy)]
#[logos(skip(r#"//[^\r\n]*"#, allow_greedy = true))]
#[logos(skip r#"[ \t\x0B\x0C\xA0]+"#)]
#[logos(skip r#"[\r\n\u2028\u2029]"#)]
pub enum Token {
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
    #[token("var")]
    Var,
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
/// 转换是纯种类映射(不携带位置与原文),105 个变体一一对应;
/// 由 match 的穷尽性保证 logos 侧新增变体时此处编译报错、不会漏转。
impl From<Token> for TokenKind {
    fn from(token: Token) -> TokenKind {
        use TokenKind::*;
        match token {
            // ---------- 多字符运算符 ----------
            Token::PlusPlus => PlusPlus,
            Token::MinusMinus => MinusMinus,
            Token::Power => Power,
            Token::PowerAssign => PowerAssign,
            Token::LeftShiftArithmetic => LeftShiftArithmetic,
            Token::RightShiftArithmetic => RightShiftArithmetic,
            Token::LeftShiftArithmeticAssign => LeftShiftArithmeticAssign,
            Token::RightShiftArithmeticAssign => RightShiftArithmeticAssign,
            Token::Equals => Equals,
            Token::NotEquals => NotEquals,
            Token::IdentityEquals => IdentityEquals,
            Token::IdentityNotEquals => IdentityNotEquals,
            Token::LessThanEquals => LessThanEquals,
            Token::GreaterThanEquals => GreaterThanEquals,
            Token::And => And,
            Token::Or => Or,
            Token::NullCoalesce => NullCoalesce,
            Token::Arrow => Arrow,
            Token::ReturnArrow => ReturnArrow,
            Token::MultiplyAssign => MultiplyAssign,
            Token::DivideAssign => DivideAssign,
            Token::ModulusAssign => ModulusAssign,
            Token::PlusAssign => PlusAssign,
            Token::MinusAssign => MinusAssign,
            Token::BitAndAssign => BitAndAssign,
            Token::BitXorAssign => BitXorAssign,
            Token::BitOrAssign => BitOrAssign,

            // ---------- 单字符运算符 / 分隔符 ----------
            Token::OpenBracket => OpenBracket,
            Token::CloseBracket => CloseBracket,
            Token::OpenParen => OpenParen,
            Token::CloseParen => CloseParen,
            Token::OpenBrace => OpenBrace,
            Token::CloseBrace => CloseBrace,
            Token::SemiColon => SemiColon,
            Token::Comma => Comma,
            Token::Assign => Assign,
            Token::QuestionMark => QuestionMark,
            Token::Colon => Colon,
            Token::Dot => Dot,
            Token::Plus => Plus,
            Token::Minus => Minus,
            Token::BitNot => BitNot,
            Token::Not => Not,
            Token::Multiply => Multiply,
            Token::Divide => Divide,
            Token::Modulus => Modulus,
            Token::LessThan => LessThan,
            Token::MoreThan => MoreThan,
            Token::BitAnd => BitAnd,
            Token::BitXOr => BitXOr,
            Token::BitOr => BitOr,
            Token::At => At,

            // ---------- 关键字 ----------
            Token::From => From,
            Token::Import => Import,
            Token::Pub => Pub,
            Token::Pri => Pri,
            Token::Const => Const,
            Token::Let => Let,
            Token::Var => Var,
            Token::Static => Static,
            Token::Func => Func,
            Token::Class => Class,
            Token::Interface => Interface,
            Token::New => New,
            Token::This => This,
            Token::If => If,
            Token::Else => Else,
            Token::Elif => Elif,
            Token::While => While,
            Token::For => For,
            Token::In => In,
            Token::Break => Break,
            Token::Continue => Continue,
            Token::Return => Return,
            Token::Default => Default,
            Token::Match => Match,
            Token::Case => Case,
            Token::KeyAnd => KeyAnd,
            Token::KeyOr => KeyOr,
            Token::KeyNot => KeyNot,
            Token::Try => Try,
            Token::Catch => Catch,
            Token::Finally => Finally,
            Token::Throw => Throw,
            Token::With => With,
            Token::As => As,
            Token::Type => Type,
            Token::Is => Is,
            Token::Async => Async,
            Token::Await => Await,
            Token::Stream => Stream,
            Token::Yield => Yield,
            Token::Macro => Macro,
            Token::Extern => Extern,

            // ---------- 字面量 ----------
            Token::Null => Null,
            Token::BooleanLiteral => BooleanLiteral,
            Token::HexIntegerLiteral => HexIntegerLiteral,
            Token::OctalIntegerLiteral => OctalIntegerLiteral,
            Token::BinaryIntegerLiteral => BinaryIntegerLiteral,
            Token::DecimalLiteral => DecimalLiteral,
            Token::StringLiteral => StringLiteral,
            Token::FormatStringLiteral => FormatStringLiteral,
            Token::RawStringLiteral => RawStringLiteral,
            Token::ByteStringLiteral => ByteStringLiteral,

            // ---------- 标识符 ----------
            Token::Identifier => Identifier,
        }
    }
}

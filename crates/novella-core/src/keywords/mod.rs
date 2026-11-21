//! 关键字种子表：`TokenKind` → 源码字面量。
//!
//! 这是**唯一的**「关键字字符串 → 词法单元」映射，用途有两个：
//! 1. 语法高亮生成器（`venv gen-syntax`）据此产出 TextMate 语法，避免在插件侧
//!    再手写一份正则关键字表（见根 AGENTS.md 的 CONVENTIONS）；
//! 2. 编辑器能力（补全 / 悬停）据此列出关键字。
//!
//! `TokenKind` 本身是纯数据投影、不携带字面量，因此映射必须单独维护；
//! 新增 / 删除关键字时，lexer 的 `RawToken`、`TokenKind` 与本表必须同步演进
//! （`keyword_literals()` 与本表的差异由测试兜底）。

use crate::token::TokenKind;

/// 一个关键字条目：词法种类 + 源码字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeywordEntry {
    /// 对应的词法单元种类
    pub kind: TokenKind,
    /// 源码中的字面量（小写拼写）
    pub literal: &'static str,
}

/// 全部关键字（含 `null` / `true` / `false`）。
///
/// 顺序与 lexer 中 `RawToken` 的声明顺序一致，便于对照维护。
pub const KEYWORDS: [KeywordEntry; 46] = [
    KeywordEntry {
        kind: TokenKind::From,
        literal: "from",
    },
    KeywordEntry {
        kind: TokenKind::Import,
        literal: "import",
    },
    KeywordEntry {
        kind: TokenKind::Pub,
        literal: "pub",
    },
    KeywordEntry {
        kind: TokenKind::Pri,
        literal: "pri",
    },
    KeywordEntry {
        kind: TokenKind::Pkg,
        literal: "pkg",
    },
    KeywordEntry {
        kind: TokenKind::Const,
        literal: "const",
    },
    KeywordEntry {
        kind: TokenKind::Let,
        literal: "let",
    },
    KeywordEntry {
        kind: TokenKind::Static,
        literal: "static",
    },
    KeywordEntry {
        kind: TokenKind::Weak,
        literal: "weak",
    },
    KeywordEntry {
        kind: TokenKind::Func,
        literal: "func",
    },
    KeywordEntry {
        kind: TokenKind::Class,
        literal: "class",
    },
    KeywordEntry {
        kind: TokenKind::Struct,
        literal: "struct",
    },
    KeywordEntry {
        kind: TokenKind::Trait,
        literal: "trait",
    },
    KeywordEntry {
        kind: TokenKind::New,
        literal: "new",
    },
    KeywordEntry {
        kind: TokenKind::This,
        literal: "this",
    },
    KeywordEntry {
        kind: TokenKind::If,
        literal: "if",
    },
    KeywordEntry {
        kind: TokenKind::Else,
        literal: "else",
    },
    KeywordEntry {
        kind: TokenKind::Elif,
        literal: "elif",
    },
    KeywordEntry {
        kind: TokenKind::While,
        literal: "while",
    },
    KeywordEntry {
        kind: TokenKind::For,
        literal: "for",
    },
    KeywordEntry {
        kind: TokenKind::In,
        literal: "in",
    },
    KeywordEntry {
        kind: TokenKind::Break,
        literal: "break",
    },
    KeywordEntry {
        kind: TokenKind::Continue,
        literal: "continue",
    },
    KeywordEntry {
        kind: TokenKind::Return,
        literal: "return",
    },
    KeywordEntry {
        kind: TokenKind::Default,
        literal: "default",
    },
    KeywordEntry {
        kind: TokenKind::Match,
        literal: "match",
    },
    KeywordEntry {
        kind: TokenKind::Case,
        literal: "case",
    },
    KeywordEntry {
        kind: TokenKind::KeyAnd,
        literal: "and",
    },
    KeywordEntry {
        kind: TokenKind::KeyOr,
        literal: "or",
    },
    KeywordEntry {
        kind: TokenKind::KeyNot,
        literal: "not",
    },
    KeywordEntry {
        kind: TokenKind::Try,
        literal: "try",
    },
    KeywordEntry {
        kind: TokenKind::Catch,
        literal: "catch",
    },
    KeywordEntry {
        kind: TokenKind::Finally,
        literal: "finally",
    },
    KeywordEntry {
        kind: TokenKind::Throw,
        literal: "throw",
    },
    KeywordEntry {
        kind: TokenKind::With,
        literal: "with",
    },
    KeywordEntry {
        kind: TokenKind::As,
        literal: "as",
    },
    KeywordEntry {
        kind: TokenKind::Type,
        literal: "type",
    },
    KeywordEntry {
        kind: TokenKind::Is,
        literal: "is",
    },
    KeywordEntry {
        kind: TokenKind::Async,
        literal: "async",
    },
    KeywordEntry {
        kind: TokenKind::Await,
        literal: "await",
    },
    KeywordEntry {
        kind: TokenKind::Yield,
        literal: "yield",
    },
    KeywordEntry {
        kind: TokenKind::Extern,
        literal: "extern",
    },
    KeywordEntry {
        kind: TokenKind::Unsafe,
        literal: "unsafe",
    },
    KeywordEntry {
        kind: TokenKind::Null,
        literal: "null",
    },
    KeywordEntry {
        kind: TokenKind::BooleanLiteral,
        literal: "true",
    },
    KeywordEntry {
        kind: TokenKind::BooleanLiteral,
        literal: "false",
    },
];

/// 关键字字面量集合（供生成器判断某个字符串是否为关键字）。
pub fn is_keyword_literal(literal: &str) -> bool {
    KEYWORDS.iter().any(|entry| entry.literal == literal)
}

/// 按字面量查找对应的词法单元种类。
pub fn kind_of(literal: &str) -> Option<TokenKind> {
    KEYWORDS
        .iter()
        .find(|entry| entry.literal == literal)
        .map(|entry| entry.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表内不得有重复字面量（重复说明维护时粘贴出错）。
    #[test]
    fn literals_are_unique() {
        for (i, a) in KEYWORDS.iter().enumerate() {
            for b in &KEYWORDS[..i] {
                assert_ne!(a.literal, b.literal, "字面量 `{}` 重复登记", a.literal);
            }
        }
    }

    /// 关键字之外的普通标识符不应被误登记。
    #[test]
    fn non_keywords_are_rejected() {
        assert!(!is_keyword_literal("main"));
        assert!(is_keyword_literal("func"));
        assert_eq!(kind_of("func"), Some(TokenKind::Func));
    }
}

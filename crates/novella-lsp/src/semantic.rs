//! 词法级语义高亮：`TokenKind` → LSP semantic token 的映射与编码。
//!
//! 本模块是薄 LSP 的核心，包含三部分：
//! 1. [`TOKEN_TYPES`]：服务器声明的高亮类别（客户端据此选择主题配色）；
//! 2. [`token_type_index`]：把 [`TokenKind`] 映射到 legend 下标，
//!    用穷尽 `match` 保证词法枚举新增变体时编译报错、不会漏配；
//! 3. [`encode`]：把 token 流编码为 LSP 要求的 delta 整数序列；
//!    字节偏移 → UTF-16 码元的换算本身由 [`crate::source::SemanticIndex`] 统一提供。
//!
//! 说明：当前只做词法级高亮，标识符不参与输出——它的语义角色
//! （变量 / 类型 / 函数）需要 parser / sema 才能判定，
//! 词法阶段强行染色反而会误导主题配色。

use lsp_types::{SemanticToken, SemanticTokenType};
use novella_core::token::Token;

use crate::source::SemanticIndex;

/// 服务器使用的语义 token 类别，数组顺序即 legend 下标。
///
/// 全部采用 LSP 标准类型（无自定义类型），因此主流主题无需额外配置即可着色。
pub const TOKEN_TYPES: [SemanticTokenType; 5] = [
    SemanticTokenType::COMMENT,
    SemanticTokenType::KEYWORD,
    SemanticTokenType::NUMBER,
    SemanticTokenType::OPERATOR,
    SemanticTokenType::STRING,
];

// ---------- legend 下标（必须与 TOKEN_TYPES 顺序一致） ----------
const COMMENT: u32 = 0;
const KEYWORD: u32 = 1;
const NUMBER: u32 = 2;
const OPERATOR: u32 = 3;
const STRING: u32 = 4;

/// 把词法单元种类映射为 legend 下标；返回 `None` 表示不输出语义 token
/// （交由编辑器主题按默认规则着色）。
///
/// 关键字分支涵盖全部关键字以及 `null` / `true` / `false`：
/// 后者在 LSP 标准里没有独立类别，主流主题也按关键字配色。
pub fn token_type_index(kind: novella_core::token::TokenKind) -> Option<u32> {
    use novella_core::token::TokenKind::*;

    let index = match kind {
        // ---------- 注释 ----------
        LineComment => COMMENT,

        // ---------- 关键字（含 null / 布尔常量） ----------
        From | Import | Pub | Pri| Pkg | Const | Let | Static | Weak | Func | Class | Struct | Trait
        | New | This | If | Else | Elif | While | For | In | Break | Continue | Return | Default
        | Match | Case | KeyAnd | KeyOr | KeyNot | Try | Catch | Finally | Throw | With | As
        | Type | Is | Async | Await | Yield | Macro | Extern | Unsafe | Null | BooleanLiteral => KEYWORD,

        // ---------- 数字字面量 ----------
        HexIntegerLiteral | OctalIntegerLiteral | BinaryIntegerLiteral | DecimalLiteral => NUMBER,

        // ---------- 字符串字面量（普通 / 格式化 / 原始 / 字节） ----------
        StringLiteral | FormatStringLiteral | RawStringLiteral | ByteStringLiteral => STRING,

        // ---------- 运算符 ----------
        // 多字符运算符
        PlusPlus | MinusMinus | LeftShiftArithmetic | RightShiftArithmetic
        | LeftShiftArithmeticAssign | RightShiftArithmeticAssign | Equals | NotEquals
        | IdentityEquals | IdentityNotEquals | LessThanEquals | GreaterThanEquals | And | Or
        | NullCoalesce | Arrow | ReturnArrow | MultiplyAssign | DivideAssign | ModulusAssign
        | PlusAssign | MinusAssign | BitAndAssign | BitXorAssign | BitOrAssign
        // 单字符运算符与语法标记（`@` 装饰器前缀）
        | Assign | QuestionMark | Plus | Minus | BitNot | Not | Multiply | Divide | Modulus
        | LessThan | MoreThan | BitAnd | BitXOr | BitOr | At => OPERATOR,

        // ---------- 不输出：结构分隔符 / 标识符 / EOF / 错误 ----------
        // 括号、分号、逗号、冒号、点由主题默认着色，强行染成 operator 反而突兀；
        // 标识符无语义角色可判；EOF 与 Error 无源码可染。
        OpenBracket | CloseBracket | OpenParen | CloseParen | OpenBrace | CloseBrace | SemiColon
        | Comma | Colon | Dot | Identifier | Eof | Error => return None,
    };

    Some(index)
}

/// 把源码与 token 流编码为 LSP semantic tokens（delta 编码）。
///
/// - `tokens` 须按源码顺序排列（`novella_lexer::lexer` 的输出天然满足），
///   否则 delta 会出现负数回退；
/// - 行内偏移一律以 **UTF-16 码元**计：换算统一交给 [`SemanticIndex`]，
///   本模块不再自带行表（含中文或 emoji 的行若直接用字节偏移，高亮会整体错位）；
/// - 跨行 token 会被按行拆成多段（LSP 要求单个语义 token 不跨行）。
///
/// 需要多次复用同一份源码的换算表时（如服务器逐请求处理），改用
/// [`encode_with_index`]，避免重复建表。
#[cfg(test)]
pub fn encode(source: &str, tokens: &[Token]) -> Vec<SemanticToken> {
    encode_with_index(&SemanticIndex::new(source), source, tokens)
}

/// 用已建好的换算索引编码语义 token。
pub fn encode_with_index(
    index: &SemanticIndex,
    source: &str,
    tokens: &[Token],
) -> Vec<SemanticToken> {
    let mut data = Vec::new();
    // delta 基准：上一段 token 的行号与行内 UTF-16 起始列
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    for token in tokens {
        let Some(token_type) = token_type_index(token.kind) else {
            continue;
        };
        for (line, start, length) in index.segments(source, token.span) {
            let delta_line = line - prev_line;
            // 同一行内 delta 相对上一 token 起点，跨行则为本行绝对列
            let delta_start = if delta_line == 0 {
                start - prev_start
            } else {
                start
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: 0,
            });
            prev_line = line;
            prev_start = start;
        }
    }

    data
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把 delta 编码展开为绝对位置 `(行, 列, 长度, 类型)`，便于断言可读性。
    fn decode(data: &[SemanticToken]) -> Vec<(u32, u32, u32, u32)> {
        let mut decoded = Vec::new();
        let mut line = 0u32;
        let mut start = 0u32;
        for token in data {
            line += token.delta_line;
            start = if token.delta_line == 0 {
                start + token.delta_start
            } else {
                token.delta_start
            };
            decoded.push((line, start, token.length, token.token_type));
        }
        decoded
    }

    /// 中文与 emoji 的行内偏移必须按 UTF-16 码元计，而非字节。
    ///
    /// 本用例是回归防线：若改回字节偏移，`=` 与字符串的列会明显右移。
    #[test]
    fn offsets_are_utf16_units() {
        // 第 1 行 "// 注释"：2 个斜杠 + 1 空格 + 2 汉字 = 5 个 UTF-16 码元
        // 第 2 行 `let x = "🐋";`：emoji 在 UTF-16 中占 2 码元
        let source = "// 注释\nlet x = \"🐋\";\n";
        let result = novella_lexer::lexer(source);
        let decoded = decode(&encode(source, &result.tokens));

        assert_eq!(
            decoded,
            vec![
                (0, 0, 5, COMMENT),  // "// 注释"
                (1, 0, 3, KEYWORD),  // let
                (1, 6, 1, OPERATOR), // =
                (1, 8, 4, STRING),   // "🐋"：引号 1 + emoji 2 + 引号 1
            ]
        );
    }

    /// 标识符与分隔符不输出；数字与关键字按类别映射。
    #[test]
    fn skips_identifier_and_delimiters() {
        let source = "let count = 42;\n";
        let result = novella_lexer::lexer(source);
        let decoded = decode(&encode(source, &result.tokens));

        // 仅 let / = / 42 三项；`count`、`;` 不输出
        assert_eq!(
            decoded,
            vec![(0, 0, 3, KEYWORD), (0, 10, 1, OPERATOR), (0, 12, 2, NUMBER)]
        );
    }

    /// 多行文档的 delta 编码：跨行时 delta_start 为该行绝对列。
    #[test]
    fn delta_encoding_across_lines() {
        let source = "func main() {\n    return 1;\n}\n";
        let result = novella_lexer::lexer(source);
        let decoded = decode(&encode(source, &result.tokens));

        assert_eq!(
            decoded,
            vec![
                (0, 0, 4, KEYWORD), // func
                (1, 4, 6, KEYWORD), // return
                (1, 11, 1, NUMBER), // 1
            ]
        );
    }

    /// 所有 keyword 变体都应映射到 KEYWORD（抽查保证映射表随枚举演进）。
    #[test]
    fn keyword_variants_map_to_keyword() {
        use novella_core::token::TokenKind;
        for kind in [
            TokenKind::Let,
            TokenKind::Const,
            TokenKind::Func,
            TokenKind::Match,
            TokenKind::Null,
            TokenKind::BooleanLiteral,
        ] {
            assert_eq!(token_type_index(kind), Some(KEYWORD), "{kind:?} 应为关键字");
        }
    }
}

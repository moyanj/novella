//! A3：文档内高亮（`textDocument/documentHighlight`）。
//!
//! 与语义高亮的区别：语义高亮按 `TokenKind` 给**全部** token 上色，而文档高亮
//! 只回答「光标所在的名字在当前文档里还有哪些出现」，用于把同名引用一起点亮。
//!
//! 这里的判定是**纯词法**的（不建符号表、不跨模块）：
//! - 声明位：`let` / `const` / `func` / `class` / `struct` / `trait` / 参数列表里的名字，
//!   以及 `import ... as 别名` / `from ... import 名字`；
//! - 其余出现一律按「引用」处理。
//!
//! 好处是不需要 AST 与 sema，代价是**同名遮蔽会被一并点亮**（例如内层 `let x`
//! 会点亮外层的 `x`）。这一点在 `docs/lsp.md` 中记录为已知边界，精确的
//! 作用域判定留给后续接入 sema 绑定表之后。

use lsp_types::{DocumentHighlight, DocumentHighlightKind};
use novella_core::token::{Token, TokenKind};

use crate::source::SemanticIndex;

/// 声明关键字集合：其后紧跟的标识符是该关键字的**名字**（不是声明）。
///
/// `import` / `as` / `break` / `continue` / `return` 等虽然也跟标识符，
/// 但后者是引用或关键字本身，不在此列。
const DECLARING_KEYWORDS: &[TokenKind] = &[
    TokenKind::Let,
    TokenKind::Const,
    TokenKind::Func,
    TokenKind::Class,
    TokenKind::Struct,
    TokenKind::Trait,
    TokenKind::Type,
    TokenKind::Static,
    TokenKind::Weak,
    TokenKind::Pkg,
    TokenKind::Pub,
    TokenKind::Pri,
];

/// 光标处的标识符 + 文档内同名的全部出现（声明与引用分别标注）。
///
/// 光标不在标识符上时返回空表——编辑器对空结果的处理是「不点亮任何东西」，
/// 优于报错。
pub fn document_highlights(
    index: &SemanticIndex,
    tokens: &[Token],
    offset: usize,
) -> Vec<DocumentHighlight> {
    let Some(target) = tokens
        .iter()
        .find(|token| token.kind == TokenKind::Identifier && contains(token, offset))
    else {
        return Vec::new();
    };

    let declarations = declaration_set(tokens);
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.kind == TokenKind::Identifier && token.slice == target.slice)
        .map(|(position, token)| DocumentHighlight {
            range: index.range(token.span),
            kind: Some(if declarations.contains(&position) {
                DocumentHighlightKind::WRITE
            } else {
                DocumentHighlightKind::READ
            }),
        })
        .collect()
}

/// token 的 span 是否覆盖给定偏移（右端开区间，EOF 之类零宽 span 不命中）。
fn contains(token: &Token, offset: usize) -> bool {
    span_contains(token.span, offset)
}

/// 字节区间是否覆盖偏移（左闭右开）。
fn span_contains(span: novella_core::Span, offset: usize) -> bool {
    span.start <= offset && offset < span.end
}

/// 找出「声明位」的 token 下标集合。
///
/// 判定规则（全部局部、只看相邻 token）：
/// 1. 声明关键字后的第一个标识符是声明的名字；
/// 2. 参数列表里的标识符：紧跟一个类型位标识符且**其后**不是 `(`（排除函数调用），
///    且前面是 `(` 或 `,`——`func f(a int, b int)` 里的参数名；
/// 3. `... as 名字`（导入别名）与 `import 名字`（具名导入）。
fn declaration_set(tokens: &[Token]) -> std::collections::HashSet<usize> {
    let mut declared = std::collections::HashSet::new();
    for (position, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Identifier {
            continue;
        }
        let previous = position.checked_sub(1).and_then(|p| tokens.get(p));
        let next = tokens.get(position + 1);
        let before_previous = position.checked_sub(2).and_then(|p| tokens.get(p));

        let is_declaration = match previous.map(|p| p.kind) {
            // 情况 1：声明关键字之后（skip 掉可能出现在 `pub func f` 中的多个修饰词）
            Some(kind) if DECLARING_KEYWORDS.contains(&kind) => true,
            // `import` / `as` 后跟名字即导入别名或具名导入
            Some(TokenKind::Import) | Some(TokenKind::As) => true,
            // 情况 2：参数名——前是 `(` 或 `,`，后是类型标识符，再后不是 `(`
            Some(TokenKind::OpenParen) | Some(TokenKind::Comma) => {
                next.is_some_and(|n| n.kind == TokenKind::Identifier)
                    && tokens
                        .get(position + 2)
                        .is_none_or(|after| after.kind != TokenKind::OpenParen)
            }
            // `pub` / `pri` 等修饰词后可能夹着别的修饰词，向前回溯一层
            Some(TokenKind::Pub) | Some(TokenKind::Pri) | Some(TokenKind::Pkg) => true,
            _ => before_previous.is_some_and(|p| DECLARING_KEYWORDS.contains(&p.kind)),
        };
        if is_declaration {
            declared.insert(position);
        }
    }
    declared
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens_of(source: &str) -> (SemanticIndex, Vec<Token>) {
        let lexed = novella_lexer::lexer(source);
        (SemanticIndex::new(source), lexed.tokens)
    }

    /// `let` 声明的名字标为写入，同名引用标为读取。
    #[test]
    fn distinguishes_declaration_from_reference() {
        let source = "func main() void {\n    let total = 0;\n    total = total + 1;\n}\n";
        let (index, tokens) = tokens_of(source);
        // 光标放在第一处 total（声明位）
        let offset = source.find("total").expect("存在 total");
        let highlights = document_highlights(&index, &tokens, offset);
        assert_eq!(highlights.len(), 3, "1 处声明 + 2 处引用");
        assert_eq!(highlights[0].kind, Some(DocumentHighlightKind::WRITE));
        assert_eq!(highlights[1].kind, Some(DocumentHighlightKind::READ));
        assert_eq!(highlights[2].kind, Some(DocumentHighlightKind::READ));
    }

    /// 函数参数名应被识别为声明，函数调用不点亮。
    #[test]
    fn parameters_are_declarations() {
        let source = "func add(a i32, b i32) i32 {\n    return a;\n}\n";
        let (index, tokens) = tokens_of(source);
        let offset = source.find("return a").expect("存在引用") + 7;
        let highlights = document_highlights(&index, &tokens, offset);
        assert_eq!(highlights.len(), 2, "参数声明 + 函数体引用");
        assert_eq!(highlights[0].kind, Some(DocumentHighlightKind::WRITE));
        assert_eq!(highlights[1].kind, Some(DocumentHighlightKind::READ));
    }

    /// 光标不在标识符上时返回空表。
    #[test]
    fn non_identifier_position_yields_nothing() {
        let source = "let x = 1;\n";
        let (index, tokens) = tokens_of(source);
        let offset = source.find('=').expect("存在等号");
        assert!(document_highlights(&index, &tokens, offset).is_empty());
    }
}
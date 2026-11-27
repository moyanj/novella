//! `textDocument/completion`：关键字、预定义类型与文档内符号的补全。

use lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse};

use crate::document::SourceDoc;
use crate::outline::{Outline, OutlineKind};

/// 补全上下文：由光标前缀决定给哪一类候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionContext {
    /// 普通位置：关键字、基础类型、本文件符号
    General,
    /// `obj.` 之后的成员访问
    Member,
    /// 类型位（`:` / `->` / `as` / `is` 之后）
    Type,
}

/// `textDocument/completion`：按上下文返回候选。
///
/// 候选来源：
/// - 关键字与基础类型：`novella_core::keywords`（唯一事实源）+ AST 的预定义类型集；
/// - 本文件符号：文档符号遍历结果（函数 / 类 / 结构体 / trait / 变量……）；
/// - 成员访问 `obj.`：本文件中所有类 / struct 的字段与方法。
///
/// **已知边界**：成员补全不推导接收者类型，给的是本文件全部成员的并集。
/// 在类型推导接进 LSP 之前，这比返回空列表有用，但候选会偏多。
pub fn completion(doc: &SourceDoc, outline: &Outline, offset: usize) -> CompletionResponse {
    let context = completion_context(doc, offset);
    let mut items = match context {
        CompletionContext::General => {
            let mut items = keyword_items();
            items.extend(predefined_type_items());
            items.extend(symbol_items(outline, &[OutlineKind::Function]));
            items
        }
        CompletionContext::Type => {
            let mut items = predefined_type_items();
            items.extend(symbol_items(
                outline,
                &[
                    OutlineKind::Class,
                    OutlineKind::Struct,
                    OutlineKind::Trait,
                    OutlineKind::TypeAlias,
                    OutlineKind::TypeParameter,
                ],
            ));
            items
        }
        CompletionContext::Member => symbol_items(
            outline,
            &[
                OutlineKind::Field,
                OutlineKind::Method,
                OutlineKind::Parameter,
            ],
        ),
    };
    // 同名去重：`int` 既在关键字表也在预定义类型表里
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.clone()));
    CompletionResponse::Array(items)
}

/// 补全关键字（与 `novella_core::keywords::KEYWORDS` 同源，不再手写第二份）。
fn keyword_items() -> Vec<CompletionItem> {
    novella_core::keywords::KEYWORDS
        .iter()
        .map(|entry| CompletionItem {
            label: entry.literal.to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect()
}

/// 预定义基础类型补全（`int` / `float` / `string` …）。
fn predefined_type_items() -> Vec<CompletionItem> {
    let mut items = vec![CompletionItem {
        label: "string".to_owned(),
        kind: Some(CompletionItemKind::TYPE_PARAMETER),
        ..Default::default()
    }];
    items.extend(
        novella_ast::types::PredefinedType::ALL
            .iter()
            .map(|ty| CompletionItem {
                label: ty.spelling().to_owned(),
                kind: Some(CompletionItemKind::TYPE_PARAMETER),
                ..Default::default()
            }),
    );
    items
}

/// 本文件里指定种类的符号转为补全项。
fn symbol_items(outline: &Outline, kinds: &[OutlineKind]) -> Vec<CompletionItem> {
    outline
        .symbols
        .iter()
        .filter(|symbol| kinds.contains(&symbol.kind))
        .map(|symbol| CompletionItem {
            label: symbol.name.clone(),
            kind: Some(match symbol.kind {
                OutlineKind::Function | OutlineKind::Method => CompletionItemKind::FUNCTION,
                OutlineKind::Class => CompletionItemKind::CLASS,
                OutlineKind::Struct => CompletionItemKind::STRUCT,
                OutlineKind::Trait => CompletionItemKind::INTERFACE,
                OutlineKind::TypeAlias
                | OutlineKind::AssociatedType
                | OutlineKind::TypeParameter => CompletionItemKind::TYPE_PARAMETER,
                OutlineKind::Parameter | OutlineKind::Variable => CompletionItemKind::VARIABLE,
                OutlineKind::Field => CompletionItemKind::FIELD,
                OutlineKind::Import => CompletionItemKind::MODULE,
            }),
            detail: symbol.detail.clone(),
            ..Default::default()
        })
        .collect()
}

/// 判定光标处的补全上下文（只看光标所在行的前缀）。
///
/// 规则从窄到宽：`.` 之后是成员；`:` / `->` / 以 `as`、`is` 结尾是类型位；其余为普通位置。
pub(crate) fn completion_context(doc: &SourceDoc, offset: usize) -> CompletionContext {
    let (line, column) = doc.index.position(offset);
    let line_text = doc.index.line_text(&doc.text, line as usize);
    let prefix = &line_text[..byte_of_column(line_text, column)];
    let trimmed = prefix.trim_end();

    if trimmed.ends_with('.') {
        return CompletionContext::Member;
    }
    // `:` 结尾（类型注解）——排除空表构造与三元等不常见情形，这里按类型位优先
    if trimmed.ends_with(':') || trimmed.ends_with("->") {
        return CompletionContext::Type;
    }
    // `as` / `is` 之后也是类型位；用「词边界结尾」判断，避免误判 `alias` / `this`
    for keyword in ["as", "is"] {
        if let Some(rest) = trimmed.strip_suffix(keyword) {
            let boundary_ok = rest.chars().next_back().is_none_or(|ch| !is_ident_char(ch));
            if boundary_ok {
                return CompletionContext::Type;
            }
        }
    }
    CompletionContext::General
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// UTF-16 列 → 行内字节偏移。
///
/// 列落在某个字符的中间（代理对后半）时取该字符起点：补全前缀按字符边界切开即可，
/// 切在代理对中间会产生非法 UTF-8。列越界时夹到行尾。
pub(crate) fn byte_of_column(line: &str, column: u32) -> usize {
    let mut utf16 = 0u32;
    for (offset, ch) in line.char_indices() {
        if utf16 + ch.len_utf16() as u32 > column {
            return offset;
        }
        utf16 += ch.len_utf16() as u32;
    }
    line.len()
}

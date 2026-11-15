//! `textDocument/selectionRange`：由内到外的逐级选择区间。

use lsp_types::{Position, Range, SelectionRange};

use novella_core::Span;

use crate::document::SourceDoc;
use crate::outline::Outline;

/// `textDocument/selectionRange`：由内到外的逐级选择区间。
///
/// 链：光标处 token → 各层覆盖光标的符号声明（按区间从小到大）→ 整个文件。
/// 不做表达式级细分（那需要完整表达式遍历，价值低于复杂度）。
pub fn selection_ranges(
    doc: &SourceDoc,
    outline: &Outline,
    positions: &[Position],
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|position| {
            let offset = doc.index.offset_of(*position);
            let mut chain: Vec<Range> = Vec::new();

            // 最内层是光标所在的那个名字（声明或引用）：AST 不为名字单独记 span，
            // 而符号的 range 覆盖整条声明，所以这里按「光标处的标识符」构造最内层
            if let Some(token) = doc
                .token_at(offset)
                .filter(|token| token.kind == novella_core::token::TokenKind::Identifier)
            {
                chain.push(doc.index.range(token.span));
            }

            let mut covering = outline
                .symbols
                .iter()
                .filter(|symbol| offset >= symbol.span.start && offset <= symbol.span.end)
                .map(|symbol| symbol.span)
                .collect::<Vec<_>>();
            covering.sort_by_key(|span| span.end - span.start);
            covering.dedup();
            for span in covering {
                let range = doc.index.range(span);
                if chain.last() != Some(&range) {
                    chain.push(range);
                }
            }

            let whole = doc.index.range(Span {
                start: 0,
                end: doc.text.len(),
            });
            if chain.last() != Some(&whole) {
                chain.push(whole);
            }

            // chain 由内到外排列；LSP 约定返回最内层节点、parent 指向更大的区间，
            // 因此倒序构造：先建整份文件（无 parent），再逐层向内包
            let mut range: Option<SelectionRange> = None;
            for current in chain.iter().rev() {
                range = Some(SelectionRange {
                    range: *current,
                    parent: range.map(Box::new),
                });
            }
            range.unwrap_or(SelectionRange {
                range: whole,
                parent: None,
            })
        })
        .collect()
}

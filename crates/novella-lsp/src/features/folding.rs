//! `textDocument/foldingRange`：把折叠区间转成 LSP 形态并按行排序。

use lsp_types::{FoldingRange, FoldingRangeKind};

use crate::outline::{FoldKind, Outline};

/// `textDocument/foldingRange`：把 [`FoldKind`] 映射到 LSP 折叠种类。
///
/// LSP 的 `FoldingRangeKind` 只有 `comment` / `imports` / `region` 三种语义，
/// 其余结构折叠留空（客户端按普通折叠展示）。
pub fn folding_ranges(outline: &Outline) -> Vec<FoldingRange> {
    let mut ranges = outline
        .folds
        .iter()
        .map(|fold| FoldingRange {
            start_line: fold.start_line,
            start_character: None,
            end_line: fold.end_line,
            end_character: None,
            kind: match fold.kind {
                FoldKind::Comments => Some(FoldingRangeKind::Comment),
                FoldKind::Imports => Some(FoldingRangeKind::Imports),
                _ => None,
            },
            collapsed_text: None,
        })
        .collect::<Vec<_>>();
    // 行号升序，客户端据此稳定渲染折叠标记
    ranges.sort_by_key(|range| (range.start_line, range.end_line));
    ranges.dedup_by_key(|range| (range.start_line, range.end_line));
    ranges
}

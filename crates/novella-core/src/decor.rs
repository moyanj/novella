//! 诊断装饰：把「parse 得到的裸诊断」补成带 `文件:行:列` 的完整诊断。
//!
//! 词法 / 语法阶段只认字节偏移（[`Span`]），它们不关心文件身份与行列展示；
//! 直到某个消费者（CLI 渲染、LSP 推送、sema 汇总）才需要完整位置。此前这段
//! 「span → `DiagnosticLocation`」的粘合逻辑在 lexer / parser / sema 各写一份，
//! 收在 core 后统一由 [`decorate`] 完成。
//!
//! 分工原则：`core` 提供字符级的行列换算，各阶段只负责产出 span。

use crate::Span;
use crate::diagnostic::Diagnostic;
use crate::line_index::LineIndex;

/// 为一批诊断补齐位置（行列由 `source` 的行索引回推）。
///
/// 返回条数与入参一致，顺序不变：
/// - 已有 `location.span` 但缺行列的，按其起点补齐；
/// - 完全没有 location 的（如文件读取失败），原样保留——它们本就只渲染消息。
pub fn decorate(diagnostics: Vec<Diagnostic>, file: Option<&str>, source: &str) -> Vec<Diagnostic> {
    let index = LineIndex::build(source);
    diagnostics
        .into_iter()
        .map(|mut diag| {
            let Some(location) = diag.location.as_mut() else {
                return diag;
            };
            if let Some(span) = location.span {
                let (line, column) = index.locate(span.start);
                location.line = line;
                location.column = column;
            }
            if location.file.is_none() {
                location.file = file.map(str::to_owned);
            }
            diag
        })
        .collect()
}

/// 为单个诊断补齐位置（[`decorate`] 的单条版本）。
pub fn decorate_one(diagnostic: Diagnostic, file: Option<&str>, source: &str) -> Diagnostic {
    decorate(vec![diagnostic], file, source)
        .into_iter()
        .next()
        .expect("decorate 保持入参条数，单条入参必有单条输出")
}

/// 平移诊断区间的起点并重算行列（f-string 等「内联源码」产生的诊断用）。
///
/// 内联片段被单独 lex/parse 时偏移从 0 起算；回到宿主文件时必须整体后移
/// `offset`，否则 caret 会指到文件开头。平移后钳到 `source_len`，避免越界。
pub fn shift_span_in_place(
    diagnostic: &mut Diagnostic,
    offset: usize,
    source_len: usize,
    index: &LineIndex,
) {
    let Some(location) = diagnostic.location.as_mut() else {
        return;
    };
    let Some(old_span) = location.span else {
        return;
    };
    let shifted = Span {
        start: (old_span.start + offset).min(source_len),
        end: (old_span.end + offset).min(source_len),
    };
    let (line, column) = index.locate(shifted.start);
    location.span = Some(shifted);
    location.line = line;
    location.column = column;
}

#[cfg(test)]
mod tests {
    use crate::Span;
    use crate::diagnostic::{Diagnostic, DiagnosticCode};

    use super::decorate;

    /// span 换算为 1-based 行列，并补上文件名。
    #[test]
    fn span_becomes_line_and_column() {
        let source = "let a = 1;\nlet b = 2;\n";
        let tail = source.find('b').expect("存在 b");
        let diagnostic = Diagnostic::error("坏")
            .with_code(DiagnosticCode::TypeError)
            .with_location(crate::diagnostic::DiagnosticLocation {
                file: None,
                line: 0,
                column: 0,
                span: Some(Span {
                    start: tail,
                    end: tail + 1,
                }),
            });
        let decorated = decorate(vec![diagnostic], Some("demo.nov"), source);
        let location = decorated[0].location.as_ref().expect("有位置");
        assert_eq!((location.line, location.column), (2, 4));
        assert_eq!(location.file.as_deref(), Some("demo.nov"));
        assert_eq!(
            location.span,
            Some(Span {
                start: tail,
                end: tail + 1
            })
        );
    }

    /// 无位置的诊断原样保留（文件读取失败等只渲染消息）。
    #[test]
    fn diagnostics_without_location_are_kept() {
        let decorated = decorate(vec![Diagnostic::error("读不到文件")], None, "");
        assert_eq!(decorated.len(), 1);
        assert!(decorated[0].location.is_none());
    }
}

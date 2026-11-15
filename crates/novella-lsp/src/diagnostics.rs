//! 词法 / 语法 / 语义诊断 → LSP `Diagnostic` 的转换与聚合。
//!
//! 三类诊断在 novella 内部都是 [`novella_core::diagnostic::Diagnostic`]：
//! - 词法：`LexerResult::errors` 里的 `TokenError::to_diagnostic`；
//! - 语法：`ParseResult::diagnostics`；
//! - 语义：`SemaResult` 各模块的 `semantic_diagnostics()`。
//!
//! LSP 只认「行 + UTF-16 列」，换算统一走 [`SemanticIndex`]（见 `source.rs`），
//! 因此这里必须用诊断自带的 `span` 而不是 `location.column`（后者是字节列）。
//! 缺少 `span` 的诊断（如文件读取失败）会退化为整行首字符，不让客户端收不到。

use lsp_types::{Diagnostic as LspDiagnostic, DiagnosticSeverity, NumberOrString, Range};
use novella_core::diagnostic::{Diagnostic, Severity};

use crate::source::SemanticIndex;

/// 单条内部诊断 → LSP 诊断。
///
/// 位置优先用 `location.span`；无 span 时退化为 `location` 行首，
/// 两者都没有（纯 I/O 错误）时锚定到文件开头——客户端需要一个区间才能展示。
pub fn to_lsp(index: &SemanticIndex, source: &str, diagnostic: &Diagnostic) -> LspDiagnostic {
    let severity = match diagnostic.severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
    };
    let range = diagnostic_range(index, source, diagnostic);
    // 错误码（如 NVL3002）放进 code 字段，客户端据此做快速修复或过滤，
    // 不要把码拼进 message——message 会随措辞变化。
    let code = diagnostic
        .code
        .as_deref()
        .map(|code| NumberOrString::String(code.to_owned()));

    LspDiagnostic {
        range,
        severity: Some(severity),
        code,
        code_description: None,
        source: Some("novella".to_owned()),
        message: diagnostic.message.clone(),
        related_information: None,
        tags: None,
        data: None,
    }
}

/// 取诊断的 LSP 区间。
fn diagnostic_range(index: &SemanticIndex, source: &str, diagnostic: &Diagnostic) -> Range {
    if let Some(span) = diagnostic.location.as_ref().and_then(|l| l.span) {
        return index.range(span);
    }
    if let Some(location) = &diagnostic.location {
        // 无 span 时把整行标红（1-based 行号 → 0-based），列从 0 起
        let line = location.line.saturating_sub(1) as u32;
        let end_column = index
            .line_text(
                source,
                (line as usize).min(index.line_count().saturating_sub(1)),
            )
            .len() as u32;
        return Range {
            start: lsp_types::Position { line, character: 0 },
            end: lsp_types::Position {
                line,
                character: end_column,
            },
        };
    }
    Range::default()
}

/// 批量转换，保持输入顺序（词法 → 语法 → 语义）。
pub fn convert_all(
    index: &SemanticIndex,
    source: &str,
    diagnostics: &[Diagnostic],
) -> Vec<LspDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| to_lsp(index, source, diagnostic))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use novella_core::Span;
    use novella_core::diagnostic::DiagnosticCode;

    /// span 经 UTF-16 换算后落在正确列，错误码进入 code 字段。
    #[test]
    fn span_becomes_utf16_range_with_code() {
        let source = "let x = \"🐋\";\n";
        let index = SemanticIndex::new(source);
        let start = source.find('"').expect("存在引号");
        let diagnostic = Diagnostic::error("类型不匹配")
            .with_code(DiagnosticCode::TypeError)
            .with_location(novella_core::diagnostic::DiagnosticLocation {
                file: None,
                line: 1,
                column: start,
                span: Some(Span {
                    start,
                    end: start + 1,
                }),
            });

        let converted = to_lsp(&index, source, &diagnostic);
        assert_eq!(converted.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            converted.code,
            Some(NumberOrString::String("NVL3002".to_owned()))
        );
        assert_eq!(converted.source.as_deref(), Some("novella"));
        assert_eq!(converted.range.start.line, 0);
        assert_eq!(converted.range.start.character, 8, "引号按 UTF-16 列定位");
    }

    /// 无 span 的诊断退化为整行区间，不为空。
    #[test]
    fn location_without_span_covers_whole_line() {
        let source = "let a = 1;\nlet b = 2;\n";
        let index = SemanticIndex::new(source);
        let diagnostic = Diagnostic::warning("注意").with_location(
            novella_core::diagnostic::DiagnosticLocation {
                file: Some("demo.nov".to_owned()),
                line: 2,
                column: 4,
                span: None,
            },
        );

        let converted = to_lsp(&index, source, &diagnostic);
        assert_eq!(converted.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(converted.range.start.line, 1);
        assert_eq!(converted.range.start.character, 0);
        assert_eq!(converted.range.end.character, 10);
    }

    /// 完全没有位置的诊断（I/O 错误）锚定到文件开头，仍能送达客户端。
    #[test]
    fn diagnostic_without_location_is_not_dropped() {
        let source = "";
        let index = SemanticIndex::new(source);
        let converted = to_lsp(&index, source, &Diagnostic::error("读不到文件"));
        assert_eq!(converted.range, Range::default());
        assert_eq!(converted.message, "读不到文件");
    }
}

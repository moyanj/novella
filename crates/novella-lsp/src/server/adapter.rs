//! 请求参数适配：把「带 textDocument 字段」的两类参数收敛成统一取法。
//!
//! lsp-types 对这些字段没有公共 trait，且字段名不统一（`text_document` /
//! `text_document_position` / `text_document_position_params`），
//! 这里用本地 trait 抹平，供 [`super::query`] 的通用取文档逻辑使用。

use lsp_types::{
    CompletionParams, DocumentSymbolParams, FoldingRangeParams, GotoDefinitionParams, HoverParams,
    ReferenceParams, RenameParams, SignatureHelpParams,
};

/// 「带 textDocument 字段」的请求参数。
///
/// lsp-types 对这类参数没有公共 trait（字段名也不统一），这里用本地 trait
/// 收敛 `document_of` / `document_and_offset` 两种取法，避免每个处理器
/// 重复一遍「解析参数 → 查文档 → 换算偏移」。
pub(crate) trait HasTextDocument {
    fn text_document_uri(&self) -> &str;
}

impl HasTextDocument for DocumentSymbolParams {
    fn text_document_uri(&self) -> &str {
        self.text_document.uri.as_str()
    }
}

impl HasTextDocument for FoldingRangeParams {
    fn text_document_uri(&self) -> &str {
        self.text_document.uri.as_str()
    }
}

/// 「带 textDocument + position」的请求参数。
pub(crate) trait HasTextDocumentPosition {
    fn text_document_uri(&self) -> &str;
    fn position(&self) -> lsp_types::Position;
}

impl HasTextDocumentPosition for HoverParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position_params
            .text_document
            .uri
            .as_str()
    }
    fn position(&self) -> lsp_types::Position {
        self.text_document_position_params.position
    }
}

impl HasTextDocumentPosition for CompletionParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position.text_document.uri.as_str()
    }
    fn position(&self) -> lsp_types::Position {
        self.text_document_position.position
    }
}

impl HasTextDocumentPosition for SignatureHelpParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position_params
            .text_document
            .uri
            .as_str()
    }
    fn position(&self) -> lsp_types::Position {
        self.text_document_position_params.position
    }
}

impl HasTextDocumentPosition for GotoDefinitionParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position_params
            .text_document
            .uri
            .as_str()
    }
    fn position(&self) -> lsp_types::Position {
        self.text_document_position_params.position
    }
}

impl HasTextDocumentPosition for ReferenceParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position.text_document.uri.as_str()
    }
    fn position(&self) -> lsp_types::Position {
        self.text_document_position.position
    }
}

impl HasTextDocumentPosition for RenameParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position.text_document.uri.as_str()
    }
    fn position(&self) -> lsp_types::Position {
        self.text_document_position.position
    }
}

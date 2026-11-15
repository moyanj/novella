//! 词法级能力：语义高亮与文档内高亮。
//!
//! 两者都只依赖文档缓存的词法结果（`SourceDoc`），不触发语法/语义分析。

use lsp_types::{DocumentHighlight, DocumentHighlightParams, SemanticTokens, SemanticTokensParams};
use serde_json::{Value, json};

use crate::semantic;

use crate::server::Server;

impl Server {
    /// 全量语义高亮：复用缓存里的词法结果，避免每个请求重扫源码。
    ///
    /// 文档未打开或参数不合法时返回空结果而非错误：
    /// 编辑器在文档关闭瞬间仍可能补发请求，报错只会产生噪音。
    pub(crate) fn semantic_tokens(&self, params: Option<&Value>) -> Value {
        let empty = json!({ "data": [] });
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<SemanticTokensParams>(p.clone()).ok())
        else {
            eprintln!("[novella-lsp] 语义 token 请求参数不合法");
            return empty;
        };
        let Some(doc) = self.documents.get(params.text_document.uri.as_str()) else {
            eprintln!(
                "[novella-lsp] 请求了未同步的文档：{}",
                params.text_document.uri.as_str()
            );
            return empty;
        };

        let tokens = SemanticTokens {
            result_id: None,
            data: semantic::encode_with_index(&doc.index, &doc.text, &doc.lexed.tokens),
        };
        serde_json::to_value(tokens).unwrap_or(empty)
    }

    /// 文档内高亮：纯词法判定同名声明与引用（见 [`crate::highlight`]）。
    ///
    /// 参数不合法或文档未同步时返回空数组，客户端据此不点亮任何东西。
    pub(crate) fn document_highlight(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<DocumentHighlightParams>(p.clone()).ok())
        else {
            eprintln!("[novella-lsp] 文档高亮参数不合法");
            return empty;
        };
        let Some(doc) = self.documents.get(
            params
                .text_document_position_params
                .text_document
                .uri
                .as_str(),
        ) else {
            return empty;
        };
        let offset = doc
            .index
            .offset_of(params.text_document_position_params.position);
        let highlights: Vec<DocumentHighlight> =
            crate::highlight::document_highlights(&doc.index, &doc.lexed.tokens, offset);
        serde_json::to_value(highlights).unwrap_or(empty)
    }
}

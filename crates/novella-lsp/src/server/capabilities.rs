//! 能力声明：`initialize` 的响应构造。
//!
//! 这里只描述「服务器支持什么」，不做任何状态变更。

use lsp_types::{
    CompletionOptions, InitializeResult, OneOf, PositionEncodingKind, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensServerCapabilities,
    ServerCapabilities, ServerInfo, SignatureHelpOptions, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions,
};
use serde_json::Value;

use crate::semantic;

use crate::server::Server;

impl Server {
    /// 声明服务器能力。
    ///
    /// 语义高亮只开 `full`：不做 `delta`（薄实现每次全量重扫，实现简单且足够快），
    /// 也不做 `range`（同一原因）。
    ///
    /// 位置编码固定按 UTF-16 回协商结果：`source.rs` 的行表就是 UTF-16 码元前缀和，
    /// 换成 utf-8 / utf-32 需要另一套换算表，目前不提供。客户端未声明
    /// `general.positionEncodings` 时按规范默认 utf-16，同样不报错。
    pub(crate) fn initialize(&mut self, params: Option<&Value>) -> Value {
        // 客户端若声明了 positionEncodings 但不含 utf-16，是能力不匹配：
        // 规范要求 utf-16 必须被支持，出现这种情况按协议违约记录并继续用 utf-16
        let client_encodings = params
            .and_then(|p| p.get("capabilities"))
            .and_then(|c| c.get("general"))
            .and_then(|g| g.get("positionEncodings"))
            .and_then(Value::as_array);
        if let Some(encodings) = client_encodings {
            let supports_utf16 = encodings.iter().any(|e| e.as_str() == Some("utf-16"));
            if !supports_utf16 {
                eprintln!(
                    "[novella-lsp] 客户端未声明 utf-16 位置编码，按协议违约处理并使用 utf-16"
                );
            }
        }

        if let Some(config) = params.and_then(|p| p.get("initializationOptions")) {
            self.options.std_root = config
                .get("stdRoot")
                .and_then(Value::as_str)
                .map(Into::into);
            for (index, key) in ["targetOs", "targetArch", "targetFamily"]
                .iter()
                .enumerate()
            {
                self.cfg_overrides[index] =
                    config.get(key).and_then(Value::as_str).map(str::to_owned);
            }
            if let Some(features) = config.get("features").and_then(Value::as_array) {
                self.options.cfg.features = features
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
            }
            self.entries = config
                .get("entryPaths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(Into::into)
                .collect();
        }
        let capabilities = ServerCapabilities {
            position_encoding: Some(PositionEncodingKind::UTF16),
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                TextDocumentSyncOptions {
                    open_close: Some(true),
                    // 全量同步：客户端每次变更都发送完整文本，服务器无需维护增量区间
                    change: Some(TextDocumentSyncKind::FULL),
                    ..Default::default()
                },
            )),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: SemanticTokensLegend {
                        token_types: semantic::TOKEN_TYPES.to_vec(),
                        token_modifiers: Vec::new(),
                    },
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    range: None,
                    ..Default::default()
                }),
            ),
            document_highlight_provider: Some(OneOf::Left(true)),
            document_symbol_provider: Some(OneOf::Left(true)),
            folding_range_provider: Some(lsp_types::FoldingRangeProviderCapability::Simple(true)),
            hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
            completion_provider: Some(CompletionOptions {
                // 成员补全（`.` 之后）与标识符前缀补全都依赖触发字符
                trigger_characters: Some(vec![".".to_owned(), ":".to_owned()]),
                ..Default::default()
            }),
            signature_help_provider: Some(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
                retrigger_characters: None,
                work_done_progress_options: Default::default(),
            }),
            selection_range_provider: Some(lsp_types::SelectionRangeProviderCapability::Simple(
                true,
            )),
            inlay_hint_provider: Some(lsp_types::OneOf::Left(true)),
            code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Left(true)),
            ..Default::default()
        };
        let result = InitializeResult {
            capabilities,
            server_info: Some(ServerInfo {
                name: "novella-lsp".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        };
        serde_json::to_value(result).unwrap_or(Value::Null)
    }
}

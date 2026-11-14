//! LSP 服务器的消息分发、文档状态与能力实现。
//!
//! 分发器按「一条报文 → 若干条回发报文」建模（[`Server::handle`] 返回
//! `Vec<Value>`）：请求产生一条响应，文档变更通知还会额外推送一条
//! `textDocument/publishDiagnostics`——这是 LSP 里唯一的「服务器主动发消息」场景。
//!
//! 能力分工：
//! - 语义高亮：词法结果经 [`crate::semantic`] 编码（唯一词法事实源是 `novella-lexer`）；
//! - 诊断：词法错误即时推送；语法 / 语义诊断是对**磁盘文件**跑 parser + sema 的结果，
//!   见 [`Server::collect_diagnostics`] 的说明；
//! - 悬停 / 补全 / 签名帮助等位置相关能力：见 [`crate::features`]。

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;

use lsp_types::{
    CodeActionParams, CompletionOptions, CompletionParams, DidChangeTextDocumentParams,
    GotoDefinitionParams, GotoDefinitionResponse, ReferenceParams, RenameParams, WorkspaceEdit,
    DidOpenTextDocumentParams, DocumentHighlight, DocumentHighlightParams, DocumentSymbolParams,
    FoldingRangeParams, HoverParams, InitializeResult, InlayHintParams, OneOf,
    PositionEncodingKind, PublishDiagnosticsParams, SelectionRangeParams, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensParams, SemanticTokensServerCapabilities,
    ServerCapabilities, ServerInfo, SignatureHelpOptions, SignatureHelpParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized,
        Notification, PublishDiagnostics,
    },
    request::{
        CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest,
        FoldingRangeRequest, GotoDefinition, HoverRequest, Initialize, InlayHintRequest,
        References, Rename, Request, SelectionRangeRequest, SemanticTokensFullRequest, Shutdown,
        SignatureHelpRequest,
    },
};
use serde_json::{Value, json};

use novella_sema::ModuleId;

use crate::{
    diagnostics, document::SourceDoc, protocol, semantic, source::SemanticIndex,
};

/// JSON-RPC 的「方法不存在」错误码。
const METHOD_NOT_FOUND: i32 = -32601;

/// 服务器状态。
#[derive(Default)]
pub struct Server {
    /// 已打开文档：URI → 解析快照。全量同步策略下每次变更直接重建，
    /// 因此无需维护增量区间。
    documents: HashMap<String, SourceDoc>,
    /// 最近一次语义分析结果（引用绑定表 / 类型摘要），
    /// definition / references / rename / 类型层级共用；
    /// 每次对 `file://` 文档推送诊断时重建。
    analysis: Option<novella_sema::SemaResult>,
    /// 是否已收到 `shutdown` 请求（决定 `exit` 通知的退出码）。
    shutdown_requested: bool,
    /// 收到 `exit` 后置位，主循环据此结束。
    exit_code: Option<i32>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// 处理一条报文，返回需要写回的**全部**报文（按顺序）。
    ///
    /// 一条请求恒产生一条响应；通知通常不回复，唯独文档同步会附带诊断推送。
    pub fn handle(&mut self, message: &Value) -> Vec<Value> {
        // 无 method 字段：客户端对服务器请求的响应。薄实现不主动发起请求，
        // 因此这里没有可匹配的待处理请求，直接忽略。
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Vec::new();
        };
        let params = message.get("params");

        match message.get("id") {
            // 带 id 是请求，必须回复
            Some(id) => vec![self.handle_request(id, method, params)],
            // 不带 id 是通知，按规范不得回复（诊断推送除外）
            None => self.handle_notification(method, params),
        }
    }

    /// 进程退出码：`exit` 通知处理后才有值。
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    // ---------- 请求 ----------

    fn handle_request(&mut self, id: &Value, method: &str, params: Option<&Value>) -> Value {
        match method {
            Initialize::METHOD => success(id, self.initialize(params)),
            Shutdown::METHOD => {
                self.shutdown_requested = true;
                success(id, Value::Null)
            }
            SemanticTokensFullRequest::METHOD => success(id, self.semantic_tokens(params)),
            DocumentHighlightRequest::METHOD => success(id, self.document_highlight(params)),
            DocumentSymbolRequest::METHOD => success(id, self.document_symbols(params)),
            FoldingRangeRequest::METHOD => success(id, self.folding_ranges(params)),
            HoverRequest::METHOD => success(id, self.hover(params)),
            Completion::METHOD => success(id, self.completion(params)),
            SignatureHelpRequest::METHOD => success(id, self.signature_help(params)),
            SelectionRangeRequest::METHOD => success(id, self.selection_ranges(params)),
            InlayHintRequest::METHOD => success(id, self.inlay_hints(params)),
            CodeActionRequest::METHOD => success(id, self.code_actions(params)),
            GotoDefinition::METHOD => success(id, self.definition(params)),
            References::METHOD => success(id, self.references(params)),
            Rename::METHOD => success(id, self.rename(params)),
            other => {
                eprintln!("[novella-lsp] 未支持的方法：{other}");
                error(id, METHOD_NOT_FOUND, &format!("未支持的方法：{other}"))
            }
        }
    }

    // ---------- 通知 ----------

    fn handle_notification(&mut self, method: &str, params: Option<&Value>) -> Vec<Value> {
        match method {
            // 初始化完成握手：无需处理
            Initialized::METHOD => Vec::new(),
            DidOpenTextDocument::METHOD => self.did_open(params),
            DidChangeTextDocument::METHOD => self.did_change(params),
            DidCloseTextDocument::METHOD => self.did_close(params),
            Exit::METHOD => {
                // 规范：未经 shutdown 直接 exit 属异常退出，退出码非 0
                self.exit_code = Some(if self.shutdown_requested { 0 } else { 1 });
                Vec::new()
            }
            other => {
                eprintln!("[novella-lsp] 忽略通知：{other}");
                Vec::new()
            }
        }
    }

    // ---------- 各能力实现 ----------

    /// 声明服务器能力。
    ///
    /// 语义高亮只开 `full`：不做 `delta`（薄实现每次全量重扫，实现简单且足够快），
    /// 也不做 `range`（同一原因）。
    ///
    /// 位置编码固定按 UTF-16 回协商结果：`source.rs` 的行表就是 UTF-16 码元前缀和，
    /// 换成 utf-8 / utf-32 需要另一套换算表，目前不提供。客户端未声明
    /// `general.positionEncodings` 时按规范默认 utf-16，同样不报错。
    fn initialize(&self, params: Option<&Value>) -> Value {
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
                eprintln!("[novella-lsp] 客户端未声明 utf-16 位置编码，按协议违约处理并使用 utf-16");
            }
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

    /// 全量语义高亮：复用缓存里的词法结果，避免每个请求重扫源码。
    ///
    /// 文档未打开或参数不合法时返回空结果而非错误：
    /// 编辑器在文档关闭瞬间仍可能补发请求，报错只会产生噪音。
    fn semantic_tokens(&self, params: Option<&Value>) -> Value {
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
    fn document_highlight(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<DocumentHighlightParams>(p.clone()).ok())
        else {
            eprintln!("[novella-lsp] 文档高亮参数不合法");
            return empty;
        };
        let Some(doc) = self.documents.get(params.text_document_position_params.text_document.uri.as_str())
        else {
            return empty;
        };
        let offset = doc
            .index
            .offset_of(params.text_document_position_params.position);
        let highlights: Vec<DocumentHighlight> = crate::highlight::document_highlights(
            &doc.index,
            &doc.lexed.tokens,
            offset,
        );
        serde_json::to_value(highlights).unwrap_or(empty)
    }

    /// 文档符号（大纲）：嵌套树形态，类的方法挂在类节点下。
    fn document_symbols(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(doc) = self.document_of::<DocumentSymbolParams>(params) else {
            return empty;
        };
        let symbols = crate::features::document_symbols(doc, &doc.outline);
        serde_json::to_value(lsp_types::DocumentSymbolResponse::Nested(symbols)).unwrap_or(empty)
    }

    /// 折叠区间：按行号升序的区间列表。
    fn folding_ranges(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(doc) = self.document_of::<FoldingRangeParams>(params) else {
            return empty;
        };
        serde_json::to_value(crate::features::folding_ranges(&doc.outline)).unwrap_or(empty)
    }

    /// 悬停：关键字说明 / 本文件符号的种类与签名；未命中返回 null。
    fn hover(&self, params: Option<&Value>) -> Value {
        let Some((doc, offset)) = self.document_and_offset::<HoverParams>(params) else {
            return Value::Null;
        };
        match crate::features::hover(doc, &doc.outline, offset) {
            Some(hover) => serde_json::to_value(hover).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    /// 补全：按上下文（普通 / 类型位 / 成员访问）给出候选列表。
    fn completion(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some((doc, offset)) = self.document_and_offset::<CompletionParams>(params) else {
            return empty;
        };
        serde_json::to_value(crate::features::completion(doc, &doc.outline, offset))
            .unwrap_or(empty)
    }

    /// 签名帮助：光标在调用实参列表内时返回被调签名；未命中返回 null。
    fn signature_help(&self, params: Option<&Value>) -> Value {
        let Some((doc, offset)) = self.document_and_offset::<SignatureHelpParams>(params) else {
            return Value::Null;
        };
        match crate::features::signature_help(doc, &doc.outline, offset) {
            Some(help) => serde_json::to_value(help).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    /// 选择范围：每个请求位置一条由内到外的链。
    fn selection_ranges(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<SelectionRangeParams>(p.clone()).ok())
        else {
            return empty;
        };
        let Some(doc) = self.documents.get(params.text_document.uri.as_str()) else {
            return empty;
        };
        let ranges = crate::features::selection_ranges(doc, &doc.outline, &params.positions);
        serde_json::to_value(ranges).unwrap_or(empty)
    }

    /// 内联提示：请求区间内的变量类型提示与实参名提示。
    fn inlay_hints(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<InlayHintParams>(p.clone()).ok())
        else {
            return empty;
        };
        let Some(doc) = self.documents.get(params.text_document.uri.as_str()) else {
            return empty;
        };
        let hints = crate::features::inlay_hints(doc, &doc.outline, params.range);
        serde_json::to_value(hints).unwrap_or(empty)
    }

    /// 代码操作：仅提供「缺失导入」快速修复（见 features::code_actions 的触发条件）。
    fn code_actions(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<CodeActionParams>(p.clone()).ok())
        else {
            return empty;
        };
        let Some(doc) = self.documents.get(params.text_document.uri.as_str()) else {
            return empty;
        };
        let actions = crate::features::code_actions(doc, &doc.outline, &params.context.diagnostics);
        serde_json::to_value(actions).unwrap_or(empty)
    }

    // ---------- 跨模块导航（B 组：依赖 analysis 引用表 / 类型摘要） ----------

    /// 跳转定义：光标处的引用（或声明自身）→ 定义位置。
    fn definition(&self, params: Option<&Value>) -> Value {
        let Some((doc, offset)) = self.document_and_offset::<GotoDefinitionParams>(params) else {
            return Value::Null;
        };
        let Some(analysis) = self.analysis.as_ref() else {
            return Value::Null;
        };
        let Some(module) = self.module_of(doc) else {
            return Value::Null;
        };
        let Some(definition) = crate::nav::definition_at(analysis, module, offset) else {
            return Value::Null;
        };
        match self.location_of(definition.module, definition.span) {
            Some(location) => serde_json::to_value(GotoDefinitionResponse::Scalar(location))
                .unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    /// 查找引用：目标定义的全部引用（按 `context.includeDeclaration` 决定是否含声明）。
    fn references(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<ReferenceParams>(p.clone()).ok())
        else {
            return empty;
        };
        let Some(doc) = self
            .documents
            .get(params.text_document_position.text_document.uri.as_str())
        else {
            return empty;
        };
        let Some(analysis) = self.analysis.as_ref() else {
            return empty;
        };
        let Some(module) = self.module_of(doc) else {
            return empty;
        };
        let offset = doc.index.offset_of(params.text_document_position.position);
        let Some(target) = crate::nav::definition_at(analysis, module, offset) else {
            return empty;
        };
        let locations: Vec<_> =
            crate::nav::references_of(analysis, &target, params.context.include_declaration)
                .into_iter()
                .filter_map(|(module, span)| self.location_of(module, span))
                .collect();
        serde_json::to_value(locations).unwrap_or(empty)
    }

    /// 重命名：声明 + 全部引用替换为新名字（跨模块，按 URI 组织 WorkspaceEdit）。
    fn rename(&self, params: Option<&Value>) -> Value {
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<RenameParams>(p.clone()).ok())
        else {
            return Value::Null;
        };
        // 新名字必须是合法标识符，否则宁可不改（rename 不可恢复地批量改写）
        if params.new_name.is_empty()
            || !params.new_name.chars().all(|ch| ch == '_' || ch.is_alphanumeric())
            || params
                .new_name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_digit())
        {
            return Value::Null;
        }
        let Some(doc) = self
            .documents
            .get(params.text_document_position.text_document.uri.as_str())
        else {
            return Value::Null;
        };
        let Some(analysis) = self.analysis.as_ref() else {
            return Value::Null;
        };
        let Some(module) = self.module_of(doc) else {
            return Value::Null;
        };
        let offset = doc.index.offset_of(params.text_document_position.position);
        let Some(target) = crate::nav::definition_at(analysis, module, offset) else {
            return Value::Null;
        };
        let mut changes: HashMap<lsp_types::Uri, Vec<lsp_types::TextEdit>> = HashMap::new();
        for (module, span) in crate::nav::rename_edits(analysis, &target) {
            let Some(analysis_path) = analysis.modules.get(module).map(|m| m.path.clone()) else {
                continue;
            };
            let Some((uri, text)) = self.module_view(&analysis_path) else {
                continue;
            };
            let index = SemanticIndex::new(&text);
            changes.entry(uri).or_default().push(lsp_types::TextEdit {
                range: index.range(span),
                new_text: params.new_name.clone(),
            });
        }
        serde_json::to_value(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        })
        .unwrap_or(Value::Null)
    }

    // ---------- 参数辅助 ----------

    /// 按「textDocument 参数」取已同步的文档；未同步或参数不合法返回 `None`。
    fn document_of<P>(&self, params: Option<&Value>) -> Option<&SourceDoc>
    where
        P: serde::de::DeserializeOwned + HasTextDocument,
    {
        let params = params.and_then(|p| serde_json::from_value::<P>(p.clone()).ok())?;
        self.documents.get(params.text_document_uri())
    }

    /// 按「textDocument + position」参数取文档与光标字节偏移。
    fn document_and_offset<P>(&self, params: Option<&Value>) -> Option<(&SourceDoc, usize)>
    where
        P: serde::de::DeserializeOwned + HasTextDocumentPosition,
    {
        let params = params.and_then(|p| serde_json::from_value::<P>(p.clone()).ok())?;
        let doc = self.documents.get(params.text_document_uri())?;
        Some((doc, doc.index.offset_of(params.position())))
    }

    /// `didOpen`：记录文档全文并推送诊断。
    fn did_open(&mut self, params: Option<&Value>) -> Vec<Value> {
        let Some(params) = params
            .and_then(|p| serde_json::from_value::<DidOpenTextDocumentParams>(p.clone()).ok())
        else {
            eprintln!("[novella-lsp] didOpen 参数不合法");
            return Vec::new();
        };
        let document = params.text_document;
        let uri = document.uri.as_str().to_owned();
        eprintln!("[novella-lsp] 打开文档：{uri}");
        self.documents.insert(
            uri.clone(),
            SourceDoc::new(document.text, crate::document::file_path_of(&uri)),
        );
        self.publish(&uri)
    }

    /// `didChange`：全量同步下，取最后一次变更的文本整份替换缓存并推送诊断。
    fn did_change(&mut self, params: Option<&Value>) -> Vec<Value> {
        let Some(params) = params
            .and_then(|p| serde_json::from_value::<DidChangeTextDocumentParams>(p.clone()).ok())
        else {
            eprintln!("[novella-lsp] didChange 参数不合法");
            return Vec::new();
        };
        let Some(change) = params.content_changes.last() else {
            return Vec::new();
        };
        let uri = params.text_document.uri.as_str().to_owned();
        let path = match self.documents.get(&uri) {
            Some(doc) => doc.path.clone(),
            None => crate::document::file_path_of(&uri),
        };
        // 未经过 didOpen 的变更：直接插入，避免后续请求拿到空结果
        self.documents
            .insert(uri.clone(), SourceDoc::new(change.text.clone(), path));
        self.publish(&uri)
    }

    /// `didClose`：丢弃文档缓存，并清空客户端侧的诊断。
    fn did_close(&mut self, params: Option<&Value>) -> Vec<Value> {
        let Some(uri) = params
            .and_then(|p| p.get("textDocument"))
            .and_then(|d| d.get("uri"))
            .and_then(Value::as_str)
        else {
            return Vec::new();
        };
        self.documents.remove(uri);
        eprintln!("[novella-lsp] 关闭文档：{uri}");
        // 文档关闭后旧诊断必须显式清空，否则客户端会一直显示残留错误
        vec![publish_diagnostics(uri, Vec::new(), None)]
    }

    /// 为一篇文档生成 `publishDiagnostics` 报文的参数。
    fn publish(&mut self, uri: &str) -> Vec<Value> {
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        let mut published =
            diagnostics::convert_all(&doc.index, &doc.text, &doc.lex_diagnostics());

        // 语法 / 语义诊断需要完整模块图（导入闭包），仅 `file://` 文档可触发；
        // `untitled:` 文档没有路径，只推送词法诊断。
        let Some(path) = doc.path.clone() else {
            return vec![publish_diagnostics(uri, published, None)];
        };

        // C2：所有打开文档的内存缓冲经 OverlayLoader 参与分析——
        // 入口文档的未保存修改直接进入模块图，诊断位置换算可复用文档自身的
        // SemanticIndex（overlay 保证分析文本与缓冲一致）。
        let overlay = self
            .documents
            .values()
            .filter_map(|d| {
                d.path
                    .clone()
                    .map(|path| (std::path::PathBuf::from(path), d.text.clone()))
            })
            .collect();
        let mut loader = novella_sema::OverlayLoader::new(overlay);
        let result = novella_sema::analyze_with(&mut loader, Path::new(&path));

        let entry_key = novella_sema::module_key(Path::new(&path));
        if let Some(module) = result
            .modules
            .iter()
            .find(|module| module.path == entry_key)
        {
            published.extend(diagnostics::convert_all(
                &doc.index,
                &doc.text,
                &module.diagnostics,
            ));
        }
        self.analysis = Some(result);
        vec![publish_diagnostics(uri, published, None)]
    }

    /// 取某模块的文本与偏移索引：打开中的文档用缓存，否则读磁盘。
    ///
    /// 供 definition / references / rename / 类型层级把 `(模块, span)`
    /// 换算成 LSP Location。
    fn module_view(&self, module_path: &Path) -> Option<(lsp_types::Uri, String)> {
        let key = novella_sema::module_key(module_path);
        for doc in self.documents.values() {
            let Some(path) = doc.path.as_deref() else {
                continue;
            };
            if novella_sema::module_key(Path::new(path)) == key {
                let uri = format!("file://{path}").parse().ok()?;
                return Some((uri, doc.text.clone()));
            }
        }
        let text = std::fs::read_to_string(module_path).ok()?;
        let uri = format!("file://{}", module_path.display()).parse().ok()?;
        Some((uri, text))
    }

    /// 光标所在文档对应的模块 id（无分析结果或文档无路径时为 None）。
    fn module_of(&self, doc: &SourceDoc) -> Option<ModuleId> {
        let path = doc.path.as_deref()?;
        let key = novella_sema::module_key(Path::new(path));
        self.analysis
            .as_ref()?
            .modules
            .iter()
            .position(|module| module.path == key)
    }

    /// `(模块, span)` → LSP Location。
    fn location_of(&self, module: ModuleId, span: novella_core::Span) -> Option<lsp_types::Location> {
        let analysis = self.analysis.as_ref()?;
        let path = &analysis.modules.get(module)?.path;
        let (uri, text) = self.module_view(path)?;
        let index = SemanticIndex::new(&text);
        Some(lsp_types::Location {
            uri,
            range: index.range(span),
        })
    }
}

/// 「带 textDocument 字段」的请求参数。
///
/// lsp-types 对这类参数没有公共 trait（字段名也不统一），这里用本地 trait
/// 收敛 `document_of` / `document_and_offset` 两种取法，避免每个处理器
/// 重复一遍「解析参数 → 查文档 → 换算偏移」。
trait HasTextDocument {
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
trait HasTextDocumentPosition {
    fn text_document_uri(&self) -> &str;
    fn position(&self) -> lsp_types::Position;
}

impl HasTextDocumentPosition for HoverParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position_params.text_document.uri.as_str()
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
        self.text_document_position_params.text_document.uri.as_str()
    }
    fn position(&self) -> lsp_types::Position {
        self.text_document_position_params.position
    }
}

impl HasTextDocumentPosition for GotoDefinitionParams {
    fn text_document_uri(&self) -> &str {
        self.text_document_position_params.text_document.uri.as_str()
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

/// 构造 `textDocument/publishDiagnostics` 通知。
fn publish_diagnostics(
    uri: &str,
    diagnostics: Vec<lsp_types::Diagnostic>,
    version: Option<i32>,
) -> Value {
    // URI 已在打开文档时由客户端给出并解析成功，这里的解析失败只可能是
    // 服务器自己拼错了字符串——记日志并放弃该次推送，不影响会话
    let params = PublishDiagnosticsParams {
        uri: match uri.parse::<lsp_types::Uri>() {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!("[novella-lsp] 诊断 URI 不合法，已跳过推送：{uri}（{e}）");
                return Value::Null;
            }
        },
        diagnostics,
        version,
    };
    json!({
        "jsonrpc": "2.0",
        "method": PublishDiagnostics::METHOD,
        "params": serde_json::to_value(params).unwrap_or(Value::Null),
    })
}

/// 运行服务器主循环，返回进程退出码。
///
/// 逐帧读取请求并串行处理：LSP 的 stdio 通道本身是串行的，
/// 薄实现无需并发或异步。
pub fn run<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<i32> {
    let mut server = Server::new();
    eprintln!("[novella-lsp] 服务已启动");

    while let Some(text) = protocol::read_message(reader)? {
        let message: Value = match serde_json::from_str(&text) {
            Ok(message) => message,
            // 单条报文损坏不应终止整个会话，记录后继续读下一帧
            Err(e) => {
                eprintln!("[novella-lsp] 报文不是合法 JSON，已跳过：{e}");
                continue;
            }
        };

        for response in server.handle(&message) {
            protocol::write_message(writer, &response)?;
        }
        if let Some(code) = server.exit_code() {
            eprintln!("[novella-lsp] 收到 exit，退出码 {code}");
            return Ok(code);
        }
    }

    // 客户端关闭了输入流（正常终止路径之一）
    eprintln!("[novella-lsp] 输入流结束，服务退出");
    Ok(0)
}

/// 构造成功响应。
fn success(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// 构造错误响应。
fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(server: &mut Server, uri: &str, text: &str) -> Vec<Value> {
        server.handle(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "novella",
                    "version": 1,
                    "text": text,
                },
            },
        }))
    }

    /// 完整闭环：初始化 → 打开文档 → 请求语义 token。
    #[test]
    fn initialize_open_and_semantic_tokens() {
        let mut server = Server::new();

        // initialize：应返回能力声明与 legend
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {},
        }));
        assert_eq!(responses.len(), 1, "请求只产生一条响应");
        let capabilities = &responses[0]["result"]["capabilities"];
        assert_eq!(
            capabilities["textDocumentSync"]["change"], 1,
            "应为全量同步"
        );
        assert_eq!(
            capabilities["semanticTokensProvider"]["legend"]["tokenTypes"][0],
            "comment"
        );

        // didOpen 是通知：不产生响应，但会推送一条诊断
        let responses = open(&mut server, "file:///demo.nov", "let x = 1;\n");
        assert_eq!(responses.len(), 1, "didOpen 应推送一次诊断");
        assert_eq!(
            responses[0]["method"], "textDocument/publishDiagnostics",
            "推送方法名"
        );
        // 该文档没有顶层 `func main`：sema 会报 NVL3003（缺少程序入口），
        // 这正是内存缓冲参与分析后的预期行为。
        let diagnostics = responses[0]["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics 应为数组");
        assert_eq!(diagnostics.len(), 1, "只有入口诊断");
        assert_eq!(diagnostics[0]["code"], "NVL3003");

        // 语义 token：3 个 token（let / = / 1），每个 5 个整数
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": "file:///demo.nov" } },
        }));
        let data = responses[0]["result"]["data"]
            .as_array()
            .expect("data 应为数组");
        assert_eq!(data.len(), 15, "3 个 token × 5 个整数");
    }

    /// 词法错误应在 didOpen / didChange 时推送到客户端。
    #[test]
    fn lex_error_is_published() {
        let mut server = Server::new();
        let responses = open(&mut server, "file:///bad.nov", "let x = `;\n");
        let published = responses[0]["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics 应为数组");
        // 词法诊断排最前，其后是语法恢复与语义诊断
        assert_eq!(published[0]["code"], "NVL1001");
        assert_eq!(published[0]["source"], "novella");
        assert_eq!(published[0]["range"]["start"]["line"], 0);
        assert!(published.len() >= 1);
    }

    /// 未同步的文档请求语义 token 应返回空结果而不是报错。
    #[test]
    fn unknown_document_returns_empty() {
        let mut server = Server::new();
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": "file:///missing.nov" } },
        }));
        assert_eq!(
            responses[0]["result"]["data"].as_array().map(Vec::len),
            Some(0)
        );
    }

    /// 未支持的方法按 JSON-RPC 返回 -32601。
    #[test]
    fn unsupported_request_returns_method_not_found() {
        let mut server = Server::new();
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "workspace/symbol",
            "params": {},
        }));
        assert_eq!(responses[0]["error"]["code"], METHOD_NOT_FOUND);
    }

    /// 未经 shutdown 的 exit 应以非 0 码退出；正常流程为 0。
    #[test]
    fn exit_code_reflects_shutdown() {
        // 异常路径：未 shutdown
        let mut server = Server::new();
        server.handle(&json!({ "jsonrpc": "2.0", "method": "exit" }));
        assert_eq!(server.exit_code(), Some(1));

        // 正常路径：先 shutdown 再 exit
        let mut server = Server::new();
        server.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "shutdown" }));
        server.handle(&json!({ "jsonrpc": "2.0", "method": "exit" }));
        assert_eq!(server.exit_code(), Some(0));
    }

    /// didChange 应整份替换文档文本（全量同步语义）。
    #[test]
    fn did_change_replaces_full_text() {
        let mut server = Server::new();
        open(&mut server, "file:///demo.nov", "let a = 1;\n");
        server.handle(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": "file:///demo.nov", "version": 2 },
                "contentChanges": [{ "text": "let b = 2;\n" }],
            },
        }));

        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": "file:///demo.nov" } },
        }));
        // 新文本仍产生 3 个 token；标识符名变化不影响数量
        assert_eq!(
            responses[0]["result"]["data"].as_array().map(Vec::len),
            Some(15)
        );
    }

    /// 位置编码固定回 utf-16；客户端声明了别的编码也不改变。
    #[test]
    fn position_encoding_is_utf16() {
        let mut server = Server::new();
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": { "general": { "positionEncodings": ["utf-8", "utf-16"] } } },
        }));
        assert_eq!(
            responses[0]["result"]["capabilities"]["positionEncoding"],
            "utf-16"
        );
    }

    /// 文档高亮返回同名标识符的全部出现。
    #[test]
    fn document_highlight_lists_occurrences() {
        let mut server = Server::new();
        open(
            &mut server,
            "file:///demo.nov",
            "func main() void {\n    let count = 0;\n    count = count + 1;\n}\n",
        );
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/documentHighlight",
            "params": {
                "textDocument": { "uri": "file:///demo.nov" },
                "position": { "line": 1, "character": 8 },
            },
        }));
        let highlights = responses[0]["result"].as_array().expect("应为数组");
        assert_eq!(highlights.len(), 3, "1 处声明 + 2 处引用");
        assert_eq!(highlights[0]["kind"], 3, "WRITE = 3");
        assert_eq!(highlights[1]["kind"], 2, "READ = 2");
    }

    /// 大纲返回嵌套符号树：方法挂在类下。
    #[test]
    fn document_symbol_returns_nested_tree() {
        let mut server = Server::new();
        open(
            &mut server,
            "file:///demo.nov",
            "class Shape {\n    func area() -> int {\n        return 1;\n    }\n}\n",
        );
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": "file:///demo.nov" } },
        }));
        let symbols = responses[0]["result"].as_array().expect("应为数组");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], "Shape");
        assert_eq!(symbols[0]["kind"], 5, "CLASS = 5");
        let children = symbols[0]["children"].as_array().expect("类应有子符号");
        assert_eq!(children[0]["name"], "area");
        assert_eq!(children[0]["kind"], 6, "METHOD = 6");
    }

    /// 折叠区间按行号升序，单行声明不产生区间。
    #[test]
    fn folding_range_covers_multiline_blocks() {
        let mut server = Server::new();
        open(
            &mut server,
            "file:///demo.nov",
            "func f() {\n    return;\n}\nlet x = 1;\n",
        );
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "textDocument/foldingRange",
            "params": { "textDocument": { "uri": "file:///demo.nov" } },
        }));
        let ranges = responses[0]["result"].as_array().expect("应为数组");
        assert_eq!(ranges.len(), 1, "只有函数体可折叠");
        assert_eq!(ranges[0]["startLine"], 0);
        assert_eq!(ranges[0]["endLine"], 2);
    }

    /// 悬停在关键字上返回说明，空白处返回 null。
    #[test]
    fn hover_returns_keyword_hint() {
        let mut server = Server::new();
        open(&mut server, "file:///demo.nov", "func f() {}\n");
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": "file:///demo.nov" },
                "position": { "line": 0, "character": 1 },
            },
        }));
        let content = &responses[0]["result"]["contents"];
        assert!(
            content.as_str().is_some_and(|text| text.contains("定义函数")),
            "func 的悬停应含说明：{content}"
        );
    }

    /// 补全返回关键字与本文件符号。
    #[test]
    fn completion_lists_keywords_and_symbols() {
        let mut server = Server::new();
        open(&mut server, "file:///demo.nov", "func helper() {}\n");
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": "file:///demo.nov" },
                "position": { "line": 1, "character": 0 },
            },
        }));
        let items = responses[0]["result"].as_array().expect("应为数组");
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"func"), "关键字应出现");
        assert!(labels.contains(&"helper"), "本文件符号应出现");
    }

    /// 光标在调用实参内时返回签名帮助。
    #[test]
    fn signature_help_at_call_site() {
        let mut server = Server::new();
        open(
            &mut server,
            "file:///demo.nov",
            "func add(a: int, b: int) -> int { return a; }\nfunc main() {\n    add(1, 2);\n}\n",
        );
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": "file:///demo.nov" },
                "position": { "line": 2, "character": 10 },
            },
        }));
        let signatures = responses[0]["result"]["signatures"]
            .as_array()
            .expect("应有签名");
        assert_eq!(signatures[0]["label"], "add(a: int, b: int) -> int");
    }

    /// 选择范围由内到外逐级扩大。
    #[test]
    fn selection_range_grows_outward() {
        let mut server = Server::new();
        open(
            &mut server,
            "file:///demo.nov",
            "func main() {\n    let total = 1;\n}\n",
        );
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "textDocument/selectionRange",
            "params": {
                "textDocument": { "uri": "file:///demo.nov" },
                "positions": [{ "line": 1, "character": 8 }],
            },
        }));
        let ranges = responses[0]["result"].as_array().expect("应为数组");
        assert_eq!(ranges.len(), 1);
        // 最内层是标识符，外层是函数，最外层是整份文件
        assert_eq!(ranges[0]["range"]["start"]["line"], 1);
        let parent = ranges[0]["parent"].as_object().expect("应有外层");
        assert_eq!(parent["range"]["start"]["line"], 0);
        assert!(parent["parent"].is_object(), "函数之外还有整份文件");
    }

    /// didClose 应清空客户端诊断，避免残留错误标记。
    #[test]
    fn did_close_clears_diagnostics() {
        let mut server = Server::new();
        open(&mut server, "file:///bad.nov", "let x = `;\n");
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": "file:///bad.nov" } },
        }));
        assert_eq!(responses.len(), 1);
        assert_eq!(
            responses[0]["params"]["diagnostics"].as_array().map(Vec::len),
            Some(0),
            "关闭后诊断必须清空"
        );
    }

    /// B1/B2 夹具：helper 定义函数、main 跨模块调用，两篇文档都在内存里。
    fn open_two_modules(server: &mut Server) {
        open(
            server,
            "file:///nav-test/helper.nov",
            "pub func f() -> int {\n    return 1;\n}\n",
        );
        open(
            server,
            "file:///nav-test/main.nov",
            "import \"helper\";\nfunc main() {\n    let x = helper.f();\n}\n",
        );
    }

    /// B1：跳转到跨模块定义（光标在 `helper.f` 的 f 上）。
    #[test]
    fn definition_jumps_across_modules() {
        let mut server = Server::new();
        open_two_modules(&mut server);
        // 主模块第三行 `    let x = helper.f();`，f 在 UTF-16 列 19
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": "file:///nav-test/main.nov" },
                "position": { "line": 2, "character": 19 },
            },
        }));
        let location = &responses[0]["result"];
        assert_eq!(
            location["uri"], "file:///nav-test/helper.nov",
            "定义应在 helper 模块"
        );
        assert_eq!(location["range"]["start"]["line"], 0);
        assert_eq!(location["range"]["start"]["character"], 10);
    }

    /// B2：references 含声明与调用两处；rename 产出跨文件 WorkspaceEdit。
    #[test]
    fn references_and_rename_cover_both_modules() {
        let mut server = Server::new();
        open_two_modules(&mut server);
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "textDocument/references",
            "params": {
                "textDocument": { "uri": "file:///nav-test/main.nov" },
                "position": { "line": 2, "character": 19 },
                "context": { "includeDeclaration": true },
            },
        }));
        let locations = responses[0]["result"].as_array().expect("引用数组");
        assert_eq!(locations.len(), 2, "声明 + 一处调用");
        let uris: Vec<&str> = locations
            .iter()
            .map(|l| l["uri"].as_str().expect("uri"))
            .collect();
        assert!(uris.contains(&"file:///nav-test/helper.nov"));
        assert!(uris.contains(&"file:///nav-test/main.nov"));

        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "textDocument/rename",
            "params": {
                "textDocument": { "uri": "file:///nav-test/main.nov" },
                "position": { "line": 2, "character": 19 },
                "newName": "g",
            },
        }));
        let changes = responses[0]["result"]["changes"]
            .as_object()
            .expect("changes 表");
        assert_eq!(changes.len(), 2, "两个文件都要改");
        let helper_edits = changes["file:///nav-test/helper.nov"]
            .as_array()
            .expect("helper 编辑");
        assert_eq!(helper_edits[0]["newText"], "g");
    }

    /// 光标不在任何引用/声明上时返回 null 而不是报错。
    #[test]
    fn definition_on_empty_position_is_null() {
        let mut server = Server::new();
        open_two_modules(&mut server);
        let responses = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": "file:///nav-test/main.nov" },
                "position": { "line": 0, "character": 0 },
            },
        }));
        assert_eq!(responses[0]["result"], Value::Null);
    }
}
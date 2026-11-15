//! 报文分发：`handle` 入口与请求/通知路由。
//!
//! 本文件只做路由，不含任何能力实现——各能力见同级 `lexical` / `syntax` /
//! `navigation` 等模块。

use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized,
        Notification,
    },
    request::{
        CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest,
        FoldingRangeRequest, GotoDefinition, HoverRequest, Initialize, InlayHintRequest,
        References, Rename, Request, SelectionRangeRequest, SemanticTokensFullRequest, Shutdown,
        SignatureHelpRequest,
    },
};
use serde_json::Value;

use super::{METHOD_NOT_FOUND, Server, error, success};

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

    pub(crate) fn handle_request(
        &mut self,
        id: &Value,
        method: &str,
        params: Option<&Value>,
    ) -> Value {
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

    pub(crate) fn handle_notification(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Vec<Value> {
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
}

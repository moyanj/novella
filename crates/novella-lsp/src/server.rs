//! 薄 LSP 服务器的消息分发与文档状态管理。
//!
//! 只实现语言服务的最小闭环：初始化、文档同步（全量）、全量语义高亮。
//! 其余请求按 JSON-RPC 规范回 `-32601`（method not found），
//! 其余通知一律忽略——薄实现不发主动请求，也不做诊断/补全/跳转。

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use lsp_types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, InitializeResult, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized,
        Notification,
    },
    request::{Initialize, Request, SemanticTokensFullRequest, Shutdown},
};
use serde_json::{Value, json};

use crate::{protocol, semantic};

/// JSON-RPC 的「方法不存在」错误码。
const METHOD_NOT_FOUND: i32 = -32601;

/// 服务器状态。
#[derive(Default)]
pub struct Server {
    /// 已打开文档：URI → 文本。全量同步策略下每次变更直接替换整份文本，
    /// 因此无需维护增量区间。
    documents: HashMap<String, String>,
    /// 是否已收到 `shutdown` 请求（决定 `exit` 通知的退出码）。
    shutdown_requested: bool,
    /// 收到 `exit` 后置位，主循环据此结束。
    exit_code: Option<i32>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// 处理一条报文，返回需要写回的响应（通知返回 `None`）。
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        // 无 method 字段：客户端对服务器请求的响应。薄实现不主动发起请求，
        // 因此这里没有可匹配的待处理请求，直接忽略（返回 None）。
        let method = message.get("method").and_then(Value::as_str)?;
        let params = message.get("params");

        match message.get("id") {
            // 带 id 是请求，必须回复
            Some(id) => Some(self.handle_request(id, method, params)),
            // 不带 id 是通知，按规范不得回复
            None => {
                self.handle_notification(method, params);
                None
            }
        }
    }

    /// 进程退出码：`exit` 通知处理后才有值。
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    // ---------- 请求 ----------

    fn handle_request(&mut self, id: &Value, method: &str, params: Option<&Value>) -> Value {
        match method {
            Initialize::METHOD => success(id, self.initialize()),
            Shutdown::METHOD => {
                self.shutdown_requested = true;
                success(id, Value::Null)
            }
            SemanticTokensFullRequest::METHOD => success(id, self.semantic_tokens(params)),
            other => {
                eprintln!("[novella-lsp] 未支持的方法：{other}");
                error(id, METHOD_NOT_FOUND, &format!("未支持的方法：{other}"))
            }
        }
    }

    // ---------- 通知 ----------

    fn handle_notification(&mut self, method: &str, params: Option<&Value>) {
        match method {
            // 初始化完成握手：无需处理
            Initialized::METHOD => {}
            DidOpenTextDocument::METHOD => self.did_open(params),
            DidChangeTextDocument::METHOD => self.did_change(params),
            DidCloseTextDocument::METHOD => self.did_close(params),
            Exit::METHOD => {
                // 规范：未经 shutdown 直接 exit 属异常退出，退出码非 0
                self.exit_code = Some(if self.shutdown_requested { 0 } else { 1 });
            }
            other => eprintln!("[novella-lsp] 忽略通知：{other}"),
        }
    }

    // ---------- 各能力实现 ----------

    /// 声明服务器能力。
    ///
    /// 语义高亮只开 `full`：不做 `delta`（薄实现每次全量重扫，实现简单且足够快），
    /// 也不做 `range`（同一原因）。
    fn initialize(&self) -> Value {
        let capabilities = ServerCapabilities {
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

    /// 全量语义高亮：对文档跑一遍词法分析，再编码为语义 token。
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
        let Some(source) = self.documents.get(params.text_document.uri.as_str()) else {
            eprintln!(
                "[novella-lsp] 请求了未同步的文档：{}",
                params.text_document.uri.as_str()
            );
            return empty;
        };

        let lexed = novella_lexer::lexer(source);
        let tokens = SemanticTokens {
            result_id: None,
            data: semantic::encode(source, &lexed.tokens),
        };
        serde_json::to_value(tokens).unwrap_or(empty)
    }

    /// `didOpen`：记录文档全文。
    fn did_open(&mut self, params: Option<&Value>) {
        let Some(params) = params
            .and_then(|p| serde_json::from_value::<DidOpenTextDocumentParams>(p.clone()).ok())
        else {
            eprintln!("[novella-lsp] didOpen 参数不合法");
            return;
        };
        let document = params.text_document;
        eprintln!("[novella-lsp] 打开文档：{}", document.uri.as_str());
        self.documents
            .insert(document.uri.as_str().to_owned(), document.text);
    }

    /// `didChange`：全量同步下，取最后一次变更的文本整份替换。
    fn did_change(&mut self, params: Option<&Value>) {
        let Some(params) = params
            .and_then(|p| serde_json::from_value::<DidChangeTextDocumentParams>(p.clone()).ok())
        else {
            eprintln!("[novella-lsp] didChange 参数不合法");
            return;
        };
        let Some(change) = params.content_changes.last() else {
            return;
        };
        let uri = params.text_document.uri.as_str().to_owned();
        match self.documents.get_mut(&uri) {
            Some(source) => *source = change.text.clone(),
            // 未经过 didOpen 的变更：直接插入，避免后续请求拿到空结果
            None => {
                self.documents.insert(uri, change.text.clone());
            }
        }
    }

    /// `didClose`：丢弃文档缓存。
    fn did_close(&mut self, params: Option<&Value>) {
        let Some(uri) = params
            .and_then(|p| p.get("textDocument"))
            .and_then(|d| d.get("uri"))
            .and_then(Value::as_str)
        else {
            return;
        };
        self.documents.remove(uri);
        eprintln!("[novella-lsp] 关闭文档：{uri}");
    }
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

        if let Some(response) = server.handle(&message) {
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

    /// 完整闭环：初始化 → 打开文档 → 请求语义 token。
    #[test]
    fn initialize_open_and_semantic_tokens() {
        let mut server = Server::new();

        // initialize：应返回能力声明与 legend
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {},
            }))
            .expect("initialize 必须响应");
        let capabilities = &response["result"]["capabilities"];
        assert_eq!(
            capabilities["textDocumentSync"]["change"], 1,
            "应为全量同步"
        );
        assert_eq!(
            capabilities["semanticTokensProvider"]["legend"]["tokenTypes"][0],
            "comment"
        );

        // didOpen 是通知：不得产生响应
        assert!(
            server
                .handle(&json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": "file:///demo.nov",
                            "languageId": "novella",
                            "version": 1,
                            "text": "let x = 1;\n",
                        },
                    },
                }))
                .is_none(),
            "通知不应有响应"
        );

        // 语义 token：3 个 token（let / = / 1），每个 5 个整数
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "textDocument/semanticTokens/full",
                "params": { "textDocument": { "uri": "file:///demo.nov" } },
            }))
            .expect("语义 token 请求必须响应");
        let data = response["result"]["data"]
            .as_array()
            .expect("data 应为数组");
        assert_eq!(data.len(), 15, "3 个 token × 5 个整数");
    }

    /// 未同步的文档请求语义 token 应返回空结果而不是报错。
    #[test]
    fn unknown_document_returns_empty() {
        let mut server = Server::new();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "textDocument/semanticTokens/full",
                "params": { "textDocument": { "uri": "file:///missing.nov" } },
            }))
            .expect("应响应");
        assert_eq!(response["result"]["data"].as_array().map(Vec::len), Some(0));
    }

    /// 未支持的方法按 JSON-RPC 返回 -32601。
    #[test]
    fn unsupported_request_returns_method_not_found() {
        let mut server = Server::new();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "textDocument/hover",
                "params": {},
            }))
            .expect("应响应");
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
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
        server.handle(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///demo.nov",
                    "languageId": "novella",
                    "version": 1,
                    "text": "let a = 1;\n",
                },
            },
        }));
        server.handle(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": "file:///demo.nov", "version": 2 },
                "contentChanges": [{ "text": "let b = 2;\n" }],
            },
        }));

        let response = server
            .handle(&json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "textDocument/semanticTokens/full",
                "params": { "textDocument": { "uri": "file:///demo.nov" } },
            }))
            .expect("应响应");
        // 新文本仍产生 3 个 token；标识符名变化不影响数量
        assert_eq!(
            response["result"]["data"].as_array().map(Vec::len),
            Some(15)
        );
    }
}

//! LSP 服务器的消息分发、文档状态与能力实现。
//!
//! 分发器按「一条报文 → 若干条回发报文」建模（[`Server::handle`] 返回
//! `Vec<Value>`）：请求产生一条响应，文档变更通知还会额外推送一条
//! `textDocument/publishDiagnostics`——这是 LSP 里唯一的「服务器主动发消息」场景。
//!
//! 文件分工：
//! - `dispatch`：`handle` 与请求/通知分派（薄，只做路由）；
//! - `capabilities`：`initialize` 的能力声明；
//! - `lexical`：语义高亮与文档内高亮（只依赖词法结果）；
//! - `syntax`：文档符号 / 折叠 / 悬停 / 补全 / 签名帮助 / 选择范围 / 内联提示 / 快速修复；
//! - `navigation`：跳转定义 / 查找引用 / 重命名（依赖语义分析的引用绑定表）；
//! - `sync`：文档同步与诊断推送；
//! - `query`：参数解码辅助（文档 / 偏移 / 模块 → Location）；
//! - `adapter`：请求参数的两类 trait 适配；
//! - `run`：stdio 主循环与报文包装。

mod adapter;
mod analysis;
mod capabilities;
mod dispatch;
mod lexical;
mod navigation;
mod query;
mod run;
mod sync;
mod syntax;

use std::collections::HashMap;

use lsp_types::notification::{Notification, PublishDiagnostics};
use serde_json::Value;

pub use run::run;

/// JSON-RPC 的「方法不存在」错误码。
const METHOD_NOT_FOUND: i32 = -32601;

/// 服务器状态。
#[derive(Default)]
pub struct Server {
    /// 已打开文档：URI → 解析快照。全量同步策略下每次变更直接重建，
    /// 因此无需维护增量区间。
    documents: HashMap<String, crate::document::SourceDoc>,
    /// 同一输入版本的多个工程快照，保留独立入口与全部导入闭包。
    analyses: Vec<novella_sema::SemaResult>,
    versions: HashMap<String, i32>,
    options: novella_sema::AnalysisOptions,
    entries: Vec<std::path::PathBuf>,
    /// stdio 模式延后语义分析，单元级 handle 仍可同步使用。
    background: bool,
    analysis_requested: bool,
    generation: u64,
    /// 是否已收到 `shutdown` 请求（决定 `exit` 通知的退出码）。
    shutdown_requested: bool,
    /// 收到 `exit` 后置位，主循环据此结束。
    exit_code: Option<i32>,
}

/// 构造成功响应。
pub(crate) fn success(id: &Value, result: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// 构造错误响应。
pub(crate) fn error(id: &Value, code: i32, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// 构造 `textDocument/publishDiagnostics` 通知。
pub(crate) fn publish_diagnostics(
    uri: &str,
    diagnostics: Vec<lsp_types::Diagnostic>,
    version: Option<i32>,
) -> Value {
    // URI 已在打开文档时由客户端给出并解析成功，这里的解析失败只可能是
    // 服务器自己拼错了字符串——记日志并放弃该次推送，不影响会话
    let params = lsp_types::PublishDiagnosticsParams {
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
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": PublishDiagnostics::METHOD,
        "params": serde_json::to_value(params).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        // 未配置为可执行入口的文档按普通模块分析，不要求 main。
        let diagnostics = responses[0]["params"]["diagnostics"]
            .as_array()
            .expect("diagnostics 应为数组");
        assert!(diagnostics.is_empty(), "普通模块无需程序入口");

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
        assert!(!published.is_empty());
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
            content
                .as_str()
                .is_some_and(|text| text.contains("定义函数")),
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
            responses[0]["params"]["diagnostics"]
                .as_array()
                .map(Vec::len),
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
    ///
    /// 期望区间是**标识符 `f` 本身**（helper 第 1 行第 9 列，`pub func ` 占 9 字节），
    /// 而不是整条声明区间：符号表用 `Symbol.name_span` 记录名字，`Definition.span`
    /// 取该区间，编辑器跳转只选中名字。
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
        assert_eq!(
            location["range"]["start"]["character"], 9,
            "应停在名字 `f` 上"
        );
        assert_eq!(location["range"]["end"]["character"], 10);
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

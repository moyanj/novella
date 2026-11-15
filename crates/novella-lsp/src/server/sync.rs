//! 文档同步与诊断推送。
//!
//! 全量同步策略：`didOpen` / `didChange` 直接整份重建 `SourceDoc`，
//! 因此无需维护增量区间。每次同步后立刻重算诊断并推送——这是 LSP 里
//! 唯一的「服务器主动发消息」场景。

use std::path::Path;

use lsp_types::{DidChangeTextDocumentParams, DidOpenTextDocumentParams};
use serde_json::Value;

use crate::{diagnostics, document::SourceDoc, server::publish_diagnostics};

use crate::server::Server;

impl Server {
    /// `didOpen`：记录文档全文并推送诊断。
    pub(crate) fn did_open(&mut self, params: Option<&Value>) -> Vec<Value> {
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
    pub(crate) fn did_change(&mut self, params: Option<&Value>) -> Vec<Value> {
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
    pub(crate) fn did_close(&mut self, params: Option<&Value>) -> Vec<Value> {
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
    pub(crate) fn publish(&mut self, uri: &str) -> Vec<Value> {
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        let mut published = diagnostics::convert_all(&doc.index, &doc.text, &doc.lex_diagnostics());

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

        let entry_key = novella_core::path::module_key(Path::new(&path));
        if let Some(module) = result
            .diagnostics
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
}

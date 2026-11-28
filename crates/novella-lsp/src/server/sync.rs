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
        self.versions.insert(uri.clone(), document.version);
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
        if self
            .versions
            .get(&uri)
            .is_some_and(|version| *version >= params.text_document.version)
        {
            return Vec::new();
        }
        self.versions
            .insert(uri.clone(), params.text_document.version);
        let path = match self.documents.get(&uri) {
            Some(doc) => doc.path.clone(),
            None => crate::document::file_path_of(&uri),
        };
        // 未经过 didOpen 的变更：直接插入，避免后续请求拿到空结果
        self.documents
            .insert(uri.clone(), SourceDoc::new(change.text.clone(), path));
        self.publish(&uri)
    }

    /// `didClose`：丢弃文档缓存，清空该文档的诊断，并刷新其余仍打开的文档。
    ///
    /// 该 URI 已在本次通知里从 `self.documents` 移除，[`Server::publish`] 只推送仍打开
    /// 的文档，因此它不会出现在返回消息里；清空客户端侧诊断由末尾那次显式空推送负责
    /// （否则客户端会一直显示残留错误）。
    pub(crate) fn did_close(&mut self, params: Option<&Value>) -> Vec<Value> {
        let Some(uri) = params
            .and_then(|p| p.get("textDocument"))
            .and_then(|d| d.get("uri"))
            .and_then(Value::as_str)
        else {
            return Vec::new();
        };
        let closed = uri.to_owned();
        self.documents.remove(&closed);
        self.versions.remove(&closed);
        eprintln!("[novella-lsp] 关闭文档：{closed}");
        let mut messages = self.publish(&closed);
        messages.push(publish_diagnostics(&closed, Vec::new(), None));
        messages
    }

    /// 客户端通知磁盘文件变化时刷新快照；清单在每次分析任务中重新读取。
    pub(crate) fn did_change_watched_files(&mut self, params: Option<&Value>) -> Vec<Value> {
        let Some(changes) = params
            .and_then(|params| params.get("changes"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };
        if changes.is_empty() {
            return Vec::new();
        }
        self.publish("")
    }

    /// 重建分析并推送仍打开文档的诊断。
    ///
    /// `focus` 是触发本次发布的文档 URI（`""` 表示批量刷新，如 `didChangeWatchedFiles`），
    /// 仅用于日志。分析是工程级的：依赖文件变化也会影响别的文档，因此推送范围始终覆盖
    /// 全部仍打开的文档，而不是只推 `focus` 一个。
    pub(crate) fn publish(&mut self, focus: &str) -> Vec<Value> {
        self.generation = self.generation.wrapping_add(1);
        self.analyses.clear();
        if self.background {
            self.analysis_requested = true;
            return Vec::new();
        }
        let job = self.analysis_job(Default::default());
        self.analyses = job.execute();
        if !focus.is_empty() {
            eprintln!("[novella-lsp] 发布诊断：focus={focus}");
        }
        self.diagnostic_messages()
    }

    /// 只从当前已提交快照发布诊断，不在此路径触发计算。
    pub(super) fn diagnostic_messages(&self) -> Vec<Value> {
        let mut messages = Vec::new();
        for (uri, doc) in &self.documents {
            let mut published = Vec::new();
            if let Some(path) = doc.path.as_deref() {
                let mut seen = std::collections::HashSet::new();
                for analysis in &self.analyses {
                    if let Some(module) = analysis.modules.find_by_path(Path::new(path)) {
                        let mut module_diagnostics =
                            analysis.diagnostics.modules[module.id].ordered();
                        if module.id == 0 {
                            module_diagnostics.extend(&analysis.diagnostics.global);
                        }
                        for diagnostic in module_diagnostics {
                            let key = serde_json::to_string(diagnostic).expect("诊断可序列化");
                            if seen.insert(key) {
                                published.extend(diagnostics::convert_all(
                                    &doc.index,
                                    &doc.text,
                                    std::slice::from_ref(diagnostic),
                                ));
                            }
                        }
                    }
                }
            } else {
                let mut diagnostics = doc.lex_diagnostics();
                diagnostics.extend(doc.parsed.diagnostics.clone());
                published = diagnostics::convert_all(&doc.index, &doc.text, &diagnostics);
            }
            messages.push(publish_diagnostics(
                uri,
                published,
                self.versions.get(uri).copied(),
            ));
        }
        messages.sort_by_key(|message| {
            message["params"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        });
        messages
    }
}

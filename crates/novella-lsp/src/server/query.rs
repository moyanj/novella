//! 参数辅助：把 JSON 参数解码成「文档 / 光标偏移 / 模块 → Location」。
//!
//! lsp-types 对「带 textDocument 的请求」没有公共 trait（字段名也不统一），
//! 因此这里用本地 trait `HasTextDocument` / `HasTextDocumentPosition` 收敛取法，
//! 避免每个处理器重复「解析参数 → 查文档 → 换算偏移」。

use std::path::Path;

use serde_json::Value;

use novella_sema::ModuleId;

use crate::{document::SourceDoc, source::SemanticIndex};

use super::adapter::{HasTextDocument, HasTextDocumentPosition};
use crate::server::Server;

impl Server {
    /// 按「textDocument 参数」取已同步的文档；未同步或参数不合法返回 `None`。
    pub(crate) fn document_of<P>(&self, params: Option<&Value>) -> Option<&SourceDoc>
    where
        P: serde::de::DeserializeOwned + HasTextDocument,
    {
        let params = params.and_then(|p| serde_json::from_value::<P>(p.clone()).ok())?;
        self.documents.get(params.text_document_uri())
    }

    /// 按「textDocument + position」参数取文档与光标字节偏移。
    pub(crate) fn document_and_offset<P>(
        &self,
        params: Option<&Value>,
    ) -> Option<(&SourceDoc, usize)>
    where
        P: serde::de::DeserializeOwned + HasTextDocumentPosition,
    {
        let params = params.and_then(|p| serde_json::from_value::<P>(p.clone()).ok())?;
        let doc = self.documents.get(params.text_document_uri())?;
        Some((doc, doc.index.offset_of(params.position())))
    }

    /// 取某模块的文本与偏移索引：打开中的文档用缓存，否则读磁盘。
    ///
    /// 供 definition / references / rename / 类型层级把 `(模块, span)`
    /// 换算成 LSP Location。
    pub(crate) fn module_view(&self, module_path: &Path) -> Option<(lsp_types::Uri, String)> {
        let key = novella_core::path::module_key(module_path);
        for doc in self.documents.values() {
            let Some(path) = doc.path.as_deref() else {
                continue;
            };
            if novella_core::path::module_key(Path::new(path)) == key {
                let uri = format!("file://{path}").parse().ok()?;
                return Some((uri, doc.text.clone()));
            }
        }
        let text = std::fs::read_to_string(module_path).ok()?;
        let uri = format!("file://{}", module_path.display()).parse().ok()?;
        Some((uri, text))
    }

    /// 光标所在文档对应的模块 id（无分析结果或文档无路径时为 None）。
    pub(crate) fn module_of(&self, doc: &SourceDoc) -> Option<ModuleId> {
        let path = doc.path.as_deref()?;
        let key = novella_core::path::module_key(Path::new(path));
        self.analysis
            .as_ref()?
            .diagnostics
            .modules
            .iter()
            .position(|module| module.path == key)
    }

    /// `(模块, span)` → LSP Location。
    pub(crate) fn location_of(
        &self,
        module: ModuleId,
        span: novella_core::Span,
    ) -> Option<lsp_types::Location> {
        let analysis = self.analysis.as_ref()?;
        let path = &analysis.diagnostics.modules.get(module)?.path;
        let (uri, text) = self.module_view(path)?;
        let index = SemanticIndex::new(&text);
        Some(lsp_types::Location {
            uri,
            range: index.range(span),
        })
    }
}

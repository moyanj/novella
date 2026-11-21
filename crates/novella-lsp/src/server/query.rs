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

    /// 位置换算只能使用产生语义 span 时的源码，绝不重新读磁盘。
    pub(crate) fn module_view(
        &self,
        analysis: &novella_sema::SemaResult,
        module_path: &Path,
    ) -> Option<(lsp_types::Uri, String)> {
        let module = analysis.modules.find_by_path(module_path)?;
        let uri = format!(
            "file://{}",
            novella_core::path::module_key(module_path).display()
        )
        .parse()
        .ok()?;
        Some((uri, module.source.clone()))
    }

    /// 多入口快照共存；优先采用包含当前模块且覆盖范围最大的工程。
    pub(crate) fn analysis_for(&self, doc: &SourceDoc) -> Option<&novella_sema::SemaResult> {
        let path = Path::new(doc.path.as_deref()?);
        self.analyses
            .iter()
            .filter(|analysis| analysis.modules.find_by_path(path).is_some())
            .max_by_key(|analysis| analysis.modules.modules.len())
    }

    /// 光标所在文档对应的模块 id（无分析结果或文档无路径时为 None）。
    pub(crate) fn module_of(
        &self,
        analysis: &novella_sema::SemaResult,
        doc: &SourceDoc,
    ) -> Option<ModuleId> {
        let path = doc.path.as_deref()?;
        let key = novella_core::path::module_key(Path::new(path));
        analysis
            .diagnostics
            .modules
            .iter()
            .position(|module| novella_core::path::module_key(&module.path) == key)
    }

    /// `(模块, span)` → LSP Location。
    pub(crate) fn location_of(
        &self,
        analysis: &novella_sema::SemaResult,
        module: ModuleId,
        span: novella_core::Span,
    ) -> Option<lsp_types::Location> {
        let path = &analysis.diagnostics.modules.get(module)?.path;
        let (uri, text) = self.module_view(analysis, path)?;
        let index = SemanticIndex::new(&text);
        Some(lsp_types::Location {
            uri,
            range: index.range(span),
        })
    }
}

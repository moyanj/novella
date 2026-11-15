//! 跨模块导航：跳转定义、查找引用、重命名。
//!
//! 依赖最近一次语义分析的引用绑定表（[`novella_sema::BindingTable`]）与
//! 类型摘要，因此只有对 `file://` 文档跑过分析后才可用。

use std::collections::HashMap;

use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, ReferenceParams, RenameParams, WorkspaceEdit,
};
use serde_json::{Value, json};

use crate::source::SemanticIndex;

use crate::server::Server;

impl Server {
    /// 跳转定义：光标处的引用（或声明自身）→ 定义位置。
    pub(crate) fn definition(&self, params: Option<&Value>) -> Value {
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
    pub(crate) fn references(&self, params: Option<&Value>) -> Value {
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
    pub(crate) fn rename(&self, params: Option<&Value>) -> Value {
        let Some(params) =
            params.and_then(|p| serde_json::from_value::<RenameParams>(p.clone()).ok())
        else {
            return Value::Null;
        };
        // 新名字必须是合法标识符，否则宁可不改（rename 不可恢复地批量改写）
        if params.new_name.is_empty()
            || !params
                .new_name
                .chars()
                .all(|ch| ch == '_' || ch.is_alphanumeric())
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
        // `lsp_types::Uri` 内含 String，用途是普通 map 键（从不修改），显式豁免
        #[allow(clippy::mutable_key_type)]
        let mut changes: HashMap<lsp_types::Uri, Vec<lsp_types::TextEdit>> = HashMap::new();
        for (module, span) in crate::nav::rename_edits(analysis, &target) {
            let Some(analysis_path) = analysis
                .diagnostics
                .modules
                .get(module)
                .map(|m| m.path.clone())
            else {
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
}

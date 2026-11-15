//! 语法级能力：文档符号、折叠、悬停、补全、签名帮助、选择范围、内联提示、快速修复。
//!
//! 全部以 [`crate::outline::Outline`]（语法树投影）加光标位置为输入，
//! 实现位于 [`crate::features`]；本文件只负责参数解码与 JSON 包装。

use lsp_types::{
    CodeActionParams, CompletionParams, DocumentSymbolParams, FoldingRangeParams, HoverParams,
    InlayHintParams, SelectionRangeParams, SignatureHelpParams,
};
use serde_json::{Value, json};

use crate::server::Server;

impl Server {
    /// 文档符号（大纲）：嵌套树形态，类的方法挂在类节点下。
    pub(crate) fn document_symbols(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(doc) = self.document_of::<DocumentSymbolParams>(params) else {
            return empty;
        };
        let symbols = crate::features::document_symbols(doc, &doc.outline);
        serde_json::to_value(lsp_types::DocumentSymbolResponse::Nested(symbols)).unwrap_or(empty)
    }

    /// 折叠区间：按行号升序的区间列表。
    pub(crate) fn folding_ranges(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some(doc) = self.document_of::<FoldingRangeParams>(params) else {
            return empty;
        };
        serde_json::to_value(crate::features::folding_ranges(&doc.outline)).unwrap_or(empty)
    }

    /// 悬停：关键字说明 / 本文件符号的种类与签名；未命中返回 null。
    pub(crate) fn hover(&self, params: Option<&Value>) -> Value {
        let Some((doc, offset)) = self.document_and_offset::<HoverParams>(params) else {
            return Value::Null;
        };
        match crate::features::hover(doc, &doc.outline, offset) {
            Some(hover) => serde_json::to_value(hover).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    /// 补全：按上下文（普通 / 类型位 / 成员访问）给出候选列表。
    pub(crate) fn completion(&self, params: Option<&Value>) -> Value {
        let empty = json!([]);
        let Some((doc, offset)) = self.document_and_offset::<CompletionParams>(params) else {
            return empty;
        };
        serde_json::to_value(crate::features::completion(doc, &doc.outline, offset))
            .unwrap_or(empty)
    }

    /// 签名帮助：光标在调用实参列表内时返回被调签名；未命中返回 null。
    pub(crate) fn signature_help(&self, params: Option<&Value>) -> Value {
        let Some((doc, offset)) = self.document_and_offset::<SignatureHelpParams>(params) else {
            return Value::Null;
        };
        match crate::features::signature_help(doc, &doc.outline, offset) {
            Some(help) => serde_json::to_value(help).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    /// 选择范围：每个请求位置一条由内到外的链。
    pub(crate) fn selection_ranges(&self, params: Option<&Value>) -> Value {
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
    pub(crate) fn inlay_hints(&self, params: Option<&Value>) -> Value {
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
    pub(crate) fn code_actions(&self, params: Option<&Value>) -> Value {
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
}

//! `textDocument/codeAction`：目前只提供「缺失导入」快速修复。

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic as LspDiagnostic, NumberOrString,
    Position, Range, TextEdit, WorkspaceEdit,
};

use crate::outline::OutlineKind;

use crate::document::SourceDoc;
use crate::outline::Outline;

/// `textDocument/codeAction`：目前只提供「缺失导入」快速修复。
///
/// 触发条件（全部满足才给修复，宁缺毋滥）：
/// - 诊断码为 NVL3001（名称错误）且消息形态是「未定义的命名空间 `X`（缺少对应 import）」；
/// - 文档是 `file://` 磁盘文件，且同目录下确实存在 `X.nov`——不存在的模块名
///   补了 import 只会换一种错误。
///
/// 修复方式：在最后一条 `import` 之后（无 import 时在文件头）插入
/// `import "X";`。
pub fn code_actions(
    doc: &SourceDoc,
    outline: &Outline,
    diagnostics: &[LspDiagnostic],
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    for diagnostic in diagnostics {
        let Some(ns) = missing_namespace_of(diagnostic) else {
            continue;
        };
        let Some(path) = doc.path.as_deref() else {
            continue;
        };
        let sibling = std::path::Path::new(path)
            .parent()
            .map(|dir| dir.join(format!("{ns}.nov")));
        if !sibling.as_ref().is_some_and(|path| path.exists()) {
            continue;
        }
        let insert_line = outline
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == OutlineKind::Import)
            .map(|symbol| doc.index.position(symbol.span.end).0 + 1)
            .max()
            .unwrap_or(0);
        let position = Position {
            line: insert_line,
            character: 0,
        };
        let edit = TextEdit {
            range: Range {
                start: position,
                end: position,
            },
            new_text: format!("import \"{ns}\";\n"),
        };
        // lsp-types 0.97 的 Uri 没有 from_file_path：拼 file:// 后走 from_str
        let Some(uri) = format!("file://{path}").parse::<lsp_types::Uri>().ok() else {
            continue;
        };
        // `lsp_types::Uri` 内含 String，本就不满足无 `mutable_key_type` 的要求；
        // 用途是普通 map 键（从不修改），此处按告警建议显式豁免
        #[allow(clippy::mutable_key_type)]
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri, vec![edit]);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("导入缺失的模块 \"{ns}\""),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            ..Default::default()
        }));
    }
    actions
}

/// 从诊断里提取「缺少 import 的命名空间名」。
fn missing_namespace_of(diagnostic: &LspDiagnostic) -> Option<&str> {
    if diagnostic.code != Some(NumberOrString::String("NVL3001".to_owned())) {
        return None;
    }
    let message = diagnostic.message.as_str();
    if !message.contains("缺少对应 import") {
        return None;
    }
    // 消息形态：「未定义的命名空间 `X`（缺少对应 import）」——取反引号间的名字
    let start = message.find('`')? + 1;
    let end = message[start..].find('`')? + start;
    Some(&message[start..end])
}

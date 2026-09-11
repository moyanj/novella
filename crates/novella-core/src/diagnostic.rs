use serde::{Deserialize, Serialize};

use crate::Span;

/// 诊断严重级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    /// 错误：导致编译失败
    Error,
    /// 警告：不阻断编译
    Warning,
}

/// 诊断位置：展示为 `文件:行:列`，并用于渲染 caret 指示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticLocation {
    /// 源文件名（展示用，可为空）
    pub file: Option<String>,
    /// 1-based 行号
    pub line: usize,
    /// 0-based 字节列号（渲染时转字符列展示，并 +1 转为 1-based）
    pub column: usize,
    /// 出错源码区间，用于渲染 caret；缺失时 caret 落在 column 处
    pub span: Option<Span>,
}

/// 与阶段无关的通用诊断。
///
/// 由 lexer / parser / sema / codegen 等阶段产出，CLI 统一渲染为
/// rustc 风格错误（`error: <msg>` + `--> 文件:行:列` + 源码行 + caret）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// 核心错误消息
    pub message: String,
    /// 出错位置；无位置时（如文件读取失败）仅渲染消息本身
    pub location: Option<DiagnosticLocation>,
    /// 附加说明，渲染在 caret 行之后
    pub note: Option<String>,
}

impl Diagnostic {
    /// 构造一个无位置的 error 诊断（如 I/O 错误）。
    pub fn error(message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            location: None,
            note: None,
        }
    }

    /// 附加位置信息。
    pub fn with_location(mut self, location: DiagnosticLocation) -> Self {
        self.location = Some(location);
        self
    }

    /// 附加说明。
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

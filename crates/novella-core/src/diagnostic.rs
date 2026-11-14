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

/// 诊断错误码。
///
/// 消息文本会随措辞调整，错误码保持稳定：编辑器 / LSP 客户端据此做快速修复、
/// 文档跳转与测试断言，禁止按消息文本匹配。编号按阶段分段：
///
/// | 段位 | 阶段 | 取值 |
/// |------|------|------|
/// | `NVL1xxx` | 词法 | 1001 非法字符；1002 未闭合字符串 |
/// | `NVL2xxx` | 语法 | 2001 parser 语法错误 |
/// | `NVL3xxx` | 语义 | 3001 名称/导入；3002 类型；3003 声明；3004 unsafe；3009 其他 |
/// | `NVL9xxx` | 工具链 | 9001 I/O |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiagnosticCode {
    /// 非法字符（词法）
    InvalidCharacter,
    /// 未闭合的字符串字面量（词法）
    UnterminatedString,
    /// 语法错误（parser）
    SyntaxError,
    /// 名称解析与导入绑定错误（语义）
    NameError,
    /// 类型检查错误（语义）
    TypeError,
    /// 声明级检查错误（语义）
    DeclarationError,
    /// unsafe 上下文规则错误（语义）
    UnsafeError,
    /// 其他语义错误
    SemanticError,
    /// 工具链 I/O 错误（读文件 / 写文件 / 报文）
    IoError,
}

impl DiagnosticCode {
    /// 稳定错误码文本，形如 `NVL3001`。
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticCode::InvalidCharacter => "NVL1001",
            DiagnosticCode::UnterminatedString => "NVL1002",
            DiagnosticCode::SyntaxError => "NVL2001",
            DiagnosticCode::NameError => "NVL3001",
            DiagnosticCode::TypeError => "NVL3002",
            DiagnosticCode::DeclarationError => "NVL3003",
            DiagnosticCode::UnsafeError => "NVL3004",
            DiagnosticCode::SemanticError => "NVL3009",
            DiagnosticCode::IoError => "NVL9001",
        }
    }
}

impl std::fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 与阶段无关的通用诊断。
///
/// 由 lexer / parser / sema / codegen 等阶段产出，CLI 统一渲染为
/// rustc 风格错误（`error: <msg>` + `--> 文件:行:列` + 源码行 + caret）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// 稳定错误码，见 [`DiagnosticCode`]；未显式设置时为 `None`
    pub code: Option<String>,
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
            code: None,
            message: message.into(),
            location: None,
            note: None,
        }
    }

    /// 构造一个无位置的 warning 诊断。
    pub fn warning(message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            code: None,
            message: message.into(),
            location: None,
            note: None,
        }
    }

    /// 附加稳定错误码。
    pub fn with_code(mut self, code: DiagnosticCode) -> Self {
        self.code = Some(code.to_string());
        self
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 错误码文本稳定，且不会在不同阶段间撞号。
    #[test]
    fn codes_are_stable_and_unique() {
        let all = [
            DiagnosticCode::InvalidCharacter,
            DiagnosticCode::UnterminatedString,
            DiagnosticCode::SyntaxError,
            DiagnosticCode::NameError,
            DiagnosticCode::TypeError,
            DiagnosticCode::DeclarationError,
            DiagnosticCode::UnsafeError,
            DiagnosticCode::SemanticError,
            DiagnosticCode::IoError,
        ];
        let mut seen = std::collections::HashSet::new();
        for code in all {
            assert!(code.as_str().starts_with("NVL"), "{code:?} 应有 NVL 前缀");
            assert_eq!(code.to_string(), code.as_str());
            assert!(seen.insert(code.as_str()), "{code:?} 错误码重复");
        }
        assert_eq!(DiagnosticCode::InvalidCharacter.as_str(), "NVL1001");
    }

    /// 构造器默认不带错误码，`with_code` 才写入。
    #[test]
    fn with_code_sets_field() {
        let diag = Diagnostic::error("坏").with_code(DiagnosticCode::TypeError);
        assert_eq!(diag.code.as_deref(), Some("NVL3002"));
        assert_eq!(Diagnostic::warning("注意").code, None);
    }
}

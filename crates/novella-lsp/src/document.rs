//! 文档缓存：每份打开文件只做一遍词法 + 语法分析，供各能力复用。
//!
//! LSP 的能力请求（语义高亮、悬停、文档符号、折叠……）都以「文档 + 光标位置」为参数，
//! 同一份文档在一次编辑后会被连续问好几种问题。把词法结果、语法树、文档结构与
//! UTF-16 换算表缓存在 [`SourceDoc`] 里，可以避免每次请求都重新扫描整份源码。
//!
//! 缓存随 `didOpen` / `didChange` 重建、随 `didClose` 丢弃；文档在多个能力间
//! 共享，因此字段一律只读暴露。

use novella_ast::program::Program;
use novella_core::{
    diagnostic::Diagnostic,
    token::{Token, TokenKind},
};
use novella_lexer::LexerResult;
use novella_parser::ParseResult;

use crate::{
    outline::{Outline, outline},
    source::SemanticIndex,
};

/// 单份文档的解析快照。
///
/// 构造时一次性完成「词法 → 过滤注释 → 语法 → 文档结构」，后续所有能力请求
/// 都只在这份快照上查询；`didChange` 时整体重建。
pub struct SourceDoc {
    /// 文档全文（与客户端同步的版本一致）
    pub text: String,
    /// 文件系统路径（`file://` URI 解码所得）；`untitled:` 等非文件 URI 为 `None`
    pub path: Option<String>,
    /// 词法结果（含错误与注释 token）
    pub lexed: LexerResult,
    /// 语法结果（注释已在喂给 parser 前过滤）
    pub parsed: ParseResult,
    /// 文档结构：符号大纲 + 折叠区间
    pub outline: Outline,
    /// UTF-16 换算表
    pub index: SemanticIndex,
}

impl std::fmt::Debug for SourceDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceDoc")
            .field("path", &self.path)
            .field("bytes", &self.text.len())
            .field("tokens", &self.lexed.tokens.len())
            .field("syntax_errors", &self.lexed.errors.len())
            .field("syntax_diagnostics", &self.parsed.diagnostics.len())
            .field("symbols", &self.outline.symbols.len())
            .finish()
    }
}

impl SourceDoc {
    /// 为一份新文本建立快照：词法 → 语法 → 文档结构。
    ///
    /// 语法错误不会阻断快照构建：解析器自带恢复，残缺的语法树依旧是
    /// 悬停 / 折叠 / 大纲的可用输入（这也是 LSP 必须「边打字边可用」的前提）。
    pub fn new(text: String, path: Option<String>) -> Self {
        let lexed = novella_lexer::lexer(&text);
        let index = SemanticIndex::new(&text);

        // 注释是正式 token（供高亮消费），但 parser 不感知注释，喂入前过滤
        let tokens = lexed
            .tokens
            .iter()
            .filter(|token| token.kind != TokenKind::LineComment)
            .cloned()
            .collect::<Vec<_>>();
        let mut parser = novella_parser::Parser::new(tokens);
        let parsed = parser.parse();

        let outline = outline(&parsed.program, &text, &index);
        SourceDoc {
            text,
            path,
            lexed,
            parsed,
            outline,
            index,
        }
    }

    /// 语法树。
    pub fn program(&self) -> &Program {
        &self.parsed.program
    }

    /// 词法错误转成内部诊断（供 [`crate::diagnostics`] 统一换算）。
    pub fn lex_diagnostics(&self) -> Vec<Diagnostic> {
        self.lexed
            .errors
            .iter()
            .map(|error| error.to_diagnostic(self.path.as_deref()))
            .collect()
    }

    /// 语法诊断（parser 产出；`file` 由调用方按需补填）。
    #[cfg(test)]
    pub fn syntax_diagnostics(&self) -> &[Diagnostic] {
        &self.parsed.diagnostics
    }

    /// 只含词法阶段的错误列表。
    #[cfg(test)]
    pub fn lex_errors(&self) -> &[novella_lexer::TokenError] {
        &self.lexed.errors
    }

    /// 光标处的 token，语义为 **`[start, end)` 左闭右开**；行尾的最后一列
    /// （`offset == 行末`）归属该行最后一个 token，方便「光标在词尾」的场景。
    ///
    /// 这是编辑器光标归属的通行约定：`1|, 2` 上的光标属于 `1` 而不是逗号。
    /// 若按两端闭区间取最窄 token，落在 `1` 与 `,` 交界时会把逗号当目标，
    /// 悬停 / 签名帮助的实参序号都会错位。
    pub fn token_at(&self, offset: usize) -> Option<&Token> {
        let mut last = None;
        for token in &self.lexed.tokens {
            if token.kind == TokenKind::Eof {
                continue;
            }
            if offset >= token.span.start && offset < token.span.end {
                return Some(token);
            }
            // 记录「右界不超过光标」的最后一个 token，用于行尾列兜底
            if token.span.end <= offset {
                last = Some(token);
            }
        }
        // 行尾列：光标紧跟在 token 之后且该 token 之后就是换行（或文件末尾）
        last.filter(|token| {
            offset == token.span.end
                && self
                    .text
                    .get(offset..)
                    .is_none_or(|rest| rest.starts_with(['\n', '\r']) || rest.is_empty())
        })
    }
}

/// 从 `file://` URI 解出本地路径；非文件 URI 返回 `None`。
///
/// 只处理 LSP 客户端实际会发来的两种形式：`file:///abs/path` 与带百分号转义的
/// `file:///abs/%E4%B8%AD.nv`。不做 Windows 盘符特判——本项目面向 Unix。
pub fn file_path_of(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    Some(decoded)
}

/// 解码 URI 中的 `%XX` 转义（非法序列原样保留）。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &text[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 词法错误应带稳定错误码与位置，供诊断推送直接使用。
    #[test]
    fn lex_error_carries_code_and_span() {
        let doc = SourceDoc::new("let x = `;\n".to_owned(), Some("demo.nv".to_owned()));
        assert_eq!(doc.lex_errors().len(), 1);
        let converted = doc.lex_diagnostics();
        assert_eq!(converted[0].code.as_deref(), Some("NVL1001"));
        assert!(converted[0].location.is_some());
    }

    /// `file://` URI 还原为本地路径，百分号转义需解码。
    #[test]
    fn file_uri_decodes_to_path() {
        assert_eq!(
            file_path_of("file:///home/moyan/a.nv").as_deref(),
            Some("/home/moyan/a.nv")
        );
        assert_eq!(
            file_path_of("file:///home/moyan/%E4%B8%AD%E6%96%87.nv").as_deref(),
            Some("/home/moyan/中文.nv")
        );
        assert_eq!(file_path_of("untitled:Untitled-1"), None);
    }

    /// 快照同时携带大纲与折叠；光标查询取最窄 token。
    #[test]
    fn snapshot_builds_outline_and_finds_token() {
        let source = "func main() {\n    return;\n}\n";
        let doc = SourceDoc::new(source.to_owned(), None);
        assert_eq!(doc.outline.symbols.len(), 1);
        assert!(!doc.outline.folds.is_empty(), "函数体应可折叠");
        let offset = source.find("main").expect("存在名字");
        let token = doc.token_at(offset).expect("光标处有 token");
        assert_eq!(token.slice, "main");
    }

    /// 语法错误不阻断快照构建（LSP 必须边打字边可用）。
    #[test]
    fn syntax_errors_keep_document_usable() {
        let doc = SourceDoc::new("func {\n".to_owned(), None);
        assert!(!doc.syntax_diagnostics().is_empty());
        assert!(doc.token_at(0).is_some());
    }
}

//! novella 语言的薄 LSP 服务器。
//!
//! 只提供**词法级**语义高亮（`textDocument/semanticTokens/full`）与文档同步，
//! 复用 [`novella_lexer`] 作为唯一词法事实源，因此不存在语法规则的双份维护。
//!
//! 不提供诊断、补全、跳转、格式化等能力——它们需要 parser / sema 落地后
//! 再在此之上扩展。
//!
//! 模块划分：
//! - [`protocol`]：stdio 上的 `Content-Length` 报文帧编解码；
//! - [`semantic`]：`TokenKind` → 语义 token 的映射与 UTF-16 偏移编码；
//! - [`server`]：消息分发、能力声明与文档缓存。

pub mod protocol;
pub mod semantic;
pub mod server;

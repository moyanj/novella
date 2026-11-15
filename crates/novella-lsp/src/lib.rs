//! novella 语言的 LSP 服务器。
//!
//! 由薄到厚分三层能力：
//! 1. **词法级**：语义高亮（`textDocument/semanticTokens/full`）、文档高亮
//!    （`textDocument/documentHighlight`）、词法诊断——只依赖 [`novella_lexer`]，
//!    因此与编辑器插件不会出现语法规则的双份维护；
//! 2. **语法/结构化**：文档符号、折叠区间、补全、悬浮、签名帮助——依赖
//!    [`novella_parser`] 产出的 AST；
//! 3. **语义级**：诊断推送（`textDocument/publishDiagnostics`）——按磁盘路径
//!    调用 [`novella_sema`] 的完整管线。
//!
//! 公开面只有 [`server::run`]：其余模块均为内部实现，不对 crate 外暴露，
//! 以免把「逐文件拆细」的结构变成需要长期兼容的 API。
//!
//! 内部模块划分：
//! - `protocol`：stdio 上的 `Content-Length` 报文帧编解码；
//! - `source`：字节偏移 ↔ LSP UTF-16 行列的唯一换算层（行表复用 `novella_core::line_index`）；
//! - `document`：单文档缓存（文本、词法结果、语法树、文档结构、偏移索引）；
//! - `semantic`：`TokenKind` → 语义 token 的映射与编码；
//! - `outline`：AST 深度遍历，抽出符号树与折叠区间；
//! - `features`：内部结构 → lsp-types 的转换层，按能力一文件（符号 / 折叠 / 悬浮 /
//!   补全 / 签名帮助 / 内联提示 / 快速修复 / 选择范围）；
//! - `diagnostics`：通用诊断 → LSP 诊断；
//! - `highlight`：词法级文档内同名标识符高亮；
//! - `nav`：跨模块导航（定义跳转 / 引用 / 重命名 / 类型层级），数据来自
//!   `novella_sema` 的引用绑定表与类型摘要；
//! - `server`：报文分发、能力声明、文档缓存与诊断推送（`dispatch` / `capabilities` /
//!   `lexical` / `syntax` / `navigation` / `sync` / `query` / `adapter` / `run`）。
mod diagnostics;
mod document;
mod features;
mod highlight;
mod nav;
mod outline;
mod protocol;
mod semantic;
pub mod server;
mod source;

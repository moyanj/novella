//! 源码预处理：把 lexer 产生的原始 token 流改写为语法分析可直接消费的形式。
//!
//! 目前只有一类重写：
//!
//! - `from "mod" import x` 中的 `import` 关键字降级为标识符。
//!   文法要求 `import` 单独成行、引入名与命名空间别名都是标识符，
//!   而 lexer 一律把 `import` 产出为 [`TokenKind::Import`]；这里在识别出
//!   合法的 from-import 结构后，将相关 `import` / `from` 之后的关键字
//!   位置改写为 [`TokenKind::Identifier`]，让语法分析统一走标识符路径。
//!
//! 历史上这里曾包含「类型位置的 `=>` 改判为 `->`」的启发式改写；该规则
//! 已被规范否决（类型位唯一箭头是 `->`，`=>` 在类型位直接报错），故已移除。

use novella_core::token::{Token, TokenKind};

/// 对 token 流做上下文相关的预处理，返回改写后的新流。
///
/// 当前规则：
/// - `from "mod" import x` 中的 `import` 降级为 [`TokenKind::Identifier`]。
pub fn preprocess(tokens: Vec<Token>) -> Vec<Token> {
    let mut result = tokens;
    downgrade_from_import(&mut result);
    result
}

/// 把 `from "path" import name...` 结构中的 `import` 关键字改写为标识符。
///
/// 识别条件（与文法 `moduleImport` 对齐）：
/// `From StringLiteral Import`，即 `from` 后紧跟一个字符串字面量、再跟 `import`。
/// 命中后仅改写 `import` 本身；`from` 保留为关键字（[`Parser`] 靠它分派导入语句）。
///
/// [`Parser`]: crate::Parser
fn downgrade_from_import(tokens: &mut [Token]) {
    if tokens.len() < 3 {
        return;
    }
    for i in 0..tokens.len() - 2 {
        let is_from_import = tokens[i].kind == TokenKind::From
            && tokens[i + 1].kind == TokenKind::StringLiteral
            && tokens[i + 2].kind == TokenKind::Import;
        if is_from_import {
            tokens[i + 2].kind = TokenKind::Identifier;
        }
    }
}

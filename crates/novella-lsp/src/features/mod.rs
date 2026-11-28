//! LSP 能力实现：内部结构 → lsp-types 形态的转换层，按能力一个文件。
//!
//! 本模块只做「数据整形」，不持有状态、不发消息：输入是
//! [`crate::document::SourceDoc`]（文档全文、词法结果、语法树、偏移索引）加光标位置，
//! 输出是可直接序列化进 JSON-RPC `result` 的 lsp-types 值。状态与分发见
//! [`crate::server`]。
//!
//! 分文件对应一项 LSP 能力：
//! - `symbols`：`textDocument/documentSymbol`
//! - `folding`：`textDocument/foldingRange`
//! - `hover`：`textDocument/hover`
//! - `completion`：`textDocument/completion`
//! - `signature_help`：`textDocument/signatureHelp`
//! - `inlay_hints`：`textDocument/inlayHint`
//! - `code_actions`：`textDocument/codeAction`
//! - `selection_ranges`：`textDocument/selectionRange`

mod code_actions;
mod completion;
mod folding;
mod hover;
mod inlay_hints;
mod selection_ranges;
mod signature_help;
mod symbols;

pub(crate) use code_actions::code_actions;
pub(crate) use completion::completion;
pub(crate) use folding::folding_ranges;
pub(crate) use hover::hover;
pub(crate) use inlay_hints::inlay_hints;
pub(crate) use selection_ranges::selection_ranges;
pub(crate) use signature_help::signature_help;
pub(crate) use symbols::document_symbols;

#[cfg(test)]
use completion::{CompletionContext, completion_context};

#[cfg(test)]
mod tests {
    use lsp_types::{
        CodeActionOrCommand, Diagnostic as LspDiagnostic, HoverContents, NumberOrString, Position,
        Range, SelectionRange,
    };

    use super::*;
    use crate::document::SourceDoc;
    use crate::features::signature_help::{
        count_top_level_commas, find_matching_paren, split_top_level,
    };

    fn doc(text: &str) -> SourceDoc {
        SourceDoc::new(text.to_owned(), Some("demo.nov".to_owned()))
    }

    /// 文档符号应是嵌套树：方法挂在类下，且选择区间落在名字上。
    #[test]
    fn symbols_are_nested() {
        let source = "class Shape {\n    func area() -> int {\n        return 1;\n    }\n}\n";
        let document = doc(source);
        let outline = &document.outline;
        let symbols = document_symbols(&document, outline);
        assert_eq!(symbols.len(), 1, "顶层只有类");
        let children = symbols[0].children.as_ref().expect("类应有子符号");
        assert_eq!(children[0].name, "area");
        assert_eq!(
            children[0].selection_range.start.character, 9,
            "selectionRange 应落在名字上"
        );
    }

    /// 折叠区间按行号升序。
    #[test]
    fn folding_ranges_are_sorted() {
        let source = "import \"std:io\";\nimport \"std:math\";\n\nfunc f() {\n    return;\n}\n";
        let document = doc(source);
        let outline = &document.outline;
        let ranges = folding_ranges(outline);
        assert!(!ranges.is_empty());
        assert!(
            ranges
                .windows(2)
                .all(|pair| pair[0].start_line <= pair[1].start_line),
            "按起始行升序"
        );
    }

    /// 悬停：关键字给说明，本文件声明的标识符给种类。
    #[test]
    fn hover_explains_keywords_and_symbols() {
        let source = "func helper() {}\nlet value = 1;\n";
        let document = doc(source);
        let outline = &document.outline;
        let keyword = hover(&document, outline, 0).expect("关键字应有悬停");
        assert!(matches!(
            keyword.contents,
            HoverContents::Scalar(lsp_types::MarkedString::String(_))
        ));
        let symbol = hover(&document, outline, 5).expect("标识符应有悬停");
        let HoverContents::Markup(markup) = symbol.contents else {
            panic!("标识符悬停应是 Markdown");
        };
        assert!(
            markup.value.contains("函数"),
            "应说明种类：{}",
            markup.value
        );
    }

    /// 成员访问之后进入成员补全上下文。
    #[test]
    fn dot_triggers_member_completion() {
        let source = "func f() {\n    shape.\n}\n";
        let document = doc(source);
        let offset = source.find(".\n").expect("存在点") + 1;
        assert_eq!(
            completion_context(&document, offset),
            CompletionContext::Member
        );
    }

    /// 签名帮助：光标在实参列表里时给出被调函数签名与实参序号。
    #[test]
    fn signature_help_reports_active_parameter() {
        let source =
            "func add(a: int, b: int) -> int { return a; }\nfunc main() {\n    add(1, 2);\n}\n";
        let document = doc(source);
        let outline = &document.outline;
        // 光标落在第二个实参「2」上（而不是逗号上）：逗号尚未生效时应仍属第一个实参
        let offset = source.rfind('2').expect("存在第二个实参");
        let help = signature_help(&document, outline, offset).expect("调用处应有签名帮助");
        assert_eq!(help.signatures.len(), 1);
        assert!(
            help.signatures[0].label.starts_with("add"),
            "标签应为函数名开头：{}",
            help.signatures[0].label
        );
        assert_eq!(help.active_parameter, Some(1), "光标在第二个实参");
    }

    /// 选择范围由内到外逐级扩大，末端是整个文件。
    #[test]
    fn selection_ranges_grow_outward() {
        let source = "func main() {\n    let total = 1;\n}\n";
        let document = doc(source);
        let outline = &document.outline;
        let ranges = selection_ranges(
            &document,
            outline,
            &[Position {
                line: 1,
                character: 8,
            }],
        );
        assert_eq!(ranges.len(), 1);
        let mut chain: Vec<&SelectionRange> = vec![&ranges[0]];
        while let Some(parent) = chain.last().and_then(|node| node.parent.as_deref()) {
            chain.push(parent);
        }
        assert!(chain.len() >= 3, "至少 token → 函数 → 整份文件三层");
        assert_eq!(
            (chain[0].range.start.line, chain[0].range.end.line),
            (1, 1),
            "最内层是光标处的标识符"
        );
        let outer = chain.last().expect("有最外层");
        assert_eq!(outer.range.start.line, 0, "最外层从文件头开始");
        assert_eq!(
            outer.range.end.line, 3,
            "整份文件含末尾空行（源以换行结尾）"
        );
    }

    /// 类型提示：`let` 无标注且初值是字面量时，名字末尾给 `: int`。
    #[test]
    fn inlay_hints_annotate_literal_variables() {
        let source =
            "func main() {\n    let total = 1;\n    let ratio = 0.5;\n    let name = \"x\";\n}\n";
        let document = doc(source);
        let whole = document.index.range(novella_core::Span {
            start: 0,
            end: source.len(),
        });
        let hints = inlay_hints(&document, &document.outline, whole);
        let labels: Vec<&str> = hints
            .iter()
            .map(|hint| match &hint.label {
                lsp_types::InlayHintLabel::String(text) => text.as_str(),
                _ => "",
            })
            .collect();
        assert!(labels.contains(&": int"), "应提示 int：{labels:?}");
        assert!(labels.contains(&": float"), "应提示 float：{labels:?}");
        assert!(labels.contains(&": string"), "应提示 string：{labels:?}");
        assert!(
            hints
                .iter()
                .all(|hint| hint.kind == Some(lsp_types::InlayHintKind::TYPE)),
            "变量提示均为类型类"
        );
    }

    /// 实参名提示：调用已知函数时给实参标形参名；实参与形参同名时跳过。
    #[test]
    fn inlay_hints_label_call_arguments() {
        let source = "func add(a: int, b: int) -> int {\n    return a;\n}\nfunc main() {\n    let b = 2;\n    add(1, b);\n}\n";
        let document = doc(source);
        let whole = document.index.range(novella_core::Span {
            start: 0,
            end: source.len(),
        });
        let hints = inlay_hints(&document, &document.outline, whole);
        let labels: Vec<&str> = hints
            .iter()
            .filter(|hint| hint.kind == Some(lsp_types::InlayHintKind::PARAMETER))
            .map(|hint| match &hint.label {
                lsp_types::InlayHintLabel::String(text) => text.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(
            labels,
            vec!["a:"],
            "只有 1 对应形参 a；b 同名跳过：{labels:?}"
        );
    }

    /// 快速修复：缺少 import 且同目录存在对应 .nov 文件时才给修复。
    #[test]
    fn code_action_offers_missing_import() {
        // 真实落盘一个 sibling 模块（触发条件要求文件存在）
        let dir = std::env::temp_dir().join(format!(
            "novella-lsp-test-{}-{}",
            std::process::id(),
            "code-action"
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        std::fs::write(dir.join("helper.nov"), "pub func f() {}\n").expect("写 sibling 模块");
        let main = dir.join("main.nov");

        let document = SourceDoc::new(
            "func main() {\n    helper::f();\n}\n".to_owned(),
            Some(main.display().to_string()),
        );
        let diagnostic = LspDiagnostic {
            range: Range::default(),
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("NVL3001".to_owned())),
            source: Some("novella".to_owned()),
            message: "未定义的命名空间 `helper`（缺少对应 import）".to_owned(),
            ..Default::default()
        };
        let actions = code_actions(&document, &document.outline, &[diagnostic]);
        assert_eq!(actions.len(), 1, "应给出一条快速修复");
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("应是 CodeAction");
        };
        assert!(action.title.contains("helper"));
        let edit = action.edit.as_ref().expect("带编辑");
        // `changes` 的键是 lsp_types::Uri：只读遍历，显式豁免 mutable_key_type
        #[allow(clippy::mutable_key_type)]
        let changes = edit.changes.as_ref().expect("按 uri 组织的改动");
        let text_edit = &changes.values().next().expect("一处改动")[0];
        assert_eq!(text_edit.new_text, "import \"helper\";\n");
        assert_eq!(text_edit.range.start.line, 0, "无既有 import 时插在文件头");

        // 不存在的模块名不给修复（补了只会换一种错误）
        let missing = LspDiagnostic {
            range: Range::default(),
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("NVL3001".to_owned())),
            source: Some("novella".to_owned()),
            message: "未定义的命名空间 `nosuch`（缺少对应 import）".to_owned(),
            ..Default::default()
        };
        assert!(
            code_actions(&document, &document.outline, &[missing]).is_empty(),
            "模块文件不存在时不给修复"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A8：三套括号扫描器共用同一套规则——字符串内的括号与逗号不算语法符号。
    ///
    /// 修复前 `find_matching_paren` 只认圆括号且不跳字符串，遇到 `f("(", 1)` 会把
    /// 字符串里的 `(` 当成嵌套深度，实参边界与 `count_top_level_commas` 分叉。
    #[test]
    fn a8_bracket_scanners_agree_on_strings() {
        // `"("` 里的括号不参与深度：第一个 `)` 才是配对的右括号
        assert_eq!(find_matching_paren("\"(\", 1)"), Some(6));
        // 转义引号不结束字符串：`"a\")b"` 整体是一个字面量
        // `text` = `"a\")b", 1)`：字面量整体跳过后第一个深度 0 的 `)` 在下标 10
        assert_eq!(find_matching_paren("\"a\\\")b\", 1)"), Some(10));

        // 逗号扫描同样跳过字符串
        let document = doc("f(\"(\", 1)");
        let text = document.text.clone();
        let open = text.find('(').expect("有左括号");
        let close = text.rfind(')').expect("有右括号");
        assert_eq!(
            count_top_level_commas(&document, open + 1, close),
            1,
            "字符串里的逗号不计入"
        );

        // 切分：字符串里的逗号不切
        let parts = split_top_level("\"a,b\", c");
        assert_eq!(parts, vec!["\"a,b\"".to_owned(), " c".to_owned()]);

        // 空串与非空串的边界
        assert_eq!(split_top_level(""), Vec::<String>::new());
        assert_eq!(split_top_level("()"), vec!["()".to_owned()]);
    }
}

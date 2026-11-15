//! `textDocument/hover`：光标处的符号说明与关键字提示。

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use crate::document::SourceDoc;
use crate::outline::{Outline, OutlineKind};

/// `textDocument/hover`：光标落在关键字 / 标识符 / 字面量上时给出说明。
///
/// 纯「本文件」实现，**不做跨模块解析**：
/// - 关键字 → 一行语义提示；
/// - 标识符 → 若在文档符号表里能找到同名声明，显示其种类与签名；
///   找不到时说明「本文件无声明」（可能来自导入或外部符号）。
///
/// 跨模块的真实类型需要 sema 支撑（后续 definition 能力），这里不假装知道。
pub fn hover(doc: &SourceDoc, outline: &Outline, offset: usize) -> Option<Hover> {
    let token = doc.token_at(offset)?;
    let range = Some(doc.index.range(token.span));

    if let Some(content) = keyword_hint(&token.slice) {
        return Some(Hover {
            contents: HoverContents::Scalar(lsp_types::MarkedString::String(content)),
            range,
        });
    }
    if token.kind != novella_core::token::TokenKind::Identifier {
        return None;
    }

    // 同名遮蔽时取声明位置最靠后的那个（就近定义更可能是用户想问的）
    let symbol = outline
        .symbols
        .iter()
        .filter(|symbol| symbol.name == token.slice)
        .max_by_key(|symbol| symbol.name_span.start);
    let content = match symbol {
        Some(symbol) => {
            let mut lines = vec![format!("`{}` {}", token.slice, describe_kind(symbol.kind))];
            if let Some(detail) = &symbol.detail {
                lines.push(detail.clone());
            }
            lines.join("\n\n")
        }
        None => format!(
            "`{}` 在本文件中没有声明（可能来自导入或外部符号）",
            token.slice
        ),
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: content,
        }),
        range,
    })
}

/// 关键字的悬停说明；非关键字返回 `None`。
fn keyword_hint(text: &str) -> Option<String> {
    let hint = match text {
        "func" => "定义函数：`func name(params) -> Ret { ... }`",
        "class" => "定义类（引用语义，可有父类）：`class Foo(Base) { ... }`",
        "struct" => "定义结构体（值语义，无继承）：`struct Point { x: int; }`",
        "trait" => "定义接口：`trait Drawable { func draw() -> void; }`",
        "type" => "定义类型别名：`type Name = int[];`",
        "import" => "导入模块：`import \"std:math\";`",
        "from" => "具名导入：`from \"mod\" import a, b as c;`",
        "as" => "别名或类型转换：`import a as b` / `expr as Type`",
        "let" => "可重新赋值的变量声明",
        "const" => "不可重赋值的常量声明（必须初始化）",
        "pub" => "可见性：公开（跨模块可见）",
        "pri" => "可见性：仅本文件可见",
        "pkg" => "可见性：包内可见",
        "static" => "成员修饰：静态（属于类型而非实例）",
        "weak" => "成员修饰：弱引用（不参与引用计数）",
        "extern" => "外部函数声明：调用点必须在 `unsafe` 内",
        "unsafe" => "不安全上下文：解引用、指针运算、指针转换须在此块内",
        "async" => "异步函数 / lambda：体内可用 `await`",
        "await" => "等待异步结果，仅允许出现在 `async` 上下文",
        "yield" => "产出值（生成器语义尚未实现）",
        "new" => "构造类实例：`new Foo(args)`（类须有 `__init`）",
        "this" => "当前实例引用（仅类方法内可用）",
        "match" => "模式匹配：`match expr { case pattern: ...; default: ...; }`",
        "is" => "类型判断：`expr is Type`",
        "return" => "从函数返回；非 void 函数必须返回同类型值",
        "while" => "条件为真时重复执行循环体",
        "for" => "两类循环：`for x in expr` 与 `for (init; cond; update)`",
        "try" => "异常处理：`try { ... } catch (e) { ... } finally { ... }`",
        "with" => "资源作用域：`with expr as binding { ... }`（离开时自动释放）",
        _ => return None,
    };
    Some(hint.to_owned())
}

pub(crate) fn describe_kind(kind: OutlineKind) -> &'static str {
    match kind {
        OutlineKind::Function => "函数",
        OutlineKind::Method => "方法",
        OutlineKind::Class => "类",
        OutlineKind::Struct => "结构体",
        OutlineKind::Trait => "trait",
        OutlineKind::TypeAlias => "类型别名",
        OutlineKind::TypeParameter => "类型参数",
        OutlineKind::Parameter => "参数",
        OutlineKind::Field => "字段",
        OutlineKind::Variable => "变量",
        OutlineKind::Import => "导入",
    }
}

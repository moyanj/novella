//! LSP 能力实现：内部结构 → lsp-types 形态的转换层。
//!
//! 本模块只做「数据整形」，不持有状态、不发消息：输入是
//! [`crate::document::SourceDoc`]（文档全文、词法结果、语法树、偏移索引）加光标位置，
//! 输出是可直接序列化进 JSON-RPC `result` 的 lsp-types 值。状态与分发见 [`crate::server`]。
//!
//! 覆盖的能力：
//! - `textDocument/documentSymbol`（[`document_symbols`]）
//! - `textDocument/foldingRange`（[`folding_ranges`]）
//! - `textDocument/hover`（[`hover`]）
//! - `textDocument/completion`（[`completion`]）
//! - `textDocument/signatureHelp`（[`signature_help`]）
//! - `textDocument/selectionRange`（[`selection_ranges`]）

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CompletionItem, CompletionItemKind,
    CompletionResponse, Diagnostic as LspDiagnostic, Documentation, FoldingRange,
    FoldingRangeKind, Hover, HoverContents, InlayHint, InlayHintKind, InlayHintLabel,
    MarkupContent, MarkupKind, NumberOrString, Position, Range, SelectionRange, SignatureHelp,
    SignatureInformation, TextEdit, WorkspaceEdit,
};
use novella_ast::{
    expr::{Expression, LambdaBody, Literal, MatchCaseBody, MatchPattern},
    program::{ClassMember, Item, Program},
    stmt::{BindingPattern, ForKind, IfBranch, Statement},
};
use novella_core::Span;

use crate::{
    ast::{FoldKind, Outline, OutlineKind},
    document::SourceDoc,
};

// ---------- 文档符号 ----------

/// `textDocument/documentSymbol`：把 [`Outline`] 的扁平符号表还原成树。
///
/// LSP 允许返回嵌套（`DocumentSymbol` 树）或扁平（`SymbolInformation`）两种形态；
/// 嵌套形态能让编辑器正确显示「类 → 方法」层级，因此优先返回树。
pub fn document_symbols(doc: &SourceDoc, outline: &Outline) -> Vec<lsp_types::DocumentSymbol> {
    // 先建所有节点的空壳，再按 parent 下标回填孩子
    let mut nodes: Vec<lsp_types::DocumentSymbol> = outline
        .symbols
        .iter()
        .map(|symbol| lsp_types::DocumentSymbol {
            name: symbol.name.clone(),
            detail: symbol.detail.clone(),
            kind: symbol_kind(symbol.kind),
            tags: None,
            #[allow(deprecated)] // lsp-types 0.97 已弃用该字段（改用 tags），但结构体仍需填充
            deprecated: None,
            range: doc.index.range(symbol.span),
            selection_range: doc.index.range(symbol.name_span),
            children: None,
        })
        .collect();

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut roots = Vec::new();
    for (index, symbol) in outline.symbols.iter().enumerate() {
        match symbol.parent {
            Some(parent) if parent < nodes.len() => children[parent].push(index),
            // 父下标越界属于内部错误，按顶层处理（不静默丢弃符号）
            _ => roots.push(index),
        }
    }
    // 逆序回填：孩子下标恒大于父下标，回填父节点时孩子已组装完毕
    for index in (0..nodes.len()).rev() {
        if children[index].is_empty() {
            continue;
        }
        let collected = children[index]
            .iter()
            .map(|&kid| nodes[kid].clone())
            .collect::<Vec<_>>();
        nodes[index].children = Some(collected);
    }

    roots.into_iter().map(|index| nodes[index].clone()).collect()
}

fn symbol_kind(kind: OutlineKind) -> lsp_types::SymbolKind {
    match kind {
        OutlineKind::Function => lsp_types::SymbolKind::FUNCTION,
        OutlineKind::Method => lsp_types::SymbolKind::METHOD,
        OutlineKind::Class => lsp_types::SymbolKind::CLASS,
        OutlineKind::Struct => lsp_types::SymbolKind::STRUCT,
        OutlineKind::Trait => lsp_types::SymbolKind::INTERFACE,
        OutlineKind::TypeAlias => lsp_types::SymbolKind::TYPE_PARAMETER,
        OutlineKind::TypeParameter => lsp_types::SymbolKind::TYPE_PARAMETER,
        OutlineKind::Parameter => lsp_types::SymbolKind::VARIABLE,
        OutlineKind::Field => lsp_types::SymbolKind::FIELD,
        OutlineKind::Variable => lsp_types::SymbolKind::VARIABLE,
        OutlineKind::Import => lsp_types::SymbolKind::MODULE,
    }
}

// ---------- 折叠 ----------

/// `textDocument/foldingRange`：把 [`FoldKind`] 映射到 LSP 折叠种类。
///
/// LSP 的 `FoldingRangeKind` 只有 `comment` / `imports` / `region` 三种语义，
/// 其余结构折叠留空（客户端按普通折叠展示）。
pub fn folding_ranges(outline: &Outline) -> Vec<FoldingRange> {
    let mut ranges = outline
        .folds
        .iter()
        .map(|fold| FoldingRange {
            start_line: fold.start_line,
            start_character: None,
            end_line: fold.end_line,
            end_character: None,
            kind: match fold.kind {
                FoldKind::Comments => Some(FoldingRangeKind::Comment),
                FoldKind::Imports => Some(FoldingRangeKind::Imports),
                _ => None,
            },
            collapsed_text: None,
        })
        .collect::<Vec<_>>();
    // 行号升序，客户端据此稳定渲染折叠标记
    ranges.sort_by_key(|range| (range.start_line, range.end_line));
    ranges.dedup_by_key(|range| (range.start_line, range.end_line));
    ranges
}

// ---------- 悬停 ----------

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

fn describe_kind(kind: OutlineKind) -> &'static str {
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

// ---------- 补全 ----------

/// 补全上下文：由光标前缀决定给哪一类候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionContext {
    /// 普通位置：关键字、基础类型、本文件符号
    General,
    /// `obj.` 之后的成员访问
    Member,
    /// 类型位（`:` / `->` / `as` / `is` 之后）
    Type,
}

/// `textDocument/completion`：按上下文返回候选。
///
/// 候选来源：
/// - 关键字与基础类型：`novella_core::keywords`（唯一事实源）+ AST 的预定义类型集；
/// - 本文件符号：文档符号遍历结果（函数 / 类 / 结构体 / trait / 变量……）；
/// - 成员访问 `obj.`：本文件中所有类 / struct 的字段与方法。
///
/// **已知边界**：成员补全不推导接收者类型，给的是本文件全部成员的并集。
/// 在类型推导接进 LSP 之前，这比返回空列表有用，但候选会偏多。
pub fn completion(doc: &SourceDoc, outline: &Outline, offset: usize) -> CompletionResponse {
    let context = completion_context(doc, offset);
    let mut items = match context {
        CompletionContext::General => {
            let mut items = keyword_items();
            items.extend(predefined_type_items());
            items.extend(symbol_items(outline, &[OutlineKind::Function]));
            items
        }
        CompletionContext::Type => {
            let mut items = predefined_type_items();
            items.extend(symbol_items(
                outline,
                &[
                    OutlineKind::Class,
                    OutlineKind::Struct,
                    OutlineKind::Trait,
                    OutlineKind::TypeAlias,
                    OutlineKind::TypeParameter,
                ],
            ));
            items
        }
        CompletionContext::Member => symbol_items(
            outline,
            &[OutlineKind::Field, OutlineKind::Method, OutlineKind::Parameter],
        ),
    };
    // 同名去重：`int` 既在关键字表也在预定义类型表里
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.clone()));
    CompletionResponse::Array(items)
}

/// 补全关键字（与 `novella_core::keywords::KEYWORDS` 同源，不再手写第二份）。
fn keyword_items() -> Vec<CompletionItem> {
    novella_core::keywords::KEYWORDS
        .iter()
        .map(|entry| CompletionItem {
            label: entry.literal.to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect()
}

/// 预定义基础类型补全（`int` / `float` / `string` …）。
fn predefined_type_items() -> Vec<CompletionItem> {
    let mut items = vec![CompletionItem {
        label: "string".to_owned(),
        kind: Some(CompletionItemKind::TYPE_PARAMETER),
        ..Default::default()
    }];
    items.extend(
        novella_ast::types::PredefinedType::ALL
            .iter()
            .map(|ty| CompletionItem {
                label: ty.spelling().to_owned(),
                kind: Some(CompletionItemKind::TYPE_PARAMETER),
                ..Default::default()
            }),
    );
    items
}

/// 本文件里指定种类的符号转为补全项。
fn symbol_items(outline: &Outline, kinds: &[OutlineKind]) -> Vec<CompletionItem> {
    outline
        .symbols
        .iter()
        .filter(|symbol| kinds.contains(&symbol.kind))
        .map(|symbol| CompletionItem {
            label: symbol.name.clone(),
            kind: Some(match symbol.kind {
                OutlineKind::Function | OutlineKind::Method => CompletionItemKind::FUNCTION,
                OutlineKind::Class => CompletionItemKind::CLASS,
                OutlineKind::Struct => CompletionItemKind::STRUCT,
                OutlineKind::Trait => CompletionItemKind::INTERFACE,
                OutlineKind::TypeAlias | OutlineKind::TypeParameter => {
                    CompletionItemKind::TYPE_PARAMETER
                }
                OutlineKind::Parameter | OutlineKind::Variable => CompletionItemKind::VARIABLE,
                OutlineKind::Field => CompletionItemKind::FIELD,
                OutlineKind::Import => CompletionItemKind::MODULE,
            }),
            detail: symbol.detail.clone(),
            ..Default::default()
        })
        .collect()
}

/// 判定光标处的补全上下文（只看光标所在行的前缀）。
///
/// 规则从窄到宽：`.` 之后是成员；`:` / `->` / 以 `as`、`is` 结尾是类型位；其余为普通位置。
fn completion_context(doc: &SourceDoc, offset: usize) -> CompletionContext {
    let (line, column) = doc.index.position(offset);
    let line_text = doc.index.line_text(&doc.text, line as usize);
    let prefix = &line_text[..byte_of_column(line_text, column)];
    let trimmed = prefix.trim_end();

    if trimmed.ends_with('.') {
        return CompletionContext::Member;
    }
    // `:` 结尾（类型注解）——排除空表构造与三元等不常见情形，这里按类型位优先
    if trimmed.ends_with(':') || trimmed.ends_with("->") {
        return CompletionContext::Type;
    }
    // `as` / `is` 之后也是类型位；用「词边界结尾」判断，避免误判 `alias` / `this`
    for keyword in ["as", "is"] {
        if let Some(rest) = trimmed.strip_suffix(keyword) {
            let boundary_ok = rest
                .chars()
                .next_back()
                .is_none_or(|ch| !is_ident_char(ch));
            if boundary_ok {
                return CompletionContext::Type;
            }
        }
    }
    CompletionContext::General
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// UTF-16 列 → 行内字节偏移。
///
/// 列落在某个字符的中间（代理对后半）时取该字符起点：补全前缀按字符边界切开即可，
/// 切在代理对中间会产生非法 UTF-8。列越界时夹到行尾。
fn byte_of_column(line: &str, column: u32) -> usize {
    let mut utf16 = 0u32;
    for (offset, ch) in line.char_indices() {
        if utf16 + ch.len_utf16() as u32 > column {
            return offset;
        }
        utf16 += ch.len_utf16() as u32;
    }
    line.len()
}

// ---------- 签名帮助 ----------

/// `textDocument/signatureHelp`：光标在调用实参列表内时给出被调函数的签名。
///
/// 在 AST 里找**覆盖光标的最内层** `Call` / `New`，再按被调用者名字在本文件符号表里
/// 取签名（`Outline.detail`）。找不到签名时返回 `None`（客户端不弹框），不伪造。
pub fn signature_help(
    doc: &SourceDoc,
    outline: &Outline,
    offset: usize,
) -> Option<SignatureHelp> {
    let call = find_call_at(doc.program(), offset)?;
    let callee = callee_name(&call.callee)?;
    let symbol = outline.symbols.iter().find(|symbol| {
        symbol.name == callee
            && matches!(
                symbol.kind,
                OutlineKind::Function | OutlineKind::Method | OutlineKind::Class
            )
    })?;
    let detail = symbol.detail.as_deref()?;
    let label = format!("{callee}{detail}");
    let parameters = split_params(detail);

    // 光标落在第几个实参：数第一个实参起点到光标之间的顶层逗号。
    // 光标停在逗号上（尚未进入下一个实参）或更早的位置，都算当前实参。
    let active_parameter = call.args.first().map_or(0, |first| {
        if offset <= first.start {
            0
        } else {
            count_top_level_commas(doc, first.start, offset)
        }
    });

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("`{callee}` {}", describe_kind(symbol.kind)),
            })),
            parameters: Some(parameters),
            active_parameter: Some(active_parameter),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_parameter),
    })
}

/// 一次调用的静态信息（避免在遍历中持有 AST 借用）。
struct CallSite {
    span: Span,
    callee: ExpressionCallee,
    args: Vec<Span>,
}

enum ExpressionCallee {
    Named(String),
    Unknown,
}

fn callee_name(callee: &ExpressionCallee) -> Option<&str> {
    match callee {
        ExpressionCallee::Named(name) => Some(name),
        ExpressionCallee::Unknown => None,
    }
}

/// 在整份程序里找覆盖 `offset` 的最内层调用。
fn find_call_at(program: &Program, offset: usize) -> Option<CallSite> {
    let mut calls = Vec::new();
    collect_program_calls(program, &mut calls);
    calls
        .into_iter()
        .filter(|call| offset >= call.span.start && offset <= call.span.end)
        .min_by_key(|call| call.span.end - call.span.start)
}

fn classify_callee(callee: &Expression) -> ExpressionCallee {
    match callee {
        Expression::Identifier { name, .. } => ExpressionCallee::Named(name.clone()),
        Expression::MemberAccess { name, .. } => ExpressionCallee::Named(name.clone()),
        _ => ExpressionCallee::Unknown,
    }
}

fn collect_program_calls(program: &Program, out: &mut Vec<CallSite>) {
    for item in &program.items {
        match item {
            Item::Function(function) => {
                if let Some(block) = &function.block {
                    collect_statement_calls(block, out);
                }
            }
            Item::Class(class) => collect_member_calls(&class.members, out),
            Item::Struct(struct_item) => collect_member_calls(&struct_item.members, out),
            Item::Trait(trait_item) => collect_member_calls(&trait_item.members, out),
            Item::Statement(statement) => collect_statement_calls(statement, out),
            Item::DecoratorBlock { block, .. } => {
                if let Some(block) = block {
                    for statement in &block.stmts {
                        collect_statement_calls(statement, out);
                    }
                }
            }
            Item::Import(_) | Item::TypeAlias(_) | Item::Decorator(_) => {}
        }
    }
}

fn collect_member_calls(members: &[ClassMember], out: &mut Vec<CallSite>) {
    for member in members {
        match member {
            ClassMember::Field(field) => {
                if let Some(initializer) = &field.initializer {
                    collect_expression_calls(initializer, out);
                }
            }
            ClassMember::Method(method) => {
                if let Some(block) = &method.block {
                    for statement in &block.stmts {
                        collect_statement_calls(statement, out);
                    }
                }
            }
        }
    }
}

fn collect_statement_calls(statement: &Statement, out: &mut Vec<CallSite>) {
    match statement {
        Statement::Variable(variable) => {
            if let Some(value) = &variable.value {
                collect_expression_calls(value, out);
            }
        }
        Statement::VariableList { declarations, .. } => {
            for declaration in declarations {
                if let Some(value) = &declaration.value {
                    collect_expression_calls(value, out);
                }
            }
        }
        Statement::Return { value, .. } | Statement::Yield { value, .. } => {
            if let Some(value) = value {
                collect_expression_calls(value, out);
            }
        }
        Statement::Throw { expr, .. } => collect_expression_calls(expr, out),
        Statement::Block(block) => {
            for statement in &block.stmts {
                collect_statement_calls(statement, out);
            }
        }
        Statement::Unsafe { block, .. } => {
            for statement in &block.stmts {
                collect_statement_calls(statement, out);
            }
        }
        Statement::If(if_statement) => {
            for branch in &if_statement.branches {
                match branch {
                    IfBranch::ElseIf {
                        condition, block, ..
                    } => {
                        collect_expression_calls(condition, out);
                        collect_statement_calls(block, out);
                    }
                    IfBranch::Else { block, .. } => collect_statement_calls(block, out),
                }
            }
        }
        Statement::While {
            condition, body, ..
        } => {
            collect_expression_calls(condition, out);
            collect_statement_calls(body, out);
        }
        Statement::For(for_statement) => {
            if let ForKind::ForLoop(loop_cond) = &for_statement.condition {
                if let Some(condition) = &loop_cond.condition {
                    collect_expression_calls(condition, out);
                }
                if let Some(update) = &loop_cond.update {
                    collect_expression_calls(update, out);
                }
            }
            collect_statement_calls(&for_statement.block, out);
        }
        Statement::Try(try_statement) => {
            for statement in &try_statement.block.stmts {
                collect_statement_calls(statement, out);
            }
            for catch in &try_statement.catches {
                for statement in &catch.block.stmts {
                    collect_statement_calls(statement, out);
                }
            }
            if let Some(finally) = &try_statement.finally {
                for statement in &finally.stmts {
                    collect_statement_calls(statement, out);
                }
            }
        }
        Statement::With(with_statement) => {
            collect_expression_calls(&with_statement.expr, out);
            for statement in &with_statement.block.stmts {
                collect_statement_calls(statement, out);
            }
        }
        Statement::Expression(expression) => collect_expression_calls(expression, out),
        Statement::Empty { .. } | Statement::Break { .. } | Statement::Continue { .. } => {}
    }
}

fn collect_expression_calls(expression: &Expression, out: &mut Vec<CallSite>) {
    match expression {
        Expression::Call {
            callee,
            args,
            span,
            ..
        }
        | Expression::New {
            target: callee,
            args,
            span,
            ..
        } => {
            out.push(CallSite {
                span: *span,
                callee: classify_callee(callee),
                args: args.iter().map(|arg| arg.span()).collect(),
            });
            collect_expression_calls(callee, out);
            for arg in args {
                collect_expression_calls(arg, out);
            }
        }
        Expression::Literal { .. } | Expression::Identifier { .. } | Expression::This { .. } => {}
        Expression::Paren { expr, .. } | Expression::Await { expr, .. } => {
            collect_expression_calls(expr, out)
        }
        Expression::Unary { operand, .. } => collect_expression_calls(operand, out),
        Expression::Tuple { elements, .. } | Expression::ArrayLiteral { elements, .. } => {
            for element in elements {
                collect_expression_calls(element, out);
            }
        }
        Expression::ObjectLiteral { properties, .. }
        | Expression::StructInit { properties, .. } => {
            for property in properties {
                if let Some(value) = &property.value {
                    collect_expression_calls(value, out);
                }
            }
        }
        Expression::Binary { left, right, .. } => {
            collect_expression_calls(left, out);
            collect_expression_calls(right, out);
        }
        Expression::Is { operand, .. } => collect_expression_calls(operand, out),
        Expression::Cast { expr, .. } => collect_expression_calls(expr, out),
        Expression::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
        } => {
            collect_expression_calls(condition, out);
            collect_expression_calls(then_expr, out);
            collect_expression_calls(else_expr, out);
        }
        Expression::Assignment { target, value, .. } => {
            collect_expression_calls(target, out);
            collect_expression_calls(value, out);
        }
        Expression::MemberAccess { target, .. } => collect_expression_calls(target, out),
        Expression::IndexAccess { target, index, .. } => {
            collect_expression_calls(target, out);
            collect_expression_calls(index, out);
        }
        Expression::MacroCall { callee, args, .. } => {
            collect_expression_calls(callee, out);
            for arg in args {
                collect_expression_calls(arg, out);
            }
        }
        Expression::Match(match_expression) => {
            collect_expression_calls(&match_expression.subject, out);
            for case in &match_expression.cases {
                if let MatchPattern::Expr { expr, .. } = &case.pattern {
                    collect_expression_calls(expr, out);
                }
                collect_match_body_calls(&case.body, out);
            }
            if let Some(default) = &match_expression.default {
                collect_match_body_calls(default, out);
            }
        }
        Expression::Lambda(lambda) => match &lambda.body {
            LambdaBody::Expr { expr, .. } => collect_expression_calls(expr, out),
            LambdaBody::Block { block, .. } => {
                for statement in &block.stmts {
                    collect_statement_calls(statement, out);
                }
            }
        },
    }
}

fn collect_match_body_calls(body: &MatchCaseBody, out: &mut Vec<CallSite>) {
    match body {
        MatchCaseBody::Expr { expr, .. } => collect_expression_calls(expr, out),
        MatchCaseBody::Block { block, .. } => {
            for statement in &block.stmts {
                collect_statement_calls(statement, out);
            }
        }
    }
}

/// 数 `start` 到 `end` 之间的顶层逗号（跳过字符串与嵌套括号）。
fn count_top_level_commas(doc: &SourceDoc, start: usize, end: usize) -> u32 {
    let start = start.min(doc.text.len());
    let end = end.min(doc.text.len()).max(start);
    let mut depth = 0i32;
    let mut count = 0u32;
    let mut characters = doc.text[start..end].char_indices().peekable();
    while let Some((_, ch)) = characters.next() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '"' | '`' | '\'' => {
                // 跳过整个字符串字面量，避免把字面量里的逗号实数
                let quote = ch;
                while let Some((_, inner)) = characters.next() {
                    if inner == '\\' {
                        characters.next();
                        continue;
                    }
                    if inner == quote {
                        break;
                    }
                }
            }
            // 切片不含 end 处的字符：光标恰在逗号上时该逗号尚未生效，天然不计入
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

/// 从 `(a: int, b: string) -> int` 里拆出参数说明。
fn split_params(detail: &str) -> Vec<lsp_types::ParameterInformation> {
    let Some(open) = detail.find('(') else {
        return Vec::new();
    };
    let rest = &detail[open + 1..];
    let Some(close) = find_matching_paren(rest) else {
        return Vec::new();
    };
    let inner = &rest[..close];
    if inner.trim().is_empty() {
        return Vec::new();
    }
    split_top_level(inner)
        .into_iter()
        .map(|param| param.trim().to_owned())
        .filter(|param| !param.is_empty())
        .map(|param| lsp_types::ParameterInformation {
            label: lsp_types::ParameterLabel::Simple(param),
            documentation: None,
        })
        .collect()
}

/// 在 `text` 里找到与最外层左括号配对的右括号下标，并相对 `text` 起点返回其位置。
///
/// `text` 是「左括号之后」的剩余部分，因此深度从 1 开始计。
fn find_matching_paren(text: &str) -> Option<usize> {
    let mut depth = 1i32;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// 按顶层逗号切分（忽略嵌套元组 / 泛型里的逗号）。
fn split_top_level(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in text.chars() {
        match ch {
            '(' | '[' | '<' => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' | '>' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

// ---------- 内联提示 ----------

/// `textDocument/inlayHint`：两类纯本文件提示。
///
/// 1. **变量类型提示**：无类型标注且初始化值是字面量的 `let` / `const`
///    （`let x = 1;` → 在名字后提示 `: int`）。只推断字面量的显然类型，
///    其余情形交给 sema（尚未接入）而不是瞎猜；
/// 2. **实参名提示**：调用本文件已知函数时在实参前提示形参名
///    （`add(1, 2)` → `a: 1, b: 2`）。实参本身就是同名标识符时跳过
///    （`add(a, b)` 再提示 `a:` 是噪音）。
pub fn inlay_hints(doc: &SourceDoc, outline: &Outline, range: Range) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    collect_variable_hints(doc, &mut hints);
    collect_argument_hints(doc, outline, &mut hints);
    hints.retain(|hint| {
        hint.position >= range.start && hint.position <= range.end
    });
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    hints
}

/// 字面量 → 显然的类型名；`null` 无显然类型，不提示。
fn literal_type_name(literal: &Literal) -> Option<&'static str> {
    match literal {
        Literal::Boolean(_) => Some("boolean"),
        Literal::String(_) => Some("string"),
        Literal::Number(raw) => {
            // 词法层保留原文：含小数点 / 科学计数的按 float，其余按 int
            if raw.contains(['.', 'e', 'E']) {
                Some("float")
            } else {
                Some("int")
            }
        }
        Literal::Null => None,
    }
}

/// 变量类型提示：绑定名末尾追加 `: <类型>`。
fn variable_hint(doc: &SourceDoc, variable: &novella_ast::stmt::VariableStmt, out: &mut Vec<InlayHint>) {
    // 已有类型标注 / 无初始化值 / 元组解构都不提示
    if variable.ty.is_some() {
        return;
    }
    let Some(value) = &variable.value else {
        return;
    };
    let Some(type_name) = literal_hint_type(value) else {
        return;
    };
    let BindingPattern::Ident { span, .. } = &variable.pattern else {
        return;
    };
    out.push(InlayHint {
        position: doc.index.lsp_position(span.end),
        label: InlayHintLabel::String(format!(": {type_name}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: None,
        data: None,
    });
}

/// 初始化值是字面量（允许一层括号 / 负号包裹）时给出类型名。
fn literal_hint_type(expression: &Expression) -> Option<&'static str> {
    match expression {
        Expression::Literal { value, .. } => literal_type_name(value),
        Expression::Paren { expr, .. } => literal_hint_type(expr),
        Expression::Unary { operand, .. } => literal_hint_type(operand),
        _ => None,
    }
}

fn collect_variable_hints(doc: &SourceDoc, out: &mut Vec<InlayHint>) {
    for item in &doc.program().items {
        walk_item_variables(doc, item, out);
    }
}

fn walk_item_variables(doc: &SourceDoc, item: &Item, out: &mut Vec<InlayHint>) {
    match item {
        Item::Function(function) => {
            if let Some(block) = &function.block {
                walk_statement_variables(doc, block, out);
            }
        }
        Item::Class(class) => walk_member_variables(doc, &class.members, out),
        Item::Struct(struct_item) => walk_member_variables(doc, &struct_item.members, out),
        Item::Trait(trait_item) => walk_member_variables(doc, &trait_item.members, out),
        Item::Statement(statement) => walk_statement_variables(doc, statement, out),
        Item::DecoratorBlock { block, .. } => {
            if let Some(block) = block {
                for statement in &block.stmts {
                    walk_statement_variables(doc, statement, out);
                }
            }
        }
        Item::Import(_) | Item::TypeAlias(_) | Item::Decorator(_) => {}
    }
}

fn walk_member_variables(doc: &SourceDoc, members: &[ClassMember], out: &mut Vec<InlayHint>) {
    for member in members {
        if let ClassMember::Method(method) = member {
            if let Some(block) = &method.block {
                for statement in &block.stmts {
                    walk_statement_variables(doc, statement, out);
                }
            }
        }
    }
}

fn walk_statement_variables(doc: &SourceDoc, statement: &Statement, out: &mut Vec<InlayHint>) {
    match statement {
        Statement::Variable(variable) => variable_hint(doc, variable, out),
        Statement::VariableList { declarations, .. } => {
            for declaration in declarations {
                variable_hint(doc, declaration, out);
            }
        }
        Statement::Block(block) => {
            for statement in &block.stmts {
                walk_statement_variables(doc, statement, out);
            }
        }
        Statement::Unsafe { block, .. } => {
            for statement in &block.stmts {
                walk_statement_variables(doc, statement, out);
            }
        }
        Statement::If(if_statement) => {
            for branch in &if_statement.branches {
                match branch {
                    IfBranch::ElseIf { block, .. } | IfBranch::Else { block, .. } => {
                        walk_statement_variables(doc, block, out)
                    }
                }
            }
        }
        Statement::While { body, .. } => walk_statement_variables(doc, body, out),
        Statement::For(for_statement) => {
            walk_statement_variables(doc, &for_statement.block, out)
        }
        Statement::Try(try_statement) => {
            for statement in &try_statement.block.stmts {
                walk_statement_variables(doc, statement, out);
            }
            for catch in &try_statement.catches {
                for statement in &catch.block.stmts {
                    walk_statement_variables(doc, statement, out);
                }
            }
            if let Some(finally) = &try_statement.finally {
                for statement in &finally.stmts {
                    walk_statement_variables(doc, statement, out);
                }
            }
        }
        Statement::With(with_statement) => {
            for statement in &with_statement.block.stmts {
                walk_statement_variables(doc, statement, out);
            }
        }
        _ => {}
    }
}

/// 实参名提示：调用本文件已知函数 / 方法 / 构造时在实参前提示形参名。
fn collect_argument_hints(doc: &SourceDoc, outline: &Outline, out: &mut Vec<InlayHint>) {
    let mut calls = Vec::new();
    collect_program_calls(doc.program(), &mut calls);
    for call in calls {
        let Some(callee) = callee_name(&call.callee) else {
            continue;
        };
        let Some(symbol) = outline.symbols.iter().find(|symbol| {
            symbol.name == callee
                && matches!(
                    symbol.kind,
                    OutlineKind::Function | OutlineKind::Method | OutlineKind::Class
                )
        }) else {
            continue;
        };
        let Some(detail) = symbol.detail.as_deref() else {
            continue;
        };
        let param_names = split_params(detail)
            .into_iter()
            .filter_map(|param| {
                let label = match param.label {
                    lsp_types::ParameterLabel::Simple(label) => label,
                    lsp_types::ParameterLabel::LabelOffsets(_) => return None,
                };
                // 「a: int = 1」→「a」
                Some(label.split(':').next()?.trim().to_owned())
            })
            .collect::<Vec<_>>();
        for (index, arg) in call.args.iter().enumerate() {
            let Some(name) = param_names.get(index) else {
                break;
            };
            // 实参本身就是同名标识符时不提示
            if doc.text.get(arg.start..arg.end) == Some(name.as_str()) {
                continue;
            }
            out.push(InlayHint {
                position: doc.index.lsp_position(arg.start),
                label: InlayHintLabel::String(format!("{name}:")),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }
    }
}

// ---------- 快速修复 ----------

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
        let Some(uri) = format!("file://{path}")
            .parse::<lsp_types::Uri>()
            .ok()
        else {
            continue;
        };
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

// ---------- 选择范围 ----------

/// `textDocument/selectionRange`：由内到外的逐级选择区间。
///
/// 链：光标处 token → 各层覆盖光标的符号声明（按区间从小到大）→ 整个文件。
/// 不做表达式级细分（那需要完整表达式遍历，价值低于复杂度）。
pub fn selection_ranges(
    doc: &SourceDoc,
    outline: &Outline,
    positions: &[Position],
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|position| {
            let offset = doc.index.offset_of(*position);
            let mut chain: Vec<Range> = Vec::new();

            // 最内层是光标所在的那个名字（声明或引用）：AST 不为名字单独记 span，
            // 而符号的 range 覆盖整条声明，所以这里按「光标处的标识符」构造最内层
            if let Some(token) = doc
                .token_at(offset)
                .filter(|token| token.kind == novella_core::token::TokenKind::Identifier)
            {
                chain.push(doc.index.range(token.span));
            }

            let mut covering = outline
                .symbols
                .iter()
                .filter(|symbol| offset >= symbol.span.start && offset <= symbol.span.end)
                .map(|symbol| symbol.span)
                .collect::<Vec<_>>();
            covering.sort_by_key(|span| span.end - span.start);
            covering.dedup();
            for span in covering {
                let range = doc.index.range(span);
                if chain.last() != Some(&range) {
                    chain.push(range);
                }
            }

            let whole = doc.index.range(Span {
                start: 0,
                end: doc.text.len(),
            });
            if chain.last() != Some(&whole) {
                chain.push(whole);
            }

            // chain 由内到外排列；LSP 约定返回最内层节点、parent 指向更大的区间，
            // 因此倒序构造：先建整份文件（无 parent），再逐层向内包
            let mut range: Option<SelectionRange> = None;
            for current in chain.iter().rev() {
                range = Some(SelectionRange {
                    range: *current,
                    parent: range.map(Box::new),
                });
            }
            range.unwrap_or(SelectionRange {
                range: whole,
                parent: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> SourceDoc {
        SourceDoc::new(text.to_owned(), Some("demo.nov".to_owned()))
    }

    /// 文档符号应是嵌套树：方法挂在类下，且选择区间落在名字上。
    #[test]
    fn symbols_are_nested() {
        let source = "class Shape {\n    func area() -> int {\n        return 1;\n    }\n}\n";
        let document = doc(source);
        let outline = &document.outline;
        let symbols = document_symbols(&document, &outline);
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
        let ranges = folding_ranges(&outline);
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
        let keyword = hover(&document, &outline, 0).expect("关键字应有悬停");
        assert!(matches!(
            keyword.contents,
            HoverContents::Scalar(lsp_types::MarkedString::String(_))
        ));
        let symbol = hover(&document, &outline, 5).expect("标识符应有悬停");
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
        let source = "func add(a: int, b: int) -> int { return a; }\nfunc main() {\n    add(1, 2);\n}\n";
        let document = doc(source);
        let outline = &document.outline;
        // 光标落在第二个实参「2」上（而不是逗号上）：逗号尚未生效时应仍属第一个实参
        let offset = source.rfind('2').expect("存在第二个实参");
        let help = signature_help(&document, &outline, offset).expect("调用处应有签名帮助");
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
            &outline,
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
        assert_eq!(outer.range.end.line, 3, "整份文件含末尾空行（源以换行结尾）");
    }

    /// UTF-16 列到字节偏移的换算在行尾越界时夹到行末。
    #[test]
    fn column_conversion_clamps() {
        assert_eq!(byte_of_column("abc", 1), 1);
        assert_eq!(byte_of_column("中文", 0), 0, "列 0 是行首");
        assert_eq!(byte_of_column("中文", 1), 3, "第二个字的 UTF-16 列从第 3 字节开始");
        assert_eq!(byte_of_column("abc", 99), 3);
    }
    /// 类型提示：`let` 无标注且初值是字面量时，名字末尾给 `: int`。
    #[test]
    fn inlay_hints_annotate_literal_variables() {
        let source = "func main() {\n    let total = 1;\n    let ratio = 0.5;\n    let name = \"x\";\n}\n";
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
        assert_eq!(labels, vec!["a:"], "只有 1 对应形参 a；b 同名跳过：{labels:?}");
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

}
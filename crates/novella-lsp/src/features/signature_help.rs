//! `textDocument/signatureHelp`：调用点签名与当前实参定位。

use lsp_types::{Documentation, MarkupContent, MarkupKind, SignatureHelp, SignatureInformation};

use novella_ast::{
    expr::{Expression, LambdaBody, MatchCaseBody, MatchPattern},
    program::{ClassMember, Item, Program},
    stmt::{ForKind, IfBranch, Statement},
};
use novella_core::Span;

use super::hover::describe_kind;
use crate::document::SourceDoc;
use crate::outline::{Outline, OutlineKind};

/// `textDocument/signatureHelp`：光标在调用实参列表内时给出被调函数的签名。
///
/// 在 AST 里找**覆盖光标的最内层** `Call` / `New`，再按被调用者名字在本文件符号表里
/// 取签名（`Outline.detail`）。找不到签名时返回 `None`（客户端不弹框），不伪造。
pub fn signature_help(doc: &SourceDoc, outline: &Outline, offset: usize) -> Option<SignatureHelp> {
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
pub(crate) struct CallSite {
    span: Span,
    pub(crate) callee: ExpressionCallee,
    pub(crate) args: Vec<Span>,
}

pub(crate) enum ExpressionCallee {
    Named(String),
    Unknown,
}

pub(crate) fn callee_name(callee: &ExpressionCallee) -> Option<&str> {
    match callee {
        ExpressionCallee::Named(name) => Some(name),
        ExpressionCallee::Unknown => None,
    }
}

/// 在整份程序里找覆盖 `offset` 的最内层调用。
pub(crate) fn find_call_at(program: &Program, offset: usize) -> Option<CallSite> {
    let mut calls = Vec::new();
    collect_program_calls(program, &mut calls);
    calls
        .into_iter()
        .filter(|call| offset >= call.span.start && offset <= call.span.end)
        .min_by_key(|call| call.span.end - call.span.start)
}

pub(crate) fn classify_callee(callee: &Expression) -> ExpressionCallee {
    match callee {
        Expression::Identifier { name, .. } => ExpressionCallee::Named(name.clone()),
        Expression::MemberAccess { name, .. } => ExpressionCallee::Named(name.clone()),
        _ => ExpressionCallee::Unknown,
    }
}

pub(crate) fn collect_program_calls(program: &Program, out: &mut Vec<CallSite>) {
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

pub(crate) fn collect_member_calls(members: &[ClassMember], out: &mut Vec<CallSite>) {
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

pub(crate) fn collect_statement_calls(statement: &Statement, out: &mut Vec<CallSite>) {
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

pub(crate) fn collect_expression_calls(expression: &Expression, out: &mut Vec<CallSite>) {
    match expression {
        Expression::Call {
            callee, args, span, ..
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

pub(crate) fn collect_match_body_calls(body: &MatchCaseBody, out: &mut Vec<CallSite>) {
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
pub(crate) fn count_top_level_commas(doc: &SourceDoc, start: usize, end: usize) -> u32 {
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
pub(crate) fn split_params(detail: &str) -> Vec<lsp_types::ParameterInformation> {
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
pub(crate) fn find_matching_paren(text: &str) -> Option<usize> {
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
pub(crate) fn split_top_level(text: &str) -> Vec<String> {
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

//! `textDocument/inlayHint`：变量类型提示与调用实参名提示。

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Range};

use novella_ast::{
    expr::{Expression, Literal},
    program::{ClassMember, Item},
    stmt::{BindingPattern, IfBranch, Statement},
};

use super::signature_help::{callee_name, collect_program_calls, split_params};

use crate::document::SourceDoc;
use crate::outline::{Outline, OutlineKind};

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
    hints.retain(|hint| hint.position >= range.start && hint.position <= range.end);
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
fn variable_hint(
    doc: &SourceDoc,
    variable: &novella_ast::stmt::VariableStmt,
    out: &mut Vec<InlayHint>,
) {
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
        if let ClassMember::Method(method) = member
            && let Some(block) = &method.block
        {
            for statement in &block.stmts {
                walk_statement_variables(doc, statement, out);
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
        Statement::For(for_statement) => walk_statement_variables(doc, &for_statement.block, out),
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

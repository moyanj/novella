//! 条件编译在声明收集之前筛选 AST；词法、语法错误仍照常报告。
//!
//! 谓词求值全部委托给 `novella_target::cfg::CfgOptions`（数据模型与求值器同源），
//! 本模块只负责 AST 遍历、指令剔除与把错误转成 `(Span, String)`。

use novella_ast::{
    program::{ClassMember, CompilerDirective, Item, Program},
    stmt::Statement,
};
use novella_core::Span;
pub use novella_target::CfgOptions;

/// 在进入声明收集前按 cfg 裁剪程序；返回带位置的错误列表。
pub(crate) fn configure(program: &mut Program, options: &CfgOptions) -> Vec<(Span, String)> {
    let mut errors = Vec::new();
    program.items.retain_mut(|item| {
        let (directives, members) = match item {
            Item::Function(value) => (&mut value.directives, None),
            Item::Class(value) => (&mut value.directives, Some(&mut value.members)),
            Item::Struct(value) => (&mut value.directives, Some(&mut value.members)),
            Item::Trait(value) => (&mut value.directives, Some(&mut value.members)),
            Item::DirectiveBlock {
                directive, block, ..
            } if directive.name == "cfg" => {
                let enabled = evaluate(directive, options, &mut errors);
                if enabled && let Some(block) = block.take() {
                    *item = Item::Statement(Box::new(Statement::Block(block)));
                }
                return enabled;
            }
            _ => return true,
        };
        if !enabled(directives, options, &mut errors) {
            return false;
        }
        if let Some(members) = members {
            members.retain_mut(|member| {
                let directives = match member {
                    ClassMember::Field(value) => &mut value.directives,
                    ClassMember::Method(value) => &mut value.directives,
                };
                enabled(directives, options, &mut errors)
            });
        }
        true
    });
    errors
}

fn enabled(
    directives: &mut Vec<CompilerDirective>,
    options: &CfgOptions,
    errors: &mut Vec<(Span, String)>,
) -> bool {
    let mut result = true;
    directives.retain(|directive| {
        if directive.name != "cfg" {
            return true;
        }
        // 所有 cfg 均解析，不能因前一个为 false 吞掉后续参数错误。
        result &= evaluate(directive, options, errors);
        false
    });
    result
}

fn evaluate(
    directive: &CompilerDirective,
    options: &CfgOptions,
    errors: &mut Vec<(Span, String)>,
) -> bool {
    // `--check-cfg` 在求值前先校验键与值，保证"所有参数都解析"的契约不变。
    for message in options.check(&directive.tokens) {
        errors.push((directive.span, message));
    }
    match options.evaluate(&directive.tokens) {
        Ok(result) => result,
        Err(message) => {
            errors.push((directive.span, message));
            false
        }
    }
}

//! 条件编译在声明收集之前筛选 AST；词法、语法错误仍照常报告。
use novella_ast::{
    program::{ClassMember, CompilerDirective, Item, Program},
    stmt::Statement,
};
use novella_core::{
    Span,
    token::{Token, TokenKind},
};
use std::collections::BTreeSet;

/// 条件编译配置；默认宿主平台，交叉分析时由调用方显式提供。
#[derive(Debug, Clone)]
pub struct CfgOptions {
    pub target_os: String,
    pub target_arch: String,
    pub target_family: String,
    pub features: BTreeSet<String>,
}

impl Default for CfgOptions {
    fn default() -> Self {
        Self {
            target_os: std::env::consts::OS.to_owned(),
            target_arch: std::env::consts::ARCH.to_owned(),
            target_family: std::env::consts::FAMILY.to_owned(),
            features: BTreeSet::new(),
        }
    }
}

pub(crate) fn configure(
    program: &mut Program,
    options: &CfgOptions,
    target: novella_core::target::BuildTarget,
) -> Vec<(Span, String)> {
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
                let enabled = evaluate(directive, options, target, &mut errors);
                if enabled && let Some(block) = block.take() {
                    *item = Item::Statement(Box::new(Statement::Block(block)));
                }
                return enabled;
            }
            _ => return true,
        };
        if !enabled(directives, options, target, &mut errors) {
            return false;
        }
        if let Some(members) = members {
            members.retain_mut(|member| {
                let directives = match member {
                    ClassMember::Field(value) => &mut value.directives,
                    ClassMember::Method(value) => &mut value.directives,
                };
                enabled(directives, options, target, &mut errors)
            });
        }
        true
    });
    errors
}

fn enabled(
    directives: &mut Vec<CompilerDirective>,
    options: &CfgOptions,
    target: novella_core::target::BuildTarget,
    errors: &mut Vec<(Span, String)>,
) -> bool {
    let mut result = true;
    directives.retain(|directive| {
        if directive.name != "cfg" {
            return true;
        }
        // 所有 cfg 均解析，不能因前一个为 false 吞掉后续参数错误。
        result &= evaluate(directive, options, target, errors);
        false
    });
    result
}

fn evaluate(
    directive: &CompilerDirective,
    options: &CfgOptions,
    target: novella_core::target::BuildTarget,
    errors: &mut Vec<(Span, String)>,
) -> bool {
    let mut parser = Predicate {
        tokens: &directive.tokens,
        index: 0,
        options,
        target,
    };
    let result = parser.parse().and_then(|result| {
        if parser.index == parser.tokens.len() {
            Ok(result)
        } else {
            Err("cfg 条件后有多余 token".to_owned())
        }
    });
    match result {
        Ok(result) => result,
        Err(message) => {
            errors.push((directive.span, message));
            false
        }
    }
}

struct Predicate<'a> {
    tokens: &'a [Token],
    index: usize,
    options: &'a CfgOptions,
    target: novella_core::target::BuildTarget,
}

impl Predicate<'_> {
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self
            .tokens
            .get(self.index)
            .is_some_and(|token| token.kind == kind)
        {
            self.index += 1;
            true
        } else {
            false
        }
    }
    fn parse(&mut self) -> Result<bool, String> {
        stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || self.parse_inner())
    }
    fn parse_inner(&mut self) -> Result<bool, String> {
        let token = self.tokens.get(self.index).ok_or("cfg 需要一个条件")?;
        self.index += 1;
        let name = token.slice.as_str();
        if self.eat(TokenKind::OpenParen) {
            if !matches!(name, "all" | "any" | "not") {
                return Err(format!("未知 cfg 组合 `{name}`"));
            }
            let mut values = Vec::new();
            if !self.eat(TokenKind::CloseParen) {
                loop {
                    values.push(self.parse()?);
                    if self.eat(TokenKind::CloseParen) {
                        break;
                    }
                    if !self.eat(TokenKind::Comma) {
                        return Err("cfg 组合条件之间需要逗号".to_owned());
                    }
                    if self.eat(TokenKind::CloseParen) {
                        break;
                    }
                }
            }
            return match name {
                "all" => Ok(values.iter().all(|value| *value)),
                "any" => Ok(values.iter().any(|value| *value)),
                "not" if values.len() == 1 => Ok(!values[0]),
                _ => Err("cfg not 必须恰有一个条件".to_owned()),
            };
        }
        if self.eat(TokenKind::Assign) {
            let value = self
                .tokens
                .get(self.index)
                .ok_or("cfg 键值条件缺少字符串")?;
            self.index += 1;
            if value.kind != TokenKind::StringLiteral {
                return Err("cfg 条件值必须是普通字符串".to_owned());
            }
            let value = novella_core::string::decode_string_literal(&value.slice)
                .map_err(|error| error.to_string())?;
            return match name {
                "target_os" => Ok(self.options.target_os == value),
                "target_arch" => Ok(self.options.target_arch == value),
                "target_family" => Ok(self.options.target_family == value),
                "feature" => Ok(self.options.features.contains(&value)),
                _ => Err(format!("未知 cfg 条件键 `{name}`")),
            };
        }
        match name {
            "target_has_float" => Ok(self.target.has_float()),
            "true" => Ok(true),
            "false" => Ok(false),
            "unix" | "windows" => Ok(self.options.target_family == name),
            _ => Err(format!("未知 cfg 条件 `{name}`")),
        }
    }
}

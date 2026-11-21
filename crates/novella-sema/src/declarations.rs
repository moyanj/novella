//! 单次分析内的声明身份与 AST 定位；下游无需重复扫描或解释字符串键。
use crate::{BindingTable, ModuleId, ModuleOutput};
use novella_ast::{
    program::{ClassMember, Item},
    types::Param,
};
use novella_core::Span;
use std::collections::HashMap;

/// 仅在本次分析快照内有效，与定义表使用相同的模块和名字位置身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeclarationId {
    pub module: ModuleId,
    pub span: Span,
}

#[derive(Debug)]
struct Location {
    item: usize,
    member: Option<usize>,
}

#[derive(Debug, Default)]
pub struct DeclarationIndex {
    named: HashMap<(ModuleId, String), DeclarationId>,
    locations: HashMap<DeclarationId, Location>,
}

impl DeclarationIndex {
    pub(crate) fn build(modules: &ModuleOutput, bindings: &BindingTable) -> Self {
        let mut result = Self::default();
        for module in &modules.modules {
            for (item_index, item) in module.program.items.iter().enumerate() {
                let (name, span, members) = match item {
                    Item::Function(value) => (&value.name, value.name_span, &[][..]),
                    Item::Struct(value) => (
                        &value.name,
                        bindings
                            .definitions
                            .values()
                            .find(|definition| {
                                definition.kind == crate::DefinitionKind::Type
                                    && definition.module == module.id
                                    && definition.name == value.name
                                    && definition.span.start >= value.span.start
                                    && definition.span.end <= value.span.end
                            })
                            .map(|definition| definition.span)
                            .unwrap_or(value.span),
                        &value.members[..],
                    ),
                    Item::Class(value) => (
                        &value.name,
                        bindings
                            .definitions
                            .values()
                            .find(|definition| {
                                definition.kind == crate::DefinitionKind::Type
                                    && definition.module == module.id
                                    && definition.name == value.name
                                    && definition.span.start >= value.span.start
                                    && definition.span.end <= value.span.end
                            })
                            .map(|definition| definition.span)
                            .unwrap_or(value.span),
                        &value.members[..],
                    ),
                    Item::Trait(value) => (
                        &value.name,
                        bindings
                            .definitions
                            .values()
                            .find(|definition| {
                                definition.kind == crate::DefinitionKind::Type
                                    && definition.module == module.id
                                    && definition.name == value.name
                                    && definition.span.start >= value.span.start
                                    && definition.span.end <= value.span.end
                            })
                            .map(|definition| definition.span)
                            .unwrap_or(value.span),
                        &value.members[..],
                    ),
                    _ => continue,
                };
                result.insert(module.id, name.clone(), span, item_index, None);
                for (member_index, member) in members.iter().enumerate() {
                    let (member_name, span) = match member {
                        ClassMember::Method(value) => (&value.name, value.name_span),
                        ClassMember::Field(value) => (&value.name, value.name_span),
                    };
                    result.insert(
                        module.id,
                        format!("{name}::{member_name}"),
                        span,
                        item_index,
                        Some(member_index),
                    );
                }
            }
        }
        result
    }
    fn insert(
        &mut self,
        module: ModuleId,
        name: String,
        span: Span,
        item: usize,
        member: Option<usize>,
    ) {
        let id = DeclarationId { module, span };
        self.named.insert((module, name), id);
        self.locations.insert(id, Location { item, member });
    }
    pub fn named(&self, module: ModuleId, name: &str) -> Option<DeclarationId> {
        self.named.get(&(module, name.to_owned())).copied()
    }
    /// 以已解析的声明身份取得参数及默认表达式，定义环境由 id.module 给出。
    pub fn parameters<'a>(
        &self,
        modules: &'a ModuleOutput,
        id: DeclarationId,
    ) -> Option<&'a [Param]> {
        let location = self.locations.get(&id)?;
        let item = modules.get(id.module)?.program.items.get(location.item)?;
        match (item, location.member) {
            (Item::Function(value), None) => Some(&value.r#type.as_ref()?.params),
            (_, Some(index)) => match members(item)?.get(index)? {
                ClassMember::Method(value) => Some(&value.params),
                _ => None,
            },
            _ => None,
        }
    }
    /// 类型声明的成员，保留源码顺序；字段初值通过此入口访问。
    pub fn members<'a>(
        &self,
        modules: &'a ModuleOutput,
        id: DeclarationId,
    ) -> Option<&'a [ClassMember]> {
        let location = self.locations.get(&id)?;
        if location.member.is_some() {
            return None;
        }
        members(modules.get(id.module)?.program.items.get(location.item)?)
    }
}
fn members(item: &Item) -> Option<&[ClassMember]> {
    match item {
        Item::Struct(value) => Some(&value.members),
        Item::Class(value) => Some(&value.members),
        Item::Trait(value) => Some(&value.members),
        _ => None,
    }
}

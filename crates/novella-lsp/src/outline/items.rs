//! 顶层遍历：import / trait / class / struct / 函数 / 指令块。

use novella_ast::program::{
    ClassItem, CompilerDirective, FunctionItem, ImportItem, ImportKind, Scope, StructItem,
    TraitItem,
};

use super::name::signature_text;
use super::{FoldKind, OutlineKind, Walker, type_expr_text};

impl Walker<'_> {
    // ---------- 顶层 ----------

    pub(crate) fn item(&mut self, item: &novella_ast::program::Item, parent: Option<usize>) {
        use novella_ast::program::Item;
        match item {
            Item::Import(import) => self.import(import, parent),
            Item::TypeAlias(alias) => {
                let detail = Some(format!("= {}", type_expr_text(&alias.alias)));
                self.push(
                    &alias.name,
                    OutlineKind::TypeAlias,
                    alias.span,
                    parent,
                    detail,
                );
                for parameter in &alias.type_parameters {
                    self.type_parameter(parameter, None);
                }
            }
            Item::Trait(trait_item) => self.trait_item(trait_item, parent),
            Item::Class(class) => self.class_item(class, parent),
            Item::Struct(struct_item) => self.struct_item(struct_item, parent),
            Item::Function(function) => self.function(function, parent),
            Item::Directive(directive) => self.directive(directive),
            Item::DirectiveBlock {
                directive,
                block,
                span,
            } => {
                self.directive(directive);
                if let Some(block) = block {
                    self.block(block, parent);
                }
                self.fold(*span, FoldKind::Block);
            }
            Item::Statement(statement) => self.statement(statement, parent),
        }
    }

    pub(crate) fn directive(&mut self, _directive: &CompilerDirective) {
        // 编译器指令参数是原始 token 流，不进符号树
    }

    /// 导入：命名空间导入出一个符号，具名导入每个说明符各出一个符号。
    ///
    /// `pub` 导入是重导出，详情里额外标注 `pub`，与普通导入区分。
    pub(crate) fn import(&mut self, import: &ImportItem, parent: Option<usize>) {
        let modifier = match import.visibility {
            Some(Scope::Public) => "pub ",
            Some(Scope::Package) => "pkg ",
            Some(Scope::Private) | None => "",
        };
        match &import.kind {
            ImportKind::Named { specifiers, .. } => {
                for specifier in specifiers {
                    // 别名的名字区间落在 specifier 内：优先定位本地别名，找不到再定位原名
                    let local = specifier.local.as_deref().unwrap_or(&specifier.imported);
                    let detail = Some(format!("{modifier}from \"{}\"", import.path));
                    self.push(local, OutlineKind::Import, specifier.span, parent, detail);
                }
            }
            ImportKind::Namespace {
                local_alias, span, ..
            } => {
                let detail = Some(format!("{modifier}module \"{}\"", import.path));
                self.push(local_alias, OutlineKind::Import, *span, parent, detail);
            }
        }
    }

    pub(crate) fn trait_item(&mut self, trait_item: &TraitItem, parent: Option<usize>) {
        let index = self.push(
            &trait_item.name,
            OutlineKind::Trait,
            trait_item.span,
            parent,
            None,
        );
        for parameter in &trait_item.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        self.members(&trait_item.members, index);
        self.fold(trait_item.span, FoldKind::TypeBody);
    }

    pub(crate) fn class_item(&mut self, class: &ClassItem, parent: Option<usize>) {
        let detail = class
            .parent
            .first()
            .map(|parent_ty| format!("extends {}", type_expr_text(parent_ty)));
        let index = self.push(&class.name, OutlineKind::Class, class.span, parent, detail);
        for parameter in &class.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        for parent_ty in &class.parent {
            self.type_expr(parent_ty, index);
        }
        self.members(&class.members, index);
        self.fold(class.span, FoldKind::TypeBody);
    }

    pub(crate) fn struct_item(&mut self, struct_item: &StructItem, parent: Option<usize>) {
        let index = self.push(
            &struct_item.name,
            OutlineKind::Struct,
            struct_item.span,
            parent,
            None,
        );
        for parameter in &struct_item.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        for heritage in &struct_item.heritage {
            self.type_expr(heritage, index);
        }
        self.members(&struct_item.members, index);
        self.fold(struct_item.span, FoldKind::TypeBody);
    }

    pub(crate) fn function(&mut self, function: &FunctionItem, parent: Option<usize>) {
        let detail = signature_text(
            function.r#type.as_ref().map(|ty| ty.params.as_slice()),
            function
                .r#type
                .as_ref()
                .and_then(|ty| ty.return_type.as_deref()),
        );
        let index = self.push(
            &function.name,
            OutlineKind::Function,
            function.span,
            parent,
            detail,
        );
        if let Some(function_type) = &function.r#type {
            self.function_type(function_type, Some(index));
        }
        if let Some(block) = &function.block {
            self.statement(block, Some(index));
        }
        self.fold(function.span, FoldKind::FunctionBody);
    }
}

//! HIR 降级：module 职责实现。

use super::*;

/// 唯一入口：把全部语义产物降级为 HIR。
///
/// 前置约定：调用方在 `diagnostics.has_errors()` 为真时不应调用；
/// 任何词法、语法、语义或加载错误均拒绝降级，防止交付残缺程序。
pub fn build_hir(sema: &SemaResult) -> Result<HirProgram, BuildError> {
    if sema.diagnostics.has_errors() {
        return Err(BuildError::at(BuildErrorKind::InvalidInput));
    }
    let mut modules = Vec::with_capacity(sema.modules.modules.len());
    for module in &sema.modules.modules {
        if module.syntax_failed || module.load_failed {
            return Err(BuildError::at(BuildErrorKind::InvalidInput)
                .with_site(module.id, Span { start: 0, end: 0 }));
        }
        if !module.required_for_codegen {
            continue;
        }
        modules.push(build_module(sema, module)?);
    }
    // 同一默认表达式可能在多个调用点展开；无捕获 lambda 只有一个实体，
    // 且函数定义归属源码模块，而非首次展开它的调用模块。
    let mut lambdas = Vec::new();
    let mut seen = HashSet::new();
    for module in &mut modules {
        let mut items = Vec::new();
        for item in std::mem::take(&mut module.items) {
            if let HirItem::Function(function) = &item
                && function.is_lambda
            {
                if seen.insert(function.id.clone()) {
                    lambdas.push(item);
                }
            } else {
                items.push(item);
            }
        }
        module.items = items;
    }
    for item in lambdas {
        let HirItem::Function(function) = &item else {
            unreachable!("只收集 lambda 函数");
        };
        let owner = modules
            .iter_mut()
            .find(|module| function.source_module == module.id)
            .expect("lambda 的定义模块来自已加载的语义模块");
        owner.items.push(item);
    }
    Ok(HirProgram { modules })
}

/// 单个模块的降级。
pub(super) fn build_module(
    sema: &SemaResult,
    module: &AnalyzedModule,
) -> Result<HirModule, BuildError> {
    let module_id = module.id;
    let mut builder = ModuleBuilder::new(sema, module_id, &module.program);

    // 先登记全部顶层声明：调用 / 递归在函数体降级前就要能取到签名
    builder.collect_declarations()?;

    let mut items = Vec::new();
    for item in &module.program.items {
        match item {
            // `extern func foo();` 只有签名、没有函数体：签名已在阶段 1 登记，
            // 产出独立外部声明，供 codegen 声明而不定义。
            Item::Function(function) if function.is_extern => {
                let mut declaration = builder.build_function(function)?;
                declaration.visibility = HirVisibility::Public;
                items.push(HirItem::ExternFunction(declaration));
            }
            Item::Function(function) => {
                if function.is_async {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "async function",
                    })
                    .with_site(module_id, function.span));
                }
                items.push(HirItem::Function(builder.build_function(function)?));
            }
            // 导入 / 类型别名：已由 sema 消费，后端不感知
            Item::Import(_) | Item::TypeAlias(_) => {}
            // 具名类型：字段布局走 `finish_types`
            Item::Struct(_) | Item::Class(_) | Item::Trait(_) => {}
            // 装饰器语句 / 装饰块：修饰关系已在 sema 侧处理
            Item::Decorator(decorator) => {
                return Err(
                    BuildError::at(BuildErrorKind::Unimplemented { node: "decorator" })
                        .with_site(module_id, decorator.span),
                );
            }
            Item::DecoratorBlock { span, .. } => {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "decorator block",
                })
                .with_site(module_id, *span));
            }
            Item::Statement(statement) => {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "top-level statement",
                })
                .with_site(module_id, statement.span()));
            } // `Item` 若有新增变体，此处会编译失败而不是静默丢弃
        }
    }

    // 方法体必须在这一轮里也降级（`finish_types` 只处理声明部分）
    let types = builder.finish_types()?;
    items.extend(builder.method_functions()?);
    // lambda 编译出的函数项追加在最后（调用点已用稳定键引用，顺序无关）
    for batch in std::mem::take(&mut builder.pending) {
        for lambda in batch {
            items.push(HirItem::Function(lambda));
        }
    }

    Ok(HirModule {
        id: module_id,
        path: module.path.display().to_string(),
        items,
        types,
    })
}

impl<'a> ModuleBuilder<'a> {
    pub(super) fn new(sema: &'a SemaResult, module: ModuleId, program: &'a Program) -> Self {
        ModuleBuilder {
            sema,
            module,
            program,
            signatures: HashMap::new(),
            functions: Vec::new(),
            top_signatures: HashMap::new(),
            pending: Vec::new(),
            struct_items: Vec::new(),
            class_items: Vec::new(),
            trait_items: Vec::new(),
        }
    }

    /// 收集全部顶层声明：函数签名与具名类型。
    pub(super) fn collect_declarations(&mut self) -> Result<(), BuildError> {
        for item in &self.program.items {
            validate_declaration(item, self.module)?;
            match item {
                Item::Function(function) => {
                    if !function.attributes.is_empty() {
                        return Err(BuildError::at(BuildErrorKind::Unimplemented {
                            node: "function decorator",
                        })
                        .with_site(self.module, function.span));
                    }
                    let id = self.function_id(&function.name);
                    let signature = self.function_signature(&id, function)?;
                    self.signatures.insert(id.clone(), signature.clone());
                    self.functions.push((function.name.clone(), id.clone()));
                    self.top_signatures.insert(function.name.clone(), signature);
                }
                Item::Struct(_) => self.struct_items.push(item),
                Item::Class(_) => self.class_items.push(item),
                Item::Trait(_) => self.trait_items.push(item),
                Item::Import(_) | Item::TypeAlias(_) => {}
                Item::Decorator(_) | Item::DecoratorBlock { .. } => {}
                Item::Statement(_) => {
                    // 顶层可执行语句在 `build_module` 里统一报 Unimplemented
                }
            }
        }
        Ok(())
    }

    /// 模块内某个顶层函数的稳定键。
    pub(super) fn function_id(&self, name: &str) -> HirFunctionId {
        HirFunctionId(format!("m{}::{}", self.module, name))
    }

    /// 从 AST 函数声明构造签名。
    pub(super) fn function_signature(
        &mut self,
        id: &HirFunctionId,
        function: &FunctionItem,
    ) -> Result<FunctionSignature, BuildError> {
        let view = self
            .sema
            .declaration_types
            .get(self.module, function.span)
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "函数签名"
                })
                .with_site(self.module, function.span)
            })?;
        let HirType::Function(mut signature) = HirType::from_view(view, &mut function_id_of)
            .map_err(|error| error.with_site(self.module, function.span))?
        else {
            return Err(BuildError::at(BuildErrorKind::MissingType {
                what: "函数签名"
            })
            .with_site(self.module, function.span));
        };
        signature.id = id.clone();
        Ok(*signature)
    }

    /// 构造单个函数的 HIR。
    pub(super) fn build_function(
        &mut self,
        function: &FunctionItem,
    ) -> Result<HirFunction, BuildError> {
        let id = self.function_id(&function.name);
        let signature = self
            .top_signatures
            .get(&function.name)
            .cloned()
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "函数签名"
                })
                .with_site(self.module, function.span)
            })?;

        // 先结束对 `self` 的借用：函数体降级期间还要把 lambda 推进 `pending`
        let functions = self.functions.clone();
        let mut body_builder = BodyBuilder::new(self.sema, self.module, functions);
        body_builder.return_type = Some(signature.ret.clone());
        // 形参先占局部槽位；不发射 `Alloc`，codegen 直接从 `get_nth_param` 拷入
        if let Some(function_type) = &function.r#type {
            for (index, param) in function_type.params.iter().enumerate() {
                let ty = signature.params.get(index).cloned().ok_or_else(|| {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "形参类型"
                    })
                    .with_site(self.module, param.span)
                })?;
                body_builder.declare_param(param.name.clone(), ty);
            }
        }

        let body = match &function.block {
            Some(block) => body_builder.build_statement(block)?,
            None => HirBlock::new(),
        };

        // lambda 降级到同模块的额外函数项里
        let lambdas = std::mem::take(&mut body_builder.lambdas);
        self.pending.push(lambdas);

        Ok(HirFunction {
            source_module: self.module,
            is_lambda: false,
            id,
            link_name: function.is_extern.then(|| function.name.clone()),
            signature,
            params: body_builder.parameter_slots(),
            body,
            visibility: visibility_of(function.scope),
        })
    }
}

/// sema 投影尚未携带这些语义，必须在丢失 AST 信息前拒绝。
fn validate_declaration(item: &Item, module: ModuleId) -> Result<(), BuildError> {
    let (attributes, parameters, members, span) = match item {
        Item::Struct(item) => (
            &item.attributes,
            &item.type_parameters,
            &item.members,
            item.span,
        ),
        Item::Class(item) => (
            &item.attributes,
            &item.type_parameters,
            &item.members,
            item.span,
        ),
        Item::Trait(item) => {
            if !item.members.is_empty() {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "trait members",
                })
                .with_site(module, item.span));
            }
            (
                &item.attributes,
                &item.type_parameters,
                &item.members,
                item.span,
            )
        }
        Item::TypeAlias(item) if !item.type_parameters.is_empty() => {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "generic type alias",
            })
            .with_site(module, item.span));
        }
        _ => return Ok(()),
    };
    if !attributes.is_empty() || !parameters.is_empty() {
        return Err(BuildError::at(BuildErrorKind::Unimplemented {
            node: "decorated / generic type",
        })
        .with_site(module, span));
    }
    for member in members {
        let (unsupported, span) = match member {
            ClassMember::Field(field) => (!field.attributes.is_empty(), field.span),
            ClassMember::Method(method) => (
                !method.attributes.is_empty() || !method.type_parameters.is_empty(),
                method.span,
            ),
        };
        if unsupported {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "decorated / generic member",
            })
            .with_site(module, span));
        }
    }
    Ok(())
}

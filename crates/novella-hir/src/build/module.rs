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
    // 旧分析 API 可独立检查 32 位源码，但不能把其 usize/isize 解释交给 64 位后端。
    let analysis_bits: u32 = match sema.analysis_target.pointer_width {
        novella_sema::PointerWidth::Bits32 => 32,
        novella_sema::PointerWidth::Bits64 => 64,
    };
    // 目标位宽取自目标描述；描述已排除 32 位可发射目标，但分析侧仍可能被显式要求 32 位。
    let target_bits = u32::from(sema.build_spec.pointer_width);
    if analysis_bits != target_bits {
        return Err(BuildError::at(BuildErrorKind::TargetWidthMismatch {
            analysis_bits,
            target_bits,
        }));
    }
    let instances = Instances::default();
    let mut modules = Vec::with_capacity(sema.modules.modules.len());
    for module in &sema.modules.modules {
        if module.syntax_failed || module.load_failed {
            return Err(BuildError::at(BuildErrorKind::InvalidInput)
                .with_site(module.id, Span { start: 0, end: 0 }));
        }
        if !module.required_for_codegen {
            continue;
        }
        modules.push(build_module(sema, module, instances.clone())?);
    }
    // 先登记实例身份，再反复从队列降低定义体；递归调用不会产生重复实体。
    loop {
        let instance = instances.borrow_mut().next();
        let Some(instance) = instance else {
            break;
        };
        let module_id = match &instance {
            mono::Instance::Aggregate(instance) => instance.module,
            mono::Instance::Function(instance) => instance.view.module,
        };
        let source = sema.modules.get(module_id).expect("实例定义来自已加载模块");
        let index = if let Some(index) = modules.iter().position(|module| module.id == module_id) {
            index
        } else {
            modules.push(build_module(sema, source, instances.clone())?);
            modules.len() - 1
        };
        let mut builder = ModuleBuilder::new(sema, module_id, &source.program, instances.clone());
        let module = &mut modules[index];
        match instance {
            mono::Instance::Aggregate(instance) => module
                .types
                .push(builder.build_aggregate_instance(&instance)?),
            mono::Instance::Function(instance) => module.items.push(HirItem::Function(
                builder.build_function_instance(&instance)?,
            )),
        }
        module
            .items
            .extend(builder.pending.into_iter().flatten().map(HirItem::Function));
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
    layout::validate_value_layouts(sema, &instances, &modules)?;
    let requirements = super::requirements::validate(sema, &instances, &modules)?;
    Ok(HirProgram {
        spec: sema.build_spec.clone(),
        target: sema.build_target,
        no_main: sema.no_main,
        requirements,
        modules,
    })
}

/// 单个模块的降级。
fn build_module(
    sema: &SemaResult,
    module: &AnalyzedModule,
    instances: Instances,
) -> Result<HirModule, BuildError> {
    let module_id = module.id;
    let mut builder = ModuleBuilder::new(sema, module_id, &module.program, instances);

    // 先登记全部顶层声明：调用 / 递归在函数体降级前就要能取到签名
    builder.collect_declarations()?;

    let mut items = Vec::new();
    for item in &module.program.items {
        match item {
            // `extern func foo();` 只有签名、没有函数体：签名已在阶段 1 登记，
            // 产出独立外部声明，供 codegen 声明而不定义。
            Item::Function(function) if is_generic(function) => {}
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
            // 指令语句 / 指令块：作用关系已在 sema 侧处理
            Item::Directive(directive)
                if matches!(directive.name.as_str(), "no_main" | "no_std" | "no_alloc") => {}
            Item::Directive(directive) => {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "compiler directive",
                })
                .with_site(module_id, directive.span));
            }
            Item::DirectiveBlock { span, .. } => {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "directive block",
                })
                .with_site(module_id, *span));
            }
            Item::Statement(statement) if matches!(statement.as_ref(), Statement::Variable(_)) => {
                let Statement::Variable(declaration) = statement.as_ref() else {
                    unreachable!()
                };
                items.push(HirItem::Global(super::globals::build_global(
                    sema,
                    &builder.instances,
                    module_id,
                    declaration,
                )?));
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
    pub(super) fn new(
        sema: &'a SemaResult,
        module: ModuleId,
        program: &'a Program,
        instances: Instances,
    ) -> Self {
        ModuleBuilder {
            sema,
            module,
            program,
            instances,
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
                    if function.directives.iter().any(|d| d.name != "export") {
                        return Err(BuildError::at(BuildErrorKind::Unimplemented {
                            node: "function directive",
                        })
                        .with_site(self.module, function.span));
                    }
                    if is_generic(function) {
                        continue;
                    }
                    let id = self.function_id(&function.name);
                    let signature = self.function_signature(&id, function)?;
                    self.signatures.insert(id.clone(), signature.clone());
                    self.functions.push((function.name.clone(), id.clone()));
                    self.top_signatures.insert(function.name.clone(), signature);
                }
                Item::Struct(inner) if inner.type_parameters.is_empty() => {
                    self.struct_items.push(item)
                }
                Item::Class(inner) if inner.type_parameters.is_empty() => {
                    self.class_items.push(item)
                }
                Item::Class(_) | Item::Struct(_) => {}
                Item::Trait(_) => self.trait_items.push(item),
                Item::Import(_) | Item::TypeAlias(_) => {}
                Item::Directive(_) | Item::DirectiveBlock { .. } => {}
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
        let HirType::Function(mut signature) =
            mono::concrete_type(self.sema, &self.instances, view, &Vec::new(), None)
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

        self.build_function_with(function, signature, Vec::new())
    }

    pub(super) fn build_function_instance(
        &mut self,
        instance: &mono::FunctionInstance,
    ) -> Result<HirFunction, BuildError> {
        let symbol = instance.view.name.as_deref().expect("实例具有声明名");
        let missing = || {
            BuildError::at(BuildErrorKind::MissingType {
                what: "单态化定义"
            })
        };
        if let Some((owner, method_name)) = symbol.split_once("::") {
            let method = self
                .sema
                .declarations
                .named(self.module, owner)
                .and_then(|id| self.sema.declarations.members(&self.sema.modules, id))
                .and_then(|members| {
                    members.iter().find_map(|member| match member {
                        ClassMember::Method(method) if method.name == method_name => Some(method),
                        _ => None,
                    })
                })
                .ok_or_else(missing)?;
            let receiver = if instance.signature.receiver {
                Some(mono::method_receiver(
                    self.sema,
                    &self.instances,
                    self.module,
                    owner,
                    &instance.subst,
                )?)
            } else {
                None
            };
            return self.build_method_with(
                method,
                instance.signature.clone(),
                instance.subst.clone(),
                receiver,
            );
        }
        let function = self
            .program
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == symbol => Some(function),
                _ => None,
            })
            .ok_or_else(missing)?;
        self.build_function_with(function, instance.signature.clone(), instance.subst.clone())
    }

    fn build_function_with(
        &mut self,
        function: &FunctionItem,
        signature: FunctionSignature,
        subst: Substitution,
    ) -> Result<HirFunction, BuildError> {
        if !subst.is_empty()
            && (function.is_extern
                || self
                    .sema
                    .exports
                    .contains_key(&(self.module, function.span)))
        {
            return Err(BuildError::at(BuildErrorKind::GenericUnsupported)
                .with_site(self.module, function.span));
        }
        // 先结束对 `self` 的借用：函数体降级期间还要把 lambda 推进 `pending`
        let functions = self.functions.clone();
        let mut body_builder = BodyBuilder::new(
            self.sema,
            self.module,
            functions,
            self.instances.clone(),
            subst,
        );
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
            id: signature.id.clone(),
            link_name: function.is_extern.then(|| function.name.clone()),
            export_name: self
                .sema
                .exports
                .get(&(self.module, function.span))
                .cloned(),
            signature,
            params: body_builder.parameter_slots(),
            body,
            visibility: visibility_of(function.scope),
        })
    }
}

fn is_generic(function: &FunctionItem) -> bool {
    function
        .r#type
        .as_ref()
        .is_some_and(|ty| !ty.type_parameters.is_empty())
}

/// sema 投影尚未携带这些语义，必须在丢失 AST 信息前拒绝。
fn validate_declaration(item: &Item, module: ModuleId) -> Result<(), BuildError> {
    let (directives, members, span) = match item {
        Item::Struct(item) => (&item.directives, &item.members, item.span),
        Item::Class(item) => (&item.directives, &item.members, item.span),
        Item::Trait(item) => (&item.directives, &item.members, item.span),
        Item::TypeAlias(item) if !item.type_parameters.is_empty() => {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "generic type alias",
            })
            .with_site(module, item.span));
        }
        _ => return Ok(()),
    };
    if !directives.is_empty() {
        return Err(BuildError::at(BuildErrorKind::Unimplemented {
            node: "directive-decorated type",
        })
        .with_site(module, span));
    }
    for member in members {
        let (unsupported, span) = match member {
            ClassMember::Field(field) => (!field.directives.is_empty(), field.span),
            ClassMember::Method(method) => (!method.directives.is_empty(), method.span),
            ClassMember::AssociatedType(_) => continue,
        };
        if unsupported {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "directive-decorated member",
            })
            .with_site(module, span));
        }
    }
    Ok(())
}

//! HIR 降级：types 职责实现。

use super::*;

/// 类型注解只消费 sema 已完成名称解析的结果，不再从 AST 猜类型。
impl BodyBuilder<'_> {
    pub(super) fn annotation_type(&self, annotation: &TypeExpr) -> Result<HirType, BuildError> {
        let span = annotation.span();
        let view = self
            .sema
            .annotation_types
            .get(self.module, span)
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "注解的语义类型",
                })
                .with_site(self.module, span)
            })?;
        self.concrete_type(view, None)
            .map_err(|error| error.with_site(self.module, span))
    }
}

impl<'a> ModuleBuilder<'a> {
    /// 完成具名类型定义。
    ///
    /// 字段与方法来自 sema 的公开类型摘要，字段继承布局与访问共用同一来源。
    pub(super) fn finish_types(&mut self) -> Result<Vec<HirTypeDef>, BuildError> {
        let mut types = Vec::with_capacity(
            self.struct_items.len() + self.class_items.len() + self.trait_items.len(),
        );
        for item in &self.struct_items {
            if let Item::Struct(item) = item {
                let summary = self.type_summary(&item.name, item.span, "struct")?;
                let fields = instance_fields(
                    self.sema,
                    &self.instances,
                    self.module,
                    &item.name,
                    item.span,
                )?;
                let methods = self.hir_methods(&summary.methods)?;
                types.push(HirTypeDef::Struct {
                    name: item.name.clone(),
                    fields,
                    methods,
                });
            }
        }
        for item in &self.class_items {
            if let Item::Class(item) = item {
                let summary = self.type_summary(&item.name, item.span, "class")?;
                let fields = instance_fields(
                    self.sema,
                    &self.instances,
                    self.module,
                    &item.name,
                    item.span,
                )?;
                let methods = self.hir_methods(&summary.methods)?;
                types.push(HirTypeDef::Class {
                    name: item.name.clone(),
                    parent: layout::instance_parent(
                        self.sema,
                        &self.instances,
                        &summary,
                        &Vec::new(),
                    )?,
                    fields,
                    methods,
                });
            }
        }
        for item in &self.trait_items {
            if let Item::Trait(item) = item {
                let summary = self.type_summary(&item.name, item.span, "trait")?;
                // Trait 方法模板只在 witness 或静态约束调用处实例化。
                let _ = summary;
                let methods = Vec::new();
                types.push(HirTypeDef::Trait {
                    name: item.name.clone(),
                    methods,
                });
            }
        }
        Ok(types)
    }

    /// 构造**带函数体**的方法对应的 [`HirFunction`]。
    ///
    /// 方法签名已经由 `hir_methods`（→ [`HirMethod`]）登记在类型定义里；
    /// 这里只产出函数体与形参槽位，返回值一律追加进模块的 `items`，
    /// 让 codegen 只需「遍历模块的函数项」即可完成代码生成。
    pub(super) fn method_functions(&mut self) -> Result<Vec<HirItem>, BuildError> {
        let mut out = Vec::new();
        let items = self
            .struct_items
            .iter()
            .chain(&self.class_items)
            .copied()
            .collect::<Vec<_>>();
        for item in items {
            let (type_name, members) = match item {
                Item::Struct(item) => (item.name.clone(), &item.members),
                Item::Class(item) => (item.name.clone(), &item.members),
                _ => continue,
            };
            for member in members {
                let ClassMember::Method(method) = member else {
                    continue;
                };
                if method.block.is_none() || !method.type_parameters.is_empty() {
                    continue;
                }
                let signature = self
                    .method_signature(&type_name, &method.name)?
                    .ok_or_else(|| {
                        BuildError::at(BuildErrorKind::MissingType {
                            what: "方法签名"
                        })
                        .with_site(self.module, method.span)
                    })?;
                let receiver = match item {
                    Item::Struct(_) => HirType::Struct(self.module, type_name.clone()),
                    Item::Class(_) => HirType::Class(self.module, type_name.clone()),
                    _ => unreachable!("方法只来自 struct / class"),
                };
                out.push(HirItem::Function(self.build_method_with(
                    method,
                    signature,
                    Vec::new(),
                    Some(receiver),
                )?));
            }
        }
        Ok(out)
    }

    pub(super) fn build_method_with(
        &mut self,
        method: &novella_ast::program::MethodMember,
        signature: FunctionSignature,
        subst: Substitution,
        receiver: Option<HirType>,
    ) -> Result<HirFunction, BuildError> {
        let missing = || {
            BuildError::at(BuildErrorKind::MissingType {
                what: "方法定义"
            })
            .with_site(self.module, method.span)
        };
        let body = method.block.as_ref().ok_or_else(missing)?;
        let mut builder = BodyBuilder::new(
            self.sema,
            self.module,
            self.functions.clone(),
            self.instances.clone(),
            subst,
        );
        builder.return_type = Some(signature.ret.clone());
        if signature.receiver {
            let receiver = receiver.ok_or_else(missing)?;
            let ty = if matches!(receiver, HirType::Struct(..)) {
                HirType::Pointer(Box::new(receiver))
            } else {
                receiver
            };
            builder.declare_param("self".to_owned(), ty);
        }
        let skip = usize::from(
            signature.receiver
                && method
                    .params
                    .first()
                    .is_some_and(|param| param.name == "self"),
        );
        if method.params.len() - skip != signature.params.len() {
            return Err(missing());
        }
        for (param, ty) in method.params.iter().skip(skip).zip(&signature.params) {
            builder.declare_param(param.name.clone(), ty.clone());
        }
        let hir_body = builder.build_statement(&Statement::Block(body.clone()))?;
        self.pending.push(std::mem::take(&mut builder.lambdas));
        Ok(HirFunction {
            link_name: None,
            export_name: None,
            source_module: self.module,
            is_lambda: false,
            id: signature.id.clone(),
            signature,
            params: builder.parameter_slots(),
            body: hir_body,
            visibility: visibility_of(method.modifiers.scope),
        })
    }

    pub(super) fn build_aggregate_instance(
        &self,
        instance: &mono::AggregateInstance,
    ) -> Result<HirTypeDef, BuildError> {
        let span = Span { start: 0, end: 0 };
        let fields = instance_fields(
            self.sema,
            &self.instances,
            instance.module,
            &instance.name,
            span,
        )?;
        // 只有已调用的方法进入工作队列；聚合布局不需要整张单态方法表。
        if instance.is_class {
            let (summary, subst) = mono::aggregate_definition(
                self.sema,
                &self.instances,
                instance.module,
                &instance.name,
            )
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "class 实例声明",
                })
            })?;
            let parent = layout::instance_parent(self.sema, &self.instances, summary, &subst)?;
            // 析构由运行时调用，源码没有显式 CallInstance，但必须登记每个具体类的实体。
            let methods = summary
                .methods
                .iter()
                .filter(|method| method.name == "__destroy")
                .map(|method| {
                    let signature = mono::instantiate_function(
                        self.sema,
                        &self.instances,
                        &method.signature,
                        subst.clone(),
                    )?;
                    Ok(HirMethod {
                        name: method.name.clone(),
                        signature,
                        is_static: method.is_static,
                    })
                })
                .collect::<Result<Vec<_>, BuildError>>()?;
            Ok(HirTypeDef::Class {
                name: instance.name.clone(),
                parent,
                fields,
                methods,
            })
        } else {
            Ok(HirTypeDef::Struct {
                name: instance.name.clone(),
                fields,
                methods: Vec::new(),
            })
        }
    }

    /// 方法签名（取自语义类型摘要）。
    pub(super) fn method_signature(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Result<Option<FunctionSignature>, BuildError> {
        let Some(summary) = self
            .sema
            .types
            .summaries
            .iter()
            .find(|summary| summary.module == self.module && summary.name == type_name)
        else {
            return Ok(None);
        };
        for method in &summary.methods {
            if method.name == method_name {
                let signature = mono::concrete_signature(
                    self.sema,
                    &self.instances,
                    &method.signature,
                    &Vec::new(),
                )
                .map_err(|error| error.with_site(self.module, Span { start: 0, end: 0 }))?;
                return Ok(Some(signature));
            }
        }
        Ok(None)
    }

    /// 取语义阶段的类型摘要：字段 / 方法的唯一来源。
    ///
    /// 摘要是 `sema` 从类型注册表投影出来的，直接以 `TypeView` 给出每个字段的
    /// 类型 —— HIR 不需要（也无法）自己重新解析一遍类型表达式。
    pub(super) fn type_summary(
        &self,
        name: &str,
        span: Span,
        what: &'static str,
    ) -> Result<novella_sema::TypeSummary, BuildError> {
        self.sema
            .types
            .summaries
            .iter()
            .find(|summary| summary.module == self.module && summary.name == name)
            .cloned()
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType { what }).with_site(self.module, span)
            })
    }

    /// 方法摘要 → HIR 方法（签名走 `FunctionView` 投影）。
    pub(super) fn hir_methods(
        &self,
        methods: &[novella_sema::MethodSummary],
    ) -> Result<Vec<HirMethod>, BuildError> {
        methods
            .iter()
            .filter(|method| method.signature.type_param_count == 0)
            .map(|method| {
                // 实例方法的 `self` 不在签名形参里（`FunctionView::receiver` 已剥离），
                // 因此这里拿到的 `signature.params` 与 AST 的 `method.params` 一一对应
                let signature = mono::concrete_signature(
                    self.sema,
                    &self.instances,
                    &method.signature,
                    &Vec::new(),
                )
                .map_err(|error| error.with_site(self.module, Span { start: 0, end: 0 }))?;
                Ok(HirMethod {
                    name: method.name.clone(),
                    signature,
                    is_static: method.is_static,
                })
            })
            .collect()
    }
}

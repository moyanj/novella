//! HIR 降级：types 职责实现。

use super::*;

/// 类型注解只消费 sema 已完成名称解析的结果，不再从 AST 猜类型。
pub(super) fn annotation_type(
    sema: &SemaResult,
    module: ModuleId,
    annotation: &TypeExpr,
) -> Result<HirType, BuildError> {
    let span = annotation.span();
    let view = sema.annotation_types.get(module, span).ok_or_else(|| {
        BuildError::at(BuildErrorKind::MissingType {
            what: "注解的语义类型",
        })
        .with_site(module, span)
    })?;
    HirType::from_view(view, &mut function_id_of).map_err(|error| error.with_site(module, span))
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
                let fields = instance_fields(self.sema, self.module, &item.name, item.span)?;
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
                let fields = instance_fields(self.sema, self.module, &item.name, item.span)?;
                let methods = self.hir_methods(&summary.methods)?;
                types.push(HirTypeDef::Class {
                    name: item.name.clone(),
                    parent: summary.parent.clone(),
                    fields,
                    methods,
                });
            }
        }
        for item in &self.trait_items {
            if let Item::Trait(item) = item {
                let summary = self.type_summary(&item.name, item.span, "trait")?;
                let methods = self.hir_methods(&summary.methods)?;
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
        for item in self.struct_items.iter().chain(self.class_items.iter()) {
            let (type_name, members) = match item {
                Item::Struct(item) => (item.name.clone(), &item.members),
                Item::Class(item) => (item.name.clone(), &item.members),
                _ => continue,
            };
            for member in members {
                let ClassMember::Method(method) = member else {
                    continue;
                };
                let Some(body) = &method.block else {
                    // 纯声明形态（`semi`）：签名已发布，无函数体
                    continue;
                };
                let signature = self
                    .method_signature(&type_name, &method.name)?
                    .ok_or_else(|| {
                        BuildError::at(BuildErrorKind::MissingType {
                            what: "方法签名"
                        })
                        .with_site(self.module, method.span)
                    })?;
                let functions = self.functions.clone();
                let mut body_builder = BodyBuilder::new(self.sema, self.module, functions);
                body_builder.return_type = Some(signature.ret.clone());
                // 实例方法：`self` 是隐式接收者槽位（类型为该具名类型）
                if signature.receiver {
                    let receiver_ty = match item {
                        Item::Struct(_) => HirType::Pointer(Box::new(HirType::Struct(
                            self.module,
                            type_name.clone(),
                        ))),
                        Item::Class(_) => HirType::Class(self.module, type_name.clone()),
                        _ => unreachable!("方法只来自 struct / class"),
                    };
                    let slot = body_builder.declare_param("self".to_string(), receiver_ty);
                    let _ = slot;
                }
                // AST 的形参列表可能含无注解的 `self`（隐式接收者），它已由上面的
                // 接收者槽位占据，这里跳过并重新对齐下标
                let mut declared = 0usize;
                for (index, param) in method.params.iter().enumerate() {
                    if !(signature.receiver && index == 0 && param.name == "self") {
                        let ty = signature.params.get(declared).cloned().ok_or_else(|| {
                            BuildError::at(BuildErrorKind::MissingType {
                                what: "形参类型"
                            })
                            .with_site(self.module, param.span)
                        })?;
                        body_builder.declare_param(param.name.clone(), ty);
                        declared += 1;
                    }
                }
                let hir_body = body_builder.build_statement(&Statement::Block(body.clone()))?;
                let lambdas = std::mem::take(&mut body_builder.lambdas);
                self.pending.push(lambdas);
                let params = body_builder.parameter_slots();
                out.push(HirItem::Function(HirFunction {
                    id: signature.id.clone(),
                    signature,
                    params,
                    body: hir_body,
                    visibility: visibility_of(method.modifiers.scope),
                }));
            }
        }
        Ok(out)
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
                let signature = FunctionSignature::from_view(&method.signature, &mut |view| {
                    self.module_function_id(view)
                })
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
            .map(|method| {
                // 实例方法的 `self` 不在签名形参里（`FunctionView::receiver` 已剥离），
                // 因此这里拿到的 `signature.params` 与 AST 的 `method.params` 一一对应
                let signature = FunctionSignature::from_view(&method.signature, &mut |view| {
                    self.module_function_id(view)
                })
                .map_err(|error| error.with_site(self.module, Span { start: 0, end: 0 }))?;
                Ok(HirMethod {
                    name: method.name.clone(),
                    signature,
                    is_static: method.is_static,
                })
            })
            .collect()
    }

    /// 名义函数键：具名的由模块 + 名字唯一确定；匿名的按位置占位。
    pub(super) fn module_function_id(&self, view: &novella_sema::FunctionView) -> HirFunctionId {
        match &view.name {
            Some(name) => HirFunctionId(format!("m{}::{}", view.module, name)),
            None => HirFunctionId::anonymous(view.module),
        }
    }
}

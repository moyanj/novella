//! 已解析签名的调用：定型实参、显式传递 struct 接收者。

use super::*;

pub(super) struct IndexPlace {
    pub(super) receiver: HirOperand,
    pub(super) key: HirOperand,
    pub(super) receiver_ty: HirType,
}

impl BodyBuilder<'_> {
    pub(super) fn overloaded_index_receiver(
        &self,
        expr: &Expression,
    ) -> Result<Option<HirType>, BuildError> {
        match expr {
            Expression::Paren { expr, .. } => self.overloaded_index_receiver(expr),
            Expression::IndexAccess { target, .. } => {
                let ty = self.expr_type(target.span())?;
                Ok((matches!(ty, HirType::Struct(..) | HirType::Class(..))
                    && !super::operators::same_string_class(self.sema, &ty))
                .then_some(ty))
            }
            _ => Ok(None),
        }
    }

    fn index_method(
        &self,
        ty: &HirType,
        name: &str,
        span: Span,
    ) -> Result<FunctionSignature, BuildError> {
        let (HirType::Class(module, owner) | HirType::Struct(module, owner)) = ty else {
            unreachable!("已检查下标协议接收者")
        };
        self.class_method_signature(*module, owner, name, span)?
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "下标协议方法签名",
                })
                .with_site(self.module, span)
            })
    }

    pub(super) fn build_index_place(
        &mut self,
        expr: &Expression,
        receiver_ty: HirType,
        method: &str,
        block: &mut HirBlock,
    ) -> Result<IndexPlace, BuildError> {
        if let Expression::Paren { expr, .. } = expr {
            return self.build_index_place(expr, receiver_ty, method, block);
        }
        let Expression::IndexAccess {
            target,
            index,
            span,
        } = expr
        else {
            unreachable!("已检查下标表达式")
        };
        let signature = self.index_method(&receiver_ty, method, *span)?;
        let receiver = if matches!(receiver_ty, HirType::Struct(..)) {
            self.aggregate_address(target, block)?
        } else {
            self.build_expression(target, block)?
        };
        let key = self.build_typed_expression(index, &signature.params[0], block)?;
        Ok(IndexPlace {
            receiver,
            key,
            receiver_ty,
        })
    }

    pub(super) fn read_index_place(
        &mut self,
        place: &IndexPlace,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let signature = self.index_method(&place.receiver_ty, "__getitem", span)?;
        let dst = self.fresh_local(signature.ret.clone());
        block.push(HirStmt::Instr(HirInstr::Call {
            callee: HirOperand::FunctionPtr(signature.id),
            args: vec![place.receiver.clone(), place.key.clone()],
            dst: Some(dst),
            dst_ty: Some(signature.ret),
        }));
        Ok(HirOperand::Local(dst))
    }

    pub(super) fn write_index_place(
        &self,
        place: IndexPlace,
        value: HirOperand,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        let signature = self.index_method(&place.receiver_ty, "__setitem", span)?;
        block.push(HirStmt::Instr(HirInstr::Call {
            callee: HirOperand::FunctionPtr(signature.id),
            args: vec![place.receiver, place.key, value],
            dst: None,
            dst_ty: None,
        }));
        Ok(())
    }

    pub(super) fn build_new(
        &mut self,
        _target: &Expression,
        type_args: &[TypeExpr],
        args: &[Expression],
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        if !type_args.is_empty() {
            return Err(
                BuildError::at(BuildErrorKind::GenericUnsupported).with_site(self.module, span)
            );
        }
        let ty = self.expr_type(span)?;
        let HirType::Class(module, name) = &ty else {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "new 的目标不是 class",
            })
            .with_site(self.module, span));
        };
        let init = self.class_method_signature(*module, name, "__init", span)?;
        if args.len() > init.as_ref().map_or(0, |signature| signature.params.len()) {
            return Err(BuildError::at(BuildErrorKind::MissingType {
                what: "class 构造签名",
            })
            .with_site(self.module, span));
        }
        let mut operands = Vec::new();
        if let Some(signature) = &init {
            if !signature.receiver {
                return Err(BuildError::at(BuildErrorKind::MissingType {
                    what: "class __init 接收者",
                })
                .with_site(self.module, span));
            }
            for (arg, expected) in args.iter().zip(&signature.params) {
                operands.push(self.build_call_argument(arg, expected, block)?);
            }
            if args.len() < signature.params.len() {
                let declaration = self
                    .sema
                    .declarations
                    .named(*module, &format!("{name}::__init"))
                    .and_then(|id| self.sema.declarations.parameters(&self.sema.modules, id))
                    .ok_or_else(|| {
                        BuildError::at(BuildErrorKind::MissingType {
                            what: "构造默认实参声明",
                        })
                        .with_site(self.module, span)
                    })?;
                let params = declaration
                    .iter()
                    .filter(|param| param.name != "self")
                    .cloned()
                    .collect();
                let mut defaults_signature = signature.clone();
                defaults_signature.receiver = false;
                self.expand_default_arguments(
                    *module,
                    params,
                    &defaults_signature,
                    &mut operands,
                    args.len(),
                    span,
                    block,
                )?;
            }
        }
        let dst = self.fresh_local(ty.clone());
        // 分配先于字段初值，所有初值完成后才调用用户构造函数。
        block.push(HirStmt::Instr(HirInstr::New {
            ty: ty.clone(),
            init: None,
            args: Vec::new(),
            dst,
        }));
        self.initialize_class_fields(&ty, dst, span, block)?;
        if let Some(signature) = init {
            operands.insert(0, HirOperand::Local(dst));
            block.push(HirStmt::Instr(HirInstr::Call {
                callee: HirOperand::FunctionPtr(signature.id),
                args: operands,
                dst: None,
                dst_ty: None,
            }));
        }
        Ok(HirOperand::Local(dst))
    }

    fn initialize_class_fields(
        &mut self,
        ty: &HirType,
        receiver: LocalId,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        let HirType::Class(module, name) = ty else {
            unreachable!("new 已验证 class")
        };
        let mut chain = Vec::new();
        let mut current = Some((*module, name.clone()));
        while let Some((module, name)) = current {
            let summary = self
                .sema
                .types
                .summaries
                .iter()
                .find(|summary| summary.module == module && summary.name == name)
                .ok_or_else(|| {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "class 字段定义",
                    })
                    .with_site(self.module, span)
                })?;
            chain.push((module, name));
            current = summary.parent.clone();
        }
        let fields = instance_fields(self.sema, *module, name, span)?;
        let mut index = 0;
        for (module, name) in chain.into_iter().rev() {
            let members = self
                .sema
                .declarations
                .named(module, &name)
                .and_then(|id| self.sema.declarations.members(&self.sema.modules, id))
                .ok_or_else(|| {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "class 字段声明",
                    })
                    .with_site(self.module, span)
                })?
                .to_vec();
            for member in members {
                if let ClassMember::Field(field) = member {
                    if let Some(expr) = field.initializer {
                        let field_ty = &fields[index].ty;
                        let value = self.in_definition_scope(
                            module,
                            vec![("self".to_owned(), receiver)],
                            |this| this.build_typed_expression(&expr, field_ty, block),
                        )?;
                        block.push(HirStmt::Instr(HirInstr::FieldStore {
                            target: HirOperand::Local(receiver),
                            target_ty: ty.clone(),
                            field: field.name,
                            index,
                            value,
                            is_weak: fields[index].is_weak,
                        }));
                    }
                    index += 1;
                }
            }
        }
        Ok(())
    }

    pub(super) fn class_method_signature(
        &self,
        module: novella_sema::ModuleId,
        type_name: &str,
        method_name: &str,
        span: Span,
    ) -> Result<Option<FunctionSignature>, BuildError> {
        let Some(summary) = self
            .sema
            .types
            .summaries
            .iter()
            .find(|summary| summary.module == module && summary.name == type_name)
        else {
            return Ok(None);
        };
        let Some(method) = summary
            .methods
            .iter()
            .find(|method| method.name == method_name)
        else {
            // __str 等实例方法遵循名义继承链；构造器仍按当前类的声明契约处理。
            if method_name != "__init"
                && let Some((parent_module, parent_name)) = &summary.parent
            {
                return self.class_method_signature(*parent_module, parent_name, method_name, span);
            }
            return Ok(None);
        };
        FunctionSignature::from_view(&method.signature, &mut function_id_of)
            .map(Some)
            .map_err(|error| error.with_site(self.module, span))
    }

    pub(super) fn build_call(
        &mut self,
        callee: &Expression,
        args: &[Expression],
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let HirType::Function(signature) = self.expr_type(callee.span())? else {
            return Err(BuildError::at(BuildErrorKind::MissingType {
                what: "调用签名"
            })
            .with_site(self.module, span));
        };
        if signature.params.len() < args.len() {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "omitted default arguments",
            })
            .with_site(self.module, span));
        }
        let mut operands = Vec::new();
        let callee_operand = if signature.receiver {
            let target = match callee {
                Expression::MemberAccess { target, .. } => target.as_ref().clone(),
                Expression::Identifier { span, .. } => Expression::This { span: *span },
                _ => {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "indirect bound method call",
                    })
                    .with_site(self.module, span));
                }
            };
            let target_ty = match &target {
                Expression::This { .. } => self
                    .lookup_local("self")
                    .map(|id| self.local_type(id))
                    .ok_or_else(|| {
                        BuildError::at(BuildErrorKind::MissingType {
                            what: "方法接收者"
                        })
                        .with_site(self.module, span)
                    })?,
                _ => self.expr_type(target.span())?,
            };
            let receiver = if matches!(target, Expression::This { .. }) {
                HirOperand::Local(self.lookup_local("self").expect("接收者已检查"))
            } else {
                match target_ty {
                    HirType::Struct(..) => self.aggregate_address(&target, block)?,
                    // class 值本身就是运行时对象引用，实例方法按引用传递。
                    HirType::Class(..) => self.build_expression(&target, block)?,
                    HirType::Pointer(_) => self.build_expression(&target, block)?,
                    _ => {
                        return Err(BuildError::at(BuildErrorKind::Unimplemented {
                            node: "non-aggregate method receiver",
                        })
                        .with_site(self.module, span));
                    }
                }
            };
            operands.push(receiver);
            HirOperand::FunctionPtr(signature.id.clone())
        } else {
            self.build_expression(callee, block)?
        };
        for (arg, expected) in args.iter().zip(&signature.params) {
            operands.push(self.build_call_argument(arg, expected, block)?);
        }
        if args.len() < signature.params.len() {
            self.append_default_arguments(
                callee.span(),
                &signature,
                &mut operands,
                args.len(),
                span,
                block,
            )?;
        }
        let ret = signature.ret.clone();
        let dst = if ret == HirType::Void {
            None
        } else {
            Some(self.fresh_local(ret.clone()))
        };
        block.push(HirStmt::Instr(HirInstr::Call {
            callee: callee_operand,
            args: operands,
            dst,
            dst_ty: dst.map(|_| ret),
        }));
        Ok(dst.map_or(HirOperand::Unit, HirOperand::Local))
    }
    /// 默认实参从原始声明取表达式；调用处只替换此前参数的已求值快照。
    pub(super) fn build_call_argument(
        &mut self,
        arg: &Expression,
        expected: &HirType,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        if let HirType::Pointer(pointee) = expected {
            let actual = self.expr_type_pref(arg.span(), Some(expected))?;
            if let HirType::Array(element, _) = &actual
                && same_value_type(pointee, element)
            {
                let address = self.aggregate_address(arg, block)?;
                return self.coerce(
                    address,
                    &HirType::Pointer(Box::new(actual)),
                    expected,
                    arg.span(),
                    block,
                );
            }
        }
        self.build_typed_expression(arg, expected, block)
    }

    fn append_default_arguments(
        &mut self,
        callee_span: Span,
        signature: &FunctionSignature,
        operands: &mut Vec<HirOperand>,
        provided: usize,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        let declaration = match self.sema.expression_types.get(self.module, callee_span) {
            Some(novella_sema::TypeView::Function(view)) => view
                .name
                .as_ref()
                .and_then(|name| self.sema.declarations.named(view.module, name))
                .and_then(|id| {
                    self.sema
                        .declarations
                        .parameters(&self.sema.modules, id)
                        .map(|params| (id.module, params.to_vec()))
                }),
            _ => None,
        };
        let (module, mut params) = declaration.ok_or_else(|| {
            BuildError::at(BuildErrorKind::Unimplemented {
                node: "defaults of indirect function",
            })
            .with_site(self.module, span)
        })?;
        if signature.receiver && params.first().is_some_and(|param| param.name == "self") {
            params.remove(0);
        }
        self.expand_default_arguments(module, params, signature, operands, provided, span, block)
    }

    /// 在定义环境展开默认值，并把先前实参保存为可供后续默认表达式引用的局部值。
    #[allow(clippy::too_many_arguments)]
    fn expand_default_arguments(
        &mut self,
        module: novella_sema::ModuleId,
        params: Vec<novella_ast::types::Param>,
        signature: &FunctionSignature,
        operands: &mut Vec<HirOperand>,
        provided: usize,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        if params.len() != signature.params.len() {
            return Err(BuildError::at(BuildErrorKind::MissingType {
                what: "默认实参签名",
            })
            .with_site(self.module, span));
        }
        let mut bindings = Vec::new();
        let receiver_count = usize::from(signature.receiver);
        if signature.receiver
            && let Some(HirOperand::Local(id)) = operands.first()
        {
            bindings.push(("self".to_string(), *id));
        }
        for (index, param) in params.iter().enumerate() {
            let ty = &signature.params[index];
            let value = if index < provided {
                operands[index + receiver_count].clone()
            } else {
                let expr = param.default.as_ref().ok_or_else(|| {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "默认实参"
                    })
                    .with_site(module, param.span)
                })?;
                let value = self.in_definition_scope(module, bindings.clone(), |this| {
                    this.build_typed_expression(expr, ty, block)
                })?;
                operands.push(value.clone());
                value
            };
            let id = self.fresh_local(ty.clone());
            block.push(HirStmt::Instr(HirInstr::Alloc {
                id,
                ty: ty.clone(),
                init: Some(value),
            }));
            bindings.push((param.name.clone(), id));
        }
        Ok(())
    }
}

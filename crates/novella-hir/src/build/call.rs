//! 已解析签名的调用：定型实参、显式传递 struct 接收者。

use super::*;

pub(super) struct IndexPlace {
    pub(super) receiver: HirOperand,
    pub(super) key: HirOperand,
    pub(super) receiver_ty: HirType,
}

mod target;
use target::CallTarget;

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
        _type_args: &[TypeExpr],
        args: &[Expression],
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let ty = self.expr_type(span)?;
        let HirType::Class(module, name) = &ty else {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "new 的目标不是 class",
            })
            .with_site(self.module, span));
        };
        let (summary, subst) =
            mono::aggregate_definition(self.sema, &self.instances, *module, name).ok_or_else(
                || {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "class 实例定义",
                    })
                    .with_site(self.module, span)
                },
            )?;
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
                    .named(*module, &format!("{}::__init", summary.name))
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
                    subst.clone(),
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
        let chain = layout::instance_lineage(self.sema, &self.instances, *module, name, span)?;
        let fields = instance_fields(self.sema, &self.instances, *module, name, span)?;
        let mut index = 0;
        for (definition, subst) in chain {
            let module = definition.module;
            let members = self
                .sema
                .declarations
                .named(module, &definition.name)
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
                            subst.clone(),
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
        let Some((summary, subst)) =
            mono::aggregate_definition(self.sema, &self.instances, module, type_name)
        else {
            return Ok(None);
        };
        if let Some(method) = summary
            .methods
            .iter()
            .find(|method| method.name == method_name)
        {
            return mono::instantiate_function(
                self.sema,
                &self.instances,
                &method.signature,
                subst,
            )
            .map(Some)
            .map_err(|error| error.with_site(self.module, span));
        }
        // 构造器只查本类；其它隐式协议跟随已实例化的父链。
        if method_name != "__init"
            && let Some((module, name)) =
                layout::instance_parent(self.sema, &self.instances, summary, &subst)?
        {
            return self.class_method_signature(module, &name, method_name, span);
        }
        Ok(None)
    }

    pub(super) fn build_call(
        &mut self,
        callee: &Expression,
        args: &[Expression],
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let instance = self.sema.call_instances.get(&(self.module, span));
        let mut substitutions = instance
            .filter(|instance| instance.signature.name.is_some())
            .map_or_else(
                || Ok(Vec::new()),
                |instance| {
                    mono::concrete_substitution(
                        self.sema,
                        &self.instances,
                        &instance.substitutions,
                        &self.subst,
                    )
                },
            )?;
        let signature_site = |error: BuildError| error.with_site(self.module, span);
        let (signature, target_kind) = if let Some(instance) = instance {
            self.resolve_call_instance(instance, &mut substitutions, span)
                .map_err(signature_site)?
        } else {
            let HirType::Function(signature) = self.expr_type(callee.span())? else {
                return Err(BuildError::at(BuildErrorKind::MissingType {
                    what: "调用签名"
                })
                .with_site(self.module, span));
            };
            (*signature, CallTarget::Expression)
        };
        let mut callee = callee;
        while let Expression::Paren { expr, .. } = callee {
            callee = expr;
        }
        if signature.params.len() < args.len() {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "omitted default arguments",
            })
            .with_site(self.module, span));
        }
        let mut operands = Vec::new();
        let mut dyn_receiver = None;
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
                    HirType::Class(..) | HirType::Dynamic(_) => {
                        self.build_expression(&target, block)?
                    }
                    HirType::Pointer(_) => self.build_expression(&target, block)?,
                    _ => {
                        return Err(BuildError::at(BuildErrorKind::Unimplemented {
                            node: "non-aggregate method receiver",
                        })
                        .with_site(self.module, span));
                    }
                }
            };
            dyn_receiver = Some((receiver.clone(), target_ty));
            operands.push(receiver);
            HirOperand::FunctionPtr(signature.id.clone())
        } else if matches!(target_kind, CallTarget::Direct { .. }) {
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
                substitutions,
                &signature,
                &mut operands,
                span,
                block,
            )?;
        }
        let ret = match &target_kind {
            CallTarget::Direct { return_type } => return_type.clone(),
            _ => signature.ret.clone(),
        };
        let dst = if ret == HirType::Void {
            None
        } else {
            Some(self.fresh_local(ret.clone()))
        };
        let declared_return = signature.ret.clone();
        let instruction = if let CallTarget::Dynamic(slot) = target_kind {
            let (receiver, receiver_ty) = dyn_receiver.expect("动态调用总有接收者");
            operands.remove(0);
            HirInstr::DynCall {
                receiver,
                receiver_ty,
                slot,
                signature: Box::new(signature),
                args: operands,
                dst,
            }
        } else {
            HirInstr::Call {
                callee: callee_operand,
                args: operands,
                dst,
                dst_ty: dst.map(|_| ret.clone()),
            }
        };
        block.push(HirStmt::Instr(instruction));
        if let Some(dst) = dst {
            self.coerce(HirOperand::Local(dst), &ret, &declared_return, span, block)
        } else {
            Ok(HirOperand::Unit)
        }
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
                && pointee.same_value_type(element)
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
        subst: Substitution,
        signature: &FunctionSignature,
        operands: &mut Vec<HirOperand>,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        let provided = operands.len() - usize::from(signature.receiver);
        let declaration = match self.sema.expression_types.get(self.module, callee_span) {
            Some(TypeView::Function(view)) => view.name.as_ref().and_then(|name| {
                let id = self.sema.declarations.named(view.module, name)?;
                let params = self.sema.declarations.parameters(&self.sema.modules, id)?;
                Some((id.module, name.clone(), params.to_vec()))
            }),
            _ => None,
        };
        let (module, symbol, mut params) = declaration.ok_or_else(|| {
            BuildError::at(BuildErrorKind::Unimplemented {
                node: "defaults of indirect function",
            })
            .with_site(self.module, span)
        })?;
        // 继承方法的默认表达式在父类布局中解释 self；实际调用仍保留原接收者。
        if signature.receiver
            && let Some(HirOperand::Local(id)) = operands.first()
            && let Some((owner, _)) = symbol.split_once("::")
        {
            let actual = self.local_type(*id);
            if matches!(actual, HirType::Class(..)) {
                let expected =
                    mono::method_receiver(self.sema, &self.instances, module, owner, &subst)?;
                operands[0] =
                    self.coerce(HirOperand::Local(*id), &actual, &expected, span, block)?;
            }
        }
        if signature.receiver && params.first().is_some_and(|param| param.name == "self") {
            params.remove(0);
        }
        self.expand_default_arguments(
            module, subst, params, signature, operands, provided, span, block,
        )
    }

    /// 在定义环境展开默认值，并把先前实参保存为可供后续默认表达式引用的局部值。
    #[allow(clippy::too_many_arguments)]
    fn expand_default_arguments(
        &mut self,
        module: novella_sema::ModuleId,
        subst: Substitution,
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
                let value =
                    self.in_definition_scope(module, bindings.clone(), subst.clone(), |this| {
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
            operands[index + receiver_count] = HirOperand::Local(id);
            bindings.push((param.name.clone(), id));
        }
        Ok(())
    }
}

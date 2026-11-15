//! 已解析签名的调用：定型实参、显式传递 struct 接收者。

use super::*;

impl BodyBuilder<'_> {
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
            if matches!(target_ty, HirType::Class(..)) {
                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                    node: "class method dispatch",
                })
                .with_site(self.module, span));
            }
            let receiver = if matches!(target, Expression::This { .. }) {
                HirOperand::Local(self.lookup_local("self").expect("接收者已检查"))
            } else {
                match target_ty {
                    HirType::Struct(..) => self.aggregate_address(&target, block)?,
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
            operands.push(self.build_typed_expression(arg, expected, block)?);
        }
        if args.len() < signature.params.len() {
            self.append_default_arguments(&signature, &mut operands, args.len(), span, block)?;
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
    fn append_default_arguments(
        &mut self,
        signature: &FunctionSignature,
        operands: &mut Vec<HirOperand>,
        provided: usize,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        // id 不作为类型名解析来源：语义 FunctionView 已给出 module + name。
        let declaration = self.sema.modules.modules.iter().find_map(|module| {
            module.program.items.iter().find_map(|item| match item {
                Item::Function(function)
                    if signature.id
                        == HirFunctionId(format!("m{}::{}", module.id, function.name)) =>
                {
                    function
                        .r#type
                        .as_ref()
                        .map(|ty| (module.id, ty.params.clone()))
                }
                Item::Struct(item) => item.members.iter().find_map(|member| match member {
                    ClassMember::Method(method)
                        if signature.id
                            == HirFunctionId(format!(
                                "m{}::{}::{}",
                                module.id, item.name, method.name
                            )) =>
                    {
                        Some((module.id, method.params.clone()))
                    }
                    _ => None,
                }),
                Item::Class(item) => item.members.iter().find_map(|member| match member {
                    ClassMember::Method(method)
                        if signature.id
                            == HirFunctionId(format!(
                                "m{}::{}::{}",
                                module.id, item.name, method.name
                            )) =>
                    {
                        Some((module.id, method.params.clone()))
                    }
                    _ => None,
                }),
                _ => None,
            })
        });
        let (module, mut params) = declaration.ok_or_else(|| {
            BuildError::at(BuildErrorKind::Unimplemented {
                node: "defaults of indirect function",
            })
            .with_site(self.module, span)
        })?;
        let mut bindings = Vec::new();
        let receiver_count = usize::from(signature.receiver);
        if signature.receiver && params.first().is_some_and(|param| param.name == "self") {
            params.remove(0);
        }
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

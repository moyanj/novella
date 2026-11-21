//! HIR 降级：aggregate 职责实现。

use super::*;

impl<'a> BodyBuilder<'a> {
    /// 显式字段按源码求值，缺省字段随后按声明顺序在定义模块求值。
    pub(super) fn build_struct_init(
        &mut self,
        _name: &str,
        properties: &[novella_ast::expr::PropertyAssignment],
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let ty = self.expr_type(span)?;
        let HirType::Struct(module, name) = &ty else {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "non-struct construction",
            })
            .with_site(self.module, span));
        };
        let fields = instance_fields(self.sema, *module, name, span)?;
        let mut slots = Vec::new();
        for property in properties {
            let expr = property
                .value
                .clone()
                .unwrap_or_else(|| Expression::Identifier {
                    name: property.name.clone(),
                    span: property.span,
                });
            let (index, field_ty) = self.field_slot(&ty, &property.name, property.span)?;
            let value = self.build_typed_expression(&expr, &field_ty, block)?;
            slots.push((index, value));
        }
        let definition = self
            .sema
            .declarations
            .named(*module, name)
            .and_then(|id| self.sema.declarations.members(&self.sema.modules, id))
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "struct 定义",
                })
                .with_site(self.module, span)
            })?;
        for member in definition {
            if let ClassMember::Field(field) = member {
                let (index, field_ty) = self.field_slot(&ty, &field.name, field.span)?;
                if slots.iter().any(|(slot, _)| *slot == index) {
                    continue;
                }
                let expr = field.initializer.as_ref().ok_or_else(|| {
                    BuildError::at(BuildErrorKind::MissingType {
                        what: "字段初值"
                    })
                    .with_site(*module, field.span)
                })?;
                let value = self.in_definition_scope(*module, Vec::new(), |this| {
                    this.build_typed_expression(expr, &field_ty, block)
                })?;
                slots.push((index, value));
            }
        }
        let id = self.fresh_local(ty.clone());
        block.push(HirStmt::Instr(HirInstr::Alloc {
            id,
            ty: ty.clone(),
            init: None,
        }));
        for (index, value) in slots {
            block.push(HirStmt::Instr(HirInstr::FieldStore {
                target: HirOperand::Local(id),
                target_ty: ty.clone(),
                field: fields[index].name.clone(),
                index,
                value,
            }));
        }
        Ok(HirOperand::Local(id))
    }

    /// subject 只求一次；每个模式的前置指令留在自己的条件中，匹配后不再求后续模式。
    pub(super) fn build_match(
        &mut self,
        node: &Match,
        preferred: Option<&HirType>,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let result_ty = self.expr_type_pref(node.span, preferred)?;
        let dst = if result_ty == HirType::Void {
            None
        } else {
            Some(self.fresh_local(result_ty.clone()))
        };
        if let Some(id) = dst {
            block.push(HirStmt::Instr(HirInstr::Alloc {
                id,
                ty: result_ty.clone(),
                init: None,
            }));
        }
        let exhaustive = self
            .sema
            .exhaustive_matches
            .contains(&(self.module, node.span));
        if dst.is_some() && node.default.is_none() && !exhaustive {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "value match without default",
            })
            .with_site(self.module, node.span));
        }
        let subject_ty = self.expr_type(node.subject.span())?;
        let subject = self.build_expression(&node.subject, block)?;
        let mut tail = node
            .default
            .as_ref()
            .map(|body| self.match_body_block(body, dst, &result_ty))
            .transpose()?;
        let mut cases = node.cases.as_slice();
        if exhaustive
            && node.default.is_none()
            && let Some((last, rest)) = cases.split_last()
        {
            tail = Some(self.match_body_block(&last.body, dst, &result_ty)?);
            cases = rest;
        }
        for case in cases.iter().rev() {
            let mut prelude = HirBlock::new();
            let test = self.fresh_local(HirType::Boolean);
            match &case.pattern {
                MatchPattern::Expr { expr, .. } => {
                    let pattern = self.build_typed_expression(expr, &subject_ty, &mut prelude)?;
                    prelude.push(HirStmt::Instr(HirInstr::Binary {
                        op: HirBinaryOp::Eq,
                        lhs_ty: subject_ty.clone(),
                        rhs_ty: subject_ty.clone(),
                        lhs: subject.clone(),
                        rhs: pattern,
                        dst: test,
                        dst_ty: HirType::Boolean,
                    }));
                }
                MatchPattern::Type { ty, .. } => {
                    let target = annotation_type(self.sema, self.module, ty)?;
                    prelude.push(HirStmt::Instr(HirInstr::TypeTest {
                        operand: subject.clone(),
                        operand_ty: subject_ty.clone(),
                        target,
                        dst: test,
                    }));
                }
            }
            let then_block = self.match_body_block(&case.body, dst, &result_ty)?;
            let mut wrapper = HirBlock::new();
            wrapper.push(HirStmt::If {
                cond: HirCond {
                    prelude,
                    value: HirOperand::Local(test),
                },
                then_block,
                else_block: tail,
            });
            tail = Some(wrapper);
        }
        if let Some(body) = tail {
            block.push(HirStmt::Block(body));
        }
        Ok(dst.map_or(HirOperand::Unit, HirOperand::Local))
    }

    fn match_body_block(
        &mut self,
        body: &MatchCaseBody,
        dst: Option<LocalId>,
        ty: &HirType,
    ) -> Result<HirBlock, BuildError> {
        self.with_scope(|this| {
            let mut block = HirBlock::new();
            match body {
                MatchCaseBody::Expr { expr, .. } => {
                    let value = this.build_typed_expression(expr, ty, &mut block)?;
                    if let Some(id) = dst {
                        block.push(HirStmt::Instr(HirInstr::Store { id, value }));
                    }
                }
                MatchCaseBody::Block { block: body, span } => {
                    if dst.is_some() {
                        return Err(BuildError::at(BuildErrorKind::Unimplemented {
                            node: "block branch in value match",
                        })
                        .with_site(this.module, *span));
                    }
                    this.emit_block(body, &mut block)?;
                }
            }
            Ok(block)
        })
    }

    /// lambda：降级成同模块的一个独立函数项，表达式位置只取它的函数指针。
    ///
    /// 捕获分析暂不实现（闭包环境需要引用计数与堆分配，属后续期）；
    /// 因此当前只允许不捕获外层局部变量的 lambda，捕获一律 fail-fast，
    /// 不生成「看起来能用」的错误代码。
    pub(super) fn build_lambda(&mut self, node: &Lambda) -> Result<HirOperand, BuildError> {
        let ty = self.expr_type_pref(node.span, None)?;
        let HirType::Function(mut signature) = ty else {
            return Err(BuildError::at(BuildErrorKind::Unimplemented {
                node: "lambda without function type",
            })
            .with_site(self.module, node.span));
        };
        signature.id = HirFunctionId(format!("m{}::lambda#{}", self.module, node.span.start));
        let mut inner = BodyBuilder::new(self.sema, self.module, self.functions.clone());
        inner.return_type = Some(signature.ret.clone());
        inner.capture_names = self
            .locals
            .iter()
            .map(|(name, _)| name.clone())
            .chain(self.capture_names.iter().cloned())
            .collect();
        // 参数名不同形态：单标识符简写 / 参数列表
        let mut names = Vec::new();
        match &node.params {
            novella_ast::expr::LambdaParams::Single { name, .. } => names.push(name.clone()),
            novella_ast::expr::LambdaParams::List { params, .. } => {
                for param in params {
                    names.push(param.name.clone());
                }
            }
        }
        for (index, name) in names.into_iter().enumerate() {
            let param_ty = signature.params.get(index).cloned().ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "lambda 形参类型",
                })
                .with_site(self.module, node.span)
            })?;
            inner.declare_param(name, param_ty);
        }
        let body = match &node.body {
            novella_ast::expr::LambdaBody::Expr { expr, .. } => {
                let mut block = HirBlock::new();
                let value = inner.build_typed_expression(expr, &signature.ret, &mut block)?;
                block.push(HirStmt::Instr(HirInstr::Return(Some(value))));
                block
            }
            novella_ast::expr::LambdaBody::Block { block, .. } => {
                inner.build_statement(&Statement::Block(block.clone()))?
            }
        };
        let params = inner.parameter_slots();
        let lambdas = std::mem::take(&mut inner.lambdas);
        self.lambdas.push(HirFunction {
            source_module: self.module,
            is_lambda: true,
            id: signature.id.clone(),
            link_name: None,
            signature: (*signature).clone(),
            params,
            body,
            visibility: HirVisibility::Private,
        });
        self.lambdas.extend(lambdas);
        Ok(HirOperand::FunctionPtr(signature.id.clone()))
    }

    /// 字面量：直接求值为常量。
    pub(super) fn build_literal(
        &self,
        literal: &Literal,
        span: Span,
    ) -> Result<HirOperand, BuildError> {
        match literal {
            Literal::Null => Ok(HirOperand::Null),
            Literal::Boolean(value) => Ok(HirOperand::BoolConst(*value)),
            Literal::String(raw) => {
                if raw.starts_with('f') || raw.starts_with('b') {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "格式化或字节字符串",
                    })
                    .with_site(self.module, span));
                }
                // 解码表在 `novella_core::string`（sema 校验非法转义时用同一份实现），
                // 此处只负责把解码失败转成降级错误
                novella_core::string::decode_string_literal(raw)
                    .map(|text| HirOperand::StringConst(text.encode_utf16().collect()))
                    .map_err(|_error| {
                        // 语义阶段已校验过转义；走到这里说明 sema 与 core 解码表不一致
                        BuildError::at(BuildErrorKind::Unimplemented {
                            node: "string literal with unsupported escape",
                        })
                        .with_site(self.module, span)
                    })
            }
            Literal::Number(raw) => {
                let text: String = raw.chars().filter(|&c| c != '_').collect();
                let lower = text.to_ascii_lowercase();
                let is_based =
                    lower.starts_with("0x") || lower.starts_with("0o") || lower.starts_with("0b");
                if !is_based && (text.contains('.') || lower.contains('e')) {
                    let value: f64 = text.parse().map_err(|_| {
                        BuildError::at(BuildErrorKind::UntypedLiteral { what: "float" })
                            .with_site(self.module, span)
                    })?;
                    return Ok(HirOperand::UntypedFloat { value, span });
                }
                let value = parse_int_literal(&text).ok_or_else(|| {
                    BuildError::at(BuildErrorKind::UntypedLiteral { what: "integer" })
                        .with_site(self.module, span)
                })?;
                Ok(HirOperand::UntypedInt { value, span })
            }
        }
    }
}

//! 标准库字符串操作：仅使用 sema 提供的名义身份和已解析插值。
use super::*;

fn unsigned_literal(expr: &Expression) -> bool {
    match expr {
        Expression::Paren { expr, .. }
        | Expression::Unary {
            op: UnaryOp::Plus,
            operand: expr,
            ..
        } => unsigned_literal(expr),
        Expression::Literal {
            value: Literal::Number(raw),
            ..
        } => parse_int_literal(&raw.replace('_', ""))
            .is_some_and(|value| value > i128::from(i64::MAX)),
        _ => false,
    }
}

impl BodyBuilder<'_> {
    pub(super) fn string_method_call(
        &mut self,
        ty: &HirType,
        method: &str,
        args: Vec<HirOperand>,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let HirType::Class(module, name) = ty else {
            return Err(BuildError::at(BuildErrorKind::MissingType {
                what: "String 类型",
            })
            .with_site(self.module, span));
        };
        let signature = self
            .class_method_signature(*module, name, method, span)?
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "String 转换/运算方法",
                })
                .with_site(self.module, span)
            })?;
        let dst = self.fresh_local(signature.ret.clone());
        block.push(HirStmt::Instr(HirInstr::Call {
            callee: HirOperand::FunctionPtr(signature.id),
            args,
            dst: Some(dst),
            dst_ty: Some(signature.ret),
        }));
        Ok(HirOperand::Local(dst))
    }

    pub(super) fn build_fstring(
        &mut self,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        let ty = self.expr_type(span)?;
        let parts = self
            .sema
            .fstrings
            .get(&(self.module, span))
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "f-string 已检查片段",
                })
                .with_site(self.module, span)
            })?
            .clone();
        let empty = self.fresh_local(ty.clone());
        block.push(HirStmt::Instr(HirInstr::StringLiteral {
            value: Vec::new(),
            ty: ty.clone(),
            dst: empty,
        }));
        let mut result = HirOperand::Local(empty);
        for part in parts {
            let value = match part {
                novella_sema::FStringPart::Text(text) => {
                    let dst = self.fresh_local(ty.clone());
                    block.push(HirStmt::Instr(HirInstr::StringLiteral {
                        value: text.encode_utf16().collect(),
                        ty: ty.clone(),
                        dst,
                    }));
                    HirOperand::Local(dst)
                }
                novella_sema::FStringPart::Expression(expr) => {
                    let hint = match self.sema.expression_types.get(self.module, expr.span()) {
                        Some(novella_sema::TypeView::IntLiteral) => {
                            let unsigned = unsigned_literal(&expr);
                            Some(HirType::Int(if unsigned { IntTy::U64 } else { IntTy::I64 }))
                        }
                        Some(novella_sema::TypeView::FloatLiteral) => {
                            Some(HirType::Float(crate::ty::FloatTy::F64))
                        }
                        _ => None,
                    };
                    let actual = self.expr_type_pref(expr.span(), hint.as_ref())?;
                    let value = self.build_typed_expression(&expr, &actual, block)?;
                    if operators::same_string_class(self.sema, &actual) {
                        value
                    } else {
                        match &actual {
                            HirType::Boolean => self.string_method_call(
                                &ty,
                                "fromBoolean",
                                vec![value],
                                span,
                                block,
                            )?,
                            HirType::Int(kind) => {
                                let unsigned = matches!(
                                    kind,
                                    IntTy::U8 | IntTy::U16 | IntTy::U32 | IntTy::U64 | IntTy::Usize
                                );
                                let integer_ty =
                                    HirType::Int(if unsigned { IntTy::U64 } else { IntTy::I64 });
                                let value =
                                    self.coerce(value, &actual, &integer_ty, expr.span(), block)?;
                                self.string_method_call(
                                    &ty,
                                    if unsigned {
                                        "fromUnsigned"
                                    } else {
                                        "fromInteger"
                                    },
                                    vec![value],
                                    span,
                                    block,
                                )?
                            }
                            HirType::Float(
                                kind @ (crate::ty::FloatTy::F32 | crate::ty::FloatTy::F64),
                            ) => self.string_method_call(
                                &ty,
                                if *kind == crate::ty::FloatTy::F32 {
                                    "fromFloat32"
                                } else {
                                    "fromFloat64"
                                },
                                vec![value],
                                span,
                                block,
                            )?,
                            HirType::Class(module, name) | HirType::Struct(module, name) => {
                                let signature = self
                                    .class_method_signature(*module, name, "__str", expr.span())?
                                    .ok_or_else(|| {
                                        BuildError::at(BuildErrorKind::MissingType {
                                            what: "__str 方法",
                                        })
                                        .with_site(self.module, expr.span())
                                    })?;
                                let receiver = if matches!(actual, HirType::Struct(..)) {
                                    let local = self.fresh_local(actual.clone());
                                    block.push(HirStmt::Instr(HirInstr::Alloc {
                                        id: local,
                                        ty: actual.clone(),
                                        init: Some(value),
                                    }));
                                    let address = self
                                        .fresh_local(HirType::Pointer(Box::new(actual.clone())));
                                    block.push(HirStmt::Instr(HirInstr::LocalAddress {
                                        local,
                                        ty: actual.clone(),
                                        dst: address,
                                    }));
                                    HirOperand::Local(address)
                                } else {
                                    value
                                };
                                let dst = self.fresh_local(ty.clone());
                                block.push(HirStmt::Instr(HirInstr::Call {
                                    callee: HirOperand::FunctionPtr(signature.id),
                                    args: vec![receiver],
                                    dst: Some(dst),
                                    dst_ty: Some(ty.clone()),
                                }));
                                HirOperand::Local(dst)
                            }
                            _ => {
                                return Err(BuildError::at(BuildErrorKind::Unimplemented {
                                    node: "f-string 转换",
                                })
                                .with_site(self.module, expr.span()));
                            }
                        }
                    }
                }
            };
            result = self.string_method_call(&ty, "concat", vec![result, value], span, block)?;
        }
        Ok(result)
    }
}

//! 无托管引用的静态存储：常量求值不执行函数体，也不引入启动初始化函数。
use super::*;
use crate::item::HirGlobal;
use novella_ast::stmt::VariableType;

fn unsupported(module: ModuleId, span: Span) -> BuildError {
    BuildError::at(BuildErrorKind::Unimplemented {
        node: "non-constant or managed global initializer",
    })
    .with_site(module, span)
}

fn global<'a>(sema: &'a SemaResult, module: ModuleId, name: &str) -> Option<&'a VariableStmt> {
    sema.modules.get(module)?.program.items.iter().find_map(|item| {
        let Item::Statement(statement) = item else { return None; };
        match statement.as_ref() {
            Statement::Variable(decl) => std::slice::from_ref(decl),
            Statement::VariableList { declarations, .. } => declarations.as_slice(),
            _ => return None,
        }.iter().find(|decl| matches!(&decl.pattern, BindingPattern::Ident { name: declared, .. } if declared == name))
    })
}

/// 先使用真实绑定，防止导入别名、命名空间与局部遮蔽改变全局身份。
pub(super) fn address(sema: &SemaResult, module: ModuleId, span: Span) -> Option<HirOperand> {
    let definition = sema.bindings.references.get(&(module, span))?;
    if definition.kind != novella_sema::DefinitionKind::Variable {
        return None;
    }
    let declaration = global(sema, definition.module, &definition.name)?;
    if !matches!(&declaration.pattern, BindingPattern::Ident { span, .. } if *span == definition.span)
    {
        return None;
    }
    Some(HirOperand::GlobalAddress {
        module: definition.module,
        name: definition.name.clone(),
    })
}

pub(super) fn build_global(
    sema: &SemaResult,
    module: ModuleId,
    declaration: &VariableStmt,
) -> Result<HirGlobal, BuildError> {
    let BindingPattern::Ident { name, .. } = &declaration.pattern else {
        return Err(unsupported(module, declaration.span));
    };
    let view = sema
        .declaration_types
        .get(module, declaration.span)
        .ok_or_else(|| unsupported(module, declaration.span))?;
    let ty = HirType::from_view(view, &mut function_id_of)
        .map_err(|e| e.with_site(module, declaration.span))?;
    if super::requirements::managed(sema, &ty) {
        return Err(unsupported(module, declaration.span));
    }
    let init = declaration
        .value
        .as_ref()
        .map(|expr| constant(sema, module, expr, &ty, &mut HashSet::new()))
        .transpose()?;
    Ok(HirGlobal {
        name: name.clone(),
        ty,
        init,
        is_mutable: declaration.r#type == VariableType::Let,
        visibility: visibility_of(declaration.scope),
    })
}

fn constant(
    sema: &SemaResult,
    module: ModuleId,
    expr: &Expression,
    expected: &HirType,
    visiting: &mut HashSet<(ModuleId, Span)>,
) -> Result<HirOperand, BuildError> {
    let fail = || unsupported(module, expr.span());
    match expr {
        Expression::Literal { value, .. } => match value {
            Literal::Number(text) => {
                if let Some(value) = parse_int_literal(text) {
                    Ok(HirOperand::IntConst(value))
                } else {
                    text.replace('_', "")
                        .parse()
                        .map(HirOperand::FloatConst)
                        .map_err(|_| fail())
                }
            }
            Literal::Boolean(value) => Ok(HirOperand::BoolConst(*value)),
            Literal::Null => Ok(HirOperand::Null),
            _ => Err(fail()),
        },
        Expression::Paren { expr, .. } => constant(sema, module, expr, expected, visiting),
        Expression::Cast { expr: operand, .. } => {
            // 指针常量保留整数载荷，后端依据目标 Pointer 类型生成常量 inttoptr。
            let view = sema
                .expression_types
                .get(module, expr.span())
                .ok_or_else(fail)?;
            let target = HirType::from_view(view, &mut function_id_of).map_err(|_| fail())?;
            let value = constant(sema, module, operand, &target, visiting)?;
            match (value, target) {
                (HirOperand::IntConst(value), HirType::Pointer(_)) => {
                    Ok(HirOperand::IntConst(value))
                }
                (HirOperand::IntConst(value), HirType::Int(kind)) => {
                    let bits = kind.bit_width();
                    let mask = (1_i128 << bits) - 1;
                    let value = value & mask;
                    let signed = kind.is_signed() && value & (1_i128 << (bits - 1)) != 0;
                    Ok(HirOperand::IntConst(if signed {
                        value - (1_i128 << bits)
                    } else {
                        value
                    }))
                }
                (HirOperand::IntConst(value), HirType::Float(_)) => {
                    Ok(HirOperand::FloatConst(value as f64))
                }
                (value @ HirOperand::FloatConst(_), HirType::Float(_)) => Ok(value),
                (
                    value @ (HirOperand::GlobalAddress { .. } | HirOperand::Null),
                    HirType::Pointer(_),
                ) => Ok(value),
                _ => Err(fail()),
            }
        }
        Expression::Unary {
            op: UnaryOp::AddressOf,
            operand,
            ..
        } => address(sema, module, operand.span()).ok_or_else(fail),
        Expression::Unary { op, operand, .. } => {
            let value = constant(sema, module, operand, expected, visiting)?;
            match (op, value) {
                (UnaryOp::Minus, HirOperand::IntConst(value)) => value
                    .checked_neg()
                    .map(HirOperand::IntConst)
                    .ok_or_else(fail),
                (UnaryOp::Plus, value) => Ok(value),
                (UnaryOp::BitNot, HirOperand::IntConst(value)) => Ok(HirOperand::IntConst(!value)),
                (UnaryOp::Not, HirOperand::BoolConst(value)) => Ok(HirOperand::BoolConst(!value)),
                (UnaryOp::Minus, HirOperand::FloatConst(value)) => {
                    Ok(HirOperand::FloatConst(-value))
                }
                _ => Err(fail()),
            }
        }
        Expression::Identifier { .. } | Expression::MemberAccess { .. } => {
            if let Some(TypeView::Function(view)) = sema.expression_types.get(module, expr.span())
                && view.name.is_some()
            {
                return Ok(HirOperand::FunctionPtr(function_id_of(view)));
            }
            let HirOperand::GlobalAddress {
                module: owner,
                name,
            } = address(sema, module, expr.span()).ok_or_else(fail)?
            else {
                unreachable!()
            };
            let declaration = global(sema, owner, &name).ok_or_else(fail)?;
            if declaration.r#type != VariableType::Const
                || !visiting.insert((owner, declaration.span))
            {
                return Err(fail());
            }
            let result = constant(
                sema,
                owner,
                declaration.value.as_ref().ok_or_else(fail)?,
                expected,
                visiting,
            );
            visiting.remove(&(owner, declaration.span));
            result
        }
        Expression::StructInit { properties, .. } => {
            let HirType::Struct(owner, name) = expected else {
                return Err(fail());
            };
            let fields = instance_fields(sema, *owner, name, expr.span())?;
            let members = sema
                .declarations
                .named(*owner, name)
                .and_then(|id| sema.declarations.members(&sema.modules, id))
                .ok_or_else(fail)?;
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                let ty = field.ty.clone();
                if let Some(property) = properties.iter().find(|p| p.name == field.name) {
                    let shorthand;
                    let value = if let Some(value) = &property.value {
                        value
                    } else {
                        shorthand = Expression::Identifier {
                            name: property.name.clone(),
                            span: property.span,
                        };
                        &shorthand
                    };
                    values.push(constant(sema, module, value, &ty, visiting)?);
                } else {
                    let value = members
                        .iter()
                        .find_map(|member| match member {
                            ClassMember::Field(def) if def.name == field.name => {
                                def.initializer.as_ref()
                            }
                            _ => None,
                        })
                        .ok_or_else(fail)?;
                    values.push(constant(sema, *owner, value, &ty, visiting)?);
                }
            }
            Ok(HirOperand::AggregateConst(values))
        }
        Expression::Tuple { elements, .. } => {
            let HirType::Tuple(types) = expected else {
                return Err(fail());
            };
            if elements.len() != types.len() {
                return Err(fail());
            }
            elements
                .iter()
                .zip(types)
                .map(|(value, ty)| constant(sema, module, value, ty, visiting))
                .collect::<Result<Vec<_>, _>>()
                .map(HirOperand::AggregateConst)
        }
        Expression::Binary {
            op, left, right, ..
        } => {
            let left = constant(sema, module, left, expected, visiting)?;
            let right = constant(sema, module, right, expected, visiting)?;
            if let (HirOperand::FloatConst(left), HirOperand::FloatConst(right)) = (&left, &right) {
                let value = match op {
                    BinaryOp::Plus => left + right,
                    BinaryOp::Minus => left - right,
                    BinaryOp::Multiply => left * right,
                    BinaryOp::Divide => left / right,
                    BinaryOp::Modulus => left % right,
                    _ => return Err(fail()),
                };
                return value
                    .is_finite()
                    .then_some(HirOperand::FloatConst(value))
                    .ok_or_else(fail);
            }
            let (HirOperand::IntConst(left), HirOperand::IntConst(right)) = (left, right) else {
                return Err(fail());
            };
            let result = match op {
                BinaryOp::Plus => left.checked_add(right),
                BinaryOp::Minus => left.checked_sub(right),
                BinaryOp::Multiply => left.checked_mul(right),
                BinaryOp::Divide => left.checked_div(right),
                BinaryOp::Modulus => left.checked_rem(right),
                BinaryOp::BitAnd => Some(left & right),
                BinaryOp::BitOr => Some(left | right),
                BinaryOp::BitXor => Some(left ^ right),
                BinaryOp::LeftShift => u32::try_from(right)
                    .ok()
                    .and_then(|bits| left.checked_shl(bits)),
                BinaryOp::RightShift => u32::try_from(right)
                    .ok()
                    .and_then(|bits| left.checked_shr(bits)),
                _ => None,
            };
            result.map(HirOperand::IntConst).ok_or_else(fail)
        }
        _ => Err(fail()),
    }
}

//! 无托管引用的静态存储：常量求值不执行函数体，也不引入启动初始化函数。
use super::{mono, *};
use crate::item::HirGlobal;
use mono::{Instances, Substitution};
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
    instances: &Instances,
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
    let ty = mono::concrete_type(sema, instances, view, &Vec::new(), None)
        .map_err(|e| e.with_site(module, declaration.span))?;
    if super::requirements::managed(sema, instances, &ty) {
        return Err(unsupported(module, declaration.span));
    }
    let init = declaration
        .value
        .as_ref()
        .map(|expr| {
            constant(
                sema,
                instances,
                module,
                expr,
                &ty,
                &Vec::new(),
                &mut HashSet::new(),
            )
        })
        .transpose()?;
    Ok(HirGlobal {
        name: name.clone(),
        ty,
        init,
        is_mutable: declaration.r#type == VariableType::Let,
        visibility: visibility_of(declaration.scope),
    })
}

/// 表达式在 sema 中已定型的 HIR 类型；常量求值同样不猜类型。
fn expression_type(
    sema: &SemaResult,
    instances: &Instances,
    module: ModuleId,
    span: Span,
    subst: &Substitution,
) -> Result<HirType, BuildError> {
    let view = sema
        .expression_types
        .get(module, span)
        .ok_or_else(|| unsupported(module, span))?;
    mono::concrete_type(sema, instances, view, subst, None).map_err(|e| e.with_site(module, span))
}

/// 二元表达式由 sema 提升出的公共类型。
///
/// 没有登记说明该运算不参与数值提升（移位、指针算术、`??` 等）；登记了却
/// 不是具体数值类型，说明 sema 与 HIR 的类型集合不一致，直接失败。
fn promoted_type(
    sema: &SemaResult,
    instances: &Instances,
    module: ModuleId,
    span: Span,
    subst: &Substitution,
) -> Result<Option<HirType>, BuildError> {
    let Some(view) = sema.binary_numeric.get(module, span) else {
        return Ok(None);
    };
    let ty = mono::concrete_type(sema, instances, view, subst, None)
        .map_err(|e| e.with_site(module, span))?;
    match ty {
        ty @ (HirType::Int(_) | HirType::Float(_)) => Ok(Some(ty)),
        _ => Err(BuildError::at(BuildErrorKind::InvalidInput).with_site(module, span)),
    }
}

/// 按目标整数类型截断常量：保留低位后按符号解释，与运行期 Cast 一致。
fn wrap_int_const(value: i128, kind: IntTy) -> i128 {
    let bits = kind.bit_width();
    let mask = (1_i128 << bits) - 1;
    let value = value & mask;
    if kind.is_signed() && value & (1_i128 << (bits - 1)) != 0 {
        value - (1_i128 << bits)
    } else {
        value
    }
}

/// f64 → IEEE binary16 的最近偶数舍入；结果仍是 f64，但值必为可表示的 f16。
fn round_f16(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let negative = value.is_sign_negative();
    let magnitude = value.abs();
    let rounded = if magnitude < 2_f64.powi(-14) {
        // 次正规数：量化步长 2^-24
        let quantum = 2_f64.powi(-24);
        (magnitude / quantum).round_ties_even() * quantum
    } else if magnitude >= 65520.0 {
        // 超过最大有限值 65504 半个 ulp（16）即溢出到无穷
        f64::INFINITY
    } else {
        // 正规数：按当前 binade 的 ulp（2^(e-10)）舍入
        let exponent = ((magnitude.to_bits() >> 52) & 0x7ff) as i32 - 1023;
        let step = 2_f64.powi(exponent - 10);
        (magnitude / step).round_ties_even() * step
    };
    if negative { -rounded } else { rounded }
}

/// 把常量舍入到目标浮点类型；f32 必须经过一次 f32 舍入，否则会把源精度丢掉。
fn round_float_const(value: f64, kind: crate::ty::FloatTy) -> f64 {
    match kind {
        crate::ty::FloatTy::F64 => value,
        crate::ty::FloatTy::F32 => value as f32 as f64,
        crate::ty::FloatTy::F16 => round_f16(value),
    }
}

/// 常量按目标数值类型做与运行期 `coerce` 等价的转换。
fn coerce_const(
    value: HirOperand,
    target: &HirType,
    module: ModuleId,
    span: Span,
) -> Result<HirOperand, BuildError> {
    let fail = || unsupported(module, span);
    match (value, target) {
        (HirOperand::IntConst(value), HirType::Int(kind)) => {
            Ok(HirOperand::IntConst(wrap_int_const(value, *kind)))
        }
        (HirOperand::IntConst(value), HirType::Float(kind)) => Ok(HirOperand::FloatConst(
            round_float_const(value as f64, *kind),
        )),
        (HirOperand::FloatConst(value), HirType::Float(kind)) => {
            Ok(HirOperand::FloatConst(round_float_const(value, *kind)))
        }
        _ => Err(fail()),
    }
}

/// 公共类型已确定时按该类型折叠；整数环绕、浮点舍入都对齐运行期指令。
fn fold_promoted(
    module: ModuleId,
    span: Span,
    op: BinaryOp,
    common: &HirType,
    left: HirOperand,
    right: HirOperand,
) -> Result<HirOperand, BuildError> {
    let fail = || unsupported(module, span);
    match common {
        HirType::Int(kind) => {
            let (HirOperand::IntConst(left), HirOperand::IntConst(right)) = (left, right) else {
                return Err(fail());
            };
            match op {
                BinaryOp::LessThan
                | BinaryOp::GreaterThan
                | BinaryOp::LessThanEquals
                | BinaryOp::GreaterThanEquals
                | BinaryOp::Equals
                | BinaryOp::NotEquals => {
                    let value = match op {
                        BinaryOp::LessThan => left < right,
                        BinaryOp::GreaterThan => left > right,
                        BinaryOp::LessThanEquals => left <= right,
                        BinaryOp::GreaterThanEquals => left >= right,
                        BinaryOp::Equals => left == right,
                        _ => left != right,
                    };
                    Ok(HirOperand::BoolConst(value))
                }
                BinaryOp::Plus
                | BinaryOp::Minus
                | BinaryOp::Multiply
                | BinaryOp::Divide
                | BinaryOp::Modulus
                | BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor => {
                    // x86 的 idiv 对 MIN / -1 会陷入；常量求值也拒绝，避免折叠出运行期得不到的值
                    let trap = kind.is_signed()
                        && right == -1
                        && left == -(1_i128 << (kind.bit_width() - 1));
                    let value = match op {
                        BinaryOp::Plus => left.wrapping_add(right),
                        BinaryOp::Minus => left.wrapping_sub(right),
                        BinaryOp::Multiply => left.wrapping_mul(right),
                        BinaryOp::Divide if !trap => left.checked_div(right).ok_or_else(fail)?,
                        BinaryOp::Modulus if !trap => left.checked_rem(right).ok_or_else(fail)?,
                        BinaryOp::BitAnd => left & right,
                        BinaryOp::BitOr => left | right,
                        BinaryOp::BitXor => left ^ right,
                        _ => return Err(fail()),
                    };
                    Ok(HirOperand::IntConst(wrap_int_const(value, *kind)))
                }
                _ => Err(fail()),
            }
        }
        HirType::Float(kind) => {
            let (HirOperand::FloatConst(left), HirOperand::FloatConst(right)) = (left, right)
            else {
                return Err(fail());
            };
            if matches!(
                op,
                BinaryOp::LessThan
                    | BinaryOp::GreaterThan
                    | BinaryOp::LessThanEquals
                    | BinaryOp::GreaterThanEquals
                    | BinaryOp::Equals
                    | BinaryOp::NotEquals
            ) {
                let value = match op {
                    BinaryOp::LessThan => left < right,
                    BinaryOp::GreaterThan => left > right,
                    BinaryOp::LessThanEquals => left <= right,
                    BinaryOp::GreaterThanEquals => left >= right,
                    BinaryOp::Equals => left == right,
                    _ => left != right,
                };
                return Ok(HirOperand::BoolConst(value));
            }
            // f32 必须用 f32 运算后再提升；f16 没有原生类型，先舍入操作数再在 f64 中
            // 计算并舍入一次结果（加减乘为正确舍入，除法仅存在极罕见双重舍入）。
            let value = match kind {
                crate::ty::FloatTy::F32 => {
                    let (left, right) = (left as f32, right as f32);
                    match op {
                        BinaryOp::Plus => (left + right) as f64,
                        BinaryOp::Minus => (left - right) as f64,
                        BinaryOp::Multiply => (left * right) as f64,
                        BinaryOp::Divide => (left / right) as f64,
                        BinaryOp::Modulus => (left % right) as f64,
                        _ => return Err(fail()),
                    }
                }
                crate::ty::FloatTy::F64 => match op {
                    BinaryOp::Plus => left + right,
                    BinaryOp::Minus => left - right,
                    BinaryOp::Multiply => left * right,
                    BinaryOp::Divide => left / right,
                    BinaryOp::Modulus => left % right,
                    _ => return Err(fail()),
                },
                crate::ty::FloatTy::F16 => {
                    let value = match op {
                        BinaryOp::Plus => left + right,
                        BinaryOp::Minus => left - right,
                        BinaryOp::Multiply => left * right,
                        BinaryOp::Divide => left / right,
                        BinaryOp::Modulus => left % right,
                        _ => return Err(fail()),
                    };
                    round_f16(value)
                }
            };
            value
                .is_finite()
                .then_some(HirOperand::FloatConst(value))
                .ok_or_else(fail)
        }
        _ => Err(fail()),
    }
}

fn constant(
    sema: &SemaResult,
    instances: &Instances,
    module: ModuleId,
    expr: &Expression,
    expected: &HirType,
    subst: &Substitution,
    visiting: &mut HashSet<(ModuleId, Span)>,
) -> Result<HirOperand, BuildError> {
    let fail = || unsupported(module, expr.span());
    match expr {
        Expression::Literal { value, .. } => match value {
            Literal::Number(text) => {
                if let Some(value) = parse_int_literal(text) {
                    Ok(HirOperand::IntConst(value))
                } else {
                    let value: f64 = text.replace('_', "").parse().map_err(|_| fail())?;
                    // 源类型是 f32/f16 时先按其精度落地，后续扩宽才不会凭空多出精度
                    let kind = match expected {
                        HirType::Float(kind) => *kind,
                        _ => crate::ty::FloatTy::F64,
                    };
                    Ok(HirOperand::FloatConst(round_float_const(value, kind)))
                }
            }
            Literal::Boolean(value) => Ok(HirOperand::BoolConst(*value)),
            Literal::Null => Ok(HirOperand::Null),
            _ => Err(fail()),
        },
        Expression::Paren { expr, .. } => {
            constant(sema, instances, module, expr, expected, subst, visiting)
        }
        Expression::Cast { expr: operand, .. } => {
            // 指针常量保留整数载荷，后端依据目标 Pointer 类型生成常量 inttoptr。
            let view = sema
                .expression_types
                .get(module, expr.span())
                .ok_or_else(fail)?;
            let target =
                mono::concrete_type(sema, instances, view, subst, None).map_err(|_| fail())?;
            let value = constant(sema, instances, module, operand, &target, subst, visiting)?;
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
            let numeric = matches!(op, UnaryOp::Minus | UnaryOp::Plus | UnaryOp::BitNot);
            // 数值一元运算先按操作数自身的类型取值（f32 源不能按外层 f64 解析）；
            // 操作数是未定型字面量时它没有具体类型，回退到声明类型。
            let operand_ty = if numeric {
                match expression_type(sema, instances, module, operand.span(), subst) {
                    Ok(ty) => ty,
                    Err(error) if matches!(error.kind, BuildErrorKind::UntypedLiteral { .. }) => {
                        expected.clone()
                    }
                    Err(error) => return Err(error),
                }
            } else {
                expected.clone()
            };
            let value = constant(
                sema,
                instances,
                module,
                operand,
                &operand_ty,
                subst,
                visiting,
            )?;
            let value = if numeric {
                coerce_const(value, &operand_ty, module, expr.span())?
            } else {
                value
            };
            let result = match (op, value) {
                (UnaryOp::Plus, value) => value,
                // 先按操作数宽度取负/取反，再落到声明宽度：`-i8::MIN` 仍为 MIN，
                // `~u8` 得到 255 而不是 i128 的 -201。
                (UnaryOp::Minus, HirOperand::IntConst(value)) => {
                    let HirType::Int(kind) = &operand_ty else {
                        return Err(fail());
                    };
                    HirOperand::IntConst(wrap_int_const(-value, *kind))
                }
                (UnaryOp::BitNot, HirOperand::IntConst(value)) => {
                    let HirType::Int(kind) = &operand_ty else {
                        return Err(fail());
                    };
                    HirOperand::IntConst(wrap_int_const(!value, *kind))
                }
                (UnaryOp::Not, HirOperand::BoolConst(value)) => HirOperand::BoolConst(!value),
                (UnaryOp::Minus, HirOperand::FloatConst(value)) => HirOperand::FloatConst(-value),
                _ => return Err(fail()),
            };
            if numeric && matches!(expected, HirType::Int(_) | HirType::Float(_)) {
                coerce_const(result, expected, module, expr.span())
            } else {
                Ok(result)
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
                instances,
                owner,
                declaration.value.as_ref().ok_or_else(fail)?,
                expected,
                &Vec::new(),
                visiting,
            );
            visiting.remove(&(owner, declaration.span));
            result
        }
        Expression::StructInit { properties, .. } => {
            let HirType::Struct(owner, name) = expected else {
                return Err(fail());
            };
            let fields = instance_fields(sema, instances, *owner, name, expr.span())?;
            let (definition, owner_subst) =
                mono::aggregate_definition(sema, instances, *owner, name).ok_or_else(fail)?;
            let members = sema
                .declarations
                .named(*owner, &definition.name)
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
                    values.push(constant(
                        sema, instances, module, value, &ty, subst, visiting,
                    )?);
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
                    values.push(constant(
                        sema,
                        instances,
                        *owner,
                        value,
                        &ty,
                        &owner_subst,
                        visiting,
                    )?);
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
                .map(|(value, ty)| constant(sema, instances, module, value, ty, subst, visiting))
                .collect::<Result<Vec<_>, _>>()
                .map(HirOperand::AggregateConst)
        }
        Expression::Binary {
            op, left, right, ..
        } => {
            // 提升后的公共类型已由 sema 算好。先按操作数各自的类型取值，再按公共
            // 类型折叠：否则 f32 源会被当成 f64，窄公共类型（如 i16）的溢出也不会环绕。
            if let Some(common) = promoted_type(sema, instances, module, expr.span(), subst)? {
                let left_span = left.span();
                let right_span = right.span();
                // 操作数是未定型字面量时没有自身类型，改用公共类型；否则它连
                // 整数字面量都无法落地（如 `A + 100 < 50` 中的 `50`）。
                let operand_type =
                    |span| match expression_type(sema, instances, module, span, subst) {
                        Ok(ty) => Ok(ty),
                        Err(error)
                            if matches!(error.kind, BuildErrorKind::UntypedLiteral { .. }) =>
                        {
                            Ok(common.clone())
                        }
                        Err(error) => Err(error),
                    };
                let left_ty = operand_type(left_span)?;
                let right_ty = operand_type(right_span)?;
                let left = constant(sema, instances, module, left, &left_ty, subst, visiting)?;
                let right = constant(sema, instances, module, right, &right_ty, subst, visiting)?;
                let left = coerce_const(left, &common, module, left_span)?;
                let right = coerce_const(right, &common, module, right_span)?;
                return fold_promoted(module, expr.span(), *op, &common, left, right);
            }
            let left = constant(sema, instances, module, left, expected, subst, visiting)?;
            let right = constant(sema, instances, module, right, expected, subst, visiting)?;
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

//! 赋值顺序、左值与复合运算。
use super::*;
use crate::typecheck::call::{
    check_index_on, check_index_operand, check_index_signature, index_parts, index_signature,
    is_custom_index,
};

/// 赋值：左值检查（追加项 B：const 与导入符号只读）+ 兼容检查。
pub(crate) fn check_assignment(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    op: AssignOp,
    value: &Expression,
    span: Span,
) -> Ty {
    let assigned = assignment_symbol(sema, ctx, target);
    // 提示查询不检查表达式，不产生读取或副作用；普通赋值的实际检查遵循 RHS→LHS。
    let early_value = if op == AssignOp::Assign {
        Some(match lvalue_hint(sema, ctx, target) {
            Some(hint) => check_expr_with_hint(sema, ctx, value, &hint),
            None => check_expr(sema, ctx, value),
        })
    } else {
        None
    };
    let checked_index = if let Some((receiver, index)) = index_parts(target) {
        let receiver_ty = check_expr(sema, ctx, receiver);
        if is_custom_index(sema, &receiver_ty) {
            let ty = check_overloaded_assignment(
                sema,
                ctx,
                &receiver_ty,
                index,
                op,
                value,
                early_value,
                span,
            );
            record_index_place_type(sema, ctx, target, &ty);
            return ty;
        }
        Some(check_index_on(sema, ctx, receiver_ty, index, target.span()))
    } else {
        None
    };
    check_lvalue_assignment(sema, ctx, target);
    let slot = if op == AssignOp::Assign {
        assigned
            .as_ref()
            .filter(|symbol| ctx.uninitialized.remove(&symbol.name_span))
            .map(|s| s.name_span)
    } else {
        None
    };
    let target_ty = if let Some(ty) = checked_index {
        record_index_place_type(sema, ctx, target, &ty);
        ty
    } else {
        check_expr(sema, ctx, target)
    };
    if let Some(slot) = slot {
        ctx.uninitialized.insert(slot);
    }
    let value_ty =
        early_value.unwrap_or_else(|| check_expr_with_hint(sema, ctx, value, &target_ty));
    if let Some(symbol) = assigned {
        ctx.uninitialized.remove(&symbol.name_span);
    }
    match op {
        AssignOp::Assign => {
            if !sema.is_assignable(&target_ty, &value_ty) {
                sema.type_error(
                    ctx.module,
                    span,
                    sema.assign_mismatch_reason(&target_ty, &value_ty)
                        .unwrap_or_else(|| format!("不能把 `{value_ty}` 赋给 `{target_ty}`")),
                );
            }
        }
        _ => {
            // 复合赋值 ≡ target = target <op> value，结果须能赋回
            let result_ty = check_arithmetic_compound(sema, ctx, op, &target_ty, &value_ty, span);
            if !result_ty.is_opaque() && !sema.is_assignable(&target_ty, &result_ty) {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("复合赋值结果 `{result_ty}` 不能赋回 `{target_ty}`"),
                );
            }
        }
    }
    target_ty
}

fn record_index_place_type(sema: &mut Sema, ctx: &BodyCtx, mut expr: &Expression, ty: &Ty) {
    loop {
        crate::typecheck::expr::record_expression_type(sema, ctx.module, expr.span(), ty.clone());
        let Expression::Paren { expr: inner, .. } = expr else {
            break;
        };
        expr = inner;
    }
}

#[allow(clippy::too_many_arguments)]
fn check_overloaded_assignment(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    receiver: &Ty,
    index: &Expression,
    op: AssignOp,
    value: &Expression,
    early_value: Option<Ty>,
    span: Span,
) -> Ty {
    let setter = check_index_signature(sema, ctx, receiver, "__setitem", span);
    if op == AssignOp::Assign {
        let Some((key, write)) = setter
            .as_ref()
            .and_then(|sig| sig.params.get(1).zip(sig.params.get(2)))
        else {
            check_expr(sema, ctx, index);
            return Ty::Error;
        };
        check_index_operand(sema, ctx, index, &key.ty);
        let actual = early_value.expect("普通赋值先检查右侧");
        if !sema.is_assignable(&write.ty, &actual) {
            sema.type_error(
                ctx.module,
                value.span(),
                format!("下标写入需要 `{}`，实为 `{actual}`", write.ty),
            );
        }
        return write.ty.clone();
    }
    let read = check_index_on(sema, ctx, receiver.clone(), index, span);
    if let Some(setter) = setter {
        check_index_update_types(sema, ctx, receiver, &setter, span);
    }
    let rhs = check_expr_with_hint(sema, ctx, value, &read);
    check_arithmetic_compound(sema, ctx, op, &read, &rhs, span);
    read
}

pub(crate) fn check_index_update_types(
    sema: &mut Sema,
    ctx: &BodyCtx,
    receiver: &Ty,
    setter: &crate::ty::FunctionSig,
    span: Span,
) {
    if let Some((_, getter)) = index_signature(sema, receiver, "__getitem")
        && let (Some(read_key), Some(write_key), Some(write)) = (
            getter.params.get(1),
            setter.params.get(1),
            setter.params.get(2),
        )
        && (!ty_eq(&read_key.ty, &write_key.ty) || !ty_eq(&getter.ret, &write.ty))
    {
        sema.type_error(
            ctx.module,
            span,
            "下标复合赋值或自增/自减要求读写方法的下标类型和值类型一致",
        );
    }
}

/// 只读查询左值的声明类型，供 RHS 的空数组等表达式取得上下文。
fn lvalue_hint(sema: &Sema, ctx: &BodyCtx, expr: &Expression) -> Option<Ty> {
    match expr {
        Expression::Identifier { name, .. } => match &sema.scopes.lookup(ctx.scope, name)?.kind {
            SymbolKind::Variable { ty, .. } => ty.clone(),
            SymbolKind::Param { ty } => Some(ty.clone()),
            SymbolKind::Function { sig } => Some(Ty::Function(std::rc::Rc::new(sig.clone()))),
            _ => None,
        },
        Expression::This { .. } => ctx.this_ty.clone(),
        Expression::Paren { expr, .. } => lvalue_hint(sema, ctx, expr),
        Expression::MemberAccess { target, name, .. } => {
            let receiver = lvalue_hint(sema, ctx, target).or_else(|| {
                let Expression::Identifier { name, .. } = &**target else {
                    return None;
                };
                let (module, name) = type_key_of_name(sema, ctx.scope, name)?;
                Some(Ty::Class(module, name))
            })?;
            let base = match &receiver {
                Ty::Pointer(inner) => &**inner,
                other => other,
            };
            if let Ty::Object(fields) = base {
                return fields.get(name).cloned();
            }
            if !matches!(base, Ty::Struct(..) | Ty::Class(..) | Ty::Applied(..)) {
                return None;
            }
            let key = crate::typecheck::this_ty_key(base);
            let hit = crate::typecheck::member::lookup_member_chain(sema, &key, name, false)?;
            let subst = crate::ty::member_substitution(sema, base, &hit.owner);
            let ty = match hit.info {
                crate::typecheck::member::MemberInfo::Field(field) => field.ty,
                crate::typecheck::member::MemberInfo::Method(method) => {
                    Ty::Function(std::rc::Rc::new(method.sig))
                }
            };
            Some(crate::ty::substitute_type_params(&ty, &subst))
        }
        Expression::IndexAccess { target, index, .. } => match lvalue_hint(sema, ctx, target)? {
            Ty::Array(inner, _) | Ty::Pointer(inner) => Some(*inner),
            Ty::Tuple(elements) => {
                crate::typecheck::call::tuple_element_hint(&elements, index).cloned()
            }
            ty if is_custom_index(sema, &ty) => index_signature(sema, &ty, "__setitem")?
                .1
                .params
                .get(2)
                .map(|param| param.ty.clone()),
            _ => None,
        },
        Expression::Unary {
            op: UnaryOp::DerefPointer,
            operand,
            ..
        } => match lvalue_hint(sema, ctx, operand)? {
            Ty::Pointer(inner) => Some(*inner),
            _ => None,
        },
        Expression::Call { callee, .. } => match lvalue_hint(sema, ctx, callee)? {
            Ty::Function(sig) => Some(if sig.is_async {
                Ty::Future(Box::new(sig.ret.clone()))
            } else {
                sig.ret.clone()
            }),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn assignment_symbol(sema: &Sema, ctx: &BodyCtx, expr: &Expression) -> Option<Symbol> {
    match expr {
        Expression::Identifier { name, .. } => sema.scopes.lookup(ctx.scope, name).cloned(),
        Expression::Paren { expr, .. } => assignment_symbol(sema, ctx, expr),
        _ => None,
    }
}
/// 复合赋值的运算检查（复用二元运算符矩阵）。
pub(crate) fn check_arithmetic_compound(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: AssignOp,
    lhs: &Ty,
    rhs: &Ty,
    span: Span,
) -> Ty {
    if lhs.is_opaque() || rhs.is_opaque() {
        return Ty::Unknown;
    }
    match op {
        AssignOp::PlusAssign => {
            if sema.is_string_type(lhs) && sema.is_string_type(rhs) {
                return lhs.clone();
            }
            check_arithmetic(sema, ctx, BinaryOp::Plus, lhs, rhs, span)
        }
        AssignOp::MinusAssign => check_arithmetic(sema, ctx, BinaryOp::Minus, lhs, rhs, span),
        AssignOp::MultiplyAssign => check_arithmetic(sema, ctx, BinaryOp::Multiply, lhs, rhs, span),
        AssignOp::DivideAssign => check_arithmetic(sema, ctx, BinaryOp::Divide, lhs, rhs, span),
        AssignOp::ModulusAssign => check_arithmetic(sema, ctx, BinaryOp::Modulus, lhs, rhs, span),
        AssignOp::BitAndAssign => check_arithmetic(sema, ctx, BinaryOp::BitAnd, lhs, rhs, span),
        AssignOp::BitXorAssign => check_arithmetic(sema, ctx, BinaryOp::BitXor, lhs, rhs, span),
        AssignOp::BitOrAssign => check_arithmetic(sema, ctx, BinaryOp::BitOr, lhs, rhs, span),
        AssignOp::LeftShiftAssign => {
            check_arithmetic(sema, ctx, BinaryOp::LeftShift, lhs, rhs, span)
        }
        AssignOp::RightShiftAssign => {
            check_arithmetic(sema, ctx, BinaryOp::RightShift, lhs, rhs, span)
        }
        AssignOp::Assign => unreachable!("Assign 已在调用点处理"),
    }
}
/// 左值校验：标识符 / 成员访问 / 索引访问 / 解引用为合法目标。
/// 追加项 B：const 变量、具名导入、函数、命名空间不可写。
pub(crate) fn is_lvalue(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> bool {
    match expr {
        Expression::Paren { expr, .. } => is_lvalue(sema, ctx, expr),
        Expression::Identifier { name, .. } => match sema.scopes.lookup(ctx.scope, name) {
            Some(Symbol {
                kind: SymbolKind::Variable { is_const, .. },
                ..
            }) => !is_const,
            Some(Symbol {
                kind: SymbolKind::Param { .. },
                ..
            }) => true,
            _ => false,
        },
        Expression::MemberAccess { target, name, .. } => {
            // 模块/静态成员的写入不合法（导入符号只读）
            if let Expression::Identifier { name, .. } = &**target
                && matches!(
                    classify_identifier(sema, ctx, name),
                    Some(IdentTarget::Namespace(_))
                )
            {
                return false;
            }
            if let Expression::Identifier {
                name: type_name, ..
            } = &**target
                && matches!(
                    classify_identifier(sema, ctx, type_name),
                    Some(IdentTarget::Type)
                )
            {
                let receiver =
                    crate::typecheck::member::static_receiver(sema, ctx, type_name, target.span());
                let base = match &receiver {
                    Ty::Applied(base, _) => &**base,
                    ty => ty,
                };
                let key = match base {
                    Ty::Struct(module, name)
                    | Ty::Class(module, name)
                    | Ty::Trait(module, name) => Some((*module, name.clone())),
                    _ => None,
                };
                return key
                    .and_then(|key| crate::typecheck::member::lookup_member_chain(sema, &key, name, true))
                    .is_some_and(|hit| matches!(hit.info, crate::typecheck::member::MemberInfo::Field(field) if field.is_static));
            }
            is_lvalue(sema, ctx, target)
        }
        Expression::IndexAccess { target, .. } => {
            let ty = sema
                .expression_types
                .get(&(ctx.module, target.span()))
                .cloned()
                .or_else(|| lvalue_hint(sema, ctx, target));
            if ty
                .as_ref()
                .is_some_and(|ty| sema.is_string_type(ty) || is_custom_index(sema, ty))
            {
                return false;
            }
            is_lvalue(sema, ctx, target)
        }
        // `this` 作为成员访问/索引的基座是左值（直接给 `this` 赋值由
        // check_lvalue_assignment 单独拒绝）
        Expression::This { .. } => true,
        Expression::Unary {
            op: UnaryOp::DerefPointer,
            ..
        } => true,
        _ => false,
    }
}
/// 赋值目标的左值检查并给出诊断。
pub(crate) fn check_lvalue_assignment(sema: &mut Sema, ctx: &mut BodyCtx, target: &Expression) {
    // 先给出针对性诊断
    if let Expression::Identifier { name, span } = target {
        match sema.scopes.lookup(ctx.scope, name) {
            Some(Symbol {
                kind: SymbolKind::Variable { is_const: true, .. },
                ..
            }) => {
                sema.type_error(
                    ctx.module,
                    *span,
                    format!("不能给 `const` 变量 `{name}` 赋值"),
                );
                return;
            }
            Some(Symbol {
                kind: SymbolKind::NamedImport { .. },
                ..
            }) => {
                sema.type_error(ctx.module, *span, format!("导入的 `{name}` 是只读的"));
                return;
            }
            Some(Symbol {
                kind: SymbolKind::Function { .. },
                ..
            }) => {
                sema.type_error(ctx.module, *span, format!("不能给函数 `{name}` 赋值"));
                return;
            }
            _ => {}
        }
    }
    if let Expression::This { span, .. } = target {
        sema.type_error(ctx.module, *span, "不能给 `this` 赋值");
        return;
    }
    if !is_lvalue(sema, ctx, target) {
        sema.type_error(ctx.module, target.span(), "赋值目标不是可写的左值");
    }
}

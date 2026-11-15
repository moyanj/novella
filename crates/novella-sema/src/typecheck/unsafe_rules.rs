//! 运算符与不变式检查：二元 / 算术矩阵、赋值兼容、cast、struct 初始化、
//! unsafe 上下文规则（S1–S10 子集 + extern 调用 E7）、左值判定。

use std::collections::HashSet;

use novella_ast::expr::{AssignOp, BinaryOp, Expression, UnaryOp};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::symbol::{Symbol, SymbolKind};
use crate::ty::{Ty, TypeDef, ty_eq};

use super::body::{BodyCtx, binary_op_text, concrete};
use super::expr::{
    IdentTarget, check_expr, check_identifier, classify_identifier, type_key_of_name,
};

/// S10：指针类操作需要 `unsafe {}`。
pub(crate) fn require_unsafe(sema: &mut Sema, ctx: &BodyCtx, span: Span, what: &str) {
    if !ctx.in_unsafe {
        sema.unsafe_error(
            ctx.module,
            span,
            format!("{what}是不安全操作，需要 `unsafe {{ }}` 块"),
        );
    }
}
pub(crate) fn check_binary(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: BinaryOp,
    left: &Expression,
    right: &Expression,
    span: Span,
) -> Ty {
    let lhs = check_expr(sema, ctx, left);
    let rhs = check_expr(sema, ctx, right);
    // 一侧不可判定时不做检查（可恢复原则）
    if lhs.is_opaque() || rhs.is_opaque() {
        return match op {
            BinaryOp::LogicalAnd
            | BinaryOp::LogicalOr
            | BinaryOp::Equals
            | BinaryOp::NotEquals
            | BinaryOp::IdentityEquals
            | BinaryOp::IdentityNotEquals
            | BinaryOp::LessThan
            | BinaryOp::LessThanEquals
            | BinaryOp::GreaterThan
            | BinaryOp::GreaterThanEquals => Ty::Boolean,
            _ => Ty::Unknown,
        };
    }
    match op {
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
            if !matches!(lhs, Ty::Boolean) || !matches!(rhs, Ty::Boolean) {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的两个操作数都必须是 `boolean`（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
            }
            Ty::Boolean
        }
        BinaryOp::NullCoalesce => {
            if !lhs.is_nullable() {
                sema.type_error(
                    ctx.module,
                    left.span(),
                    format!("`??` 的左操作数必须可空，实为 `{lhs}`"),
                );
            }
            // 结果类型 = 左操作数去空；无法表达时退化为右操作数类型
            match &lhs {
                Ty::Pointer(inner) => (**inner).clone(),
                Ty::Class(..) => lhs,
                Ty::Null => rhs,
                _ => rhs,
            }
        }
        BinaryOp::Equals
        | BinaryOp::NotEquals
        | BinaryOp::IdentityEquals
        | BinaryOp::IdentityNotEquals => {
            // §8.5：struct 不允许 ==
            for (side, ty) in [("左", &lhs), ("右", &rhs)] {
                if matches!(ty, Ty::Struct(..)) {
                    sema.type_error(
                        ctx.module,
                        span,
                        format!("struct 类型不支持 `==`/`!=` 比较（{side}操作数为 `{ty}`）"),
                    );
                }
            }
            if !comparable(&lhs, &rhs) {
                sema.type_error(ctx.module, span, format!("`{lhs}` 与 `{rhs}` 不可比较"));
            }
            Ty::Boolean
        }
        BinaryOp::LessThan
        | BinaryOp::LessThanEquals
        | BinaryOp::GreaterThan
        | BinaryOp::GreaterThanEquals => {
            let ok = (lhs.is_numeric() && rhs.is_numeric() && unify_numeric(&lhs, &rhs).is_some())
                || (matches!(lhs, Ty::String) && matches!(rhs, Ty::String));
            if !ok {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的操作数类型不兼容（`{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
            }
            Ty::Boolean
        }
        BinaryOp::Plus => {
            if matches!(lhs, Ty::String) && matches!(rhs, Ty::String) {
                return Ty::String;
            }
            check_arithmetic(sema, ctx, op, &lhs, &rhs, span)
        }
        BinaryOp::Minus | BinaryOp::Multiply | BinaryOp::Divide => {
            check_arithmetic(sema, ctx, op, &lhs, &rhs, span)
        }
        BinaryOp::Modulus => match unify_numeric(&lhs, &rhs) {
            Some(ty) if ty.is_integer() => ty,
            Some(_) => {
                sema.type_error(ctx.module, span, "`%` 只能用于整数类型");
                Ty::Unknown
            }
            None => {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("`%` 的操作数类型不兼容（`{lhs}` 与 `{rhs}`）"),
                );
                Ty::Unknown
            }
        },
        BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr => {
            if lhs.is_integer() && rhs.is_integer() {
                match unify_numeric(&lhs, &rhs) {
                    Some(ty) if ty.is_integer() => ty,
                    _ => {
                        sema.type_error(
                            ctx.module,
                            span,
                            format!(
                                "`{}` 的整数操作数位宽不一致（`{lhs}` 与 `{rhs}`）",
                                binary_op_text(op)
                            ),
                        );
                        Ty::Unknown
                    }
                }
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 只能用于整数类型（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
                Ty::Unknown
            }
        }
        BinaryOp::LeftShift | BinaryOp::RightShift => {
            if !lhs.is_integer() || !rhs.is_integer() {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "`{}` 的两个操作数都必须是整数（实为 `{lhs}` 与 `{rhs}`）",
                        binary_op_text(op)
                    ),
                );
                Ty::Unknown
            } else {
                concrete(&lhs)
            }
        }
    }
}
/// 加减乘除：数值同型运算 + S7 指针算术。
pub(crate) fn check_arithmetic(
    sema: &mut Sema,
    ctx: &BodyCtx,
    op: BinaryOp,
    lhs: &Ty,
    rhs: &Ty,
    span: Span,
) -> Ty {
    // S7：指针 ± 整数 → 同型指针（unsafe）
    if matches!(op, BinaryOp::Plus | BinaryOp::Minus) {
        if let (Ty::Pointer(_), b) = (lhs, rhs)
            && b.is_integer()
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return lhs.clone();
        }
        if matches!(op, BinaryOp::Plus)
            && let (Ty::Pointer(_), a) = (rhs, lhs)
            && a.is_integer()
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return rhs.clone();
        }
        // 指针 - 指针 → isize（S7，要求同 pointee）
        if matches!(op, BinaryOp::Minus)
            && let (Ty::Pointer(a), Ty::Pointer(b)) = (lhs, rhs)
            && (ty_eq(a, b) || matches!(**a, Ty::Void) || matches!(**b, Ty::Void))
        {
            require_unsafe(sema, ctx, span, "指针算术");
            return Ty::Int(crate::ty::IntKind::Isize);
        }
    }
    match unify_numeric(lhs, rhs) {
        Some(ty) => ty,
        None => {
            sema.type_error(
                ctx.module,
                span,
                format!(
                    "`{}` 的操作数类型不兼容（`{lhs}` 与 `{rhs}`）",
                    binary_op_text(op)
                ),
            );
            Ty::Unknown
        }
    }
}
/// 数值统一：字面量折叠 + 同 kind 检查。不做隐式数值转换。
pub(crate) fn unify_numeric(a: &Ty, b: &Ty) -> Option<Ty> {
    match (a, b) {
        (Ty::IntLiteral, Ty::Int(k)) | (Ty::Int(k), Ty::IntLiteral) => Some(Ty::Int(*k)),
        (Ty::IntLiteral, Ty::IntLiteral) => Some(Ty::IntLiteral),
        (Ty::FloatLiteral, Ty::Float(k)) | (Ty::Float(k), Ty::FloatLiteral) => Some(Ty::Float(*k)),
        (Ty::FloatLiteral, Ty::FloatLiteral) => Some(Ty::FloatLiteral),
        (Ty::Int(k1), Ty::Int(k2)) if k1 == k2 => Some(Ty::Int(*k1)),
        (Ty::Float(k1), Ty::Float(k2)) if k1 == k2 => Some(Ty::Float(*k1)),
        _ => None,
    }
}
/// 相等比较的可比性：同型 / 字面量折叠 / null 与可空。
pub(crate) fn comparable(a: &Ty, b: &Ty) -> bool {
    if a.is_opaque() || b.is_opaque() {
        return true;
    }
    if ty_eq(a, b) || unify_numeric(a, b).is_some() {
        return true;
    }
    (matches!(a, Ty::Null) && b.is_nullable()) || (matches!(b, Ty::Null) && a.is_nullable())
}
/// 通用类型合一（三元/数组元素用）：等价 / 数值统一 / 单向可赋值。
pub(crate) fn unify_types(a: &Ty, b: &Ty) -> Option<Ty> {
    if a.is_opaque() {
        return Some(b.clone());
    }
    if b.is_opaque() {
        return Some(a.clone());
    }
    if ty_eq(a, b) {
        return Some(a.clone());
    }
    if let Some(ty) = unify_numeric(a, b) {
        return Some(ty);
    }
    if matches!(a, Ty::Null) && b.is_nullable() {
        return Some(b.clone());
    }
    if matches!(b, Ty::Null) && a.is_nullable() {
        return Some(a.clone());
    }
    None
}
/// `as` 显式转换（design.md §6.3）：数值互转安全；涉指针的转换 unsafe。
pub(crate) fn check_cast(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    expr: &Expression,
    ty: &TypeExpr,
    span: Span,
) -> Ty {
    let source = check_expr(sema, ctx, expr);
    let target = sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
    if source.is_opaque() || target.is_opaque() {
        return target;
    }
    // 数值互转（含 int→float）本身安全；null 可显式转任意可空类型
    let ok = if ty_eq(&source, &target)
        || (source.is_numeric() && target.is_numeric())
        || (matches!(source, Ty::Null) && target.is_nullable())
    {
        true
    } else if (matches!(target, Ty::Pointer(_))
        && (matches!(source, Ty::Pointer(_)) || source.is_integer()))
        || (matches!(source, Ty::Pointer(_)) && target.is_integer())
    {
        require_unsafe(sema, ctx, span, "指针显式转换");
        true
    } else if let (Ty::Class(..), Ty::Class(..)) = (&source, &target) {
        if sema.is_subclass(&source, &target) {
            true // 向上转型（实际可隐式）
        } else if sema.is_subclass(&target, &source) {
            require_unsafe(sema, ctx, span, "向下转型");
            true
        } else {
            false
        }
    } else {
        false
    };
    if !ok {
        sema.type_error(
            ctx.module,
            span,
            format!("无法把 `{source}` 转换为 `{target}`"),
        );
    }
    target
}
/// struct 初始化 `Point{ x: 1 }`：字段存在性/重复/缺失检查。
pub(crate) fn check_struct_init(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    name: &str,
    properties: &[novella_ast::expr::PropertyAssignment],
    span: Span,
) -> Ty {
    let struct_key = match type_key_of_name(sema, ctx.scope, name) {
        Some(key) => {
            if let Some(symbol) = sema.scopes.lookup(ctx.scope, name).cloned() {
                sema.record_reference(ctx.module, span, &symbol);
            }
            key
        }
        None if sema.scopes.lookup(ctx.scope, name).is_some() => {
            sema.type_error(
                ctx.module,
                span,
                format!("`{name}` 不是类型，不能用于初始化"),
            );
            return Ty::Unknown;
        }
        None => {
            sema.type_error(ctx.module, span, format!("未定义的类型 `{name}`"));
            return Ty::Error;
        }
    };
    match sema.type_defs.get(&struct_key) {
        Some(TypeDef::Struct(def)) => {
            let def = def.clone();
            let mut seen = HashSet::new();
            for property in properties {
                if !seen.insert(property.name.clone()) {
                    sema.type_error(
                        ctx.module,
                        property.span,
                        format!("字段 `{}` 重复初始化", property.name),
                    );
                    continue;
                }
                let Some(field_def) = def.fields.iter().find(|f| f.name == property.name) else {
                    sema.type_error(
                        ctx.module,
                        property.span,
                        format!("struct `{name}` 没有字段 `{}`", property.name),
                    );
                    continue;
                };
                let value_ty = match &property.value {
                    Some(value) => check_expr(sema, ctx, value),
                    None => {
                        // 简写 {x} ≡ {x: x}
                        check_identifier(sema, ctx, &property.name, property.span)
                    }
                };
                if !sema.is_assignable(&field_def.ty, &value_ty) {
                    sema.type_error(
                        ctx.module,
                        property.span,
                        format!(
                            "字段 `{}` 需要 `{}`，实为 `{value_ty}`",
                            property.name, field_def.ty
                        ),
                    );
                }
            }
            // 缺失字段（无默认值的必须提供）
            for field_def in &def.fields {
                if !field_def.has_default && !seen.contains(&field_def.name) {
                    sema.type_error(
                        ctx.module,
                        span,
                        format!("struct `{name}` 的字段 `{}` 未初始化", field_def.name),
                    );
                }
            }
            Ty::Struct(struct_key.0, struct_key.1)
        }
        Some(TypeDef::Class(_)) => {
            sema.type_error(
                ctx.module,
                span,
                format!("class `{name}` 不能用 `{{...}}` 初始化，请用 `new {name}(...)`"),
            );
            Ty::Unknown
        }
        Some(_) => {
            sema.type_error(ctx.module, span, format!("`{name}` 不是 struct/class 类型"));
            Ty::Error
        }
        None => Ty::Unknown, // 语法失败的模块，注册阶段已报过
    }
}
/// 赋值：左值检查（追加项 B：const 与导入符号只读）+ 兼容检查。
pub(crate) fn check_assignment(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    target: &Expression,
    op: AssignOp,
    value: &Expression,
    span: Span,
) -> Ty {
    check_lvalue_assignment(sema, ctx, target);
    let target_ty = check_expr(sema, ctx, target);
    let value_ty = check_expr(sema, ctx, value);
    match op {
        AssignOp::Assign => {
            if !sema.is_assignable(&target_ty, &value_ty) {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("不能把 `{value_ty}` 赋给 `{target_ty}`"),
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
            if matches!(lhs, Ty::String) && matches!(rhs, Ty::String) {
                return Ty::String;
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
        Expression::MemberAccess { target, .. } => {
            // 模块/静态成员的写入不合法（导入符号只读）
            if let Expression::Identifier { name, .. } = &**target
                && matches!(
                    classify_identifier(sema, ctx, name),
                    Some(IdentTarget::Namespace(_) | IdentTarget::Type)
                )
            {
                return false;
            }
            is_lvalue(sema, ctx, target)
        }
        Expression::IndexAccess { target, .. } => is_lvalue(sema, ctx, target),
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

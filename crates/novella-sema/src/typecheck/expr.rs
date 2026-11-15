//! 表达式检查：字面量值解析、标识符解析、数组 / 对象字面量、一元运算。

use std::collections::HashSet;
use std::rc::Rc;

use novella_ast::expr::{Expression, Literal, UnaryOp};
use novella_ast::stmt::Statement;
use novella_core::Span;
use novella_parser::ParserInput;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::{NsTarget, ScopeId, Symbol, SymbolKind};
use crate::ty::{Ty, classify_number_literal};

use super::body::{BodyCtx, concrete, expect_boolean, unary_op_text};
use super::call::{check_call, check_index_access, check_lambda, check_match, check_new};
use super::member::check_member_access;
use super::unsafe_rules::{
    check_assignment, check_binary, check_cast, check_struct_init, is_lvalue, require_unsafe,
    unify_types,
};

// ---------- 表达式 ----------
/// 表达式检查，返回表达式类型（出错时返回 Unknown 并记录诊断，保证可恢复）。
pub(crate) fn check_expr(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> Ty {
    match expr {
        Expression::Literal { value, span } => check_literal(sema, ctx, value, *span),
        Expression::Identifier { name, span } => check_identifier(sema, ctx, name, *span),
        Expression::Paren { expr, .. } => check_expr(sema, ctx, expr),
        Expression::Tuple { elements, .. } => {
            let tys = elements.iter().map(|e| check_expr(sema, ctx, e)).collect();
            Ty::Tuple(tys)
        }
        Expression::ArrayLiteral { elements, span } => {
            check_array_literal(sema, ctx, elements, *span)
        }
        Expression::ObjectLiteral { properties, .. } => check_object_literal(sema, ctx, properties),
        Expression::This { span } => match &ctx.this_ty {
            Some(ty) => ty.clone(),
            None => {
                sema.type_error(ctx.module, *span, "`this` 只能在方法内使用");
                Ty::Unknown
            }
        },
        Expression::Unary { op, operand, span } => check_unary(sema, ctx, *op, operand, *span),
        Expression::Await { expr, span } => {
            if !ctx.in_async {
                sema.type_error(ctx.module, *span, "`await` 只能在 async 函数内使用");
            }
            // Promise 类型属后续期，本期透传操作数类型
            check_expr(sema, ctx, expr)
        }
        Expression::Binary {
            op,
            left,
            right,
            span,
        } => check_binary(sema, ctx, *op, left, right, *span),
        Expression::Is { operand, ty, .. } => {
            check_expr(sema, ctx, operand);
            sema.resolve_type(ctx.module, ctx.scope, ty, &ctx.type_env);
            Ty::Boolean
        }
        Expression::Cast { expr, ty, span } => check_cast(sema, ctx, expr, ty, *span),
        Expression::StructInit {
            name,
            properties,
            span,
        } => check_struct_init(sema, ctx, name, properties, *span),
        Expression::Ternary {
            condition,
            then_expr,
            else_expr,
            span,
        } => {
            expect_boolean(sema, ctx, condition, "三元表达式的条件");
            let then_ty = check_expr(sema, ctx, then_expr);
            let else_ty = check_expr(sema, ctx, else_expr);
            unify_types(&then_ty, &else_ty).unwrap_or_else(|| {
                sema.type_error(
                    ctx.module,
                    *span,
                    format!("三元表达式分支类型 `{then_ty}` 与 `{else_ty}` 不一致"),
                );
                Ty::Unknown
            })
        }
        Expression::Assignment {
            target,
            op,
            value,
            span,
        } => check_assignment(sema, ctx, target, *op, value, *span),
        Expression::MemberAccess { target, name, span } => {
            check_member_access(sema, ctx, target, name, *span)
        }
        Expression::IndexAccess {
            target,
            index,
            span,
        } => check_index_access(sema, ctx, target, index, *span),
        Expression::Call {
            callee,
            type_args,
            args,
            span,
        } => check_call(sema, ctx, callee, type_args, args, *span),
        Expression::New {
            target,
            type_args,
            args,
            span,
        } => check_new(sema, ctx, target, type_args, args, *span),
        Expression::MacroCall { args, .. } => {
            // 宏展开属后续期；参数照常检查，返回类型视为未知
            for arg in args {
                check_expr(sema, ctx, arg);
            }
            Ty::Unknown
        }
        Expression::Match(m) => check_match(sema, ctx, &m.subject, &m.cases, m.default.as_ref()),
        Expression::Lambda(lambda) => check_lambda(sema, ctx, lambda),
    }
}
pub(crate) fn check_literal(sema: &mut Sema, ctx: &mut BodyCtx, value: &Literal, span: Span) -> Ty {
    match value {
        Literal::Null => Ty::Null,
        Literal::Boolean(_) => Ty::Boolean,
        Literal::String(raw) => {
            check_fstring(sema, ctx, raw, span);
            Ty::String
        }
        Literal::Number(raw) => classify_number_literal(raw).unwrap_or_else(|| {
            sema.type_error(ctx.module, span, format!("非法数字字面量 `{raw}`"));
            Ty::Unknown
        }),
    }
}
/// f-string（`f"..."` / `f'...'`）：校验插值表达式的语法与类型。
///
/// 子表达式独立 lex+parse，其内部诊断的 span 是相对于插值文本的，
/// 检查完成后统一平移到源文件坐标（`adjust_diagnostics`）。
pub(crate) fn check_fstring(sema: &mut Sema, ctx: &mut BodyCtx, raw: &str, span: Span) {
    if !raw.starts_with("f\"") && !raw.starts_with("f'") {
        return;
    }
    // raw 形如 f"..."/f'...'（首尾分别为 f 前缀与引号）
    let inner = &raw[2..raw.len().saturating_sub(1)];
    let inner_base = span.start + 2; // 插值文本在源文件中的起始偏移
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        match ch {
            b'\\' => escaped = true,
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'{' if !in_single && !in_double => {
                if bytes.get(i + 1) == Some(&b'{') {
                    i += 2; // {{ 转义
                    continue;
                }
                // 找配对的 }（不允许嵌套 {}，插值为单层表达式）
                let mut j = i + 1;
                let mut inner_single = false;
                let mut inner_double = false;
                while j < bytes.len() {
                    let c = bytes[j];
                    match c {
                        b'\'' if !inner_double => inner_single = !inner_single,
                        b'"' if !inner_single => inner_double = !inner_double,
                        b'}' if !inner_single && !inner_double => break,
                        _ => {}
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    sema.type_error(ctx.module, span, "f-string 插值缺少配对的 `}`");
                    return;
                }
                let text = &inner[i + 1..j];
                let expr_span = Span {
                    start: inner_base + i,
                    end: inner_base + j + 1,
                };
                check_fstring_interpolation(sema, ctx, text, expr_span);
                i = j + 1;
            }
            _ => i += 1,
        }
    }
}
/// 检查单个 f-string 插值 `{expr}`：独立 lex+parse 后类型检查。
pub(crate) fn check_fstring_interpolation(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    text: &str,
    span: Span,
) {
    let text = text.trim();
    if text.is_empty() {
        sema.type_error(ctx.module, span, "f-string 插值不能为空");
        return;
    }
    let lex_result = novella_lexer::lexer(text);
    if !lex_result.errors.is_empty() {
        sema.type_error(ctx.module, span, "f-string 插值存在词法错误");
        return;
    }
    let tokens: Vec<_> = lex_result
        .tokens
        .into_iter()
        .filter(|t| t.kind != novella_core::token::TokenKind::LineComment)
        .collect();
    let mut parser = novella_parser::Parser::new(ParserInput {
        tokens,
        file: "Sema".to_string(),
    });
    let program = parser.parse();
    if !program.diagnostics.is_empty() {
        sema.type_error(ctx.module, span, "f-string 插值不是合法表达式");
        return;
    }
    // 只接受单个表达式语句
    let expression = match program.program.items.as_slice() {
        [novella_ast::program::Item::Statement(stmt)] => match &**stmt {
            Statement::Expression(expr) => expr.clone(),
            _ => {
                sema.type_error(ctx.module, span, "f-string 插值必须是单个表达式");
                return;
            }
        },
        _ => {
            sema.type_error(ctx.module, span, "f-string 插值必须是单个表达式");
            return;
        }
    };
    let diag_before = sema.modules[ctx.module].diagnostics.len();
    check_expr(sema, ctx, &expression);
    adjust_diagnostics(sema, ctx.module, diag_before, span.start + 1, text);
}
/// 把 [from_idx..) 的诊断平移：span 加 offset，并按新 span 重算行列。
pub(crate) fn adjust_diagnostics(
    sema: &mut Sema,
    module: ModuleId,
    from_idx: usize,
    offset: usize,
    _text: &str,
) {
    let source_len = sema.modules[module].source.len();
    let lines = sema.modules[module].lines.clone();
    for diag in sema.modules[module].diagnostics.iter_mut().skip(from_idx) {
        novella_core::decor::shift_span_in_place(diag, offset, source_len, &lines);
    }
}
/// 标识符：查作用域链，取符号的值类型。
pub(crate) fn check_identifier(sema: &mut Sema, ctx: &mut BodyCtx, name: &str, span: Span) -> Ty {
    let Some(symbol) = sema.scopes.lookup(ctx.scope, name).cloned() else {
        sema.type_error(ctx.module, span, format!("未定义的标识符 `{name}`"));
        return Ty::Error;
    };
    sema.record_reference(ctx.module, span, &symbol);
    value_type_of_symbol(sema, ctx, symbol, name, span)
}
/// 符号 → 值类型（供标识符/导入成员访问共用）。
pub(crate) fn value_type_of_symbol(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    symbol: Symbol,
    name: &str,
    span: Span,
) -> Ty {
    match symbol.kind {
        SymbolKind::Variable { ty, .. } => ty.unwrap_or(Ty::Unknown),
        SymbolKind::Param { ty } => ty,
        SymbolKind::Function { sig } => Ty::Function(Rc::new(sig)),
        SymbolKind::NamedImport { target, imported } => match target {
            Some(target_module) => {
                // 沿导入链取目标符号（不支持再导出链，直接取目标模块全局符号）
                match sema.global_scope_symbol(target_module, &imported) {
                    Some(target_symbol) => {
                        value_type_of_symbol(sema, ctx, target_symbol, name, span)
                    }
                    None => Ty::Unknown, // 绑定阶段已报过诊断
                }
            }
            None => Ty::External, // std 外部符号
        },
        SymbolKind::NamespaceImport { .. } => {
            sema.type_error(
                ctx.module,
                span,
                format!("命名空间 `{name}` 不能作为值使用，请用 `{name}.成员`"),
            );
            Ty::Unknown
        }
        SymbolKind::Type => {
            sema.type_error(ctx.module, span, format!("类型 `{name}` 不能作为值使用"));
            Ty::Unknown
        }
    }
}
/// 将「类型位置的标识符」解析为注册表键（具名导入转发到目标模块的原始名）。
pub(crate) fn type_key_of_name(
    sema: &Sema,
    scope: ScopeId,
    name: &str,
) -> Option<(ModuleId, String)> {
    match sema.scopes.lookup(scope, name) {
        Some(Symbol {
            kind: SymbolKind::Type,
            module,
            ..
        }) => Some((*module, name.to_string())),
        Some(Symbol {
            kind:
                SymbolKind::NamedImport {
                    target: Some(target),
                    imported,
                },
            ..
        }) => Some((*target, imported.clone())),
        _ => None,
    }
}
/// 判定标识符指向的符号种类（成员访问/赋值的特殊路径用）。
pub(crate) enum IdentTarget {
    Value,
    Type,
    Namespace(NsTarget),
}
pub(crate) fn classify_identifier(sema: &Sema, ctx: &BodyCtx, name: &str) -> Option<IdentTarget> {
    let symbol = sema.scopes.lookup(ctx.scope, name)?.clone();
    Some(match symbol.kind {
        SymbolKind::Type => IdentTarget::Type,
        SymbolKind::NamespaceImport { target } => IdentTarget::Namespace(target),
        SymbolKind::NamedImport {
            target,
            ref imported,
        } => {
            // 具名导入的类型也可用于静态成员路径
            if let Some(target_module) = target
                && matches!(
                    sema.global_scope_symbol(target_module, imported),
                    Some(Symbol {
                        kind: SymbolKind::Type,
                        ..
                    })
                )
            {
                return Some(IdentTarget::Type);
            }
            IdentTarget::Value
        }
        _ => IdentTarget::Value,
    })
}
/// 带期望类型的表达式检查（当前只对能由标注定型的几种表达式生效，其余回退
/// 到普通 [`check_expr`]）。返回表达式自身的类型，兼容性由调用方判定。
pub(crate) fn check_expr_with_hint(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    expr: &Expression,
    hint: &Ty,
) -> Ty {
    match (expr, hint) {
        // 空数组字面量：元素类型取自标注 `T[]`
        (Expression::ArrayLiteral { elements, .. }, Ty::Array(element)) if elements.is_empty() => {
            Ty::Array(element.clone())
        }
        // `null` → 标注为可空类型时即为该类型
        (
            Expression::Literal {
                value: Literal::Null,
                ..
            },
            hint,
        ) if hint.is_nullable() => hint.clone(),
        _ => check_expr(sema, ctx, expr),
    }
}
pub(crate) fn check_array_literal(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    elements: &[Expression],
    span: Span,
) -> Ty {
    if elements.is_empty() {
        sema.type_error(
            ctx.module,
            span,
            "空数组字面量无法推断元素类型，请显式写出元素或改用类型标注",
        );
        return Ty::Unknown;
    }
    let mut element_ty = check_expr(sema, ctx, &elements[0]);
    for element in &elements[1..] {
        let ty = check_expr(sema, ctx, element);
        element_ty = match unify_types(&element_ty, &ty) {
            Some(unified) => unified,
            None => {
                sema.type_error(
                    ctx.module,
                    element.span(),
                    format!("数组元素类型 `{ty}` 与首个元素 `{element_ty}` 不一致"),
                );
                element_ty
            }
        };
    }
    Ty::Array(Box::new(concrete(&element_ty)))
}
/// 对象字面量（追加项 E）：重复键检查 + 简写 `{b}` 展开校验。
pub(crate) fn check_object_literal(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    properties: &[novella_ast::expr::PropertyAssignment],
) -> Ty {
    let mut seen = HashSet::new();
    for property in properties {
        if !seen.insert(property.name.clone()) {
            sema.type_error(
                ctx.module,
                property.span,
                format!("对象字面量的键 `{}` 重复", property.name),
            );
        }
        match &property.value {
            Some(value) => {
                check_expr(sema, ctx, value);
            }
            None => {
                // 简写 {b} ≡ {b: b}：校验标识符存在且是值
                if sema.scopes.lookup(ctx.scope, &property.name).is_none() {
                    sema.type_error(
                        ctx.module,
                        property.span,
                        format!("对象简写键 `{}` 没有可展开的同名变量", property.name),
                    );
                }
            }
        }
    }
    // 匿名对象的结构类型属后续期
    Ty::Unknown
}
pub(crate) fn check_unary(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    op: UnaryOp,
    operand: &Expression,
    span: Span,
) -> Ty {
    let operand_ty = check_expr(sema, ctx, operand);
    match op {
        UnaryOp::Plus | UnaryOp::Minus => {
            if operand_ty.is_numeric() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                Ty::Unknown
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!(
                        "一元 `{}` 只能用于数值类型，实为 `{operand_ty}`",
                        unary_op_text(op)
                    ),
                );
                Ty::Unknown
            }
        }
        UnaryOp::Not => {
            if !matches!(operand_ty, Ty::Boolean) && !operand_ty.is_opaque() {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("`not` 只能用于 `boolean`，实为 `{operand_ty}`"),
                );
            }
            Ty::Boolean
        }
        UnaryOp::BitNot => {
            if operand_ty.is_integer() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                Ty::Unknown
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("`~` 只能用于整数类型，实为 `{operand_ty}`"),
                );
                Ty::Unknown
            }
        }
        UnaryOp::PostIncrement | UnaryOp::PostDecrement => {
            if !is_lvalue(sema, ctx, operand) {
                sema.type_error(ctx.module, operand.span(), "自增/自减需要可写的左值");
            }
            if operand_ty.is_numeric() {
                concrete(&operand_ty)
            } else if operand_ty.is_opaque() {
                Ty::Unknown
            } else {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("自增/自减只能用于数值类型，实为 `{operand_ty}`"),
                );
                Ty::Unknown
            }
        }
        UnaryOp::AddressOf => {
            // 取址是安全操作（design.md §6），但需要左值；
            // 操作数类型未知时错误已报过，跳过左值检查避免级联
            if !operand_ty.is_opaque() && !is_lvalue(sema, ctx, operand) {
                sema.type_error(ctx.module, operand.span(), "`&` 取址需要左值");
            }
            Ty::Pointer(Box::new(operand_ty))
        }
        UnaryOp::DerefPointer => {
            // S2：解引用必须 unsafe
            require_unsafe(sema, ctx, span, "指针解引用");
            match operand_ty {
                Ty::Pointer(inner) => *inner,
                Ty::Null => {
                    sema.type_error(ctx.module, span, "不能解引用 `null` 字面量");
                    Ty::Unknown
                }
                ty if ty.is_opaque() => Ty::Unknown,
                ty => {
                    sema.type_error(ctx.module, span, format!("只能解引用指针，实为 `{ty}`"));
                    Ty::Unknown
                }
            }
        }
    }
}

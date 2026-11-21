//! 字面量和 f-string 子表达式的宿主坐标检查。
use super::*;

pub(crate) fn check_literal(sema: &mut Sema, ctx: &mut BodyCtx, value: &Literal, span: Span) -> Ty {
    match value {
        Literal::Null => Ty::Null,
        Literal::Boolean(_) => Ty::Boolean,
        Literal::String(raw) => {
            if raw.starts_with('b') {
                if let Err(error) = novella_core::string::decode_byte_literal(raw) {
                    sema.type_error(ctx.module, span, error.to_string());
                }
                return Ty::Array(Box::new(Ty::Int(crate::ty::IntKind::U8)));
            }
            if raw.starts_with('f') {
                check_fstring(sema, ctx, raw, span);
                return Ty::String;
            }
            // 普通 / 原始 / 字节 / 格式化字符串一律按 core 的唯一解码表校验转义
            // （design.md §3.3「非法转义留待语义阶段」；矩阵 L08）
            match novella_core::string::decode_string_literal(raw) {
                Ok(_) => {}
                Err(error) => sema.type_error(ctx.module, span, error.to_string()),
            }
            check_fstring(sema, ctx, raw, span);
            Ty::String
        }
        Literal::Number(raw) => classify_number_literal(raw).unwrap_or_else(|| {
            sema.type_error(ctx.module, span, format!("非法数字字面量 `{raw}`"));
            Ty::Error
        }),
    }
}
/// f-string（`f"..."` / `f'...'`）：校验插值表达式的语法与类型。
///
/// 子表达式独立 lex+parse，但片段接在**父源码坐标系**上解析（见
/// [`check_fstring_interpolation`]），因此内部诊断与类型表键都直接是源文件坐标。
pub(crate) fn check_fstring(sema: &mut Sema, ctx: &mut BodyCtx, raw: &str, span: Span) {
    if !raw.starts_with("f\"") && !raw.starts_with("f'") {
        return;
    }
    // raw 形如 f"..."/f'...'（首尾分别为 f 前缀与引号）
    let inner = &raw[2..raw.len().saturating_sub(1)];
    let inner_base = span.start + 2; // 插值文本在源文件中的起始偏移
    let bytes = inner.as_bytes();
    let mut i = 0;
    let mut literal_start = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'{' && bytes.get(i + 1) != Some(&b'{') {
            if let Err(error) = novella_core::string::decode_escapes(&inner[literal_start..i]) {
                sema.type_error(ctx.module, span, error.to_string());
            }
            let mut j = i + 1;
            let mut depth = 1;
            let mut quote = None;
            let mut escaped = false;
            while j < bytes.len() {
                let ch = bytes[j];
                if escaped {
                    escaped = false;
                } else if quote.is_some() && ch == b'\\' {
                    escaped = true;
                } else if let Some(q) = quote {
                    if ch == q {
                        quote = None;
                    }
                } else if matches!(ch, b'\'' | b'"') {
                    quote = Some(ch);
                } else if ch == b'{' {
                    depth += 1;
                } else if ch == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j += 1;
            }
            if j == bytes.len() {
                sema.type_error(ctx.module, span, "f-string 插值缺少配对的 }");
                return;
            }
            check_fstring_interpolation(
                sema,
                ctx,
                &inner[i + 1..j],
                Span {
                    start: inner_base + i,
                    end: inner_base + j + 1,
                },
            );
            i = j + 1;
            literal_start = i;
        } else if matches!(bytes[i], b'{' | b'}') && bytes.get(i + 1) == Some(&bytes[i]) {
            i += 2;
        } else if bytes[i] == b'}' {
            sema.type_error(ctx.module, span, "f-string 中的字面 } 必须写成 }}");
            i += 1;
        } else {
            i += 1;
        }
    }
    if let Err(error) = novella_core::string::decode_escapes(&inner[literal_start..]) {
        sema.type_error(ctx.module, span, error.to_string());
    }
}
/// 检查单个 f-string 插值 `{expr}`：独立 lex+parse 后类型检查。
///
/// 坐标约定（plan E07）：`span` 覆盖从 `{` 到 `}` 的插值区间；`text` 是该区间
/// 去掉花括号后的原文。先计入 trim 去除的前导字节，再以 lexer_at 设置宿主
/// 文件原点；token、AST、诊断、绑定和类型表从生成时就使用相同坐标。
/// 注释仍由 parser 的 preprocess 唯一过滤，不另行平移已登记产物。
pub(crate) fn check_fstring_interpolation(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    text: &str,
    span: Span,
) {
    let leading = text.len() - text.trim_start().len();
    let text = text.trim();
    if text.is_empty() {
        sema.type_error(ctx.module, span, "f-string 插值不能为空");
        return;
    }
    // 片段在源码中的起始偏移：`{` 之后 + 被 trim 掉的前导空白
    let base = span.start + 1 + leading;
    let lex_result = novella_lexer::lexer_at(text, base);
    if !lex_result.errors.is_empty() {
        sema.type_error(
            ctx.module,
            Span {
                start: lex_result.errors[0].token.span.start,
                end: lex_result.errors[0].token.span.end,
            },
            "f-string 插值存在词法错误",
        );
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
    // `lexer_at` 已把片段 token 平移到父坐标：插值内的 AST span、诊断、类型表键
    // 全部直接是真坐标，不需要任何事后平移（旧实现的 `adjust_diagnostics` 只平移
    // 诊断而漏掉类型表 / 绑定表，正是 plan E07 报出的错位来源）。
    check_expr(sema, ctx, &expression);
}

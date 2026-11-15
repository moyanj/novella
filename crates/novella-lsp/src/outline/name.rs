//! 名字区间定位与签名摘要：从声明 span 内找出名字本身的位置。

use novella_ast::types::{Param, TypeExpr};
use novella_core::Span;

use super::types_text::type_expr_text;

/// 在声明区间内定位名字的源码区间。
///
/// AST 只保存整条声明的 span 与字符串名字，因此在声明前缀里找**第一个**出现的
/// 完整名字（前后不能紧邻标识符字符），避免把 `Foo` 定位到 `Foobar` 里。
/// 找不到时退化为 `span.start` 处的空区间——客户端跳转仍指向声明起点。
pub(crate) fn locate_name(source: &str, span: Span, name: &str) -> Span {
    let start = span.start.min(source.len());
    let end = span.end.min(source.len()).max(start);
    let haystack = &source[start..end];
    if name.is_empty() {
        return Span { start, end: start };
    }
    let mut from = 0;
    while let Some(found) = haystack[from..].find(name) {
        let begin = from + found;
        let stop = begin + name.len();
        let before_ok = haystack[..begin]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_ident_char(ch));
        let after_ok = haystack[stop..]
            .chars()
            .next()
            .is_none_or(|ch| !is_ident_char(ch));
        if before_ok && after_ok {
            return Span {
                start: start + begin,
                end: start + stop,
            };
        }
        from = stop;
    }
    Span { start, end: start }
}

/// 标识符字符判定（与 lexer 的标识符字符集保持一致：字母、数字、`_`、`$`）。
pub(crate) fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// 由参数与返回类型拼一个签名摘要（写入 `DocumentSymbol.detail`）。
pub(crate) fn signature_text(
    params: Option<&[Param]>,
    return_type: Option<&TypeExpr>,
) -> Option<String> {
    let params = params?;
    let params = params
        .iter()
        .map(|param| match &param.ty {
            Some(ty) => format!("{}: {}", param.name, type_expr_text(ty)),
            None => param.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    match return_type {
        Some(ty) => Some(format!("({params}) -> {}", type_expr_text(ty))),
        None => Some(format!("({params})")),
    }
}

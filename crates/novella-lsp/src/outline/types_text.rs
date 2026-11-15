//! 类型表达式 → 简短文本：供 `DocumentSymbol.detail` 与补全详情使用。

use novella_ast::types::{FunctionType, PredefinedType, TypeExpr};

/// 类型表达式的简短文本（写入 `DocumentSymbol.detail` / 补全详情）。
///
/// 只做结构还原，不做解析：形如 `int[]`、`(int, string)`、`int*`。
pub fn type_expr_text(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Predefined { ty, .. } => predefined_text(*ty).to_owned(),
        TypeExpr::Reference {
            qualifier,
            name,
            generic_args,
            ..
        } => {
            let mut text = String::new();
            for segment in qualifier {
                text.push_str(segment);
                text.push('.');
            }
            text.push_str(name);
            if !generic_args.is_empty() {
                text.push('<');
                text.push_str(
                    &generic_args
                        .iter()
                        .map(type_expr_text)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                text.push('>');
            }
            text
        }
        TypeExpr::Array { element, .. } => format!("{}[]", type_expr_text(element)),
        TypeExpr::Pointer { element, .. } => format!("{}*", type_expr_text(element)),
        TypeExpr::Tuple { elements, .. } => format!(
            "({})",
            elements
                .iter()
                .map(type_expr_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExpr::Function(function_type) => function_type_text(function_type),
        TypeExpr::Union { types, .. } => types
            .iter()
            .map(type_expr_text)
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

pub(crate) fn function_type_text(function_type: &FunctionType) -> String {
    let params = function_type
        .params
        .iter()
        .map(|param| match &param.ty {
            Some(ty) => format!("{}: {}", param.name, type_expr_text(ty)),
            None => param.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "func({params}) -> {}",
        type_expr_text(&function_type.return_type)
    )
}

pub(crate) fn predefined_text(ty: PredefinedType) -> &'static str {
    use PredefinedType as P;
    match ty {
        P::Null => "null",
        P::Boolean => "boolean",
        P::Int => "int",
        P::Float => "float",
        P::Void => "void",
        P::U8 => "u8",
        P::U16 => "u16",
        P::U32 => "u32",
        P::U64 => "u64",
        P::I8 => "i8",
        P::I16 => "i16",
        P::I32 => "i32",
        P::I64 => "i64",
        P::F8 => "f8",
        P::F16 => "f16",
        P::F32 => "f32",
        P::F64 => "f64",
        P::Usize => "usize",
        P::Isize => "isize",
    }
}

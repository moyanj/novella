//! 类型表达式 → 简短文本：供 `DocumentSymbol.detail` 与补全详情使用。

use novella_ast::types::{AssociatedTypeBinding, FunctionType, PredefinedType, TypeExpr};

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
        } => reference_text(qualifier, name, generic_args, &[]),
        TypeExpr::Dynamic {
            trait_ref,
            bindings,
            ..
        } => {
            if let TypeExpr::Reference {
                qualifier,
                name,
                generic_args,
                ..
            } = trait_ref.as_ref()
            {
                format!(
                    "dyn {}",
                    reference_text(qualifier, name, generic_args, bindings)
                )
            } else {
                format!("dyn {}", type_expr_text(trait_ref))
            }
        }
        TypeExpr::Array {
            element, length, ..
        } => format!(
            "{}[{}]",
            type_expr_text(element),
            length.map(|length| length.to_string()).unwrap_or_default()
        ),
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

/// 合并位置实参与关联类型绑定，不为两组实参各写一对尖括号。
fn reference_text(
    qualifier: &[String],
    name: &str,
    generic_args: &[TypeExpr],
    bindings: &[AssociatedTypeBinding],
) -> String {
    let mut text = qualifier
        .iter()
        .map(String::as_str)
        .chain([name])
        .collect::<Vec<_>>()
        .join(".");
    if !generic_args.is_empty() || !bindings.is_empty() {
        let args = generic_args
            .iter()
            .map(type_expr_text)
            .chain(
                bindings
                    .iter()
                    .map(|binding| format!("{} = {}", binding.name, type_expr_text(&binding.ty))),
            )
            .collect::<Vec<_>>()
            .join(", ");
        text.push('<');
        text.push_str(&args);
        text.push('>');
    }
    text
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
    match &function_type.return_type {
        Some(return_type) => format!("func({params}) -> {}", type_expr_text(return_type)),
        None => format!("func({params})"),
    }
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
        P::F16 => "f16",
        P::F32 => "f32",
        P::F64 => "f64",
        P::Usize => "usize",
        P::Isize => "isize",
    }
}

//! x86-64 C ABI 的窄整数扩展，声明与调用点必须保持一致。

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::context::Context;
use novella_hir::{FunctionSignature, HirType, IntTy};

pub(crate) fn extensions(
    context: &Context,
    signature: &FunctionSignature,
) -> Vec<(AttributeLoc, Attribute)> {
    let mut attributes = Vec::new();
    for (location, ty) in std::iter::once((AttributeLoc::Return, &signature.ret)).chain(
        signature
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| (AttributeLoc::Param(index as u32), ty)),
    ) {
        let name = match ty {
            HirType::Boolean | HirType::Int(IntTy::U8 | IntTy::U16) => "zeroext",
            HirType::Int(IntTy::I8 | IntTy::I16) => "signext",
            _ => continue,
        };
        attributes.push((
            location,
            context.create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0),
        ));
    }
    attributes
}

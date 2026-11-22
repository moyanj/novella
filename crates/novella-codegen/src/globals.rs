//! 无托管静态数据与递归常量初始化；所有声明先于初值，允许前向取址。

use inkwell::types::AnyType;
use inkwell::{
    context::Context,
    module::{Linkage, Module},
    values::BasicValueEnum,
};
use novella_hir::{HirItem, HirOperand, HirProgram, HirType};

use crate::{CodegenError, Result, Types, body::Functions, value_type};

pub(crate) fn name(module: usize, name: &str) -> String {
    format!("m{module}::{name}")
}

fn unmanaged(types: &Types<'_>, ty: &HirType) -> Result<()> {
    match ty {
        HirType::Int(_)
        | HirType::Float(_)
        | HirType::Boolean
        | HirType::Pointer(_)
        | HirType::Function(_) => Ok(()),
        HirType::Tuple(elements) => elements.iter().try_for_each(|ty| unmanaged(types, ty)),
        HirType::Struct(module, name) => types
            .fields
            .get(&(*module, name.clone()))
            .ok_or_else(|| CodegenError(format!("缺少静态 struct 布局 `{name}`")))?
            .iter()
            .try_for_each(|field| {
                if field.is_weak {
                    return Err(CodegenError("静态数据不能包含 weak 字段".into()));
                }
                unmanaged(types, &field.ty)
            }),
        _ => Err(CodegenError(format!(
            "静态数据不支持托管类型或非值类型 {ty:?}"
        ))),
    }
}

pub(crate) fn declare<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    types: &Types<'ctx>,
    program: &HirProgram,
) -> Result<()> {
    for source in &program.modules {
        for item in &source.items {
            let HirItem::Global(global) = item else {
                continue;
            };
            unmanaged(types, &global.ty)?;
            let symbol = name(source.id, &global.name);
            if module.get_global(&symbol).is_some() || module.get_function(&symbol).is_some() {
                return Err(CodegenError(format!("重复静态符号 `{symbol}`")));
            }
            let value = module.add_global(value_type(context, types, &global.ty)?, None, &symbol);
            value.set_linkage(Linkage::Internal);
            value.set_constant(!global.is_mutable);
        }
    }
    Ok(())
}

pub(crate) fn initialize<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    types: &Types<'ctx>,
    functions: &Functions<'ctx, '_>,
    program: &HirProgram,
) -> Result<()> {
    for source in &program.modules {
        for item in &source.items {
            let HirItem::Global(global) = item else {
                continue;
            };
            let value = match &global.init {
                Some(init) => constant(context, module, types, functions, init, &global.ty)?,
                None => value_type(context, types, &global.ty)?.const_zero(),
            };
            module
                .get_global(&name(source.id, &global.name))
                .expect("静态声明已建立")
                .set_initializer(&value);
        }
    }
    Ok(())
}

pub(crate) fn constant<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    types: &Types<'ctx>,
    functions: &Functions<'ctx, '_>,
    operand: &HirOperand,
    ty: &HirType,
) -> Result<BasicValueEnum<'ctx>> {
    let llvm_ty = value_type(context, types, ty)?;
    match (operand, ty) {
        (HirOperand::IntConst(value), HirType::Int(_)) => Ok(llvm_ty
            .into_int_type()
            .const_int(*value as u64, false)
            .into()),
        // HIR 已执行显式整数到裸指针的转换，按目标指针宽度保留地址位。
        (HirOperand::IntConst(value), HirType::Pointer(_)) => Ok(context
            .i64_type()
            .const_int(*value as u64, false)
            .const_to_pointer(llvm_ty.into_pointer_type())
            .into()),
        (HirOperand::BoolConst(value), HirType::Boolean) => Ok(context
            .bool_type()
            .const_int(u64::from(*value), false)
            .into()),
        (HirOperand::FloatConst(value), HirType::Float(_)) => {
            Ok(llvm_ty.into_float_type().const_float(*value).into())
        }
        (HirOperand::Null, HirType::Pointer(_) | HirType::Function(_)) => {
            Ok(llvm_ty.into_pointer_type().const_null().into())
        }
        (
            HirOperand::GlobalAddress {
                module: source,
                name: source_name,
            },
            HirType::Pointer(inner),
        ) => {
            let symbol = name(*source, source_name);
            let global = module
                .get_global(&symbol)
                .ok_or_else(|| CodegenError(format!("未定义静态数据 `{symbol}`")))?;
            if global.get_value_type() != value_type(context, types, inner)?.as_any_type_enum() {
                return Err(CodegenError(format!("静态数据 `{symbol}` 取址类型不匹配")));
            }
            Ok(global.as_pointer_value().into())
        }
        (HirOperand::Function(id) | HirOperand::FunctionPtr(id), HirType::Function(expected)) => {
            let (function, signature) = functions
                .get(id)
                .ok_or_else(|| CodegenError(format!("未定义函数 `{id}`")))?;
            if crate::function_type(context, types, signature)?
                != crate::function_type(context, types, expected)?
            {
                return Err(CodegenError(format!("静态函数指针 `{id}` 签名不匹配")));
            }
            Ok(function.as_global_value().as_pointer_value().into())
        }
        (HirOperand::AggregateConst(elements), HirType::Struct(..) | HirType::Tuple(_)) => {
            let fields: Vec<_> = match ty {
                HirType::Tuple(fields) => fields.clone(),
                HirType::Struct(source, name) => types.fields[&(*source, name.clone())]
                    .iter()
                    .map(|field| field.ty.clone())
                    .collect(),
                _ => unreachable!(),
            };
            if elements.len() != fields.len() {
                return Err(CodegenError("静态聚合初值的字段数量不匹配".into()));
            }
            let values = elements
                .iter()
                .zip(&fields)
                .map(|(operand, ty)| constant(context, module, types, functions, operand, ty))
                .collect::<Result<Vec<_>>>()?;
            Ok(llvm_ty
                .into_struct_type()
                .const_named_struct(&values)
                .into())
        }
        _ => Err(CodegenError(format!(
            "不是可发射的静态常量：{operand:?} / {ty:?}"
        ))),
    }
}

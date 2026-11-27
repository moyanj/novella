//! 聚合值的引用操作，以及 class 的用户析构和自动字段清理。

use inkwell::{
    builder::Builder,
    context::Context,
    module::{Linkage, Module},
    values::{BasicValueEnum, PointerValue},
};
use novella_hir::{HirProgram, HirType, HirTypeDef};

use crate::{CodegenError, Result, Types, body::Functions, value_type};

pub(crate) fn destructor_name(ty: &HirType) -> Result<String> {
    match ty {
        HirType::Class(module, name) => Ok(format!("__novella_drop_m{module}_{name}")),
        _ => Err(CodegenError(format!("不是 class：{ty:?}"))),
    }
}

/// 结构体及元组按值复制时，递归保留/释放其拥有成员；weak 成员操作控制块。
/// 定长数组自身没有对象计数；只递归维护元素内部的拥有引用。
pub(crate) fn reference_op<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    types: &Types<'ctx>,
    builder: &Builder<'ctx>,
    operation: &str,
    value: BasicValueEnum<'ctx>,
    ty: &HirType,
) -> Result<()> {
    match ty {
        HirType::Class(..) => {
            reference_call(
                context,
                module,
                builder,
                operation,
                value.into_pointer_value(),
            )?;
        }
        HirType::Dynamic(_) => {
            let data = builder.build_extract_value(value.into_struct_value(), 0, "dyn.data")?;
            reference_call(
                context,
                module,
                builder,
                operation,
                data.into_pointer_value(),
            )?;
        }
        HirType::Array(element, length) if has_owned_members(types, element) => {
            for index in (0..*length).rev() {
                let member = builder.build_extract_value(
                    value.into_array_value(),
                    index,
                    "owned_element",
                )?;
                reference_op(context, module, types, builder, operation, member, element)?;
            }
        }
        HirType::Struct(source, name) => {
            let fields = types
                .fields
                .get(&(*source, name.clone()))
                .ok_or_else(|| CodegenError(format!("缺少 struct 布局 {name}")))?;
            for (index, field) in fields.iter().enumerate().rev() {
                let member = builder.build_extract_value(
                    value.into_struct_value(),
                    index as u32,
                    "owned_member",
                )?;
                if field.is_weak {
                    weak_reference_op(context, module, builder, name_for_weak(operation), member)?;
                } else {
                    reference_op(
                        context, module, types, builder, operation, member, &field.ty,
                    )?;
                }
            }
        }
        HirType::Tuple(elements) => {
            for (index, ty) in elements.iter().enumerate().rev() {
                let member = builder.build_extract_value(
                    value.into_struct_value(),
                    index as u32,
                    "owned_element",
                )?;
                reference_op(context, module, types, builder, operation, member, ty)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn has_owned_members(types: &Types<'_>, ty: &HirType) -> bool {
    match ty {
        HirType::Class(..) | HirType::Dynamic(_) => true,
        HirType::Array(element, length) => *length != 0 && has_owned_members(types, element),
        HirType::Tuple(elements) => elements
            .iter()
            .any(|element| has_owned_members(types, element)),
        HirType::Struct(module, name) => {
            types
                .fields
                .get(&(*module, name.clone()))
                .is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|field| field.is_weak || has_owned_members(types, &field.ty))
                })
        }
        _ => false,
    }
}

fn name_for_weak(name: &str) -> &str {
    if name == "novella_rt_retain" {
        "novella_rt_weak_retain"
    } else {
        "novella_rt_weak_release"
    }
}

fn weak_reference_op<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    operation: &str,
    value: BasicValueEnum<'ctx>,
) -> Result<()> {
    reference_call(
        context,
        module,
        builder,
        operation,
        value.into_pointer_value(),
    )
}

/// 空句柄不进入运行时；保留与释放都保持在各自的求值位置。
pub(crate) fn reference_call<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    operation: &str,
    value: PointerValue<'ctx>,
) -> Result<()> {
    let nonnull = builder.build_is_not_null(value, "arc.nonnull")?;
    if nonnull.get_zero_extended_constant() == Some(0) {
        return Ok(());
    }
    let continuation = if nonnull.get_zero_extended_constant().is_none() {
        let function = builder
            .get_insert_block()
            .and_then(|block| block.get_parent())
            .ok_or_else(|| CodegenError("引用操作缺少 LLVM 插入块".into()))?;
        let call = context.append_basic_block(function, "arc.call");
        let next = context.append_basic_block(function, "arc.next");
        builder.build_conditional_branch(nonnull, call, next)?;
        builder.position_at_end(call);
        Some(next)
    } else {
        None
    };
    let signature = context
        .void_type()
        .fn_type(&[value.get_type().into()], false);
    let function = module
        .get_function(operation)
        .unwrap_or_else(|| module.add_function(operation, signature, None));
    builder.build_call(function, &[value.into()], "")?;
    if let Some(next) = continuation {
        builder.build_unconditional_branch(next)?;
        builder.position_at_end(next);
    }
    Ok(())
}

pub(crate) fn emit_destructors<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    types: &Types<'ctx>,
    functions: &Functions<'ctx, '_>,
    program: &HirProgram,
) -> Result<()> {
    let pointer = context.ptr_type(inkwell::AddressSpace::default());
    let signature = context.void_type().fn_type(&[pointer.into()], false);
    // 分配指令已登记根析构包装；沿继承链补齐父类，其他声明不发射。
    loop {
        let mut added = false;
        for source in &program.modules {
            for definition in &source.types {
                let HirTypeDef::Class {
                    name,
                    parent: Some((parent_module, parent_name)),
                    ..
                } = definition
                else {
                    continue;
                };
                if module
                    .get_function(&destructor_name(&HirType::Class(source.id, name.clone()))?)
                    .is_some()
                {
                    let parent =
                        destructor_name(&HirType::Class(*parent_module, parent_name.clone()))?;
                    if module.get_function(&parent).is_none() {
                        module.add_function(&parent, signature, Some(Linkage::Internal));
                        added = true;
                    }
                }
            }
        }
        if !added {
            break;
        }
    }
    for source in &program.modules {
        for definition in &source.types {
            let HirTypeDef::Class {
                name,
                fields,
                parent,
                ..
            } = definition
            else {
                continue;
            };
            let ty = HirType::Class(source.id, name.clone());
            let Some(function) = module.get_function(&destructor_name(&ty)?) else {
                continue;
            };
            let builder = context.create_builder();
            builder.position_at_end(context.append_basic_block(function, "entry"));
            let object = function
                .get_first_param()
                .expect("析构有 self")
                .into_pointer_value();
            if let Some(id) = types.destructor(&ty) {
                let user = functions
                    .get(id)
                    .ok_or_else(|| CodegenError(format!("缺少析构函数 {id}")))?;
                builder.build_call(user.0, &[object.into()], "")?;
            }
            let aggregate = types.aggregate(source.id, name)?;
            let inherited = parent
                .as_ref()
                .map(|key| {
                    types
                        .fields
                        .get(key)
                        .map(Vec::len)
                        .ok_or_else(|| CodegenError("缺少父类字段布局".into()))
                })
                .transpose()?
                .unwrap_or(0);
            for (index, field) in fields.iter().enumerate().skip(inherited).rev() {
                let slot =
                    builder.build_struct_gep(aggregate, object, index as u32, "drop_field")?;
                let field_type = value_type(context, types, &field.ty)?;
                let value = builder.build_load(field_type, slot, "owned_field")?;
                builder.build_store(slot, field_type.const_zero())?;
                if field.is_weak {
                    weak_reference_op(context, module, &builder, "novella_rt_weak_release", value)?;
                } else {
                    reference_op(
                        context,
                        module,
                        types,
                        &builder,
                        "novella_rt_release",
                        value,
                        &field.ty,
                    )?;
                }
            }
            if let Some((module_id, name)) = parent {
                let parent_drop = module
                    .get_function(&destructor_name(&HirType::Class(*module_id, name.clone()))?)
                    .ok_or_else(|| CodegenError("缺少父类析构包装".into()))?;
                builder.build_call(parent_drop, &[object.into()], "")?;
            }
            builder.build_return(None)?;
        }
    }
    Ok(())
}

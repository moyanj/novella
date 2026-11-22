//! 聚合值的引用操作，以及 class 的用户析构和自动字段清理。

use inkwell::{
    builder::Builder,
    context::Context,
    module::{Linkage, Module},
    values::BasicValueEnum,
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
/// class 和数组本身只操作对象计数，最后一个所有者的 runtime 回调负责内部字段。
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
        HirType::Class(..) | HirType::Array(_) => {
            let signature = context
                .void_type()
                .fn_type(&[value.get_type().into()], false);
            let function = module
                .get_function(operation)
                .unwrap_or_else(|| module.add_function(operation, signature, None));
            builder.build_call(function, &[value.into()], "")?;
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
    let signature = context.void_type().fn_type(
        &[context.ptr_type(inkwell::AddressSpace::default()).into()],
        false,
    );
    let function = module
        .get_function(operation)
        .unwrap_or_else(|| module.add_function(operation, signature, None));
    builder.build_call(function, &[value.into()], "")?;
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

//! 从已定型的 HIR 发射 Linux x86-64 目标文件；链接由调用方负责。

mod abi;
mod body;
mod identity;
mod operators;
mod ownership;

pub use identity::llvm_library_path;

/// 当前原生目标；缓存身份与链接驱动必须和 LLVM 发射保持一致。
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";

use std::collections::HashMap;

use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType};
use inkwell::{AddressSpace, OptimizationLevel};
use novella_hir::{
    FunctionSignature, HirFunction, HirFunctionId, HirItem, HirProgram, HirType, HirTypeDef, IntTy,
};

type Result<Value> = std::result::Result<Value, CodegenError>;

/// 目标代码生成失败，可包含出错模块与函数符号。
#[derive(Debug)]
pub struct CodegenError(String);

impl std::fmt::Display for CodegenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "代码生成失败：{}", self.0)
    }
}

impl std::error::Error for CodegenError {}

impl From<inkwell::builder::BuilderError> for CodegenError {
    fn from(error: inkwell::builder::BuilderError) -> Self {
        Self(error.to_string())
    }
}

fn entry(program: &HirProgram) -> Result<&HirFunction> {
    let module = program
        .modules
        .first()
        .ok_or_else(|| CodegenError("没有入口模块".into()))?;
    let name = format!("m{}::main", module.id);
    module
        .items
        .iter()
        .find_map(|item| match item {
            HirItem::Function(function)
                if function.id.0 == name
                    && function.signature.params.is_empty()
                    && matches!(
                        function.signature.ret,
                        HirType::Void | HirType::Int(IntTy::I32)
                    ) =>
            {
                Some(function)
            }
            _ => None,
        })
        .ok_or_else(|| {
            CodegenError(format!(
                "{}：入口模块必须定义无参 `main`，返回 int 或 void",
                module.path
            ))
        })
}

pub(crate) struct Types<'ctx> {
    aggregates: HashMap<(usize, String), StructType<'ctx>>,
    fields: HashMap<(usize, String), Vec<novella_hir::HirField>>,
    destructors: HashMap<(usize, String), HirFunctionId>,
}

impl<'ctx> Types<'ctx> {
    fn build(context: &'ctx Context, program: &HirProgram) -> Result<Self> {
        let mut aggregates = HashMap::new();
        let mut fields_by_type = HashMap::new();
        let mut destructors = HashMap::new();
        for module in &program.modules {
            for definition in &module.types {
                let name = match definition {
                    HirTypeDef::Struct { name, .. }
                    | HirTypeDef::Class { name, .. }
                    | HirTypeDef::Trait { name, .. } => name,
                };
                if !matches!(definition, HirTypeDef::Trait { .. }) {
                    aggregates.insert(
                        (module.id, name.clone()),
                        context.opaque_struct_type(&format!("m{}${name}", module.id)),
                    );
                }
            }
        }
        let types = Self {
            aggregates,
            fields: HashMap::new(),
            destructors: HashMap::new(),
        };
        for module in &program.modules {
            for definition in &module.types {
                let (name, fields) = match definition {
                    HirTypeDef::Struct { name, fields, .. } => (name, fields),
                    HirTypeDef::Class {
                        name,
                        fields,
                        methods,
                        ..
                    } => {
                        if let Some(destructor) =
                            methods.iter().find(|method| method.name == "__destroy")
                        {
                            destructors
                                .insert((module.id, name.clone()), destructor.signature.id.clone());
                        }
                        (name, fields)
                    }
                    HirTypeDef::Trait { .. } => continue,
                };
                let aggregate = types
                    .aggregates
                    .get(&(module.id, name.clone()))
                    .ok_or_else(|| CodegenError(format!("缺少聚合类型 `{name}`")))?;
                let field_types = fields
                    .iter()
                    .map(|field| value_type(context, &types, &field.ty))
                    .collect::<Result<Vec<_>>>()?;
                fields_by_type.insert((module.id, name.clone()), fields.clone());
                aggregate.set_body(&field_types, false);
            }
        }
        Ok(Self {
            aggregates: types.aggregates,
            fields: fields_by_type,
            destructors,
        })
    }

    fn aggregate(&self, module: usize, name: &str) -> Result<StructType<'ctx>> {
        self.aggregates
            .get(&(module, name.to_string()))
            .copied()
            .ok_or_else(|| CodegenError(format!("未定义聚合类型 `m{module}::{name}`")))
    }

    fn field_type(&self, ty: &HirType, index: usize) -> Result<HirType> {
        let (module, name) = match ty {
            HirType::Struct(module, name) | HirType::Class(module, name) => (*module, name),
            HirType::Pointer(inner) => return self.field_type(inner, index),
            _ => return Err(CodegenError(format!("不是聚合类型：{ty:?}"))),
        };
        self.fields
            .get(&(module, name.clone()))
            .and_then(|fields| fields.get(index))
            .map(|field| field.ty.clone())
            .ok_or_else(|| CodegenError(format!("字段下标越界：{index}")))
    }

    fn field_is_weak(&self, ty: &HirType, index: usize) -> Result<bool> {
        let (module, name) = match ty {
            HirType::Struct(module, name) | HirType::Class(module, name) => (*module, name),
            HirType::Pointer(inner) => return self.field_is_weak(inner, index),
            _ => return Err(CodegenError(format!("不是聚合类型：{ty:?}"))),
        };
        self.fields
            .get(&(module, name.clone()))
            .and_then(|fields| fields.get(index))
            .map(|field| field.is_weak)
            .ok_or_else(|| CodegenError(format!("字段下标越界：{index}")))
    }

    pub(crate) fn destructor(&self, ty: &HirType) -> Option<&HirFunctionId> {
        let (module, name) = match ty {
            HirType::Class(module, name) => (*module, name),
            _ => return None,
        };
        self.destructors.get(&(module, name.clone()))
    }
}

fn value_type<'ctx>(
    context: &'ctx Context,
    types: &Types<'ctx>,
    ty: &HirType,
) -> Result<BasicTypeEnum<'ctx>> {
    match ty {
        HirType::Int(kind) => Ok(match kind {
            IntTy::U8 | IntTy::I8 => context.i8_type(),
            IntTy::U16 | IntTy::I16 => context.i16_type(),
            IntTy::U32 | IntTy::I32 => context.i32_type(),
            IntTy::U64 | IntTy::I64 | IntTy::Usize | IntTy::Isize => context.i64_type(),
        }
        .into()),
        HirType::Boolean => Ok(context.bool_type().into()),
        HirType::Float(kind) => Ok(match kind {
            novella_hir::FloatTy::F16 => context.f16_type(),
            novella_hir::FloatTy::F32 => context.f32_type(),
            novella_hir::FloatTy::F64 => context.f64_type(),
            novella_hir::FloatTy::F8 => return Err(CodegenError("LLVM 目标暂不支持 f8".into())),
        }
        .into()),
        HirType::Tuple(elements) => Ok(context
            .struct_type(
                &elements
                    .iter()
                    .map(|element| value_type(context, types, element))
                    .collect::<Result<Vec<_>>>()?,
                false,
            )
            .into()),
        HirType::Array(_) | HirType::Pointer(_) => {
            Ok(context.ptr_type(AddressSpace::default()).into())
        }
        // class 在 ABI 中始终以指向 runtime 对象载荷的 opaque pointer 表示。
        HirType::Class(_, _) => Ok(context.ptr_type(AddressSpace::default()).into()),
        HirType::Function(signature) => {
            function_type(context, types, signature)?;
            Ok(context.ptr_type(AddressSpace::default()).into())
        }
        HirType::Struct(module, name) => Ok(types.aggregate(*module, name)?.into()),
        _ => Err(CodegenError(format!("暂不支持值类型 {ty:?}"))),
    }
}

fn function_type<'ctx>(
    context: &'ctx Context,
    types: &Types<'ctx>,
    signature: &FunctionSignature,
) -> Result<FunctionType<'ctx>> {
    if signature.is_async || signature.type_param_count != 0 {
        return Err(CodegenError("暂不支持异步或泛型函数".into()));
    }
    let mut params = Vec::with_capacity(signature.params.len() + usize::from(signature.receiver));
    if signature.receiver {
        params.push(context.ptr_type(AddressSpace::default()).into());
    }
    params.extend(
        signature
            .params
            .iter()
            .map(|ty| value_type(context, types, ty).map(BasicMetadataTypeEnum::from))
            .collect::<Result<Vec<_>>>()?,
    );
    match &signature.ret {
        HirType::Void => Ok(context.void_type().fn_type(&params, false)),
        ty => Ok(value_type(context, types, ty)?.fn_type(&params, false)),
    }
}

/// 验证 LLVM 模块并发射 ELF 目标文件字节。
pub fn emit_object(program: &HirProgram) -> Result<Vec<u8>> {
    let entry = entry(program)?;
    Target::initialize_x86(&InitializationConfig::default());
    let triple = TargetTriple::create(TARGET_TRIPLE);
    let target = Target::from_triple(&triple).map_err(|error| CodegenError(error.to_string()))?;
    let machine = target
        .create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::None,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| CodegenError("无法创建 x86-64 Linux 目标机器".into()))?;
    let context = Context::create();
    let types = Types::build(&context, program)?;
    let module = context.create_module("novella");
    module.set_triple(&triple);
    module.set_data_layout(&machine.get_target_data().get_data_layout());

    let mut functions = HashMap::new();
    let mut external_abis = HashMap::new();
    for source in &program.modules {
        for item in &source.items {
            let function = match item {
                HirItem::Function(function) | HirItem::ExternFunction(function) => function,
                HirItem::Global(global) => {
                    return Err(CodegenError(format!(
                        "{}：暂不支持全局变量 `{}`",
                        source.path, global.name
                    )));
                }
            };
            let contextualize = |error: CodegenError| {
                CodegenError(format!("{}：{}：{}", source.path, function.id, error.0))
            };
            let external = matches!(item, HirItem::ExternFunction(_));
            if external != function.signature.is_extern || external != function.link_name.is_some()
            {
                return Err(contextualize(CodegenError(
                    "HIR 外部声明与链接名不一致".into(),
                )));
            }
            let ty = function_type(&context, &types, &function.signature).map_err(contextualize)?;
            let (name, linkage) = if let Some(name) = &function.link_name {
                if name.is_empty() || name == "main" {
                    return Err(contextualize(CodegenError(
                        "外部链接名为空或与 main 入口冲突".into(),
                    )));
                }
                (name.as_str(), Linkage::External)
            } else if function.id == entry.id && entry.signature.ret != HirType::Void {
                ("main", Linkage::External)
            } else {
                (function.id.0.as_str(), Linkage::Internal)
            };
            if functions.contains_key(&function.id) {
                return Err(contextualize(CodegenError("重复的 HIR 函数符号".into())));
            }
            // 多模块可声明同一个 C 符号，但它们的 ABI 必须完全一致。
            let value = if let Some(existing) = module.get_function(name) {
                if !external || existing.get_type() != ty {
                    return Err(contextualize(CodegenError(format!(
                        "链接符号 `{name}` 的声明冲突"
                    ))));
                }
                existing
            } else {
                module.add_function(name, ty, Some(linkage))
            };
            let extensions = abi::extensions(&context, &function.signature);
            if external
                && let Some(previous) = external_abis.insert(name, extensions.clone())
                && previous != extensions
            {
                return Err(contextualize(CodegenError(format!(
                    "链接符号 `{name}` 的整数扩展约定冲突"
                ))));
            }
            for (location, attribute) in extensions {
                value.add_attribute(location, attribute);
            }
            functions.insert(function.id.clone(), (value, &function.signature));
        }
    }
    ownership::emit_destructors(&context, &module, &types, &functions, program)?;
    for source in &program.modules {
        for item in &source.items {
            if let HirItem::Function(function) = item {
                body::emit(&context, &module, &types, &functions, function).map_err(|error| {
                    CodegenError(format!("{}：{}：{}", source.path, function.id, error.0))
                })?;
            }
        }
    }
    if entry.signature.ret == HirType::Void {
        let main = module.add_function("main", context.i32_type().fn_type(&[], false), None);
        let builder = context.create_builder();
        builder.position_at_end(context.append_basic_block(main, "entry"));
        builder.build_call(functions[&entry.id].0, &[], "")?;
        builder.build_return(Some(&context.i32_type().const_zero()))?;
    }
    module
        .verify()
        .map_err(|error| CodegenError(error.to_string()))?;
    machine
        .write_to_memory_buffer(&module, FileType::Object)
        .map(|buffer| buffer.as_slice().to_vec())
        .map_err(|error| CodegenError(error.to_string()))
}

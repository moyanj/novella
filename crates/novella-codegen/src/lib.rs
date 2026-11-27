//! 从已定型的 HIR 发射 x86-64 Linux 或裸机目标文件；链接由调用方负责。

mod abi;
mod body;
mod globals;
mod identity;
mod operators;
mod ownership;
mod target;

pub use identity::llvm_library_path;

/// 当前原生目标；缓存身份与链接驱动必须和 LLVM 发射保持一致。
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";

use std::collections::{HashMap, HashSet};

use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType};
use inkwell::{AddressSpace, OptimizationLevel};
use novella_core::target::{CodeModel as TargetCodeModel, OptLevel, RuntimeRequirements};
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
    bare_metal: bool,
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
            bare_metal: program.spec.is_like_bare,
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
            bare_metal: types.bare_metal,
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
    if types.bare_metal {
        target::validate_type(ty)?;
    }
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
        HirType::Array(element, length) => Ok(value_type(context, types, element)?
            .array_type(*length)
            .into()),
        HirType::Pointer(_) => Ok(context.ptr_type(AddressSpace::default()).into()),
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

/// 目标文件字节及其实际运行时依赖，供 CLI 链接与缓存共同使用。
pub struct ObjectArtifact {
    pub bytes: Vec<u8>,
    pub requirements: RuntimeRequirements,
}

/// Novella 优化级别 → LLVM `OptimizationLevel`；`-Os` 没有独立档位，按 `-O2` 取机器级优化，
/// 体积目标由 `PassManagerBuilder::set_size_level` 表达。
fn optimization_level(level: novella_core::target::OptLevel) -> OptimizationLevel {
    match level {
        novella_core::target::OptLevel::None => OptimizationLevel::None,
        novella_core::target::OptLevel::Less => OptimizationLevel::Less,
        novella_core::target::OptLevel::Default | novella_core::target::OptLevel::Size => {
            OptimizationLevel::Default
        }
        novella_core::target::OptLevel::Aggressive => OptimizationLevel::Aggressive,
    }
}

/// 按优化级别运行模块级 pass pipeline。
///
/// 走 LLVM 的新 pass manager：`run_passes` 接收与 `opt` 的 `-passes=` 相同语法的管道名，
/// `default<Ox>` 表示该级别下的完整默认管道。`-Os` 用 `default<O2>` 再关闭以空间换时间的
/// 变换（循环展开、循环/片段向量化、循环交错），从而在保持常规优化的同时压体积。
/// `-O0` 不调用本函数，目标机器也只做无优化发射。
fn optimize_module(module: &Module, machine: &TargetMachine, level: OptLevel) -> Result<()> {
    let (pipeline, plain) = match level {
        OptLevel::None => return Ok(()),
        OptLevel::Less => ("default<O1>", true),
        OptLevel::Default => ("default<O2>", true),
        OptLevel::Aggressive => ("default<O3>", true),
        OptLevel::Size => ("default<O2>", false),
    };
    let options = PassBuilderOptions::create();
    if !plain {
        options.set_loop_unrolling(false);
        options.set_loop_vectorization(false);
        options.set_loop_slp_vectorization(false);
        options.set_loop_interleaving(false);
    }
    module
        .run_passes(pipeline, machine, options)
        .map_err(|error| CodegenError(format!("优化管道 {pipeline} 执行失败：{error}")))?;
    Ok(())
}

/// 兼容仅消费目标文件字节的既有调用方。
pub fn emit_object(program: &HirProgram) -> Result<Vec<u8>> {
    emit_artifact(program).map(|artifact| artifact.bytes)
}

/// 验证 LLVM 模块并发射带依赖契约的 ELF 目标文件。
pub fn emit_artifact(program: &HirProgram) -> Result<ObjectArtifact> {
    program.target.validate().map_err(CodegenError)?;
    program.spec.validate().map_err(CodegenError)?;
    let emit = program
        .spec
        .emit
        .ok_or_else(|| CodegenError(format!("目标 {} 不可发射原生代码", program.spec.triple)))?;
    let bare = emit == novella_core::target::EmitBackend::X86_64Bare;
    // 目标描述与实际 `BuildTarget` 必须属于同一平台，否则发射出的对象与链接脚本不匹配。
    if program.spec.platform().map_err(CodegenError)? != program.target.platform {
        return Err(CodegenError(format!(
            "目标描述 {} 与目标平台 {} 不一致",
            program.spec.triple, program.target.platform
        )));
    }
    let entry = if program.no_main {
        None
    } else {
        Some(entry(program)?)
    };
    Target::initialize_x86(&InitializationConfig::default());
    let triple = TargetTriple::create(program.target.triple());
    let target = Target::from_triple(&triple).map_err(|error| CodegenError(error.to_string()))?;
    // 目标描述自带的 LLVM 特性串；裸机描述里已包含禁用向量寄存器的全部项。
    let features = program.spec.llvm_features().unwrap_or_default();
    let machine = target
        .create_target_machine(
            &triple,
            &program.spec.cpu,
            &features,
            optimization_level(program.target.opt_level),
            match program.spec.reloc_mode {
                novella_core::target::RelocMode::Pic => RelocMode::PIC,
                novella_core::target::RelocMode::Static => RelocMode::Static,
            },
            match program.target.code_model {
                TargetCodeModel::Small => CodeModel::Small,
                TargetCodeModel::Kernel => CodeModel::Kernel,
            },
        )
        .ok_or_else(|| CodegenError("无法创建 x86-64 目标机器".into()))?;
    let context = Context::create();
    let types = Types::build(&context, program)?;
    let module = context.create_module("novella");
    module.set_triple(&triple);
    module.set_data_layout(&machine.get_target_data().get_data_layout());

    let mut functions = HashMap::new();
    let mut external_abis = HashMap::new();
    let mut definitions = HashSet::new();
    let mut user_externs = HashSet::new();
    globals::declare(&context, &module, &types, program)?;
    for source in &program.modules {
        for item in &source.items {
            let function = match item {
                HirItem::Function(function) | HirItem::ExternFunction(function) => function,
                HirItem::Global(_) => continue,
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
            if external && function.export_name.is_some() {
                return Err(contextualize(CodegenError(
                    "extern 不能同时指定导出名".into(),
                )));
            }
            let ty = function_type(&context, &types, &function.signature).map_err(contextualize)?;
            let (name, linkage) = if let Some(name) = &function.link_name {
                if name.is_empty() || name.contains('\0') || (entry.is_some() && name == "main") {
                    return Err(contextualize(CodegenError(
                        "外部链接名为空或与 main 入口冲突".into(),
                    )));
                }
                (name.as_str(), Linkage::External)
            } else if let Some(name) = &function.export_name {
                if name.is_empty() || name.contains('\0') || (entry.is_some() && name == "main") {
                    return Err(contextualize(CodegenError(
                        "导出链接名为空或与 main 入口冲突".into(),
                    )));
                }
                (name.as_str(), Linkage::External)
            } else if entry.is_some_and(|entry| {
                function.id == entry.id && entry.signature.ret != HirType::Void
            }) {
                ("main", Linkage::External)
            } else {
                (function.id.0.as_str(), Linkage::Internal)
            };
            if name.starts_with("__novella_drop_") || name.starts_with("llvm.") {
                return Err(contextualize(CodegenError(format!(
                    "链接符号 `{name}` 使用后端保留命名空间"
                ))));
            }
            if functions.contains_key(&function.id) {
                return Err(contextualize(CodegenError("重复的 HIR 函数符号".into())));
            }
            if external {
                user_externs.insert(name.to_owned());
            }
            if module.get_global(name).is_some() {
                return Err(contextualize(CodegenError(format!(
                    "链接符号 `{name}` 与静态数据冲突"
                ))));
            }
            if !external && !definitions.insert(name) {
                return Err(contextualize(CodegenError(format!(
                    "链接符号 `{name}` 有多个定义"
                ))));
            }
            // 多模块可声明同一个 C 符号，也可引用本程序的导出定义；次序不影响结果。
            let value = if let Some(existing) = module.get_function(name) {
                if linkage != Linkage::External
                    || existing.get_type() != ty
                    || existing.get_linkage() != Linkage::External
                {
                    return Err(contextualize(CodegenError(format!(
                        "链接符号 `{name}` 的声明冲突"
                    ))));
                }
                existing
            } else {
                module.add_function(name, ty, Some(linkage))
            };
            let extensions = abi::extensions(&context, &function.signature);
            if let Some(previous) = external_abis.insert(name, extensions.clone())
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
    globals::initialize(&context, &module, &types, &functions, program)?;
    for source in &program.modules {
        for item in &source.items {
            if let HirItem::Function(function) = item {
                body::emit(&context, &module, &types, &functions, function).map_err(|error| {
                    CodegenError(format!("{}：{}：{}", source.path, function.id, error.0))
                })?;
            }
        }
    }
    ownership::emit_destructors(&context, &module, &types, &functions, program)?;
    if let Some(entry) =
        entry.filter(|entry| functions[&entry.id].0.get_name().to_bytes() != b"main")
    {
        let main = module.add_function("main", context.i32_type().fn_type(&[], false), None);
        let builder = context.create_builder();
        builder.position_at_end(context.append_basic_block(main, "entry"));
        let call = builder.build_call(functions[&entry.id].0, &[], "")?;
        let result = call
            .try_as_basic_value()
            .basic()
            .map(|value| value.into_int_value())
            .unwrap_or_else(|| context.i32_type().const_zero());
        builder.build_return(Some(&result))?;
    }
    if bare {
        target::attributes(&context, &module, &features);
    }
    module
        .verify()
        .map_err(|error| CodegenError(error.to_string()))?;
    // 语言级优化：只在级别允许时跑模块级 pass pipeline，`-O0` 保持逐个指令的忠实映射。
    optimize_module(&module, &machine, program.target.opt_level)?;
    let buffer = machine
        .write_to_memory_buffer(&module, FileType::Object)
        .map_err(|error| CodegenError(error.to_string()))?;
    let managed = target::audit(program, &module, &buffer, &user_externs)?;
    Ok(ObjectArtifact {
        bytes: buffer.as_slice().to_vec(),
        requirements: RuntimeRequirements { managed },
    })
}

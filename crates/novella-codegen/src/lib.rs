//! 从已定型的 HIR 发射 Linux x86-64 目标文件；链接由调用方负责。

mod body;
mod identity;
mod operators;

pub use identity::llvm_library_path;

/// 当前原生目标；缓存身份与链接驱动必须和 LLVM 发射保持一致。
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";

use std::collections::HashMap;

use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::{AddressSpace, OptimizationLevel};
use novella_hir::{HirFunction, HirItem, HirProgram, HirType, IntTy};

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

fn value_type<'ctx>(context: &'ctx Context, ty: &HirType) -> Result<BasicTypeEnum<'ctx>> {
    match ty {
        HirType::Int(kind) => Ok(match kind {
            IntTy::U8 | IntTy::I8 => context.i8_type(),
            IntTy::U16 | IntTy::I16 => context.i16_type(),
            IntTy::U32 | IntTy::I32 => context.i32_type(),
            IntTy::U64 | IntTy::I64 | IntTy::Usize | IntTy::Isize => context.i64_type(),
        }
        .into()),
        HirType::Boolean => Ok(context.bool_type().into()),
        HirType::Pointer(inner) if matches!(**inner, HirType::Int(_) | HirType::Boolean) => {
            Ok(context.ptr_type(AddressSpace::default()).into())
        }
        _ => Err(CodegenError(format!("暂不支持值类型 {ty:?}"))),
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
    let module = context.create_module("novella");
    module.set_triple(&triple);
    module.set_data_layout(&machine.get_target_data().get_data_layout());

    let mut functions = HashMap::new();
    for source in &program.modules {
        for item in &source.items {
            let function = match item {
                HirItem::Function(function) => function,
                HirItem::ExternFunction(function) => {
                    return Err(CodegenError(format!(
                        "{}：暂不支持外部函数 `{}`",
                        source.path, function.id
                    )));
                }
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
            if function.signature.receiver
                || function.signature.is_async
                || function.signature.type_param_count != 0
            {
                return Err(contextualize(CodegenError(
                    "暂不支持带接收者、异步或泛型函数".into(),
                )));
            }
            let params = function
                .signature
                .params
                .iter()
                .map(|ty| value_type(&context, ty).map(BasicMetadataTypeEnum::from))
                .collect::<Result<Vec<_>>>()
                .map_err(contextualize)?;
            let ty = match &function.signature.ret {
                HirType::Void => context.void_type().fn_type(&params, false),
                ty => value_type(&context, ty)
                    .map_err(contextualize)?
                    .fn_type(&params, false),
            };
            let (name, linkage) = if function.id == entry.id && entry.signature.ret != HirType::Void
            {
                ("main", Linkage::External)
            } else {
                (function.id.0.as_str(), Linkage::Internal)
            };
            if functions.contains_key(&function.id) {
                return Err(contextualize(CodegenError("重复的 HIR 函数符号".into())));
            }
            functions.insert(
                function.id.clone(),
                (
                    module.add_function(name, ty, Some(linkage)),
                    &function.signature,
                ),
            );
        }
    }
    for source in &program.modules {
        for item in &source.items {
            if let HirItem::Function(function) = item {
                body::emit(&context, &module, &functions, function).map_err(|error| {
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

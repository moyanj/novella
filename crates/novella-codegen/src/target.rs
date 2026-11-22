//! 裸机目标约束与最终目标文件的依赖审计。

use std::collections::HashSet;

use inkwell::values::BasicValue;
use inkwell::{
    attributes::{Attribute, AttributeLoc},
    context::Context,
    memory_buffer::MemoryBuffer,
    module::Module,
};
use novella_core::target::RuntimeMode;
use novella_hir::{HirProgram, HirType};

pub(crate) fn validate_type(ty: &HirType) -> Result<()> {
    match ty {
        HirType::Float(_) => Err(CodegenError("裸机目标不支持显式浮点类型".into())),
        HirType::Pointer(inner) | HirType::Array(inner) => validate_type(inner),
        HirType::Tuple(elements) => elements.iter().try_for_each(validate_type),
        HirType::Function(signature) => {
            validate_type(&signature.ret)?;
            signature.params.iter().try_for_each(validate_type)
        }
        _ => Ok(()),
    }
}

use crate::{CodegenError, Result};

/// 禁用所有向量寄存器，避免整数聚合复制被选择为 SIMD 指令。
pub(crate) const FEATURES: &str = "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,-avx512f,-avx512vl,-avx512bw,-avx512dq,-fma,-fma4,-xop,+soft-float";

pub(crate) fn attributes(context: &Context, module: &Module<'_>) {
    // 统一在全部辅助函数生成后应用，避免自动析构和数组包装遗漏约束。
    for function in module.get_functions() {
        for name in ["noredzone", "noimplicitfloat", "nounwind"] {
            function.add_attribute(
                AttributeLoc::Function,
                context.create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0),
            );
        }
        function.add_attribute(
            AttributeLoc::Function,
            context.create_string_attribute("target-features", FEATURES),
        );
        function.add_attribute(
            AttributeLoc::Function,
            context.create_string_attribute("frame-pointer", "all"),
        );
    }
}

pub(crate) fn audit(
    program: &HirProgram,
    module: &Module<'_>,
    buffer: &MemoryBuffer<'_>,
    user_externs: &HashSet<String>,
) -> Result<bool> {
    let mut managed = program.requirements.managed;
    // HIR 的需求之外，后端实际引入的 runtime 调用也必须纳入链接契约。
    for function in module.get_functions() {
        let name = function.get_name().to_string_lossy();
        if name.starts_with("novella_rt_")
            && function
                .as_global_value()
                .as_pointer_value()
                .get_first_use()
                .is_some()
        {
            managed = true;
        }
    }
    let binary = buffer
        .create_binary_file(None)
        .map_err(|error| CodegenError(error.to_string()))?;
    let mut source_sections = binary
        .get_sections()
        .ok_or_else(|| CodegenError("目标文件缺少节表".into()))?;
    // 只检查重定位真正引用的符号，避免将 ELF 的 FILE/ABS 元数据当成 undefined。
    while let Some(source_section) = source_sections.next_section() {
        let mut relocations = source_section.get_relocations();
        while let Some(relocation) = relocations.next_relocation() {
            let symbol = relocation.get_symbol();
            let Some(name) = symbol.get_name() else {
                continue;
            };
            let name = name.to_string_lossy();
            if name.is_empty() {
                continue;
            }
            let mut sections = binary
                .get_sections()
                .ok_or_else(|| CodegenError("目标文件缺少节表".into()))?;
            let mut defined = false;
            while let Some(section) = sections.next_section() {
                if section.contains_symbol(&symbol) {
                    defined = true;
                    break;
                }
            }
            if defined {
                continue;
            }
            if name.starts_with("novella_rt_") {
                managed = true;
            } else if program.target.is_bare_metal()
                && !user_externs.contains(name.as_ref())
                && !matches!(name.as_ref(), "memcpy" | "memmove" | "memset" | "memcmp")
            {
                // 按来源区分用户 extern 与 LLVM 隐式 helper；不能按 __ 前缀误伤用户符号。
                return Err(CodegenError(format!(
                    "裸机目标不支持编译器引入的外部 helper `{name}`"
                )));
            }
            if program.target.is_bare_metal()
                && (name.starts_with("_Unwind_")
                    || name.starts_with("__cxa_")
                    || matches!(
                        name.as_ref(),
                        "__gxx_personality_v0"
                            | "__gxx_personality_seh0"
                            | "__gcc_personality_v0"
                            | "rust_eh_personality"
                    ))
            {
                return Err(CodegenError(format!("裸机目标不支持异常辅助依赖 `{name}`")));
            }
        }
    }
    if program.target.runtime == RuntimeMode::None && managed {
        return Err(CodegenError(
            "runtime=none 的目标文件仍包含 novella_rt 依赖".into(),
        ));
    }
    Ok(managed)
}

//! 原子链接输出；裸机只调用 ELF 链接器，不引入宿主 CRT 或 libc。
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use novella_codegen::ObjectArtifact;
use novella_core::{
    project_config::ResolvedBuildConfig,
    target::{CodeModel, RuntimeMode},
};
use object::read::elf::ProgramHeader;
use object::{Object, ObjectSection, ObjectSymbol};

pub(crate) fn output_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

pub(crate) fn validate_config(config: &ResolvedBuildConfig) -> Result<(), Box<dyn Error>> {
    if config.target.is_bare_metal() {
        if config.entry.is_none() || config.linker_script.is_none() {
            return Err(
                "裸机可执行文件必须指定 --entry 和 --linker-script（或清单中的对应项）".into(),
            );
        }
        if let Some(linker) = &config.linker {
            let name = linker
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.contains("clang") || name.contains("gcc") || name == "cc" {
                return Err(
                    "裸机目标必须指定 ELF 链接器（如 ld.lld 或 ld），不能使用编译器驱动".into(),
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn write_executable(
    path: &Path,
    artifact: &ObjectArtifact,
    config: &ResolvedBuildConfig,
) -> Result<(), Box<dyn Error>> {
    validate_config(config)?;
    if artifact.requirements.managed && config.target.runtime == RuntimeMode::None {
        return Err("目标文件需要 managed runtime，但当前配置为 runtime=none".into());
    }
    validate_object(&artifact.bytes, "编译产物")?;
    // 额外输入必须是同架构的可重定位 ELF；不接受共享库、归档或可执行文件。
    let cwd = std::env::current_dir()?;
    let extra_objects: Vec<_> = config
        .link_object
        .iter()
        .map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                cwd.join(path)
            }
        })
        .collect();
    for object in &extra_objects {
        let bytes = fs::read(object)
            .map_err(|error| format!("无法读取链接对象 '{}'：{error}", object.display()))?;
        validate_object(&bytes, &object.display().to_string())?;
    }
    let staging = tempfile::tempdir_in(output_parent(path))?;
    let object = staging.path().join("program.o");
    fs::write(&object, &artifact.bytes)?;
    let output = staging.path().join("artifact");
    let bare = config.target.is_bare_metal();
    let kernel = config.target.code_model == CodeModel::Kernel;
    let linker = config
        .linker
        .clone()
        .unwrap_or_else(|| PathBuf::from(if bare { "ld.lld" } else { "clang" }));
    let mut command = Command::new(&linker);
    if bare {
        command.args([
            "-m",
            "elf_x86_64",
            "--static",
            "--no-undefined",
            "--fatal-warnings",
            "--build-id=none",
        ]);
    } else {
        command.arg(format!("--target={}", config.target.triple()));
    }
    command.arg(&object).args(&extra_objects);
    if artifact.requirements.managed {
        let managed = staging.path().join("libnovella_managed.a");
        fs::write(&managed, novella_runtime::managed_archive(bare, kernel))?;
        command.arg(managed);
    }
    if bare {
        // 裸机显式提供内存操作；Linux 保持由宿主 libc 提供的原有行为。
        let support = staging.path().join("libnovella_support.a");
        fs::write(&support, novella_runtime::support_archive(bare, kernel))?;
        command.arg(support);
    }
    if let Some(entry) = &config.entry {
        if bare {
            command.arg("--entry").arg(entry);
        } else {
            command.arg(format!("-Wl,--entry={entry}"));
        }
    }
    if let Some(script) = &config.linker_script {
        if bare {
            command.arg("--script").arg(script);
        } else {
            command
                .arg("-Xlinker")
                .arg("--script")
                .arg("-Xlinker")
                .arg(script);
        }
    }
    if !bare {
        // LLVM 的浮点余数指令由 clang 降为 libm 的 fmod；即使当前模块未使用 `%`，
        // -lm 也无副作用，且保持普通 Linux 目标的链接行为一致。
        command.arg("-lm");
    }
    command.arg("-o").arg(&output);
    let result = command
        .output()
        .map_err(|error| format!("无法运行链接器 '{}'：{error}", linker.display()))?;
    if !result.status.success() {
        return Err(format!(
            "{} 链接失败（{}）：{}",
            linker.display(),
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        )
        .into());
    }
    if bare {
        validate_executable(&fs::read(&output)?)?;
    }
    // 失败路径不会触碰原输出；发布动作与最终文件处于同一文件系统。
    fs::rename(&output, path)?;
    Ok(())
}

fn elf(bytes: &[u8]) -> Result<object::File<'_>, Box<dyn Error>> {
    let file = object::File::parse(bytes)?;
    if file.format() != object::BinaryFormat::Elf
        || file.architecture() != object::Architecture::X86_64
        || !file.is_64()
        || !file.is_little_endian()
    {
        return Err("链接输入必须为 little-endian ELF64 x86_64，禁止目标架构混用".into());
    }
    // EI_OSABI 只能是 System V 或 Linux；其它 OS ABI 不属于当前两个目标。
    if !matches!(bytes.get(7), Some(0 | 3)) {
        return Err("ELF OS ABI 与目标不兼容".into());
    }
    Ok(file)
}

fn validate_object(bytes: &[u8], name: &str) -> Result<(), Box<dyn Error>> {
    let file = elf(bytes).map_err(|error| format!("链接对象 '{name}'：{error}"))?;
    if file.kind() != object::ObjectKind::Relocatable {
        return Err(
            format!("链接对象 '{name}' 必须为可重定位目标文件，禁止动态库或可执行文件").into(),
        );
    }
    Ok(())
}

fn validate_executable(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let file = elf(bytes)?;
    if file.kind() != object::ObjectKind::Executable {
        return Err("裸机输出必须为静态 ELF 可执行文件".into());
    }
    // 段头同样需要验证：即使链接脚本移除节名，动态加载仍由 PT_DYNAMIC/PT_INTERP 指定。
    if let object::File::Elf64(elf) = &file
        && elf.elf_program_headers().iter().any(|header| {
            matches!(
                header.p_type(elf.endian()),
                object::elf::PT_DYNAMIC | object::elf::PT_INTERP
            )
        })
    {
        return Err("裸机输出包含动态加载段".into());
    }
    if file
        .sections()
        .any(|section| matches!(section.name(), Ok(".dynamic" | ".interp")))
        || file.dynamic_symbols().next().is_some()
    {
        return Err("裸机输出包含动态链接信息".into());
    }
    if let Some(symbol) = file
        .symbols()
        .find(|symbol| symbol.is_undefined() && symbol.name().is_ok_and(|name| !name.is_empty()))
    {
        return Err(format!(
            "裸机输出含未定义符号：{}",
            symbol.name().unwrap_or("<未知>")
        )
        .into());
    }
    Ok(())
}

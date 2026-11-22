//! runtime 分别构建 hosted 和裸机归档；目标独立于构建编译器本身的平台。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(command: &mut Command) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("无法运行 runtime 构建命令 {command:?}：{error}"));
    assert!(
        output.status.success(),
        "runtime 构建失败 {command:?}（{}）：\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn compile(compiler: &OsStr, output: &Path, variant: &str, source: &str) -> PathBuf {
    let object = output.join(format!("{variant}-{source}.o"));
    let mut command = Command::new(compiler);
    command.args([
        "-std=c11",
        "-O2",
        "-ffreestanding",
        "-fno-builtin",
        "-fno-stack-protector",
        "-fno-unwind-tables",
        "-fno-asynchronous-unwind-tables",
        "-fvisibility=hidden",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-Iinclude",
    ]);
    if variant == "linux" {
        command.args(["--target=x86_64-unknown-linux-gnu", "-fPIC"]);
    } else {
        command.args([
            "--target=x86_64-unknown-none-elf",
            "-mno-red-zone",
            "-mgeneral-regs-only",
            "-fno-pic",
            "-fno-pie",
        ]);
        command.arg(if variant == "bare-kernel" {
            "-mcmodel=kernel"
        } else {
            "-mcmodel=small"
        });
    }
    run(command
        .arg("-c")
        .arg(format!("src/{source}.c"))
        .arg("-o")
        .arg(&object));
    object
}

fn archive(archiver: &OsStr, path: &Path, objects: &[PathBuf]) {
    // 先写空归档，避免增量构建保留已移除或改名的对象成员。
    std::fs::write(path, b"!<arch>\n").expect("必须能够初始化 runtime 归档");
    run(Command::new(archiver).arg("crsD").arg(path).args(objects));
}

fn main() {
    for path in [
        "build.rs",
        "src/rt.c",
        "src/hosted.c",
        "src/memory.c",
        "include/novella_rt.h",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in ["NOVELLA_RUNTIME_CC", "NOVELLA_RUNTIME_AR"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo 必须提供 OUT_DIR"));
    let compiler = std::env::var_os("NOVELLA_RUNTIME_CC").unwrap_or_else(|| "clang".into());
    let archiver = std::env::var_os("NOVELLA_RUNTIME_AR").unwrap_or_else(|| "ar".into());
    for variant in ["linux", "bare-small", "bare-kernel"] {
        let mut managed = vec![compile(&compiler, &output, variant, "rt")];
        if variant == "linux" {
            managed.push(compile(&compiler, &output, variant, "hosted"));
        }
        let managed_name = if variant == "linux" {
            "libnovella_rt.a".to_owned()
        } else {
            format!("libnovella_rt-{variant}.a")
        };
        archive(&archiver, &output.join(managed_name), &managed);
        let memory = compile(&compiler, &output, variant, "memory");
        archive(
            &archiver,
            &output.join(format!("libnovella_support-{variant}.a")),
            &[memory],
        );
    }
}

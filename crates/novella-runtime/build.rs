//! runtime 以目标平台 C ABI 构建；不把系统层实现混入标准库源码。

use std::path::PathBuf;
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

fn main() {
    for path in ["src/object.c", "include/novella_rt.h"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in ["NOVELLA_RUNTIME_CC", "NOVELLA_RUNTIME_AR"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo 必须提供 OUT_DIR"));
    let object = output.join("object.o");
    let archive = output.join("libnovella_rt.a");
    let compiler = std::env::var_os("NOVELLA_RUNTIME_CC").unwrap_or_else(|| "clang".into());
    let archiver = std::env::var_os("NOVELLA_RUNTIME_AR").unwrap_or_else(|| "ar".into());
    run(Command::new(compiler)
        .args([
            "--target=x86_64-unknown-linux-gnu",
            "-std=c11",
            "-O2",
            "-fPIC",
            "-fvisibility=hidden",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Iinclude",
            "-c",
            "src/object.c",
            "-o",
        ])
        .arg(&object));
    run(Command::new(archiver).arg("crsD").arg(archive).arg(object));
}

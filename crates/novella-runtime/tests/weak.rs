//! 直接通过公开 C ABI 验证真实静态库，覆盖引用销毁和多线程弱升级竞争。

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::path::PathBuf;
use std::process::Command;

#[test]
fn weak_lifetime_and_concurrent_upgrade() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let executable =
        PathBuf::from(env!("OUT_DIR")).join(format!("weak-abi-test-{}", std::process::id()));
    let compiler = std::env::var_os("NOVELLA_RUNTIME_CC").unwrap_or_else(|| "clang".into());
    let compilation = Command::new(compiler)
        .current_dir(&root)
        .args([
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-pthread",
            "-Iinclude",
        ])
        .arg("tests/weak.c")
        .arg(PathBuf::from(env!("OUT_DIR")).join("libnovella_rt.a"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("必须能够运行 C 编译器以验证 runtime ABI");
    assert!(
        compilation.status.success(),
        "runtime ABI 测试编译失败：{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let execution = Command::new(executable)
        .output()
        .expect("必须能够执行 runtime ABI 测试");
    assert!(
        execution.status.success(),
        "runtime ABI 测试失败（{}）：{}",
        execution.status,
        String::from_utf8_lossy(&execution.stderr)
    );
}

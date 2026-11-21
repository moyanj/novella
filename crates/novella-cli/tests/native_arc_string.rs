//! 跨 sema → HIR → LLVM → runtime 的真实原生回归。

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::{
    path::Path,
    process::{Command, Output},
};

fn compile(source: &Path, output: &Path) {
    let result = Command::new(env!("CARGO_BIN_EXE_novella"))
        .args([
            "compile",
            source.to_str().expect("源码路径"),
            "-o",
            output.to_str().expect("输出路径"),
        ])
        .output()
        .expect("启动编译器");
    assert!(
        result.status.success(),
        "{}\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn run(program: &Path) -> Output {
    Command::new(program).output().expect("执行原生程序")
}

#[test]
fn string_arc_and_weak_examples() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temporary = tempfile::tempdir().expect("创建原生回归临时目录");
    for (name, code, stdout) in [
        ("string-literal", 8, ""),
        ("string-ops", 11, ""),
        ("arc-weak", 0, "ABCED"),
        ("runtime-weak", 42, "APCZ"),
        ("arc-lifetime", 0, "ABCDEFGH"),
        ("arc-aggregate", 0, "A"),
        ("array-arc", 0, "ABCEDGFJHI"),
        ("string-format", 0, ""),
    ] {
        let output = temporary.path().join(name);
        compile(
            &workspace.join("examples").join(format!("{name}.nov")),
            &output,
        );
        let result = run(&output);
        assert_eq!(
            result.status.code(),
            Some(code),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, stdout.as_bytes(), "{name}");
    }
}

#[test]
fn no_std_disables_string_but_keeps_user_classes() {
    let temporary = tempfile::tempdir().expect("创建 no_std 回归临时目录");
    // 用户同名类型及 string.nov 文件不得触发标准库 String 特殊降级。
    let source = temporary.path().join("string.nov");
    std::fs::write(&source, "@no_std;\nclass String {}\nfunc main() -> int { let value = new String(); if (value == value) { return 0; } return 1; }\n").expect("写入用户 String 示例");
    let executable = temporary.path().join("user-string");
    compile(&source, &executable);
    assert!(run(&executable).status.success());

    let source = temporary.path().join("no-std-string.nov");
    std::fs::write(
        &source,
        "@no_std;\nfunc main() -> int { let value = \"unavailable\"; return 0; }\n",
    )
    .expect("写入禁用标准库示例");
    let result = Command::new(env!("CARGO_BIN_EXE_novella"))
        .args(["check", source.to_str().expect("源码路径")])
        .output()
        .expect("运行语义检查");
    assert!(!result.status.success(), "no_std 不应隐式提供 String");
    assert!(String::from_utf8_lossy(&result.stderr).contains("String"));
}

#[test]
fn rejects_destructor_resurrection_and_weak_addresses() {
    let temporary = tempfile::tempdir().expect("创建所有权诊断临时目录");
    for (name, declarations, body, diagnostic) in [
        (
            "direct",
            "pub func __destroy(self) { inspect(this); }",
            "",
            "不能建立或捕获 self 的强引用",
        ),
        (
            "return-helper",
            "pub func myself(self) -> Node { return this; } pub func __destroy(self) { let value = this.myself(); }",
            "",
            "不能调用或提取自身实例方法",
        ),
        (
            "void-helper",
            "pub func escape(self) { inspect(this); } pub func __destroy(self) { this.escape(); }",
            "",
            "不能调用或提取自身实例方法",
        ),
        (
            "weak-address",
            "pub weak parent: Node;",
            "let pointer = &node.parent;",
            "weak",
        ),
    ] {
        let source = temporary.path().join(format!("{name}.nov"));
        std::fs::write(&source, format!("@no_std;\nclass Node {{ {declarations} }}\nfunc inspect(value: Node) {{}}\nfunc main() -> int {{ let node = new Node(); {body} return 0; }}\n")).expect("写入所有权负例");
        let result = Command::new(env!("CARGO_BIN_EXE_novella"))
            .args(["check", source.to_str().expect("源码路径")])
            .output()
            .expect("运行所有权检查");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{name} 不应通过语义检查");
        assert!(stderr.contains(diagnostic), "{name}: {stderr}");
    }
}

#[test]
fn scalar_no_std_does_not_pull_in_runtime() {
    let temporary = tempfile::tempdir().expect("创建链接回归临时目录");
    let source = temporary.path().join("scalar.nov");
    std::fs::write(&source, "@no_std;\nfunc main() -> int { return 42; }\n").expect("写入标量示例");
    let executable = temporary.path().join("scalar");
    compile(&source, &executable);
    assert_eq!(run(&executable).status.code(), Some(42));
    let symbols = Command::new("nm")
        .args(["-g", "--defined-only"])
        .arg(&executable)
        .output()
        .expect("检查 ELF 定义符号");
    assert!(symbols.status.success());
    assert!(!String::from_utf8_lossy(&symbols.stdout).contains("novella_rt_"));
}

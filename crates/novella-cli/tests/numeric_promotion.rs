//! 隐式数值提升的 CLI 回归：sema 规则、HIR 比较精度与原生运行结果。
//!
//! 断言只依赖稳定诊断码 `NVL3002`，不按诊断文本匹配。

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::{
    path::Path,
    process::{Command, Output},
};

fn compiler() -> Command {
    Command::new(env!("CARGO_BIN_EXE_novella"))
}

/// 编译源码并运行产物，返回进程输出。
fn compile_and_run(directory: &Path, name: &str, source: &str) -> Output {
    let source_path = directory.join(format!("{name}.nov"));
    std::fs::write(&source_path, source).expect("写入回归源码");
    let executable = directory.join(name);
    let result = compiler()
        .args([
            "compile",
            source_path.to_str().expect("源码路径"),
            "-o",
            executable.to_str().expect("输出路径"),
        ])
        .output()
        .expect("启动编译器");
    assert!(
        result.status.success(),
        "{name} 编译失败:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Command::new(&executable).output().expect("执行原生程序")
}

/// 用 `check` 断言源码被拒，且至少出现一个 `NVL3002` 类型错误。
fn expect_type_error(directory: &Path, name: &str, source: &str) {
    let source_path = directory.join(format!("{name}.nov"));
    std::fs::write(&source_path, source).expect("写入反例源码");
    let result = compiler()
        .args(["check", source_path.to_str().expect("源码路径")])
        .output()
        .expect("启动编译器");
    assert!(
        !result.status.success(),
        "{name} 本应被拒绝，stderr:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("NVL3002"), "{name} 缺少 NVL3002:\n{stderr}");
}

#[test]
fn mixed_width_comparison_does_not_truncate() {
    let temporary = tempfile::tempdir().expect("创建比较回归临时目录");
    // 若比较把右侧截成左侧类型，2147483648 会变成负数，断言会翻转。
    let output = compile_and_run(
        temporary.path(),
        "mixed-comparison",
        r#"func main() -> int {
    let a: i32 = 2147483647;
    let b: i64 = 2147483648;
    if (!(a < b)) { return 1; }
    if (!(a <= b)) { return 2; }
    if (!(b > a)) { return 3; }
    if (!(a != b)) { return 4; }
    if (a == b) { return 5; }

    let u: u32 = 4294967295;
    let v: i64 = 4294967296;
    if (!(u < v)) { return 6; }

    let f: f32 = 1.5;
    let g: f64 = 1.5000001;
    if (!(f < g)) { return 7; }
    return 0;
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn widening_flows_through_arguments_returns_and_ternary() {
    let temporary = tempfile::tempdir().expect("创建扩宽回归临时目录");
    let output = compile_and_run(
        temporary.path(),
        "widening",
        r#"func takes_i16(value: i16) -> i16 { return value; }
func takes_f64(value: f64) -> f64 { return value; }
func pick(flag: boolean, a: i32, b: i64) -> i64 { return flag ? a : b; }

func main() -> int {
    let narrow: u8 = 200;
    let wide: i16 = 300;
    let single: f32 = 1.5;
    let a: i32 = 1;
    let b: i64 = 2;

    let sum: i16 = narrow + wide;
    if (sum != 500) { return 1; }

    let promoted: i64 = a + b;
    if (promoted != 3) { return 2; }

    let float_sum: f64 = single + 2.25;
    if (float_sum != 3.75) { return 3; }

    let via_argument: i16 = takes_i16(narrow);
    if (via_argument != 200) { return 4; }

    let via_return: f64 = takes_f64(single);
    if (via_return != 1.5) { return 5; }

    let from_ternary: i64 = true ? a : b;
    if (from_ternary != 1) { return 6; }
    if (pick(false, a, b) != 2) { return 7; }
    return 0;
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn array_unification_and_global_const_are_folded() {
    let temporary = tempfile::tempdir().expect("创建数组合一临时目录");
    let output = compile_and_run(
        temporary.path(),
        "aggregate-const",
        r#"const BASE: u8 = 200;
const OFFSET: i16 = 32767;
const WRAPPED: i64 = BASE + OFFSET;
const SINGLE: f32 = 0.1;
const DOUBLE: f64 = 0.2;
const FLOAT_FOLDED: f64 = SINGLE + DOUBLE;

func main() -> int {
    let narrow: u8 = 7;
    let wide: i16 = 8;
    let integers: i16[2] = [narrow, wide];
    if (integers[0] != 7 || integers[1] != 8) { return 1; }

    let small: i32 = 5;
    let large: i64 = 6;
    let promoted: i64[2] = [small, large];
    if (promoted[0] != 5 || promoted[1] != 6) { return 2; }

    let single: f32 = 0.5;
    let double: f64 = 0.25;
    let floats: f64[2] = [single, double];
    if (floats[0] != 0.5 || floats[1] != 0.25) { return 3; }

    const total: i64 = BASE + OFFSET;
    if (total != -32569 || WRAPPED != -32569) { return 4; }
    let runtime_float: f64 = SINGLE + DOUBLE;
    if (FLOAT_FOLDED != runtime_float) { return 5; }
    return 0;
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn narrowing_cross_family_and_out_of_range_literals_are_rejected() {
    let temporary = tempfile::tempdir().expect("创建拒绝回归临时目录");
    for (name, source) in [
        (
            "narrow-int",
            "func main() -> int { let wide: i64 = 1; let narrow: i32 = wide; return 0; }\n",
        ),
        (
            "narrow-float",
            "func main() -> int { let wide: f64 = 1.5; let narrow: f32 = wide; return 0; }\n",
        ),
        (
            "unsafe-cross-sign-assignment",
            "func main() -> int { let wide: u64 = 1; let signed: i64 = wide; return 0; }\n",
        ),
        (
            "no-common-type",
            "func main() -> int { let a: u64 = 1; let b: i64 = 2; let sum = a + b; return 0; }\n",
        ),
        (
            "cross-family",
            "func main() -> int { let a: i32 = 1; let b: f64 = 2.0; let sum = a + b; return 0; }\n",
        ),
        (
            "literal-to-float",
            "func main() -> int { let value: f64 = 1; return 0; }\n",
        ),
        (
            "out-of-range-literal",
            "func main() -> int { let a: i8 = 1; let b = a + 200; return 0; }\n",
        ),
    ] {
        expect_type_error(temporary.path(), name, source);
    }
}

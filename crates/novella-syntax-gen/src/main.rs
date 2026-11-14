//! `novella-syntax-gen`：从 lexer 的 logos 声明生成 TextMate 语法。
//!
//! 用法：
//!
//! ```bash
//! cargo run -p novella-syntax-gen                 # 写 editors/vscode/syntaxes/novella.tmLanguage.json
//! cargo run -p novella-syntax-gen -- --check      # 只校验生成物是否最新（CI / just 用）
//! ```
//!
//! 生成的语法只用于编辑器**冷启动期**着色——LSP 语义 token 返回之前的几百毫秒。
//! 语言的事实源始终是 `novella-lexer`，语法文件不得反向约束词法实现。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use novella_syntax_gen::{default_output, generate, write_grammar};

fn main() -> ExitCode {
    // 工作区根目录：本 crate 位于 <root>/crates/novella-syntax-gen
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(root) = manifest_dir.parent().and_then(Path::parent) else {
        eprintln!("无法定位工作区根目录：{manifest_dir:?}");
        return ExitCode::FAILURE;
    };

    let lexer_source = match fs::read_to_string(root.join("crates/novella-lexer/src/lib.rs")) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("读取 lexer 源码失败：{err}");
            return ExitCode::FAILURE;
        }
    };

    let grammar = generate(&lexer_source, &novella_core::keywords::KEYWORDS);
    let output = default_output(root);
    let check = env::args().any(|arg| arg == "--check");

    if check {
        return check_output(&output, &grammar);
    }

    match write_grammar(&output, &grammar) {
        Ok(()) => {
            println!("已生成 {}", output.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("写入语法文件失败：{err}");
            ExitCode::FAILURE
        }
    }
}

/// `--check`：生成物与源码不一致时报错（提示重新生成），用于检查生成物是否过期。
fn check_output(output: &Path, grammar: &str) -> ExitCode {
    match fs::read_to_string(output) {
        Ok(existing) if existing == grammar => {
            println!("语法文件已是最新：{}", output.display());
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!(
                "语法文件已过期：{}（请运行 `just gen-syntax` 重新生成）",
                output.display()
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("读取语法文件失败：{err}（请运行 `just gen-syntax`）");
            ExitCode::FAILURE
        }
    }
}

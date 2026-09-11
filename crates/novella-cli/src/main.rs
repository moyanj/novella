use crate::error::render_diagnostic;
use clap::Parser;
use novella_core::diagnostic::Diagnostic;
use std::{fs, io, process::exit};

mod cli;
mod error;

fn main() {
    let args = cli::Cli::parse();
    match args.command {
        cli::Command::Compile { file, output } => {
            println!("Compiling...");
            // 读取失败：区分常见原因给出友好提示，并带上文件路径
            let source_code = match fs::read_to_string(&file) {
                Ok(r) => r,
                Err(e) => {
                    let hint = match e.kind() {
                        io::ErrorKind::NotFound => "文件不存在",
                        io::ErrorKind::PermissionDenied => "没有读取权限",
                        _ => "读取失败",
                    };
                    let diag = Diagnostic::error(format!("无法读取文件 '{file}'：{hint}（{e}）"));
                    eprintln!("{}", render_diagnostic(&diag, None));
                    exit(1);
                }
            };
            let result = novella_lexer::lexer(&source_code);
            // 词法错误（非法字符 / 未闭合字符串）可恢复，就近渲染到 stderr，不中断输出
            for err in &result.errors {
                let diag = err.to_diagnostic(Some(&file));
                eprintln!("{}", render_diagnostic(&diag, Some(&source_code)));
            }
            if !result.errors.is_empty() {
                eprintln!("检测到 {} 个词法错误", result.errors.len());
            }
            fs::write(
                &output,
                serde_json::to_string_pretty(&result.tokens).unwrap(),
            )
            .unwrap_or_else(|e| {
                let diag = Diagnostic::error(format!("无法写入文件 '{output}'：{e}"));
                eprintln!("{}", render_diagnostic(&diag, None));
                exit(1);
            });
        }
    }
}

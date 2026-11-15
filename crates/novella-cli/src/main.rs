use crate::error::render_diagnostic;
use clap::Parser;
use novella_core::diagnostic::Diagnostic;
use novella_parser::ParserInput;
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
            // ---------- 词法分析 ----------
            let lex_result = novella_lexer::lexer(&source_code);
            // 词法错误（非法字符 / 未闭合字符串）可恢复，就近渲染到 stderr，不中断输出
            for err in &lex_result.errors {
                let diag = err.to_diagnostic(Some(&file));
                eprintln!("{}", render_diagnostic(&diag, Some(&source_code)));
            }

            // ---------- 语法分析 ----------
            // parser 消费 token 流产出 AST；为保留 tokens 于 JSON 输出，此处克隆一份。
            // 注释 token（LineComment）在语法层面视为空白，parser 不感知：
            // 喂给 parser 前过滤，JSON 输出则保留注释（供高亮等消费方使用）。
            // 注：未来 parser 若需要 doc comment，应改为 parser 内部吞注释、移除此处过滤。
            let parser_tokens: Vec<_> = lex_result.tokens.clone();
            let mut parse_result = {
                let mut parser = novella_parser::Parser::new(ParserInput {
                    file: file.clone(),
                    tokens: parser_tokens,
                });
                parser.parse()
            };
            // 语法错误（收集于 diagnostics）可恢复，就近渲染到 stderr，不中断输出；
            for diag in &mut parse_result.diagnostics {
                if let Some(loc) = &mut diag.location {
                    loc.file = Some(file.clone());
                }
                eprintln!("{}", render_diagnostic(diag, Some(&source_code)));
            }
            eprintln!(
                "共 {} 个错误",
                lex_result.errors.len() + parse_result.diagnostics.len()
            );

            // ---------- 语义分析 ----------
            // sema 独立重走 lexer→parser 逐模块加载导入闭包；此处只渲染语义诊断
            // （词法 / 语法诊断已由上方渲染，避免重复）。
            let sema_result = novella_sema::analyze(std::path::Path::new(&file));
            let mut sema_error_count = 0usize;
            for diag in &sema_result.diagnostics.global {
                eprintln!("{}", render_diagnostic(diag, None));
                sema_error_count += 1;
            }
            for module in &sema_result.diagnostics.modules {
                // 渲染需要对应模块的源码，从磁盘重读（失败则退化为无源码渲染）
                let module_source = fs::read_to_string(&module.path).ok();
                for diag in module.semantic_diagnostics() {
                    eprintln!("{}", render_diagnostic(diag, module_source.as_deref()));
                    if diag.severity == novella_core::diagnostic::Severity::Error {
                        sema_error_count += 1;
                    }
                }
            }
            if sema_error_count > 0 {
                eprintln!("检测到 {sema_error_count} 个语义错误");
            }

            // ---------- 输出：tokens + AST + 诊断（JSON） ----------
            let sema_json: Vec<_> = sema_result
                .diagnostics
                .modules
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "path": m.path,
                        "diagnostics": m.semantic_diagnostics(),
                    })
                })
                .collect();
            let output_json = serde_json::json!({
                "tokens": lex_result.tokens,
                "program": parse_result.program,
                "diagnostics": parse_result.diagnostics,
                "sema_diagnostics": sema_json,
            });
            fs::write(&output, serde_json::to_string_pretty(&output_json).unwrap()).unwrap_or_else(
                |e| {
                    let diag = Diagnostic::error(format!("无法写入文件 '{output}'：{e}"));
                    eprintln!("{}", render_diagnostic(&diag, None));
                    exit(1);
                },
            );
        }
    }
}

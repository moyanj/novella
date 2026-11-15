use crate::error::render_diagnostic;
use clap::Parser;
use novella_core::diagnostic::{Diagnostic, Severity};
use std::{fs, process::exit};

mod cli;
mod error;

fn main() {
    let args = cli::Cli::parse();
    match args.command {
        cli::Command::Compile { file, output } => {
            println!("Compiling...");
            // ---------- 词法 / 语法 / 语义分析 ----------
            // 三个阶段统一由 `novella_sema::analyze` 编排：它内部对每个模块只做
            // 一次「读文件 → 词法 → 语法」，产物（源码、token 流、AST、诊断）
            // 经 `SemaResult.modules` 交还。CLI 只负责渲染与序列化，
            // 不得自行再跑 lexer / parser（否则入口文件会被解析两次）。
            let sema_result = novella_sema::analyze(std::path::Path::new(&file));

            let mut error_count = 0usize;

            // 无模块归属的错误（入口读取失败等）
            for diag in &sema_result.diagnostics.global {
                eprintln!("{}", render_diagnostic(diag, None));
                if diag.severity == Severity::Error {
                    error_count += 1;
                }
            }

            // 逐模块渲染词法 / 语法诊断：入口在前（下标 0），与模块编号一致
            for module in &sema_result.modules.modules {
                for diag in &module.syntax_diagnostics {
                    eprintln!("{}", render_diagnostic(diag, Some(&module.source)));
                    if diag.severity == Severity::Error {
                        error_count += 1;
                    }
                }
            }
            // 零错误时不打印汇总（`共 0 个错误` 无信息量）
            if error_count > 0 {
                eprintln!("共 {error_count} 个错误");
            }

            // 语义诊断：`diagnostics.modules` 与 `modules` 下标一一对应，
            // 渲染所需的源码直接取同一份产物，不再回盘重读。
            let mut sema_error_count = 0usize;
            for (id, module_diagnostics) in sema_result.diagnostics.modules.iter().enumerate() {
                let source = sema_result
                    .modules
                    .get(id)
                    .map(|module| module.source.as_str());
                for diag in module_diagnostics.semantic_diagnostics() {
                    eprintln!("{}", render_diagnostic(diag, source));
                    if diag.severity == Severity::Error {
                        sema_error_count += 1;
                    }
                }
            }
            if sema_error_count > 0 {
                eprintln!("检测到 {sema_error_count} 个语义错误");
            }

            // ---------- 输出：tokens + AST + 诊断（JSON） ----------
            // 入口模块按规范化路径定位（`analyze` 收到的 entry 未规范化，
            // 形如 `./a.nov` 时直接取下标 0 会混淆）。
            let entry = sema_result.modules.find_entry(std::path::Path::new(&file));
            let (tokens, program) = match entry {
                Some(module) => (
                    serde_json::to_value(&module.tokens).unwrap_or(serde_json::Value::Null),
                    serde_json::to_value(&module.program).unwrap_or(serde_json::Value::Null),
                ),
                None => (
                    serde_json::Value::Array(Vec::new()),
                    serde_json::Value::Null,
                ),
            };

            // 诊断只有一棵树：`diagnostics` 下 `global` 放无模块归属的诊断，
            // `modules` 按模块归组并按**源码顺序**合并词法 / 语法 / 语义诊断
            // （同一处只出现一次，不再分成 `diagnostics` + `sema_diagnostics` 两份）。
            let modules_json: Vec<_> = sema_result
                .diagnostics
                .modules
                .iter()
                .enumerate()
                .map(|(id, module_diagnostics)| {
                    let mut merged: Vec<&Diagnostic> = sema_result
                        .modules
                        .get(id)
                        .map(|module| module.syntax_diagnostics.iter().collect())
                        .unwrap_or_default();
                    merged.extend(module_diagnostics.semantic_diagnostics());
                    // 语法诊断与语义诊断各自有序，合并后按源码位置重排
                    merged.sort_by_key(|diag| {
                        diag.location
                            .as_ref()
                            .and_then(|loc| loc.span)
                            .map(|span| span.start)
                            .unwrap_or(0)
                    });
                    serde_json::json!({
                        "id": id,
                        "path": module_diagnostics.path,
                        "diagnostics": merged,
                    })
                })
                .collect();
            let output_json = serde_json::json!({
                "tokens": tokens,
                "program": program,
                "diagnostics": {
                    "global": sema_result.diagnostics.global,
                    "modules": modules_json,
                },
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

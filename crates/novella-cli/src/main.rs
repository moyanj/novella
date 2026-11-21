use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Parser;
use novella_core::diagnostic::{Diagnostic, DiagnosticLocation};
use novella_core::line_index::LineIndex;
use novella_hir::{HirProgram, build_hir};
use novella_sema::{AnalysisOptions, FsLoader, SemaResult, analyze_with_options};

use crate::cli::{AnalysisArgs, Emit};
use crate::error::render_diagnostic;

mod build;
mod cache;
mod cli;
mod error;
mod project;

fn main() -> ExitCode {
    match run(cli::Cli::parse().command) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!(
                "{}",
                render_diagnostic(&Diagnostic::error(error.to_string()), None)
            );
            ExitCode::FAILURE
        }
    }
}

fn run(command: cli::Command) -> Result<bool, Box<dyn std::error::Error>> {
    let (args, output) = match command {
        cli::Command::New { path } => {
            project::create(&path)?;
            return Ok(true);
        }
        cli::Command::Build {
            options,
            output,
            no_cache,
            verbose,
        } => {
            let project = project::Project::discover(&std::env::current_dir()?)?;
            let output = match output {
                Some(path) => path,
                None => project.default_output()?,
            };
            return build::run(
                AnalysisArgs {
                    file: project.entry(),
                    options,
                },
                output,
                project.root(),
                no_cache,
                verbose,
            );
        }
        cli::Command::Check(args) => (args, None),
        cli::Command::Compile {
            analysis,
            output,
            emit,
        } => (
            analysis,
            Some((
                emit,
                output.unwrap_or_else(|| PathBuf::from(emit.default_output())),
            )),
        ),
    };
    let native = output.as_ref().is_some_and(|(emit, _)| emit.is_native());
    let module_only = output
        .as_ref()
        .is_none_or(|(emit, _)| matches!(emit, Emit::Ast));
    let result = analyze(&args, module_only, native)?;
    render_diagnostics(&result);
    if result.diagnostics.has_errors() {
        return Ok(false);
    }
    let Some((emit, output)) = output else {
        return Ok(true);
    };
    if matches!(emit, Emit::Ast) {
        write_output(&output, &ast_json(&result, &args.file)?, false)?;
        return Ok(true);
    }
    let Some(hir) = lower(&result) else {
        return Ok(false);
    };
    match emit {
        Emit::Hir => {
            write_output(&output, &serde_json::to_vec_pretty(&hir)?, false)?;
        }
        Emit::Obj | Emit::Exe => {
            let object = novella_codegen::emit_object(&hir)?;
            write_output(&output, &object, matches!(emit, Emit::Exe))?;
        }
        Emit::Ast => unreachable!("AST 在 HIR 降级前处理"),
    }
    Ok(true)
}

fn render_diagnostics(result: &SemaResult) {
    for diagnostic in &result.diagnostics.global {
        eprintln!("{}", render_diagnostic(diagnostic, None));
    }
    for (id, diagnostics) in result.diagnostics.modules.iter().enumerate() {
        let source = result.modules.get(id).map(|module| module.source.as_str());
        for diagnostic in diagnostics.ordered() {
            eprintln!("{}", render_diagnostic(diagnostic, source));
        }
    }
}

fn analyze(
    args: &AnalysisArgs,
    module_only: bool,
    native: bool,
) -> Result<SemaResult, Box<dyn std::error::Error>> {
    let options = analysis_options(args, module_only, native)?;
    Ok(analyze_with_options(&mut FsLoader, &args.file, options))
}

fn analysis_options(
    args: &AnalysisArgs,
    module_only: bool,
    native: bool,
) -> Result<AnalysisOptions, Box<dyn std::error::Error>> {
    let flags = &args.options;
    let mut options = AnalysisOptions {
        std_root: flags
            .std_root
            .clone()
            .or_else(|| AnalysisOptions::default().std_root),
        module_only,
        ..Default::default()
    };
    options.cfg.features.extend(flags.features.iter().cloned());
    if native {
        options.target.pointer_width = novella_sema::PointerWidth::Bits64;
        for (flag, value, expected) in [
            ("target-os", flags.target_os.as_deref(), "linux"),
            ("target-arch", flags.target_arch.as_deref(), "x86_64"),
            ("target-family", flags.target_family.as_deref(), "unix"),
        ] {
            if let Some(value) = value.filter(|value| *value != expected) {
                return Err(format!(
                    "原生后端只支持 x86_64-unknown-linux-gnu，不能指定 --{flag}={value}"
                )
                .into());
            }
        }
        options.cfg.target_os = "linux".into();
        options.cfg.target_arch = "x86_64".into();
        options.cfg.target_family = "unix".into();
    }
    if let Some(value) = &flags.target_os {
        options.cfg.target_os.clone_from(value);
    }
    if let Some(value) = &flags.target_arch {
        options.cfg.target_arch.clone_from(value);
    }
    if let Some(value) = &flags.target_family {
        options.cfg.target_family.clone_from(value);
    }
    Ok(options)
}

fn lower(result: &SemaResult) -> Option<HirProgram> {
    match build_hir(result) {
        Ok(hir) => Some(hir),
        Err(error) => {
            let mut diagnostic = Diagnostic::error(error.to_string());
            let module = error.module.and_then(|id| result.modules.get(id));
            if let Some(module) = module {
                let (line, column) = error.span.map_or((1, 0), |span| {
                    LineIndex::build(&module.source).locate(span.start)
                });
                diagnostic = diagnostic.with_location(DiagnosticLocation {
                    file: Some(module.path.display().to_string()),
                    line,
                    column,
                    span: error.span,
                });
            }
            eprintln!(
                "{}",
                render_diagnostic(&diagnostic, module.map(|module| module.source.as_str()))
            );
            None
        }
    }
}

fn ast_json(result: &SemaResult, file: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let entry = result
        .modules
        .find_entry(file)
        .ok_or("找不到入口模块的语法产物")?;
    let modules: Vec<_> = result.diagnostics.modules.iter().enumerate().map(|(id, module)| {
        serde_json::json!({"id": id, "path": module.path, "diagnostics": module.ordered()})
    }).collect();
    let output = serde_json::json!({
        "tokens": entry.tokens,
        "program": entry.program,
        "diagnostics": {"global": result.diagnostics.global, "modules": modules},
    });
    Ok(serde_json::to_vec_pretty(&output)?)
}

fn write_output(path: &Path, bytes: &[u8], link: bool) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        // 临时目录与输出同文件系统；只在生成或链接成功后替换最终文件。
        let staging = tempfile::tempdir_in(parent)?;
        let artifact = staging.path().join("artifact");
        if link {
            let object = staging.path().join("program.o");
            fs::write(&object, bytes)?;
            let result = Command::new("clang")
                .arg(format!("--target={}", novella_codegen::TARGET_TRIPLE))
                .arg(&object)
                .arg("-o")
                .arg(&artifact)
                .output()
                .map_err(|error| format!("无法运行链接驱动 clang：{error}"))?;
            if !result.status.success() {
                return Err(format!(
                    "clang 链接失败（{}）：{}",
                    result.status,
                    String::from_utf8_lossy(&result.stderr).trim()
                )
                .into());
            }
        } else {
            fs::write(&artifact, bytes)?;
        }
        fs::rename(artifact, path)?;
        Ok(())
    })();
    result.map_err(|error| format!("无法生成 '{}'：{error}", path.display()).into())
}

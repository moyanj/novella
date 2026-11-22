//! 项目构建编排；缓存错误不改变编译结果，链接与最终输出保持原有事务语义。

use std::path::{Path, PathBuf};
use std::time::Instant;

use novella_sema::{FileLoader, analyze_with_options};

use crate::cache::{Cache, Lookup, TrackingLoader};
use crate::cli::AnalysisArgs;

pub(crate) fn run(
    args: AnalysisArgs,
    output: PathBuf,
    root: &Path,
    no_cache: bool,
    verbose: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    let start = Instant::now();
    let config = crate::config::resolve(&args)?;
    crate::link::validate_config(&config)?;
    let options = crate::analysis_options(&args, false, &config)?;
    let mut cache = if no_cache {
        if verbose {
            eprintln!("缓存已禁用（--no-cache）");
        }
        None
    } else {
        match Cache::open(root, &args.file, &options) {
            Ok(cache) => Some(cache),
            Err(error) => {
                eprintln!("缓存不可用，继续正常编译：{error}");
                None
            }
        }
    };
    if let Some(cache) = &cache {
        match cache.lookup() {
            Ok(Lookup::Hit(hit)) => {
                if verbose {
                    eprintln!("缓存命中：复用 LLVM 目标文件（校验 {:?}）", start.elapsed());
                }
                hit.render_diagnostics();
                let link_start = Instant::now();
                crate::link::write_executable(&output, &hit.artifact, &config)?;
                if verbose {
                    eprintln!(
                        "链接 {:?}；构建完成 {:?}",
                        link_start.elapsed(),
                        start.elapsed()
                    );
                }
                return Ok(true);
            }
            Ok(Lookup::Miss(reason)) => {
                if verbose {
                    eprintln!("缓存未命中：{reason}（校验 {:?}）", start.elapsed());
                }
            }
            Err(error) => eprintln!("缓存不可用，继续正常编译：{error}"),
        }
    }
    let mut loader = TrackingLoader::default();
    if cache.is_some() {
        // 项目发现已校验清单；把本次清单内容也纳入依赖，读取失败只禁用缓存发布。
        if let Err(error) = loader.read_to_string(&root.join("nov.toml")) {
            eprintln!("无法记录项目清单，继续正常编译：{error}");
            cache = None;
        }
    }
    let compile_start = Instant::now();
    let result = analyze_with_options(&mut loader, &args.file, options);
    crate::render_diagnostics(&result);
    if result.diagnostics.has_errors() {
        return Ok(false);
    }
    let Some(hir) = crate::lower(&result) else {
        return Ok(false);
    };
    let artifact = novella_codegen::emit_artifact(&hir)?;
    if verbose {
        eprintln!("分析、HIR 与 LLVM 发射 {:?}", compile_start.elapsed());
    }
    if let Some(cache) = cache {
        let store_start = Instant::now();
        match cache.store(loader, &result, &artifact) {
            Ok(()) if verbose => eprintln!("缓存已写入（{:?}）", store_start.elapsed()),
            Ok(()) => {}
            Err(error) => eprintln!("无法写入编译缓存，继续生成输出：{error}"),
        }
    }
    let link_start = Instant::now();
    crate::link::write_executable(&output, &artifact, &config)?;
    if verbose {
        eprintln!(
            "链接 {:?}；构建完成 {:?}",
            link_start.elapsed(),
            start.elapsed()
        );
    }
    Ok(true)
}

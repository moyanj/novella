//! `--print` 查询：输出内置目标清单或目标描述 JSON 后退出。
//!
//! 查询不读源码、不解析工程，供构建脚本与外部工具核对编译器支持的目标。

use crate::cli::AnalysisArgs;

pub(crate) fn run(
    kind: novella_target::PrintKind,
    args: &AnalysisArgs,
) -> Result<bool, Box<dyn std::error::Error>> {
    match kind {
        novella_target::PrintKind::TargetList => {
            for name in novella_target::builtin::list() {
                println!("{name}");
            }
        }
        novella_target::PrintKind::TargetSpecJson => {
            // 对齐 rustc：`--print target-spec-json` 必须与 `--target` 同时给出。
            let selector = args
                .options
                .target
                .clone()
                .ok_or("`--print target-spec-json` 需要同时指定 `--target`")?;
            let root = std::env::current_dir()?;
            let custom_allowed = crate::config::custom_targets_allowed(&args.options.unstable)?;
            let spec = novella_target::resolve_selector(&selector, &root, custom_allowed)?;
            print!("{}", novella_target::query::target_spec_json(&spec)?);
        }
    }
    Ok(true)
}

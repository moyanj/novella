//! 将 CLI 覆盖项与共享清单配置合并，所有命令使用同一份目标信息。
use crate::cli::AnalysisArgs;
use novella_core::project_config::{BuildConfig, ResolvedBuildConfig, load_project_config};

pub(crate) fn resolve(
    args: &AnalysisArgs,
) -> Result<ResolvedBuildConfig, Box<dyn std::error::Error>> {
    let flags = &args.options;
    let overrides = BuildConfig {
        target: flags.target,
        runtime: flags.runtime,
        code_model: flags.code_model,
        linker: flags.linker.clone(),
        entry: flags.entry.clone(),
        linker_script: flags.linker_script.clone(),
        link_object: flags.link_object.clone(),
    };
    let project = load_project_config(&args.file)?;
    let resolved = if let Some(project) = project {
        project.build.resolve(&overrides, &project.root)?
    } else {
        BuildConfig::default().resolve(&overrides, &std::env::current_dir()?)?
    };
    resolved.target.validate_cfg(
        flags.target_os.as_deref(),
        flags.target_arch.as_deref(),
        flags.target_family.as_deref(),
    )?;
    Ok(resolved)
}

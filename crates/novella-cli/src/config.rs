//! 将 CLI 覆盖项与共享清单配置合并，所有命令使用同一份目标信息。
//!
//! 目标选择的解析顺序：CLI `--target` > 清单 `build.target` > 默认内置目标；
//! 自定义目标描述必须开启 `-Z custom-targets`。

use novella_core::project_config::{BuildConfig, ResolvedBuildConfig, load_project_config};
use novella_core::target::TargetPlatform;

/// 解析后的构建配置及其来源目标描述。
pub(crate) struct Resolved {
    pub config: ResolvedBuildConfig,
    pub spec: novella_target::TargetSpec,
}

/// 解析 `-Z` 开关；目前仅支持 `custom-targets`。
pub(crate) fn custom_targets_allowed(unstable: &[String]) -> Result<bool, String> {
    let mut allowed = false;
    for flag in unstable {
        match flag.as_str() {
            "custom-targets" => allowed = true,
            other => {
                return Err(format!("未知 -Z 选项 `{other}`（当前支持 custom-targets）"));
            }
        }
    }
    Ok(allowed)
}

pub(crate) fn resolve(
    args: &crate::cli::AnalysisArgs,
) -> Result<Resolved, Box<dyn std::error::Error>> {
    let flags = &args.options;
    let overrides = BuildConfig {
        target: flags.target.clone(),
        runtime: flags.runtime,
        code_model: flags.code_model,
        opt_level: flags.opt_level,
        linker: flags.linker.clone(),
        entry: flags.entry.clone(),
        linker_script: flags.linker_script.clone(),
        link_object: flags.link_object.clone(),
    };
    let custom_allowed = custom_targets_allowed(&flags.unstable)?;
    let project = match &args.file {
        Some(file) => load_project_config(file)?,
        None => None,
    };
    let (root, manifest) = match project {
        Some(project) => (project.root, project.build),
        None => (std::env::current_dir()?, BuildConfig::default()),
    };
    // 清单目录是自定义目标描述相对路径的解析基准。
    let selector = overrides
        .target
        .clone()
        .or_else(|| manifest.target.clone())
        .unwrap_or(novella_core::project_config::TargetSelector::Builtin(
            TargetPlatform::default(),
        ));
    let spec = novella_target::resolve_selector(&selector, &root, custom_allowed)?;
    let config = novella_target::resolve_build_config(&manifest, &overrides, &root, &spec)?;
    config.target.validate_cfg(
        flags.target_os.as_deref(),
        flags.target_arch.as_deref(),
        flags.target_family.as_deref(),
    )?;
    Ok(Resolved { config, spec })
}

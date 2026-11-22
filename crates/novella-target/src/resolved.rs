//! 构建配置合并：把清单与 CLI 覆盖项合并为最终目标与链接选项。

use std::path::Path;

use novella_core::project_config::{BuildConfig, ResolvedBuildConfig};
use novella_core::target::CodeModel;

use crate::spec::TargetSpec;

/// 合并清单与 CLI 覆盖项，得到唯一的 `BuildTarget` 与链接配置。
///
/// 优先级：CLI 覆盖项 > 清单 > 目标描述的默认值。
pub fn resolve_build_config(
    manifest: &BuildConfig,
    overrides: &BuildConfig,
    manifest_root: &Path,
    spec: &TargetSpec,
) -> Result<ResolvedBuildConfig, String> {
    let mut target = spec.build_target()?;
    target.runtime = overrides
        .runtime
        .or(manifest.runtime)
        .unwrap_or(target.runtime);
    target.code_model = overrides
        .code_model
        .or(manifest.code_model)
        .unwrap_or(target.code_model);
    target.validate()?;
    if let Some(models) = &spec.code_models
        && !models.contains(&target.code_model)
    {
        return Err(format!(
            "目标 {} 不支持代码模型 {}",
            spec.triple, target.code_model
        ));
    }
    if target.code_model == CodeModel::Kernel && !spec.is_like_bare {
        return Err(format!(
            "kernel 代码模型仅适用于裸机目标，目标为 {}",
            spec.triple
        ));
    }
    manifest.resolve(overrides, manifest_root, target)
}

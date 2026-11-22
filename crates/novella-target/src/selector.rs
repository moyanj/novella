//! 目标选择解析：内置 triple 名或自定义目标描述文件。

use std::path::{Path, PathBuf};

use novella_core::project_config::TargetSelector;
use novella_core::target::TargetPlatform;

use crate::builtin;
use crate::spec::TargetSpec;

/// 目标解析失败原因；区分"未开门禁"与"描述本身无效"。
#[derive(Debug)]
pub enum ResolveError {
    /// 使用了自定义目标描述，但没有开启 `-Z custom-targets`。
    CustomTargetsDisabled(PathBuf),
    /// 其他原因（不支持的内置名、文件读取/解析失败）。
    Message(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CustomTargetsDisabled(path) => {
                write!(f, "自定义目标 '{}' 需要 -Z custom-targets", path.display())
            }
            Self::Message(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ResolveError {}

impl From<String> for ResolveError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

/// 把 CLI/清单里的目标选择解析为具体描述。
///
/// `root` 是清单所在目录或当前目录；相对路径的目标描述按此补全。
pub fn resolve_selector(
    selector: &TargetSelector,
    root: &Path,
    custom_targets_allowed: bool,
) -> Result<TargetSpec, ResolveError> {
    match selector {
        TargetSelector::Builtin(platform) => resolve_builtin(*platform),
        TargetSelector::Custom(path) => {
            if !custom_targets_allowed {
                return Err(ResolveError::CustomTargetsDisabled(path.clone()));
            }
            let path = if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            };
            crate::loader::load_file(&path).map_err(ResolveError::Message)
        }
    }
}

/// 内置目标：直接查内置集合；清单与内置集合必须一致。
pub fn resolve_builtin(platform: TargetPlatform) -> Result<TargetSpec, ResolveError> {
    builtin::lookup(platform.triple())
        .expect("内置平台枚举与内置目标清单必须一一对应")
        .map_err(ResolveError::Message)
}

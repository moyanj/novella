//! CLI 与 LSP 共用的项目构建配置；这里只解析数据，不执行工具链。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::target::{BuildTarget, CodeModel, RuntimeMode, TargetPlatform};

/// 未覆盖默认值的原始选项；保留 Option，确保目标默认值在合并之后才应用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct BuildConfig {
    pub target: Option<TargetPlatform>,
    pub runtime: Option<RuntimeMode>,
    pub code_model: Option<CodeModel>,
    pub linker: Option<PathBuf>,
    pub entry: Option<String>,
    pub linker_script: Option<PathBuf>,
    pub link_object: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ProjectBuildConfig {
    pub root: PathBuf,
    pub build: BuildConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolvedBuildConfig {
    pub target: BuildTarget,
    pub linker: Option<PathBuf>,
    pub entry: Option<String>,
    pub linker_script: Option<PathBuf>,
    pub link_object: Vec<PathBuf>,
}

impl BuildConfig {
    /// CLI 选项优先；清单文件路径相对项目根，CLI 路径保持调用方 cwd 语义。
    pub fn resolve(
        &self,
        overrides: &Self,
        manifest_root: &Path,
    ) -> Result<ResolvedBuildConfig, String> {
        let platform = overrides.target.or(self.target).unwrap_or_default();
        let mut target = platform.build_target();
        target.runtime = overrides.runtime.or(self.runtime).unwrap_or(target.runtime);
        target.code_model = overrides
            .code_model
            .or(self.code_model)
            .unwrap_or(target.code_model);
        target.validate()?;
        let linker = overrides.linker.clone().or_else(|| {
            self.linker.as_ref().map(|path| {
                // 一个裸命令名通过 PATH 查找；含目录分量的清单链接器路径相对项目根。
                if path.components().count() > 1 {
                    rooted(manifest_root, path)
                } else {
                    path.clone()
                }
            })
        });
        let linker_script = overrides.linker_script.clone().or_else(|| {
            self.linker_script
                .as_ref()
                .map(|path| rooted(manifest_root, path))
        });
        let link_object = if overrides.link_object.is_empty() {
            self.link_object
                .iter()
                .map(|path| rooted(manifest_root, path))
                .collect()
        } else {
            overrides.link_object.clone()
        };
        let entry = overrides.entry.clone().or_else(|| self.entry.clone());
        if let Some(symbol) = &entry
            && !valid_link_symbol(symbol)
        {
            return Err(format!("无效的入口符号 `{symbol}`"));
        }
        Ok(ResolvedBuildConfig {
            target,
            linker,
            entry,
            linker_script,
            link_object,
        })
    }
}

/// 导出与入口共用的保守 C/ELF 符号子集，避免接受链接器表达式或空名称。
pub fn valid_link_symbol(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && bytes.all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
}

fn rooted(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    build: BuildConfig,
}

/// 找到最近的 nov.toml；一旦存在但读取或解析失败，立即返回错误，不能退回父项目。
pub fn load_project_config(start: &Path) -> Result<Option<ProjectBuildConfig>, String> {
    let absolute = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("无法读取当前目录：{error}"))?
            .join(start)
    };
    let start = if absolute.is_file() {
        absolute.parent().unwrap_or(&absolute)
    } else {
        &absolute
    };
    for root in start.ancestors() {
        let path = root.join("nov.toml");
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let source = fs::read_to_string(&path)
                    .map_err(|error| format!("无法读取清单 '{}'：{error}", path.display()))?;
                let manifest: Manifest = toml::from_str(&source)
                    .map_err(|error| format!("无法解析清单 '{}'：{error}", path.display()))?;
                return Ok(Some(ProjectBuildConfig {
                    root: root.to_path_buf(),
                    build: manifest.build,
                }));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法检查清单 '{}'：{error}", path.display())),
        }
    }
    Ok(None)
}

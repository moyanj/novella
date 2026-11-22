//! CLI 与 LSP 共用的项目构建配置；这里只解析数据，不执行工具链。
//!
//! 目标选择可以是内置 triple 名，也可以是目标描述 JSON 的路径；两者在
//! `novella-target` 中统一解析为 `TargetSpec`，本文档只负责清单读取与原始选项。

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::target::{BuildTarget, CodeModel, RuntimeMode, TargetPlatform};

/// 目标选择：内置 triple 名或自定义目标描述的路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSelector {
    Builtin(TargetPlatform),
    /// 相对路径由解析方按项目根目录补全。
    Custom(PathBuf),
}

impl TargetSelector {
    /// 路径形态：含 `/`（含 Windows 分隔符）、以 `.json` 结尾或存在同名文件。
    pub fn is_path(value: &str) -> bool {
        value.ends_with(".json")
            || value.contains('/')
            || value.contains(std::path::MAIN_SEPARATOR)
            || value.contains('\\')
    }
}

impl FromStr for TargetSelector {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Ok(platform) = TargetPlatform::from_str(value) {
            return Ok(Self::Builtin(platform));
        }
        // 非内置名一律视为目标描述路径；由加载器报告文件是否存在。
        if Self::is_path(value) {
            return Ok(Self::Custom(PathBuf::from(value)));
        }
        Err(format!(
            "不支持目标 `{value}`；可选 x86_64-unknown-linux-gnu、x86_64-unknown-none-elf 或目标描述 JSON 路径"
        ))
    }
}

impl fmt::Display for TargetSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin(platform) => f.write_str(platform.triple()),
            Self::Custom(path) => write!(f, "{}", path.display()),
        }
    }
}

impl Serialize for TargetSelector {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TargetSelector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

/// 未覆盖默认值的原始选项；保留 Option，确保目标默认值在合并之后才应用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct BuildConfig {
    pub target: Option<TargetSelector>,
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

/// 已合并的目标与链接选项；由 `novella-target` 构造，CLI 与 LSP 共同消费。
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
    ///
    /// `target` 由调用方先经 `novella-target` 解析为具体 `BuildTarget` 传入。
    pub fn resolve(
        &self,
        overrides: &Self,
        manifest_root: &Path,
        target: BuildTarget,
    ) -> Result<ResolvedBuildConfig, String> {
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

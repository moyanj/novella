//! 编译各阶段共享的原生目标与运行时契约；不混用宿主条件与目标条件。
//!
//! 本模块只放**值类型**与旧的目标枚举；完整的目标描述（`TargetSpec`）、
//! 内置目标集合与描述加载在 `novella-target`，避免 core 反向依赖。

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

mod types;

pub use types::{EmitBackend, Endian, LinkerFlavor, RelocMode, TargetTriple};

/// 已验证的原生发射平台；未实现的 triple 必须显式拒绝。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetPlatform {
    #[default]
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    LinuxX86_64,
    #[serde(rename = "x86_64-unknown-none-elf")]
    BareMetalX86_64,
}

impl TargetPlatform {
    /// 全部内置目标，顺序即 `--print target-list` 的输出顺序。
    pub const ALL: [Self; 2] = [Self::LinuxX86_64, Self::BareMetalX86_64];

    pub const fn triple(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Self::BareMetalX86_64 => "x86_64-unknown-none-elf",
        }
    }

    /// 默认值由最终平台决定，不能先套用宿主默认值再覆盖 triple。
    pub const fn build_target(self) -> BuildTarget {
        match self {
            Self::LinuxX86_64 => BuildTarget {
                platform: self,
                runtime: RuntimeMode::Managed,
                code_model: CodeModel::Small,
            },
            Self::BareMetalX86_64 => BuildTarget {
                platform: self,
                runtime: RuntimeMode::None,
                code_model: CodeModel::Kernel,
            },
        }
    }
}

impl FromStr for TargetPlatform {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "x86_64-unknown-linux-gnu" => Ok(Self::LinuxX86_64),
            "x86_64-unknown-none-elf" => Ok(Self::BareMetalX86_64),
            _ => Err(format!(
                "不支持目标 `{value}`；可选 x86_64-unknown-linux-gnu 或 x86_64-unknown-none-elf"
            )),
        }
    }
}

impl fmt::Display for TargetPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.triple())
    }
}

/// none 关闭托管对象支持，仍允许独立的编译器内存基础函数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    None,
    #[default]
    Managed,
}

impl FromStr for RuntimeMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "managed" => Ok(Self::Managed),
            _ => Err(format!("不支持 runtime `{value}`；可选 none 或 managed")),
        }
    }
}

impl fmt::Display for RuntimeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Managed => "managed",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeModel {
    #[default]
    Small,
    Kernel,
}

impl FromStr for CodeModel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "small" => Ok(Self::Small),
            "kernel" => Ok(Self::Kernel),
            _ => Err(format!("不支持代码模型 `{value}`；可选 small 或 kernel")),
        }
    }
}

impl fmt::Display for CodeModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Small => "small",
            Self::Kernel => "kernel",
        })
    }
}

/// 解析并校验后的唯一目标配置；直接传递到 HIR、目标发射与缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildTarget {
    pub platform: TargetPlatform,
    pub runtime: RuntimeMode,
    pub code_model: CodeModel,
}

impl Default for BuildTarget {
    fn default() -> Self {
        TargetPlatform::default().build_target()
    }
}

impl BuildTarget {
    pub const fn triple(self) -> &'static str {
        self.platform.triple()
    }
    pub const fn is_bare_metal(self) -> bool {
        matches!(self.platform, TargetPlatform::BareMetalX86_64)
    }
    /// 当前目标的执行环境是否允许浮点；与是否链接托管运行时无关。
    pub const fn has_float(self) -> bool {
        match self.platform {
            TargetPlatform::LinuxX86_64 => true,
            TargetPlatform::BareMetalX86_64 => false,
        }
    }
    pub const fn bare_metal() -> Self {
        TargetPlatform::BareMetalX86_64.build_target()
    }
    pub const fn os(self) -> &'static str {
        if self.is_bare_metal() {
            "none"
        } else {
            "linux"
        }
    }
    pub const fn arch(self) -> &'static str {
        "x86_64"
    }
    pub const fn family(self) -> &'static str {
        if self.is_bare_metal() { "none" } else { "unix" }
    }

    pub fn validate(self) -> Result<(), String> {
        if !self.is_bare_metal() && self.code_model == CodeModel::Kernel {
            return Err("kernel 代码模型仅适用于 x86_64-unknown-none-elf".into());
        }
        Ok(())
    }

    /// 条件编译不能伪装成另一种实际发射目标。
    pub fn validate_cfg(
        self,
        os: Option<&str>,
        arch: Option<&str>,
        family: Option<&str>,
    ) -> Result<(), String> {
        self.validate()?;
        for (name, actual, expected) in [
            ("target-os", os, self.os()),
            ("target-arch", arch, self.arch()),
            ("target-family", family, self.family()),
        ] {
            if let Some(actual) = actual.filter(|actual| *actual != expected) {
                return Err(format!(
                    "--{name}={actual} 与目标 {} 不一致，目标值为 {expected}",
                    self.triple()
                ));
            }
        }
        Ok(())
    }
}

/// 对象文件需要的语言运行时；与对象字节一起缓存，命中后不重新猜测。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRequirements {
    pub managed: bool,
}

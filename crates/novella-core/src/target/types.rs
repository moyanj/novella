//! 目标描述的**值类型**：三元组、端序、重定位模式、链接器风格与发射后端。
//!
//! 这些类型被子系统（CLI 解析、清单 serde、codegen inkwell 映射）共同使用，
//! 因此放在 core；完整的 `TargetSpec` 描述与加载逻辑在 `novella-target`。

use std::{fmt, str::FromStr, sync::Arc};

/// 目标三元组；不可发射的 triple 也可以被描述，由发射后端决定是否支持。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetTriple(Arc<str>);

impl TargetTriple {
    /// 校验 `arch-vendor-os` 或 `arch-vendor-os-env` 形式；四段以上保留末段为 env。
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value: String = value.into();
        let segments: Vec<_> = value.split('-').collect();
        if segments.len() < 3 || segments.iter().any(|segment| segment.is_empty()) {
            return Err(format!(
                "目标三元组 `{value}` 格式无效；应为 `arch-vendor-os[-env]`"
            ));
        }
        Ok(Self(Arc::from(value)))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// 拆分为 `(arch, vendor, os, env)`；`env` 为 `-` 之后的剩余段，允许缺省。
    pub fn segments(&self) -> (&str, &str, &str, Option<&str>) {
        let mut parts = self.0.splitn(4, '-');
        let arch = parts.next().unwrap_or_default();
        let vendor = parts.next().unwrap_or_default();
        let os = parts.next().unwrap_or_default();
        (arch, vendor, os, parts.next())
    }
}

impl FromStr for TargetTriple {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for TargetTriple {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for TargetTriple {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// 目标字节序；`data-layout` 首字符必须与之一致。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Endian {
    #[default]
    Little,
    Big,
}

impl Endian {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Little => "little",
            Self::Big => "big",
        }
    }
}

impl FromStr for Endian {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "little" => Ok(Self::Little),
            "big" => Ok(Self::Big),
            _ => Err(format!("不支持字节序 `{value}`；可选 little 或 big")),
        }
    }
}

impl fmt::Display for Endian {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 代码生成的重定位模式；裸机静态、托管平台 PIC。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelocMode {
    #[default]
    Pic,
    Static,
}

impl FromStr for RelocMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pic" => Ok(Self::Pic),
            "static" => Ok(Self::Static),
            _ => Err(format!("不支持重定位模式 `{value}`；可选 pic 或 static")),
        }
    }
}

impl fmt::Display for RelocMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pic => "pic",
            Self::Static => "static",
        })
    }
}

/// 链接驱动风格；决定命令行参数形态，与具体可执行文件名无关。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkerFlavor {
    /// `clang --target=<triple>`：托管平台，依赖宿主 CRT 与 libc。
    #[default]
    Clang,
    /// `ld.lld`：裸机静态链接，不引入 CRT。
    Lld,
    /// `gcc`：显式使用 GNU 驱动，行为同 clang 风格。
    Gnu,
}

impl FromStr for LinkerFlavor {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "clang" => Ok(Self::Clang),
            "lld" => Ok(Self::Lld),
            "gnu" => Ok(Self::Gnu),
            _ => Err(format!(
                "不支持链接器风格 `{value}`；可选 clang、lld 或 gnu"
            )),
        }
    }
}

impl fmt::Display for LinkerFlavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Clang => "clang",
            Self::Lld => "lld",
            Self::Gnu => "gnu",
        })
    }
}

/// 已实现的目标发射后端；`None` 表示目标只能被描述与查询，不能生成原生代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmitBackend {
    #[serde(rename = "x86_64-linux")]
    X86_64Linux,
    #[serde(rename = "x86_64-bare")]
    X86_64Bare,
}

impl EmitBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-linux",
            Self::X86_64Bare => "x86_64-bare",
        }
    }
}

impl FromStr for EmitBackend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "x86_64-linux" => Ok(Self::X86_64Linux),
            "x86_64-bare" => Ok(Self::X86_64Bare),
            _ => Err(format!(
                "不支持发射后端 `{value}`；可选 x86_64-linux 或 x86_64-bare"
            )),
        }
    }
}

impl fmt::Display for EmitBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

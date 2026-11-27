//! 标准库分层清单：`std/modules.toml` → 每个模块的 tier 与目标能力要求。
//!
//! `@no_std` 只允许导入 core / alloc 层模块。清单属于工具链数据，由编译器读取，
//! 不是语言语法的一部分；未列出的模块按 `std` 层处理（保守拒绝）。

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::FileLoader;

/// 标准库模块层级；声明顺序即允许范围（core ⊂ alloc ⊂ std）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Tier {
    Core,
    Alloc,
    Std,
}

impl Tier {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Alloc => "alloc",
            Self::Std => "std",
        }
    }
}

/// 单个标准库模块的元数据。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ModuleInfo {
    tier: Tier,
    #[serde(default, rename = "requires-float")]
    requires_float: bool,
}

impl ModuleInfo {
    pub(crate) fn tier(&self) -> Tier {
        self.tier
    }

    pub(crate) fn requires_float(&self) -> bool {
        self.requires_float
    }
}

/// `std/modules.toml` 的解析结果；缺失清单时为空表。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct StdManifest {
    #[serde(default)]
    module: BTreeMap<String, ModuleInfo>,
    /// 清单文件是否真实存在；缺失时 `@no_std` 的导入诊断要指出这一点。
    #[serde(skip)]
    present: bool,
}

impl StdManifest {
    /// 读取 `std_root/modules.toml`；返回清单与需要上报的全局诊断消息。
    ///
    /// 清单缺失不算错误（只在 `@no_std` 需要判定层级时由调用方报错），
    /// 但存在却无法读取或解析时立即返回错误消息，不做静默降级。
    pub(crate) fn load(loader: &mut dyn FileLoader, std_root: &Path) -> (Self, Vec<String>) {
        let path = std_root.join("modules.toml");
        if !loader.exists(&path) {
            return (Self::default(), Vec::new());
        }
        let source = match loader.read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                return (
                    Self::default(),
                    vec![format!("无法读取标准库清单 '{}'：{error}", path.display())],
                );
            }
        };
        let mut manifest: Self = match toml::from_str(&source) {
            Ok(manifest) => manifest,
            Err(error) => {
                return (
                    Self::default(),
                    vec![format!("标准库清单 '{}' 解析失败：{error}", path.display())],
                );
            }
        };
        manifest.present = true;
        let mut errors = Vec::new();
        for name in manifest.module.keys() {
            let file = std_root.join(format!("{name}.nov"));
            let nested = std_root.join(name).join("lib.nov");
            if !loader.exists(&file) && !loader.exists(&nested) {
                errors.push(format!(
                    "标准库清单声明了不存在的模块 `{name}`（缺少 {}）",
                    file.display()
                ));
            }
        }
        (manifest, errors)
    }

    /// 按导入路径（`std:` 之后的部分）查询模块元数据。
    pub(crate) fn module(&self, path: &str) -> Option<&ModuleInfo> {
        self.module.get(path)
    }

    /// 清单文件是否存在（解析失败时按缺失处理，错误已另行上报）。
    pub(crate) fn present(&self) -> bool {
        self.present
    }
}

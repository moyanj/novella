//! 目标描述：与 rustc `TargetOptions` 同构的结构化描述。
//!
//! 描述文件**不允许**出现未知字段时被静默丢弃的陷阱以外的强约束：
//! 未知字段被忽略（schema 演进），缺失字段由 `builtin/base.json` 模板补齐。

use novella_core::target::{
    BuildTarget, CodeModel, EmitBackend, Endian, LinkerFlavor, RelocMode, RuntimeMode,
    TargetPlatform, TargetTriple,
};
use serde::{Deserialize, Serialize};

/// 运行时归档族；由目标描述派生，链接层据此选择 `libnovella_*.a`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKey {
    /// 托管平台（Linux），使用宿主 libc 与 CRT。
    Linux,
    /// 裸机 small 代码模型。
    BareSmall,
    /// 裸机 kernel 代码模型。
    BareKernel,
}

/// 一个目标的完整描述；`emit = None` 表示只能被描述、不能生成原生代码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TargetSpec {
    pub triple: TargetTriple,
    pub arch: String,
    #[serde(default)]
    pub vendor: Option<String>,
    pub os: String,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub abi: Option<String>,
    pub data_layout: String,
    pub pointer_width: u8,
    #[serde(default)]
    pub endian: Endian,
    #[serde(default)]
    pub target_family: Vec<String>,
    #[serde(default)]
    pub llvm_target: Option<String>,
    pub cpu: String,
    #[serde(default)]
    pub target_features: Vec<String>,
    #[serde(default)]
    pub reloc_mode: RelocMode,
    #[serde(default)]
    pub code_models: Option<Vec<CodeModel>>,
    #[serde(default)]
    pub runtime: Option<RuntimeMode>,
    #[serde(default)]
    pub default_code_model: Option<CodeModel>,
    pub has_float: bool,
    pub requires_no_std: bool,
    #[serde(default)]
    pub emit: Option<EmitBackend>,
    #[serde(default)]
    pub linker: Option<String>,
    #[serde(default)]
    pub linker_flavor: LinkerFlavor,
    #[serde(default)]
    pub linker_args: Vec<String>,
    #[serde(default)]
    pub is_like_linux: bool,
    #[serde(default)]
    pub is_like_bare: bool,
    #[serde(default)]
    pub is_like_msvc: bool,
}

impl TargetSpec {
    /// 校验描述内部一致性；任何下游（sema / HIR / codegen / 链接）都只调用这里。
    pub fn validate(&self) -> Result<(), String> {
        let first = self.data_layout.chars().next();
        let expected = match self.endian {
            Endian::Little => Some('e'),
            Endian::Big => Some('E'),
        };
        if first != expected {
            return Err(format!(
                "目标 {} 的 data-layout 必须以 `{}` 开头（字节序 {}）",
                self.triple,
                expected.unwrap_or('e'),
                self.endian
            ));
        }
        if !matches!(self.pointer_width, 32 | 64) {
            return Err(format!(
                "目标 {} 的 pointer-width 必须为 32 或 64，实际为 {}",
                self.triple, self.pointer_width
            ));
        }
        if self.arch.is_empty() || self.os.is_empty() {
            return Err(format!("目标 {} 缺少 arch 或 os", self.triple));
        }
        if let Some(models) = &self.code_models {
            if models.is_empty() {
                return Err(format!("目标 {} 的 code-models 不能为空", self.triple));
            }
            if let Some(default) = self.default_code_model
                && !models.contains(&default)
            {
                return Err(format!(
                    "目标 {} 的默认代码模型 {default} 不在 code-models 中",
                    self.triple
                ));
            }
        }
        if let Some(default) = self.default_code_model
            && default == CodeModel::Kernel
            && !self.is_like_bare
        {
            // kernel 代码模型要求不假设小地址空间；裸机是唯一已支持场景。
            return Err(format!(
                "目标 {} 不是裸机目标，不能使用 kernel 代码模型",
                self.triple
            ));
        }
        match self.emit {
            None if self.linker.is_some() || !self.linker_args.is_empty() => Err(format!(
                "目标 {} 不可发射，不应声明 linker 或 linker-args",
                self.triple
            )),
            None => Ok(()),
            Some(EmitBackend::X86_64Linux) => {
                if self.requires_no_std {
                    return Err(format!(
                        "目标 {} 使用 x86_64-linux 后端，不能要求 @no_std",
                        self.triple
                    ));
                }
                if !self.has_float {
                    return Err(format!(
                        "目标 {} 使用 x86_64-linux 后端，必须支持浮点",
                        self.triple
                    ));
                }
                Ok(())
            }
            Some(EmitBackend::X86_64Bare) => {
                if self.has_float {
                    return Err(format!(
                        "目标 {} 使用 x86_64-bare 后端，暂不支持浮点",
                        self.triple
                    ));
                }
                Ok(())
            }
        }
    }

    /// 该描述映射到的原生平台；不可发射的目标返回错误，调用方不得猜测。
    pub fn platform(&self) -> Result<TargetPlatform, String> {
        match self.emit {
            Some(EmitBackend::X86_64Linux) => Ok(TargetPlatform::LinuxX86_64),
            Some(EmitBackend::X86_64Bare) => Ok(TargetPlatform::BareMetalX86_64),
            None => Err(format!("目标 {} 当前不可发射", self.triple)),
        }
    }

    /// 派生唯一的运行时目标；`runtime` / `default-code-model` 缺省时用平台默认值。
    pub fn build_target(&self) -> Result<BuildTarget, String> {
        let platform = self.platform()?;
        let mut target = platform.build_target();
        if let Some(runtime) = self.runtime {
            target.runtime = runtime;
        }
        if let Some(code_model) = self.default_code_model {
            target.code_model = code_model;
        }
        target.validate()?;
        Ok(target)
    }

    /// 按 (平台, 代码模型) 选择 runtime 归档族。
    pub fn archive_key(&self, target: BuildTarget) -> Result<ArchiveKey, String> {
        match self.platform()? {
            TargetPlatform::LinuxX86_64 => Ok(ArchiveKey::Linux),
            TargetPlatform::BareMetalX86_64 => Ok(match target.code_model {
                CodeModel::Small => ArchiveKey::BareSmall,
                CodeModel::Kernel => ArchiveKey::BareKernel,
            }),
        }
    }

    /// 目标描述文件的默认链接器命令名。
    pub fn default_linker(&self) -> &'static str {
        match self.linker_flavor {
            LinkerFlavor::Lld => "ld.lld",
            LinkerFlavor::Clang => "clang",
            LinkerFlavor::Gnu => "gcc",
        }
    }

    /// 传给 LLVM 的目标特性串；空列表返回 `None`（使用 LLVM 默认）。
    pub fn llvm_features(&self) -> Option<String> {
        if self.target_features.is_empty() {
            None
        } else {
            Some(self.target_features.join(","))
        }
    }
}

#[cfg(test)]
mod tests {
    /// 内置目标必须能通过自身校验，且模板合并保留 base 中的默认值。
    #[test]
    fn 内置目标通过校验() {
        for name in crate::builtin::list() {
            let spec = crate::builtin::lookup(name)
                .unwrap_or_else(|| panic!("缺少内置目标 {name}"))
                .unwrap_or_else(|error| panic!("内置目标 {name} 无效：{error}"));
            assert_eq!(spec.triple.as_str(), *name);
            assert!(!spec.target_family.is_empty());
            assert!(spec.emit.is_some());
        }
    }

    /// `--print target-spec-json` 的结果必须可被 `--target <file>` 原样回读。
    #[test]
    fn 目标描述_json_往返一致() {
        for name in crate::builtin::list() {
            let spec = crate::builtin::lookup(name).unwrap().unwrap();
            let json = serde_json::to_string(&spec).unwrap();
            let loaded = crate::loader::load_str(&json).unwrap();
            assert_eq!(loaded, spec);
        }
    }

    /// 数据布局的字节序前缀与 `endian` 必须一致。
    #[test]
    fn 字节序与数据布局冲突被拒绝() {
        let mut spec = crate::builtin::lookup("x86_64-unknown-linux-gnu")
            .unwrap()
            .unwrap();
        spec.data_layout = "E-m:e-i64:64-n8:16:32:64-S128".into();
        assert!(spec.validate().is_err());
    }

    /// 不可发射的目标描述不得声明链接参数。
    ///
    /// 内置 Linux 目标的 `linker-args` 为空，因此用例需自行注入一条链接参数，
    /// 不能依赖内置描述携带 `-lm` 之类的固定参数。
    #[test]
    fn 无发射后端时禁止链接参数() {
        let mut spec = crate::builtin::lookup("x86_64-unknown-linux-gnu")
            .unwrap()
            .unwrap();
        spec.linker_args = vec!["-lm".into()];
        spec.emit = None;
        assert!(spec.validate().is_err());
        spec.linker_args.clear();
        spec.linker = None;
        assert!(spec.validate().is_ok());
        assert!(spec.platform().is_err());
    }
}

//! 目标描述查询：`--print target-list` 与 `--print target-spec-json`。

use crate::spec::TargetSpec;

/// 支持 `--print` 的查询种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintKind {
    TargetList,
    TargetSpecJson,
}

impl PrintKind {
    pub const ALL: [Self; 2] = [Self::TargetList, Self::TargetSpecJson];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TargetList => "target-list",
            Self::TargetSpecJson => "target-spec-json",
        }
    }
}

impl std::str::FromStr for PrintKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "target-list" => Ok(Self::TargetList),
            "target-spec-json" => Ok(Self::TargetSpecJson),
            _ => Err(format!(
                "未知的 --print 查询 `{value}`；可选 target-list 或 target-spec-json"
            )),
        }
    }
}

impl std::fmt::Display for PrintKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 目标描述的 JSON；输出可被 `--target <file.json>` 原样回读。
pub fn target_spec_json(spec: &TargetSpec) -> Result<String, String> {
    serde_json::to_string_pretty(spec).map_err(|error| format!("无法序列化目标描述：{error}"))
}

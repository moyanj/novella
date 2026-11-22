//! 目标描述文件的读取与解析。

use std::path::Path;

use crate::spec::TargetSpec;

/// 读取并解析一个目标描述 JSON；缺失字段由内置模板补齐。
pub fn load_file(path: &Path) -> Result<TargetSpec, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("无法读取目标描述 '{}'：{error}", path.display()))?;
    load_str(&source).map_err(|error| format!("目标描述 '{}'：{error}", path.display()))
}

/// 解析目标描述文本；用于测试与 `--print` 的 round-trip 校验。
pub fn load_str(source: &str) -> Result<TargetSpec, String> {
    crate::builtin::merge_source(source)
}

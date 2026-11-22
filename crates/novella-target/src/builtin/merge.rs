//! 模板合并：目标 JSON 覆盖 `base.json` 的默认值，未知字段忽略。
//!
//! 合并策略是"逐键覆盖"而非 serde 默认值：模板提供全部键，覆盖层只写差异，
//! 因此自定义目标可以只写 `triple` 与少量必要字段。

use serde_json::{Map, Value};

use crate::spec::TargetSpec;

/// 解析内置/自定义目标描述。
///
/// 调用方传入的 `source` 必须包含 `triple`（模板无法提供默认 triple）。
pub(crate) fn load(source: &str) -> Result<TargetSpec, String> {
    super::merge::merge(super::base(), source)
}

/// 与 [`load`] 相同，但把错误包装成带来源说明的文案。
pub(crate) fn merge(base: &str, over: &str) -> Result<TargetSpec, String> {
    let mut merged = parse_object(base, "默认模板")?;
    let overlay = parse_object(over, "目标描述")?;
    for (key, value) in overlay {
        // null 在覆盖层中表示"不改变模板值"，避免必须重复全部键。
        if value.is_null() {
            continue;
        }
        merged.insert(key, value);
    }
    let merged = Value::Object(merged);
    let spec: TargetSpec =
        serde_json::from_value(merged).map_err(|error| format!("无法解析目标描述：{error}"))?;
    spec.validate()?;
    Ok(spec)
}

fn parse_object(source: &str, label: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(source)
        .map_err(|error| format!("{label} 不是合法 JSON：{error}"))?
    {
        Value::Object(map) => Ok(map),
        _ => Err(format!("{label} 必须是 JSON 对象")),
    }
}

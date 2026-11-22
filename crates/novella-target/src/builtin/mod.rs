//! 内置目标集合：`include_str!` 内嵌 JSON，与自定义目标共用同一条解析路径。

mod merge;

use crate::spec::TargetSpec;

/// 内置目标的全部 triple，顺序即 `--print target-list` 的输出顺序。
pub fn list() -> &'static [&'static str] {
    &["x86_64-unknown-linux-gnu", "x86_64-unknown-none-elf"]
}

/// 目标描述的默认值模板；所有内置与自定义目标都以此为基线。
pub fn base() -> &'static str {
    include_str!("base.json")
}

/// 按 triple 名查找内置目标；未命中返回 `None`，由调用方决定降级为路径还是报错。
pub fn lookup(name: &str) -> Option<Result<TargetSpec, String>> {
    let source = match name {
        "x86_64-unknown-linux-gnu" => {
            include_str!("x86_64-unknown-linux-gnu.json")
        }
        "x86_64-unknown-none-elf" => include_str!("x86_64-unknown-none-elf.json"),
        _ => return None,
    };
    Some(merge::load(source).map_err(|error| format!("内置目标 {name} 的描述无效：{error}")))
}

/// 全部内置目标（解析并校验后）；任一内置目标失效即视为编译器缺陷。
pub fn all() -> Result<Vec<TargetSpec>, String> {
    list()
        .iter()
        .map(|name| {
            lookup(name)
                .expect("内置目标清单与 JSON 文件必须一一对应")
                .and_then(|spec| {
                    spec.validate()?;
                    Ok(spec)
                })
        })
        .collect()
}

/// 解析任意目标描述文本（内置与自定义共用模板合并逻辑）。
pub(crate) fn merge_source(source: &str) -> Result<TargetSpec, String> {
    merge::merge(base(), source)
}

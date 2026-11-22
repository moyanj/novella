//! feature 体系：`--feature` 的展开（含 `implies`）与合法性校验。
//!
//! novella 的 feature 与 rustc 的 `cfg(feature)` 同义：`--feature a` 等价于
//! `--cfg 'feature="a"'`，但额外支持清单中声明的隐含关系。

use std::collections::{BTreeMap, BTreeSet};

/// 一个项目的 feature 声明与其启用状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureSet {
    /// 启用后自动启用的其他 feature（清单 `[features] a = ["b"]`）。
    pub implies: BTreeMap<String, Vec<String>>,
    /// 启用后额外启用的**其他 crate** 同名 feature 的引用（`"dep/feature"`）。
    pub cross_crate: BTreeMap<String, Vec<String>>,
}

impl FeatureSet {
    /// 解析 `[features]` 表；键为 feature 名，值为隐含列表。
    ///
    /// 允许的值形态：
    /// - `"b"`：同一 crate 内隐含 `b`；
    /// - `"dep:b"`：隐含另一个 crate 的 `b`，本阶段只记录不参与 cfg；
    /// - `"-b"`：负 feature，直接拒绝。
    pub fn from_table<I, J>(entries: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = (String, J)>,
        J: IntoIterator<Item = String>,
    {
        let mut set = Self::default();
        for (name, values) in entries {
            validate_name(&name)?;
            for value in values {
                if let Some(target) = value.strip_prefix("dep:") {
                    validate_name(target)?;
                    set.cross_crate
                        .entry(name.clone())
                        .or_default()
                        .push(target.to_owned());
                } else {
                    validate_name(&value)?;
                    set.implies.entry(name.clone()).or_default().push(value);
                }
            }
        }
        Ok(set)
    }

    /// 展开命令行请求的 feature 集合；重复请求幂等，未知 feature 视为显式定义。
    pub fn resolve(&self, requested: &[String]) -> Result<BTreeSet<String>, String> {
        let mut enabled = BTreeSet::new();
        let mut pending: Vec<String> = Vec::new();
        for name in requested {
            validate_name(name)?;
            if enabled.insert(name.clone()) {
                pending.push(name.clone());
            }
        }
        while let Some(name) = pending.pop() {
            if let Some(implied) = self.implies.get(&name) {
                for next in implied {
                    if enabled.insert(next.clone()) {
                        pending.push(next.clone());
                    }
                }
            }
        }
        Ok(enabled)
    }

    /// 所有在清单中声明过隐含关系的 feature；供 `--check-cfg` 生成白名单。
    pub fn declared(&self) -> BTreeSet<String> {
        self.implies
            .keys()
            .chain(self.cross_crate.keys())
            .cloned()
            .collect()
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("feature 名不能为空".into());
    }
    if name.starts_with('-') {
        return Err(format!("不支持的负 feature `{name}`"));
    }
    if name.contains('/') || name.contains(':') {
        return Err(format!("feature 名 `{name}` 不能包含 `/` 或 `:`"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `implies` 传递展开且幂等；重复解析不改变结果。
    #[test]
    fn feature_传递展开() {
        let set = FeatureSet {
            implies: BTreeMap::from([
                ("full".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
                ("a".to_owned(), vec!["c".to_owned()]),
            ]),
            cross_crate: BTreeMap::new(),
        };
        let resolved = set.resolve(&["full".to_owned()]).unwrap();
        assert_eq!(
            resolved,
            BTreeSet::from([
                "full".to_owned(),
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned()
            ])
        );
        assert_eq!(set.resolve(&["full".to_owned()]).unwrap(), resolved);
    }

    /// 负 feature 与含分隔符的名字必须在解析期拒绝，而不是静默忽略。
    #[test]
    fn feature_名字校验() {
        let set = FeatureSet::default();
        assert_eq!(
            set.resolve(&["-default".to_owned()]).unwrap_err(),
            "不支持的负 feature `-default`"
        );
        assert_eq!(
            set.resolve(&["a/b".to_owned()]).unwrap_err(),
            "feature 名 `a/b` 不能包含 `/` 或 `:`"
        );
    }
}

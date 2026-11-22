//! 条件编译配置：`CfgOptions` 数据模型、由目标描述派生、`--cfg` 解析与求值器。
//!
//! 模型与 rustc 的 `Cfg` 同构：键值谓词可以有多个值（`target_feature`、重复的
//! `--cfg 'k="v"'`），裸 flag 单独成集合；`features` 保存已展开的 feature 名。
//! `--cfg` 的显式注入存放于 `extra`，求值时与目标派生值取并集。

use std::collections::{BTreeMap, BTreeSet};

use novella_core::Span;
use novella_core::token::{Token, TokenKind};

use crate::spec::TargetSpec;

/// `--check-cfg` 白名单；`None` 表示不检查（默认，与历史行为一致）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckCfg {
    /// 允许出现的 cfg 键（含裸 flag 名）。
    pub keys: BTreeSet<String>,
    /// 键允许的值集合；未列出的键不做值检查。
    pub values: BTreeMap<String, BTreeSet<String>>,
}

impl CheckCfg {
    /// 合并一条 `--check-cfg` 规格：`cfg(k1,k2)` 或 `values(k,"v1","v2")`。
    pub fn extend_from_spec(&mut self, spec: &str) -> Result<(), String> {
        let spec = spec.trim();
        let (is_values, inner) = if let Some(inner) = spec
            .strip_prefix("cfg(")
            .and_then(|inner| inner.strip_suffix(')'))
        {
            (false, inner)
        } else if let Some(inner) = spec
            .strip_prefix("values(")
            .and_then(|inner| inner.strip_suffix(')'))
        {
            (true, inner)
        } else {
            return Err(format!(
                "--check-cfg 只接受 `cfg(键,...)` 或 `values(键,\"值\",...)`，收到 `{spec}`"
            ));
        };
        let mut parts = split_top_level(inner)?.into_iter();
        let key = parts
            .next()
            .filter(|key| !key.is_empty())
            .ok_or_else(|| format!("--check-cfg 缺少键名：`{spec}`"))?;
        if is_values {
            let values = self.values.entry(key.clone()).or_default();
            for value in parts {
                values.insert(unquote(&value)?);
            }
            // values(...) 同时声明键合法，避免必须再写一条 cfg()。
            self.keys.insert(key);
        } else {
            self.keys.insert(key);
            for extra in parts {
                if !extra.is_empty() {
                    self.keys.insert(extra);
                }
            }
        }
        Ok(())
    }

    /// 没有任何声明时视为未开启。
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.values.is_empty()
    }
}

/// 条件编译配置；默认宿主平台，交叉分析时由调用方显式提供。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgOptions {
    /// 键值谓词：键 → 允许值集合（多值）。
    pub values: BTreeMap<String, BTreeSet<String>>,
    /// 裸 flag 谓词，如 `unix`、`test`、`target_has_float`。
    pub flags: BTreeSet<String>,
    /// 已展开的 feature 名。
    pub features: BTreeSet<String>,
    /// `--cfg` 显式注入项；求值时与上面三组取并集。
    pub extra: CfgExtra,
    /// `--check-cfg` 白名单；`None` 表示不检查。
    pub check: Option<CheckCfg>,
}

/// 命令行显式注入的 cfg 项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CfgExtra {
    pub values: BTreeMap<String, BTreeSet<String>>,
    pub flags: BTreeSet<String>,
}

impl Default for CfgOptions {
    /// 宿主默认值：与 `BuildTarget::default()`（Linux x86-64）一致，
    /// 保证未经目标解析的调用方仍得到可用的谓词集合。
    fn default() -> Self {
        Self::host()
    }
}

impl CfgOptions {
    /// 空集合；所有谓词均为 false，用于自描述与测试。
    pub fn empty() -> Self {
        Self {
            values: BTreeMap::new(),
            flags: BTreeSet::new(),
            features: BTreeSet::new(),
            extra: CfgExtra::default(),
            check: None,
        }
    }

    /// 宿主平台的默认集合：与 `std::env::consts` 对应的谓词子集。
    pub fn host() -> Self {
        let mut options = Self::empty();
        options.insert_value("target_os", std::env::consts::OS);
        options.insert_value("target_arch", std::env::consts::ARCH);
        options.insert_value("target_family", std::env::consts::FAMILY);
        options.insert_value(
            "target_endian",
            if cfg!(target_endian = "big") {
                "big"
            } else {
                "little"
            },
        );
        options.insert_value(
            "target_pointer_width",
            if usize::BITS == 32 { "32" } else { "64" },
        );
        match std::env::consts::FAMILY {
            "unix" => {
                options.flags.insert("unix".to_owned());
            }
            "windows" => {
                options.flags.insert("windows".to_owned());
            }
            _ => {}
        }
        options
    }

    /// 由目标描述派生 `target_*` 谓词。
    pub fn from_spec(spec: &TargetSpec) -> Self {
        let mut options = Self::empty();
        options.insert_value("target_os", &spec.os);
        options.insert_value("target_arch", &spec.arch);
        for family in &spec.target_family {
            options.insert_value("target_family", family);
        }
        if let Some(vendor) = &spec.vendor {
            options.insert_value("target_vendor", vendor);
        }
        // env 缺省时 rustc 用空串表示；保留该语义以便 `@cfg(target_env = "")` 成立。
        options.insert_value("target_env", spec.env.as_deref().unwrap_or(""));
        if let Some(abi) = &spec.abi {
            options.insert_value("target_abi", abi);
        }
        options.insert_value("target_endian", spec.endian.as_str());
        options.insert_value("target_pointer_width", &spec.pointer_width.to_string());
        for feature in &spec.target_features {
            // LLVM 特性串可能带 `+`/`-` 前缀；cfg 谓词只暴露启用项。
            if let Some(name) = feature.strip_prefix('+') {
                options.insert_value("target_feature", name);
            } else if !feature.starts_with('-') {
                options.insert_value("target_feature", feature);
            }
        }
        if spec.has_float {
            options.flags.insert("target_has_float".to_owned());
        }
        if spec.target_family.iter().any(|family| family == "unix") {
            options.flags.insert("unix".to_owned());
        }
        if spec.target_family.iter().any(|family| family == "windows") {
            options.flags.insert("windows".to_owned());
        }
        options
    }

    fn insert_value(&mut self, key: &str, value: &str) {
        self.values
            .entry(key.to_owned())
            .or_default()
            .insert(value.to_owned());
    }

    /// 写入已展开的 feature 集合。
    pub fn set_features(&mut self, features: BTreeSet<String>) {
        self.features = features;
    }

    /// 解析一条 `--cfg` 规格：`key` 或 `key="value"`。
    pub fn extend_from_cfg_spec(&mut self, spec: &str) -> Result<(), String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("--cfg 值不能为空".into());
        }
        match spec.split_once('=') {
            Some((key, value)) => {
                let key = key.trim();
                if key.is_empty() {
                    return Err(format!("--cfg 缺少键名：`{spec}`"));
                }
                self.extra
                    .values
                    .entry(key.to_owned())
                    .or_default()
                    .insert(unquote(value.trim())?);
            }
            None => {
                self.extra.flags.insert(spec.to_owned());
            }
        }
        Ok(())
    }

    /// 目标派生值与 `--cfg` 注入值的合并视图：键值取并集，flag 取并集。
    pub fn merged(&self) -> (BTreeMap<String, BTreeSet<String>>, BTreeSet<String>) {
        let mut values = self.values.clone();
        for (key, extra) in &self.extra.values {
            values
                .entry(key.clone())
                .or_default()
                .extend(extra.iter().cloned());
        }
        let mut flags = self.flags.clone();
        flags.extend(self.extra.flags.iter().cloned());
        (values, flags)
    }

    /// 键的全部可用值（含 `--cfg` 注入值）。
    fn key_values(&self, key: &str) -> Option<BTreeSet<String>> {
        let mut values = self.values.get(key).cloned();
        if let Some(extra) = self.extra.values.get(key) {
            values
                .get_or_insert_with(BTreeSet::new)
                .extend(extra.iter().cloned());
        }
        values
    }

    fn has_flag(&self, name: &str) -> bool {
        self.flags.contains(name) || self.extra.flags.contains(name)
    }

    /// 求值一个 `@cfg(...)` 谓词 token 序列。
    pub fn evaluate(&self, tokens: &[Token]) -> Result<bool, String> {
        let mut parser = Predicate {
            tokens,
            index: 0,
            cfg: self,
        };
        let result = parser.parse()?;
        if parser.index == parser.tokens.len() {
            Ok(result)
        } else {
            Err("cfg 条件后有多余 token".to_owned())
        }
    }

    /// 在求值前执行 `--check-cfg` 校验；未开启检查时直接放行。
    ///
    /// 返回被检查出的全部问题，调用方负责聚合为诊断。
    pub fn check(&self, tokens: &[Token]) -> Vec<String> {
        let Some(check) = &self.check else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        let mut index = 0;
        while index < tokens.len() {
            let token = &tokens[index];
            index += 1;
            let name = token.slice.as_str();
            if token.kind != TokenKind::Identifier {
                continue;
            }
            match tokens.get(index) {
                Some(next) if next.kind == TokenKind::OpenParen => index += 1,
                Some(next) if next.kind == TokenKind::Assign => {
                    index += 1;
                    let Some(value) = tokens.get(index) else {
                        continue;
                    };
                    index += 1;
                    if value.kind != TokenKind::StringLiteral {
                        continue;
                    }
                    let Ok(value) = novella_core::string::decode_string_literal(&value.slice)
                    else {
                        continue;
                    };
                    if !check.keys.contains(name) && !KNOWN_KEYS.contains(&name) {
                        errors.push(format!("未声明的 cfg 键 `{name}`"));
                    }
                    if let Some(allowed) = check.values.get(name)
                        && !allowed.contains(&value)
                    {
                        errors.push(format!("未声明的 cfg 值 `{value}`（键 {name}）"));
                    }
                }
                _ => {
                    // 裸 flag：`all`/`any`/`not` 是组合器，不参与键检查。
                    if matches!(name, "all" | "any" | "not") {
                        continue;
                    }
                    if !check.keys.contains(name) && !KNOWN_FLAGS.contains(&name) {
                        errors.push(format!("未声明的 cfg 键 `{name}`"));
                    }
                }
            }
        }
        errors
    }
}

/// 求值器内建的键值谓词。
const KNOWN_KEYS: &[&str] = &[
    "target_os",
    "target_arch",
    "target_family",
    "target_vendor",
    "target_env",
    "target_abi",
    "target_endian",
    "target_pointer_width",
    "target_feature",
    "feature",
];

/// 求值器内建的裸 flag 谓词。
const KNOWN_FLAGS: &[&str] = &[
    "unix",
    "windows",
    "target_has_float",
    "true",
    "false",
    "test",
    "debug_assertions",
    "panic",
];

struct Predicate<'a> {
    tokens: &'a [Token],
    index: usize,
    cfg: &'a CfgOptions,
}

impl Predicate<'_> {
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self
            .tokens
            .get(self.index)
            .is_some_and(|token| token.kind == kind)
        {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn parse(&mut self) -> Result<bool, String> {
        stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || self.parse_inner())
    }

    fn parse_inner(&mut self) -> Result<bool, String> {
        let token = self.tokens.get(self.index).ok_or("cfg 需要一个条件")?;
        self.index += 1;
        let name = token.slice.as_str();
        if self.eat(TokenKind::OpenParen) {
            if !matches!(name, "all" | "any" | "not") {
                return Err(format!("未知 cfg 组合 `{name}`"));
            }
            let mut values = Vec::new();
            if !self.eat(TokenKind::CloseParen) {
                loop {
                    values.push(self.parse()?);
                    if self.eat(TokenKind::CloseParen) {
                        break;
                    }
                    if !self.eat(TokenKind::Comma) {
                        return Err("cfg 组合条件之间需要逗号".to_owned());
                    }
                    if self.eat(TokenKind::CloseParen) {
                        break;
                    }
                }
            }
            return match name {
                "all" => Ok(values.iter().all(|value| *value)),
                "any" => Ok(values.iter().any(|value| *value)),
                "not" if values.len() == 1 => Ok(!values[0]),
                _ => Err("cfg not 必须恰有一个条件".to_owned()),
            };
        }
        if self.eat(TokenKind::Assign) {
            let value = self
                .tokens
                .get(self.index)
                .ok_or("cfg 键值条件缺少字符串")?;
            self.index += 1;
            if value.kind != TokenKind::StringLiteral {
                return Err("cfg 条件值必须是普通字符串".to_owned());
            }
            let value = novella_core::string::decode_string_literal(&value.slice)
                .map_err(|error| error.to_string())?;
            return self.key_value(name, &value);
        }
        self.flag(name)
    }

    fn key_value(&self, name: &str, value: &str) -> Result<bool, String> {
        match name {
            "feature" => Ok(self.cfg.features.contains(value)),
            "target_os"
            | "target_arch"
            | "target_family"
            | "target_vendor"
            | "target_env"
            | "target_abi"
            | "target_endian"
            | "target_pointer_width"
            | "target_feature" => Ok(self
                .cfg
                .key_values(name)
                .is_some_and(|values| values.contains(value))),
            // `--cfg 'k="v"'` 允许自定义键，与 rustc 一致：可用键由目标描述与命令行共同决定。
            _ => match self.cfg.key_values(name) {
                Some(values) => Ok(values.contains(value)),
                None => Err(format!("未知 cfg 条件键 `{name}`")),
            },
        }
    }

    fn flag(&self, name: &str) -> Result<bool, String> {
        match name {
            "true" => Ok(true),
            "false" => Ok(false),
            "unix" | "windows" | "target_has_float" | "debug_assertions" => {
                Ok(self.cfg.has_flag(name))
            }
            // `panic` 在本语言固定为 unwind 关闭的 abort 语义，恒为 "abort"。
            "panic" => Ok(false),
            // `test` 只在测试编译时由调用方显式注入。
            "test" => Ok(self.cfg.has_flag(name)),
            // 形如 `--cfg foo` 的自定义 flag 通过 extra.flags 表达。
            _ if self.cfg.has_flag(name) => Ok(true),
            _ => Err(format!("未知 cfg 条件 `{name}`")),
        }
    }
}

/// 按顶层逗号切分；字符串字面量内的逗号不参与切分。
fn split_top_level(input: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                current.push(ch);
                escaped = true;
            }
            '"' => {
                in_string = !in_string;
                current.push(ch);
            }
            ',' if !in_string => {
                parts.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if in_string {
        return Err("--check-cfg 的字符串字面量没有闭合".into());
    }
    parts.push(current.trim().to_owned());
    Ok(parts)
}

/// 去掉可选的双引号并解码转义；未加引号时按原文返回。
fn unquote(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.starts_with('"') {
        return novella_core::string::decode_string_literal(value)
            .map_err(|error| error.to_string());
    }
    if value.is_empty() {
        return Err("cfg 值不能为空".into());
    }
    Ok(value.to_owned())
}

/// 求值报告：谓词真值与检查错误；供 sema 在裁剪 AST 时使用。
pub struct Evaluation {
    pub enabled: bool,
    pub errors: Vec<(Span, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use novella_core::token::{Token, TokenKind};
    use novella_core::{Span, string::decode_string_literal};

    /// 构造 cfg 谓词 token 序列：标识符 / 字符串 / 标点，跳过解码细节。
    fn tokens(source: &str) -> Vec<Token> {
        let mut result = Vec::new();
        let mut index = 0;
        let bytes = source.as_bytes();
        while index < bytes.len() {
            let ch = source[index..].chars().next().unwrap();
            let start = index;
            if ch.is_whitespace() {
                index += ch.len_utf8();
                continue;
            }
            let (kind, end) = match ch {
                '"' => {
                    let mut end = index + 1;
                    while end < bytes.len() && bytes[end] != b'"' {
                        end += 1;
                    }
                    (TokenKind::StringLiteral, end + 1)
                }
                '(' => (TokenKind::OpenParen, index + 1),
                ')' => (TokenKind::CloseParen, index + 1),
                ',' => (TokenKind::Comma, index + 1),
                '=' => (TokenKind::Assign, index + 1),
                _ => {
                    let mut end = index;
                    while end < bytes.len() {
                        let next = source[end..].chars().next().unwrap();
                        if !(next.is_alphanumeric() || next == '_' || next == '-' || next == '.') {
                            break;
                        }
                        end += next.len_utf8();
                    }
                    (TokenKind::Identifier, end)
                }
            };
            index = end;
            result.push(Token {
                kind,
                span: Span { start, end },
                slice: source[start..end].to_string(),
                line: 1,
                column: start,
            });
        }
        result
    }

    fn linux() -> CfgOptions {
        CfgOptions::from_spec(
            &crate::resolve_builtin(novella_core::target::TargetPlatform::LinuxX86_64).unwrap(),
        )
    }

    fn bare() -> CfgOptions {
        CfgOptions::from_spec(
            &crate::resolve_builtin(novella_core::target::TargetPlatform::BareMetalX86_64).unwrap(),
        )
    }

    #[test]
    fn 目标派生谓词() {
        let linux = linux();
        assert_eq!(linux.evaluate(&tokens(r#"target_os = "linux""#)), Ok(true));
        assert_eq!(linux.evaluate(&tokens(r#"target_os = "none""#)), Ok(false));
        assert_eq!(linux.evaluate(&tokens("unix")), Ok(true));
        assert_eq!(linux.evaluate(&tokens("target_has_float")), Ok(true));
        assert_eq!(
            linux.evaluate(&tokens(r#"target_pointer_width = "64""#)),
            Ok(true)
        );
        let bare = bare();
        assert_eq!(bare.evaluate(&tokens("unix")), Ok(false));
        assert_eq!(bare.evaluate(&tokens("target_has_float")), Ok(false));
        assert_eq!(
            bare.evaluate(&tokens(r#"target_family = "none""#)),
            Ok(true)
        );
    }

    #[test]
    fn 组合谓词与错误() {
        let options = linux();
        assert_eq!(
            options.evaluate(&tokens(r#"not(target_os = "none")"#)),
            Ok(true)
        );
        assert_eq!(options.evaluate(&tokens("all()")), Ok(true));
        assert_eq!(options.evaluate(&tokens("any()")), Ok(false));
        assert_eq!(
            options.evaluate(&tokens("not(unix, windows)")),
            Err("cfg not 必须恰有一个条件".to_owned())
        );
        assert_eq!(
            options.evaluate(&tokens("unknown_flag")),
            Err("未知 cfg 条件 `unknown_flag`".to_owned())
        );
        assert_eq!(
            options.evaluate(&tokens(r#"unknown_key = "x""#)),
            Err("未知 cfg 条件键 `unknown_key`".to_owned())
        );
    }

    /// 显式 `--cfg` 与目标派生值取并集，且 feature 只认已展开集合。
    #[test]
    fn cfg_注入与_feature() {
        let mut options = linux();
        options.extend_from_cfg_spec(r#"build = "dev""#).unwrap();
        options.extend_from_cfg_spec("ci").unwrap();
        options.set_features(
            crate::FeatureSet::default()
                .resolve(&["extra".to_owned()])
                .unwrap(),
        );
        assert_eq!(options.evaluate(&tokens(r#"build = "dev""#)), Ok(true));
        assert_eq!(options.evaluate(&tokens(r#"build = "prod""#)), Ok(false));
        assert_eq!(options.evaluate(&tokens(r#"feature = "extra""#)), Ok(true));
        assert_eq!(
            options.evaluate(&tokens(r#"feature = "missing""#)),
            Ok(false)
        );
        assert_eq!(options.evaluate(&tokens("ci")), Ok(true));
        assert_eq!(options.evaluate(&tokens("unix")), Ok(true));
    }

    /// `--check-cfg` 只校验键值，裸 flag 走独立白名单。
    #[test]
    fn check_cfg_白名单() {
        let mut options = linux();
        let mut check = CheckCfg::default();
        check.extend_from_spec("cfg(build)").unwrap();
        check
            .extend_from_spec(r#"values(feature,"extra")"#)
            .unwrap();
        options.check = Some(check);
        assert!(options.check(&tokens(r#"build = "dev""#)).is_empty());
        assert_eq!(
            options.check(&tokens(r#"foo = "bar""#)),
            vec!["未声明的 cfg 键 `foo`".to_owned()]
        );
        assert_eq!(
            options.check(&tokens(r#"feature = "other""#)),
            vec!["未声明的 cfg 值 `other`（键 feature）".to_owned()]
        );
        assert!(options.check(&tokens("unix")).is_empty());
    }

    /// 字符串字面量解码与注入路径一致，避免同一写法在两处得到不同值。
    #[test]
    fn 字符串字面量解码一致() {
        assert_eq!(
            decode_string_literal(
                r#""a
b""#
            )
            .unwrap(),
            "a
b"
        );
    }
}

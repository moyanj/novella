//! 从 `novella-lexer` 的 logos 声明中提取词法规则，生成 TextMate 语法的正则片段。
//!
//! 事实源是 `crates/novella-lexer/src/lib.rs` 里 `RawToken` 上的
//! `#[token("...")]` / `#[regex("...")]` 属性（logos 声明），本工具只做机械转换：
//!
//! 1. 读取源码，切出 `pub enum RawToken { ... }` 块；
//! 2. 逐变体收集属性里的字面量 / 正则，按声明顺序记录（logos 的优先级即声明顺序）；
//! 3. 把 logos 语法（`\p{L}`、`(?i)` 等）转换为 Oniguruma（TextMate 使用的引擎）
//!    可接受的形式；
//! 4. 按「注释 → 字符串 → 数字 → 关键字/运算符 → 标识符」的顺序拼成一条
//!    alternation 列表，由调用方写进 `editors/vscode/syntaxes/novella.tmLanguage.json`。
//!
//! 为什么不直接读 `TokenKind`？`TokenKind` 是纯投影、不带字面量；真正的事实源是
//! lexer 的声明。关键字表另由 `novella-core::keywords` 提供（`RawToken` 只有关键字
//! 字面量，没有说明它对应哪个 `TokenKind`）。
//!
//! 本工具**只影响编辑器冷启动着色**（LSP 语义 token 返回之前的空白期），
//! 生成的语法文件不作为语言的事实源，也不参与任何编译判定。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use novella_core::keywords::KeywordEntry;

/// 一个 logos 变体的词法声明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexRule {
    /// `RawToken` 变体名（如 `StringLiteral`）
    pub variant: String,
    /// 声明形态：字面量（`#[token]`）或正则（`#[regex]`）
    pub kind: RuleKind,
    /// 原始模式文本（未转义）
    pub pattern: String,
}

/// 声明形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// `#[token("...")]`
    Token,
    /// `#[regex(r"...")]`
    Regex,
}

/// TextMate 语法中的一条规则（scope 名 + 已转换的正则）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmRule {
    /// TextMate 作用域名（如 `keyword.control.novella`）
    pub scope: String,
    /// Oniguruma 正则（不含外层括号）
    pub regex: String,
}

/// 从一个 `#[token]` / `#[regex]` 属性行里提取模式文本。
///
/// 支持三种书写：`#[token("x")]`、`#[regex(r#"..."#)]`、`#[regex(r"...")]`。
/// 返回 `None` 表示该行不是字面量 / 正则属性（如 `#[logos(skip ...)]` 或纯标记属性）。
pub fn parse_attr(line: &str) -> Option<(RuleKind, String)> {
    let trimmed = line.trim();
    let (kind, rest) = if let Some(rest) = trimmed.strip_prefix("#[token(") {
        (RuleKind::Token, rest)
    } else {
        // 非 `#[token]` 时只可能是 `#[regex]`，其余属性（如 `#[logos(skip ...)]`）返回 None
        (RuleKind::Regex, trimmed.strip_prefix("#[regex(")?)
    };

    // 属性的模式文本可能出现在三种引号中，且其后可能有额外参数（如 `allow_greedy = true`）：
    // `"..."`、`r"..."`、`r#"..."#`。直接按引号形态截取，跳过尾部参数。
    let body = rest.trim_start();
    if let Some(after) = body.strip_prefix("r#\"") {
        let end = after.find("\"#")?;
        return Some((kind, after[..end].to_owned()));
    }
    if let Some(after) = body.strip_prefix("r\"") {
        let end = after.find('"')?;
        return Some((kind, after[..end].to_owned()));
    }
    let after = body.strip_prefix('"')?;
    let end = after.find('"')?;
    Some((kind, after[..end].to_owned()))
}

/// 从 lexer 源码中提取全部规则（按声明顺序）。
///
/// 只看 `pub enum RawToken { ... }` 的主体，跳过 `#[logos(skip ...)]` 这类
/// 枚举级属性；`skip` 规则由调用方单独处理（空白与换行不产生 token）。
pub fn extract_rules(source: &str) -> Vec<LexRule> {
    let Some(start) = source.find("pub enum RawToken") else {
        return Vec::new();
    };
    let body_start = source[start..]
        .find('{')
        .map_or(source.len(), |offset| start + offset + 1);
    let mut depth = 1i32;
    let mut end = body_start;
    for (offset, ch) in source[body_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = body_start + offset;
                    break;
                }
            }
            _ => {}
        }
    }

    let mut rules = Vec::new();
    // 当前变体名之前累积的待配对属性（logos 允许一个变体挂多条属性）
    let mut pending: Vec<(RuleKind, String)> = Vec::new();
    for line in source[body_start..end].lines() {
        let trimmed = line.trim();
        if let Some((kind, pattern)) = parse_attr(trimmed) {
            pending.push((kind, pattern));
            continue;
        }
        // 变体名行：形如 `Identifier,`（带行尾逗号）或枚举末尾的 `LineComment`（无逗号）
        if !trimmed.is_empty()
            && !trimmed.starts_with("//")
            && !trimmed.starts_with('#')
            && !trimmed.starts_with('}')
            && let name = trimmed.strip_suffix(',').unwrap_or(trimmed)
            && !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            for (kind, pattern) in pending.drain(..) {
                rules.push(LexRule {
                    variant: name.to_owned(),
                    kind,
                    pattern,
                });
            }
        }
    }
    rules
}

/// logos 正则 → Oniguruma 正则的机械转换。
///
/// 处理项：
/// - `(?i)` → 删除（关键字按小写处理，大小写敏感由调用方决定）；
/// - `\p{L}` / `\p{Ll}` 等 Unicode 类别 → 等价的 POSIX 类别
///   （Oniguruma 支持 `\p{L}`，这里保留原样，仅把 logos 不支持而 Oniguruma 支持的
///   写法统一到 Oniguruma 方言，故仅做危险的 `(?...)` 剥离与字符串转义）。
pub fn to_oniguruma(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // 跳过 `(?i)` / `(?s)` 这类内联标志（TextMate 用 `(?i)` 亦可，但为避免
        // 与后缀组合冲突，统一剥离并在外层按需添加）
        if chars[i] == '('
            && i + 3 < chars.len()
            && chars[i + 1] == '?'
            && let Some(close) = chars[i..].iter().position(|&c| c == ')')
            && chars[i + 2..i + close]
                .iter()
                .all(|c| matches!(c, 'i' | 's' | 'm'))
        {
            i += close + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 把提取出的规则按用途分组。
///
/// 返回 `(注释, 字符串, 数字, 运算符, 关键字字面量, 标识符)` 六组模式；
/// 分组依据是变体名，语义与 lexer 注释中的分组标题对应。
pub fn group_rules(rules: &[LexRule]) -> TmGroups {
    let mut groups = TmGroups::default();
    for rule in rules {
        let pattern = to_oniguruma(&rule.pattern);
        let name = rule.variant.as_str();
        match name {
            "LineComment" => groups.comment.push(pattern),
            name if name.contains("StringLiteral") => groups.string.push(pattern),
            name if name.contains("IntegerLiteral") || name == "DecimalLiteral" => {
                groups.number.push(pattern)
            }
            // 关键字由 `novella_core::keywords` 提供（词法上等价于标识符），
            // 因此 `#[token("if")]` 之类的字面量不能再进入运算符组
            "Identifier" => groups.identifier.push(pattern),
            name if is_keyword_variant(name) => {}
            _ => {
                if rule.kind == RuleKind::Token {
                    groups.operator.push(pattern);
                }
            }
        }
    }
    groups
}

/// 按作用域归组后的模式集合。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TmGroups {
    /// 注释模式
    pub comment: Vec<String>,
    /// 字符串模式
    pub string: Vec<String>,
    /// 数字模式
    pub number: Vec<String>,
    /// 运算符 / 分隔符模式
    pub operator: Vec<String>,
    /// 标识符模式
    pub identifier: Vec<String>,
}

impl TmGroups {
    /// 拼成一条 alternation（各模式用 `|` 连接并加括号保护优先级）。
    pub fn join(patterns: &[String]) -> String {
        patterns
            .iter()
            .map(|p| format!("({p})"))
            .collect::<Vec<_>>()
            .join("|")
    }
}

/// 生成 TextMate 语法 JSON（字符串形式）。
///
/// `keywords` 为关键字种子表；生成结果中每个 scope 一条匹配：
/// `comment.line.novella` / `string.quoted.novella` / `constant.numeric.novella` /
/// `keyword.control.novella` / `keyword.operator.novella` / `variable.other.novella`。
pub fn build_grammar(groups: &TmGroups, keywords: &[KeywordEntry]) -> String {
    let mut literal_groups: HashMap<&str, Vec<&str>> = HashMap::new();
    for entry in keywords {
        literal_groups
            .entry(keyword_scope(entry.literal))
            .or_default()
            .push(entry.literal);
    }

    let mut patterns: Vec<String> = Vec::new();
    // 注释与字符串必须排在运算符之前（`//` 比 `/` 长，靠顺序而非回溯保证正确）
    for (scope, rule_list) in [
        ("comment.line.novella", &groups.comment),
        ("string.quoted.novella", &groups.string),
        ("constant.numeric.novella", &groups.number),
    ] {
        if rule_list.is_empty() {
            continue;
        }
        patterns.push(format!(
            "        {{\n            \"name\": \"{scope}\",\n            \"match\": \"{}\"\n        }}",
            escape_json(&TmGroups::join(rule_list))
        ));
    }

    // 关键字：按组各自一条（控制流 / 声明 / 字面量常量），便于主题分别配色
    for (scope, _) in [
        ("keyword.control.novella", ""),
        ("keyword.other.novella", ""),
        ("constant.language.novella", ""),
    ] {
        let mut literals: Vec<&str> = Vec::new();
        for (key, entries) in &literal_groups {
            if *key == scope {
                literals.extend(entries.iter().copied());
            }
        }
        if literals.is_empty() {
            continue;
        }
        literals.sort_unstable_by_key(|literal| std::cmp::Reverse(literal.len()));
        let alternation = literals
            .iter()
            .map(|literal| format!(r"\b{}\b", regex_escape(literal)))
            .collect::<Vec<_>>()
            .join("|");
        // 正则里的 `\|` / `\` 等必须以 JSON 转义形式出现，否则整份语法文件解析失败
        patterns.push(format!(
            "        {{\n            \"name\": \"{scope}\",\n            \"match\": \"{}\"\n        }}",
            escape_json(&alternation)
        ));
    }

    // 运算符 / 分隔符：长条优先（`<<=` 先于 `<<` 先于 `<`），否则 `==` 会被拆成两个 `=`
    if !groups.operator.is_empty() {
        let mut operators = groups.operator.clone();
        operators.sort_unstable_by_key(|operator| std::cmp::Reverse(operator.len()));
        let alternation = operators
            .iter()
            .map(|operator| regex_escape(operator))
            .collect::<Vec<_>>()
            .join("|");
        patterns.push(format!(
            "        {{\n            \"name\": \"keyword.operator.novella\",\n            \"match\": \"{}\"\n        }}",
            escape_json(&alternation)
        ));
    }

    if let Some(identifier) = groups.identifier.first() {
        patterns.push(format!(
            "        {{\n            \"name\": \"variable.other.novella\",\n            \"match\": \"{}\"\n        }}",
            escape_json(identifier)
        ));
    }

    let body = patterns.join(",\n");

    // 手工拼装而非依赖 serde：输出必须稳定（每次生成同一份文本，便于 git diff 审查）
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(
        "    \"$schema\": \"https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json\",\n",
    );
    out.push_str("    \"name\": \"Novella\",\n");
    out.push_str("    \"scopeName\": \"source.novella\",\n");
    out.push_str(
        "    \"comment\": \"由 novella-syntax-gen 从 novella-lexer 的 logos 声明生成，请勿手工编辑；重新生成：just gen-syntax\",\n",
    );
    out.push_str("    \"patterns\": [\n");
    out.push_str("        {\n");
    out.push_str("            \"include\": \"#expressions\"\n");
    out.push_str("        }\n");
    out.push_str("    ],\n");
    out.push_str("    \"repository\": {\n");
    out.push_str("        \"expressions\": {\n");
    out.push_str("            \"patterns\": [\n");
    out.push_str(&body);
    out.push_str("\n            ]\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

/// 变体名是否属于关键字（`from` / `let` / `func` … `null` / `BooleanLiteral`）。
///
/// 关键字的分组抓取交给 `novella_core::keywords`，这里只负责把它们从运算符组里剔除，
/// 避免 `if` 同时被匹配为 `keyword.other` 与 `keyword.operator`。
fn is_keyword_variant(variant: &str) -> bool {
    // `BooleanLiteral` 与 `Null` 在词法上是字面量关键字，同样不进入运算符组
    novella_core::keywords::KEYWORDS
        .iter()
        .any(|entry| variant_name(entry.kind) == variant)
        || matches!(variant, "Null" | "BooleanLiteral")
}

/// 从 `TokenKind` 取变体名（用于与 logos 变体名对照；两者拼写一致）。
fn variant_name(kind: novella_core::token::TokenKind) -> &'static str {
    // `TokenKind` 未提供 `as_str`，但 Debug 输出即变体名，且本项目无自定义 Debug
    use novella_core::token::TokenKind;
    match kind {
        TokenKind::From => "From",
        TokenKind::Import => "Import",
        TokenKind::Pub => "Pub",
        TokenKind::Pri => "Pri",
        TokenKind::Pkg => "Pkg",
        TokenKind::Const => "Const",
        TokenKind::Let => "Let",
        TokenKind::Static => "Static",
        TokenKind::Weak => "Weak",
        TokenKind::Func => "Func",
        TokenKind::Class => "Class",
        TokenKind::Struct => "Struct",
        TokenKind::Trait => "Trait",
        TokenKind::New => "New",
        TokenKind::This => "This",
        TokenKind::If => "If",
        TokenKind::Else => "Else",
        TokenKind::Elif => "Elif",
        TokenKind::While => "While",
        TokenKind::For => "For",
        TokenKind::In => "In",
        TokenKind::Break => "Break",
        TokenKind::Continue => "Continue",
        TokenKind::Return => "Return",
        TokenKind::Default => "Default",
        TokenKind::Match => "Match",
        TokenKind::Case => "Case",
        TokenKind::KeyAnd => "KeyAnd",
        TokenKind::KeyOr => "KeyOr",
        TokenKind::KeyNot => "KeyNot",
        TokenKind::Try => "Try",
        TokenKind::Catch => "Catch",
        TokenKind::Finally => "Finally",
        TokenKind::Throw => "Throw",
        TokenKind::With => "With",
        TokenKind::As => "As",
        TokenKind::Type => "Type",
        TokenKind::Is => "Is",
        TokenKind::Async => "Async",
        TokenKind::Await => "Await",
        TokenKind::Yield => "Yield",
        TokenKind::Extern => "Extern",
        TokenKind::Unsafe => "Unsafe",
        TokenKind::Null => "Null",
        TokenKind::BooleanLiteral => "BooleanLiteral",
        _ => "",
    }
}

/// 关键字字面量 → TextMate 作用域。
///
/// 该映射只影响**冷启动期的配色观感**，与 LSP 语义 token 的类别无关，
/// 因此允许在编辑体验层面做细分（控制流 / 声明 / 常量）。
pub fn keyword_scope(literal: &str) -> &'static str {
    match literal {
        "true" | "false" | "null" => "constant.language.novella",
        "if" | "else" | "elif" | "while" | "for" | "in" | "break" | "continue" | "return"
        | "match" | "case" | "default" | "try" | "catch" | "finally" | "throw" | "with" | "and"
        | "or" | "not" | "yield" | "await" | "async" => "keyword.control.novella",
        _ => "keyword.other.novella",
    }
}

/// 转义为 JSON 字符串内容（不包含外层引号）。
fn escape_json(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 转义为 Oniguruma 正则字面量。
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 生成入口：读 lexer 源码与关键字表，返回语法文件文本。
pub fn generate(lexer_source: &str, keywords: &[KeywordEntry]) -> String {
    let rules = extract_rules(lexer_source);
    let groups = group_rules(&rules);
    build_grammar(&groups, keywords)
}

/// 把生成结果写入目标路径（创建父目录）。
pub fn write_grammar(path: &Path, grammar: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, grammar)
}

/// 默认输出路径：`editors/vscode/syntaxes/novella.tmLanguage.json`。
pub fn default_output(workspace_root: &Path) -> PathBuf {
    workspace_root.join("editors/vscode/syntaxes/novella.tmLanguage.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 lexer 源码必须能提取出全部关键字之外的规则。
    #[test]
    fn extracts_rules_from_real_lexer() {
        let source = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../novella-lexer/src/lib.rs"),
        )
        .expect("应能读取 lexer 源码");
        let rules = extract_rules(&source);
        assert!(rules.len() > 80, "应提取到全部字面量 / 正则声明");
        assert!(
            rules.iter().any(|rule| rule.variant == "LineComment"),
            "应提取到行注释规则"
        );
        assert!(
            rules.iter().any(|rule| rule.variant == "Identifier"),
            "应提取到标识符规则"
        );
    }

    /// 属性解析支持普通字符串与原始字符串两种写法。
    #[test]
    fn parses_attribute_forms() {
        assert_eq!(
            parse_attr(r#"#[token("func")]"#),
            Some((RuleKind::Token, "func".to_owned()))
        );
        // 原始字符串内层带 `"` 的写法：外层用 r##"..."## 包裹
        assert_eq!(
            parse_attr(r##"#[regex(r#"[_\p{L}$]+"#)]"##),
            Some((RuleKind::Regex, r"[_\p{L}$]+".to_owned()))
        );
        // 带额外参数的属性（allow_greedy）：仍能取出模式文本
        assert_eq!(
            parse_attr(r##"#[regex(r#"//[^\n]*"#, allow_greedy = true)]"##),
            Some((RuleKind::Regex, r"//[^\n]*".to_owned()))
        );
        // 枚举级属性（skip）不是变体声明
        assert_eq!(parse_attr(r##"#[logos(skip r"[ \t]+")]"##), None);
    }

    /// 生成结果必须是合法 JSON（结构性回归防线）。
    #[test]
    fn generated_grammar_is_valid_json() {
        let source = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../novella-lexer/src/lib.rs"),
        )
        .expect("应能读取 lexer 源码");
        let grammar = generate(&source, &novella_core::keywords::KEYWORDS);
        let value: serde_json::Value = serde_json::from_str(&grammar).expect("应为合法 JSON");
        assert_eq!(value["scopeName"], "source.novella");
        assert!(
            value["repository"]["expressions"]["patterns"]
                .as_array()
                .is_some()
        );
    }
}

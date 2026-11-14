use colored::Colorize;
use novella_core::diagnostic::{Diagnostic, Severity};

/// 将诊断渲染为 rustc 风格文本（多行），例如：
///
/// ```text
/// error[NVL1002]: 未闭合的字符串字面量：以 '"' 开头，缺少闭合引号
///   --> test.nv:2:5
///    |
///  2 | let x = "abc
///    |       ^^^^^
/// ```
///
/// - `source` 为出错文件的源码，用于展示出错行与 caret 指示；
/// - 带错误码时首行渲染为 `error[NVLxxxx]: <msg>`，无错误码时退回 `error: <msg>`；
/// - 诊断无位置（`location` 为 None，如文件读取失败）时仅输出首行；
/// - caret 按 span 长度渲染（至少 1 个 `^`，不越出该行），
///   列号以字节偏移计、渲染时转字符列，保证多字节字符前指示不错位。
pub fn render_diagnostic(diag: &Diagnostic, source: Option<&str>) -> String {
    let mut out = String::new();
    let severity = match diag.severity {
        Severity::Error => "error".red(),
        Severity::Warning => "warning".yellow(),
    };
    // 错误码固定渲染在 severity 之后（rustc 的 `error[E0308]` 风格），便于按码检索
    match &diag.code {
        Some(code) => out.push_str(&format!("{severity}[{code}]: {}\n", diag.message)),
        None => out.push_str(&format!("{severity}: {}\n", diag.message)),
    }

    let Some(loc) = &diag.location else {
        return out;
    };
    let file = loc.file.as_deref().unwrap_or("<unknown>");
    // 展示列号转 1-based（内部存 0-based 字节偏移）
    out.push_str(&format!("  --> {file}:{}:{}\n", loc.line, loc.column + 1));

    if let Some(src) = source.and_then(|s| s.lines().nth(loc.line - 1)) {
        let line_no = loc.line.to_string();
        let gutter = " ".repeat(line_no.len());
        out.push_str(&format!("{gutter} |\n"));
        out.push_str(&format!("{line_no} | {src}\n"));
        // caret 长度：span 字节长 clamp 到 [1, 行内剩余字节]；
        // 再按该字节区间内的完整字符数渲染，避免多字节字符（如 emoji）被拆散或超长
        let caret_bytes = loc
            .span
            .map(|s| s.end.saturating_sub(s.start))
            .unwrap_or(1)
            .clamp(1, src.len().saturating_sub(loc.column).max(1));
        let col_chars = src.chars().take(loc.column).count();
        // token 起点/终点均在字符边界，get 越界或落于字符中间时退化为 1
        let caret_chars = src
            .get(loc.column..loc.column + caret_bytes)
            .map(|seg| seg.chars().count())
            .unwrap_or(1)
            .max(1);
        out.push_str(&format!(
            "{gutter} | {}{}\n",
            " ".repeat(col_chars),
            "^".repeat(caret_chars)
        ));
    }

    if let Some(note) = &diag.note {
        out.push_str(&format!("  = note: {note}\n"));
    }
    out
}

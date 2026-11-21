//! 字符串字面量解码：语言层字符串的**唯一**解码实现。
//!
//! 为什么放在 core：sema（类型检查期校验非法转义）与 HIR（降级期取字面量内容）
//! 必须给出一致结果，两处各写一份解码表迟早漂移。这里定义一次，两边共用
//! （见 `docs/sema-coverage.md` 矩阵 L10）。
//!
//! 支持的形态（design.md §3.3）：
//!
//! | 前缀 | 形态 | 转义 |
//! | --- | --- | --- |
//! | 无 / `b` | `"..."` / `'...'` | 按 [`decode_escapes`] 解释 |
//! | `f` | `f"..."` / `f'...'` | 同普通字符串（插值由 sema 另行处理） |
//! | `r` | `r"..."` / `r'...'` | **不解释**，反斜杠即普通字符 |
//!
//! 转义表：`\n` `\r` `\t` `\0` `\\` `\'` `\"` `\xHH`（两位十六进制，必须 ≤ 0x7F）
//! `\u{H...}`（1–6 位十六进制 Unicode 标量值）。其余一律视为非法。

/// 解码失败的原因（调用方据此给出精确诊断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// 字节串禁止 Unicode 转义和超出单字节的字符。
    NotAByte,
    /// 不是可识别的字符串字面量形态（缺引号、前后引号不一致、前缀未知）
    NotAStringLiteral,
    /// 反斜杠后跟了不受支持的转义引导字符
    UnknownEscape(char),
    /// `\x` 之后不是两位十六进制
    BadHexEscape,
    /// `\u{...}` 结构不合法（无花括号、位数为 0 或超过 6、不是十六进制）
    BadUnicodeEscape,
    /// `\u{...}` 的码点不是合法 Unicode 标量值（代理区或超出 U+10FFFF）
    InvalidCodePoint(u32),
    /// 字面量以反斜杠结尾（转义序列不完整）
    TrailingBackslash,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAByte => write!(f, "字节字符串仅允许单字节字符与字节转义"),
            Self::NotAStringLiteral => write!(f, "不是合法的字符串字面量"),
            Self::UnknownEscape(ch) => write!(f, "非法转义序列 `\\{ch}`"),
            Self::BadHexEscape => write!(f, "`\\x` 之后必须是两位十六进制数字"),
            Self::BadUnicodeEscape => write!(f, "`\\u{{...}}` 内必须是 1–6 位十六进制码点"),
            Self::InvalidCodePoint(cp) => {
                write!(f, "`U+{cp:04X}` 不是合法的 Unicode 标量值")
            }
            Self::TrailingBackslash => write!(f, "字面量以反斜杠结尾，转义序列不完整"),
        }
    }
}

/// 解码字节字符串；载荷保留原始字节，不能经 UTF-8 再编码。
pub fn decode_byte_literal(raw: &str) -> Result<Vec<u8>, DecodeError> {
    let rest = raw
        .strip_prefix('b')
        .ok_or(DecodeError::NotAStringLiteral)?;
    let quote = rest.chars().next().ok_or(DecodeError::NotAStringLiteral)?;
    if !matches!(quote, '\'' | '"') {
        return Err(DecodeError::NotAStringLiteral);
    }
    let body = rest
        .strip_prefix(quote)
        .and_then(|s| s.strip_suffix(quote))
        .ok_or(DecodeError::NotAStringLiteral)?;
    let mut chars = body.chars();
    let mut result = Vec::new();
    while let Some(ch) = chars.next() {
        let value = if ch == '\\' {
            match chars.next().ok_or(DecodeError::TrailingBackslash)? {
                'n' => b'\n',
                'r' => b'\r',
                't' => b'\t',
                '0' => 0,
                '\\' => b'\\',
                '\'' => b'\'',
                '"' => b'"',
                'x' => {
                    let a = chars
                        .next()
                        .and_then(|c| c.to_digit(16))
                        .ok_or(DecodeError::BadHexEscape)?;
                    let b = chars
                        .next()
                        .and_then(|c| c.to_digit(16))
                        .ok_or(DecodeError::BadHexEscape)?;
                    (a * 16 + b) as u8
                }
                'u' => return Err(DecodeError::NotAByte),
                other => return Err(DecodeError::UnknownEscape(other)),
            }
        } else {
            u8::try_from(u32::from(ch)).map_err(|_| DecodeError::NotAByte)?
        };
        result.push(value);
    }
    Ok(result)
}

/// 字面量的前缀分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prefix {
    Plain,
    Format,
    Raw,
    Byte,
}

/// 解码 a string literal to its runtime text.
///
/// `raw` is the literal exactly as it appears in the source, including any
/// prefix (`f`, `r`, `b`) and the surrounding quotes.
pub fn decode_string_literal(raw: &str) -> Result<String, DecodeError> {
    let (prefix, rest) = split_prefix(raw);
    let quote = rest.chars().next().ok_or(DecodeError::NotAStringLiteral)?;
    if !matches!(quote, '\'' | '"') {
        return Err(DecodeError::NotAStringLiteral);
    }
    let body = rest
        .strip_prefix(quote)
        .and_then(|s| s.strip_suffix(quote))
        .ok_or(DecodeError::NotAStringLiteral)?;
    // 原始字符串：内容原样保留（引号内的一切都是字面字符）
    if prefix == Prefix::Raw {
        return Ok(body.to_string());
    }
    decode_escapes(body)
}

/// 拆出前缀，返回（前缀类别, 去前缀后的剩余文本）。
fn split_prefix(raw: &str) -> (Prefix, &str) {
    match raw.as_bytes().first() {
        Some(b'f') => (Prefix::Format, &raw[1..]),
        Some(b'r') => (Prefix::Raw, &raw[1..]),
        Some(b'b') => (Prefix::Byte, &raw[1..]),
        _ => (Prefix::Plain, raw),
    }
}

/// 按转义表解码字符串主体（不含引号）。
///
/// 逐字符扫描；遇到 `\` 时按引导字符分派。`\x` 与 `\u{...}` 需要读取后续字符，
/// 因此用 `Peekable` 而不是索引，避免多字节字符切错位置。
pub fn decode_escapes(body: &str) -> Result<String, DecodeError> {
    let mut chars = body.chars().peekable();
    let mut out = String::with_capacity(body.len());
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some(lead) = chars.next() else {
            return Err(DecodeError::TrailingBackslash);
        };
        match lead {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'x' => out.push(decode_hex_escape(&mut chars)?),
            'u' => out.push(decode_unicode_escape(&mut chars)?),
            other => return Err(DecodeError::UnknownEscape(other)),
        }
    }
    Ok(out)
}

/// `\xHH`：两位十六进制，值域限定 ASCII（design.md §3.3 只定义了单字节转义）。
fn decode_hex_escape(chars: &mut impl Iterator<Item = char>) -> Result<char, DecodeError> {
    let mut value = 0u32;
    for _ in 0..2 {
        let digit = chars.next().ok_or(DecodeError::BadHexEscape)?;
        value = value * 16 + digit.to_digit(16).ok_or(DecodeError::BadHexEscape)?;
    }
    if value > 0x7F {
        return Err(DecodeError::InvalidCodePoint(value));
    }
    char::from_u32(value).ok_or(DecodeError::InvalidCodePoint(value))
}

/// `\u{H...}`：1–6 位十六进制码点，必须是合法 Unicode 标量值。
fn decode_unicode_escape(chars: &mut impl Iterator<Item = char>) -> Result<char, DecodeError> {
    if chars.next() != Some('{') {
        return Err(DecodeError::BadUnicodeEscape);
    }
    let mut digits = String::new();
    loop {
        match chars.next() {
            Some('}') => break,
            Some(ch) if ch.is_ascii_hexdigit() && digits.len() < 6 => digits.push(ch),
            // 超过 6 位或出现非十六进制字符：结构不合法
            Some(_) => return Err(DecodeError::BadUnicodeEscape),
            None => return Err(DecodeError::BadUnicodeEscape),
        }
    }
    let codepoint = u32::from_str_radix(&digits, 16).map_err(|_| DecodeError::BadUnicodeEscape)?;
    char::from_u32(codepoint).ok_or(DecodeError::InvalidCodePoint(codepoint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_supported_escapes() {
        assert_eq!(decode_escapes(r"a\nb\tc").unwrap(), "a\nb\tc");
        assert_eq!(decode_escapes(r"\x41").unwrap(), "A");
        assert_eq!(decode_escapes(r"\u{4F60}").unwrap(), "你");
    }

    #[test]
    fn rejects_unknown_and_malformed_escapes() {
        assert_eq!(decode_escapes(r"\q"), Err(DecodeError::UnknownEscape('q')));
        assert_eq!(decode_escapes(r"\x4"), Err(DecodeError::BadHexEscape));
        assert_eq!(decode_escapes("\\"), Err(DecodeError::TrailingBackslash));
        assert_eq!(
            decode_escapes(r"\u{110000}"),
            Err(DecodeError::InvalidCodePoint(0x11_0000))
        );
    }

    #[test]
    fn raw_strings_keep_backslashes_verbatim() {
        assert_eq!(decode_string_literal(r#"r"C:\new""#).unwrap(), r"C:\new");
        // 普通字符串里同样的文本会被解码
        assert_eq!(decode_string_literal(r#""C:\new""#).unwrap(), "C:\new");
    }

    #[test]
    fn prefixes_and_quotes() {
        assert_eq!(decode_string_literal("'abc'").unwrap(), "abc");
        assert_eq!(decode_string_literal(r#"b"ab""#).unwrap(), "ab");
        assert_eq!(decode_string_literal(r#"f"ab""#).unwrap(), "ab");
        assert_eq!(
            decode_string_literal("abc"),
            Err(DecodeError::NotAStringLiteral)
        );
    }
}

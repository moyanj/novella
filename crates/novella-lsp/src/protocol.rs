//! LSP over stdio 的报文帧编解码。
//!
//! LSP 报文格式为 `Content-Length: <字节数>\r\n`（可附带 `Content-Type` 等头）、
//! 一个空行，以及 JSON 正文。本模块只负责帧的读写定界，
//! 不解释 JSON-RPC 语义（那是 [`crate::server`] 的职责）。

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// 从 `reader` 读取一帧报文，返回其 JSON 文本；流正常结束返回 `Ok(None)`。
///
/// 以下情形视为不可恢复的协议错误（交由上层终止）：
/// - 头部尚未结束就遇到流结束；
/// - 缺少 `Content-Length` 头，或其数值非法；
/// - 正文不是合法 UTF-8。
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    let mut header_started = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            // EOF：一个头部字节都没读到属正常结束（客户端关闭管道），
            // 否则说明报文在头部中途被截断
            return if header_started {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "报文头未结束即遇到流结束",
                ))
            } else {
                Ok(None)
            };
        }
        header_started = true;
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break; // 空行标志头部结束
        }
        // 只关心长度头，其余头（Content-Type 等）按规范忽略
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Content-Length 非法：{e}"),
                )
            })?);
        }
    }

    let length = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "报文缺少 Content-Length 头"))?;
    // 按字节精确读取正文：多字节字符（如中文）按字符读会截断
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let text = String::from_utf8(body).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("报文体不是合法 UTF-8：{e}"),
        )
    })?;
    Ok(Some(text))
}

/// 向 `writer` 写出一帧报文并刷新。
///
/// `Content-Length` 按正文的 UTF-8 字节数计算（非字符数），
/// 这是客户端定界读取的唯一依据。
pub fn write_message<W: Write>(writer: &mut W, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("报文序列化失败：{e}")))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// 帧编解码往返：写出的报文能被原样读回，且长度头按字节计。
    #[test]
    fn round_trip_single_message() {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "初始化",
            "params": { "text": "中文内容 emoji 🐋" },
        });
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).expect("写入应成功");

        let mut reader = Cursor::new(buffer);
        let text = read_message(&mut reader).expect("读取应成功");
        let parsed: Value =
            serde_json::from_str(&text.expect("应读到一帧")).expect("应是合法 JSON");
        assert_eq!(parsed, message);
        // 下一帧应为正常结束
        assert!(read_message(&mut reader).expect("EOF 应正常").is_none());
    }

    /// 连续多帧按序读出，互不粘连。
    #[test]
    fn round_trip_multiple_messages() {
        let first = serde_json::json!({ "id": 1, "method": "a" });
        let second = serde_json::json!({ "id": 2, "method": "b" });
        let mut buffer = Vec::new();
        write_message(&mut buffer, &first).expect("写入应成功");
        write_message(&mut buffer, &second).expect("写入应成功");

        let mut reader = Cursor::new(buffer);
        for expected in [&first, &second] {
            let text = read_message(&mut reader)
                .expect("读取应成功")
                .expect("应读到一帧");
            let parsed: Value = serde_json::from_str(&text).expect("应是合法 JSON");
            assert_eq!(&parsed, expected);
        }
        assert!(read_message(&mut reader).expect("EOF 应正常").is_none());
    }

    /// 额外头部（如 Content-Type）应被忽略。
    #[test]
    fn tolerates_extra_headers() {
        let body = r#"{"jsonrpc":"2.0","id":1}"#;
        let raw = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = Cursor::new(raw.into_bytes());
        let text = read_message(&mut reader)
            .expect("读取应成功")
            .expect("应读到一帧");
        assert_eq!(text, body);
    }

    /// 缺少 Content-Length 头应报错，而不是静默挂起。
    #[test]
    fn missing_content_length_is_error() {
        let raw = "Content-Type: application/json\r\n\r\n";
        let mut reader = Cursor::new(raw.as_bytes().to_vec());
        assert!(read_message(&mut reader).is_err());
    }
}

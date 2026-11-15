//! 服务主循环：帧读取、分发、回写。
//!
//! 逐帧读取请求并串行处理：LSP 的 stdio 通道本身是串行的，
//! 薄实现无需并发或异步。

use std::io::{self, BufRead, Write};

use serde_json::Value;

use crate::{protocol, server::Server};

/// 运行服务器主循环，返回进程退出码。
///
/// 逐帧读取请求并串行处理：LSP 的 stdio 通道本身是串行的，
/// 薄实现无需并发或异步。
pub fn run<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<i32> {
    let mut server = Server::new();
    eprintln!("[novella-lsp] 服务已启动");

    while let Some(text) = protocol::read_message(reader)? {
        let message: Value = match serde_json::from_str(&text) {
            Ok(message) => message,
            // 单条报文损坏不应终止整个会话，记录后继续读下一帧
            Err(e) => {
                eprintln!("[novella-lsp] 报文不是合法 JSON，已跳过：{e}");
                continue;
            }
        };

        for response in server.handle(&message) {
            protocol::write_message(writer, &response)?;
        }
        if let Some(code) = server.exit_code() {
            eprintln!("[novella-lsp] 收到 exit，退出码 {code}");
            return Ok(code);
        }
    }

    // 客户端关闭了输入流（正常终止路径之一）
    eprintln!("[novella-lsp] 输入流结束，服务退出");
    Ok(0)
}

//! `novella-lsp` 可执行入口：以 stdio 为传输通道运行语言服务器。
//!
//! 重要：LSP 的 stdout 是协议通道，所有日志必须走 stderr，
//! 否则会破坏 `Content-Length` 报文定界、让客户端解析失败。

use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;

fn main() -> ExitCode {
    // 锁定标准流，避免多次加锁与拷贝
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());

    match novella_lsp::server::run(&mut reader, &mut writer) {
        // 退出码遵循 LSP 规范：经 shutdown 正常退出为 0，异常退出为非 0
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("[novella-lsp] 通信失败：{e}");
            ExitCode::FAILURE
        }
    }
}

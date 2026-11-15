//! 模块路径规范化：模块 = 文件，路径是模块的唯一标识。
//!
//! 两件事收在此处（原先混在 sema 的模块加载里，导致 LSP 只能经 sema 访问）：
//! - [`normalize_path`]：**纯词法**消除 `.` 与 `..`，不触盘；
//! - [`module_key`]：模块唯一键 = 规范化后的绝对路径，优先 `fs::canonicalize`
//!   （解析符号链接），失败退回词法归一化。
//!
//! 二者成对使用：任何「这个文件属于哪个模块」的判断都必须经 [`module_key`]，
//! 否则符号链接 / `..` 写法会让同一文件出现两个模块 id。

use std::path::{Component, Path, PathBuf};

/// 词法消除路径中的 `.` 与 `..`（不触盘）。
///
/// `super:` 导入的路径含 `..`，必须先归一化才能作为模块唯一键。
/// 无法继续上溯时（相对路径开头）保留 `..`，避免把 `../a` 误归一成 `a`。
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // 无法继续上溯时（相对路径开头）保留 `..`
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 模块唯一键：优先 `fs::canonicalize`（解析符号链接），失败退回词法归一化
/// （文件尚不存在时，如 `didOpen` 新建的未保存文档）。
pub fn module_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::normalize_path;

    #[test]
    fn 词法归一化消除当前目录与上溯() {
        assert_eq!(normalize_path(Path::new("a/./b")), Path::new("a/b"));
        assert_eq!(normalize_path(Path::new("a/x/../b")), Path::new("a/b"));
        assert_eq!(normalize_path(Path::new("./a")), Path::new("a"));
    }

    #[test]
    fn 相对路径开头的上溯被保留() {
        assert_eq!(normalize_path(Path::new("../a")), Path::new("../a"));
        assert_eq!(normalize_path(Path::new("./../a")), Path::new("../a"));
    }
}

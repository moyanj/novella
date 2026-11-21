//! 编译器随包携带的目标平台 runtime；最终链接器只抽取被引用的对象文件。

/// 当前 runtime 的 C ABI 目标，必须与代码生成和最终链接的目标一致。
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";

/// 与编译器同时构建的静态库，不依赖运行时安装目录或开发仓库路径。
pub const STATIC_ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libnovella_rt.a"));

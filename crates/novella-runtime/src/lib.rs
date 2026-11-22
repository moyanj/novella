//! 编译器随包携带的目标平台 runtime；最终链接器只抽取被引用的对象文件。

/// 当前 runtime 的 C ABI 目标，必须与代码生成和最终链接的目标一致。
pub const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";

/// 与编译器同时构建的静态库，不依赖运行时安装目录或开发仓库路径。
pub const STATIC_ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libnovella_rt.a"));

/// 按平台与代码模型选择 ARC 归档；Linux 始终返回原有 hosted 归档。
/// 裸机归档要求最终程序实现 novella_platform_alloc/free/panic。
pub fn managed_archive(bare_metal: bool, kernel: bool) -> &'static [u8] {
    match (bare_metal, kernel) {
        (false, _) => STATIC_ARCHIVE,
        (true, false) => include_bytes!(concat!(env!("OUT_DIR"), "/libnovella_rt-bare-small.a")),
        (true, true) => include_bytes!(concat!(env!("OUT_DIR"), "/libnovella_rt-bare-kernel.a")),
    }
}

/// 返回独立的 memcpy/memmove/memset/memcmp 归档，不包含 ARC 或平台 hooks。
/// hosted 链接默认使用 libc，仅在调用方明确需要时加入此归档。
pub fn support_archive(bare_metal: bool, kernel: bool) -> &'static [u8] {
    match (bare_metal, kernel) {
        (false, _) => include_bytes!(concat!(env!("OUT_DIR"), "/libnovella_support-linux.a")),
        (true, false) => {
            include_bytes!(concat!(env!("OUT_DIR"), "/libnovella_support-bare-small.a"))
        }
        (true, true) => include_bytes!(concat!(
            env!("OUT_DIR"),
            "/libnovella_support-bare-kernel.a"
        )),
    }
}

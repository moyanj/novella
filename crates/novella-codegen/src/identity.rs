//! 查询当前进程实际使用的 LLVM 所属文件，供调用方识别缓存工具链。

use std::io;
use std::path::PathBuf;

/// 返回 LLVM 符号所在模块；静态链接时该模块为编译器自身。
/// 无法取得身份时返回错误，调用方应禁用缓存，而非仅依赖 LLVM 版本号。
pub fn llvm_library_path() -> io::Result<PathBuf> {
    library_path()
}

#[cfg(unix)]
fn library_path() -> io::Result<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    let address = inkwell::llvm_sys::core::LLVMContextCreate as *const () as *const libc::c_void;
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // 地址来自已链接的 LLVM 函数；仅在 dladdr 成功后读取其写入的结构。
    unsafe {
        if libc::dladdr(address, info.as_mut_ptr()) == 0 {
            return Err(io::Error::other("无法定位已加载的 LLVM 模块"));
        }
        let info = info.assume_init();
        if info.dli_fname.is_null() {
            return Err(io::Error::other("已加载的 LLVM 模块没有文件路径"));
        }
        let bytes = CStr::from_ptr(info.dli_fname).to_bytes();
        std::fs::canonicalize(std::ffi::OsStr::from_bytes(bytes))
    }
}

#[cfg(windows)]
fn library_path() -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };

    let address = inkwell::llvm_sys::core::LLVMContextCreate as *const () as *const u16;
    let mut module = std::ptr::null_mut();
    let mut buffer = vec![0u16; 32768];
    // FROM_ADDRESS 将参数解释为符号地址；不改变模块引用计数，也不释放该句柄。
    unsafe {
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address,
            &mut module,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let length = GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32);
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize >= buffer.len() {
            return Err(io::Error::other("LLVM 模块路径超出缓冲区"));
        }
        std::fs::canonicalize(std::ffi::OsString::from_wide(&buffer[..length as usize]))
    }
}

#[cfg(not(any(unix, windows)))]
fn library_path() -> io::Result<PathBuf> {
    Err(io::Error::other("当前宿主平台不支持 LLVM 模块身份查询"))
}

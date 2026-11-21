//! 导入路径解析：导入语句 → 真实文件路径 / 编译器 Exception 根。
//!
//! 纯路径运算，不触发加载；也负责 crate 根（`nov.toml`）的懒查找。
//! 绑定阶段再把文件路径映射为模块 id（[`resolve_import_path`]）。

use std::path::{Path, PathBuf};

use novella_ast::program::ImportItem;
use novella_core::path::module_key;

use super::{FileLoader, ModuleId};

/// 从 `file` 所在目录向上查找最近的 `nov.toml`，返回其所在目录（crate 根）。
pub(crate) fn find_crate_root(loader: &mut dyn FileLoader, file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if loader.exists(&dir.join("nov.toml")) {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// 导入路径解析结果。
pub(crate) enum ResolvedImport {
    /// 解析到文件路径（待加载 / 已加载模块）
    File(PathBuf),
    /// std:exception 的编译器静态 Exception 根
    ExceptionBuiltin,
}

/// 导入绑定阶段的解析结果（文件路径已映射为模块 id）。
pub(crate) enum NsResolution {
    Module(ModuleId),
    ExceptionBuiltin,
}

/// 解析导入语句到目标模块（加载阶段完成后调用；路径→id 映射见
/// [`crate::Sema::module_by_path`]）。错误与加载阶段同源，调用方一般忽略
/// （诊断已在加载阶段报出）。
pub(crate) fn resolve_import_path(
    sema: &crate::Sema,
    module: ModuleId,
    import: &ImportItem,
) -> Result<NsResolution, String> {
    let importer = &sema.modules[module];
    // 以规范化键为基准解析，与加载阶段（`load_modules` 的 importer_dir）一致
    let dir = importer
        .key
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    match resolve_import(
        import,
        &dir,
        importer.crate_root.as_deref(),
        sema.std_root.as_deref(),
        |path| sema.module_by_path.contains_key(&module_key(path)),
    )? {
        ResolvedImport::File(path) => {
            let key = module_key(&path);
            sema.module_by_path
                .get(&key)
                .copied()
                .map(NsResolution::Module)
                .ok_or_else(|| format!("模块文件未加载：{}", path.display()))
        }
        ResolvedImport::ExceptionBuiltin => Ok(NsResolution::ExceptionBuiltin),
    }
}

/// 将导入语句映射到目标（纯路径运算，不触发加载）。
///
/// `crate_root` 是导入方的 crate 根（懒查找 nov.toml）。
/// 文件候选优先于目录下的 `lib.nov`；加载阶段通过加载器检查存在性，
/// 绑定阶段只查询已加载模块，避免绕过内存 overlay 或重新观察磁盘。
pub(crate) fn resolve_import(
    import: &ImportItem,
    importer_dir: &Path,
    crate_root: Option<&Path>,
    std_root: Option<&Path>,
    mut exists: impl FnMut(&Path) -> bool,
) -> Result<ResolvedImport, String> {
    // `:` 仅作别名分隔符且最多一个：parser 已在第一个 `:` 处切分，
    // path 内残留的 `:` 即第二个分隔符
    if import.path.contains(':') {
        return Err("路径段分隔符只支持 `/`（`:` 仅作别名分隔符，最多一个）".to_string());
    }
    let segments: Vec<&str> = import.path.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(format!("非法模块路径 \"{}\"（含空段）", import.raw_path));
    }

    if segments.iter().any(|s| matches!(*s, "." | "..")) {
        return Err("模块路径不允许 . 或 .. 路径段".to_owned());
    }

    let mut base = match import.prefix.as_deref() {
        None => importer_dir.to_path_buf(),
        Some("@") => match crate_root {
            Some(root) => root.to_path_buf(),
            None => return Err("未找到 nov.toml（crate 根），无法解析 `@:` 导入".to_string()),
        },
        Some("super") => match importer_dir.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return Err("`super:` 导入已超出文件系统根".to_string()),
        },
        Some("std") if import.path == "exception" => return Ok(ResolvedImport::ExceptionBuiltin),
        Some("std") => std_root
            .ok_or_else(|| {
                format!(
                    "标准库模块 `{}` 缺少声明：请配置 std_root，不能跳过类型检查",
                    import.path
                )
            })?
            .to_path_buf(),
        Some(other) => return Err(format!("未知路径别名 `{other}`")),
    };
    for segment in &segments {
        base.push(segment);
    }
    base.set_extension("nov");
    if !exists(&base) {
        base.pop();
        base.push(segments[segments.len() - 1]);
        base.push("lib");
        base.set_extension("nov");
        if !exists(&base) {
            return Err(format!("模块文件不存在：{}", base.display()));
        }
    }
    Ok(ResolvedImport::File(base))
}

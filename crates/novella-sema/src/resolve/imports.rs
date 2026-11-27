//! 具名和命名空间导入绑定。
use super::*;
use crate::symbol::ScopeKind;
// ---------- Phase 2a：导入绑定 ----------

/// 绑定一个模块的全部导入符号到其全局作用域（单次尝试，可重复执行）。
///
/// 返回 `true` 表示该模块本轮有导入因「目标尚未就绪」而跳过：目标模块的全局
/// 作用域在 Phase 1 已建立，但其中的具名导入要等它自己的 [`bind_imports`] 执行
/// 后才可见，而模块下标顺序不代表依赖顺序（`pub` 重导出可任意成链）。调用方
/// 据此进入下一轮（见 `analysis.rs` Phase 2a 的轮转），直到没有模块再等待。
///
/// 中间轮的失败一律只登记为待定，不产生诊断：真正的缺失（拼错、目标模块没有
/// 该符号）与非 `pub` 目标由收敛后的 [`report_unresolved_imports`] 统一报告，
/// 避免「还没轮到」被误判成「不存在」。
///
/// 已绑定的名字直接跳过，因此重复执行是幂等的。
pub(crate) fn bind_imports(sema: &mut Sema, module: ModuleId) -> bool {
    if sema.modules[module].syntax_failed {
        return false;
    }
    // 本轮跳过、留给后续轮次的具名导入。只用于判断「是否还需继续轮转」，
    // 具体诊断在收敛后由 `report_unresolved_imports` 依最终状态给出。
    let mut deferred = false;
    let scope = sema.modules[module].global_scope;
    let imports: Vec<novella_ast::program::ImportItem> = sema.modules[module]
        .program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Import(import) => Some(import.clone()),
            _ => None,
        })
        .collect();

    for import in &imports {
        // 路径解析错误已在加载阶段报过，此处跳过
        let Ok(resolution) = resolve_import_path(sema, module, import) else {
            continue;
        };
        let target = match resolution {
            NsResolution::Module(target) => {
                if sema.modules[target].syntax_failed && !sema.modules[target].load_failed {
                    sema.warning(
                        module,
                        import.span,
                        format!(
                            "所导入的模块 '{}' 存在语法错误，已跳过其语义检查",
                            sema.modules[target].path.display()
                        ),
                    );
                }
                NsTarget::Module(target)
            }
            NsResolution::ExceptionBuiltin => NsTarget::ExceptionBuiltin,
        };

        match &import.kind {
            ImportKind::Namespace { local_alias, .. } => {
                if import.prefix.as_deref() == Some("std") && import.path == "exception" {
                    sema.exception_imports.insert((module, local_alias.clone()));
                }
                if sema.scopes.lookup_local(scope, local_alias).is_some() {
                    continue;
                }
                let symbol = Symbol {
                    name: local_alias.clone(),
                    kind: SymbolKind::NamespaceImport { target },
                    // 可见性来自导入声明的 `pub` / `pkg` 前缀（None = 不重导出）
                    visibility: import.visibility,
                    span: import.span,
                    name_span: import.span,
                    module,
                };
                define_import(sema, module, scope, symbol);
            }
            ImportKind::Named { specifiers, .. } => {
                for specifier in specifiers {
                    if matches!(target, NsTarget::ExceptionBuiltin)
                        && specifier.imported != "Exception"
                    {
                        sema.error(
                            module,
                            specifier.span,
                            format!(
                                "std:exception 中尚未声明符号 `{}`，不能跳过类型检查",
                                specifier.imported
                            ),
                        );
                        continue;
                    }
                    let local = specifier
                        .local
                        .clone()
                        .unwrap_or_else(|| specifier.imported.clone());
                    if import.prefix.as_deref() == Some("std")
                        && import.path == "exception"
                        && specifier.imported == "Exception"
                    {
                        sema.exception_imports.insert((module, local.clone()));
                    }
                    // 文件读取失败时仍绑定本地名字，供使用点以 Ty::Error 恢复。
                    if let NsTarget::Module(target_module) = target
                        && !sema.modules[target_module].load_failed
                    {
                        match sema.modules[target_module]
                            .global_scope_symbol(&sema.scopes, &specifier.imported)
                        {
                            None => {
                                // 目标模块可能尚未执行自己的导入绑定（模块下标顺序
                                // 不代表依赖顺序）：留到下一轮，只有轮转收敛后仍
                                // 查不到才算真的缺失
                                deferred = true;
                                continue;
                            }
                            Some(symbol) if !symbol.is_importable() => {
                                // 目标非 `pub`：若它本身是尚未绑定的具名导入
                                // （仍在等待自己那一跳），下一轮可能变可见，先跳过
                                if matches!(symbol.kind, SymbolKind::NamedImport { .. }) {
                                    deferred = true;
                                    continue;
                                }
                                sema.error(
                                    module,
                                    specifier.span,
                                    format!("符号 `{}` 不是 `pub`，无法导入", specifier.imported),
                                );
                                continue;
                            }
                            // 目标可见：正常绑定，本轮不再等待
                            Some(_) => {}
                        }
                    }
                    let target_module = match target {
                        NsTarget::Module(m) => Some(m),
                        NsTarget::ExceptionBuiltin => None,
                    };
                    // 已绑定过（前几轮已成功绑定）：跳过而不是重复定义
                    {
                        let local_name = specifier
                            .local
                            .clone()
                            .unwrap_or_else(|| specifier.imported.clone());
                        if sema.scopes.lookup_local(scope, &local_name).is_some() {
                            continue;
                        }
                    }
                    let symbol = Symbol {
                        name: local,
                        kind: SymbolKind::NamedImport {
                            target: target_module,
                            imported: specifier.imported.clone(),
                        },
                        // 可见性来自导入声明的 `pub` / `pkg` 前缀（None = 不重导出）
                        visibility: import.visibility,
                        span: specifier.span,
                        name_span: specifier.span,
                        module,
                    };
                    define_import(sema, module, scope, symbol);
                }
            }
        }
    }
    deferred
}

/// 轮转收敛后复核仍有导入未绑定成功的模块：仍缺失的符号在此报错。
///
/// 这样「目标模块还没轮到绑定」与「目标模块根本没有该符号」不会互相污染，
/// 中间轮次静默跳过，只有最终状态才决定诊断。
pub(crate) fn report_unresolved_imports(sema: &mut Sema) {
    for module in 0..sema.modules.len() {
        if sema.modules[module].syntax_failed {
            continue;
        }
        let scope = sema.modules[module].global_scope;
        let imports: Vec<novella_ast::program::ImportItem> = sema.modules[module]
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Import(import) => Some(import.clone()),
                _ => None,
            })
            .collect();
        for import in &imports {
            let Ok(resolution) = resolve_import_path(sema, module, import) else {
                continue;
            };
            let NsResolution::Module(target_module) = resolution else {
                continue;
            };
            if sema.modules[target_module].load_failed {
                continue;
            }
            let ImportKind::Named { specifiers, .. } = &import.kind else {
                continue;
            };
            for specifier in specifiers {
                let local = specifier
                    .local
                    .clone()
                    .unwrap_or_else(|| specifier.imported.clone());
                // 已绑定则无事可报
                if sema.scopes.lookup_local(scope, &local).is_some() {
                    continue;
                }
                match sema.modules[target_module]
                    .global_scope_symbol(&sema.scopes, &specifier.imported)
                {
                    None => sema.error(
                        module,
                        specifier.span,
                        format!(
                            "模块 '{}' 中不存在符号 `{}`",
                            sema.modules[target_module].path.display(),
                            specifier.imported
                        ),
                    ),
                    Some(symbol) if !symbol.is_importable() => sema.error(
                        module,
                        specifier.span,
                        format!("符号 `{}` 不是 `pub`，无法导入", specifier.imported),
                    ),
                    Some(_) => {}
                }
            }
        }
    }
}

/// 把 `std:site` 的绑定作为父作用域注入其他模块；模块自身保持独立，避免自引用。
pub(crate) fn bind_site_imports(sema: &mut Sema) {
    let Some(std_root) = sema.std_root.as_deref() else {
        return;
    };
    let site_path = std_root.join("site.nov");
    let site_key = novella_core::path::module_key(&site_path);
    let Some(&site_module) = sema.module_by_path.get(&site_key) else {
        return;
    };
    if sema.modules[site_module].load_failed || sema.modules[site_module].syntax_failed {
        return;
    }

    let site_scope = sema.modules[site_module].global_scope;
    let symbols: Vec<Symbol> = sema.scopes.scopes[site_scope]
        .symbols
        .values()
        .cloned()
        .collect();
    let prelude = sema.scopes.new_scope(None, ScopeKind::Module);
    for symbol in symbols {
        if sema.scopes.lookup_local(prelude, &symbol.name).is_none() {
            let _ = sema.scopes.define(prelude, symbol);
        }
    }
    for module in 0..sema.modules.len() {
        if module != site_module
            && !sema.modules[module]
                .key
                .starts_with(novella_core::path::module_key(std_root))
        {
            sema.scopes
                .set_parent(sema.modules[module].global_scope, Some(prelude));
        }
    }
}

/// 定义导入符号（冲突诊断措辞与本地定义区分）。
fn define_import(sema: &mut Sema, module: ModuleId, scope: ScopeId, symbol: Symbol) {
    let name = symbol.name.clone();
    let span = symbol.span;
    if sema.scopes.define(scope, symbol).is_err() {
        sema.error(
            module,
            span,
            format!("导入绑定名 `{name}` 与本模块符号冲突"),
        );
    }
}

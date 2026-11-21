//! 具名和命名空间导入绑定。
use super::*;
// ---------- Phase 2a：导入绑定 ----------

/// 绑定一个模块的全部导入符号到其全局作用域。
pub(crate) fn bind_imports(sema: &mut Sema, module: ModuleId) {
    if sema.modules[module].syntax_failed {
        return;
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
                let symbol = Symbol {
                    name: local_alias.clone(),
                    kind: SymbolKind::NamespaceImport { target },
                    visibility: None,
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
                    // 具名导入的存在性 / 可见性检查（std 外部符号不查）
                    if let NsTarget::Module(target_module) = target {
                        match sema.modules[target_module]
                            .global_scope_symbol(&sema.scopes, &specifier.imported)
                        {
                            None => {
                                sema.error(
                                    module,
                                    specifier.span,
                                    format!(
                                        "模块 '{}' 中不存在符号 `{}`",
                                        sema.modules[target_module].path.display(),
                                        specifier.imported
                                    ),
                                );
                                continue;
                            }
                            Some(symbol) if !symbol.is_importable() => {
                                sema.error(
                                    module,
                                    specifier.span,
                                    format!("符号 `{}` 不是 `pub`，无法导入", specifier.imported),
                                );
                                continue;
                            }
                            Some(_) => {}
                        }
                    }
                    let target_module = match target {
                        NsTarget::Module(m) => Some(m),
                        NsTarget::ExceptionBuiltin => None,
                    };
                    let symbol = Symbol {
                        name: local,
                        kind: SymbolKind::NamedImport {
                            target: target_module,
                            imported: specifier.imported.clone(),
                        },
                        visibility: None,
                        span: specifier.span,
                        name_span: specifier.span,
                        module,
                    };
                    define_import(sema, module, scope, symbol);
                }
            }
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

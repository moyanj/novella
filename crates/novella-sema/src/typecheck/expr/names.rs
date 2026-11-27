//! 值与类型符号查询。
use super::*;

use crate::resolve::{ExportTarget, resolve_exported};

/// 标识符：查作用域链，取符号的值类型。
pub(crate) fn check_identifier(sema: &mut Sema, ctx: &mut BodyCtx, name: &str, span: Span) -> Ty {
    let Some(symbol) = sema.scopes.lookup(ctx.scope, name).cloned() else {
        sema.type_error(ctx.module, span, format!("未定义的标识符 `{name}`"));
        return Ty::Error;
    };
    sema.record_reference(ctx.module, span, &symbol);
    if ctx.reachable && ctx.uninitialized.contains(&symbol.name_span) {
        sema.type_error(
            ctx.module,
            span,
            format!("局部变量 `{name}` 可能尚未初始化"),
        );
    }
    value_type_of_symbol(sema, ctx, symbol, name, span)
}
/// 符号 → 值类型（供标识符/导入成员访问共用）。
pub(crate) fn value_type_of_symbol(
    sema: &mut Sema,
    ctx: &mut BodyCtx,
    symbol: Symbol,
    name: &str,
    span: Span,
) -> Ty {
    match symbol.kind {
        SymbolKind::Variable { ty, .. } => {
            if ty.is_none() {
                crate::check::globals::resolve_name(sema, symbol.module, &symbol.name);
                return sema
                    .global_scope_symbol(symbol.module, &symbol.name)
                    .and_then(|s| match s.kind {
                        SymbolKind::Variable { ty, .. } => ty,
                        _ => None,
                    })
                    .unwrap_or(Ty::Error);
            }
            ty.unwrap_or(Ty::Error)
        }
        SymbolKind::Param { ty } => ty,
        SymbolKind::Function { mut sig } => {
            if let Some(name) = &sig.name
                && crate::check::returns::resolve(sema, sig.module, name)
                && let Some(Symbol {
                    kind: SymbolKind::Function { sig: resolved },
                    ..
                }) = sema.global_scope_symbol(sig.module, name)
            {
                sig = resolved;
            }
            if ctx.destructor_self.is_some() && sig.is_method {
                sema.type_error(
                    ctx.module,
                    span,
                    "析构接收者不能调用或提取自身实例方法；该方法可能间接建立 self 的强引用",
                );
            }
            Ty::Function(Rc::new(sig))
        }
        SymbolKind::NamedImport { target, imported } => match target {
            Some(target_module) => {
                // 沿再导出链取终极定义（`pub` 重导入可多层，禁止只读一跳）
                match resolve_exported(sema, target_module, &imported) {
                    Ok(ExportTarget::Symbol(target_symbol)) => {
                        value_type_of_symbol(sema, ctx, *target_symbol, name, span)
                    }
                    // 链末端才暴露成环 / 非 pub / 缺失，诊断落在使用点
                    Err(error) => {
                        sema.type_error(ctx.module, span, error.message());
                        Ty::Error
                    }
                    Ok(_) => Ty::Error, // 命名空间 / Exception 根在绑定阶段已拒绝作值使用
                }
            }
            None => {
                sema.type_error(
                    ctx.module,
                    span,
                    format!("`{name}` 是标准库内建类型，不能作为普通值使用"),
                );
                Ty::Error
            }
        },
        SymbolKind::NamespaceImport { .. } => {
            sema.type_error(
                ctx.module,
                span,
                format!("命名空间 `{name}` 不能作为值使用，请用 `{name}.成员`"),
            );
            Ty::Error
        }
        SymbolKind::Type => {
            sema.type_error(ctx.module, span, format!("类型 `{name}` 不能作为值使用"));
            Ty::Error
        }
    }
}
/// 将「类型位置的标识符」解析为注册表键（具名导入沿再导出链转发到原始定义）。
pub(crate) fn type_key_of_name(
    sema: &Sema,
    scope: ScopeId,
    name: &str,
) -> Option<(ModuleId, String)> {
    match sema.scopes.lookup(scope, name)? {
        Symbol {
            kind: SymbolKind::Type,
            module,
            ..
        } => Some((*module, name.to_string())),
        Symbol {
            kind:
                SymbolKind::NamedImport {
                    target: Some(target),
                    imported,
                },
            ..
        } => match resolve_exported(sema, *target, imported) {
            Ok(ExportTarget::Symbol(symbol)) if matches!(symbol.kind, SymbolKind::Type) => {
                Some((symbol.module, symbol.name.clone()))
            }
            _ => None,
        },
        _ => None,
    }
}
/// 判定标识符指向的符号种类（成员访问/赋值的特殊路径用）。
pub(crate) enum IdentTarget {
    Value,
    Type,
    Namespace(NsTarget),
}
pub(crate) fn classify_identifier(sema: &Sema, ctx: &BodyCtx, name: &str) -> Option<IdentTarget> {
    let symbol = sema.scopes.lookup(ctx.scope, name)?.clone();
    Some(match symbol.kind {
        SymbolKind::Type => IdentTarget::Type,
        SymbolKind::NamespaceImport { target } => IdentTarget::Namespace(target),
        SymbolKind::NamedImport {
            target: Some(target_module),
            ref imported,
        } => {
            // 具名导入的类型也可用于静态成员路径（沿再导出链判定终极种类）
            match resolve_exported(sema, target_module, imported) {
                Ok(ExportTarget::Symbol(symbol)) if matches!(symbol.kind, SymbolKind::Type) => {
                    IdentTarget::Type
                }
                Ok(ExportTarget::Namespace(target)) => IdentTarget::Namespace(target),
                _ => IdentTarget::Value,
            }
        }
        SymbolKind::NamedImport { target: None, .. } => IdentTarget::Value,
        _ => IdentTarget::Value,
    })
}

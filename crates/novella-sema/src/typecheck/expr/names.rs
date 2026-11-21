//! 值与类型符号查询。
use super::*;

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
        SymbolKind::Function { sig } => {
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
                // 沿导入链取目标符号（不支持再导出链，直接取目标模块全局符号）
                match sema.global_scope_symbol(target_module, &imported) {
                    Some(target_symbol) => {
                        value_type_of_symbol(sema, ctx, target_symbol, name, span)
                    }
                    None => Ty::Error, // 绑定阶段已报过诊断
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
/// 将「类型位置的标识符」解析为注册表键（具名导入转发到目标模块的原始名）。
pub(crate) fn type_key_of_name(
    sema: &Sema,
    scope: ScopeId,
    name: &str,
) -> Option<(ModuleId, String)> {
    match sema.scopes.lookup(scope, name) {
        Some(Symbol {
            kind: SymbolKind::Type,
            module,
            ..
        }) => Some((*module, name.to_string())),
        Some(Symbol {
            kind:
                SymbolKind::NamedImport {
                    target: Some(target),
                    imported,
                },
            ..
        }) => Some((*target, imported.clone())),
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
            target,
            ref imported,
        } => {
            // 具名导入的类型也可用于静态成员路径
            if let Some(target_module) = target
                && matches!(
                    sema.global_scope_symbol(target_module, imported),
                    Some(Symbol {
                        kind: SymbolKind::Type,
                        ..
                    })
                )
            {
                return Some(IdentTarget::Type);
            }
            IdentTarget::Value
        }
        _ => IdentTarget::Value,
    })
}

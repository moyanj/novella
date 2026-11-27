//! 目标相关的入口、C 导出边界与无运行时约束。
use std::collections::{HashMap, HashSet};

use novella_ast::program::Item;
use novella_core::{Span, target::RuntimeMode};

use crate::{
    ModuleId, Sema,
    ty::{Ty, TypeDef},
};

/// 按值保存托管引用的类型需要 ARC；裸指针与函数指针只保存地址。
pub(crate) fn managed(sema: &Sema, ty: &Ty) -> bool {
    fn visit(sema: &Sema, ty: &Ty, seen: &mut HashSet<(ModuleId, String)>) -> bool {
        match ty {
            Ty::Class(..) | Ty::Trait(..) | Ty::Future(_) => true,
            Ty::Array(element, length) => *length != 0 && visit(sema, element, seen),
            Ty::Tuple(items) | Ty::Union(items) => items.iter().any(|ty| visit(sema, ty, seen)),
            Ty::Object(items) => items.values().any(|ty| visit(sema, ty, seen)),
            Ty::Applied(base, _) => {
                let substitution = crate::ty::instance_substitution(sema, ty);
                if let Ty::Struct(module, name) = base.as_ref() {
                    if !seen.insert((*module, name.clone())) {
                        return false;
                    }
                    matches!(sema.type_defs.get(&(*module, name.clone())), Some(TypeDef::Struct(def)) if def.fields.iter().filter(|f| !f.is_static).any(|f| f.is_weak || visit(sema, &crate::ty::substitute_type_params(&f.ty, &substitution), seen)))
                } else {
                    visit(sema, base, seen)
                }
            }
            Ty::Struct(module, name) => {
                if !seen.insert((*module, name.clone())) {
                    return false;
                }
                matches!(sema.type_defs.get(&(*module, name.clone())), Some(TypeDef::Struct(def)) if def.fields.iter().filter(|f| !f.is_static).any(|f| f.is_weak || visit(sema, &f.ty, seen)))
            }
            _ => false,
        }
    }
    visit(sema, ty, &mut HashSet::new())
}

fn floating(sema: &Sema, ty: &Ty, seen: &mut HashSet<(ModuleId, String)>) -> bool {
    match ty {
        Ty::Float(_) | Ty::FloatLiteral(_) => true,
        Ty::Pointer(inner) | Ty::Array(inner, _) | Ty::Future(inner) => floating(sema, inner, seen),
        Ty::Tuple(items) | Ty::Union(items) => items.iter().any(|ty| floating(sema, ty, seen)),
        Ty::Object(items) => items.values().any(|ty| floating(sema, ty, seen)),
        Ty::Applied(base, args) => {
            floating(sema, base, seen) || args.iter().any(|ty| floating(sema, ty, seen))
        }
        Ty::Function(sig) => {
            sig.params.iter().any(|p| floating(sema, &p.ty, seen)) || floating(sema, &sig.ret, seen)
        }
        Ty::Struct(module, name) | Ty::Class(module, name) => {
            if !seen.insert((*module, name.clone())) {
                return false;
            }
            let fields = match sema.type_defs.get(&(*module, name.clone())) {
                Some(TypeDef::Struct(def)) => &def.fields,
                Some(TypeDef::Class(def)) => &def.fields,
                _ => return false,
            };
            fields.iter().any(|f| floating(sema, &f.ty, seen))
        }
        _ => false,
    }
}

fn c_abi(ty: &Ty, returns: bool) -> bool {
    match ty {
        Ty::Void => returns,
        Ty::Boolean | Ty::Int(_) | Ty::Float(_) => true,
        Ty::Pointer(inner) => !matches!(
            inner.as_ref(),
            Ty::Class(..) | Ty::Trait(..) | Ty::Future(_)
        ),
        Ty::Function(sig) => {
            !sig.is_async
                && sig.type_param_count == 0
                && !sig.is_method
                && sig.params.iter().all(|p| c_abi(&p.ty, false))
                && c_abi(&sig.ret, true)
        }
        _ => false,
    }
}

/// 只有有体的可调用项才会实际建立参数槽位；struct 的 self 则传递借用地址。
pub(crate) fn check_body_signature(
    sema: &mut Sema,
    module: ModuleId,
    span: Span,
    sig: &crate::ty::FunctionSig,
) {
    let owns_managed = sig.params.iter().enumerate().any(|(index, param)| {
        !(sig.is_method && index == 0 && param.name == "self" && matches!(param.ty, Ty::Struct(..)))
            && managed(sema, &param.ty)
    }) || managed(sema, &sig.ret);
    if sema.build_target.runtime == RuntimeMode::None && owns_managed {
        sema.type_error(
            module,
            span,
            "无运行时模式不能定义需要 ARC 的函数参数或返回值",
        );
    }
}

pub(crate) fn validate(sema: &mut Sema) {
    if let Err(message) = sema.build_spec.validate() {
        sema.global_diagnostics
            .push(novella_core::diagnostic::Diagnostic::error(message));
    }
    if let Err(message) = sema.build_target.validate() {
        sema.global_diagnostics
            .push(novella_core::diagnostic::Diagnostic::error(message));
    }
    // 裸机约束优先读目标描述：requires-no-std / is-like-bare 是描述的一部分，
    // 内置目标两者与旧枚举一致，自定义目标也能表达同样的约束。
    let bare = sema.build_spec.is_like_bare;
    if bare && !sema.module_only && (!sema.no_std || !sema.no_main) && !sema.modules.is_empty() {
        sema.type_error(
            0,
            Span { start: 0, end: 0 },
            "裸机目标必须在入口模块声明 @no_std; 和 @no_main;",
        );
    }
    let mut symbols = HashMap::new();
    // extern 的链接名字是全程序命名空间，先收集后检查 export，避免声明顺序影响结果。
    for module in &sema.modules {
        for item in &module.program.items {
            if let Item::Function(function) = item
                && function.is_extern
            {
                symbols.insert(function.name.clone(), false);
            }
        }
    }
    let mut no_main_seen = false;
    for module in 0..sema.modules.len() {
        for item in sema.modules[module].program.items.clone() {
            match item {
                Item::Directive(directive) if directive.name == "no_main" => {
                    if module != 0 || !directive.tokens.is_empty() || no_main_seen {
                        sema.type_error(
                            module,
                            directive.span,
                            "@no_main 只允许在入口模块独立声明一次且不接受参数",
                        );
                    }
                    if module == 0 {
                        no_main_seen = true;
                    }
                }
                Item::Function(function) => {
                    for directive in function.directives.iter().filter(|d| d.name == "export") {
                        let symbol = match directive.tokens.as_slice() {
                            [token]
                                if token.slice.starts_with('"') && token.slice.ends_with('"') =>
                            {
                                &token.slice[1..token.slice.len() - 1]
                            }
                            _ => {
                                sema.type_error(
                                    module,
                                    directive.span,
                                    "@export 必须接受唯一字符串符号名：@export(\"symbol\")",
                                );
                                continue;
                            }
                        };
                        if !novella_core::project_config::valid_link_symbol(symbol) {
                            sema.type_error(module, directive.span, "@export 的链接符号名非法");
                            continue;
                        }
                        if function.is_extern || function.is_async || function.block.is_none() {
                            sema.type_error(
                                module,
                                function.span,
                                "@export 只能用于有函数体的非 extern、非 async 顶层函数",
                            );
                            continue;
                        }
                        let valid = matches!(sema.declaration_types.get(&(module, function.span)), Some(Ty::Function(sig)) if sig.type_param_count == 0 && sig.params.iter().all(|p| c_abi(&p.ty, false)) && c_abi(&sig.ret, true));
                        if !valid {
                            sema.type_error(module, function.span, "@export 不允许泛型、聚合或托管类型跨 C ABI；仅支持标量、裸指针、函数指针和 void 返回值");
                            continue;
                        }
                        if symbols.insert(symbol.to_owned(), true).is_some() {
                            sema.type_error(
                                module,
                                directive.span,
                                format!("重复导出或与 extern 声明冲突的符号 `{symbol}`"),
                            );
                        } else if sema
                            .exports
                            .insert((module, function.span), symbol.to_owned())
                            .is_some()
                        {
                            sema.type_error(module, directive.span, "函数不能重复声明 @export");
                        }
                    }
                    if sema.build_target.runtime == RuntimeMode::None
                        && function.block.is_some()
                        && let Some(Ty::Function(sig)) =
                            sema.declaration_types.get(&(module, function.span))
                        && (sig.params.iter().any(|p| managed(sema, &p.ty))
                            || managed(sema, &sig.ret))
                    {
                        sema.type_error(
                            module,
                            function.span,
                            "无运行时模式不能定义需要 ARC 的函数参数或返回值",
                        );
                    }
                }
                _ => {}
            }
        }
    }
    // 类型声明本身不引入运行时依赖；实际表达式与变量槽位才产生托管操作。
    let mut violations = HashSet::new();
    for table in [
        &sema.expression_types,
        &sema.annotation_types,
        &sema.declaration_types,
    ] {
        for ((module, span), ty) in table {
            if !sema.build_spec.has_float && floating(sema, ty, &mut HashSet::new()) {
                violations.insert((
                    *module,
                    *span,
                    "裸机目标暂不支持浮点类型或浮点运算（包括 extern 签名）",
                ));
            }
        }
    }
    if sema.build_target.runtime == RuntimeMode::None {
        for table in [&sema.expression_types, &sema.declaration_types] {
            for ((module, span), ty) in table {
                if managed(sema, ty) {
                    violations.insert((
                        *module,
                        *span,
                        "无运行时模式不支持托管引用、ARC 或 weak 操作",
                    ));
                }
            }
        }
    }
    let mut violations: Vec<_> = violations.into_iter().collect();
    violations.sort_by_key(|(module, span, _)| (*module, span.start, span.end));
    for (module, span, message) in violations {
        sema.type_error(module, span, message);
    }
}

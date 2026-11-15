//! Phase 2b 类型注册表与 Phase 2c class 继承链。
//!
//! - [`build_type_defs`]：解析字段 / 方法 / 函数签名，填充类型注册表；
//! - [`resolve_class_parents`]：解析 class 继承链并检测继承环。

use std::collections::{HashMap, HashSet};

use novella_ast::program::{ClassMember, FieldMember, FunctionItem, Item, MethodMember};
use novella_core::Span;

use crate::Sema;
use crate::module::ModuleId;
use crate::symbol::SymbolKind;
use crate::ty::{FieldDef, MethodSig, Ty, TypeDef};

use super::CLASS_SPECIAL_METHODS;

/// 构建一个模块的函数签名与 struct / class / trait 成员表。
pub(crate) fn build_type_defs(sema: &mut Sema, module: ModuleId) {
    if sema.modules[module].syntax_failed {
        return;
    }
    let global_scope = sema.modules[module].global_scope;
    let items = sema.modules[module].program.items.clone();
    for item in &items {
        match item {
            Item::Function(function) => build_function_sig(sema, module, global_scope, function),
            Item::Class(class) => {
                let env = identity_env(class.type_parameters.iter().map(|p| p.name.as_str()));
                let mut fields = vec![];
                let mut methods = HashMap::new();
                let mut member_names: HashMap<String, Span> = HashMap::new();
                for member in &class.members {
                    match member {
                        ClassMember::Field(field) => {
                            check_member_collision(
                                sema,
                                module,
                                &mut member_names,
                                &field.name,
                                field.span,
                            );
                            fields.push(build_field(sema, module, global_scope, field, &env));
                        }
                        ClassMember::Method(method) => {
                            check_member_collision(
                                sema,
                                module,
                                &mut member_names,
                                &method.name,
                                method.span,
                            );
                            check_special_method_name(sema, module, method, true);
                            let owner = Ty::Class(module, class.name.clone());
                            let sig =
                                build_method_sig(sema, module, global_scope, method, &env, &owner);
                            methods.insert(method.name.clone(), sig);
                        }
                    }
                }
                if class.parent.len() > 1 {
                    sema.decl_error(module, class.span, "多继承暂不支持（仅首个父类生效）");
                }
                if let Some(TypeDef::Class(def)) =
                    sema.type_defs.get_mut(&(module, class.name.clone()))
                {
                    def.fields = fields;
                    def.methods = methods;
                }
            }
            Item::Struct(struct_item) => {
                let env = identity_env(struct_item.type_parameters.iter().map(|p| p.name.as_str()));
                let mut fields = vec![];
                let mut methods = HashMap::new();
                let mut member_names: HashMap<String, Span> = HashMap::new();
                for member in &struct_item.members {
                    match member {
                        ClassMember::Field(field) => {
                            check_member_collision(
                                sema,
                                module,
                                &mut member_names,
                                &field.name,
                                field.span,
                            );
                            fields.push(build_field(sema, module, global_scope, field, &env));
                        }
                        ClassMember::Method(method) => {
                            check_member_collision(
                                sema,
                                module,
                                &mut member_names,
                                &method.name,
                                method.span,
                            );
                            check_special_method_name(sema, module, method, false);
                            let owner = Ty::Struct(module, struct_item.name.clone());
                            let sig =
                                build_method_sig(sema, module, global_scope, method, &env, &owner);
                            methods.insert(method.name.clone(), sig);
                        }
                    }
                }
                // struct 无继承（design.md §4.7；parser 保留 heritage，sema 判定）
                if !struct_item.heritage.is_empty() {
                    sema.decl_error(module, struct_item.span, "struct 不支持继承");
                }
                if let Some(TypeDef::Struct(def)) =
                    sema.type_defs.get_mut(&(module, struct_item.name.clone()))
                {
                    def.fields = fields;
                    def.methods = methods;
                }
            }
            _ => {}
        }
    }
}

/// 泛型形参的恒等映射环境（`T` → `Ty::TypeParam("T")`）。
fn identity_env<'a>(names: impl Iterator<Item = &'a str>) -> HashMap<String, Ty> {
    names
        .map(|name| (name.to_string(), Ty::TypeParam(name.to_string())))
        .collect()
}

/// 成员重名检查（字段与方法共用一个命名空间）。
fn check_member_collision(
    sema: &mut Sema,
    module: ModuleId,
    seen: &mut HashMap<String, Span>,
    name: &str,
    span: Span,
) {
    if seen.insert(name.to_string(), span).is_some() {
        sema.decl_error(module, span, format!("重复的成员 `{name}`"));
    }
}

/// `__` 前缀方法命名检查（design.md §4.6.1 / §4.7）。
fn check_special_method_name(
    sema: &mut Sema,
    module: ModuleId,
    method: &MethodMember,
    is_class: bool,
) {
    if !method.name.starts_with("__") {
        return;
    }
    if is_class {
        if !CLASS_SPECIAL_METHODS.contains(&method.name.as_str()) {
            sema.decl_error(
                module,
                method.span,
                format!(
                    "未知特殊方法 `{}`（已登记：{}）",
                    method.name,
                    CLASS_SPECIAL_METHODS.join(" / ")
                ),
            );
        }
    } else {
        sema.decl_error(
            module,
            method.span,
            format!("struct 不识别 `__` 前缀特殊方法（`{}`）", method.name),
        );
    }
}

/// 解析字段定义（无类型标注且无默认值 → 诊断；无标注有默认值 → 类型在
/// Phase 2d 由初始化表达式推导回填）。
fn build_field(
    sema: &mut Sema,
    module: ModuleId,
    scope: crate::symbol::ScopeId,
    field: &FieldMember,
    env: &HashMap<String, Ty>,
) -> FieldDef {
    let ty = match &field.ty {
        Some(ty) => sema.resolve_type(module, scope, ty, env),
        None => {
            if field.initializer.is_none() {
                sema.decl_error(
                    module,
                    field.span,
                    format!("字段 `{}` 需要类型标注或默认值", field.name),
                );
            }
            Ty::Unknown // 待 2d 推导回填
        }
    };
    FieldDef {
        name: field.name.clone(),
        ty,
        has_default: field.initializer.is_some(),
        is_weak: field.modifiers.is_weak,
        is_static: field.modifiers.is_static,
    }
}

/// 解析方法签名（返回类型缺省为 `void`；方法不支持 async）。
fn build_method_sig(
    sema: &mut Sema,
    module: ModuleId,
    scope: crate::symbol::ScopeId,
    method: &MethodMember,
    env: &HashMap<String, Ty>,
    owner: &Ty,
) -> MethodSig {
    // 方法自身泛型形参叠加进环境
    let mut env = env.clone();
    for param in &method.type_parameters {
        env.insert(param.name.clone(), Ty::TypeParam(param.name.clone()));
    }
    check_duplicate_params(sema, module, &method.params, method.span);
    let params: Vec<crate::ty::ParamSig> = method
        .params
        .iter()
        .enumerate()
        .map(|(index, p)| crate::ty::ParamSig {
            name: p.name.clone(),
            ty: match &p.ty {
                Some(ty) => sema.resolve_type(module, scope, ty, &env),
                // 实例方法的首个 self 由属主确定，必须在语义阶段完成定型，
                // 使成员访问及下游 expression_types 共用同一类型来源。
                None if index == 0 && p.name == "self" && !method.modifiers.is_static => {
                    owner.clone()
                }
                None => Ty::Unknown,
            },
            has_default: p.default.is_some(),
        })
        .collect();
    let ret = match &method.return_type {
        Some(ty) => sema.resolve_type(module, scope, ty, &env),
        None => Ty::Void,
    };
    MethodSig {
        sig: crate::ty::FunctionSig {
            // 方法与顶层函数共享模块符号空间，以属主隔开同名方法。
            module,
            name: Some(match owner {
                Ty::Struct(_, name) | Ty::Class(_, name) => format!("{name}::{}", method.name),
                _ => unreachable!("方法属主恒为 struct / class"),
            }),
            params,
            ret,
            type_param_count: method.type_parameters.len(),
            is_async: false,
            is_extern: false,
            // 公开投影略去首个 self，并置接收者标记。
            is_method: !method.modifiers.is_static,
        },
        is_static: method.modifiers.is_static,
    }
}

/// 解析顶层函数签名并回填全局符号（extern 规则 E2–E5 一并检查）。
fn build_function_sig(
    sema: &mut Sema,
    module: ModuleId,
    global_scope: crate::symbol::ScopeId,
    function: &FunctionItem,
) {
    let mut sig = match &function.r#type {
        Some(function_type) => {
            check_duplicate_params(sema, module, &function_type.params, function.span);
            sema.resolve_function_type(module, global_scope, function_type, &HashMap::new())
        }
        // 防御：parser 恒产出签名，None 仅见于异常恢复路径
        None => crate::ty::FunctionSig {
            module,
            name: None,
            params: vec![],
            ret: Ty::Void,
            type_param_count: 0,
            is_async: false,
            is_extern: false,
            is_method: false,
        },
    };
    sig.is_async = function.is_async;
    sig.is_extern = function.is_extern;
    // 回填名义键（`resolve_function_type` 不知道函数名，此处补上）
    sig.module = module;
    sig.name = Some(function.name.clone());
    sema.declaration_types.insert(
        (module, function.span),
        Ty::Function(std::rc::Rc::new(sig.clone())),
    );

    if function.is_extern {
        // E2：extern 参数必须显式标注类型（FFI 边界无法推导）。
        // 注：返回类型在 AST 层无法区分「显式 void」与「缺省补 void」（parser
        // 统一回填），故返回标注检查暂不落地（见 docs/sema-phase1.md §6.1）
        if let Some(function_type) = &function.r#type {
            for param in &function_type.params {
                if param.ty.is_none() {
                    sema.decl_error(
                        module,
                        param.span,
                        format!("extern 函数参数 `{}` 必须显式标注类型", param.name),
                    );
                }
            }
        }
        // E3：extern 不得有函数体（文法已强制 `;` 结尾，防御性复查）
        if function.block.is_some() {
            sema.decl_error(module, function.span, "extern 函数不能有函数体");
        }
        // E4：extern 不支持泛型（FFI 边界无单态化）
        if sig.type_param_count > 0 {
            sema.decl_error(module, function.span, "extern 函数暂不支持泛型参数");
        }
        // E5：入口必须有函数体
        if function.name == "main" {
            sema.decl_error(module, function.span, "`main` 不能声明为 extern");
        }
    }

    if let Some(symbol) = sema.scopes.lookup_mut(global_scope, &function.name) {
        symbol.kind = SymbolKind::Function { sig };
    }
}

/// 形参重名检查。
fn check_duplicate_params(
    sema: &mut Sema,
    module: ModuleId,
    params: &[novella_ast::types::Param],
    _span: Span,
) {
    let mut seen: HashMap<&str, Span> = HashMap::new();
    for param in params {
        if seen.insert(param.name.as_str(), param.span).is_some() {
            sema.decl_error(module, param.span, format!("重复的形参 `{}`", param.name));
        }
    }
}

/// 解析全部 class 的父类引用并检测继承环。
pub(crate) fn resolve_class_parents(sema: &mut Sema) {
    let class_keys: Vec<(ModuleId, String)> = sema
        .type_defs
        .iter()
        .filter(|(_, def)| matches!(def, TypeDef::Class(_)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in class_keys {
        let (module, parent) = match sema.type_defs.get(&key) {
            Some(TypeDef::Class(def)) => (key.0, def.parent.clone()),
            _ => continue,
        };
        let Some(parent_ty_expr) = parent else {
            continue;
        };
        let scope = sema.modules[module].global_scope;
        let span = parent_ty_expr.span();
        let ty = sema.resolve_type(module, scope, &parent_ty_expr, &HashMap::new());
        match ty {
            Ty::Class(pm, pn) => {
                if let Some(TypeDef::Class(def)) = sema.type_defs.get_mut(&key) {
                    def.parent_resolved = Some((pm, pn));
                }
            }
            Ty::Unknown => {} // 诊断已报
            other => sema.decl_error(
                module,
                span,
                format!("父类必须是 class 类型，实为 `{other}`"),
            ),
        }
    }
    // 继承环检测：沿 parent_resolved 链走访，回到起点即成环
    let class_keys: Vec<(ModuleId, String)> = sema
        .type_defs
        .iter()
        .filter(|(_, def)| matches!(def, TypeDef::Class(_)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in class_keys {
        let mut visited = HashSet::new();
        let mut current = key.clone();
        loop {
            if !visited.insert(current.clone()) {
                // 类定义节点 span 未入注册表，环诊断退化到文件头
                let span = Span { start: 0, end: 0 };
                sema.decl_error(key.0, span, format!("class `{}` 的继承链存在循环", key.1));
                break;
            }
            match sema.type_defs.get(&current) {
                Some(TypeDef::Class(def)) => match &def.parent_resolved {
                    Some(parent) => current = parent.clone(),
                    None => break,
                },
                _ => break,
            }
        }
    }
}

//! 函数与成员签名、声明约束。
use super::*;

/// 构建一个模块的函数签名与 struct / class / trait 成员表。
pub(crate) fn build_type_defs(sema: &mut Sema, module: ModuleId) {
    if sema.modules[module].syntax_failed {
        return;
    }
    let global_scope = sema.modules[module].global_scope;
    let items = sema.modules[module].program.items.clone();
    for item in &items {
        match item {
            Item::TypeAlias(alias) => {
                let env = identity_env(module, &alias.type_parameters);
                // 别名定义本身也须检查，未使用的循环/非法类型不能躲过诊断。
                sema.resolve_type(module, global_scope, &alias.alias, &env);
            }
            Item::Function(function) => build_function_sig(sema, module, global_scope, function),
            Item::Trait(trait_item) => {
                let mut env = identity_env(module, &trait_item.type_parameters);
                env.insert(
                    "Self".into(),
                    Ty::TypeParam(crate::traits::self_key(module, &trait_item.name)),
                );
                let mut methods = HashMap::new();
                let mut names = HashMap::new();
                for member in &trait_item.members {
                    let (name, span, directives) = match member {
                        ClassMember::Field(f) => (&f.name, f.name_span, &f.directives),
                        ClassMember::Method(m) => (&m.name, m.name_span, &m.directives),
                        ClassMember::AssociatedType(_) => continue,
                    };
                    check_member_collision(sema, module, &mut names, name, span);
                    for directive in directives {
                        sema.type_error(module, directive.span, "成员上的编译器指令尚未实现");
                    }
                    match member {
                        ClassMember::Field(field) => sema.decl_error(
                            module,
                            field.span,
                            format!("trait `{}` 不能声明字段 `{}`", trait_item.name, field.name),
                        ),
                        ClassMember::Method(method) => {
                            check_special_method_name(sema, module, method, true);
                            let owner = Ty::Trait(module, trait_item.name.clone());
                            let sig =
                                build_method_sig(sema, module, global_scope, method, &env, &owner);
                            sema.declaration_types.insert(
                                (module, method.span),
                                Ty::Function(std::rc::Rc::new(sig.sig.clone())),
                            );
                            methods.insert(method.name.clone(), sig);
                        }
                        ClassMember::AssociatedType(_) => {}
                    }
                }
                if let Some(TypeDef::Trait(def)) =
                    sema.type_defs.get_mut(&(module, trait_item.name.clone()))
                {
                    def.methods = methods;
                }
            }
            Item::Class(class) => {
                let env = identity_env(module, &class.type_parameters);
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
                                method.name.as_str(),
                                method.span,
                            );
                            check_special_method_name(sema, module, method, true);
                            let owner = Ty::Class(module, class.name.clone());
                            let sig =
                                build_method_sig(sema, module, global_scope, method, &env, &owner);
                            methods.insert(method.name.clone(), sig);
                        }
                        ClassMember::AssociatedType(_) => {}
                    }
                }
                if let Some(TypeDef::Class(def)) =
                    sema.type_defs.get_mut(&(module, class.name.clone()))
                {
                    def.fields = fields;
                    def.methods = methods;
                }
            }
            Item::Struct(struct_item) => {
                let env = identity_env(module, &struct_item.type_parameters);
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
                                method.name.as_str(),
                                method.span,
                            );
                            check_special_method_name(sema, module, method, false);
                            let owner = Ty::Struct(module, struct_item.name.clone());
                            let sig =
                                build_method_sig(sema, module, global_scope, method, &env, &owner);
                            methods.insert(method.name.clone(), sig);
                        }
                        ClassMember::AssociatedType(_) => {}
                    }
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
pub(super) fn identity_env(
    module: ModuleId,
    params: &[novella_ast::types::TypeParameter],
) -> HashMap<String, Ty> {
    params
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                Ty::TypeParam(crate::ty::param_key(module, p)),
            )
        })
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
    } else if !matches!(method.name.as_str(), "__getitem" | "__setitem") {
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
        name_span: field.name_span,
        name: field.name.clone(),
        ty,
        has_default: field.initializer.is_some(),
        is_weak: field.modifiers.is_weak,
        is_static: field.modifiers.is_static,
        // D02：缺省可见性为 `pri`（仅声明类型内部以及与声明同模块的代码可见）
        is_public: is_public_member(field.modifiers.scope),
        module,
    }
}

/// D02 可见性归一：`pub` → 公开；缺省 / `pri` / `pkg` → 对声明模块之外不可见。
///
/// `pkg` 与 `pri` 目前同归为一类，因为语言尚未引入包边界（模块 = 文件，crate 根
/// 只有一层），划出独立的包作用域没有可检查的判据。`pri` 可见范围 = 声明类型
/// 内部 + 声明模块；跨模块访问要求成员显式 `pub`。
fn is_public_member(scope: Option<novella_ast::program::Scope>) -> bool {
    matches!(scope, Some(novella_ast::program::Scope::Public))
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
        env.insert(
            param.name.clone(),
            Ty::TypeParam(crate::ty::param_key(module, param)),
        );
    }
    sema.register_bounds(module, scope, &method.type_parameters, &env);
    check_duplicate_params(sema, module, &method.params, method.span);
    let params: Vec<crate::ty::ParamSig> = method
        .params
        .iter()
        .enumerate()
        .map(|(index, p)| crate::ty::ParamSig {
            name: p.name.clone(),
            ty: match &p.ty {
                Some(ty) => sema.resolve_parameter_type(module, scope, ty, &env),
                // 实例方法的首个 self 由属主确定，必须在语义阶段完成定型，
                // 使成员访问及下游 expression_types 共用同一类型来源。
                None if index == 0 && p.name == "self" && !method.modifiers.is_static => {
                    match owner {
                        Ty::Trait(_, _) => env["Self"].clone(),
                        _ => sema.identity_instance(owner.clone()),
                    }
                }
                None => Ty::Unknown,
            },
            has_default: p.default.is_some(),
        })
        .collect();
    let ret = match &method.return_type {
        Some(ty) => sema.resolve_type(module, scope, ty, &env),
        None if method.block.is_some()
            && !crate::check::CLASS_SPECIAL_METHODS.contains(&method.name.as_str()) =>
        {
            Ty::Unknown
        }
        None => Ty::Void,
    };
    for (index, param) in method.params.iter().enumerate() {
        if param.name == "self" && (method.modifiers.is_static || index != 0) {
            sema.decl_error(module, param.span, "self 只能是实例方法的第一个形参");
        }
        let owner_args = sema
            .type_defs
            .get(&crate::typecheck::this_ty_key(owner))
            .map(|def| {
                def.type_params()
                    .iter()
                    .cloned()
                    .map(Ty::TypeParam)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let declared_owner = if owner_args.is_empty() {
            owner.clone()
        } else {
            Ty::Applied(Box::new(owner.clone()), owner_args)
        };
        if param.name == "self"
            && param.ty.is_some()
            && !crate::ty::ty_eq(&params[index].ty, &declared_owner)
            && !env
                .get("Self")
                .is_some_and(|ty| crate::ty::ty_eq(ty, &params[index].ty))
        {
            sema.decl_error(module, param.span, "self 的类型标注必须是方法所属类型");
        }
    }
    let method_name = method.name.as_str();
    if matches!(method_name, "__getitem" | "__setitem") {
        let read = method_name == "__getitem";
        let count = if read { 2 } else { 3 };
        if method.modifiers.is_static
            || !method.type_parameters.is_empty()
            || params.len() != count
            || params.first().is_none_or(|param| param.name != "self")
            || params
                .iter()
                .any(|param| param.has_default || matches!(param.ty, Ty::Unknown | Ty::Void))
            || (read == matches!(ret, Ty::Void))
        {
            sema.decl_error(module, method.span, if read {
                "下标读方法必须是 `func __getitem(self, index: I) -> T`，且不能是 static、泛型或含默认值"
            } else {
                "下标写方法必须是 `func __setitem(self, index: I, value: T) -> void`，且不能是 static、泛型或含默认值"
            });
        }
    }
    let special_ret = match method_name {
        "__init" | "__destroy" | "__enter" | "__exit" => Some(Ty::Void),
        "__str" => Some(sema.string_type().unwrap_or(Ty::Error)),
        _ => None,
    };
    if let Some(expected) = special_ret {
        if !method.type_parameters.is_empty() {
            sema.decl_error(module, method.name_span, "特殊方法不能声明方法类型参数");
        }
        if !crate::ty::ty_eq(&expected, &ret) {
            sema.decl_error(
                module,
                method.span,
                format!("特殊方法 `{}` 必须返回 `{expected}`", method.name),
            );
        }
        if method.modifiers.is_static {
            sema.decl_error(module, method.span, "特殊方法不能是 static");
        }
        let count = params.iter().filter(|p| p.name != "self").count();
        if method.name.as_str() != "__init" && count != 0 {
            sema.decl_error(module, method.span, "该特殊方法不能接受显式实参");
        }
    }
    MethodSig {
        name_span: method.name_span,
        sig: crate::ty::FunctionSig {
            // 方法与顶层函数共享模块符号空间，以属主隔开同名方法。
            module,
            name: Some(match owner {
                // trait 方法也带 `Trait::方法` 前缀，避免与顶层函数同名混淆
                Ty::Struct(_, name) | Ty::Class(_, name) | Ty::Trait(_, name) => {
                    format!("{name}::{}", method.name)
                }
                _ => unreachable!("方法属主恒为 struct / class / trait"),
            }),
            params,
            ret,
            type_param_count: method.type_parameters.len(),
            type_params: method
                .type_parameters
                .iter()
                .map(|p| crate::ty::param_key(module, p))
                .collect(),
            is_async: false,
            is_extern: false,
            // 公开投影略去首个 self，并置接收者标记。
            is_method: !method.modifiers.is_static,
        },
        is_static: method.modifiers.is_static,
        is_public: is_public_member(method.modifiers.scope),
        name: method.name.clone(),
        // D02：只有声明（`;`）没有函数体的方法即抽象方法
        is_abstract: method.block.is_none(),
        span: method.span,
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
            for param in &function_type.params {
                if param.name == "self" {
                    sema.decl_error(module, param.span, "顶层函数不能声明 self 形参");
                }
            }
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
            type_params: vec![],
            is_async: false,
            is_extern: false,
            is_method: false,
        },
    };
    sig.is_async = function.is_async;
    if !function.is_extern
        && function
            .r#type
            .as_ref()
            .is_some_and(|ty| ty.return_type.is_none())
    {
        sig.ret = Ty::Unknown;
    }
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
        // extern 没有可推导的函数体；省略返回注解仍按 void（design.md §4.8.1）。
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

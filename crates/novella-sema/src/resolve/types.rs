//! 类型解析、别名展开与 trait bounds。
use super::*;
// ---------- 类型解析（TypeExpr -> Ty） ----------

impl Sema {
    /// 解析类型表达式为语义类型。
    ///
    /// - `scope`：使用点作用域（类型参数 / 命名空间导入查找沿父链）；
    /// - `type_env`：泛型形参替换环境（函数类型形参按恒等映射为
    ///   [`Ty::TypeParam`]；泛型别名展开时映射为实参类型）。
    pub(crate) fn resolve_type(
        &mut self,
        module: ModuleId,
        scope: ScopeId,
        ty: &TypeExpr,
        type_env: &HashMap<String, Ty>,
    ) -> Ty {
        let resolved = stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
            self.resolve_type_inner(module, scope, ty, type_env)
        });
        self.annotation_types
            .insert((module, ty.span()), resolved.clone());
        resolved
    }

    /// 统一在包装层登记注解类型，避免下游重复名称解析。
    fn resolve_type_inner(
        &mut self,
        module: ModuleId,
        scope: ScopeId,
        ty: &TypeExpr,
        type_env: &HashMap<String, Ty>,
    ) -> Ty {
        match ty {
            TypeExpr::Predefined { ty, .. } => predefined_from_enum(*ty),
            TypeExpr::Reference {
                qualifier,
                name,
                generic_args,
                span,
            } => self.resolve_type_reference(
                module,
                scope,
                qualifier,
                name,
                generic_args,
                *span,
                type_env,
            ),
            TypeExpr::Array { element, .. } => Ty::Array(Box::new(
                self.resolve_type(module, scope, element, type_env),
            )),
            TypeExpr::Pointer { element, .. } => Ty::Pointer(Box::new(
                self.resolve_type(module, scope, element, type_env),
            )),
            TypeExpr::Tuple { elements, .. } => Ty::Tuple(
                elements
                    .iter()
                    .map(|e| self.resolve_type(module, scope, e, type_env))
                    .collect(),
            ),
            TypeExpr::Function(function_type) => {
                let sig = self.resolve_function_type(module, scope, function_type, type_env);
                Ty::Function(Rc::new(sig))
            }
            TypeExpr::Union { types, span } => {
                if types.len() < 2 {
                    self.error(module, *span, "联合类型至少需要两个成员".to_string());
                }
                Ty::Union(
                    types
                        .iter()
                        .map(|t| self.resolve_type(module, scope, t, type_env))
                        .collect(),
                )
            }
        }
    }

    /// 解析类型引用（含限定名 `mod.Type`）。
    #[allow(clippy::too_many_arguments)] // 参数是 resolve_type 上下文的自然拆分
    fn resolve_type_reference(
        &mut self,
        module: ModuleId,
        scope: ScopeId,
        qualifier: &[String],
        name: &str,
        generic_args: &[TypeExpr],
        span: Span,
        type_env: &HashMap<String, Ty>,
    ) -> Ty {
        // 引用绑定只选择末尾类型名，不能覆盖限定路径或泛型实参。
        let span = self.modules[module]
            .tokens
            .iter()
            .filter(|token| {
                token.span.start >= span.start
                    && token.span.end <= span.end
                    && token.kind == novella_core::token::TokenKind::Identifier
            })
            .nth(qualifier.len())
            .map(|token| token.span)
            .unwrap_or(span);
        // 泛型实参单独解析并保留类型身份。
        let args: Vec<Ty> = generic_args
            .iter()
            .map(|a| self.resolve_type(module, scope, a, type_env))
            .collect();

        if qualifier.is_empty() {
            if name == "Future" {
                if args.len() != 1 {
                    self.error(module, span, "Future 需要一个类型实参");
                    return Ty::Error;
                }
                return Ty::Future(Box::new(args[0].clone()));
            }
            // 1) 替换环境（泛型形参 / 别名实参）
            if let Some(ty) = type_env.get(name) {
                if !args.is_empty() {
                    self.error(module, span, "类型参数不能再带泛型实参");
                    return Ty::Error;
                }
                return ty.clone();
            }
            // 2) 预定义类型按名归类（design.md §5.1）
            if let Some(ty) = predefined_from_name(name) {
                if !args.is_empty() {
                    self.error(module, span, format!("预定义类型 `{name}` 不接受泛型实参"));
                }
                return ty;
            }
            // 3) 符号查找
            let Some(symbol) = self.scopes.lookup(scope, name).cloned() else {
                self.error(module, span, format!("未定义的类型 `{name}`"));
                return Ty::Error;
            };
            self.record_reference(module, span, &symbol);
            return self.type_from_symbol(module, span, &symbol, name, args, type_env);
        }

        // 限定名：首段须为命名空间导入绑定；多级限定暂不支持
        if qualifier.len() > 1 {
            self.error(
                module,
                span,
                "暂不支持多级限定名（模块 = 文件，无嵌套模块）".to_string(),
            );
            return Ty::Error;
        }
        let ns = &qualifier[0];
        let Some(symbol) = self.scopes.lookup(scope, ns).cloned() else {
            self.error(
                module,
                span,
                format!("未定义的命名空间 `{ns}`（缺少对应 import）"),
            );
            return Ty::Error;
        };
        let SymbolKind::NamespaceImport { target } = symbol.kind else {
            self.error(module, span, format!("`{ns}` 不是命名空间导入绑定"));
            return Ty::Error;
        };
        match target {
            NsTarget::ExceptionBuiltin => {
                if name == "Exception" && self.exception_imports.contains(&(module, ns.clone())) {
                    if !args.is_empty() {
                        self.error(module, span, "Exception 不接受泛型实参");
                    }
                    crate::ty::exception_type()
                } else {
                    self.error(
                        module,
                        span,
                        format!("std:exception 中尚未声明类型 `{name}`"),
                    );
                    Ty::Error
                }
            }
            NsTarget::Module(target_module) => {
                let Some(target_symbol) = self.modules[target_module]
                    .global_scope_symbol(&self.scopes, name)
                    .cloned()
                else {
                    self.error(module, span, format!("模块中不存在类型 `{name}`"));
                    return Ty::Error;
                };
                self.record_reference(module, span, &target_symbol);
                if !target_symbol.is_importable() {
                    self.error(module, span, format!("类型 `{name}` 不是 `pub`"));
                    return Ty::Error;
                }
                self.type_from_symbol(module, span, &target_symbol, name, args, type_env)
            }
        }
    }

    /// 由类型符号产出 `Ty`（按注册表实体分派；别名就地展开）。
    fn type_from_symbol(
        &mut self,
        use_module: ModuleId,
        span: Span,
        symbol: &Symbol,
        name: &str,
        args: Vec<Ty>,
        type_env: &HashMap<String, Ty>,
    ) -> Ty {
        // 具名导入：跟到目标模块的符号再判定
        if let SymbolKind::NamedImport {
            target: Some(target_module),
            imported,
        } = &symbol.kind
        {
            let key = (*target_module, imported.clone());
            let Some(def) = self.type_defs.get(&key).cloned() else {
                self.error(use_module, span, format!("`{imported}` 不是类型"));
                return Ty::Error;
            };
            return self.type_from_def(use_module, span, key, def, args, type_env);
        }
        if matches!(&symbol.kind, SymbolKind::NamedImport { target: None, imported } if imported == "Exception")
            && self
                .exception_imports
                .contains(&(symbol.module, symbol.name.clone()))
        {
            if !args.is_empty() {
                self.error(use_module, span, "Exception 不接受泛型实参");
            }
            return crate::ty::exception_type();
        }
        if !matches!(symbol.kind, SymbolKind::Type) {
            self.error(use_module, span, format!("`{name}` 不是类型"));
            return Ty::Error;
        }
        let key = (symbol.module, symbol.name.clone());
        let Some(def) = self.type_defs.get(&key).cloned() else {
            self.error(use_module, span, format!("`{name}` 的类型定义缺失"));
            return Ty::Error;
        };
        self.type_from_def(use_module, span, key, def, args, type_env)
    }

    /// 由注册表实体产出 `Ty`（含别名展开与泛型 arity 检查）。
    fn type_from_def(
        &mut self,
        use_module: ModuleId,
        span: Span,
        key: (ModuleId, String),
        def: TypeDef,
        args: Vec<Ty>,
        type_env: &HashMap<String, Ty>,
    ) -> Ty {
        // 泛型 arity 检查（浅泛型：实参不代入定义体，别名除外）
        let expected = def.type_param_count();
        if expected != args.len() && !(args.is_empty() && expected == 0) {
            self.error(
                use_module,
                span,
                format!(
                    "类型 `{}` 需要 {expected} 个泛型实参，实给了 {}",
                    key.1,
                    args.len()
                ),
            );
            return Ty::Error;
        }
        let base = match def {
            TypeDef::Struct(_) => Ty::Struct(key.0, key.1),
            TypeDef::Class(_) => Ty::Class(key.0, key.1),
            TypeDef::Trait(_) => Ty::Trait(key.0, key.1),
            TypeDef::Alias(alias) => {
                return self.expand_alias(use_module, span, key, alias, args, type_env);
            }
        };
        if args.is_empty() {
            base
        } else {
            let ty = Ty::Applied(Box::new(base), args);
            self.generic_uses.insert((use_module, span), ty.clone());
            ty
        }
    }

    /// 展开类型别名（在别名**定义模块**的作用域解析目标；循环展开报错）。
    fn expand_alias(
        &mut self,
        use_module: ModuleId,
        span: Span,
        key: (ModuleId, String),
        alias: AliasDef,
        args: Vec<Ty>,
        _type_env: &HashMap<String, Ty>,
    ) -> Ty {
        if !self.resolving_aliases.insert(key.clone()) {
            self.error(use_module, span, format!("类型别名 `{}` 循环定义", key.1));
            return Ty::Error;
        }
        // 定义位置只登记符号类型，调用处再代入，避免不同实例覆盖同一注解 span。
        let mut env = HashMap::new();
        let mut subst = HashMap::new();
        for (param, arg) in alias.type_params.iter().zip(&args) {
            env.insert(
                param.split('@').next().unwrap_or(param).to_owned(),
                Ty::TypeParam(param.clone()),
            );
            subst.insert(param.clone(), arg.clone());
        }
        // 别名本身的约束也在最终阶段校验；载体只用于内部约束队列，不对外投影。
        if !args.is_empty() {
            self.generic_uses.insert(
                (use_module, span),
                Ty::Applied(Box::new(Ty::Struct(key.0, key.1.clone())), args),
            );
        }
        let def_scope = self.modules[key.0].global_scope;
        let target = alias.target.clone();
        let symbolic = self.resolve_type(key.0, def_scope, &target, &env);
        let ty = crate::ty::substitute_type_params(&symbolic, &subst);
        self.resolving_aliases.remove(&key);
        ty
    }

    /// 解析函数类型签名（`func (a: int) -> int` / 函数声明的签名部分）。
    pub(crate) fn resolve_function_type(
        &mut self,
        module: ModuleId,
        scope: ScopeId,
        function_type: &novella_ast::types::FunctionType,
        type_env: &HashMap<String, Ty>,
    ) -> FunctionSig {
        // 泛型形参按恒等映射加入环境（`T` 解析为 TypeParam("T")）
        let mut env = type_env.clone();
        for param in &function_type.type_parameters {
            env.insert(
                param.name.clone(),
                Ty::TypeParam(crate::ty::param_key(module, param)),
            );
        }
        self.register_bounds(module, scope, &function_type.type_parameters, &env);
        let params = function_type
            .params
            .iter()
            .map(|p| ParamSig {
                name: p.name.clone(),
                ty: match &p.ty {
                    Some(ty) => self.resolve_type(module, scope, ty, &env),
                    // 参数类型推导属后续期：未标注按 Unknown，调用点跳过该参检查
                    None => Ty::Unknown,
                },
                has_default: p.default.is_some(),
            })
            .collect();
        let ret = self.resolve_type(module, scope, &function_type.return_type, &env);
        FunctionSig {
            module,
            name: None,
            params,
            ret,
            type_param_count: function_type.type_parameters.len(),
            type_params: function_type
                .type_parameters
                .iter()
                .map(|p| crate::ty::param_key(module, p))
                .collect(),
            is_async: false,
            is_extern: false,
            is_method: false,
        }
    }
}

impl Sema {
    pub(crate) fn register_bounds(
        &mut self,
        module: ModuleId,
        scope: ScopeId,
        params: &[novella_ast::types::TypeParameter],
        env: &HashMap<String, Ty>,
    ) {
        for param in params {
            if let Some(bound) = &param.bound {
                let ty = self.resolve_type(module, scope, bound, env);
                let base = match &ty {
                    Ty::Applied(base, _) => &**base,
                    ty => ty,
                };
                if !matches!(base, Ty::Trait(..)) && !ty.is_opaque() {
                    self.type_error(module, bound.span(), "泛型约束必须是 trait 类型");
                }
                self.type_param_bounds
                    .insert(crate::ty::param_key(module, param), ty);
            }
        }
    }

    pub(crate) fn satisfies_bound(&self, bound: &Ty, actual: &Ty) -> bool {
        if actual.is_opaque() || bound.is_opaque() {
            return true;
        }
        if let Ty::TypeParam(name) = actual {
            return self
                .type_param_bounds
                .get(name)
                .is_some_and(|ty| crate::ty::ty_eq(bound, ty));
        }
        let mut current = actual.clone();
        let mut visited = std::collections::HashSet::new();
        loop {
            if crate::ty::ty_eq(bound, &current) {
                return true;
            }
            let base = match &current {
                Ty::Applied(base, _) => &**base,
                other => other,
            };
            if !matches!(base, Ty::Class(..)) {
                break;
            }
            let key = crate::typecheck::this_ty_key(&current);
            if !visited.insert(key.clone()) {
                break;
            }
            let Some(TypeDef::Class(def)) = self.type_defs.get(&key) else {
                break;
            };
            let subst = crate::ty::instance_substitution(self, &current);
            if def.trait_types.iter().any(|implemented| {
                crate::ty::ty_eq(
                    bound,
                    &crate::ty::substitute_type_params(implemented, &subst),
                )
            }) {
                return true;
            }
            let Some(parent) = &def.parent_type else {
                break;
            };
            current = crate::ty::substitute_type_params(parent, &subst);
        }
        false
    }
}

//! 名称解析：Phase 1 顶层声明收集、Phase 2a 导入绑定，以及
//! `TypeExpr -> Ty` 的类型解析（含类型别名展开与循环检测）。

use std::collections::HashMap;
use std::rc::Rc;

use novella_ast::program::{ImportKind, Item, Scope as Visibility};
use novella_ast::stmt::{BindingPattern, Statement};
use novella_ast::types::TypeExpr;
use novella_core::Span;

use crate::Sema;
use crate::module::{ModuleId, NsResolution, resolve_import_path};
use crate::symbol::{NsTarget, ScopeId, Symbol, SymbolKind};
use crate::ty::{
    AliasDef, ClassDef, FunctionSig, ParamSig, StructDef, TraitDef, Ty, TypeDef,
    predefined_from_enum, predefined_from_name,
};

// ---------- Phase 1：顶层声明收集 ----------

/// 收集一个模块的顶层声明到其全局作用域。
///
/// 只登记名字与种类（函数签名、字段表等在 Phase 2b 解析，
/// 彼时全部模块的顶层符号已就绪，可跨模块引用）。
pub(crate) fn collect_module(sema: &mut Sema, module: ModuleId) {
    if sema.modules[module].syntax_failed {
        return;
    }
    let scope = sema.modules[module].global_scope;
    let items = sema.modules[module].program.items.clone();
    for item in &items {
        match item {
            Item::Import(_) => {} // 导入在 Phase 2a 绑定
            Item::TypeAlias(alias) => {
                let def = AliasDef {
                    type_params: alias
                        .type_parameters
                        .iter()
                        .map(|p| p.name.clone())
                        .collect(),
                    target: alias.alias.clone(),
                };
                sema.type_defs
                    .insert((module, alias.name.clone()), TypeDef::Alias(def));
                // 类型别名暂不支持 `pub`（文法未提供可见性修饰），即不可跨模块导入
                define_type(sema, module, scope, &alias.name, None, alias.span);
            }
            Item::Trait(trait_item) => {
                let def = TraitDef {
                    type_param_count: trait_item.type_parameters.len(),
                };
                sema.type_defs
                    .insert((module, trait_item.name.clone()), TypeDef::Trait(def));
                define_type(
                    sema,
                    module,
                    scope,
                    &trait_item.name,
                    trait_item.scope,
                    trait_item.span,
                );
            }
            Item::Class(class) => {
                let def = ClassDef {
                    fields: vec![],
                    methods: HashMap::new(),
                    type_param_count: class.type_parameters.len(),
                    parent: class.parent.first().cloned(),
                    parent_resolved: None,
                };
                sema.type_defs
                    .insert((module, class.name.clone()), TypeDef::Class(def));
                define_type(sema, module, scope, &class.name, class.scope, class.span);
            }
            Item::Struct(struct_item) => {
                let def = StructDef {
                    fields: vec![],
                    methods: HashMap::new(),
                    type_param_count: struct_item.type_parameters.len(),
                };
                sema.type_defs
                    .insert((module, struct_item.name.clone()), TypeDef::Struct(def));
                define_type(
                    sema,
                    module,
                    scope,
                    &struct_item.name,
                    struct_item.scope,
                    struct_item.span,
                );
            }
            Item::Function(function) => {
                // 签名在 Phase 2b 解析，此处先占位登记名字
                let sig = FunctionSig {
                    module,
                    // 占位签名（Phase 2b 前），名义键在回填时补上函数名
                    name: None,
                    params: vec![],
                    ret: Ty::Unknown,
                    type_param_count: 0,
                    is_async: function.is_async,
                    is_extern: function.is_extern,
                };
                let symbol = Symbol {
                    name: function.name.clone(),
                    kind: SymbolKind::Function { sig },
                    visibility: function.scope,
                    span: function.span,
                    name_span: function.name_span,
                    module,
                };
                define(sema, module, scope, symbol);
            }
            Item::Statement(stmt) => collect_toplevel_variables(sema, module, scope, stmt),
            // 独立装饰器 / 装饰块：无符号产出（指令解释属后续期）
            Item::Decorator(_) | Item::DecoratorBlock { .. } => {}
        }
    }
}

/// 登记类型符号（struct / class / trait / 别名共用；实体在 type_defs）。
fn define_type(
    sema: &mut Sema,
    module: ModuleId,
    scope: ScopeId,
    name: &str,
    visibility: Option<Visibility>,
    span: Span,
) {
    let symbol = Symbol {
        name: name.to_string(),
        kind: SymbolKind::Type,
        visibility,
        span,
        name_span: span,
        module,
    };
    define(sema, module, scope, symbol);
}

/// 定义符号并处理重定义诊断。
fn define(sema: &mut Sema, module: ModuleId, scope: ScopeId, symbol: Symbol) {
    let name = symbol.name.clone();
    let span = symbol.span;
    if let Err(existing) = sema.scopes.define(scope, symbol) {
        sema.error(
            module,
            span,
            format!(
                "重复定义 `{name}`（首次定义见本模块 {} 字节偏移处）",
                existing.span.start
            ),
        );
    }
}

/// 登记顶层 `let` / `const` 变量（元组解构逐名登记；类型在检查阶段回填）。
fn collect_toplevel_variables(sema: &mut Sema, module: ModuleId, scope: ScopeId, stmt: &Statement) {
    let declarations: &[novella_ast::stmt::VariableStmt] = match stmt {
        Statement::Variable(decl) => std::slice::from_ref(decl),
        Statement::VariableList { declarations, .. } => declarations,
        _ => return,
    };
    for decl in declarations {
        let is_const = matches!(decl.r#type, novella_ast::stmt::VariableType::Const);
        let mut names = vec![];
        collect_pattern_names(&decl.pattern, &mut names);
        for (name, span) in names {
            let symbol = Symbol {
                name,
                kind: SymbolKind::Variable { is_const, ty: None },
                visibility: decl.scope,
                span,
                name_span: span,
                module,
            };
            define(sema, module, scope, symbol);
        }
    }
}

/// 收集绑定模式中的全部标识符（递归元组解构）。
pub(crate) fn collect_pattern_names(pattern: &BindingPattern, out: &mut Vec<(String, Span)>) {
    match pattern {
        BindingPattern::Ident { name, span } => out.push((name.clone(), *span)),
        BindingPattern::Tuple { elements, .. } => {
            for element in elements {
                collect_pattern_names(element, out);
            }
        }
    }
}

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
            NsResolution::StdExternal => NsTarget::StdExternal,
        };

        match &import.kind {
            ImportKind::Namespace { local_alias, .. } => {
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
                    let local = specifier
                        .local
                        .clone()
                        .unwrap_or_else(|| specifier.imported.clone());
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
                        NsTarget::StdExternal => None,
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
        // 先解析泛型实参（浅泛型：只查 arity，实参类型不代入定义体）
        let args: Vec<Ty> = generic_args
            .iter()
            .map(|a| self.resolve_type(module, scope, a, type_env))
            .collect();

        if qualifier.is_empty() {
            // 1) 替换环境（泛型形参 / 别名实参）
            if let Some(ty) = type_env.get(name) {
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
                return Ty::Unknown;
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
            return Ty::Unknown;
        }
        let ns = &qualifier[0];
        let Some(symbol) = self.scopes.lookup(scope, ns).cloned() else {
            self.error(
                module,
                span,
                format!("未定义的命名空间 `{ns}`（缺少对应 import）"),
            );
            return Ty::Unknown;
        };
        let SymbolKind::NamespaceImport { target } = symbol.kind else {
            self.error(module, span, format!("`{ns}` 不是命名空间导入绑定"));
            return Ty::Unknown;
        };
        match target {
            NsTarget::StdExternal => Ty::External,
            NsTarget::Module(target_module) => {
                let Some(target_symbol) = self.modules[target_module]
                    .global_scope_symbol(&self.scopes, name)
                    .cloned()
                else {
                    self.error(module, span, format!("模块中不存在类型 `{name}`"));
                    return Ty::Unknown;
                };
                self.record_reference(module, span, &target_symbol);
                if !target_symbol.is_importable() {
                    self.error(module, span, format!("类型 `{name}` 不是 `pub`"));
                    return Ty::Unknown;
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
                return Ty::Unknown;
            };
            return self.type_from_def(use_module, span, key, def, args, type_env);
        }
        if !matches!(symbol.kind, SymbolKind::Type) {
            self.error(use_module, span, format!("`{name}` 不是类型"));
            return Ty::Unknown;
        }
        let key = (symbol.module, symbol.name.clone());
        let Some(def) = self.type_defs.get(&key).cloned() else {
            self.error(use_module, span, format!("`{name}` 的类型定义缺失"));
            return Ty::Unknown;
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
            return Ty::Unknown;
        }
        match def {
            TypeDef::Struct(_) => Ty::Struct(key.0, key.1),
            TypeDef::Class(_) => Ty::Class(key.0, key.1),
            TypeDef::Trait(_) => Ty::Trait(key.0, key.1),
            TypeDef::Alias(alias) => {
                self.expand_alias(use_module, span, key, alias, args, type_env)
            }
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
        type_env: &HashMap<String, Ty>,
    ) -> Ty {
        if !self.resolving_aliases.insert(key.clone()) {
            self.error(use_module, span, format!("类型别名 `{}` 循环定义", key.1));
            return Ty::Unknown;
        }
        // 泛型别名：形参 → 实参的替换环境
        let mut env = type_env.clone();
        for (param, arg) in alias.type_params.iter().zip(args) {
            env.insert(param.clone(), arg);
        }
        let def_scope = self.modules[key.0].global_scope;
        let target = alias.target.clone();
        let ty = self.resolve_type(key.0, def_scope, &target, &env);
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
            env.insert(param.name.clone(), Ty::TypeParam(param.name.clone()));
        }
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
            is_async: false,
            is_extern: false,
        }
    }
}

//! 顶层符号与绑定模式的声明收集。
use super::*;
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
                        .map(|p| crate::ty::param_key(module, p))
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
                    type_params: trait_item
                        .type_parameters
                        .iter()
                        .map(|p| crate::ty::param_key(module, p))
                        .collect(),
                    type_param_count: trait_item.type_parameters.len(),
                    // 成员签名在 build_type_defs 阶段补齐（需要类型解析环境）
                    methods: HashMap::new(),
                    name: trait_item.name.clone(),
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
                    parent_type: None,
                    trait_types: Vec::new(),
                    type_params: class
                        .type_parameters
                        .iter()
                        .map(|p| crate::ty::param_key(module, p))
                        .collect(),
                    fields: vec![],
                    methods: HashMap::new(),
                    type_param_count: class.type_parameters.len(),
                    // D04：括号列表位置不预设语义，先在 resolve_class_parents 中
                    // 由 build_type_defs 填候选，再按符号表分类（class→父类 / trait→实现）
                    parent_resolved: None,
                    name: class.name.clone(),
                    span: class.span,
                    trait_impls: vec![],
                    trait_impls_resolved: vec![],
                };
                sema.type_defs
                    .insert((module, class.name.clone()), TypeDef::Class(def));
                define_type(sema, module, scope, &class.name, class.scope, class.span);
            }
            Item::Struct(struct_item) => {
                let def = StructDef {
                    type_params: struct_item
                        .type_parameters
                        .iter()
                        .map(|p| crate::ty::param_key(module, p))
                        .collect(),
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
                    type_params: vec![],
                    is_async: function.is_async,
                    is_extern: function.is_extern,
                    is_method: false,
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

//! 内部符号状态、诊断与公开投影的组装。
use crate::{
    ModuleId,
    analysis::AnalysisTarget,
    api::*,
    check,
    module::Module,
    symbol::{ScopeTree, Symbol, SymbolKind},
    ty::{self, TypeDef},
};
use novella_core::{
    Span,
    diagnostic::{Diagnostic, DiagnosticCode},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

/// 语义分析共享状态。
pub(crate) struct Sema {
    pub std_root: Option<PathBuf>,
    pub fstrings: HashMap<(ModuleId, Span), Vec<FStringPart>>,

    pub module_only: bool,
    pub exception_imports: HashSet<(ModuleId, String)>,
    pub generic_uses: HashMap<(ModuleId, Span), ty::Ty>,
    pub type_param_bounds: HashMap<String, Vec<ty::Ty>>,
    pub target: AnalysisTarget,
    pub build_target: novella_core::target::BuildTarget,
    /// 目标描述；语义阶段按指针宽度、浮点能力与 @no_std 约束读取。
    pub build_spec: novella_target::TargetSpec,
    pub no_main: bool,
    pub no_std: bool,
    /// 入口模块关闭整层标准库导入（`@no_alloc`，隐含 `no_std`）。
    pub no_alloc: bool,
    pub exports: HashMap<(ModuleId, Span), String>,
    pub exhaustive_matches: HashSet<(ModuleId, Span)>,
    pub return_tasks: HashMap<(ModuleId, String), check::returns::ReturnTask>,
    pub global_tasks: HashMap<(ModuleId, Span), check::globals::GlobalTask>,
    pub global_tasks_by_name: HashMap<(ModuleId, String), Span>,
    pub field_tasks: HashMap<check::fields::FieldKey, check::fields::FieldTask>,
    pub modules: Vec<Module>,
    pub scopes: ScopeTree,
    /// 类型定义注册表：`(模块, 类型名) -> 定义实体`
    pub type_defs: HashMap<(ModuleId, String), TypeDef>,
    /// 类型别名展开中的集合（循环检测）
    pub resolving_aliases: HashSet<(ModuleId, String)>,
    /// 规范化路径 → 模块 id（导入绑定阶段解析目标模块）
    pub module_by_path: HashMap<PathBuf, ModuleId>,
    pub global_diagnostics: Vec<Diagnostic>,
    /// 引用 → 定义绑定表（键为引用处的 `(模块, span)`）
    pub references: HashMap<(ModuleId, Span), Definition>,
    /// 表达式 → 语义类型（键为 `(模块, 表达式 span)`），供 codegen 推导布局
    pub expression_types: HashMap<(ModuleId, Span), ty::Ty>,
    pub operator_calls: HashMap<(ModuleId, Span), crate::OperatorCall>,
    pub call_instances: HashMap<(ModuleId, Span), crate::CallInstance>,
    pub dyn_conversions: HashMap<(ModuleId, Span), crate::DynConversion>,
    /// 二元数值运算的公共类型（键为 `(模块, 二元表达式 span)`）。
    ///
    /// 两侧位宽/浮点精度不一致时，HIR 需要按它把操作数统一提升后再运算；
    /// 未记录时表示两侧本就同型或该运算不参与提升（指针算术、移位）。
    pub binary_numeric: HashMap<(ModuleId, Span), ty::Ty>,
    pub annotation_types: HashMap<(ModuleId, Span), ty::Ty>,
    pub declaration_types: HashMap<(ModuleId, Span), ty::Ty>,
}

impl Sema {
    pub(crate) fn string_type(&self) -> Option<ty::Ty> {
        let root = self.std_root.as_deref()?;
        let path = novella_core::path::module_key(&root.join("string.nov"));
        let module = *self.module_by_path.get(&path)?;
        matches!(
            self.type_defs.get(&(module, "String".to_owned())),
            Some(TypeDef::Class(_))
        )
        .then(|| ty::Ty::Class(module, "String".to_owned()))
    }

    pub(crate) fn is_string_type(&self, candidate: &ty::Ty) -> bool {
        self.string_type()
            .is_some_and(|string_type| ty::ty_eq(candidate, &string_type))
    }

    /// 定义身份只在本次分析内有效；导入引用仍直达原始定义。
    pub(crate) fn definition_index(&self) -> HashMap<(ModuleId, Span), Definition> {
        let mut definitions = HashMap::new();
        for scope in &self.scopes.scopes {
            for symbol in scope.symbols.values() {
                if matches!(
                    symbol.kind,
                    SymbolKind::NamedImport { .. } | SymbolKind::NamespaceImport { .. }
                ) {
                    continue;
                }
                let definition = Definition::from_symbol(symbol);
                definitions.insert((definition.module, definition.span), definition);
            }
        }
        for ((module, _), definition) in &self.type_defs {
            let (fields, methods) = match definition {
                TypeDef::Struct(value) => (&value.fields[..], &value.methods),
                TypeDef::Class(value) => (&value.fields[..], &value.methods),
                TypeDef::Trait(value) => (&[][..], &value.methods),
                TypeDef::Alias(_) => continue,
            };
            for field in fields {
                let definition = Definition {
                    name: field.name.clone(),
                    module: *module,
                    span: field.name_span,
                    kind: DefinitionKind::Variable,
                };
                definitions.insert((*module, field.name_span), definition);
            }
            for method in methods.values() {
                let definition = Definition {
                    name: method.name.clone(),
                    module: *module,
                    span: method.name_span,
                    kind: DefinitionKind::Function,
                };
                definitions.insert((*module, method.name_span), definition);
            }
        }
        definitions
    }

    pub(crate) fn new() -> Self {
        Sema {
            std_root: None,
            fstrings: HashMap::new(),
            module_only: false,
            exception_imports: HashSet::new(),
            generic_uses: HashMap::new(),
            type_param_bounds: HashMap::new(),
            target: AnalysisTarget::default(),
            build_target: Default::default(),
            build_spec: novella_target::resolve_builtin(
                novella_core::target::TargetPlatform::default(),
            )
            .expect("内置目标描述必须有效"),
            no_main: false,
            no_std: false,
            no_alloc: false,
            exports: HashMap::new(),
            exhaustive_matches: HashSet::new(),
            return_tasks: HashMap::new(),
            global_tasks: HashMap::new(),
            global_tasks_by_name: HashMap::new(),
            field_tasks: HashMap::new(),
            modules: Vec::new(),
            scopes: ScopeTree::default(),
            type_defs: HashMap::new(),
            resolving_aliases: HashSet::new(),
            module_by_path: HashMap::new(),
            global_diagnostics: Vec::new(),
            references: HashMap::new(),
            expression_types: HashMap::new(),
            operator_calls: HashMap::new(),
            call_instances: HashMap::new(),
            dyn_conversions: HashMap::new(),
            binary_numeric: HashMap::new(),
            annotation_types: HashMap::new(),
            declaration_types: HashMap::new(),
        }
    }

    /// 记录一次「引用 → 定义」绑定。
    ///
    /// 具名导入符号先追到目标模块的原始定义（跳转直达源头而非 import 行）；
    /// 追不到（目标未导出等，诊断已在绑定阶段报过）时退而记录本地绑定。
    pub(crate) fn record_reference(&mut self, module: ModuleId, span: Span, symbol: &Symbol) {
        // 零长 span 来自占位 / 合成节点，记进引用表会让任意光标偏移误命中
        if span.start >= span.end {
            return;
        }
        // 沿再导出链追到终极定义（可多层 `pub` 重导入）；断裂时退回本地绑定
        let chased = match &symbol.kind {
            SymbolKind::NamedImport {
                target: Some(target),
                imported,
            } => match crate::resolve::resolve_exported(self, *target, imported) {
                Ok(crate::resolve::ExportTarget::Symbol(symbol)) => Some(*symbol),
                // 链在命名空间 / Exception 根 / 成环处终止：引用仍指向原始定义位置，
                // 不要退回到导入绑定本身（否则跳转落到 import 语句）
                Ok(_) | Err(_) => None,
            },
            _ => None,
        };
        let symbol = chased.as_ref().unwrap_or(symbol);
        self.references
            .insert((module, span), Definition::from_symbol(symbol));
    }

    /// 汇总类型注册表为层级摘要（声明 span 取自模块全局符号）。
    pub(crate) fn type_summaries(&self) -> Vec<TypeSummary> {
        let mut summaries = Vec::with_capacity(self.type_defs.len());
        for ((module, name), def) in &self.type_defs {
            let (kind, parent) = match def {
                TypeDef::Struct(_) => (TypeSummaryKind::Struct, None),
                TypeDef::Class(class) => (TypeSummaryKind::Class, class.parent_resolved.clone()),
                TypeDef::Trait(_) => (TypeSummaryKind::Trait, None),
                TypeDef::Alias(_) => (TypeSummaryKind::Alias, None),
            };
            let span = self.modules[*module]
                .global_scope_symbol(&self.scopes, name)
                .map(|symbol| symbol.name_span)
                .unwrap_or(Span { start: 0, end: 0 });
            let (fields, methods) = self.type_members(*module, name, def);
            summaries.push(TypeSummary {
                module: *module,
                name: name.clone(),
                kind,
                span,
                parent,
                applied_parent: match def {
                    TypeDef::Class(class) => class.parent_type.as_ref().map(ty::Ty::view),
                    _ => None,
                },
                type_parameters: def.type_params().to_vec(),
                fields,
                methods,
            });
        }
        summaries
    }

    /// 汇总类型定义的字段与方法（方法按名字排序，跨 HashMap 迭代顺序稳定）。
    fn type_members(
        &self,
        module: ModuleId,
        name: &str,
        def: &TypeDef,
    ) -> (Vec<FieldSummary>, Vec<MethodSummary>) {
        let (fields, methods) = match def {
            TypeDef::Struct(item) => (&item.fields, &item.methods),
            TypeDef::Class(item) => (&item.fields, &item.methods),
            TypeDef::Trait(item) => {
                let trait_ref = self.identity_instance(ty::Ty::Trait(module, name.to_owned()));
                let mut methods: Vec<_> = item
                    .methods
                    .iter()
                    .map(|(name, method)| MethodSummary {
                        name: name.clone(),
                        signature: method.sig.view(),
                        is_static: method.is_static,
                        is_dyn_dispatchable: self.dyn_dispatchable(&trait_ref, method),
                    })
                    .collect();
                methods.sort_by(|a, b| a.name.cmp(&b.name));
                return (Vec::new(), methods);
            }
            TypeDef::Alias(_) => return (Vec::new(), Vec::new()),
        };
        let fields = fields
            .iter()
            .map(|field| FieldSummary {
                name: field.name.clone(),
                ty: field.ty.view(),
                has_default: field.has_default,
                is_weak: field.is_weak,
                is_static: field.is_static,
            })
            .collect();
        let mut methods: Vec<MethodSummary> = methods
            .iter()
            .map(|(name, method)| MethodSummary {
                name: name.clone(),
                signature: method.sig.view(),
                is_static: method.is_static,
                is_dyn_dispatchable: false,
            })
            .collect();
        methods.sort_by(|a, b| a.name.cmp(&b.name));
        (fields, methods)
    }

    /// 在指定模块的 span 处追加 error 诊断（错误码取 [`DiagnosticCode::NameError`]，
    /// 即名称解析 / 导入绑定阶段的语义错误）。
    ///
    /// 类型检查与声明检查等阶段应改用 [`Sema::type_error`] / [`Sema::decl_error`] /
    /// [`Sema::unsafe_error`]，以便编辑器按错误码做快速修复。
    pub fn error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::NameError, message);
    }

    /// 在指定模块的 span 处追加带错误码的 error 诊断。
    pub fn error_with_code(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        self.modules[module].error_with_code(span, code, message);
    }

    /// 类型检查错误（`NVL3002`）。
    pub fn type_error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::TypeError, message);
    }

    /// 声明级检查错误（`NVL3003`）。
    pub fn decl_error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::DeclarationError, message);
    }

    /// unsafe 上下文规则错误（`NVL3004`）。
    pub fn unsafe_error(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.error_with_code(module, span, DiagnosticCode::UnsafeError, message);
    }

    /// 在指定模块的 span 处追加 warning 诊断。
    pub fn warning(
        &mut self,
        module: ModuleId,
        span: novella_core::Span,
        message: impl Into<String>,
    ) {
        self.modules[module].warning(span, message);
    }

    /// 取目标模块的全局符号（克隆返回，便于跨模块借用）。
    pub(crate) fn global_scope_symbol(
        &self,
        module: ModuleId,
        name: &str,
    ) -> Option<crate::symbol::Symbol> {
        self.modules
            .get(module)?
            .global_scope_symbol(&self.scopes, name)
            .cloned()
    }

    /// class 继承链上的 `is_subclass(ancestor, derived)` 判定（含自身）。
    pub fn is_subclass(&self, ancestor: &ty::Ty, derived: &ty::Ty) -> bool {
        let base = match ancestor {
            ty::Ty::Applied(base, _) => &**base,
            other => other,
        };
        if matches!(base, ty::Ty::Trait(..)) {
            return self.satisfies_bound(ancestor, derived);
        }
        let mut current = derived.clone();
        let mut visited = HashSet::new();
        loop {
            if ty::ty_eq(ancestor, &current) {
                return true;
            }
            let base = match &current {
                ty::Ty::Applied(base, _) => &**base,
                ty => ty,
            };
            let ty::Ty::Class(module, name) = base else {
                return false;
            };
            if matches!(ancestor,ty::Ty::Class(am,an) if am == module && an == name) {
                return true;
            }
            let key = (*module, name.clone());
            if !visited.insert(key.clone()) {
                return false;
            }
            let Some(TypeDef::Class(def)) = self.type_defs.get(&key) else {
                return false;
            };
            let Some(parent) = &def.parent_type else {
                return false;
            };
            current =
                ty::substitute_type_params(parent, &ty::instance_substitution(self, &current));
        }
    }

    /// 赋值不兼容的**原因**文案；兼容时返回 `None`。
    ///
    /// 与 [`Sema::is_assignable`] 判据保持一致，只是把「字面量超出目标位宽」
    /// 这类可用数值说清的情形单独给出可读原因（其余退回 None，由调用方用通用文案）。
    pub fn assign_mismatch_reason(&self, target: &ty::Ty, source: &ty::Ty) -> Option<String> {
        if self.is_assignable(target, source) {
            return None;
        }
        if let (ty::Ty::Float(kind), ty::Ty::FloatLiteral(value)) = (target, source) {
            if !value.is_finite() {
                return Some(format!(
                    "浮点字面量 `{value:?}` 不是有限值，超出 `{}` 的可表示范围 {}",
                    kind.name(),
                    ty::float_range_text(*kind)
                ));
            }
            return Some(format!(
                "浮点字面量 `{value:?}` 超出 `{}` 的可表示范围 {}",
                kind.name(),
                ty::float_range_text(*kind)
            ));
        }
        if let (ty::Ty::Int(kind), ty::Ty::IntLiteral(negative, value)) = (target, source) {
            return Some(format!(
                "整数字面量 `{}{value}` 超出 `{}` 的取值范围 {}",
                if *negative { "-" } else { "" },
                kind.name(),
                ty::int_range_text(ty::target_int_kind(*kind, self.target.pointer_width))
            ));
        }
        // 已定型整数：说明是窄化还是跨符号，两条路径都必须显式 `as`
        if let (ty::Ty::Int(target_kind), ty::Ty::Int(source_kind)) = (target, source) {
            let target_kind = ty::target_int_kind(*target_kind, self.target.pointer_width);
            let source_kind = ty::target_int_kind(*source_kind, self.target.pointer_width);
            let reason = if target_kind.is_signed() != source_kind.is_signed() {
                "跨符号转换"
            } else {
                "窄化转换"
            };
            return Some(format!(
                "{reason}从 `{}` 到 `{}` 会丢失或改变值域（{} 不能无损容纳 {}）；请显式 `as`",
                source_kind.name(),
                target_kind.name(),
                target_kind.name(),
                ty::int_range_text(source_kind)
            ));
        }
        // 已定型浮点：`f64 → f32` 等窄化必须显式 `as`
        if let (ty::Ty::Float(target_kind), ty::Ty::Float(source_kind)) = (target, source) {
            return Some(format!(
                "浮点窄化从 `{}` 到 `{}` 会丢失精度；请显式 `as`",
                source_kind.name(),
                target_kind.name()
            ));
        }
        None
    }

    /// `target ← source` 可赋值性（注入继承链判定）。
    pub fn is_assignable(&self, target: &ty::Ty, source: &ty::Ty) -> bool {
        let mut is_subclass = |a: &ty::Ty, b: &ty::Ty| self.is_subclass(a, b);
        ty::assignable(target, source, &mut is_subclass, self.target.pointer_width)
    }

    /// 记录二元数值运算的公共类型（供 HIR 统一提升操作数）。
    ///
    /// 只记录已落地的具体数值（`Int` / `Float`）：字面量态、指针、`??` 与
    /// 移位不参与提升，HIR 会按自身规则处理。重复记录以最后一次为准。
    pub(crate) fn record_binary_numeric(&mut self, module: ModuleId, span: Span, common: ty::Ty) {
        // 只收已定型的具体数值：两个字面量保持未定型态，由上下文各自落地；
        // 指针算术、`??`、移位与指针比较不进入本表。
        if matches!(common, ty::Ty::Int(_) | ty::Ty::Float(_)) && span.start < span.end {
            self.binary_numeric.insert((module, span), common);
        }
    }
}

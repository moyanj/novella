//! 模块加载、声明定型与函数体检查的阶段编排。
use crate::{FileLoader, FsLoader, Sema, api::*, check, module, resolve, ty};
use std::path::{Path, PathBuf};

/// 分析输入配置；标准库必须提供真实声明，不使用不透明符号兜底。
#[derive(Debug, Clone)]
pub struct AnalysisOptions {
    pub target: AnalysisTarget,
    /// 代码生成目标与运行时策略。
    pub build_target: novella_core::target::BuildTarget,
    pub cfg: crate::cfg::CfgOptions,
    /// std 模块目录；std:io 对应此目录下的 io.nov。
    pub std_root: Option<PathBuf>,
    /// 仅检查模块，不要求可执行程序的 main 入口。
    pub module_only: bool,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        let environment_root = std::env::var_os("NOVELLA_STD_ROOT").map(PathBuf::from);
        let bundled_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../std");
        Self {
            target: AnalysisTarget::default(),
            build_target: Default::default(),
            cfg: crate::cfg::CfgOptions::default(),
            std_root: environment_root.or_else(|| bundled_root.is_dir().then_some(bundled_root)),
            module_only: false,
        }
    }
}

/// 以真实文件系统分析入口文件及其全部导入闭包。
pub fn analyze(entry: &Path) -> SemaResult {
    analyze_with(&mut FsLoader, entry)
}

/// 以指定文件加载器分析（测试可注入内存文件系统）。
pub fn analyze_with(loader: &mut dyn FileLoader, entry: &Path) -> SemaResult {
    analyze_with_target(loader, entry, AnalysisTarget::default())
}

/// 静态分析的目标指针宽度；只接受实际支持的 32/64 位配置。
#[derive(Debug, Clone, Copy, Default)]
pub enum PointerWidth {
    Bits32,
    #[default]
    Bits64,
}

#[derive(Debug, Clone, Copy)]
pub struct AnalysisTarget {
    pub pointer_width: PointerWidth,
}

impl Default for AnalysisTarget {
    fn default() -> Self {
        Self {
            pointer_width: if usize::BITS == 32 {
                PointerWidth::Bits32
            } else {
                PointerWidth::Bits64
            },
        }
    }
}

/// 使用显式目标配置分析；现有 analyze/analyze_with 保持宿主目标默认行为。
pub fn analyze_with_target(
    loader: &mut dyn FileLoader,
    entry: &Path,
    target: AnalysisTarget,
) -> SemaResult {
    analyze_with_options(
        loader,
        entry,
        AnalysisOptions {
            target,
            ..Default::default()
        },
    )
}

/// 使用显式目标、条件编译及标准库声明目录分析。
pub fn analyze_with_options(
    loader: &mut dyn FileLoader,
    entry: &Path,
    mut options: AnalysisOptions,
) -> SemaResult {
    let mut sema = Sema::new();
    sema.module_only = options.module_only;
    sema.target = options.target;
    sema.build_target = options.build_target;
    options.cfg.target_os = options.build_target.os().to_owned();
    options.cfg.target_arch = options.build_target.arch().to_owned();
    options.cfg.target_family = options.build_target.family().to_owned();
    // Phase 0：模块加载（含词法 / 语法分析）
    let (modules, global_diagnostics, module_by_path) =
        module::load_modules(loader, entry, &mut sema.scopes, &mut options);
    // module_only 不拥有工程入口指令，但裸机模块仍继承禁止标准库的分析环境。
    sema.no_std = options.module_only && options.build_target.is_bare_metal()
        || modules.first().is_some_and(|module| module.program.items.iter().any(|item| matches!(item, novella_ast::program::Item::Directive(d) if d.name == "no_std" && d.tokens.is_empty())));
    sema.no_main = modules.first().is_some_and(|module| module.program.items.iter().any(|item| matches!(item, novella_ast::program::Item::Directive(d) if d.name == "no_main" && d.tokens.is_empty())));
    sema.std_root = options.std_root;
    sema.modules = modules;
    sema.global_diagnostics = global_diagnostics;
    sema.module_by_path = module_by_path;

    let complete = (|| {
        // Phase 1：全模块顶层声明收集（先于导入绑定，保证跨模块引用可见）
        for id in 0..sema.modules.len() {
            if loader.is_cancelled() {
                return false;
            }
            resolve::collect_module(&mut sema, id);
        }
        // Phase 2a：绑定导入符号
        for id in 0..sema.modules.len() {
            if loader.is_cancelled() {
                return false;
            }
            resolve::bind_imports(&mut sema, id);
        }
        resolve::bind_site_imports(&mut sema);
        // Phase 2b：构建类型注册表（字段 / 方法签名）
        for id in 0..sema.modules.len() {
            if loader.is_cancelled() {
                return false;
            }
            check::build_type_defs(&mut sema, id);
        }
        if loader.is_cancelled() {
            return false;
        }
        // Phase 2c：注册继承关系，再按依赖定型字段、全局变量和函数返回类型。
        check::resolve_class_parents(&mut sema);
        check::returns::collect(&mut sema);
        check::globals::collect(&mut sema);
        check::fields::check_fields(&mut sema);
        check::globals::check_all(&mut sema);
        check::returns::check_all(&mut sema);
        if loader.is_cancelled() {
            return false;
        }
        check::check_inherited_signatures(&mut sema);
        // Phase 2d：声明级检查 + 函数体 / 顶层语句类型检查
        for id in 0..sema.modules.len() {
            if loader.is_cancelled() {
                return false;
            }
            check::check_module(&mut sema, id);
        }
        for ((module, span), ty) in sema.generic_uses.clone() {
            let subst = ty::instance_substitution(&sema, &ty);
            for (param, actual) in &subst {
                if let Some(bound) = sema.type_param_bounds.get(param) {
                    let bound = ty::substitute_type_params(bound, &subst);
                    if !sema.satisfies_bound(&bound, actual) {
                        sema.type_error(
                            module,
                            span,
                            format!("类型实参 `{actual}` 不满足 `{bound}` 约束"),
                        );
                    }
                }
            }
        }

        !loader.is_cancelled()
    })();
    if !complete {
        sema.global_diagnostics
            .push(novella_core::diagnostic::Diagnostic::error(
                "分析已取消，产物不可使用",
            ));
    }

    crate::freestanding::validate(&mut sema);
    let string_type = sema.string_type().map(|ty| ty.view());
    let types = sema.type_summaries();
    let definitions = sema.definition_index();
    let codegen_modules = crate::dependencies::codegen_modules(&sema);

    // 模块产物和诊断分别移入公开视图；源码、AST 不在此处深拷贝。
    let module_count = sema.modules.len();
    let mut analyzed_modules = Vec::with_capacity(module_count);
    let mut module_diagnostics = Vec::with_capacity(module_count);
    for (id, module) in std::mem::take(&mut sema.modules).into_iter().enumerate() {
        let split_at = module.syntax_diagnostic_count;
        let diagnostics = module.diagnostics;
        let path = module.path;
        analyzed_modules.push(AnalyzedModule {
            id,
            required_for_codegen: codegen_modules.contains(&id),
            path: path.clone(),
            source: module.source,
            tokens: module.tokens,
            program: module.program,
            syntax_failed: module.syntax_failed,
            load_failed: module.load_failed,
        });
        // 所有诊断只有一份，阶段切片与排序由 ModuleDiagnostics 统一提供。
        module_diagnostics.push(ModuleDiagnostics {
            path,
            diagnostics,
            syntax_diagnostic_count: split_at,
        });
    }

    let mut result = SemaResult {
        analysis_target: sema.target,
        build_target: sema.build_target,
        no_main: sema.no_main,
        no_std: sema.no_std,
        exports: sema.exports,
        string_type,
        fstrings: sema.fstrings,
        declarations: Default::default(),
        exhaustive_matches: sema.exhaustive_matches,
        diagnostics: DiagnosticReport {
            modules: module_diagnostics,
            global: sema.global_diagnostics,
        },
        bindings: BindingTable {
            references: sema.references,
            definitions,
        },
        types: TypeHierarchy { summaries: types },
        modules: ModuleOutput {
            modules: analyzed_modules,
        },
        expression_types: ExpressionTypes {
            types: sema
                .expression_types
                .into_iter()
                .map(|(key, ty)| (key, ty.view()))
                .collect(),
        },
        annotation_types: ExpressionTypes {
            types: sema
                .annotation_types
                .into_iter()
                .map(|(key, ty)| (key, ty.view()))
                .collect(),
        },
        declaration_types: ExpressionTypes {
            types: sema
                .declaration_types
                .into_iter()
                .map(|(key, ty)| (key, ty.view()))
                .collect(),
        },
    };
    result.declarations = crate::DeclarationIndex::build(&result.modules, &result.bindings);
    result
}

use super::*;
use novella_core::Span;
use std::{collections::HashMap, path::PathBuf};

/// OverlayLoader 的内存文本参与分析：引用表记录跨模块调用与类型引用，
/// 类型摘要记录 class 继承链（B1/B3/C2 的联合验证）。
#[test]
fn references_and_type_summaries_are_collected() {
    let dir = PathBuf::from("/novella-mem-test");
    let main = dir.join("main.nov");
    let helper = dir.join("helper.nov");
    let mut overlay = HashMap::new();
    overlay.insert(
        helper.clone(),
        "pub func f() -> int {\n    return 1;\n}\n".to_owned(),
    );
    overlay.insert(
            main.clone(),
            "import \"helper\";\nclass Base {}\nclass Shape(Base) {}\nfunc main() {\n    let x = helper.f();\n    let b = new Base();\n}\n"
                .to_owned(),
        );
    let mut loader = OverlayLoader::new(overlay);
    let result = analyze_with(&mut loader, &main);
    assert!(
        !result.diagnostics.has_errors(),
        "overlay 分析应无错误：{:?}",
        result
            .diagnostics
            .modules
            .iter()
            .flat_map(|m| m.diagnostics.iter().map(|d| &d.message))
            .collect::<Vec<_>>()
    );

    // 类型摘要：Base / Shape 都在，Shape 的父类解析为 Base
    let shape = result
        .types
        .summaries
        .iter()
        .find(|t| t.name == "Shape")
        .expect("Shape 应在类型摘要中");
    assert_eq!(shape.kind, TypeSummaryKind::Class);
    let parent = shape.parent.as_ref().expect("Shape 有父类");
    assert_eq!(parent.1, "Base");
    assert_eq!(parent.0, shape.module, "同模块继承");

    // 引用表：class 声明里的父类名 Base、`Base()` 构造、`helper.f()` 成员访问
    assert!(
        result
            .bindings
            .references
            .values()
            .any(|d| d.name == "Base"),
        "应记录 Base 的类型引用：{:?}",
        result.bindings.references
    );
    assert!(
        result
            .bindings
            .references
            .values()
            .any(|d| d.name == "f" && d.kind == DefinitionKind::Function),
        "应记录 helper.f 的函数引用"
    );
    // 定义均落在声明模块内（helper.f 定义在 helper 模块）
    let f_def = result
        .bindings
        .references
        .values()
        .find(|d| d.name == "f")
        .expect("f 的引用");
    assert_eq!(
        result.diagnostics.modules[f_def.module].path,
        module_key(&helper)
    );
}

/// 表达式类型表按 `(模块, span)` 导出，且投影为公开的 `TypeView`。
#[test]
fn expression_types_are_exported_as_type_views() {
    let dir = PathBuf::from("/novella-mem-types");
    let main = dir.join("main.nov");
    let source = "func main() {\n    let x: int = 1 + 2;\n    let b = true;\n}\n";
    let mut overlay = HashMap::new();
    overlay.insert(main.clone(), source.to_owned());
    let mut loader = OverlayLoader::new(overlay);
    let result = analyze_with(&mut loader, &main);
    assert!(!result.diagnostics.has_errors(), "不应有错误");

    let entry = result.modules.find_entry(&main).expect("入口模块产物");
    let program = &entry.program;
    // 收集所有表达式 span，逐个查类型表，覆盖率必须 100%
    let spans = collect_expression_spans(program);
    assert!(!spans.is_empty(), "示例应含表达式");
    for span in &spans {
        assert!(
            result.expression_types.get(0, *span).is_some(),
            "表达式 span {span:?} 缺少类型"
        );
    }
    // `1`、`2` 与 `1 + 2` 都是未定型字面量类型（`IntLiteral`）；
    // 只有对照标注时才会定型为 `i32`，因此这里断言 `IntLiteral` 而不假定具体类型
    let literals = spans
        .iter()
        .map(|s| result.expression_types.get(0, *s).expect("已校验"))
        .filter(|t| matches!(t, TypeView::IntLiteral))
        .count();
    assert_eq!(literals, 3, "`1`、`2` 与 `1 + 2` 均为未定型整数");
    assert!(
        spans
            .iter()
            .any(|s| matches!(result.expression_types.get(0, *s), Some(TypeView::Boolean))),
        "`true` 应投影为 Boolean"
    );
}

/// 重导入（`pub` 重导出）支持多层链，且以 `pub` 控制可见性。
#[test]
fn reexport_chain_resolves_through_public_imports() {
    let dir = PathBuf::from("/novella-mem-reexport");
    let main = dir.join("main.nov");
    let mid = dir.join("mid.nov");
    let base = dir.join("base.nov");
    let mut overlay = HashMap::new();
    overlay.insert(
        base.clone(),
        "pub func make() -> int {\n    return 1;\n}\n".to_owned(),
    );
    // mid 用 `pub` 重导出 base 的符号
    overlay.insert(mid.clone(), "pub from \"base\" import make;\n".to_owned());
    overlay.insert(
        main.clone(),
        "from \"mid\" import make;\n\nfunc main() {\n    let x = make();\n}\n".to_owned(),
    );
    let mut loader = OverlayLoader::new(overlay);
    let result = analyze_with(&mut loader, &main);
    let errors: Vec<&str> = result
        .diagnostics
        .modules
        .iter()
        .flat_map(|m| m.diagnostics.iter().map(|d| d.message.as_str()))
        .collect();
    assert!(errors.is_empty(), "多层 pub 重导出应通过，实际：{errors:?}");
}

/// 未标 `pub` 的导入不能被下游再导入（非重导出）。
#[test]
fn private_import_is_not_reexported() {
    let dir = PathBuf::from("/novella-mem-noreexport");
    let main = dir.join("main.nov");
    let mid = dir.join("mid.nov");
    let base = dir.join("base.nov");
    let mut overlay = HashMap::new();
    overlay.insert(
        base.clone(),
        "pub func make() -> int {\n    return 1;\n}\n".to_owned(),
    );
    overlay.insert(mid.clone(), "from \"base\" import make;\n".to_owned());
    overlay.insert(
        main.clone(),
        "from \"mid\" import make;\n\nfunc main() {\n    let x = make();\n}\n".to_owned(),
    );
    let mut loader = OverlayLoader::new(overlay);
    let result = analyze_with(&mut loader, &main);
    let messages: Vec<String> = result
        .diagnostics
        .modules
        .iter()
        .flat_map(|m| m.diagnostics.iter().map(|d| d.message.clone()))
        .collect();
    // 私有导入在中间模块的全局作用域里以不可导入形态存在，最终复核报「不是 pub」
    assert!(
        messages
            .iter()
            .any(|message| message.contains("不是 `pub`")),
        "未重导出的导入应被拒绝，实际：{messages:?}"
    );
}

/// 重导出链成环时报错而不是挂起。
///
/// 两个模块互相 `pub from` 导入了对方并不存在的名字：绑定轮转收敛后，
/// 复核阶段必须报告缺失，且分析整体能结束（不能死循环）。
#[test]
fn reexport_cycle_is_reported() {
    let dir = PathBuf::from("/novella-mem-cycle");
    let main = dir.join("main.nov");
    let a = dir.join("a.nov");
    let b = dir.join("b.nov");
    let mut overlay = HashMap::new();
    overlay.insert(a.clone(), "pub from \"b\" import x;\n".to_owned());
    overlay.insert(b.clone(), "pub from \"a\" import x;\n".to_owned());
    overlay.insert(
        main.clone(),
        "from \"a\" import x;\n\nfunc main() {\n    let y = x;\n}\n".to_owned(),
    );
    let mut loader = OverlayLoader::new(overlay);
    let result = analyze_with(&mut loader, &main);
    let messages: Vec<&str> = result
        .diagnostics
        .modules
        .iter()
        .flat_map(|m| m.diagnostics.iter().map(|d| d.message.as_str()))
        .collect();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("中不存在符号 `x`")),
        "成环链应报缺失，实际：{messages:?}"
    );
}

/// 递归收集 AST 中全部表达式 span（含嵌套在语句里的）。
///
/// 与 `typecheck` 的遍历保持同一套递归字段（AST 新增递归字段时本函数
/// 因 `match` 无兜底分支而编译失败，不会静默漏收）。
fn collect_expression_spans(program: &novella_ast::program::Program) -> Vec<Span> {
    use novella_ast::expr::Expression;
    use novella_ast::program::Item;
    use novella_ast::stmt::{ForInit, ForKind, IfBranch, Statement};

    fn walk_expr(expr: &Expression, out: &mut Vec<Span>) {
        out.push(expr.span());
        match expr {
            Expression::Paren { expr, .. }
            | Expression::Unary { operand: expr, .. }
            | Expression::Await { expr, .. }
            | Expression::Cast { expr, .. } => walk_expr(expr, out),
            Expression::Tuple { elements, .. } | Expression::ArrayLiteral { elements, .. } => {
                for element in elements {
                    walk_expr(element, out);
                }
            }
            Expression::ObjectLiteral { properties, .. } => {
                for property in properties {
                    if let Some(value) = &property.value {
                        walk_expr(value, out);
                    }
                }
            }
            Expression::Binary { left, right, .. } => {
                walk_expr(left, out);
                walk_expr(right, out);
            }
            Expression::Ternary {
                condition,
                then_expr,
                else_expr,
                ..
            } => {
                walk_expr(condition, out);
                walk_expr(then_expr, out);
                walk_expr(else_expr, out);
            }
            Expression::Assignment { target, value, .. } => {
                walk_expr(target, out);
                walk_expr(value, out);
            }
            Expression::MemberAccess { target, .. } => walk_expr(target, out),
            Expression::IndexAccess { target, index, .. } => {
                walk_expr(target, out);
                walk_expr(index, out);
            }
            Expression::Call { callee, args, .. } => {
                walk_expr(callee, out);
                for arg in args {
                    walk_expr(arg, out);
                }
            }
            Expression::New { args, .. } => {
                for arg in args {
                    walk_expr(arg, out);
                }
            }
            Expression::Is { operand, .. } => walk_expr(operand, out),
            Expression::Match(m) => walk_expr(&m.subject, out),
            // lambda 体不在本期类型检查范围内（`check_lambda` 自建上下文）
            Expression::Lambda(_) => {}
            Expression::Literal { .. }
            | Expression::Identifier { .. }
            | Expression::This { .. }
            | Expression::StructInit { .. } => {}
        }
    }

    fn walk_stmt(stmt: &Statement, out: &mut Vec<Span>) {
        match stmt {
            Statement::Block(block) => {
                for s in &block.stmts {
                    walk_stmt(s, out);
                }
            }
            Statement::Variable(v) => {
                if let Some(value) = &v.value {
                    walk_expr(value, out);
                }
            }
            Statement::VariableList { declarations, .. } => {
                for v in declarations {
                    if let Some(value) = &v.value {
                        walk_expr(value, out);
                    }
                }
            }
            Statement::Expression(expr) => walk_expr(expr, out),
            Statement::Return { value, .. } | Statement::Yield { value, .. } => {
                if let Some(value) = value {
                    walk_expr(value, out);
                }
            }
            Statement::Throw { expr, .. } => {
                if let Some(expr) = expr {
                    walk_expr(expr, out);
                }
            }
            Statement::If(s) => {
                for branch in &s.branches {
                    match branch {
                        IfBranch::ElseIf {
                            condition, block, ..
                        } => {
                            walk_expr(condition, out);
                            walk_stmt(block, out);
                        }
                        IfBranch::Else { block, .. } => walk_stmt(block, out),
                    }
                }
            }
            Statement::While {
                condition, body, ..
            } => {
                walk_expr(condition, out);
                walk_stmt(body, out);
            }
            Statement::For(s) => {
                match &s.condition {
                    ForKind::ForIn(cond) => walk_expr(&cond.right, out),
                    ForKind::ForLoop(cond) => {
                        if let Some(init) = &cond.init {
                            match init {
                                ForInit::Declaration { declarations, .. } => {
                                    for v in declarations {
                                        if let Some(value) = &v.value {
                                            walk_expr(value, out);
                                        }
                                    }
                                }
                                ForInit::Expression { expr, .. } => walk_expr(expr, out),
                            }
                        }
                        if let Some(condition) = &cond.condition {
                            walk_expr(condition, out);
                        }
                        if let Some(update) = &cond.update {
                            walk_expr(update, out);
                        }
                    }
                }
                walk_stmt(&s.block, out);
            }
            Statement::Try(s) => {
                for s in &s.block.stmts {
                    walk_stmt(s, out);
                }
                for catch in &s.catches {
                    for s in &catch.block.stmts {
                        walk_stmt(s, out);
                    }
                }
                if let Some(finally) = &s.finally {
                    for s in &finally.stmts {
                        walk_stmt(s, out);
                    }
                }
            }
            Statement::With(s) => {
                walk_expr(&s.expr, out);
                for s in &s.block.stmts {
                    walk_stmt(s, out);
                }
            }
            Statement::Unsafe { block, .. } => {
                for s in &block.stmts {
                    walk_stmt(s, out);
                }
            }
            Statement::Empty { .. } | Statement::Break { .. } | Statement::Continue { .. } => {}
        }
    }

    let mut out = Vec::new();
    for item in &program.items {
        if let Item::Function(function) = item
            && let Some(block) = &function.block
        {
            walk_stmt(block, &mut out);
        }
    }
    out
}

/// A1：`static_only` 真正生效——静态 / 实例成员互不遮蔽，跨静态性覆写被拒。
///
/// 修复前 `lookup_member_chain` 忽略 `static_only`，导致：
/// - 父类**实例**方法被当作静态同名方法命中，`子类静态方法覆写父类实例方法` 不再报错；
/// - 子类实例字段与父类静态成员被判为「遮蔽继承成员」（误报）。
#[test]
fn static_only_filters_member_staticity() {
    let dir = PathBuf::from("/novella-mem-static-only");
    let main = dir.join("main.nov");
    let source = "\
class Base {
    static func make() -> int { return 1; }
    func run() -> int { return 2; }
    static tag: int = 3;
}
class Child(Base) {
    static func run() -> int { return 4; }
}
func main() { }
";
    let mut overlay = HashMap::new();
    overlay.insert(main.clone(), source.to_owned());
    let mut loader = OverlayLoader::new(overlay);
    let result = analyze_with(&mut loader, &main);
    let messages: Vec<&str> = result
        .diagnostics
        .modules
        .iter()
        .flat_map(|m| m.diagnostics.iter().map(|d| d.message.as_str()))
        .collect();
    // `Child::run` 是静态方法，`Base::run` 是实例方法 → 静态性不一致
    assert!(
        messages
            .iter()
            .any(|msg| msg.contains("静态 / 实例属性不一致")),
        "应报静态性不一致：{messages:?}"
    );
}

/// A1：子类**实例**字段不与父类**静态**成员冲突（不存在遮蔽）。
#[test]
fn instance_field_does_not_shadow_parent_static() {
    let dir = PathBuf::from("/novella-mem-shadow");
    let main = dir.join("main.nov");
    let source = "\
class Base {
    static tag: int = 3;
}
class Child(Base) {
    pub let tag: int = 4;
}
func main() { }
";
    let mut overlay = HashMap::new();
    overlay.insert(main.clone(), source.to_owned());
    let mut loader = OverlayLoader::new(overlay);
    let result = analyze_with(&mut loader, &main);
    let messages: Vec<&str> = result
        .diagnostics
        .modules
        .iter()
        .flat_map(|m| m.diagnostics.iter().map(|d| d.message.as_str()))
        .collect();
    assert!(
        messages.iter().all(|msg| !msg.contains("不能遮蔽继承成员")),
        "实例字段不应与父类静态成员冲突：{messages:?}"
    );
}

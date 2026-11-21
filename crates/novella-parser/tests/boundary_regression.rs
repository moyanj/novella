//! parser → sema 边界修复的回归测试。
//!
//! 每个用例以条目编号命名（A/B/C/D/E 类）；条目修复后测试长期驻留防回归。

use novella_ast::{
    expr::Expression,
    program::{ClassMember, ImportKind, Item, Scope},
    stmt::{BindingPattern, ForInit, ForKind, Statement},
    types::TypeExpr,
};
use novella_core::token::TokenKind;
use novella_lexer::lexer;
use novella_parser::{ParseResult, Parser, ParserInput};

/// 词法 + 语法解析；词法错误直接失败（测试输入都应当词法合法）。
fn parse(src: &str) -> ParseResult {
    let mut lexed = lexer(src);
    assert!(lexed.errors.is_empty(), "词法错误：{:?}", lexed.errors);
    // 项目约定：注释在语法层透明，喂给 parser 前过滤
    lexed
        .tokens
        .retain(|token| token.kind != TokenKind::LineComment);
    Parser::new(ParserInput {
        tokens: lexed.tokens,
        file: "TEST".to_string(),
    })
    .parse()
}

/// 解析并断言零诊断。
fn parse_clean(src: &str) -> ParseResult {
    let result = parse(src);
    assert!(
        result.diagnostics.is_empty(),
        "期望零诊断，实际：{:?}",
        messages(&result)
    );
    result
}

fn messages(result: &ParseResult) -> Vec<&str> {
    result
        .diagnostics
        .iter()
        .map(|diag| diag.message.as_str())
        .collect()
}

fn assert_has_diagnostic(result: &ParseResult, needle: &str) {
    assert!(
        messages(result).iter().any(|msg| msg.contains(needle)),
        "期望包含诊断「{needle}」，实际：{:?}",
        messages(result)
    );
}

/// 取出函数体的语句列表（测试输入都是单个 `func` 顶层声明）。
fn function_stmts(result: &ParseResult) -> &[Statement] {
    let Some(Item::Function(func)) = result.program.items.first() else {
        panic!("期望顶层 func，实际：{:?}", result.program.items);
    };
    let Some(block) = &func.block else {
        panic!("期望函数体存在");
    };
    let Statement::Block(block) = block.as_ref() else {
        panic!("期望函数体为块");
    };
    &block.stmts
}

// ---------- A 类：递归防护（stacker 分段栈） ----------

/// A-01 ~ A-04：一元 / 三元 / 赋值 / 类型路径的深层嵌套**不再是错误**。
///
/// 修复前用 `MAX_NESTING = 128` 拒绝这些输入；现改为 `stacker` 分段栈，
/// 300~500 层的合法输入应零诊断解析，并如数保留嵌套结构（不再被截断）。
#[test]
fn a_deep_nesting_is_parsed_not_rejected() {
    // A-01：一元 `!` 链（500 层）
    let unary = format!("func f() {{ let x = {}true; }}", "!".repeat(500));
    let result = parse_clean(&unary);
    let Statement::Variable(var) = &function_stmts(&result)[0] else {
        panic!("期望变量声明");
    };
    // 逐层剥开 `Expression::Unary`，断言 500 层全部保留
    let mut depth = 0usize;
    let mut current = var.value.as_ref().expect("期望初始值");
    while let Expression::Unary { operand, .. } = current {
        depth += 1;
        current = operand;
    }
    assert_eq!(depth, 500, "一元链被截断（实际 {depth} 层）");

    // A-02：三元链（右递归，300 层）
    let ternary = format!("func f() {{ let x = a{}; }}", "?b:c".repeat(300));
    let result = parse_clean(&ternary);
    let Statement::Variable(var) = &function_stmts(&result)[0] else {
        panic!("期望变量声明");
    };
    let mut depth = 0usize;
    let mut current = var.value.as_ref().expect("期望初始值");
    // `a?b:c?b:c` 的右递归落在 else 分支上（then 分支自成一棵子树）
    while let Expression::Ternary { else_expr, .. } = current {
        depth += 1;
        current = else_expr;
    }
    assert_eq!(depth, 300, "三元链被截断（实际 {depth} 层）");

    // A-03：赋值链（右递归，300 层）
    let assignment = format!("func f() {{ {} }}", "a = ".repeat(300) + "a;");
    let result = parse_clean(&assignment);
    let Statement::Expression(expr) = &function_stmts(&result)[0] else {
        panic!("期望表达式语句");
    };
    let mut depth = 0usize;
    let mut current = expr;
    while let Expression::Assignment { value, .. } = current {
        depth += 1;
        current = value;
    }
    assert_eq!(depth, 300, "赋值链被截断（实际 {depth} 层）");

    // A-04：类型括号嵌套（300 层）
    let ty = format!(
        "func f() {{ let x: {}i32{} = y; }}",
        "(".repeat(300),
        ")".repeat(300)
    );
    parse_clean(&ty);
}

/// A-05：在小栈线程里解析超深输入也不溢出真实栈。
///
/// 线程栈刻意压到 256 KiB（远小于 `recursion::STACK_SEGMENT` 的 2 MiB 分段），
/// 若分段栈失效，8000 层括号会立刻 abort；此用例是分段栈生效的直接证据。
///
/// 层数取 8000 而非更高，是因为 debug 构建下每层括号约耗 131 KiB 栈，
/// 而分段预算 `MAX_STACK_BYTES` 为 1 GiB——8000 层约 1 GiB 之内可稳定通过。
#[test]
fn a_deep_nesting_survives_small_thread_stack() {
    let src = format!(
        "func f() {{ let x = {}1{}; }}",
        "(".repeat(8_000),
        ")".repeat(8_000)
    );
    let result = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || parse(&src))
        .expect("创建小栈线程失败")
        .join()
        .expect("解析线程 panic");
    assert!(
        result.diagnostics.is_empty(),
        "小栈线程下出现诊断：{:?}",
        messages(&result)
    );
}

/// A-06：超出分段预算的病态输入仍线性终止并报诊断，不无限下钻。
#[test]
fn a_excessive_depth_reports_upper_bound() {
    // 预算按**实际新开分段数**计：1 GiB / 2 MiB = 512 段，debug 下每层括号
    // 约耗 131 KiB，对应约 8000 层。取 2 万层确保远超上限、必定触发诊断，
    // 同时把输入规模控制在能在 1 秒内跑完（早期用 40 万层曾把整机 OOM）。
    let parens = 20_000usize;
    let src = format!(
        "func f() {{ let x = {}1{}; }}",
        "(".repeat(parens),
        ")".repeat(parens)
    );
    let result = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || parse(&src))
        .expect("创建小栈线程失败")
        .join()
        .expect("解析线程 panic");
    assert_has_diagnostic(&result, "超出工程上限");
}

// ---------- B 类：合法输入被拒 ----------

/// B-01：with 表头按文法无括号。
#[test]
fn b_01_with_header_without_parens() {
    let result =
        parse_clean("func f() { with open(\"x\") as file { read(file); } with sysCtx { run(); } }");
    let stmts = function_stmts(&result);
    let Statement::With(with_stmt) = &stmts[0] else {
        panic!("期望 with 语句，实际：{:?}", stmts[0]);
    };
    assert!(with_stmt.binding.is_some());
    let Statement::With(no_binding) = &stmts[1] else {
        panic!("期望 with 语句，实际：{:?}", stmts[1]);
    };
    assert!(no_binding.binding.is_none());

    // 兼容：旧括号形态给出迁移诊断，但仍能解析（括号按表达式分组）
    let paren = parse("func f() { with (sysCtx) { run(); } }");
    assert_has_diagnostic(&paren, "with 表头无需括号");
}

/// B-02：括号类型之后允许 `*` / `[]` 后缀。
#[test]
fn b_02_type_suffix_after_paren() {
    let result = parse_clean("func f() { let a: (A | B)* = x; let b: (i32, i32)[] = y; }");
    let stmts = function_stmts(&result);
    let Statement::Variable(a) = &stmts[0] else {
        panic!("期望变量声明，实际：{:?}", stmts[0]);
    };
    assert!(
        matches!(a.ty, Some(TypeExpr::Pointer { .. })),
        "期望指针类型，实际：{:?}",
        a.ty
    );
    let Statement::Variable(b) = &stmts[1] else {
        panic!("期望变量声明，实际：{:?}", stmts[1]);
    };
    assert!(
        matches!(b.ty, Some(TypeExpr::Array { .. })),
        "期望数组类型，实际：{:?}",
        b.ty
    );
}

/// B-03/B-04：空参 lambda 与非首参带注解的 lambda。
#[test]
fn b_03_b_04_lambda_forms() {
    let result = parse_clean("func f() { let a = () => 1; let b = (x, y: i32) => y; }");
    let stmts = function_stmts(&result);
    let Statement::Variable(a) = &stmts[0] else {
        panic!("期望变量声明，实际：{:?}", stmts[0]);
    };
    assert!(matches!(a.value, Some(Expression::Lambda(_))));
    let Statement::Variable(b) = &stmts[1] else {
        panic!("期望变量声明，实际：{:?}", stmts[1]);
    };
    assert!(matches!(b.value, Some(Expression::Lambda(_))));
}

/// B-05/D-02：单参数简写 lambda 支持返回注解、`async` 标记不再丢失。
#[test]
fn b_05_d_02_single_param_lambda() {
    let result = parse_clean("func f() { let a = x: i32 => x; let b = async x => x; }");
    let stmts = function_stmts(&result);
    let Statement::Variable(a) = &stmts[0] else {
        panic!("期望变量声明，实际：{:?}", stmts[0]);
    };
    let Some(Expression::Lambda(lambda)) = &a.value else {
        panic!("期望 lambda，实际：{:?}", a.value);
    };
    assert!(lambda.return_type.is_some(), "单参 lambda 返回注解丢失");
    let Statement::Variable(b) = &stmts[1] else {
        panic!("期望变量声明，实际：{:?}", stmts[1]);
    };
    let Some(Expression::Lambda(async_lambda)) = &b.value else {
        panic!("期望 lambda，实际：{:?}", b.value);
    };
    assert!(async_lambda.is_async, "async 单参 lambda 丢失 async 标记");
}

/// B-06：for init 段的索引 / 成员赋值是表达式，不是裸声明。
#[test]
fn b_06_for_init_index_assignment_is_expression() {
    let result = parse_clean("func f() { for (arr[i] = 0; i < 3; i++) {} }");
    let Statement::For(for_stmt) = &function_stmts(&result)[0] else {
        panic!("期望 for 语句，实际：{:?}", function_stmts(&result)[0]);
    };
    let ForKind::ForLoop(cond) = &for_stmt.condition else {
        panic!("期望三段式 for，实际：{:?}", for_stmt.condition);
    };
    assert!(
        matches!(cond.init, Some(ForInit::Expression { .. })),
        "索引赋值应解析为表达式 init，实际：{:?}",
        cond.init
    );
}

/// B-07：`func` 类型可作为调用的泛型实参。
#[test]
fn b_07_func_type_as_call_type_argument() {
    parse_clean("func f() { g<func (a: i32) -> i32>(id); }");
}

/// B-08/C-04/D-01：`new` 目标是整条后缀表达式；顶层 Call 提升实参，
/// 泛型实参保留在 `New.type_args`。
#[test]
fn b_08_c_04_d_01_new_expression() {
    let result = parse_clean("func f() { let a = new getFoo()(); let b = new Vec<i32>(1); }");
    let stmts = function_stmts(&result);

    // `new getFoo()()`：target 是 `getFoo()`（调用），实参为空
    let Statement::Variable(a) = &stmts[0] else {
        panic!("期望变量声明，实际：{:?}", stmts[0]);
    };
    let Some(Expression::New {
        target,
        type_args,
        args,
        ..
    }) = &a.value
    else {
        panic!("期望 new 表达式，实际：{:?}", a.value);
    };
    assert!(
        matches!(target.as_ref(), Expression::Call { .. }),
        "new 目标应为整条调用表达式，实际：{target:?}"
    );
    assert!(type_args.is_empty() && args.is_empty());

    // `new Vec<i32>(1)`：泛型实参与构造实参都保留
    let Statement::Variable(b) = &stmts[1] else {
        panic!("期望变量声明，实际：{:?}", stmts[1]);
    };
    let Some(Expression::New {
        type_args, args, ..
    }) = &b.value
    else {
        panic!("期望 new 表达式，实际：{:?}", b.value);
    };
    assert_eq!(type_args.len(), 1, "泛型实参丢失（D-01）");
    assert_eq!(args.len(), 1);
}

// ---------- C 类：AST 结构误判 ----------

/// C-01：with 绑定支持元组解构。
#[test]
fn c_01_with_tuple_binding() {
    let result = parse_clean("func f() { with pair() as (a, b) { g(a, b); } }");
    let Statement::With(with_stmt) = &function_stmts(&result)[0] else {
        panic!("期望 with 语句，实际：{:?}", function_stmts(&result)[0]);
    };
    let binding = with_stmt.binding.as_ref().expect("期望绑定子句存在");
    assert!(
        matches!(binding.pattern, BindingPattern::Tuple { .. }),
        "期望元组解构绑定，实际：{:?}",
        binding.pattern
    );
}

/// C-02：语句位孤分号是空语句节点（循环体可为空语句）。
#[test]
fn c_02_empty_statement() {
    let result = parse_clean("func f() { while (x); }");
    let Statement::While { body, .. } = &function_stmts(&result)[0] else {
        panic!("期望 while 语句，实际：{:?}", function_stmts(&result)[0]);
    };
    assert!(
        matches!(body.as_ref(), Statement::Empty { .. }),
        "空语句应产出 Statement::Empty，实际：{body:?}"
    );
}

/// C-03：for 条件中嵌套括号内的 `in` 不触发 for-in 判定（`in` 不是表达式
/// 运算符，嵌套形态本就是非法输入；此处回归的是判定逻辑本身——同层 `in`
/// 必须正确识别，嵌套 `in` 不得抢先命中）。
#[test]
fn c_03_for_in_requires_top_level_in() {
    // 顶层 `in`（含解构模式）：for-in
    let result = parse_clean("func f() { for ((a, b) in m) {} }");
    let Statement::For(for_stmt) = &function_stmts(&result)[0] else {
        panic!("期望 for 语句，实际：{:?}", function_stmts(&result)[0]);
    };
    assert!(matches!(for_stmt.condition, ForKind::ForIn(_)));

    // 括号分组后的顶层 `in`：仍是 for-in
    let result = parse_clean("func f() { for ((a) in m) {} }");
    let Statement::For(for_stmt) = &function_stmts(&result)[0] else {
        panic!("期望 for 语句，实际：{:?}", function_stmts(&result)[0]);
    };
    assert!(matches!(for_stmt.condition, ForKind::ForIn(_)));

    // `in` 只出现在嵌套括号内：不得判成 for-in（输入本身非法，
    // 只断言没有静默产出 ForIn 结构）
    let result = parse("func f() { for (arr[in k] = 0; i < 3; i++) {} }");
    let is_for_in = result.program.items.iter().any(|item| {
        matches!(
            item,
            Item::Function(func)
                if func.block.as_ref().is_some_and(|body| {
                    matches!(
                        body.as_ref(),
                        Statement::Block(block)
                            if block.stmts.iter().any(|stmt| matches!(
                                stmt,
                                Statement::For(f) if matches!(f.condition, ForKind::ForIn(_))
                            ))
                    )
                })
        )
    });
    assert!(!is_for_in, "嵌套 `in` 不应触发 for-in 判定");
}

/// C-05：match 的 default 重复 / default 之后的 case 都有诊断。
#[test]
fn c_05_match_default_diagnostics() {
    let dup = parse("func f() { let x = match a { case 1 => 1 default => 2 default => 3 }; }");
    assert_has_diagnostic(&dup, "match 中只允许一个 default 分支");

    let after = parse("func f() { let x = match a { default => 2 case 1 => 1 }; }");
    assert_has_diagnostic(&after, "case 分支不能出现在 default 之后");

    // 正常形态（default 在末尾）零诊断
    parse_clean("func f() { let x = match a { case 1 => 1 default => 2 }; }");
}

/// C-06：多绑定声明产出 VariableList，不包 Block。
#[test]
fn c_06_multi_binding_is_variable_list() {
    let result = parse_clean("func f() { let a = 1, b = 2; }");
    let Statement::VariableList { declarations, .. } = &function_stmts(&result)[0] else {
        panic!("期望 VariableList，实际：{:?}", function_stmts(&result)[0]);
    };
    assert_eq!(declarations.len(), 2);
}

// ---------- D 类：AST 信息丢失 ----------

/// D-03/E-01：字段上的编译器指令保留。
#[test]
fn d_03_field_attributes_kept() {
    let result = parse_clean("class C { @obs count: i32; }");
    let Some(Item::Class(class)) = result.program.items.first() else {
        panic!("期望 class，实际：{:?}", result.program.items);
    };
    let Some(ClassMember::Field(field)) = class.members.first() else {
        panic!("期望字段成员，实际：{:?}", class.members);
    };
    assert_eq!(field.directives.len(), 1, "字段编译器指令丢失（D-03）");
}

/// D-04：trait 可见性保留。
#[test]
fn d_04_trait_scope() {
    let result = parse_clean("pub trait T {}");
    let Some(Item::Trait(trait_item)) = result.program.items.first() else {
        panic!("期望 trait，实际：{:?}", result.program.items);
    };
    assert_eq!(trait_item.scope, Some(Scope::Public));
}

/// D-05：extern 函数上的编译器指令保留。
#[test]
fn d_05_extern_attributes() {
    let result = parse_clean("@link(\"c\") extern func puts(s: i8*): i32;");
    let Some(Item::Function(func)) = result.program.items.first() else {
        panic!("期望 extern func，实际：{:?}", result.program.items);
    };
    assert!(func.is_extern);
    assert_eq!(func.directives.len(), 1, "extern 编译器指令丢失（D-05）");
}

/// D-06：struct 继承子句保留（是否允许由 sema 判定）。
#[test]
fn d_06_struct_heritage_kept() {
    let result = parse_clean("struct S : Base {}");
    let Some(Item::Struct(struct_item)) = result.program.items.first() else {
        panic!("期望 struct，实际：{:?}", result.program.items);
    };
    assert_eq!(struct_item.heritage.len(), 1, "struct 继承子句丢失（D-06）");
}

/// D-07/D-09：item 的 span 覆盖编译器指令与可见性。
#[test]
fn d_07_d_09_item_span_starts_at_modifiers() {
    let src = "  async func f() {}\n  pub class C {}";
    let result = parse_clean(src);
    let Some(Item::Function(func)) = result.program.items.first() else {
        panic!("期望 func，实际：{:?}", result.program.items);
    };
    assert_eq!(func.span.start, 2, "async func 的 span 应从 async 起算");
    let Some(Item::Class(class)) = result.program.items.get(1) else {
        panic!("期望 class，实际：{:?}", result.program.items);
    };
    assert_eq!(class.span.start, src.find("pub").unwrap());
}

/// D-08：导入路径的 `别名:路径` 前缀由 parser 切分。
#[test]
fn d_08_import_prefix() {
    let result = parse_clean("from \"std:exception\" import ValueError;");
    let Some(Item::Import(import)) = result.program.items.first() else {
        panic!("期望 import，实际：{:?}", result.program.items);
    };
    assert_eq!(import.prefix.as_deref(), Some("std"));
    assert_eq!(import.path, "exception");
    assert!(matches!(import.kind, ImportKind::Named { .. }));

    // 无前缀形态不受影响
    let result = parse_clean("import \"novella.std\";");
    let Some(Item::Import(import)) = result.program.items.first() else {
        panic!("期望 import，实际：{:?}", result.program.items);
    };
    assert_eq!(import.prefix, None);
    assert_eq!(import.path, "novella.std");
}

// ---------- E 类：规范拍板 ----------

/// E-03：类型位唯一箭头是 `->`，`=>` 在类型位报诊断；
/// lambda 返回注解（内嵌类型位）是例外。
#[test]
fn e_03_arrow_in_type_position_rejected() {
    let alias = parse("type F = (A) => B;");
    assert_has_diagnostic(&alias, "类型位不支持 `=>`");

    let param = parse("func f(x: func (a: i32) => i32) {}");
    assert_has_diagnostic(&param, "类型位不支持 `=>`");

    // lambda 返回注解是内嵌类型位（E-03 例外），零诊断
    parse_clean("func f() { let g = (x: i32): i32 => x; }");
}

/// E-05：方法必须以 `func` 开头；`constructor(...)` / `name(...)` 省略形态报诊断。
#[test]
fn e_05_method_requires_func_keyword() {
    let bare = parse("class C { constructor() {} }");
    assert_has_diagnostic(&bare, "方法声明须以 `func` 开头");

    let named = parse("class C { run() {} }");
    assert_has_diagnostic(&named, "方法声明须以 `func` 开头");

    parse_clean("class C { func run() {} }");
}

/// E-06：`weak` 不能修饰方法。
#[test]
fn e_06_weak_method_rejected() {
    let result = parse("class C { weak func m() {} }");
    assert_has_diagnostic(&result, "`weak` 不能修饰方法");

    parse_clean("class C { weak next: Node; }");
}

/// E-07：`()` 与 `(a,)` 在表达式位与类型位都是语法错误。
#[test]
fn e_07_tuple_arity() {
    let empty_expr = parse("func f() { let a = (); }");
    assert_has_diagnostic(&empty_expr, "空元组 `()` 不是合法表达式");

    let single_expr = parse("func f() { let a = (1,); }");
    assert_has_diagnostic(&single_expr, "单元素元组");

    let empty_ty = parse("func f() { let a: () = x; }");
    assert_has_diagnostic(&empty_ty, "空元组");

    // 正常元组与空调用不受影响
    parse_clean("func f() { let a = (1, 2); run(); }");
}

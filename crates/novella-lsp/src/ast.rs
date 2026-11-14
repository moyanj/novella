//! AST 遍历：从语法树里抽取「可导航结构」（文档符号、折叠区间）。
//!
//! 本模块只做**一次深度遍历**，把散落在各节点里的名字与区间收集成扁平的
//! 内部结构（[`Outline`]），再由 [`crate::features`] 转成 lsp-types 的形态。
//! 这样做的原因：
//! - `novella-ast` 没有 visitor 抽象，逐个节点写两遍遍历（符号一次、折叠一次）
//!   会重复且容易漏分支；
//! - LSP 类型不适合作为递归函数的中间产物（`DocumentSymbol` 需要 `deprecated`、
//!   `detail` 等一堆可选字段），先收敛成内部结构更易维护。
//!
//! 节点名字本身**不带 span**（`ClassItem.name` / `FunctionItem.name` 等都是纯
//! `String`），只有整个声明带 span。因此名字的源码区间由直接在声明区间内按
//! 「整词 + 前后无标识符字符」的方式查找（[`locate_name`]）。

use novella_ast::{
    decorator::Decorator,
    expr::{Expression, LambdaBody, LambdaParams, MatchCaseBody, MatchPattern, PropertyAssignment},
    program::{
        ClassItem, ClassMember, FieldMember, FunctionItem, ImportItem, ImportKind, MethodMember,
        Program, StructItem, TraitItem,
    },
    stmt::{
        BindingPattern, Block, CatchClause, ForInit, ForKind, IfBranch, IfStmt, Statement,
        VariableStmt, WithBinding,
    },
    types::{FunctionType, Param, PredefinedType, TypeExpr, TypeParameter},
};
use novella_core::Span;

/// 符号种类（与 LSP `SymbolKind` 的映射在 [`crate::features`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineKind {
    Function,
    Method,
    Class,
    Struct,
    Trait,
    TypeAlias,
    /// 类型参数（`<T>`）与函数参数
    TypeParameter,
    Parameter,
    Field,
    Variable,
    Import,
}

/// 一个可导航符号。
#[derive(Debug, Clone)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: OutlineKind,
    /// 整个声明的区间（跳转 / 选择范围用）
    pub span: Span,
    /// 名字本身的区间（在 `span` 内定位；定位失败时退化为 `span.start` 处的空区间）
    pub name_span: Span,
    /// 父符号在 [`Outline::symbols`] 中的下标，顶层为 `None`
    pub parent: Option<usize>,
    /// 附加说明（签名 / 类型），用于 `DocumentSymbol.detail`
    pub detail: Option<String>,
}

/// 可折叠区间及其种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldKind {
    /// 类 / struct / trait 体
    TypeBody,
    /// 函数体 / 方法体
    FunctionBody,
    /// 普通语句块、控制流块
    Block,
    /// 多行 `if` 链 / `for` 循环
    Conditional,
    /// 多行 `match` 分支
    Match,
    /// 多行 `try` / `catch` / `finally`
    TryBlock,
    /// 连续两行以上的行注释
    Comments,
    /// import 语句连续块
    Imports,
}

/// 一个折叠区间（行号 0-based，闭区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRegion {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: FoldKind,
}

/// 一次遍历的结果。
#[derive(Debug, Clone, Default)]
pub struct Outline {
    pub symbols: Vec<OutlineSymbol>,
    pub folds: Vec<FoldRegion>,
}

/// 遍历整份程序，产出符号树与折叠区间。
///
/// 需要源码文本才能精确定位名字（AST 不保存名字区间），
/// 行号换算由 [`crate::source::SemanticIndex`] 提供。
pub fn outline(program: &Program, source: &str, index: &crate::source::SemanticIndex) -> Outline {
    let mut walker = Walker {
        symbols: Vec::new(),
        folds: Vec::new(),
        source,
        index,
    };
    // import 连续块：一条 item 一条地扫，遇到非 import 就断开
    let mut comment_blanks: Vec<Span> = Vec::new();
    for item in &program.items {
        walker.item(item, None);
        if let novella_ast::program::Item::Import(import) = item {
            comment_blanks.push(import.span);
        }
    }
    walker.merge_imports(&comment_blanks);
    walker.collect_comment_folds();
    Outline {
        symbols: walker.symbols,
        folds: walker.folds,
    }
}

/// 遍历过程中的可变状态。
struct Walker<'a> {
    symbols: Vec<OutlineSymbol>,
    folds: Vec<FoldRegion>,
    source: &'a str,
    index: &'a crate::source::SemanticIndex,
}

impl Walker<'_> {
    /// 记录一个符号，返回它在 `symbols` 里的下标。
    fn push(
        &mut self,
        name: &str,
        kind: OutlineKind,
        span: Span,
        parent: Option<usize>,
        detail: Option<String>,
    ) -> usize {
        let name_span = locate_name(self.source, span, name);
        let index = self.symbols.len();
        self.symbols.push(OutlineSymbol {
            name: name.to_owned(),
            kind,
            span,
            name_span,
            parent,
            detail,
        });
        index
    }

    /// 记录折叠区间（单行区间不产生折叠；`end` 按闭区间折算到末字节所在行）。
    fn fold(&mut self, span: Span, kind: FoldKind) {
        let start_line = self.index.position(span.start).0;
        let end_line = self.index.position(span.end.saturating_sub(1)).0;
        if end_line > start_line {
            self.folds.push(FoldRegion {
                start_line,
                end_line,
                kind,
            });
        }
    }

    /// 连续的 import 语句合并成一个折叠区间。
    fn merge_imports(&mut self, spans: &[Span]) {
        if spans.len() < 2 {
            return;
        }
        let start_line = self.index.position(spans[0].start).0;
        let end_line = self.index
            .position(spans[spans.len() - 1].end.saturating_sub(1))
            .0;
        if end_line > start_line {
            self.folds.push(FoldRegion {
                start_line,
                end_line,
                kind: FoldKind::Imports,
            });
        }
    }

    /// 收集连续两行以上的行注释块。
    ///
    /// 只用词法结果（`LineComment` token）即可，不必回到 AST。
    fn collect_comment_folds(&mut self) {
        let mut run_start: Option<u32> = None;
        let mut run_end: u32 = 0;
        let flush = |run: &mut Option<u32>, end: u32, folds: &mut Vec<FoldRegion>| {
            if let Some(start) = run.take()
                && end > start
            {
                folds.push(FoldRegion {
                    start_line: start,
                    end_line: end,
                    kind: FoldKind::Comments,
                });
            }
        };
        let lexed = novella_lexer::lexer(self.source);
        for token in &lexed.tokens {
            if token.kind != novella_core::token::TokenKind::LineComment {
                continue;
            }
            let (line, _) = self.index.position(token.span.start);
            match run_start {
                Some(_) if line <= run_end + 1 => run_end = line,
                _ => {
                    flush(&mut run_start, run_end, &mut self.folds);
                    run_start = Some(line);
                    run_end = line;
                }
            }
        }
        flush(&mut run_start, run_end, &mut self.folds);
    }

    // ---------- 顶层 ----------

    fn item(&mut self, item: &novella_ast::program::Item, parent: Option<usize>) {
        use novella_ast::program::Item;
        match item {
            Item::Import(import) => self.import(import, parent),
            Item::TypeAlias(alias) => {
                let detail = Some(format!("= {}", type_expr_text(&alias.alias)));
                self.push(&alias.name, OutlineKind::TypeAlias, alias.span, parent, detail);
                for parameter in &alias.type_parameters {
                    self.type_parameter(parameter, None);
                }
            }
            Item::Trait(trait_item) => self.trait_item(trait_item, parent),
            Item::Class(class) => self.class_item(class, parent),
            Item::Struct(struct_item) => self.struct_item(struct_item, parent),
            Item::Function(function) => self.function(function, parent),
            Item::Decorator(decorator) => self.decorator(decorator),
            Item::DecoratorBlock { decorator, block, span } => {
                self.decorator(decorator);
                if let Some(block) = block {
                    self.block(block, parent);
                }
                self.fold(*span, FoldKind::Block);
            }
            Item::Statement(statement) => self.statement(statement, parent),
        }
    }

    fn decorator(&mut self, _decorator: &Decorator) {
        // 装饰器参数是原始 token 流，不进符号树
    }

    /// 导入：命名空间导入出一个符号，具名导入每个说明符各出一个符号。
    fn import(&mut self, import: &ImportItem, parent: Option<usize>) {
        match &import.kind {
            ImportKind::Named { specifiers, .. } => {
                for specifier in specifiers {
                    // 别名的名字区间落在 specifier 内：优先定位本地别名，找不到再定位原名
                    let local = specifier.local.as_deref().unwrap_or(&specifier.imported);
                    let detail = Some(format!("from \"{}\"", import.path));
                    self.push(local, OutlineKind::Import, specifier.span, parent, detail);
                }
            }
            ImportKind::Namespace {
                local_alias, span, ..
            } => {
                let detail = Some(format!("module \"{}\"", import.path));
                self.push(local_alias, OutlineKind::Import, *span, parent, detail);
            }
        }
    }

    fn trait_item(&mut self, trait_item: &TraitItem, parent: Option<usize>) {
        let index = self.push(
            &trait_item.name,
            OutlineKind::Trait,
            trait_item.span,
            parent,
            None,
        );
        for parameter in &trait_item.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        self.members(&trait_item.members, index);
        self.fold(trait_item.span, FoldKind::TypeBody);
    }

    fn class_item(&mut self, class: &ClassItem, parent: Option<usize>) {
        let detail = class
            .parent
            .first()
            .map(|parent_ty| format!("extends {}", type_expr_text(parent_ty)));
        let index = self.push(&class.name, OutlineKind::Class, class.span, parent, detail);
        for parameter in &class.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        for parent_ty in &class.parent {
            self.type_expr(parent_ty, index);
        }
        self.members(&class.members, index);
        self.fold(class.span, FoldKind::TypeBody);
    }

    fn struct_item(&mut self, struct_item: &StructItem, parent: Option<usize>) {
        let index = self.push(
            &struct_item.name,
            OutlineKind::Struct,
            struct_item.span,
            parent,
            None,
        );
        for parameter in &struct_item.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        for heritage in &struct_item.heritage {
            self.type_expr(heritage, index);
        }
        self.members(&struct_item.members, index);
        self.fold(struct_item.span, FoldKind::TypeBody);
    }

    fn function(&mut self, function: &FunctionItem, parent: Option<usize>) {
        let detail = signature_text(
            function.r#type.as_ref().map(|ty| ty.params.as_slice()),
            function.r#type.as_ref().map(|ty| ty.return_type.as_ref()),
        );
        let index = self.push(
            &function.name,
            OutlineKind::Function,
            function.span,
            parent,
            detail,
        );
        if let Some(function_type) = &function.r#type {
            self.function_type(function_type, Some(index));
        }
        if let Some(block) = &function.block {
            self.statement(block, Some(index));
        }
        self.fold(function.span, FoldKind::FunctionBody);
    }

    fn members(&mut self, members: &[ClassMember], parent: usize) {
        for member in members {
            match member {
                ClassMember::Field(field) => self.field(field, parent),
                ClassMember::Method(method) => self.method(method, parent),
            }
        }
    }

    fn field(&mut self, field: &FieldMember, parent: usize) {
        let detail = field.ty.as_ref().map(type_expr_text);
        let index = self.push(&field.name, OutlineKind::Field, field.span, Some(parent), detail);
        if let Some(ty) = &field.ty {
            self.type_expr(ty, index);
        }
        if let Some(initializer) = &field.initializer {
            self.expression(initializer, index);
        }
    }

    fn method(&mut self, method: &MethodMember, parent: usize) {
        let detail = signature_text(Some(&method.params), method.return_type.as_ref());
        let index = self.push(
            &method.name,
            OutlineKind::Method,
            method.span,
            Some(parent),
            detail,
        );
        for parameter in &method.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        for parameter in &method.params {
            self.param(parameter, index);
        }
        if let Some(return_type) = &method.return_type {
            self.type_expr(return_type, index);
        }
        if let Some(block) = &method.block {
            self.block(block, Some(index));
        }
        self.fold(method.span, FoldKind::FunctionBody);
    }

    fn type_parameter(&mut self, parameter: &TypeParameter, parent: Option<usize>) {
        let index = self.push(
            &parameter.name,
            OutlineKind::TypeParameter,
            parameter.span,
            parent,
            None,
        );
        if let Some(bound) = &parameter.bound {
            self.type_expr(bound, index);
        }
    }

    fn param(&mut self, param: &Param, parent: usize) {
        let detail = param.ty.as_ref().map(type_expr_text);
        let index = self.push(
            &param.name,
            OutlineKind::Parameter,
            param.span,
            Some(parent),
            detail,
        );
        if let Some(ty) = &param.ty {
            self.type_expr(ty, index);
        }
        if let Some(default) = &param.default {
            self.expression(default, index);
        }
    }

    fn function_type(&mut self, function_type: &FunctionType, parent: Option<usize>) {
        for parameter in &function_type.type_parameters {
            self.type_parameter(parameter, parent);
        }
        if let Some(parent) = parent {
            for parameter in &function_type.params {
                self.param(parameter, parent);
            }
        }
        if let Some(parent) = parent {
            self.type_expr(&function_type.return_type, parent);
        }
    }

    // ---------- 类型表达式 ----------

    fn type_expr(&mut self, ty: &TypeExpr, parent: usize) {
        match ty {
            TypeExpr::Predefined { .. } => {}
            TypeExpr::Reference { generic_args, .. } => {
                for argument in generic_args {
                    self.type_expr(argument, parent);
                }
            }
            TypeExpr::Array { element, .. } | TypeExpr::Pointer { element, .. } => {
                self.type_expr(element, parent)
            }
            TypeExpr::Tuple { elements, .. } | TypeExpr::Union { types: elements, .. } => {
                for element in elements {
                    self.type_expr(element, parent);
                }
            }
            TypeExpr::Function(function_type) => self.function_type(function_type, Some(parent)),
        }
    }

    // ---------- 语句 ----------

    fn block(&mut self, block: &Block, parent: Option<usize>) {
        for statement in &block.stmts {
            self.statement(statement, parent);
        }
        self.fold(block.span, FoldKind::Block);
    }

    fn statement(&mut self, statement: &Statement, parent: Option<usize>) {
        match statement {
            Statement::Variable(variable) => self.variable(variable, parent),
            Statement::VariableList { declarations, .. } => {
                for declaration in declarations {
                    self.variable(declaration, parent);
                }
            }
            Statement::Empty { .. } | Statement::Break { .. } | Statement::Continue { .. } => {}
            Statement::Return { value, .. } | Statement::Yield { value, .. } => {
                if let (Some(index), Some(value)) = (parent, value) {
                    self.expression(value, index);
                }
            }
            Statement::Throw { expr, .. } => {
                if let Some(index) = parent {
                    self.expression(expr, index);
                }
            }
            Statement::Block(block) => self.block(block, parent),
            Statement::If(if_statement) => self.if_statement(if_statement, parent),
            Statement::While {
                condition, body, span,
            } => {
                if let Some(index) = parent {
                    self.expression(condition, index);
                }
                self.statement(body, parent);
                self.fold(*span, FoldKind::Block);
            }
            Statement::For(for_statement) => {
                if let Some(index) = parent {
                    self.for_kind(&for_statement.condition, index);
                }
                self.statement(&for_statement.block, parent);
                self.fold(for_statement.span, FoldKind::Conditional);
            }
            Statement::Try(try_statement) => {
                if let Some(index) = parent {
                    for catch in &try_statement.catches {
                        self.catch_clause(catch, index);
                    }
                }
                self.block(&try_statement.block, parent);
                if let Some(finally) = &try_statement.finally {
                    self.block(finally, parent);
                }
                self.fold(try_statement.span, FoldKind::TryBlock);
            }
            Statement::With(with_statement) => {
                if let Some(index) = parent {
                    self.expression(&with_statement.expr, index);
                    if let Some(binding) = &with_statement.binding {
                        self.with_binding(binding, index);
                    }
                }
                self.block(&with_statement.block, parent);
                self.fold(with_statement.span, FoldKind::Block);
            }
            Statement::Unsafe { block, span } => {
                self.block(block, parent);
                self.fold(*span, FoldKind::Block);
            }
            Statement::Expression(expression) => {
                if let Some(index) = parent {
                    self.expression(expression, index);
                }
            }
        }
    }

    fn for_kind(&mut self, kind: &ForKind, parent: usize) {
        match kind {
            ForKind::ForIn(for_in) => {
                self.binding_pattern(&for_in.left, parent, None);
                self.expression(&for_in.right, parent);
            }
            ForKind::ForLoop(for_loop) => {
                match &for_loop.init {
                    Some(ForInit::Declaration { declarations, .. }) => {
                        for declaration in declarations {
                            self.variable(declaration, Some(parent));
                        }
                    }
                    Some(ForInit::Expression { expr, .. }) => self.expression(expr, parent),
                    None => {}
                }
                if let Some(condition) = &for_loop.condition {
                    self.expression(condition, parent);
                }
                if let Some(update) = &for_loop.update {
                    self.expression(update, parent);
                }
            }
        }
    }

    fn variable(&mut self, variable: &VariableStmt, parent: Option<usize>) {
        let Some(index) = parent else {
            return;
        };
        let detail = variable.ty.as_ref().map(type_expr_text);
        self.binding_pattern(&variable.pattern, index, detail);
        if let Some(ty) = &variable.ty {
            self.type_expr(ty, index);
        }
        if let Some(value) = &variable.value {
            self.expression(value, index);
        }
    }

    fn binding_pattern(&mut self, pattern: &BindingPattern, parent: usize, detail: Option<String>) {
        match pattern {
            BindingPattern::Ident { name, span } => {
                self.push(name, OutlineKind::Variable, *span, Some(parent), detail);
            }
            BindingPattern::Tuple { elements, .. } => {
                for element in elements {
                    self.binding_pattern(element, parent, None);
                }
            }
        }
    }

    fn catch_clause(&mut self, catch: &CatchClause, parent: usize) {
        let detail = catch.ty.as_ref().map(type_expr_text);
        let index = self.push(
            &catch.name,
            OutlineKind::Variable,
            catch.span,
            Some(parent),
            detail,
        );
        if let Some(ty) = &catch.ty {
            self.type_expr(ty, index);
        }
        self.block(&catch.block, Some(index));
    }

    fn with_binding(&mut self, binding: &WithBinding, parent: usize) {
        let detail = binding.ty.as_ref().map(type_expr_text);
        self.binding_pattern(&binding.pattern, parent, detail);
        if let Some(ty) = &binding.ty {
            self.type_expr(ty, parent);
        }
    }

    fn if_statement(&mut self, if_statement: &IfStmt, parent: Option<usize>) {
        // if 链整体作为一条「多行条件块」折叠，不在每个分支重复折叠
        for branch in &if_statement.branches {
            match branch {
                IfBranch::ElseIf {
                    condition, block, ..
                } => {
                    if let Some(index) = parent {
                        self.expression(condition, index);
                    }
                    self.statement(block, parent);
                }
                IfBranch::Else { block, .. } => self.statement(block, parent),
            }
        }
        self.fold(if_statement.span, FoldKind::Conditional);
    }

    // ---------- 表达式 ----------

    fn expression(&mut self, expression: &Expression, parent: usize) {
        match expression {
            Expression::Literal { .. }
            | Expression::This { .. }
            | Expression::Identifier { .. } => {}
            Expression::Paren { expr, .. } | Expression::Await { expr, .. } => {
                self.expression(expr, parent)
            }
            Expression::Unary { operand, .. } => self.expression(operand, parent),
            Expression::Tuple { elements, .. } | Expression::ArrayLiteral { elements, .. } => {
                for element in elements {
                    self.expression(element, parent);
                }
            }
            Expression::ObjectLiteral { properties, .. }
            | Expression::StructInit { properties, .. } => {
                for property in properties {
                    self.property(property, parent);
                }
            }
            Expression::Binary { left, right, .. } => {
                self.expression(left, parent);
                self.expression(right, parent);
            }
            Expression::Is { operand, ty, .. } | Expression::Cast { expr: operand, ty, .. } => {
                self.expression(operand, parent);
                self.type_expr(ty, parent);
            }
            Expression::Ternary {
                condition,
                then_expr,
                else_expr,
                ..
            } => {
                self.expression(condition, parent);
                self.expression(then_expr, parent);
                self.expression(else_expr, parent);
            }
            Expression::Assignment { target, value, .. } => {
                self.expression(target, parent);
                self.expression(value, parent);
            }
            Expression::MemberAccess { target, .. } => self.expression(target, parent),
            Expression::IndexAccess { target, index, .. } => {
                self.expression(target, parent);
                self.expression(index, parent);
            }
            Expression::Call {
                callee,
                type_args,
                args,
                ..
            }
            | Expression::New {
                target: callee,
                type_args,
                args,
                ..
            } => {
                self.expression(callee, parent);
                for argument in args {
                    self.expression(argument, parent);
                }
                for type_arg in type_args {
                    self.type_expr(type_arg, parent);
                }
            }
            Expression::MacroCall { callee, args, .. } => {
                self.expression(callee, parent);
                for argument in args {
                    self.expression(argument, parent);
                }
            }
            Expression::Match(match_expression) => {
                self.expression(&match_expression.subject, parent);
                for case in &match_expression.cases {
                    match &case.pattern {
                        MatchPattern::Expr { expr, .. } => self.expression(expr, parent),
                        MatchPattern::Type { ty, .. } => self.type_expr(ty, parent),
                    }
                    self.match_case_body(&case.body, parent);
                }
                if let Some(default) = &match_expression.default {
                    self.match_case_body(default, parent);
                }
                self.fold(match_expression.span, FoldKind::Match);
            }
            Expression::Lambda(lambda) => {
                if let LambdaParams::List { params, .. } = &lambda.params {
                    for parameter in params {
                        self.param(parameter, parent);
                    }
                }
                if let Some(return_type) = &lambda.return_type {
                    self.type_expr(return_type, parent);
                }
                match &lambda.body {
                    LambdaBody::Expr { expr, .. } => self.expression(expr, parent),
                    LambdaBody::Block { block, .. } => self.block(block, Some(parent)),
                }
            }
        }
    }

    fn property(&mut self, property: &PropertyAssignment, parent: usize) {
        if let Some(value) = &property.value {
            self.expression(value, parent);
        }
    }

    fn match_case_body(&mut self, body: &MatchCaseBody, parent: usize) {
        match body {
            MatchCaseBody::Expr { expr, .. } => self.expression(expr, parent),
            MatchCaseBody::Block { block, .. } => self.block(block, Some(parent)),
        }
    }
}

/// 在声明区间内定位名字的源码区间。
///
/// AST 只保存整条声明的 span 与字符串名字，因此在声明前缀里找**第一个**出现的
/// 完整名字（前后不能紧邻标识符字符），避免把 `Foo` 定位到 `Foobar` 里。
/// 找不到时退化为 `span.start` 处的空区间——客户端跳转仍指向声明起点。
fn locate_name(source: &str, span: Span, name: &str) -> Span {
    let start = span.start.min(source.len());
    let end = span.end.min(source.len()).max(start);
    let haystack = &source[start..end];
    if name.is_empty() {
        return Span { start, end: start };
    }
    let mut from = 0;
    while let Some(found) = haystack[from..].find(name) {
        let begin = from + found;
        let stop = begin + name.len();
        let before_ok = haystack[..begin]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_ident_char(ch));
        let after_ok = haystack[stop..]
            .chars()
            .next()
            .is_none_or(|ch| !is_ident_char(ch));
        if before_ok && after_ok {
            return Span {
                start: start + begin,
                end: start + stop,
            };
        }
        from = stop;
    }
    Span { start, end: start }
}

/// 标识符字符判定（与 lexer 的标识符字符集保持一致：字母、数字、`_`、`$`）。
fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// 由参数与返回类型拼一个签名摘要（写入 `DocumentSymbol.detail`）。
fn signature_text(params: Option<&[Param]>, return_type: Option<&TypeExpr>) -> Option<String> {
    let params = params?;
    let params = params
        .iter()
        .map(|param| match &param.ty {
            Some(ty) => format!("{}: {}", param.name, type_expr_text(ty)),
            None => param.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    match return_type {
        Some(ty) => Some(format!("({params}) -> {}", type_expr_text(ty))),
        None => Some(format!("({params})")),
    }
}

/// 类型表达式的简短文本（写入 `DocumentSymbol.detail` / 补全详情）。
///
/// 只做结构还原，不做解析：形如 `int[]`、`(int, string)`、`int*`。
pub fn type_expr_text(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Predefined { ty, .. } => predefined_text(*ty).to_owned(),
        TypeExpr::Reference {
            qualifier,
            name,
            generic_args,
            ..
        } => {
            let mut text = String::new();
            for segment in qualifier {
                text.push_str(segment);
                text.push('.');
            }
            text.push_str(name);
            if !generic_args.is_empty() {
                text.push('<');
                text.push_str(
                    &generic_args
                        .iter()
                        .map(type_expr_text)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                text.push('>');
            }
            text
        }
        TypeExpr::Array { element, .. } => format!("{}[]", type_expr_text(element)),
        TypeExpr::Pointer { element, .. } => format!("{}*", type_expr_text(element)),
        TypeExpr::Tuple { elements, .. } => format!(
            "({})",
            elements
                .iter()
                .map(type_expr_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeExpr::Function(function_type) => function_type_text(function_type),
        TypeExpr::Union { types, .. } => types
            .iter()
            .map(type_expr_text)
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

fn function_type_text(function_type: &FunctionType) -> String {
    let params = function_type
        .params
        .iter()
        .map(|param| match &param.ty {
            Some(ty) => format!("{}: {}", param.name, type_expr_text(ty)),
            None => param.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "func({params}) -> {}",
        type_expr_text(&function_type.return_type)
    )
}

fn predefined_text(ty: PredefinedType) -> &'static str {
    use PredefinedType as P;
    match ty {
        P::Null => "null",
        P::Boolean => "boolean",
        P::Int => "int",
        P::Float => "float",
        P::Void => "void",
        P::U8 => "u8",
        P::U16 => "u16",
        P::U32 => "u32",
        P::U64 => "u64",
        P::I8 => "i8",
        P::I16 => "i16",
        P::I32 => "i32",
        P::I64 => "i64",
        P::F8 => "f8",
        P::F16 => "f16",
        P::F32 => "f32",
        P::F64 => "f64",
        P::Usize => "usize",
        P::Isize => "isize",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use novella_parser::Parser;

    /// 解析一段源码（过滤行注释），返回 AST 与偏移索引。
    fn build(text: &str) -> (Program, crate::source::SemanticIndex) {
        let lexed = novella_lexer::lexer(text);
        let tokens = lexed
            .tokens
            .into_iter()
            .filter(|token| token.kind != novella_core::token::TokenKind::LineComment)
            .collect::<Vec<_>>();
        let mut parser = Parser::new(tokens);
        let parsed = parser.parse();
        let index = crate::source::SemanticIndex::new(text);
        (parsed.program, index)
    }

    /// 类型 / 函数 / 字段应进入符号树，名字区间需精确落在声明处。
    #[test]
    fn outline_collects_declarations() {
        let source = concat!(
            "class Shape {\n",
            "    pub name: string;\n",
            "    pub func area(factor: int) -> int {\n",
            "        return factor;\n",
            "    }\n",
            "}\n",
            "func main() {\n",
            "    let count = 0;\n",
            "}\n",
        );
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);

        let names = outline
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"Shape"), "类应进符号树：{names:?}");
        assert!(names.contains(&"name"), "字段应进符号树：{names:?}");
        assert!(names.contains(&"area"), "方法应进符号树：{names:?}");
        assert!(names.contains(&"main"), "函数应进符号树：{names:?}");
        assert!(names.contains(&"count"), "局部变量应进符号树：{names:?}");

        // 名字区间必须落在源码里且与名字一致
        for symbol in &outline.symbols {
            let text = &source[symbol.name_span.start..symbol.name_span.end];
            assert_eq!(text, symbol.name, "名字区间应精确：{:?}", symbol);
        }
        // 方法挂在类下面
        let area = outline
            .symbols
            .iter()
            .find(|symbol| symbol.name == "area")
            .expect("存在 area");
        let parent = area.parent.map(|index| &outline.symbols[index]);
        assert_eq!(parent.map(|symbol| symbol.name.as_str()), Some("Shape"));
    }

    /// 多行类型体 / 函数体 / 语句块应产生折叠区间，且函数体区间被外层包裹。
    #[test]
    fn outline_folds_multiline_bodies() {
        let source = concat!(
            "class Shape {\n",
            "    func area() -> int {\n",
            "        if (true) {\n",
            "            return 1;\n",
            "        }\n",
            "        return 0;\n",
            "    }\n",
            "}\n",
        );
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);

        assert!(
            outline
                .folds
                .iter()
                .any(|fold| fold.kind == FoldKind::TypeBody),
            "应折叠类体"
        );
        assert!(
            outline
                .folds
                .iter()
                .any(|fold| fold.kind == FoldKind::FunctionBody),
            "应折叠方法体"
        );
        assert!(
            outline
                .folds
                .iter()
                .any(|fold| fold.kind == FoldKind::Conditional),
            "应折叠 if 链"
        );
        // 类体从第 1 行（0-based）延续到末行
        let type_body = outline
            .folds
            .iter()
            .find(|fold| fold.kind == FoldKind::TypeBody)
            .expect("类体折叠");
        assert_eq!(type_body.start_line, 0);
    }

    /// 单行结构不产生折叠，避免客户端出现无意义的折叠箭头。
    #[test]
    fn single_line_structure_is_not_folded() {
        let source = "func f() { return; }\n";
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);
        assert_eq!(outline.folds.len(), 0, "单行函数体不应折叠");
    }

    /// 连续行注释块合并为一个折叠区间。
    #[test]
    fn comment_block_is_folded() {
        let source = "// 第一行\n// 第二行\n// 第三行\nlet x = 1;\n";
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);
        let comments = outline
            .folds
            .iter()
            .find(|fold| fold.kind == FoldKind::Comments)
            .expect("注释块折叠");
        assert_eq!((comments.start_line, comments.end_line), (0, 2));
    }
}
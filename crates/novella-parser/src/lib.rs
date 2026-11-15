use novella_ast::{
    program::Decorator,
    program::{
        ClassItem, ClassMember, FieldMember, FunctionItem, ImportItem, ImportKind, ImportSpecifier,
        Item, MemberModifiers, MethodMember, Program, Scope, StructItem, TraitItem, TypeAliasItem,
    },
    stmt::Statement,
    types::{FunctionType, PredefinedType, TypeExpr},
};
use novella_core::{
    Span,
    diagnostic::{Diagnostic, DiagnosticCode, DiagnosticLocation},
    token::{Token, TokenKind},
};

mod expression;
mod preprocess;
mod recursion;
mod stmt;
mod types;

use preprocess::preprocess;

/// 解析结果：程序 AST 与解析诊断。
///
/// 语法错误不中断解析，全部收集进 `diagnostics`，风格与
/// `novella_lexer::LexerResult` 的"可恢复错误只进 errors"一致。
pub struct ParseResult {
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
    /// 是否因语法错误过多而提前终止（见 [`Parser::MAX_DIAGNOSTICS`]）；
    /// 为 `true` 时程序 AST 不是完整的输入投影，仅保留已解析出的部分。
    pub aborted: bool,
}

/// 诊断条数上限：超过后置位 `aborted` 并停止解析，避免病态输入产生海量诊断。
pub const MAX_DIAGNOSTICS: usize = 1000;

/// 手写递归下降解析器：持有 token 流、游标与诊断收集器，逐条解析顶层 item。
///
/// 只消费 [`Token`]/[`TokenKind`]，不接触 lexer 内部类型；
/// 语法错误就近经 `error_here` 记录并继续解析后续 item。
///
/// 构造时会对 token 流做一次性重写（`from`/`import` 降级、`>>` 拆分），把
/// 「仅由上下文才能区分的歧义」提前消解：
///
/// - `import` 在 `from "m" import ...` 中充当分隔符，在 `import "m";` 中充当起始关键字，
///   两者词法相同；解析时按「当前语句是否以 `from` 开头」决定其种类；
/// - `<` / `>` 在「比较 / 泛型」与「移位」之间是上下文相关的：`>>` 与 `>` `>` 同样
///   逻辑等价（`Vec<Vec<int>>`），解析 `<...>` 实参列表时把 `>>` 拆成两个 `>`。
///
/// 重写只改 `kind`，`slice` / `span` / 行列保持源码原样，JSON 输出仍反映真实输入。
///
/// 注：`(...)` 之后的 `=>` 一律按 lambda 处理；类型位箭头唯一为 `->`，
/// 类型位遇 `=>` 由类型解析就近报诊断（E-03 规范拍板，不再做 token 改判）。
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// 解析期间收集的诊断，`parse()` 结束时取出返回。
    diagnostics: Vec<Diagnostic>,
    /// 当前递归链的深度（每次 [`Parser::guarded`] 进出成对增减）。
    ///
    /// 深度**不再作为递归闸门**：栈安全由分段栈负责，病态输入由
    /// `recursion::MAX_STACK_SEGMENTS` 兜底。该计数只反映「此刻嵌套多深」，
    /// 不跨表达式累计，因此不影响合法输入的解析结果。
    stack_segments: usize,
    /// 是否已报告过「嵌套层级超出工程上限」，避免同一轮刷屏。
    stack_exhausted: bool,
    /// 当前处于 `with (...)` 条件内的层数（>0 时 `as` 优先解释为上下文绑定）。
    pub(crate) with_stack: u32,
    /// 括号 / 实参屏蔽深度：with 表头内嵌套括号中的 `as` 不参与绑定判定（E-02）。
    pub(crate) with_shield: u32,
    /// 类型位宽容开关：lambda 返回注解等内嵌类型位允许 `=>` 作为箭头（E-03 例外）。
    pub(crate) allow_arrow_after_type: bool,
    /// 当前处于 match 分支内的层数（>0 时表达式在 `case` / `default` 处结束）。
    pub(crate) inside_match: u32,
    /// 正在解析 `match` 的主体表达式（此时标识符后的 `{` 属于 match 块，不是 StructInit）。
    pub(crate) in_match_subject: bool,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens: preprocess(tokens),
            pos: 0,
            diagnostics: vec![],
            stack_segments: 0,
            stack_exhausted: false,
            with_stack: 0,
            with_shield: 0,
            allow_arrow_after_type: false,
            inside_match: 0,
            in_match_subject: false,
        }
    }

    // ---------- 游标与前瞻 ----------

    /// 当前 token（不移动游标）；EOF 或流结束返回 None。
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// 前瞻第 n 个 token（不移动游标）；超出流范围返回 None。
    fn peek_n(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.pos + n)
    }

    /// 是否已到达输入末尾（EOF token 或流结束）。
    fn at_eof(&self) -> bool {
        match self.peek() {
            None => true,
            Some(token) => token.kind == TokenKind::Eof,
        }
    }

    /// 返回当前 token 并前进游标（bump 语义）。
    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    // ---------- token 判定与消费 ----------

    /// 当前 token 是否为指定 kind（不消费）。
    fn check(&self, kind: TokenKind) -> bool {
        matches!(self.peek(), Some(token) if token.kind == kind)
    }

    /// 前瞻第 n 个 token 是否为指定 kind（不消费）。
    pub(crate) fn check_n(&self, kind: TokenKind, n: usize) -> bool {
        matches!(self.peek_n(n), Some(token) if token.kind == kind)
    }

    /// 尝试消费指定 kind 的 token；命中则前进并返回该 token，否则返回 None（不移动游标）。
    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        if self.check(kind) {
            self.advance()
        } else {
            None
        }
    }

    /// 必须消费指定 kind 的 token；未命中时记录一条「此处应为 `X`」诊断并返回 None。
    ///
    /// `X` 取自 [`TokenKind::Display`]（字面量写法），失败信息同时带上实际遇到的字符，
    /// 便于定位；措辞通过 [`describe_token`] 换成源码里的样子。
    fn expect(&mut self, kind: TokenKind) -> Option<Token> {
        match self.eat(kind) {
            Some(token) => Some(token),
            None => {
                let found = self
                    .peek()
                    .map(|token| describe_token(&token.kind))
                    .unwrap_or_else(|| "文件结尾".to_string());
                self.error_here(format!("此处应为 `{kind}`，实际遇到 {found}"));
                None
            }
        }
    }

    // ---------- 试探式解析 ----------

    /// 记录游标、诊断长度与递归预算的快照，供试探解析回滚（见 [`Parser::rollback`]）。
    ///
    /// 必须包含 `stack_segments` / `stack_exhausted`：推测分支若在深处耗尽了
    /// 分段预算或消耗了唯一一次「超出工程上限」诊断，回滚时不恢复这些状态，
    /// 真正路径上的解析会在**不报诊断**的情况下静默截断。
    pub(crate) fn checkpoint(&self) -> (usize, usize, usize, bool) {
        (
            self.pos,
            self.diagnostics.len(),
            self.stack_segments,
            self.stack_exhausted,
        )
    }

    /// 回滚到 [`Parser::checkpoint`] 记录的快照（游标 / 诊断 / 递归预算）。
    pub(crate) fn rollback(&mut self, checkpoint: (usize, usize, usize, bool)) {
        self.pos = checkpoint.0;
        self.diagnostics.truncate(checkpoint.1);
        self.stack_segments = checkpoint.2;
        self.stack_exhausted = checkpoint.3;
    }

    // ---------- 尖括号 ----------

    /// 就地把 token 流中 `pos` 起的 `>>` / `<<` 拆成两个单字符 token。
    ///
    /// 用于 `expect_angle_close` 这类「需要一个 `>`，但当前是 `>>`」的场景。
    pub(crate) fn split_shift_at(&mut self, pos: usize) -> bool {
        let Some(token) = self.tokens.get(pos).cloned() else {
            return false;
        };
        let (open, close) = match token.kind {
            TokenKind::RightShiftArithmetic => (TokenKind::MoreThan, TokenKind::MoreThan),
            TokenKind::LeftShiftArithmetic => (TokenKind::LessThan, TokenKind::LessThan),
            _ => return false,
        };
        let (first, second) = split_token_pair(&token, open, close);
        self.tokens[pos] = first;
        self.tokens.insert(pos + 1, second);
        true
    }

    /// 消费闭合 `>`；当前是 `>>` 时就地拆分后只消费前半个。
    pub(crate) fn expect_angle_close(&mut self) -> Option<Token> {
        self.split_shift_at(self.pos);
        self.expect(TokenKind::MoreThan)
    }

    // ---------- 诊断 ----------

    /// 在当前 token 处记录一条错误诊断（不移动游标）；流结束时诊断无位置。
    fn error_here(&mut self, message: impl Into<String>) {
        let mut diag = Diagnostic::error(message).with_code(DiagnosticCode::SyntaxError);
        if let Some(token) = self.peek() {
            diag = diag.with_location(DiagnosticLocation {
                file: None,
                line: token.line,
                column: token.column,
                span: Some(token.span),
            });
        }
        self.diagnostics.push(diag);
    }

    /// 在指定 span 处记录一条错误诊断（行列按 span 起点回推，供「问题出在过去
    /// 某个 token」的场景，如重复的 `default` 分支）。
    pub(crate) fn error_at_span(&mut self, span: Span, message: impl Into<String>) {
        let (line, column) = self
            .tokens
            .iter()
            .find(|t| t.span.start == span.start)
            .map(|t| (t.line, t.column))
            .unwrap_or((0, 0));
        self.diagnostics.push(
            Diagnostic::error(message)
                .with_code(DiagnosticCode::SyntaxError)
                .with_location(DiagnosticLocation {
                    file: None,
                    line,
                    column,
                    span: Some(span),
                }),
        );
    }

    // ---------- 顶层 item 分发 ----------

    /// 解析整个 token 流为程序（对应文法 `program`）。
    pub fn parse(&mut self) -> ParseResult {
        let mut items: Vec<Item> = vec![];
        while !self.at_eof() {
            if self.overflowed() {
                break;
            }
            // 顶层同步点：条目解析未把游标推进到下一行时强制跳过，防止死循环
            let start = self.pos;
            // 单条 item 解析失败时诊断已记录，继续解析后续 item
            if let Some(item) = self.parse_item() {
                items.push(item);
            }
            self.recover_after_item(start);
        }
        // 文件区间：从 0 到 EOF token 的结束位置
        let end = self.tokens.last().map(|t| t.span.end).unwrap_or(0);
        ParseResult {
            program: Program {
                items,
                span: Span { start: 0, end },
            },
            aborted: self.overflowed(),
            diagnostics: std::mem::take(&mut self.diagnostics),
        }
    }

    /// 诊断数量是否已超上限（超限后解析循环应尽快停下）。
    fn overflowed(&self) -> bool {
        self.diagnostics.len() > MAX_DIAGNOSTICS
    }

    /// 顶层同步：条目解析结束后若未推进到新的一行，说明卡在无法恢复的 token 上，
    /// 跳过当前行剩余 token，保证 `parse()` 循环必定前进。
    fn recover_after_item(&mut self, start: usize) {
        if self.pos > start || self.at_eof() {
            return;
        }
        let line = match self.tokens.get(self.pos) {
            Some(token) => token.line,
            None => return,
        };
        while let Some(token) = self.tokens.get(self.pos) {
            if token.kind == TokenKind::Eof || token.line != line {
                break;
            }
            self.pos += 1;
        }
        // 兜底：整行都不前进（理论上不可达）时至少消费一个 token
        if self.pos == start {
            self.pos += 1;
        }
    }

    /// 解析单个顶层 item（对应文法 `sourceElement`）。
    ///
    /// 固定三段顺序，把"修饰符在前、声明关键字在后"的位置约束集中在此处：
    /// ① 装饰器列表（对应文法 `decorator*`，等价 Rust 的 `#[...]`）；
    /// ② 可见性 `pub` / `pri` / `pkg`（对应文法声明前的 `(Pub | Pri | Pkg)?`）；
    /// ③ 按当前 token 分发到各自的 `parse_xxx`。
    ///
    /// 起始关键字一律由本方法按分支自行消费（`advance`），因此游标只会前进、
    /// 绝不回退；各 `parse_xxx` 的前置约定统一为"关键字已被消费，游标指向关键字之后"。
    ///
    /// 文法位置约束在此集中校验（对应示例里"`impl` 不能带 `pub`"那类检查）：
    /// - 装饰器只允许修饰 `@foo;` / `@foo { ... }`（文法 `decoratorStatement`）
    ///   与 `func` / `class` / `trait`（文法中仅这三者带 `decorator*`）；
    /// - 可见性统一写在声明关键字之前，只允许修饰 `class` / `func` / `let` / `const`
    ///   与 `extern`（`pub extern func`，对应文法 `(Pub | Pri | Pkg)? Extern Func` 的
    ///   可见性位）；`extern` 之后不允许再出现可见性（`extern pub func` 是错误写法）。
    fn parse_item(&mut self) -> Option<Item> {
        // item 的 span 起点覆盖装饰器与可见性（D-07/D-09：不能只从关键字起算）
        let item_start = self.peek().map(|token| token.span.start).unwrap_or(0);
        // ① 先看有没有装饰器（属性）
        let attributes = self.parse_decorators();
        // ② 再解析可见性
        let scope = self.parse_visibility();

        // 当前 token 决定了后面是什么 item；TokenKind: Copy，取出后不再借用 self。
        // `async` 是函数声明的修饰前缀（`async func f()`），在这里并入 `func` 分支。
        let kind = match self.peek().map(|token| token.kind) {
            Some(TokenKind::Async) if self.check_n(TokenKind::Func, 1) => TokenKind::Func,
            other => other?,
        };

        if !attributes.is_empty()
            && !matches!(
                kind,
                TokenKind::SemiColon
                    | TokenKind::OpenBrace
                    | TokenKind::Func
                    | TokenKind::Async
                    | TokenKind::Class
                    | TokenKind::Struct
                    | TokenKind::Trait
                    | TokenKind::Extern
                    | TokenKind::Pub
                    | TokenKind::Pri
                    | TokenKind::Pkg
            )
        {
            self.error_here("装饰器后应为 `;`、块 `{...}` 或声明（func / class / trait）");
            return None;
        }
        if scope.is_some() {
            match kind {
                TokenKind::Class
                | TokenKind::Struct
                | TokenKind::Trait
                | TokenKind::Func
                | TokenKind::Let
                | TokenKind::Const
                | TokenKind::Extern => {}
                _ => {
                    self.error_here("`pub` / `pri` / `pkg` 后应为 class / struct / trait / func / extern / let / const 声明");
                    return None;
                }
            }
        }

        // ③ 根据下一个 token 分发
        match kind {
            // 装饰器独立形态：`@foo;`（装饰器语句）与 `@foo { ... }`（装饰块）
            TokenKind::SemiColon | TokenKind::OpenBrace if !attributes.is_empty() => {
                self.parse_decorator_statement(attributes)
            }
            // 空语句 `;`：直接跳过，不产出 item
            TokenKind::SemiColon => {
                self.advance();
                None
            }
            TokenKind::From | TokenKind::Import => {
                self.advance();
                self.parse_import(item_start)
            }
            TokenKind::Type => {
                self.advance();
                self.parse_type_alias(item_start)
            }
            TokenKind::Trait => {
                self.advance();
                self.parse_trait(attributes, scope, item_start)
            }
            TokenKind::Class => {
                self.advance();
                self.parse_class(attributes, scope, item_start)
            }
            TokenKind::Struct => {
                self.advance();
                self.parse_struct(attributes, scope, item_start)
            }
            TokenKind::Func => self.parse_function(attributes, scope, item_start),
            TokenKind::Extern => {
                self.advance();
                self.parse_extern_function(attributes, scope, item_start)
            }
            // 顶层 `pub let` / `pub const`（文法 `variableStatement: Pub? (Const | Let) ...`），
            // 与兜底分支同样包装成 [`Item::Statement`]
            TokenKind::Let | TokenKind::Const => {
                // 可见性已在上面解析并校验过；这里带着它继续解析声明本体
                self.parse_variable_statement(scope)
                    .map(Box::new)
                    .map(Item::Statement)
            }
            // 其余起始关键字按文法 `sourceElement` 的 `statement` 分支兜底，
            // 对应 [`Item::Statement`]；起始 token 由 parse_statement 自行消费。
            _ => self.parse_statement().map(Box::new).map(Item::Statement),
        }
    }

    /// 解析可见性修饰符（对应文法声明前的 `(Pub | Pri | Pkg)?`）。
    ///
    /// 显式 `pub` / `pri` / `pkg` 分别返回 `Some(Scope::Public)` / `Private` / `Package`，
    /// 无修饰符返回 `None`（此时不消费任何 token）。是否允许修饰当前 item
    /// 由 [`Parser::parse_item`] 校验。
    fn parse_visibility(&mut self) -> Option<Scope> {
        let scope = Scope::from_token(&self.peek()?.kind)?;
        self.advance();
        Some(scope)
    }

    // ---------- 各 item 解析 ----------

    /// 解析导入语句（对应文法 `moduleImport`）。
    ///
    /// 前置：`from` / `import` 起始关键字已被 [`Parser::parse_item`] 消费；
    /// `start` 为整条 item 的 span 起点（含装饰器 / 可见性，见 [`Parser::parse_item`]）。
    ///
    /// - `fromImport`: `From StringLiteral Import importSpecifier (',' importSpecifier)* SemiColon`
    /// - `directImport`: `Import StringLiteral SemiColon`
    ///
    /// 路径字面量支持 `别名:路径` 前缀（design.md §4.3.1）：`:` 前的别名写入
    /// [`ImportItem::prefix`]，`:` 后的逻辑路径写入 [`ImportItem::path`]（D-08）。
    fn parse_import(&mut self, start: usize) -> Option<Item> {
        // 起始关键字（`from` / `import`）已被 parse_item 消费，据此区分两种形态
        let from_form = self.tokens[self.pos.saturating_sub(1)].kind == TokenKind::From;
        // 两种形态的路径都是字符串字面量
        let Some(literal) = self.peek().filter(|token| {
            matches!(
                token.kind,
                TokenKind::StringLiteral | TokenKind::RawStringLiteral
            )
        }) else {
            self.error_here("此处应为模块路径字符串");
            return None;
        };
        let literal = literal.clone();
        self.advance();
        let raw_path = literal.slice.clone();
        let quoted_path = unquote(&raw_path);
        // 切分 `别名:路径` 前缀（如 "std:exception" → prefix=std、path=exception）；
        // 只允许非空别名（`":foo"` 按无前缀处理，留待 sema 判定路径合法性）
        let (prefix, path) = match quoted_path.find(':') {
            Some(idx) if idx > 0 => (
                Some(quoted_path[..idx].to_string()),
                quoted_path[idx + 1..].to_string(),
            ),
            _ => (None, quoted_path),
        };
        // 形态分派：`from "m" import a, b as c;`（具名）与 `import "m";`（命名空间）。
        // from 形态的分隔符 `import` 已被预处理降级为标识符（原始字符串路径等
        // 未命中降级条件时仍是关键字，两种形态都要消费掉）。
        let kind = if from_form {
            if self.check(TokenKind::Identifier) {
                self.advance();
            } else {
                self.expect(TokenKind::Import)?;
            }
            // `from "..." import a, b as c;`
            let mut specifiers = vec![];
            loop {
                let imported = self.expect(TokenKind::Identifier)?;
                let mut local = None;
                let mut end = imported.span.end;
                if self.eat(TokenKind::As).is_some() {
                    let alias = self.expect(TokenKind::Identifier)?;
                    end = alias.span.end;
                    local = Some(alias.slice);
                }
                specifiers.push(ImportSpecifier {
                    local,
                    imported: imported.slice,
                    span: Span {
                        start: imported.span.start,
                        end,
                    },
                });
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
            }
            ImportKind::Named {
                span: Span {
                    start,
                    end: self.last_end(),
                },
                specifiers,
            }
        } else {
            // `import "..."`：局部名取路径末段（去掉扩展名），无末段时用全文
            let local_alias = namespace_alias(&path);
            ImportKind::Namespace {
                local_alias,
                span: literal.span,
            }
        };
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("导入语句末尾应为 `;`");
        }
        Some(Item::Import(ImportItem {
            raw_path,
            prefix,
            path,
            kind,
            span: Span {
                start,
                end: self.last_end(),
            },
        }))
    }

    /// 解析类型别名声明（对应文法 `typeAliasDeclaration`）。
    ///
    /// 前置：`type` 关键字已被 [`Parser::parse_item`] 消费。
    /// 语法：`Type identifier typeParameters? Assign typeExpression SemiColon` → [`Item::TypeAlias`]。
    /// `start` 为整条 item 的 span 起点（见 [`Parser::parse_item`]）。
    fn parse_type_alias(&mut self, start: usize) -> Option<Item> {
        let name = self.expect(TokenKind::Identifier)?;
        let type_parameters = self.parse_type_parameters();
        self.expect(TokenKind::Assign)?;
        let alias = self.parse_type_expr()?;
        let end = alias.span().end;
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("类型别名末尾应为 `;`");
        }
        Some(Item::TypeAlias(TypeAliasItem {
            name: name.slice,
            type_parameters,
            alias,
            span: Span { start, end },
        }))
    }

    /// 解析 trait 声明（对应文法 `traitDeclaration`）。
    ///
    /// 前置：`trait` 关键字已被 [`Parser::parse_item`] 消费，游标指向 trait 名。
    /// `attributes` 为声明前的装饰器列表（对应文法 `decorator*`），
    /// `scope` 为可见性（对应文法 `(Pub|Pri|Pkg)?`，D-04），
    /// 均由 [`Parser::parse_item`] 传入；`start` 为整条 item 的 span 起点。
    /// 语法：`decorator* (Pub|Pri|Pkg)? Trait identifier typeParameters? traitBody` → [`Item::Trait`]。
    fn parse_trait(
        &mut self,
        attributes: Vec<Decorator>,
        scope: Option<Scope>,
        start: usize,
    ) -> Option<Item> {
        let name = self.expect(TokenKind::Identifier)?;
        let type_parameters = self.parse_type_parameters();
        let (members, end) = self.parse_member_block()?;
        Some(Item::Trait(TraitItem {
            attributes,
            name: name.slice,
            type_parameters,
            members,
            scope,
            span: Span { start, end },
        }))
    }

    /// 解析类声明（对应文法 `classDeclaration`）。
    ///
    /// 前置：`class` 关键字已被 [`Parser::parse_item`] 消费，游标指向类名。
    /// `_attributes` 为声明前的装饰器列表（对应文法 `decorator*`），
    /// `_scope` 为可见性（对应文法 `Pub?`），均由 [`Parser::parse_item`] 传入。
    /// 语法：`decorator* Pub? Class identifier typeParameters? classHeritage? classBody`
    /// → [`Item::Class`]。`start` 为整条 item 的 span 起点（见 [`Parser::parse_item`]）。
    fn parse_class(
        &mut self,
        attributes: Vec<Decorator>,
        scope: Option<Scope>,
        start: usize,
    ) -> Option<Item> {
        let name = self.expect(TokenKind::Identifier)?;
        let type_parameters = self.parse_type_parameters();
        let parent = self.parse_class_parent();
        let (members, end) = self.parse_member_block()?;
        Some(Item::Class(ClassItem {
            attributes,
            name: name.slice,
            type_parameters,
            parent,
            members,
            scope,
            span: Span { start, end },
        }))
    }

    /// 解析 extern 函数声明（对应文法 `externFunctionDeclaration`）。
    ///
    /// 前置：`extern` 关键字已被 [`Parser::parse_item`] 消费。
    /// `attributes` 为声明前的装饰器列表（D-05：文法补记 `decorator*`，装饰器
    /// 语义如链接名映射由语义分析解释）；`scope` 为可见性（对应文法
    /// `Pub? Extern Func ...` 的 `Pub?`），由 [`Parser::parse_item`] 在 `extern`
    /// 之前解析并传入；`extern` 之后不得再出现可见性修饰符（`extern pub func`
    /// 已在 [`Parser::parse_item`] 拦截报错）。`start` 为整条 item 的 span 起点。
    /// 语法：`decorator* Pub? Extern Func identifier typeParameters? '(' parameterList? ')'`
    /// `typeAnnotation? ReturnArrow? typeExpression? SemiColon`
    /// → [`FunctionItem`]（`is_extern: true`、`block: None`、`scope` 取自入参）。
    fn parse_extern_function(
        &mut self,
        attributes: Vec<Decorator>,
        scope: Option<Scope>,
        start: usize,
    ) -> Option<Item> {
        self.expect(TokenKind::Func)?;
        let name = self.expect(TokenKind::Identifier)?;
        let type_parameters = self.parse_type_parameters();
        let (params, params_span) = self.parse_typed_params()?;
        let (_, return_type) = self.parse_return_type();
        let end = self.last_end();
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("extern 函数声明末尾应为 `;`");
        }
        let function_type = FunctionType {
            type_parameters,
            params,
            return_type: Box::new(return_type.unwrap_or(TypeExpr::Predefined {
                ty: PredefinedType::Void,
                span: params_span,
            })),
            span: Span {
                start: name.span.start,
                end,
            },
        };
        Some(Item::Function(FunctionItem {
            attributes,
            name: name.slice,
            r#type: Some(function_type),
            block: None,
            scope,
            is_extern: true,
            is_async: false,
            name_span: name.span,
            span: Span { start, end },
        }))
    }

    // ---------- 类型声明体（class / trait / struct 共用） ----------

    /// 解析类继承子句（对应文法 `classHeritage`）。
    ///
    /// 主形态为 `ClassName? '(' typeList? ')'`（如 `class Shape(Drawable)`），
    /// 同时兼容 `: Type, Type` 写法。两者可同时出现。
    fn parse_class_parent(&mut self) -> Vec<TypeExpr> {
        let mut parent = vec![];
        if self.eat(TokenKind::OpenParen).is_some() {
            if !self.check(TokenKind::CloseParen) {
                while let Some(ty) = self.parse_type_expr() {
                    parent.push(ty);
                    if self.eat(TokenKind::Comma).is_none() {
                        break;
                    }
                }
            }
            self.expect(TokenKind::CloseParen);
        }
        if self.eat(TokenKind::Colon).is_none() {
            return parent;
        }
        while let Some(ty) = self.parse_type_expr() {
            parent.push(ty);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        parent
    }

    /// 解析成员块 `{ member }`，返回成员列表与块的结束位置。
    fn parse_member_block(&mut self) -> Option<(Vec<ClassMember>, usize)> {
        self.expect(TokenKind::OpenBrace)?;
        let mut members = vec![];
        while !self.check(TokenKind::CloseBrace) && !self.at_eof() {
            if self.overflowed() {
                break;
            }
            let before = self.pos;
            if let Some(member) = self.parse_member() {
                members.push(member);
            }
            if self.pos == before {
                // 无法识别成员：跳到下一个分号或本行行尾，保证前进
                self.recover_to_statement_boundary();
            }
        }
        let close = self.expect(TokenKind::CloseBrace)?;
        Some((members, close.span.end))
    }

    /// 解析单个成员：字段声明或方法声明（对应文法 `fieldDeclaration` / `methodDeclaration`）。
    ///
    /// 两者前缀相同（装饰器 + 可见性 + `static` + `weak`），区别只在于后面是
    /// `func`（方法）还是标识符（字段），因此统一处理前缀后再分派。
    /// 成员的 span 起点覆盖装饰器与全部修饰符（与顶层 item 的 D-07/D-09 同一约定）。
    fn parse_member(&mut self) -> Option<ClassMember> {
        let member_start = self.peek().map(|token| token.span.start).unwrap_or(0);
        let attributes = self.parse_decorators();
        let scope = self.parse_visibility();
        let mut is_static = false;
        let mut is_weak = false;
        // 可见性之后允许 `static` / `weak` 任意顺序出现各一次
        loop {
            match self.peek().map(|token| token.kind) {
                Some(TokenKind::Static) if !is_static => {
                    self.advance();
                    is_static = true;
                }
                Some(TokenKind::Weak) if !is_weak => {
                    self.advance();
                    is_weak = true;
                }
                _ => break,
            }
        }
        let modifiers = MemberModifiers {
            scope,
            is_static,
            is_weak,
        };
        // E-05：方法必须以 `func` 开头；`constructor(...)` / `name(...)` 的省略
        // 形态不受支持。为不丢成员、便于恢复，仍按方法解析并给出诊断；
        // `constructor` 不再做特殊命名——它就是普通标识符方法名。
        let bare_method = matches!(
            (
                self.peek().map(|token| token.kind),
                self.peek_n(1).map(|token| token.kind)
            ),
            (Some(TokenKind::Identifier), Some(TokenKind::OpenParen))
        );
        let method_like = self.check(TokenKind::Func) || bare_method;
        if method_like {
            if bare_method {
                self.error_here(
                    "方法声明须以 `func` 开头（`constructor(...)` / `name(...)` 的省略形态不受支持）",
                );
            }
            // E-06：weak 不修饰方法（弱引用只作用于字段 / 变量的持有方），
            // 报诊断后继续按方法解析
            if is_weak {
                self.error_here("`weak` 不能修饰方法（weak 仅用于字段 / 变量）");
            }
            self.eat(TokenKind::Func);
            let name = self.expect(TokenKind::Identifier)?;
            let type_parameters = self.parse_type_parameters();
            let (params, _) = self.parse_typed_params()?;
            let (_, return_type) = self.parse_return_type();
            let end = self.last_end();
            let block = if self.check(TokenKind::OpenBrace) {
                Some(self.parse_block()?)
            } else {
                if self.eat(TokenKind::SemiColon).is_none() {
                    self.error_here("方法声明末尾应为 `;` 或块 `{ ... }`");
                }
                None
            };
            return Some(ClassMember::Method(MethodMember {
                attributes,
                modifiers,
                name: name.slice,
                type_parameters,
                params,
                return_type,
                block,
                span: Span {
                    start: member_start,
                    end: self.last_end().max(end),
                },
            }));
        }
        // 字段：装饰器按 E-01 保留（合法性由语义分析判定）
        let name = self.expect(TokenKind::Identifier)?;
        let mut ty = None;
        if self.eat(TokenKind::Colon).is_some() {
            ty = Some(self.parse_type_expr()?);
        }
        let mut initializer = None;
        if self.eat(TokenKind::Assign).is_some() {
            initializer = Some(self.parse_expression()?);
        }
        let end = self.last_end();
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("字段声明末尾应为 `;`");
        }
        Some(ClassMember::Field(FieldMember {
            attributes,
            modifiers,
            name: name.slice,
            ty,
            initializer,
            span: Span {
                start: member_start,
                end,
            },
        }))
    }

    // ---------- 装饰器（attribute） ----------

    /// 解析单个装饰器（对应文法 `decorator`）：`@name` 或 `@name(...)`。
    ///
    /// 前置：游标位于 `@`。
    /// 括号内为自由 token 流（允许嵌套括号），原样收集进 [`Decorator::tokens`]
    /// （不含外层括号本身），结构化解释（如 `@cfg(target_os = "linux")`）
    /// 延后到语义分析阶段，符合「值语义延后」的项目约定。
    fn parse_decorator(&mut self) -> Option<Decorator> {
        let at = self.expect(TokenKind::At)?;
        let start = at.span.start;
        let (name, name_end) = match self.peek() {
            Some(token) if token.kind == TokenKind::Identifier => {
                (token.slice.clone(), token.span.end)
            }
            _ => {
                self.error_here("`@` 后应有装饰器名");
                return None;
            }
        };
        self.pos += 1;
        let mut tokens = vec![];
        let mut end = name_end;
        if self.eat(TokenKind::OpenParen).is_some() {
            // 收集到匹配的外层右括号；嵌套括号平铺保留在 token 序列中
            let mut depth = 1usize;
            while let Some(token) = self.advance() {
                end = token.span.end;
                match token.kind {
                    TokenKind::Eof => {
                        // 括号未闭合是装饰器的致命错误，直接失败返回
                        self.error_here("装饰器括号未闭合");
                        return None;
                    }
                    TokenKind::OpenParen => depth += 1,
                    TokenKind::CloseParen => {
                        depth -= 1;
                        if depth == 0 {
                            break; // 外层右括号：end 已更新，结束收集
                        }
                    }
                    _ => {}
                }
                tokens.push(token);
            }
        }
        Some(Decorator {
            name,
            tokens,
            span: Span { start, end },
        })
    }

    /// 解析声明前的装饰器列表（对应文法 `decorator*`）。
    ///
    /// 收集零个或多个 `@name(...)?`，游标停在第一个非 `@` token 处；
    /// 由 [`Parser::parse_item`] 调用，其归属（独立语句 / 装饰块 / 声明前缀）
    /// 由 [`Parser::parse_item`] 按后续 token 决定。
    fn parse_decorators(&mut self) -> Vec<Decorator> {
        let mut decorators = vec![];
        while self.check(TokenKind::At) {
            match self.parse_decorator() {
                Some(decorator) => decorators.push(decorator),
                // 解析失败已报诊断；`@` 已被消费，循环自然退出
                None => break,
            }
        }
        decorators
    }

    /// 解析装饰器独立形态（对应文法 `decoratorStatement: decorator (SemiColon | block)`）。
    ///
    /// 前置：游标位于 `;` 或 `{`（尚未消费），`decorators` 为 [`Parser::parse_item`]
    /// 已收集的非空装饰器列表；装饰器后接 `func` / `class` / `trait` 的声明前缀形态
    /// 不经本方法，由 [`Parser::parse_item`] 直接交给对应的 `parse_xxx`。
    ///
    /// 文法只允许这两种形态各携带单个装饰器，多个装饰器在此报错。
    fn parse_decorator_statement(&mut self, decorators: Vec<Decorator>) -> Option<Item> {
        let start = decorators.first().map(|d| d.span.start)?;
        if decorators.len() != 1 {
            self.error_here("装饰器语句 / 装饰块只允许一个装饰器");
            return None;
        }
        let decorator = decorators.into_iter().next().expect("len == 1 已检查");
        match self.peek().map(|t| t.kind) {
            // `@foo;`：独立装饰器语句
            Some(TokenKind::SemiColon) => {
                let semi = self.advance().expect("已确认当前为分号");
                Some(Item::Decorator(Decorator {
                    span: Span {
                        start,
                        end: semi.span.end,
                    },
                    ..decorator
                }))
            }
            // `@foo { ... }`：装饰块（调用方已限定当前 token 为 `;` 或 `{`）
            _ => {
                let block = self.parse_block()?;
                let end = block.span.end;
                Some(Item::DecoratorBlock {
                    decorator,
                    block: Some(block),
                    span: Span { start, end },
                })
            }
        }
    }

    // ---------- 函数声明 ----------

    /// 解析函数声明（对应文法 `functionDeclaration`）。
    ///
    /// 前置：游标指向 `func` / `async`（本方法自行消费；[`Parser::parse_item`]
    /// 不预消费）。`attributes` 为声明前的装饰器列表（对应文法 `decorator*`），
    /// `scope` 为可见性（对应文法 `Pub?`），均由 [`Parser::parse_item`] 传入；
    /// 无修饰时为空 / `None`。`start` 为整条 item 的 span 起点（含装饰器 /
    /// 可见性，D-07：不能用 `pos-1` 回推——`async func` 会取到 `async`）。
    ///
    /// 语法：`decorator* Pub? Async? Func identifier typeParameters? '(' parameterList? ')'`
    /// `typeAnnotation? ReturnArrow? typeExpression? block` → [`Item::Function`]。
    fn parse_function(
        &mut self,
        attributes: Vec<Decorator>,
        scope: Option<Scope>,
        start: usize,
    ) -> Option<Item> {
        // 入口：`Func` 或 `Async` 开头（后者隐含 `func` 紧随其后）
        let is_async = self.peek().map(|token| token.kind) == Some(TokenKind::Async);
        if is_async {
            self.advance();
        }
        self.expect(TokenKind::Func)?;
        let name = match self.expect(TokenKind::Identifier) {
            Some(name) => name,
            None => {
                self.error_here("`func` 后应为函数名");
                self.skip_to_function_end();
                return None;
            }
        };
        let type_parameters = self.parse_type_parameters();
        let (params, _) = match self.parse_typed_params() {
            Some(params) => params,
            None => {
                self.skip_to_function_end();
                return None;
            }
        };
        let (_, return_type) = self.parse_return_type();
        if !self.check(TokenKind::OpenBrace) {
            self.error_here("函数声明体应以 `{` 开始");
            self.skip_to_function_end();
            return None;
        }
        let block = self.parse_block()?;
        let end = block.span.end;
        let function_type = FunctionType {
            type_parameters,
            params,
            return_type: Box::new(return_type.unwrap_or(TypeExpr::Predefined {
                ty: PredefinedType::Void,
                span: Span { start, end },
            })),
            span: Span { start, end },
        };
        Some(Item::Function(FunctionItem {
            attributes,
            name: name.slice,
            r#type: Some(function_type),
            block: Some(Box::new(Statement::Block(block))),
            scope,
            is_extern: false,
            is_async,
            name_span: name.span,
            span: Span { start, end },
        }))
    }

    /// 结构化声明：`Struct identifier typeParameters? classHeritage? structBody`
    /// → [`Item::Struct`]。
    ///
    /// 文法中 struct 与 class 同构（仅缺 `parent`），故复用成员块解析。
    /// 继承子句按 class 语法解析后保留在 [`StructItem::heritage`]（D-06）：
    /// struct 是否允许继承是语义问题，由语义分析判定并给出诊断，parser 只保留信息。
    /// `start` 为整条 item 的 span 起点（见 [`Parser::parse_item`]）。
    fn parse_struct(
        &mut self,
        attributes: Vec<Decorator>,
        scope: Option<Scope>,
        start: usize,
    ) -> Option<Item> {
        let name = self.expect(TokenKind::Identifier)?;
        let type_parameters = self.parse_type_parameters();
        let heritage = self.parse_class_parent();
        let (members, end) = self.parse_member_block()?;
        Some(Item::Struct(StructItem {
            attributes,
            name: name.slice,
            type_parameters,
            heritage,
            members,
            scope,
            span: Span { start, end },
        }))
    }

    /// 错误恢复辅助：跳过函数声明剩余部分，直到 `;`（extern 形态）或平衡的 `{...}` 块结束。
    ///
    /// 记录为失败后的同步手段；正常路径不调用。
    fn skip_to_function_end(&mut self) {
        let mut brace_depth = 0usize;
        while let Some(token) = self.tokens.get(self.pos) {
            let kind = token.kind; // TokenKind: Copy，复制后借用即结束
            match kind {
                TokenKind::Eof => break,
                TokenKind::SemiColon if brace_depth == 0 => {
                    self.pos += 1;
                    break;
                }
                TokenKind::OpenBrace => {
                    brace_depth += 1;
                    self.pos += 1;
                }
                TokenKind::CloseBrace => {
                    self.pos += 1;
                    if brace_depth == 0 {
                        break;
                    }
                    brace_depth -= 1;
                    if brace_depth == 0 {
                        break;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }
}

/// 把 `>>` / `<<` 这类双字符 token 拆成两个逻辑 token。
///
/// `slice` 按字符切半（`>>` → `>` + `>`），区间按字节偏移对半分；
/// 因为 `kind` 被改成单字符种类，下游只关心 `kind` 与 `span`。
fn split_token_pair(
    token: &Token,
    first_kind: TokenKind,
    second_kind: TokenKind,
) -> (Token, Token) {
    let chars: Vec<char> = token.slice.chars().collect();
    let half = chars.len() / 2;
    let first_slice: String = chars[..half].iter().collect();
    let second_slice: String = chars[half..].iter().collect();
    let mid = token.span.start + first_slice.len();
    (
        Token {
            kind: first_kind,
            span: Span {
                start: token.span.start,
                end: mid,
            },
            slice: first_slice,
            line: token.line,
            column: token.column,
        },
        Token {
            kind: second_kind,
            span: Span {
                start: mid,
                end: token.span.end,
            },
            slice: second_slice,
            line: token.line,
            column: token.column + mid - token.span.start,
        },
    )
}

/// 去掉字符串字面量的引号与字面量前缀（`"a/b"` → `a/b`，`r"x"` → `x`）。
///
/// 仅用于导入路径这类「源码即路径」的场景，不做转义还原（值语义延后到语义阶段）。
pub(crate) fn unquote(raw: &str) -> String {
    let mut text = raw;
    // 前缀：`f` / `r` / `b` 及其组合（如 `fr"..."`）
    let trimmed_start = text.trim_start_matches(['f', 'r', 'b']);
    if trimmed_start.len() != text.len() && trimmed_start.starts_with('"') {
        text = trimmed_start;
    }
    text.trim_matches('"').to_string()
}

/// 由导入路径推导默认的命名空间别名（取末段并去掉扩展名）：`"std/io.nov"` → `io`。
fn namespace_alias(path: &str) -> String {
    let last = path.rsplit(['/', '\\', '.']).find(|part| !part.is_empty());
    match last {
        Some(part) => part.to_string(),
        None => path.to_string(),
    }
}

/// 把 token 种类渲染成源码里的写法，供诊断信息使用。
fn describe_token(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Eof => "文件结尾".to_string(),
        TokenKind::Identifier => "标识符".to_string(),
        other => format!("`{other}`"),
    }
}

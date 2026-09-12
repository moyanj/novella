use novella_ast::{
    decorator::Decorator,
    expr::Expression,
    program::{FunctionItem, Item, Program, Scope},
    stmt::{BindingPattern, Block, Statement, VariableStmt, VariableType},
    types::TypeExpr,
};
use novella_core::{
    Span,
    diagnostic::{Diagnostic, DiagnosticLocation},
    token::{Token, TokenKind},
};

/// 解析结果：程序 AST 与解析诊断。
///
/// 语法错误不中断解析，全部收集进 `diagnostics`，风格与
/// `novella_lexer::LexerResult` 的"可恢复错误只进 errors"一致。
pub struct ParseResult {
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
}

/// 手写递归下降解析器：持有 token 流、游标与诊断收集器，逐条解析顶层 item。
///
/// 只消费 [`Token`]/[`TokenKind`]，不接触 lexer 内部类型；
/// 语法错误就近经 `error_here` 记录并继续解析后续 item。
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// 解析期间收集的诊断，`parse()` 结束时取出返回。
    diagnostics: Vec<Diagnostic>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            pos: 0,
            diagnostics: vec![],
        }
    }

    // ---------- 游标与前瞻 ----------

    /// 当前 token（不移动游标）；EOF 或流结束返回 None。
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// 前瞻第 n 个 token（不移动游标）；超出流范围返回 None。
    ///
    /// 骨架阶段尚未使用，实现 parse_xxx 时经 `check_n` 用上后可移除标注。
    #[allow(dead_code)]
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
    ///
    /// 骨架阶段尚未使用，实现 parse_xxx 时用上后可移除标注。
    #[allow(dead_code)]
    fn check(&self, kind: TokenKind) -> bool {
        matches!(self.peek(), Some(token) if token.kind == kind)
    }

    /// 前瞻第 n 个 token 是否为指定 kind（不消费）。
    ///
    /// 骨架阶段尚未使用，实现 parse_xxx 时用上后可移除标注。
    #[allow(dead_code)]
    fn check_n(&self, kind: TokenKind, n: usize) -> bool {
        matches!(self.peek_n(n), Some(token) if token.kind == kind)
    }

    /// 尝试消费指定 kind 的 token；命中则前进并返回该 token，否则返回 None（不移动游标）。
    ///
    /// 骨架阶段尚未使用，实现 parse_xxx 时用上后可移除标注。
    #[allow(dead_code)]
    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        if self.check(kind) {
            self.advance()
        } else {
            None
        }
    }

    /// 必须消费指定 kind 的 token；未命中时记录一条 `expected {kind}` 诊断并返回 None。
    ///
    /// 消息中的 `{kind}` 为 [`TokenKind`] 的 `Display`（变体名），
    /// 需要更友好的措辞时可在此处定制。
    /// 骨架阶段尚未使用，实现 parse_xxx 时用上后可移除标注。
    #[allow(dead_code)]
    fn expect(&mut self, kind: TokenKind) -> Option<Token> {
        match self.eat(kind) {
            Some(token) => Some(token),
            None => {
                self.error_here(format!("expected {kind}"));
                None
            }
        }
    }

    // ---------- 诊断 ----------

    /// 在当前 token 处记录一条错误诊断（不移动游标）；流结束时诊断无位置。
    fn error_here(&mut self, message: impl Into<String>) {
        let mut diag = Diagnostic::error(message);
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

    /// 在指定 token 处记录一条错误诊断（不移动游标）。
    ///
    /// 适用于已消费 token 后需回指其位置的场景；`token` 须为独立引用
    /// （如 `bump()` 返回的拥有值），不能是借自 `self` 的 `peek()` 结果，
    /// 否则与 `&mut self` 冲突。当前骨架方法尚未用到，用上后可移除标注。
    #[allow(dead_code)]
    fn error_at(&mut self, token: &Token, message: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(message).with_location(DiagnosticLocation {
                file: None,
                line: token.line,
                column: token.column,
                span: Some(token.span),
            }),
        );
    }

    fn except_identifier(&mut self, token: &Token) -> Option<String> {
        if token.kind == TokenKind::Identifier {
            Some(token.slice.clone())
        } else {
            self.error_at(token, "Expected identifier");
            None
        }
    }

    // ---------- 顶层 item 分发 ----------

    /// 解析整个 token 流为程序（对应文法 `program`）。
    pub fn parse(&mut self) -> ParseResult {
        let mut items: Vec<Item> = vec![];
        while !self.at_eof() {
            // 单条 item 解析失败时诊断已记录，继续解析后续 item
            if let Some(item) = self.parse_item() {
                items.push(item);
            }
        }
        // 文件区间：从 0 到 EOF token 的结束位置
        let end = self.tokens.last().map(|t| t.span.end).unwrap_or(0);
        ParseResult {
            program: Program {
                items,
                span: Span { start: 0, end },
            },
            diagnostics: std::mem::take(&mut self.diagnostics),
        }
    }

    /// 解析单个顶层 item（对应文法 `sourceElement`）。
    ///
    /// 固定三段顺序，把"修饰符在前、声明关键字在后"的位置约束集中在此处：
    /// ① 装饰器列表（对应文法 `decorator*`，等价 Rust 的 `#[...]`）；
    /// ② 可见性 `pub`（对应文法声明前的 `Pub?`）；
    /// ③ 按当前 token 分发到各自的 `parse_xxx`。
    ///
    /// 起始关键字一律由本方法按分支自行消费（`advance`），因此游标只会前进、
    /// 绝不回退；各 `parse_xxx` 的前置约定统一为"关键字已被消费，游标指向关键字之后"。
    ///
    /// 文法位置约束在此集中校验（对应示例里"`impl` 不能带 `pub`"那类检查）：
    /// - 装饰器只允许修饰 `@foo;` / `@foo { ... }`（文法 `decoratorStatement`）
    ///   与 `func` / `class` / `trait`（文法中仅这三者带 `decorator*`）；
    /// - 可见性只允许修饰 `class` / `func` / `let` / `const`；
    ///   `extern` 的 `Pub?` 在 `extern` 之后（文法 `Extern Pub? Func`），不在此处。
    fn parse_item(&mut self) -> Option<Item> {
        // ① 先看有没有装饰器（属性）
        let attributes = self.parse_decorators();
        // ② 再解析可见性
        let scope = self.parse_visibility();

        // 当前 token 决定了后面是什么 item；TokenKind: Copy，取出后不再借用 self
        let kind = self.peek().map(|token| token.kind)?;

        if !attributes.is_empty()
            && !matches!(
                kind,
                TokenKind::SemiColon
                    | TokenKind::OpenBrace
                    | TokenKind::Func
                    | TokenKind::Class
                    | TokenKind::Trait
            )
        {
            self.error_here("装饰器后应为 `;`、块 `{...}` 或声明（func / class / trait）");
            return None;
        }
        if scope.is_some() {
            match kind {
                TokenKind::Class | TokenKind::Func | TokenKind::Let | TokenKind::Const => {}
                TokenKind::Extern => {
                    self.error_here("`extern` 的可见性修饰符应写在 `extern` 之后（`extern pub func`）");
                    return None;
                }
                _ => {
                    self.error_here("`pub` 后应为 class / func / let / const 声明");
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
                self.parse_import()
            }
            TokenKind::Type => {
                self.advance();
                self.parse_type_alias()
            }
            TokenKind::Trait => {
                self.advance();
                self.parse_trait(attributes)
            }
            TokenKind::Class => {
                self.advance();
                self.parse_class(attributes, scope)
            }
            TokenKind::Func => {
                self.advance();
                self.parse_function(attributes, scope)
            }
            TokenKind::Extern => {
                self.advance();
                self.parse_extern_function()
            }
            // 顶层 `pub let` / `pub const`（文法 `variableStatement: Pub? (Const | Let) ...`）
            TokenKind::Let | TokenKind::Const => self.parse_variable_declaration(scope),
            // 其余起始关键字按文法 `sourceElement` 的 `statement` 分支兜底，
            // 对应 [`Item::Statement`]；起始 token 由 parse_statement 自行消费。
            _ => self.parse_statement().map(Box::new).map(Item::Statement),
        }
    }

    // ---------- 各 item 解析（骨架：方法体留给你写） ----------

    /// 解析导入语句（对应文法 `moduleImport`）。
    ///
    /// 前置：`from` / `import` 起始关键字已被主循环消费。
    /// TODO(自己写)：按文法实现
    /// - `fromImport`: `From StringLiteral Import identifier (',' identifier)* SemiColon`
    /// - `directImport`: `Import StringLiteral SemiColon`
    fn parse_import(&mut self) -> Option<Item> {
        // 骨架：待实现
        None
    }

    /// 解析类型别名声明（对应文法 `typeAliasDeclaration`）。
    ///
    /// 前置：`type` 关键字已被主循环消费。
    /// TODO(自己写)：`Type identifier typeParameters? Assign typeExpression SemiColon`
    /// → [`Item::TypeAlias`]。
    fn parse_type_alias(&mut self) -> Option<Item> {
        // 骨架：待实现
        None
    }

    /// 解析 trait 声明（对应文法 `traitDeclaration`）。
    ///
    /// 前置：`trait` 关键字已被消费，游标指向 trait 名。
    /// `_attributes` 为声明前的装饰器列表（对应文法 `decorator*`），
    /// 由 [`Parser::parse_decorated`] 传入；无装饰器时为空。
    /// TODO(自己写)：`decorator* Trait identifier typeParameters? traitBody`
    /// → [`Item::Trait`]；实现时将 `_attributes` 改名为 `attributes` 填入
    /// `TraitItem` 的 `attributes` 字段。
    fn parse_trait(&mut self, _attributes: Vec<Decorator>) -> Option<Item> {
        // 骨架：待实现
        None
    }

    /// 解析类声明（对应文法 `classDeclaration`）。
    ///
    /// 前置：`class` 关键字已被消费，游标指向类名。
    /// `_attributes` 为声明前的装饰器列表（对应文法 `decorator*`），
    /// 由 [`Parser::parse_decorated`] 传入；无装饰器时为空。
    /// TODO(自己写)：`decorator* Pub? Class identifier typeParameters? classHeritage? classBody`
    /// → [`Item::Class`]；实现时将 `_attributes` 改名为 `attributes` 填入
    /// `ClassItem` 的 `attributes` 字段，并处理 `Pub?` 修饰。
    fn parse_class(&mut self, _attributes: Vec<Decorator>) -> Option<Item> {
        // 骨架：待实现
        None
    }

    /// 解析 extern 函数声明（对应文法 `externFunctionDeclaration`）。
    ///
    /// 前置：`extern` 关键字已被主循环消费。
    /// TODO(自己写)：`Extern Pub? Func identifier typeParameters? '(' parameterList? ')'`
    /// `typeAnnotation? ReturnArrow? typeExpression? SemiColon`
    /// → [`FunctionItem`]（`is_extern: true`、`block: None`）。
    fn parse_extern_function(&mut self) -> Option<Item> {
        // 骨架：待实现
        None
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
    /// 由 [`Parser::parse_decorated`] 调用，按后续 token 决定装饰器归属。
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

    /// 解析装饰器起始的顶层结构（对应文法 `decoratorStatement` 与声明前的 `decorator*`）。
    ///
    /// 前置：游标位于 `@`。
    /// 先收集连续装饰器前缀，再按后续 token 分发：
    /// - `;`：独立装饰器语句 [`Item::Decorator`]（文法 `decoratorStatement` 仅允许单个装饰器）；
    /// - `{`：装饰块 [`Item::DecoratorBlock`]（同上）；
    /// - `func` / `class` / `trait`：作为声明前缀，把装饰器列表交给对应解析
    ///   （填各声明的 `attributes` 字段）。
    fn parse_decorated(&mut self) -> Option<Item> {
        let decorators = self.parse_decorators();
        let start = decorators.first().map(|d| d.span.start)?;
        match self.peek().map(|t| t.kind) {
            Some(TokenKind::SemiColon) => {
                if decorators.len() != 1 {
                    self.error_here("独立装饰器语句只允许一个装饰器");
                    return None;
                }
                let semi = self.advance().expect("已确认当前为分号");
                let decorator = decorators.into_iter().next().expect("len == 1 已检查");
                Some(Item::Decorator(Decorator {
                    span: Span {
                        start,
                        end: semi.span.end,
                    },
                    ..decorator
                }))
            }
            Some(TokenKind::OpenBrace) => {
                if decorators.len() != 1 {
                    self.error_here("装饰块只允许一个装饰器");
                    return None;
                }
                let decorator = decorators.into_iter().next().expect("len == 1 已检查");
                let block = self.parse_block()?;
                let end = block.span.end;
                Some(Item::DecoratorBlock {
                    decorator,
                    block: Some(block),
                    span: Span { start, end },
                })
            }
            Some(TokenKind::Func) => {
                self.advance();
                self.parse_function(decorators)
            }
            Some(TokenKind::Class) => {
                self.advance();
                self.parse_class(decorators)
            }
            Some(TokenKind::Trait) => {
                self.advance();
                self.parse_trait(decorators)
            }
            _ => {
                self.error_here("装饰器后应为 `;`、块 `{...}` 或声明（func / class / trait）");
                None
            }
        }
    }

    /// 解析语句块（对应文法 `block`：`{ statement* }`）。
    ///
    /// 前置：游标位于 `{`（尚未消费）。
    /// 块内语句逐条交给 [`Parser::parse_statement`]，由它自行消费起始 token
    /// （其契约：返回 `None` 时也已消费过 token，保证循环推进）；
    /// 嵌套块经 `statement` 的块分支递归回到本方法，故此处不手工计数括号深度。
    fn parse_block(&mut self) -> Option<Block> {
        let open = self.expect(TokenKind::OpenBrace)?;
        let start = open.span.start;

        let mut stmts = vec![];
        while !self.at_eof() {
            // 匹配的右花括号：块结束，区间取到 `}` 的结束位置
            if self.check(TokenKind::CloseBrace) {
                let close = self.advance()?;
                return Some(Block {
                    stmts,
                    span: Span {
                        start,
                        end: close.span.end,
                    },
                });
            }
            // 单条语句解析失败时诊断已记录，继续解析后续语句
            if let Some(stmt) = self.parse_statement() {
                stmts.push(stmt);
            }
        }
        // 块未闭合是致命错误，丢弃本块（诊断位置落在 EOF 处）
        self.error_here("语句块未闭合");
        None
    }

    /// 解析顶层语句（对应文法 `statement`，[`Item::Statement`]）。
    ///
    /// 主循环无法识别为 item 起始关键字的 token 均由此兜底。
    /// 骨架：仅实现变量声明分支（`let` / `const`），
    /// 其余 14 种形态（return / break / if / while / for ...）待实现。
    fn parse_statement(&mut self) -> Option<Statement> {
        match self.peek().map(|t| t.kind) {
            Some(TokenKind::Let) | Some(TokenKind::Const) => self.parse_variable_declaration(),
            Some(TokenKind::OpenBrace) => self.parse_block().map(Statement::Block),
            _ => {
                self.error_here("未知语句");
                // 消费未知起始 token，避免外层循环原地踏步
                self.advance();
                None
            }
        }
    }

    /// 解析变量声明语句（对应文法 `variableDeclaration`）。
    ///
    /// 前置：游标位于 `let` / `const`（尚未消费）。
    /// 骨架实现：仅支持单个标识符绑定（元组解构 `(a, b)` 待实现）、
    /// 无 `pub` 修饰；类型注解与初始化表达式经 `parse_type_expr` /
    /// `parse_expression` 占位（当前均未实现，返回 None）。
    /// 支持 [逐声明清单]`BindPattern::Tuple`、回调 `valueDeclarationList`。
    fn parse_variable_declaration(&mut self) -> Option<Statement> {
        // 消费声明关键字，作为变量声明区间起点
        let keyword = self.advance()?;
        let r#type = if keyword.kind == TokenKind::Const {
            VariableType::Const
        } else {
            VariableType::Let
        };

        // 变量名：单个标识符（元组解构待实现）
        let name_token = match self.peek() {
            Some(token) if token.kind == TokenKind::Identifier => token.clone(),
            _ => {
                self.error_here("`let` / `const` 后应为变量名");
                return None;
            }
        };
        self.advance();
        // 失败时诊断已由 except_identifier 记录，`?` 直接返回
        let name = self.except_identifier(&name_token)?;

        // 可选类型注解 `: type`（parse_type_expr 骨架占位返回 None）
        let ty = if self.eat(TokenKind::Colon).is_some() {
            let ty = self.parse_type_expr();
            // 类型表达式解析未实现：跳过剩余部分，避免对后续合法 token 误报错
            if ty.is_none() {
                self.skip_to_statement_end();
            }
            ty
        } else {
            None
        };
        // 可选初始化 `= expr`（parse_expression 骨架占位返回 None）
        let value = if self.eat(TokenKind::Assign).is_some() {
            let expr = self.parse_expression();
            // 表达式解析未实现：同样跳过到分号前，保持骨架语义
            if expr.is_none() {
                self.skip_to_statement_end();
            }
            expr
        } else {
            None
        };

        // 语句结束分号
        let semi = self.expect(TokenKind::SemiColon)?;
        Some(Statement::Variable(VariableStmt {
            pattern: BindingPattern::Ident {
                name,
                span: name_token.span,
            },
            ty,
            value,
            scope: None,
            r#type,
            span: Span {
                start: keyword.span.start,
                end: semi.span.end,
            },
        }))
    }

    /// 占位辅助：跳过当前语句剩余部分，直到 `;` 或 EOF（不消费分号）。
    ///
    /// 类型 / 表达式解析尚未实现时调用，避免游标停在未消费 token 上
    /// 对后续合法输入产生误导性诊断；与 [`Parser::skip_to_function_end`] 同风格。
    fn skip_to_statement_end(&mut self) {
        while let Some(token) = self.tokens.get(self.pos) {
            match token.kind {
                TokenKind::Eof | TokenKind::SemiColon => break,
                _ => self.pos += 1,
            }
        }
    }

    fn parse_expression(&mut self) -> Option<Expression> {
        None
    }

    fn parse_type_expr(&mut self) -> Option<TypeExpr> {
        None
    }

    // ---------- 函数声明（最小可运行占位） ----------

    /// 解析函数声明（对应文法 `functionDeclaration`）。
    ///
    /// 前置：`func` 关键字已被消费，游标指向函数名。
    /// `attributes` 为声明前的装饰器列表（对应文法 `decorator*`），
    /// 由 [`Parser::parse_decorated`] 传入；无装饰器时为空。
    /// 占位实现：仅读取函数名并构造骨架节点（签名与函数体待补充），
    /// 随后跳过声明剩余部分，避免占位阶段产生误导性错误。
    /// TODO(自己写)：按文法 `decorator* Pub? Func identifier typeParameters? '(' parameterList? ')'`
    /// `typeAnnotation? ReturnArrow? typeExpression? block` 补充完整解析。
    fn parse_function(&mut self, attributes: Vec<Decorator>) -> Option<Item> {
        let (name, span) = match self.peek() {
            Some(token) if token.kind == TokenKind::Identifier => (token.slice.clone(), token.span),
            _ => {
                self.error_here("`func` 后应有函数名");
                // 失败也恢复同步：跳过声明剩余部分，避免外层循环对后续 token 重复报错
                self.skip_to_function_end();
                return None;
            }
        };
        self.advance();
        let item = Item::Function(FunctionItem {
            attributes,
            name,
            r#type: None,
            block: None,
            scope: None,
            is_extern: false,
            is_async: false,
            // 占位：完整解析后应覆盖整条函数声明的源码区间
            span,
        });
        self.skip_to_function_end();
        Some(item)
    }

    /// 占位辅助：跳过函数声明剩余部分，直到 `;`（extern 形态）或平衡的 `{...}` 块结束。
    ///
    /// 完整签名与函数体解析落地后应移除。
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

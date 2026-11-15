//! 语句解析（对应文法 `statement` 及其子规则）。
//!
//! 覆盖：变量声明（含多绑定与解构）、`if / elif / else`、`while`、`for`（三段式与
//! `in` 两种形态）、`try / catch / finally`、`with`、`unsafe`、`match` 语句、
//! 跳转语句、块语句与表达式语句。

use novella_ast::{
    expr::Expression,
    program::Scope,
    stmt::{
        BindingPattern, Block, CatchClause, ForInCond, ForInit, ForKind, ForLoopCond, ForStmt,
        IfBranch, IfStmt, Statement, TryStmt, VariableStmt, VariableType, WithBinding, WithStmt,
    },
};
use novella_core::{
    Span,
    diagnostic::{Diagnostic, DiagnosticCode, DiagnosticLocation},
    token::{Token, TokenKind},
};

use crate::Parser;

impl Parser {
    /// 解析块语句 `{ ... }`（对应文法 `block`）。
    pub(crate) fn parse_block(&mut self) -> Option<Block> {
        let open = self.expect(TokenKind::OpenBrace)?;
        // 块本身算一层递归：经分段栈扩展，`{{{...}}}` 这类深层块（每层至少一帧）
        // 同样不会耗尽真实栈
        self.guarded(|p| p.parse_block_inner(open))?
    }

    /// [`Parser::parse_block`] 的实体（分段栈扩展由外层包装负责）。
    fn parse_block_inner(&mut self, open: Token) -> Option<Block> {
        let mut statements = vec![];
        while !self.check(TokenKind::CloseBrace) && !self.at_eof() {
            let before = self.pos;
            match self.parse_statement() {
                Some(statement) => statements.push(statement),
                None => {
                    // 无法解析的语句：跳到语句边界后继续，避免死循环
                    if self.pos == before {
                        self.recover_to_statement_boundary();
                    }
                }
            }
            if self.overflowed() {
                break;
            }
        }
        let close = self.expect(TokenKind::CloseBrace)?;
        Some(Block {
            stmts: statements,
            span: Span {
                start: open.span.start,
                end: close.span.end,
            },
        })
    }

    /// 解析一条语句（对应文法 `statement`）。
    pub(crate) fn parse_statement(&mut self) -> Option<Statement> {
        self.guarded(|p| p.parse_statement_inner())?
    }

    /// `parse_statement` 的实际分发。
    fn parse_statement_inner(&mut self) -> Option<Statement> {
        let kind = self.peek()?.kind;
        match kind {
            TokenKind::OpenBrace => Some(Statement::Block(self.parse_block()?)),
            // 空语句 `;`（文法 `emptyStatement`）：产出 `Statement::Empty` 节点，
            // 供 sema / 工具链区分「空语句」与「语句缺失」（C-02）
            TokenKind::SemiColon => {
                let token = self.advance()?;
                Some(Statement::Empty { span: token.span })
            }
            TokenKind::Let | TokenKind::Const => self.parse_variable_statement(None),
            // 块内也允许带可见性前缀的声明（文法 `variableStatement: Pub? ...`）
            TokenKind::Pub | TokenKind::Pri | TokenKind::Pkg => {
                let scope = self.parse_visibility();
                self.parse_variable_statement(scope)
            }
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Try => self.parse_try(),
            TokenKind::With => self.parse_with(),
            TokenKind::Unsafe => self.parse_unsafe(),
            TokenKind::Match => self.parse_match_statement(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Break => self.parse_jump(|span| Statement::Break { span }),
            TokenKind::Continue => self.parse_jump(|span| Statement::Continue { span }),
            TokenKind::Throw => self.parse_throw(),
            TokenKind::Yield => self.parse_yield(),
            _ => {
                let expr = match self.parse_expression() {
                    Some(expr) => expr,
                    None => {
                        // 表达式解析失败：跳到语句边界，避免在外层块里反复报错
                        self.recover_to_statement_boundary();
                        return None;
                    }
                };
                self.expect_statement_end();
                Some(Statement::Expression(expr))
            }
        }
    }

    // ---------- 变量声明 ----------

    /// 解析变量声明语句（对应文法 `variableStatement`）。
    ///
    /// 文法允许一条声明绑定多个变量：`let a, (b, c) = ...;`。
    /// 单绑定产出 [`Statement::Variable`]；多绑定产出 [`Statement::VariableList`]
    /// （同一作用域按序求值，不引入嵌套块，C-06）。
    pub(crate) fn parse_variable_statement(&mut self, scope: Option<Scope>) -> Option<Statement> {
        let keyword = self.peek()?.clone();
        let variable_type = match keyword.kind {
            TokenKind::Let => VariableType::Let,
            TokenKind::Const => VariableType::Const,
            _ => {
                self.error_here("此处应为 `let` 或 `const`");
                return None;
            }
        };
        self.advance();
        let start = keyword.span.start;

        // 一条声明可绑定多个目标：`let a, b, c: int, d = 1;`
        // AST 的 `VariableStmt` 只承载单个 pattern，因此后续目标各自生成一条声明，
        // 共享同一关键字、作用域与类型（文法中注解/初值可分别出现在任意目标之后）。
        let mut declarations = vec![];
        loop {
            let pattern_start = self.peek()?.span.start;
            let Some(pattern) = self.parse_binding_pattern() else {
                break;
            };
            let mut ty = None;
            if self.eat(TokenKind::Colon).is_some() {
                ty = self.parse_type_expr();
            }
            let mut value = None;
            if self.eat(TokenKind::Assign).is_some() {
                value = self.parse_expression();
            }
            // 注解/初值解析失败时不中断整条声明，交由后续同步
            if ty.is_none()
                && value.is_none()
                && !self.check(TokenKind::Comma)
                && !self.check(TokenKind::SemiColon)
            {
                break;
            }
            declarations.push(VariableStmt {
                pattern,
                ty,
                value,
                scope,
                r#type: variable_type,
                span: Span {
                    start: pattern_start,
                    end: self.last_end(),
                },
            });
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        let end = self.last_end();
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("声明末尾应为 `;`");
        }

        if declarations.len() == 1 {
            let declaration = declarations.into_iter().next().expect("len 已判定为 1");
            return Some(Statement::Variable(declaration));
        }
        // 多绑定：同一作用域按序求值。包成 VariableList 而非 Block——
        // Block 会引入不存在的嵌套作用域，导致 sema 作用域分析出错（C-06）
        Some(Statement::VariableList {
            declarations,
            span: Span { start, end },
        })
    }

    /// 解析绑定目标（对应文法 `bindingPattern`）：标识符或括号解构（可递归）。
    ///
    /// 解构可无限嵌套（`(((a, b)))`），故同样经 [`Parser::guarded`] 扩展分段栈；
    /// 实体拆到 [`Parser::parse_binding_pattern_inner`]。
    fn parse_binding_pattern(&mut self) -> Option<BindingPattern> {
        self.guarded(|p| p.parse_binding_pattern_inner())?
    }

    /// [`Parser::parse_binding_pattern`] 的实体（分段栈扩展由外层包装负责）。
    fn parse_binding_pattern_inner(&mut self) -> Option<BindingPattern> {
        let open = self.peek()?.clone();
        if open.kind == TokenKind::OpenParen {
            self.advance();
            let mut elements = vec![];
            loop {
                elements.push(self.parse_binding_pattern()?);
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
            }
            let close = self.expect(TokenKind::CloseParen)?;
            return Some(BindingPattern::Tuple {
                elements,
                span: Span {
                    start: open.span.start,
                    end: close.span.end,
                },
            });
        }
        let identifier = self.expect(TokenKind::Identifier)?;
        Some(BindingPattern::Ident {
            name: identifier.slice,
            span: identifier.span,
        })
    }

    // ---------- 条件与循环 ----------

    /// 解析 `if / elif / else`（对应文法 `ifStatement`）。
    fn parse_if(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::If)?;
        let start = keyword.span.start;
        let condition = self.parse_parenthesized_condition()?;
        let block = self.parse_statement_body()?;
        let mut branches = vec![IfBranch::ElseIf {
            condition: Box::new(condition),
            block: Box::new(block),
            span: Span {
                start,
                end: self.last_end(),
            },
        }];
        while self.check(TokenKind::Elif) {
            let elif = self.advance()?;
            let condition = self.parse_parenthesized_condition()?;
            let block = self.parse_statement_body()?;
            branches.push(IfBranch::ElseIf {
                condition: Box::new(condition),
                block: Box::new(block),
                span: Span {
                    start: elif.span.start,
                    end: self.last_end(),
                },
            });
        }
        if self.eat(TokenKind::Else).is_some() {
            let block = self.parse_statement_body()?;
            branches.push(IfBranch::Else {
                block: Box::new(block),
                span: Span {
                    start,
                    end: self.last_end(),
                },
            });
        }
        Some(Statement::If(IfStmt {
            branches,
            span: Span {
                start,
                end: self.last_end(),
            },
        }))
    }

    /// 解析语句体：`{ ... }` 块或单条语句（`if (x) y = 0;`）。
    ///
    /// 文法里分支体写作 `block | statement`，单条语句时包成单元素块，
    /// 使 AST 保持统一的 `Box<Statement>` 形态。
    fn parse_statement_body(&mut self) -> Option<Statement> {
        if self.check(TokenKind::OpenBrace) {
            return Some(Statement::Block(self.parse_block()?));
        }
        let statement = self.parse_statement()?;
        Some(statement)
    }

    /// 条件表达式统一写作 `( expr )`（文法 `'(' expression ')'`）。
    fn parse_parenthesized_condition(&mut self) -> Option<Expression> {
        self.expect(TokenKind::OpenParen)?;
        let condition = self.parse_expression()?;
        self.expect(TokenKind::CloseParen)?;
        Some(condition)
    }

    /// 解析 `while (cond) block`（对应文法 `whileStatement`）。
    fn parse_while(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::While)?;
        let condition = self.parse_parenthesized_condition()?;
        let body = self.parse_statement_body()?;
        Some(Statement::While {
            condition,
            body: Box::new(body),
            span: Span {
                start: keyword.span.start,
                end: self.last_end(),
            },
        })
    }

    /// 解析 `for (...)`（对应文法 `forStatement`，`forInCondition | forLoopCondition`）。
    fn parse_for(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::For)?;
        let start = keyword.span.start;
        self.expect(TokenKind::OpenParen)?;

        // 先按 `in` 形态尝试：`for (x in xs)` / `for ((k, v) in m)`
        let condition = if self.looks_like_for_in() {
            let left = self.parse_binding_pattern()?;
            self.expect(TokenKind::In)?;
            let right = self.parse_expression()?;
            ForKind::ForIn(Box::new(ForInCond {
                left,
                right,
                span: Span {
                    start,
                    end: self.last_end(),
                },
            }))
        } else {
            ForKind::ForLoop(Box::new(self.parse_for_loop_condition(start)?))
        };
        self.expect(TokenKind::CloseParen)?;
        let body = self.parse_statement_body()?;
        Some(Statement::For(ForStmt {
            condition,
            block: Box::new(body),
            span: Span {
                start,
                end: self.last_end(),
            },
        }))
    }

    /// 前瞻判断 `for (` 之后是否为 `in` 形态。
    ///
    /// `for (a in xs)` 起始是标识符；`for (let i = 0; ...)` 起始是 `let`/`const`/可见性，
    /// 不会走这里。含解构 `for ((k, v) in m)` 时向后找**同层**的 `in`
    /// （嵌套括号内的 `in` 属于内部表达式，不能作为判定依据，C-03）。
    fn looks_like_for_in(&self) -> bool {
        let mut depth = 0usize;
        let mut idx = self.pos;
        while let Some(token) = self.tokens.get(idx) {
            match token.kind {
                TokenKind::OpenParen | TokenKind::OpenBracket => depth += 1,
                TokenKind::CloseParen | TokenKind::CloseBracket if depth == 0 => return false,
                TokenKind::CloseParen | TokenKind::CloseBracket => depth -= 1,
                TokenKind::In if depth == 0 => return true,
                TokenKind::SemiColon if depth == 0 => return false,
                TokenKind::Eof => return false,
                _ => {}
            }
            idx += 1;
        }
        false
    }

    /// 解析三段式 `for` 的条件部分（`forInit? ';' expression? ';' expression?`）。
    fn parse_for_loop_condition(&mut self, start: usize) -> Option<ForLoopCond> {
        let init = self.parse_for_init();
        self.expect(TokenKind::SemiColon)?;
        let condition = if self.check(TokenKind::SemiColon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        self.expect(TokenKind::SemiColon)?;
        let update = if self.check(TokenKind::CloseParen) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        Some(ForLoopCond {
            init,
            condition,
            update,
            span: Span {
                start,
                end: self.last_end(),
            },
        })
    }

    /// 解析 `for` 的初始化部分：声明列表或表达式（对应文法 `forInit`）。
    fn parse_for_init(&mut self) -> Option<ForInit> {
        if self.check(TokenKind::SemiColon) {
            return None;
        }
        let start = self.peek()?.span.start;
        // 文法中 init 段的声明不带 `const/let`，统一按 `Let` 承载（见 `ForInit` 注释）。
        // 但为兼容用户书写习惯，也接受显式 `let` / `const`。
        let declares = matches!(
            self.peek()?.kind,
            TokenKind::Let | TokenKind::Const | TokenKind::Pub | TokenKind::Pri | TokenKind::Pkg
        ) || self.looks_like_bare_declaration();
        if declares {
            let scope = self.parse_visibility();
            let variable_type = match self.peek().map(|t| t.kind) {
                Some(TokenKind::Const) => {
                    self.advance();
                    VariableType::Const
                }
                Some(TokenKind::Let) => {
                    self.advance();
                    VariableType::Let
                }
                _ => VariableType::Let,
            };
            let mut declarations = vec![];
            loop {
                let pattern_start = self.peek()?.span.start;
                let pattern = self.parse_binding_pattern()?;
                let mut ty = None;
                if self.eat(TokenKind::Colon).is_some() {
                    ty = Some(self.parse_type_expr()?);
                }
                let mut value = None;
                if self.eat(TokenKind::Assign).is_some() {
                    value = Some(self.parse_expression()?);
                }
                declarations.push(VariableStmt {
                    pattern,
                    ty,
                    value,
                    scope,
                    r#type: variable_type,
                    span: Span {
                        start: pattern_start,
                        end: self.last_end(),
                    },
                });
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
            }
            return Some(ForInit::Declaration {
                declarations,
                span: Span {
                    start,
                    end: self.last_end(),
                },
            });
        }
        let expr = self.parse_expression()?;
        Some(ForInit::Expression {
            span: expr.span(),
            expr,
        })
    }

    /// `for (i: int = 0; ...)` 这类无 `let` 的声明：`Identifier (':' ty)? '='`。
    ///
    /// 扫描首个标识符之后的 token：遇 `:` / `=` 判定为声明；遇标识符 /
    /// 分隔符 / 表达式起始符号（`(` 调用、`[` 索引、`.` 成员、`{` 构造）
    /// 判定为表达式——后者若漏判，声明扫描会吞掉表达式的剩余部分（B-06）。
    fn looks_like_bare_declaration(&self) -> bool {
        if !self.check(TokenKind::Identifier) {
            return false;
        }
        for offset in 1.. {
            let Some(token) = self.peek_n(offset) else {
                return false;
            };
            match token.kind {
                TokenKind::Colon | TokenKind::Assign => return true,
                TokenKind::Identifier => return false,
                TokenKind::Comma | TokenKind::SemiColon | TokenKind::Eof => return false,
                TokenKind::OpenParen | TokenKind::OpenBracket | TokenKind::Dot => return false,
                TokenKind::OpenBrace => return false,
                _ => {}
            }
        }
        false
    }

    // ---------- try / with / unsafe ----------

    /// 解析 `try / catch / finally`（对应文法 `tryStatement`）。
    fn parse_try(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::Try)?;
        let block = self.parse_block()?;
        let mut catches = vec![];
        while self.check(TokenKind::Catch) {
            let catch = self.advance()?;
            self.expect(TokenKind::OpenParen)?;
            let name = self.expect(TokenKind::Identifier)?;
            let mut ty = None;
            if self.eat(TokenKind::Colon).is_some() {
                ty = Some(self.parse_type_expr()?);
            }
            self.expect(TokenKind::CloseParen)?;
            let catch_block = self.parse_block()?;
            catches.push(CatchClause {
                name: name.slice,
                ty,
                block: catch_block,
                span: Span {
                    start: catch.span.start,
                    end: self.last_end(),
                },
            });
        }
        let finally = if self.eat(TokenKind::Finally).is_some() {
            Some(self.parse_block()?)
        } else {
            None
        };
        Some(Statement::Try(TryStmt {
            block,
            catches,
            finally,
            span: Span {
                start: keyword.span.start,
                end: self.last_end(),
            },
        }))
    }

    /// 解析 `with expr [as pattern: T] block`（对应文法 `withStatement`）。
    ///
    /// 文法中表头**无括号**（`With expression (As ...)? block`）。历史代码强制
    /// 要求 `with (...)` 括号形态、与文法相悖（B-01）；括号形态现被表达式解析
    /// 当作括号分组（`as` 在其中按 cast 处理），语义不同，故给出迁移诊断。
    fn parse_with(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::With)?;
        if self.check(TokenKind::OpenParen) {
            self.error_here(
                "with 表头无需括号（文法为 `with expr as name { ... }`），括号会被当作表达式分组",
            );
        }
        // 标记「当前在 with 表头内」：表头顶层的 `as` 优先解释为上下文绑定（E-02）
        self.with_stack += 1;
        let expr = self.parse_expression();
        self.with_stack -= 1;
        let expr = expr?;
        let mut binding = None;
        if self.eat(TokenKind::As).is_some() {
            let pattern = self.parse_binding_pattern()?;
            let mut ty = None;
            if self.eat(TokenKind::Colon).is_some() {
                ty = Some(self.parse_type_expr()?);
            }
            binding = Some(WithBinding {
                pattern,
                ty,
                span: Span {
                    start: expr.span().end,
                    end: self.last_end(),
                },
            });
        }
        let block = self.parse_block()?;
        Some(Statement::With(WithStmt {
            expr,
            binding,
            block,
            span: Span {
                start: keyword.span.start,
                end: self.last_end(),
            },
        }))
    }

    /// 解析 `unsafe block`（对应文法 `unsafeStatement`）。
    fn parse_unsafe(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::Unsafe)?;
        let block = self.parse_block()?;
        Some(Statement::Unsafe {
            block: Box::new(block),
            span: Span {
                start: keyword.span.start,
                end: self.last_end(),
            },
        })
    }

    // ---------- match 语句 ----------

    /// 解析 match 语句：`match subject { ... }`（与表达式形态共用解析逻辑）。
    fn parse_match_statement(&mut self) -> Option<Statement> {
        let expr = self.parse_match()?;
        self.expect_statement_end();
        Some(Statement::Expression(expr))
    }

    // ---------- 跳转语句 ----------

    /// 解析 `return [expr];`（对应文法 `returnStatement`）。
    fn parse_return(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::Return)?;
        let value = if self.check(TokenKind::SemiColon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("语句末尾应为 `;`");
        }
        Some(Statement::Return {
            value,
            span: Span {
                start: keyword.span.start,
                end: self.last_end(),
            },
        })
    }

    /// 解析 `break;` / `continue;`（对应文法 `breakStatement` / `continueStatement`）。
    fn parse_jump(&mut self, make: impl FnOnce(Span) -> Statement) -> Option<Statement> {
        let keyword = self.advance()?;
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("语句末尾应为 `;`");
        }
        Some(make(Span {
            start: keyword.span.start,
            end: self.last_end(),
        }))
    }

    /// 解析 `throw expr;`（对应文法 `throwStatement`）。
    fn parse_throw(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::Throw)?;
        let expr = self.parse_expression()?;
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("语句末尾应为 `;`");
        }
        Some(Statement::Throw {
            expr,
            span: Span {
                start: keyword.span.start,
                end: self.last_end(),
            },
        })
    }

    /// 解析 `yield [expr];`（对应文法 `yieldStatement`）。
    fn parse_yield(&mut self) -> Option<Statement> {
        let keyword = self.expect(TokenKind::Yield)?;
        let value = if self.check(TokenKind::SemiColon) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        if self.eat(TokenKind::SemiColon).is_none() {
            self.error_here("语句末尾应为 `;`");
        }
        Some(Statement::Yield {
            value,
            span: Span {
                start: keyword.span.start,
                end: self.last_end(),
            },
        })
    }

    /// 语句结尾检查：`;` 必填；紧随 `}` 时容忍省略（块的最后一条语句）。
    pub(crate) fn expect_statement_end(&mut self) {
        if self.eat(TokenKind::SemiColon).is_some() {
            return;
        }
        if matches!(
            self.peek().map(|token| token.kind),
            Some(TokenKind::CloseBrace | TokenKind::Eof)
        ) {
            return;
        }
        self.error_here("语句末尾应为 `;`");
    }

    // ---------- 恢复 ----------

    /// 按花括号配对跳过当前块剩余部分（吃到最后匹配的 `}`）。
    ///
    /// 用于恢复病理输入时的块剩余部分：只数括号、不再递归，保证过程线性且必然终止。
    #[allow(dead_code)]
    fn skip_balanced_block(&mut self, open: &novella_core::token::Token) {
        let mut level = 1usize;
        while let Some(token) = self.peek() {
            match token.kind {
                TokenKind::OpenBrace => level += 1,
                TokenKind::CloseBrace => {
                    level -= 1;
                    if level == 0 {
                        self.advance();
                        return;
                    }
                }
                TokenKind::Eof => return,
                _ => {}
            }
            self.advance();
        }
        // 走到流末尾仍未闭合：以块的起始位置补一条诊断（外层守卫已报过深度问题）
        let mut diag =
            Diagnostic::error("此处缺少与之配对的 `}`").with_code(DiagnosticCode::SyntaxError);
        diag = diag.with_location(DiagnosticLocation {
            file: None,
            line: open.line,
            column: open.column,
            span: Some(open.span),
        });
        self.diagnostics.push(diag);
    }

    /// 语句级同步：跳过当前无法解析的 token，直到语句边界（`;` / `}` / 行首）。
    pub(crate) fn recover_to_statement_boundary(&mut self) {
        let line = self.peek().map(|token| token.line).unwrap_or(0);
        while let Some(token) = self.peek().cloned() {
            match token.kind {
                TokenKind::SemiColon => {
                    self.advance();
                    return;
                }
                TokenKind::CloseBrace | TokenKind::Eof => return,
                _ if token.line != line => return,
                _ => {
                    self.advance();
                }
            }
        }
    }
}

//! match 表达式解析：分支、默认分支、分支体与错误恢复。

use novella_ast::expr::{Expression, Match, MatchCase, MatchCaseBody, MatchPattern};
use novella_core::{Span, token::TokenKind};

use crate::Parser;

impl Parser {
    // ---------- match ----------

    /// 解析 match 表达式（对应文法 `matchExpression`）。
    ///
    /// 前置：游标位于 `match`。
    ///
    /// 分支体可内含更深的 match（`match a { case 1 => match b { ... } }`），
    /// 故同样经 [`Parser::guarded`] 扩展分段栈；实体拆到
    /// [`Parser::parse_match_inner`]。
    pub(crate) fn parse_match(&mut self) -> Option<Expression> {
        self.guarded(|p| p.parse_match_inner())?
    }

    /// [`Parser::parse_match`] 的实体（分段栈扩展由外层包装负责）。
    fn parse_match_inner(&mut self) -> Option<Expression> {
        let keyword = self.advance()?;
        let start = keyword.span.start;
        // 主体后紧跟的 `{` 是 match 块，不能交给 StructInit 当结构体字面量
        let was_match_subject = std::mem::replace(&mut self.in_match_subject, true);
        let subject = self.parse_expression();
        self.in_match_subject = was_match_subject;
        let subject = subject?;
        self.expect(TokenKind::OpenBrace)?;
        let mut cases = vec![];
        let mut default = None;
        while !self.check(TokenKind::CloseBrace) && !self.at_eof() {
            let before = self.pos;
            if self.check(TokenKind::Default) {
                let default_keyword = self.advance()?;
                match self.parse_match_body() {
                    Some(body) => {
                        // C-05：default 不再静默覆盖——重复出现报诊断，
                        // 保留首个 default，丢弃后续重复分支
                        if default.is_some() {
                            self.error_at_span(
                                default_keyword.span,
                                "match 中只允许一个 default 分支",
                            );
                        } else {
                            default = Some(body);
                        }
                    }
                    None => self.recover_to_next_match_arm(before),
                }
                continue;
            }
            if self.check(TokenKind::Case) {
                let case_keyword_span = self.peek().map(|token| token.span);
                match self.parse_match_case() {
                    Some(case) => {
                        // 文法 `matchCase+ matchDefault?`：default 必须位于末尾，
                        // 其后的 case 报诊断但仍保留（C-05）
                        if default.is_some()
                            && let Some(span) = case_keyword_span
                        {
                            self.error_at_span(span, "case 分支不能出现在 default 之后");
                        }
                        cases.push(case);
                    }
                    None => self.recover_to_next_match_arm(before),
                }
                continue;
            }
            // 既不是 case / default / }：跳过该 token 继续，保证前进一步
            self.error_here("此处应为 `case` / `default` 或 `}`");
            self.advance();
        }
        let close = self.expect(TokenKind::CloseBrace)?;
        if cases.is_empty() {
            self.error_at_span(close.span, "match 至少需要一个 case 分支");
        }
        Some(Expression::Match(Match {
            subject: Box::new(subject),
            cases,
            default,
            span: Span {
                start,
                end: close.span.end,
            },
        }))
    }

    /// match 分支解析失败后的同步：跳到下一个 `case` / `default` / `}`。
    fn recover_to_next_match_arm(&mut self, before: usize) {
        if self.pos == before {
            self.advance();
        }
        while let Some(token) = self.peek() {
            match token.kind {
                TokenKind::Case | TokenKind::Default | TokenKind::CloseBrace | TokenKind::Eof => {
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// 解析单个 match 分支（对应文法 `matchCase`）。
    fn parse_match_case(&mut self) -> Option<MatchCase> {
        let keyword = self.expect(TokenKind::Case)?;
        let start = keyword.span.start;
        // 模式：`case : T` 是类型模式，其余是表达式模式
        let pattern = if self.check(TokenKind::Colon) {
            self.advance();
            // 类型模式：类型之后紧跟分支箭头 `=>`，该 `=>` 属于 match 语法
            // 而非「类型位 `=>`」诊断对象（E-03 例外，同 lambda 返回注解）
            let was_allowed = std::mem::replace(&mut self.allow_arrow_after_type, true);
            let ty = self.parse_type_expr();
            self.allow_arrow_after_type = was_allowed;
            let ty = ty?;
            MatchPattern::Type {
                span: ty.span(),
                ty,
            }
        } else {
            let expr = self.parse_match_pattern_expression()?;
            MatchPattern::Expr {
                span: expr.span(),
                expr: Box::new(expr),
            }
        };
        // 分支体内的表达式遇到下一个 `case` / `default` / `}` 即结束
        self.inside_match += 1;
        let body = self.parse_match_body();
        self.inside_match -= 1;
        let body = body?;
        let end = body.span().end;
        Some(MatchCase {
            pattern,
            body,
            span: Span { start, end },
        })
    }

    pub(crate) fn at_match_pattern_boundary(&self) -> bool {
        self.match_pattern_boundary
            .is_some_and(|start| self.peek().is_some_and(|token| token.span.start == start))
    }

    fn parse_match_pattern_expression(&mut self) -> Option<Expression> {
        let checkpoint = self.checkpoint();
        let pattern = self.parse_expression();
        if pattern.is_some() && (self.check(TokenKind::Arrow) || self.check(TokenKind::OpenBrace)) {
            return pattern;
        }
        // 普通表达式可能把模式后的箭头或块吃进 lambda / struct 初始化。
        // 只在未留下分支定界符时重试，保留合法的内层 lambda 与 struct 表达式。
        if let Some(boundary) = self.match_pattern_delimiter(checkpoint.0) {
            self.rollback(checkpoint);
            let previous = self.match_pattern_boundary.replace(boundary);
            let retry = self.parse_expression();
            self.match_pattern_boundary = previous;
            return retry;
        }
        pattern
    }

    fn match_pattern_delimiter(&self, start: usize) -> Option<usize> {
        let mut depth = 0;
        let mut block = None;
        for token in &self.tokens[start..] {
            match token.kind {
                TokenKind::Arrow if depth == 0 => return Some(token.span.start),
                TokenKind::OpenBrace => {
                    if depth == 0 {
                        block = Some(token.span.start);
                    }
                    depth += 1;
                }
                TokenKind::OpenParen | TokenKind::OpenBracket => depth += 1,
                TokenKind::CloseBrace | TokenKind::CloseParen | TokenKind::CloseBracket
                    if depth > 0 =>
                {
                    depth -= 1
                }
                TokenKind::Eof => break,
                TokenKind::Case | TokenKind::Default | TokenKind::CloseBrace if depth == 0 => {
                    break;
                }
                _ => {}
            }
        }
        block
    }

    /// 解析分支的表达式体。
    ///
    /// 分支之间没有分隔符，表达式体遇到下一个 `case` / `default` / `}` 即自然结束；
    /// 若在表达式位置直接看到这些 token，则记录一条「应以表达式开始」的诊断并返回 `None`。
    fn parse_match_case_expression(&mut self) -> Option<Expression> {
        if matches!(
            self.peek().map(|token| token.kind),
            Some(TokenKind::Case | TokenKind::Default | TokenKind::CloseBrace)
        ) {
            self.error_here("此处应为表达式");
            return None;
        }
        self.parse_expression()
    }

    /// 解析分支体：`=> expression` 或 `block`（对应文法 `'=>' expression | block`）。
    fn parse_match_body(&mut self) -> Option<MatchCaseBody> {
        if self.eat(TokenKind::Arrow).is_some() {
            // 实参列表前若已到下一个分支或 `}`，说明表达式体为空（缺省写法）
            let expr = self.parse_match_case_expression()?;
            let span = expr.span();
            return Some(MatchCaseBody::Expr {
                expr: Box::new(expr),
                span,
            });
        }
        let block = self.parse_block()?;
        let span = block.span;
        Some(MatchCaseBody::Block { block, span })
    }
}

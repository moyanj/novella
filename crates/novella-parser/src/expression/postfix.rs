//! 后缀与调用：成员 / 索引 / 调用 / 宏调用 / `new` / 后缀自增、泛型实参扫描。

use novella_ast::{
    expr::{Expression, Literal, UnaryOp},
    types::TypeExpr,
};
use novella_core::{
    Span,
    token::{Token, TokenKind},
};

use crate::{Parser, split_token_pair};

use super::helper::is_type_token;

impl Parser {
    pub(crate) fn parse_postfix(&mut self) -> Option<Expression> {
        let mut expr = self.parse_primary()?;
        loop {
            let kind = self.peek().map(|t| t.kind);
            match kind {
                // 成员访问 `a.b`
                Some(TokenKind::Dot) => {
                    self.advance();
                    let name = self.expect(TokenKind::Identifier)?;
                    let span = Span {
                        start: expr.span().start,
                        end: name.span.end,
                    };
                    expr = Expression::MemberAccess {
                        target: Box::new(expr),
                        name: name.slice,
                        span,
                    };
                }
                // 索引访问 `a[i]`
                Some(TokenKind::OpenBracket) => {
                    self.advance();
                    let index = self.parse_expression()?;
                    let close = self.expect(TokenKind::CloseBracket)?;
                    let span = Span {
                        start: expr.span().start,
                        end: close.span.end,
                    };
                    expr = Expression::IndexAccess {
                        target: Box::new(expr),
                        index: Box::new(index),
                        span,
                    };
                }
                // 宏调用 `f!(a, b)`
                Some(TokenKind::Not) if self.check_n(TokenKind::OpenParen, 1) => {
                    let checkpoint = self.checkpoint();
                    self.advance();
                    let Some(args) = self.parse_arguments() else {
                        // 实参失败（如 `f!(x)` 中 `x` 之后不是 `)`）：回退到 `!` 之前，
                        // 保留已解析的 `expr`，由上层按缺少后续运算符继续
                        self.rollback(checkpoint);
                        break;
                    };
                    let end = self.last_end();
                    let span = Span {
                        start: expr.span().start,
                        end,
                    };
                    expr = Expression::MacroCall {
                        callee: Box::new(expr),
                        args,
                        span,
                    };
                }
                // 调用 `f(a)` / 泛型调用 `f<int>(a)`
                Some(TokenKind::OpenParen) | Some(TokenKind::LessThan)
                    if self.looks_like_call() =>
                {
                    // 实参解析失败说明不是调用（如 `f(*p)` 等歧义）：放弃后缀，
                    // 让上层按二元/关系运算继续，未消费的 token 交给语句层同步。
                    let checkpoint = self.checkpoint();
                    // 后缀是累积构造的，这里先把已构造好的部分暂存一份：
                    // 若调用解析失败，就回滚游标并退回这份快照继续解析其它后缀。
                    let saved_expr = expr.clone();
                    let callee = std::mem::replace(
                        &mut expr,
                        Expression::Literal {
                            value: Literal::Null,
                            span: Span { start: 0, end: 0 },
                        },
                    );
                    match self.parse_call_suffix(callee) {
                        Some(call) => expr = call,
                        None => {
                            // 实参无法解析：回滚游标与诊断，保留快照继续，
                            // 避免整条表达式被丢弃导致后续语句连锁解析失败
                            self.rollback(checkpoint);
                            expr = saved_expr;
                            break;
                        }
                    }
                }
                // 后缀自增 / 自减
                Some(TokenKind::PlusPlus) => {
                    let token = self.advance()?;
                    let span = Span {
                        start: expr.span().start,
                        end: token.span.end,
                    };
                    expr = Expression::Unary {
                        op: UnaryOp::PostIncrement,
                        operand: Box::new(expr),
                        span,
                    };
                }
                Some(TokenKind::MinusMinus) => {
                    let token = self.advance()?;
                    let span = Span {
                        start: expr.span().start,
                        end: token.span.end,
                    };
                    expr = Expression::Unary {
                        op: UnaryOp::PostDecrement,
                        operand: Box::new(expr),
                        span,
                    };
                }
                _ => break,
            }
        }
        Some(expr)
    }

    /// 在 `callee` 之后解析 `[<type_args>] '(' args ')'`，产出调用表达式。
    ///
    /// 泛型实参解析失败（如 `a < b > (c)` 这类比较链被误判）时，
    /// 游标回退到尖括号之前并返回 `None`，交回关系运算符路径处理。
    fn parse_call_suffix(&mut self, callee: Expression) -> Option<Expression> {
        let saved = self.pos;
        let saved_diagnostics = self.diagnostics.len();
        let type_args = if self.check(TokenKind::LessThan) {
            match self.parse_call_type_args() {
                Some(args) => args,
                None => {
                    self.pos = saved;
                    self.diagnostics.truncate(saved_diagnostics);
                    return None;
                }
            }
        } else {
            vec![]
        };
        if !self.check(TokenKind::OpenParen) {
            self.pos = saved;
            self.diagnostics.truncate(saved_diagnostics);
            return None;
        }
        let args = self.parse_arguments()?;
        let span = Span {
            start: callee.span().start,
            end: self.last_end(),
        };
        Some(Expression::Call {
            callee: Box::new(callee),
            type_args,
            args,
            span,
        })
    }

    /// 当前 token 是否是「调用」的开头。
    ///
    /// `(` 一定是；`<` 需要向后确认「尖括号内全是类型 token 且闭合 `>` 之后紧跟 `(`」，
    /// 否则它是比较运算符（`a < b`）而非泛型实参。
    pub(crate) fn looks_like_call(&self) -> bool {
        if self.check(TokenKind::OpenParen) {
            return true;
        }
        if !self.check(TokenKind::LessThan) {
            return false;
        }
        let Some((inner, after)) = self.scan_call_angle() else {
            return false;
        };
        if inner.is_empty() || !inner.iter().all(|token| is_type_token(token.kind)) {
            return false;
        }
        // `>` 之后紧跟 `(` 才是泛型调用；`a < b > (c)` 这类比较链虽形似，
        // 但会在类型解析失败时退回关系运算（见 `parse_postfix` 的兜底）。
        after == Some(TokenKind::OpenParen)
    }

    /// 扫描调用处的尖括号，返回括号内 token 与闭合 `>` 之后下一个 token 的种类。
    ///
    /// 与 [`Parser::scan_angle`] 的区别：这里报告闭合之后的位置，用于判断
    /// `f<T>(` 中的 `(`。`>>` 闭合时后半个 `>` 仍属于后续扫描，故直接指向它之后。
    fn scan_call_angle(&self) -> Option<(Vec<Token>, Option<TokenKind>)> {
        let mut inner = vec![];
        let mut depth = 0usize;
        let mut idx = self.pos;
        while let Some(token) = self.tokens.get(idx) {
            idx += 1;
            match token.kind {
                TokenKind::LessThan => {
                    depth += 1;
                    if depth > 1 {
                        inner.push(token.clone());
                    }
                }
                TokenKind::LeftShiftArithmetic => {
                    depth += 2;
                    let (first, second) =
                        split_token_pair(token, TokenKind::LessThan, TokenKind::LessThan);
                    depth -= 2;
                    inner.push(first);
                    inner.push(second);
                }
                TokenKind::MoreThan => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((inner, self.tokens.get(idx).map(|t| t.kind)));
                    }
                    inner.push(token.clone());
                }
                TokenKind::RightShiftArithmetic => {
                    let (first, second) =
                        split_token_pair(token, TokenKind::MoreThan, TokenKind::MoreThan);
                    depth -= 1;
                    if depth == 0 {
                        // 第一个 `>` 闭合当前尖括号；下一个「token」是它的后半段，
                        // 不可能是 `(`，故直接返回 None 交回关系运算符路径。
                        let _ = second;
                        return Some((inner, None));
                    }
                    depth -= 1;
                    inner.push(first);
                    if depth == 0 {
                        return Some((inner, self.tokens.get(idx).map(|t| t.kind)));
                    }
                    inner.push(second);
                }
                TokenKind::SemiColon
                | TokenKind::OpenBrace
                | TokenKind::CloseBrace
                | TokenKind::Eof => return None,
                _ => {
                    if depth == 0 {
                        return None;
                    }
                    inner.push(token.clone());
                }
            }
        }
        None
    }

    /// 解析调用处的泛型实参 `f<int>(...)`；前置：游标位于 `<`。
    pub(crate) fn parse_call_type_args(&mut self) -> Option<Vec<TypeExpr>> {
        let mut type_args = vec![];
        self.expect(TokenKind::LessThan)?;
        while let Some(ty) = self.parse_type_expr() {
            type_args.push(ty);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect_angle_close();
        Some(type_args)
    }

    /// 解析实参列表 `(a, b,)`（对应文法 `argumentList`）。
    pub(crate) fn parse_arguments(&mut self) -> Option<Vec<Expression>> {
        // 实参列表属于「括号内部」：with 表头内嵌套调用实参中的 `as`
        // 一律按 cast 处理，不参与 with 绑定判定（E-02）
        self.with_shield += 1;
        let result = self.parse_arguments_inner();
        self.with_shield -= 1;
        result
    }

    /// [`Parser::parse_arguments`] 的实体（with 屏蔽由外层包装负责）。
    fn parse_arguments_inner(&mut self) -> Option<Vec<Expression>> {
        self.expect(TokenKind::OpenParen)?;
        let mut args = vec![];
        if !self.check(TokenKind::CloseParen) {
            loop {
                args.push(self.parse_expression()?);
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                if self.check(TokenKind::CloseParen) {
                    break;
                }
            }
        }
        self.expect(TokenKind::CloseParen)?;
        Some(args)
    }

    /// 上一个已消费 token 的结束位置（用于构造区间）。
    pub(crate) fn last_end(&self) -> usize {
        self.pos
            .checked_sub(1)
            .and_then(|idx| self.tokens.get(idx))
            .map(|token| token.span.end)
            .unwrap_or(0)
    }
}

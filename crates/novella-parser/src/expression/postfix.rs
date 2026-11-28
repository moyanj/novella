//! 后缀与调用：成员 / 索引 / 调用 / `new` / 后缀自增、泛型实参扫描。

use novella_ast::{
    expr::{Expression, UnaryOp},
    types::TypeExpr,
};
use novella_core::{Span, token::TokenKind};

use crate::Parser;

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
                // 调用 `f(a)` / 泛型调用 `f<int>(a)`
                Some(TokenKind::OpenParen) | Some(TokenKind::LessThan)
                    if self.looks_like_call() =>
                {
                    // 实参解析失败说明不是调用（如 `f(*p)` 等歧义）：放弃后缀，
                    // 让上层按二元/关系运算继续，未消费的 token 交给语句层同步。
                    let checkpoint = self.checkpoint();
                    match self.parse_call_suffix(&expr) {
                        Some(call) => expr = call,
                        None => {
                            // 实参无法解析：回滚游标（含诊断），保留已构建的左半表达式
                            // 继续交给上层，避免整条表达式被丢弃导致后续语句连锁失败
                            self.rollback(checkpoint);
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
    /// `callee` 以借用传入：本方法只读它的 span，只有成功时才 clone 进 `Expression::Call`。
    /// 因此失败路径不会留下任何占位节点（旧实现需要用 `Literal::Null` 换手，失败时
    /// 会把一个指向 0 字节的 `null` 字面量混进表达式树，让 sema 报出定位错误的类型诊断）。
    ///
    /// 泛型实参解析失败（如 `a < b > (c)` 这类比较链被误判）、或实参解析失败时，
    /// 本方法负责把游标与诊断回滚到调用前的状态并返回 `None`，交回关系运算符路径处理。
    fn parse_call_suffix(&mut self, callee: &Expression) -> Option<Expression> {
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
        // 实参解析失败也必须回滚（旧实现里的 `?` 会带着已产生的诊断和半截游标直接返回）
        let Some(args) = self.parse_arguments() else {
            self.pos = saved;
            self.diagnostics.truncate(saved_diagnostics);
            return None;
        };
        let span = Span {
            start: callee.span().start,
            end: self.last_end(),
        };
        Some(Expression::Call {
            callee: Box::new(callee.clone()),
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
        self.is_call_angle().is_some()
    }

    /// 判断 `self.pos` 处的 `<` 是否开启泛型实参；是则返回闭合 `>` 的 token 下标。
    ///
    /// 原实现先完整扫描尖括号再判类型，`a < b && c < d ...` 这类比较链上每个 `<`
    /// 都会扫到表达式结尾，整体退化为 O(n²)（3KB 链即上百毫秒）。这里改为单趟：
    /// 逐 token 判定「是否仍是合法的类型实参」，一旦首个非类型 token 出现就停止，
    /// 比较链通常在第二个 token 即失败。仅当全部 token 都是类型 token 时才接受扫描结果，
    /// 因此与旧实现的接受集合完全一致（旧实现的 `!is_type_token → false` 分支现在提前）。
    fn is_call_angle(&self) -> Option<usize> {
        let mut depth = 0usize;
        let mut idx = self.pos;
        while let Some(token) = self.tokens.get(idx) {
            idx += 1;
            match token.kind {
                TokenKind::LessThan => {
                    depth += 1;
                    if !is_type_token(token.kind) {
                        return None;
                    }
                }
                TokenKind::LeftShiftArithmetic => {
                    // 拆成两个 `<`；内层 `<` 是类型 token，外层仍属尖括号。
                    depth += 2;
                }
                TokenKind::MoreThan => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return (self.tokens.get(idx).map(|t| t.kind)
                            == Some(TokenKind::OpenParen))
                        .then_some(idx - 1);
                    }
                }
                TokenKind::RightShiftArithmetic => {
                    // 第一个 `>` 闭合当前尖括号；其后是后半个 `>`，不可能是 `(`。
                    if depth <= 1 {
                        return None;
                    }
                    depth -= 2;
                    if depth == 0 {
                        return (self.tokens.get(idx).map(|t| t.kind)
                            == Some(TokenKind::OpenParen))
                        .then_some(idx - 1);
                    }
                }
                TokenKind::SemiColon
                | TokenKind::OpenBrace
                | TokenKind::CloseBrace
                | TokenKind::Eof => return None,
                kind => {
                    if depth == 0 || !is_type_token(kind) {
                        return None;
                    }
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

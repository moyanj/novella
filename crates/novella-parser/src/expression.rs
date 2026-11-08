//! 表达式解析（对应文法 `expression` 的全部产生式）。
//!
//! 采用分层递归（每层一个函数，层序即文法中 ANTLR 左递归备选的优先级顺序），
//! 完成文法 `primaryExpression` / `tupleExpression` / 各二元与一元运算 /
//! 调用与访问 / 三元 / 赋值 / `is` / `as` / lambda / match。
//!
//! 优先级（高 → 低，与文法备选顺序一致）：
//! 原子 → 成员 / 索引 / 调用 / 宏调用 / new / 后缀自增 → 前缀一元 / await / `is`
//! → `* / %` → `+ -` → `as` → 移位 → 关系 → 相等 → `&` → `^` → `|`
//! → `&&` → `||` → `??` → 三元 → lambda → 赋值（右结合）。

use novella_ast::{
    expr::{
        AssignOp, BinaryOp, Expression, Lambda, LambdaBody, LambdaParams, Literal, Match,
        MatchCase, MatchCaseBody, MatchPattern, PropertyAssignment, UnaryOp,
    },
    types::TypeExpr,
};
use novella_core::{
    Span,
    token::{Token, TokenKind},
};

use crate::{Parser, split_token_pair};

impl Parser {
    /// 解析表达式（对应文法 `expression`）。
    pub(crate) fn parse_expression(&mut self) -> Option<Expression> {
        // match 分支内：`case` / `default` / `}` 是表达式体的天然终止符
        if self.inside_match > 0
            && matches!(
                self.peek().map(|token| token.kind),
                Some(TokenKind::Case | TokenKind::Default | TokenKind::CloseBrace)
            )
        {
            return None;
        }
        let depth = self.enter_nesting();
        // 超出嵌套上限：立即掐断递归（诊断已在 `enter_nesting` 里报过一条），
        // 否则病态输入（上万个 `(`）会耗尽栈直接 abort。
        let expr = if self.nesting_exceeded() {
            None
        } else {
            self.parse_assignment()
        };
        self.leave_nesting(depth);
        expr
    }

    /// 赋值层（对应文法 `AssignmentExpr`，右结合）。
    ///
    /// 右结合靠自递归实现，是递归入口之一：先登记嵌套深度、超限即截断（A-03）。
    fn parse_assignment(&mut self) -> Option<Expression> {
        let depth = self.enter_nesting();
        let result = if self.nesting_exceeded() {
            None
        } else {
            self.parse_assignment_chain()
        };
        self.leave_nesting(depth);
        result
    }

    /// [`Parser::parse_assignment`] 的实体（嵌套防护由外层包装负责）。
    fn parse_assignment_chain(&mut self) -> Option<Expression> {
        let left = self.parse_ternary()?;
        let Some(op) = self.peek().and_then(|t| assign_op(t.kind)) else {
            return Some(left);
        };
        self.advance();
        let value = self.parse_assignment()?;
        let span = Span {
            start: left.span().start,
            end: value.span().end,
        };
        Some(Expression::Assignment {
            target: Box::new(left),
            op,
            value: Box::new(value),
            span,
        })
    }

    /// 三元层（对应文法 `TernaryExpr`）。
    ///
    /// then / else 分支都靠递归实现，是递归入口之一：先登记嵌套深度、
    /// 超限即截断（A-02）。
    fn parse_ternary(&mut self) -> Option<Expression> {
        let depth = self.enter_nesting();
        let result = if self.nesting_exceeded() {
            None
        } else {
            self.parse_ternary_chain()
        };
        self.leave_nesting(depth);
        result
    }

    /// [`Parser::parse_ternary`] 的实体（嵌套防护由外层包装负责）。
    fn parse_ternary_chain(&mut self) -> Option<Expression> {
        let condition = self.parse_coalesce()?;
        if self.eat(TokenKind::QuestionMark).is_none() {
            return Some(condition);
        }
        let then_expr = self.parse_ternary()?;
        self.expect(TokenKind::Colon)?;
        let else_expr = self.parse_assignment()?;
        let span = Span {
            start: condition.span().start,
            end: else_expr.span().end,
        };
        Some(Expression::Ternary {
            condition: Box::new(condition),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
            span,
        })
    }

    /// 前瞻判断 `as` 之后是否是 `with` 语句头部的绑定（`as file)` / `as f {` /
    /// `as (k, v)` 等形态）。
    ///
    /// 绑定形态（E-02「绑定优先」）：
    /// - `as (` 且括号内以「标识符 + `,` / `)`」开头 → 元组解构绑定；
    /// - `as` 后为标识符，且紧随 `)` / `:` / `{` 之一（表头结束标记）。
    ///
    /// `expr as T` 作为表达式（cast）时，类型之后通常还有运算符、`[` 等，
    /// 因此这条规则只会在 `with` 表头产生误判——而那里 `as` 本来就应该
    /// 交给 `with` 处理；表头内嵌套括号中的 `as` 由 [`Parser::with_shield`]
    /// 放行（如 `with ((e as T))`、`with (f(e as T))`）。
    fn looks_like_with_binding(&self) -> bool {
        if !self.check(TokenKind::As) {
            return false;
        }
        // 元组解构绑定：`as (a, b)` / `as (a)`（绑定优先，cast 到元组类型
        // 需用双层括号 `with ((e as (A, B)))` 逃逸）
        if self.check_n(TokenKind::OpenParen, 1) {
            return self.check_n(TokenKind::Identifier, 2)
                && matches!(
                    self.peek_n(3).map(|token| token.kind),
                    Some(TokenKind::Comma | TokenKind::CloseParen)
                );
        }
        if !self.check_n(TokenKind::Identifier, 1) {
            return false;
        }
        matches!(
            self.peek_n(2).map(|token| token.kind),
            Some(TokenKind::CloseParen | TokenKind::Colon | TokenKind::OpenBrace)
        )
    }

    /// `??` 层（对应文法 `CoalesceExpr`）。
    fn parse_coalesce(&mut self) -> Option<Expression> {
        let mut left = self.parse_logical_or()?;
        while self.check(TokenKind::NullCoalesce) {
            self.advance();
            let right = self.parse_logical_or()?;
            left = binary(BinaryOp::NullCoalesce, left, right);
        }
        Some(left)
    }

    /// `||` / `or` 层（对应文法 `LogicalOrExpr`）。
    fn parse_logical_or(&mut self) -> Option<Expression> {
        let mut left = self.parse_logical_and()?;
        while matches!(
            self.peek().map(|t| t.kind),
            Some(TokenKind::Or | TokenKind::KeyOr)
        ) {
            self.advance();
            let right = self.parse_logical_and()?;
            left = binary(BinaryOp::LogicalOr, left, right);
        }
        Some(left)
    }

    /// `&&` / `and` 层（对应文法 `LogicalAndExpr`）。
    fn parse_logical_and(&mut self) -> Option<Expression> {
        let mut left = self.parse_bit_or()?;
        while matches!(
            self.peek().map(|t| t.kind),
            Some(TokenKind::And | TokenKind::KeyAnd)
        ) {
            self.advance();
            let right = self.parse_bit_or()?;
            left = binary(BinaryOp::LogicalAnd, left, right);
        }
        Some(left)
    }

    /// `|` 层（对应文法 `BitOrExpr`）。
    fn parse_bit_or(&mut self) -> Option<Expression> {
        let mut left = self.parse_bit_xor()?;
        while self.check(TokenKind::BitOr) {
            self.advance();
            let right = self.parse_bit_xor()?;
            left = binary(BinaryOp::BitOr, left, right);
        }
        Some(left)
    }

    /// `^` 层（对应文法 `BitXorExpr`）。
    fn parse_bit_xor(&mut self) -> Option<Expression> {
        let mut left = self.parse_bit_and()?;
        while self.check(TokenKind::BitXOr) {
            self.advance();
            let right = self.parse_bit_and()?;
            left = binary(BinaryOp::BitXor, left, right);
        }
        Some(left)
    }

    /// `&` 层（对应文法 `BitAndExpr`）。
    fn parse_bit_and(&mut self) -> Option<Expression> {
        let mut left = self.parse_equality()?;
        while self.check(TokenKind::BitAnd) {
            self.advance();
            let right = self.parse_equality()?;
            left = binary(BinaryOp::BitAnd, left, right);
        }
        Some(left)
    }

    /// `==` / `!=` / `===` / `!==` 层（对应文法 `EqualityExpr`）。
    fn parse_equality(&mut self) -> Option<Expression> {
        let mut left = self.parse_relational()?;
        loop {
            let op = match self.peek().map(|t| t.kind) {
                Some(TokenKind::Equals) => BinaryOp::Equals,
                Some(TokenKind::NotEquals) => BinaryOp::NotEquals,
                Some(TokenKind::IdentityEquals) => BinaryOp::IdentityEquals,
                Some(TokenKind::IdentityNotEquals) => BinaryOp::IdentityNotEquals,
                _ => break,
            };
            self.advance();
            let right = self.parse_relational()?;
            left = binary(op, left, right);
        }
        Some(left)
    }

    /// `<` / `>` / `<=` / `>=` 层（对应文法 `RelationalExpr`）。
    fn parse_relational(&mut self) -> Option<Expression> {
        let mut left = self.parse_shift()?;
        loop {
            let op = match self.peek().map(|t| t.kind) {
                Some(TokenKind::LessThan) => BinaryOp::LessThan,
                Some(TokenKind::MoreThan) => BinaryOp::GreaterThan,
                Some(TokenKind::LessThanEquals) => BinaryOp::LessThanEquals,
                Some(TokenKind::GreaterThanEquals) => BinaryOp::GreaterThanEquals,
                _ => break,
            };
            self.advance();
            let right = self.parse_shift()?;
            left = binary(op, left, right);
        }
        Some(left)
    }

    /// `<<` / `>>` 层（对应文法 `ShiftExpr`）。
    ///
    /// `>>` 在此处是单个移位 token：泛型实参列表走的是自己的解析路径
    /// （[`Parser::scan_angle`]），不会被这一层看到。
    fn parse_shift(&mut self) -> Option<Expression> {
        let mut left = self.parse_cast()?;
        loop {
            let op = match self.peek().map(|t| t.kind) {
                Some(TokenKind::LeftShiftArithmetic) => BinaryOp::LeftShift,
                Some(TokenKind::RightShiftArithmetic) => BinaryOp::RightShift,
                _ => break,
            };
            self.advance();
            let right = self.parse_cast()?;
            left = binary(op, left, right);
        }
        Some(left)
    }

    /// `as` 层（对应文法 `CastExpr`，位于加减之后）。
    fn parse_cast(&mut self) -> Option<Expression> {
        let mut expr = self.parse_additive()?;
        // `with (expr as binding)`：`as` 后是绑定形态时属于 `with` 的上下文绑定
        // 而非强制转换（E-02 绑定优先）；表头内嵌套括号（`with_shield > 0`，
        // 如 `with ((e as T))`、`with (f(e as T))`）中的 `as` 不受此限，按 cast 处理
        while self.check(TokenKind::As)
            && !(self.with_stack > 0 && self.with_shield == 0 && self.looks_like_with_binding())
        {
            self.advance();
            let ty = self.parse_type_expr()?;
            let span = Span {
                start: expr.span().start,
                end: ty.span().end,
            };
            expr = Expression::Cast {
                expr: Box::new(expr),
                ty,
                span,
            };
        }
        Some(expr)
    }

    /// `+` / `-` 层（对应文法 `AdditiveExpr`）。
    fn parse_additive(&mut self) -> Option<Expression> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek().map(|t| t.kind) {
                Some(TokenKind::Plus) => BinaryOp::Plus,
                Some(TokenKind::Minus) => BinaryOp::Minus,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            left = binary(op, left, right);
        }
        Some(left)
    }

    /// `*` / `/` / `%` 层（对应文法 `MultiplicativeExpr`）。
    fn parse_multiplicative(&mut self) -> Option<Expression> {
        let mut left = self.parse_is()?;
        loop {
            let op = match self.peek().map(|t| t.kind) {
                Some(TokenKind::Multiply) => BinaryOp::Multiply,
                Some(TokenKind::Divide) => BinaryOp::Divide,
                Some(TokenKind::Modulus) => BinaryOp::Modulus,
                _ => break,
            };
            self.advance();
            let right = self.parse_is()?;
            left = binary(op, left, right);
        }
        Some(left)
    }

    /// `is` 层（对应文法 `IsExpr`，右操作数是类型）。
    fn parse_is(&mut self) -> Option<Expression> {
        let operand = self.parse_unary()?;
        if self.eat(TokenKind::Is).is_some() {
            let ty = self.parse_type_expr()?;
            let span = Span {
                start: operand.span().start,
                end: ty.span().end,
            };
            return Some(Expression::Is {
                operand: Box::new(operand),
                ty,
                span,
            });
        }
        Some(operand)
    }

    /// 前缀一元层（对应文法 `PrefixExpr` / `AwaitExpr`），右结合。
    ///
    /// 右结合靠自递归实现，是递归入口之一：先登记嵌套深度、超限即截断（A-01）；
    /// 实体内的 `?` 提前返回不得跳过深度恢复（A-05），故实体拆到
    /// [`Parser::parse_unary_inner`]，本包装保证 `leave_nesting` 成对执行。
    fn parse_unary(&mut self) -> Option<Expression> {
        let depth = self.enter_nesting();
        let result = if self.nesting_exceeded() {
            None
        } else {
            self.parse_unary_inner()
        };
        self.leave_nesting(depth);
        result
    }

    /// [`Parser::parse_unary`] 的实体（嵌套防护由外层包装负责）。
    fn parse_unary_inner(&mut self) -> Option<Expression> {
        let unary_op = match self.peek().map(|t| t.kind) {
            Some(TokenKind::Plus) => Some(UnaryOp::Plus),
            Some(TokenKind::Minus) => Some(UnaryOp::Minus),
            Some(TokenKind::Not | TokenKind::KeyNot) => Some(UnaryOp::Not),
            Some(TokenKind::BitNot) => Some(UnaryOp::BitNot),
            Some(TokenKind::BitAnd) => Some(UnaryOp::AddressOf),
            Some(TokenKind::Multiply) => Some(UnaryOp::DerefPointer),
            _ => None,
        };
        if let Some(op) = unary_op {
            let keyword = self.advance()?;
            let operand = self.parse_unary()?;
            let span = Span {
                start: keyword.span.start,
                end: operand.span().end,
            };
            Some(Expression::Unary {
                op,
                operand: Box::new(operand),
                span,
            })
        } else if let Some(keyword) = self.eat(TokenKind::Await) {
            let expr = self.parse_unary()?;
            let span = Span {
                start: keyword.span.start,
                end: expr.span().end,
            };
            Some(Expression::Await {
                expr: Box::new(expr),
                span,
            })
        } else {
            self.parse_postfix()
        }
    }

    /// 后缀层（对应文法 `PostfixExpr` 与访问 / 调用链）。
    ///
    /// `new` 与宏调用在原子层处理：文法中 `New expression` 的目标是紧随其后的
    /// 表达式（含调用），故 `new Foo(1)` 由 `New` + `Call` 组合而成。
    fn parse_postfix(&mut self) -> Option<Expression> {
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

    /// 原子表达式（对应文法 `primaryExpression`）。
    ///
    /// 括号 / lambda / match 等分支都会递归回表达式解析，是递归入口之一：
    /// 先登记嵌套深度、超限即截断；实体拆到 [`Parser::parse_primary_atom`]，
    /// 保证其中的 `?` / 提前返回不会跳过深度恢复（A-05）。
    fn parse_primary(&mut self) -> Option<Expression> {
        let depth = self.enter_nesting();
        let result = if self.nesting_exceeded() {
            None
        } else {
            self.parse_primary_atom()
        };
        self.leave_nesting(depth);
        result
    }

    /// [`Parser::parse_primary`] 的实体（嵌套防护由外层包装负责）。
    ///
    /// 分支顺序与文法一致：字面量 → `new` → lambda → `match` → 标识符 + 对象字面量
    /// （struct 构造）→ 标识符 → 括号 / 元组 → 数组 → 对象 → `this`。
    fn parse_primary_atom(&mut self) -> Option<Expression> {
        match self.peek().map(|t| t.kind) {
            Some(TokenKind::Null) => {
                let token = self.advance()?;
                Some(Expression::Literal {
                    value: Literal::Null,
                    span: token.span,
                })
            }
            Some(TokenKind::BooleanLiteral) => {
                let token = self.advance()?;
                Some(Expression::Literal {
                    value: Literal::Boolean(token.slice == "true"),
                    span: token.span,
                })
            }
            Some(
                TokenKind::StringLiteral
                | TokenKind::FormatStringLiteral
                | TokenKind::RawStringLiteral
                | TokenKind::ByteStringLiteral,
            ) => {
                let token = self.advance()?;
                Some(Expression::Literal {
                    value: Literal::String(token.slice),
                    span: token.span,
                })
            }
            Some(
                TokenKind::DecimalLiteral
                | TokenKind::HexIntegerLiteral
                | TokenKind::OctalIntegerLiteral
                | TokenKind::BinaryIntegerLiteral,
            ) => {
                let token = self.advance()?;
                Some(Expression::Literal {
                    value: Literal::Number(token.slice),
                    span: token.span,
                })
            }
            Some(TokenKind::New) => {
                let keyword = self.advance()?;
                // 文法 `New expression`：构造目标是紧随其后的整条后缀表达式
                // （标识符链 / 括号 / 调用）。若该表达式整体是一次调用，
                // 则其 callee 作为构造目标、type_args / args 提升为构造实参
                // （`new getFoo()()` = 构造 `getFoo()` 的结果，`new (Foo)()` 合法）；
                // 否则整个表达式是构造目标、实参为空（B-08 / C-04 / D-01）
                let expr = self.parse_postfix()?;
                let end = expr.span().end;
                let (target, type_args, args) = match expr {
                    Expression::Call {
                        callee,
                        type_args,
                        args,
                        ..
                    } => (*callee, type_args, args),
                    other => (other, vec![], vec![]),
                };
                Some(Expression::New {
                    target: Box::new(target),
                    type_args,
                    args,
                    span: Span {
                        start: keyword.span.start,
                        end,
                    },
                })
            }
            Some(TokenKind::Match) => self.parse_match(),
            Some(TokenKind::Async) => self.parse_lambda(),
            // `(s: string): string => s`、`(x, y: int) => x` 等 lambda 形态由
            // `parse_paren_or_lambda` 先按「参数列表 + `=>`」试探，失败再按括号 / 元组解析
            Some(TokenKind::OpenParen) => self.parse_paren_or_lambda(),
            Some(TokenKind::OpenBracket) => self.parse_array_literal(),
            Some(TokenKind::OpenBrace) => self.parse_object_literal(),
            Some(TokenKind::This) => {
                let token = self.advance()?;
                Some(Expression::This { span: token.span })
            }
            Some(TokenKind::Identifier) => {
                let token = self.advance()?;
                // `Point{ x: 1 }`：标识符后紧接对象字面量 → struct 字段初始化构造。
                // `match subject { ... }` 的主体后面同样紧跟 `{`，那里必须留给 match 块；
                // 无括号 `with` 表头同理——表头顶层的 `{` 是 with 块的起始（B-01），
                // 括号 / 实参内部（with_shield > 0）的 `{` 仍属于 StructInit。
                if self.check(TokenKind::OpenBrace)
                    && !self.in_match_subject
                    && !(self.with_stack > 0 && self.with_shield == 0)
                {
                    let (properties, span) = self.parse_object_body(token.span.start)?;
                    return Some(Expression::StructInit {
                        name: token.slice,
                        properties,
                        span,
                    });
                }
                // `x => ...`：标识符后紧接 `=>` 是单参数 lambda
                if self.check(TokenKind::Arrow) {
                    return self.parse_single_param_lambda(&token, token.span.start, false);
                }
                // `x: T => ...`：单参数 lambda 带返回注解（文法 `typeAnnotation?`，
                // 注解落 `Lambda.return_type`）。与三元 `a ? x : y` 的 `:` 歧义：
                // 先试探「`: T` 之后紧跟 `=>`」，不满足则回退按标识符处理（B-05）
                if self.check(TokenKind::Colon) {
                    let checkpoint = self.checkpoint();
                    let (_, return_type) = self.parse_return_type();
                    if return_type.is_some() && self.check(TokenKind::Arrow) {
                        return self.finish_single_param_lambda(
                            &token,
                            token.span.start,
                            false,
                            return_type,
                        );
                    }
                    self.rollback(checkpoint);
                }
                Some(Expression::Identifier {
                    name: token.slice,
                    span: token.span,
                })
            }
            _ => {
                // 记为「此处应为表达式」，交由上层决定是否继续
                self.error_here("此处应为表达式");
                None
            }
        }
    }

    /// `(` 开头的表达式：lambda / 括号 / 元组三选一。
    ///
    /// 判定顺序：
    /// 1. 先按「参数列表 + 可选返回注解 + `=>`」试探解析 lambda——覆盖
    ///    `(s: string): string => s`、`(x, y: int) => x`、`() => 1`、
    ///    `(a, b) => c` 全部 paren lambda 形态（B-03 / B-04）；
    /// 2. 试探失败则整体回滚（游标与诊断），按括号 / 元组解析。
    fn parse_paren_or_lambda(&mut self) -> Option<Expression> {
        let checkpoint = self.checkpoint();
        if let Some(lambda) = self.try_parse_paren_lambda() {
            return Some(lambda);
        }
        self.rollback(checkpoint);
        self.parse_paren_or_tuple()
    }

    /// 试探把 `( 参数列表 ) [: T] => body` 按 lambda 解析；不是 lambda 时
    /// 返回 `None`（游标 / 诊断由调用方回滚）。
    fn try_parse_paren_lambda(&mut self) -> Option<Expression> {
        let start = self.peek()?.span.start;
        let (params, params_span) = self.parse_typed_params()?;
        let (has_annotation, return_type) = self.parse_return_type();
        if has_annotation && return_type.is_none() {
            return None;
        }
        if !self.check(TokenKind::Arrow) {
            return None;
        }
        self.advance();
        let body = self.parse_lambda_body()?;
        let end = body.span().end;
        Some(Expression::Lambda(Lambda {
            is_async: false,
            params: LambdaParams::List {
                params,
                span: params_span,
            },
            return_type,
            body,
            span: Span { start, end },
        }))
    }

    /// `(` 开头且不是 lambda 的表达式：括号分组 / 元组（对应文法
    /// `tupleExpression` / `parenExpression`）。
    ///
    /// E-07：元组至少两个元素——`()` 与 `(a,)` 报诊断（仍产出 Tuple 节点
    /// 以便错误恢复），单元素无尾逗号 `(a)` 是括号分组。
    fn parse_paren_or_tuple(&mut self) -> Option<Expression> {
        let open = self.advance()?;
        let start = open.span.start;
        // 括号内部属于「嵌套括号」：with 表头内双层括号中的 `as`
        // 按 cast 处理，不参与 with 绑定判定（E-02 逃逸口）
        self.with_shield += 1;
        let inner = self.parse_comma_separated_exprs();
        self.with_shield -= 1;
        let (elements, trailing_comma, close_end) = inner?;
        let paren_span = Span {
            start,
            end: close_end,
        };
        match elements.len() {
            0 => {
                self.error_here("空元组 `()` 不是合法表达式（元组至少需要两个元素）");
                Some(Expression::Tuple {
                    elements,
                    span: paren_span,
                })
            }
            1 if trailing_comma => {
                self.error_here("单元素元组 `(a,)` 不受支持（元组至少需要两个元素）");
                Some(Expression::Tuple {
                    elements,
                    span: paren_span,
                })
            }
            1 => {
                let mut elements = elements;
                Some(Expression::Paren {
                    expr: Box::new(elements.pop().expect("len 已判定为 1")),
                    span: paren_span,
                })
            }
            _ => Some(Expression::Tuple {
                elements,
                span: paren_span,
            }),
        }
    }

    /// 解析 `( expr, ..., )` 的元素列表与闭合括号（with 屏蔽由调用方负责）。
    ///
    /// 返回 `(元素列表, 末元素后是否有逗号, 闭合括号结束位置)`。
    fn parse_comma_separated_exprs(&mut self) -> Option<(Vec<Expression>, bool, usize)> {
        let mut elements = vec![];
        let mut trailing_comma = false;
        if !self.check(TokenKind::CloseParen) {
            loop {
                elements.push(self.parse_expression()?);
                if self.eat(TokenKind::Comma).is_some() {
                    trailing_comma = true;
                    if self.check(TokenKind::CloseParen) {
                        break;
                    }
                    continue;
                }
                trailing_comma = false;
                break;
            }
        }
        let close = self.expect(TokenKind::CloseParen)?;
        Some((elements, trailing_comma, close.span.end))
    }

    /// 数组字面量 `[a, b,]`（对应文法 `arrayLiteral`）。
    fn parse_array_literal(&mut self) -> Option<Expression> {
        let open = self.advance()?;
        let start = open.span.start;
        let mut elements = vec![];
        if !self.check(TokenKind::CloseBracket) {
            loop {
                elements.push(self.parse_expression()?);
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                if self.check(TokenKind::CloseBracket) {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::CloseBracket)?;
        Some(Expression::ArrayLiteral {
            elements,
            span: Span {
                start,
                end: close.span.end,
            },
        })
    }

    /// 对象字面量 `{ a: 1, b }`（对应文法 `objectLiteral`）。
    fn parse_object_literal(&mut self) -> Option<Expression> {
        let start = self.peek()?.span.start;
        let (properties, span) = self.parse_object_body(start)?;
        Some(Expression::ObjectLiteral { properties, span })
    }

    /// 解析对象字面量主体（前置：游标位于 `{`）。
    fn parse_object_body(&mut self, start: usize) -> Option<(Vec<PropertyAssignment>, Span)> {
        self.expect(TokenKind::OpenBrace)?;
        let mut properties = vec![];
        if !self.check(TokenKind::CloseBrace) {
            loop {
                // 属性名在文法中是 `identifier`；字符串键（`{"a": 1}`）一并接受
                let name = match self.peek().map(|token| token.kind) {
                    Some(TokenKind::Identifier)
                    | Some(TokenKind::StringLiteral)
                    | Some(TokenKind::RawStringLiteral)
                    | Some(TokenKind::FormatStringLiteral)
                    | Some(TokenKind::ByteStringLiteral) => self.advance()?,
                    _ => {
                        self.expect(TokenKind::Identifier)?;
                        return None;
                    }
                };
                let name_text = if name.kind == TokenKind::Identifier {
                    name.slice.clone()
                } else {
                    crate::unquote(&name.slice)
                };
                let mut value = None;
                let mut end = name.span.end;
                if self.eat(TokenKind::Colon).is_some() {
                    let expr = self.parse_expression()?;
                    end = expr.span().end;
                    value = Some(expr);
                }
                properties.push(PropertyAssignment {
                    name: name_text,
                    value,
                    span: Span {
                        start: name.span.start,
                        end,
                    },
                });
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                if self.check(TokenKind::CloseBrace) {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::CloseBrace)?;
        Some((
            properties,
            Span {
                start,
                end: close.span.end,
            },
        ))
    }

    // ---------- lambda ----------

    /// 解析 lambda（对应文法 `lambdaExpression`）。
    ///
    /// 前置：游标位于 `async` 或 `(`，或已确认标识符后紧跟 `=>` 时的标识符。
    fn parse_lambda(&mut self) -> Option<Expression> {
        let start = self.peek()?.span.start;
        let is_async = self.eat(TokenKind::Async).is_some();
        // `async` 之后的单参数简写形态：`async x => ...` / `async x: T => ...`
        //（D-02：必须继承 `async` 标记与 `async` 关键字位置）
        if let Some(name_token) = self.peek().filter(|t| {
            t.kind == TokenKind::Identifier
                && (self.check_n(TokenKind::Arrow, 1) || self.check_n(TokenKind::Colon, 1))
        }) {
            let token = name_token.clone();
            self.advance();
            return self.parse_single_param_lambda(&token, start, is_async);
        }
        let params = self.parse_lambda_params()?;
        let (_, return_type) = self.parse_return_type();
        self.expect(TokenKind::Arrow)?;
        let body = self.parse_lambda_body()?;
        let end = body.span().end;
        Some(Expression::Lambda(Lambda {
            is_async,
            params,
            return_type,
            body,
            span: Span { start, end },
        }))
    }

    /// 单参数简写 lambda：`x => body` / `x: T => body`（前置：游标位于
    /// `=>` 或返回注解的 `:`）。
    ///
    /// `start` 是 lambda 整体的起点（含 `async` 关键字）；`is_async` 由调用方
    /// 传入（修复 async 单参数 lambda 丢失 `async` 标记的 D-02）。
    fn parse_single_param_lambda(
        &mut self,
        name_token: &Token,
        start: usize,
        is_async: bool,
    ) -> Option<Expression> {
        // 可选返回注解 `: T`（文法 `typeAnnotation?`，落 `Lambda.return_type`，B-05）
        let (_, return_type) = self.parse_return_type();
        self.finish_single_param_lambda(name_token, start, is_async, return_type)
    }

    /// 单参数简写 lambda 的收口：消费 `=>` 与 lambda 体并组装节点。
    ///
    /// 前置：参数名与返回注解均已解析，游标位于 `=>`。
    fn finish_single_param_lambda(
        &mut self,
        name_token: &Token,
        start: usize,
        is_async: bool,
        return_type: Option<TypeExpr>,
    ) -> Option<Expression> {
        self.expect(TokenKind::Arrow)?;
        let body = self.parse_lambda_body()?;
        let end = body.span().end;
        Some(Expression::Lambda(Lambda {
            is_async,
            params: LambdaParams::Single {
                name: name_token.slice.clone(),
                span: name_token.span,
            },
            return_type,
            body,
            span: Span { start, end },
        }))
    }

    /// 解析 lambda 参数（对应文法 `lambdaSignature`）：`(参数列表?)`。
    fn parse_lambda_params(&mut self) -> Option<LambdaParams> {
        let (params, span) = self.parse_typed_params()?;
        Some(LambdaParams::List { params, span })
    }

    /// 解析 lambda 体（对应文法 `block | expression`）。
    fn parse_lambda_body(&mut self) -> Option<LambdaBody> {
        if self.check(TokenKind::OpenBrace) {
            let block = self.parse_block()?;
            let span = block.span;
            return Some(LambdaBody::Block { block, span });
        }
        let expr = self.parse_expression()?;
        let span = expr.span();
        Some(LambdaBody::Expr {
            expr: Box::new(expr),
            span,
        })
    }

    // ---------- match ----------

    /// 解析 match 表达式（对应文法 `matchExpression`）。
    ///
    /// 前置：游标位于 `match`。
    pub(crate) fn parse_match(&mut self) -> Option<Expression> {
        let keyword = self.advance()?;
        let start = keyword.span.start;
        // 主体后紧跟的 `{` 是 match 块，不能交给 StructInit 当结构体字面量
        self.in_match_subject = true;
        let subject = self.parse_expression();
        self.in_match_subject = false;
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
            self.allow_arrow_after_type = true;
            let ty = self.parse_type_expr();
            self.allow_arrow_after_type = false;
            let ty = ty?;
            MatchPattern::Type {
                span: ty.span(),
                ty,
            }
        } else {
            let expr = self.parse_expression()?;
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

/// 把二元运算的两侧组装为 [`Expression::Binary`]。
fn binary(op: BinaryOp, left: Expression, right: Expression) -> Expression {
    let span = Span {
        start: left.span().start,
        end: right.span().end,
    };
    Expression::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
        span,
    }
}

/// token 种类是否可能出现在类型实参列表中（用于判定 `<` 是否为泛型起点）。
///
/// 须覆盖文法 `typeExpression` 能包含的全部 token 种类：`func (a: int) -> int`
/// 形态需要 `Func` 与 `Colon`（B-07）。
fn is_type_token(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Identifier
            | TokenKind::Null
            | TokenKind::Func
            | TokenKind::Colon
            | TokenKind::OpenParen
            | TokenKind::CloseParen
            | TokenKind::OpenBracket
            | TokenKind::CloseBracket
            | TokenKind::LessThan
            | TokenKind::MoreThan
            | TokenKind::Comma
            | TokenKind::Multiply
            | TokenKind::BitOr
            | TokenKind::ReturnArrow
    )
}

/// token 种类到赋值运算符的映射（非赋值 token 返回 `None`）。
fn assign_op(kind: TokenKind) -> Option<AssignOp> {
    Some(match kind {
        TokenKind::Assign => AssignOp::Assign,
        TokenKind::PlusAssign => AssignOp::PlusAssign,
        TokenKind::MinusAssign => AssignOp::MinusAssign,
        TokenKind::MultiplyAssign => AssignOp::MultiplyAssign,
        TokenKind::DivideAssign => AssignOp::DivideAssign,
        TokenKind::ModulusAssign => AssignOp::ModulusAssign,
        TokenKind::BitAndAssign => AssignOp::BitAndAssign,
        TokenKind::BitOrAssign => AssignOp::BitOrAssign,
        TokenKind::BitXorAssign => AssignOp::BitXorAssign,
        TokenKind::LeftShiftArithmeticAssign => AssignOp::LeftShiftAssign,
        TokenKind::RightShiftArithmeticAssign => AssignOp::RightShiftAssign,
        _ => return None,
    })
}

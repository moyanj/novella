//! 表达式解析：优先级阶梯与入口。
//!
//! 分层递归，每层一个函数，层序即文法中 ANTLR 左递归备选的优先级顺序。
//! 优先级（高 → 低，与文法备选顺序一致）：
//! 原子 → 成员 / 索引 / 调用 / new / 后缀自增 → 前缀一元 / await / `is`
//! → `* / %` → `+ -` → `as` → 移位 → 关系 → 相等 → `&` → `^` → `|`
//! → `&&` → `||` → `??` → 三元 → lambda → 赋值（右结合）。
//!
//! 原子与后缀（`primary` / `postfix` 子模块）负责最低层的构建，本文件只做
//! 逐层折叠；lambda 与 match 因自身含块与分支，独立成子模块。

use novella_ast::expr::{BinaryOp, Expression, UnaryOp};
use novella_core::{Span, token::TokenKind};

use crate::Parser;

use super::helper::{assign_op, binary};

impl Parser {
    // ---------- 优先级阶梯 ----------

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
        // 栈不足时切到新分段继续递归（见 `recursion` 模块）；分段预算耗尽返回 None
        self.guarded(|p| p.parse_assignment())?
    }

    /// 赋值层（对应文法 `AssignmentExpr`，右结合）。
    ///
    /// 右结合靠自递归实现，是递归入口之一：经 [`Parser::guarded`] 扩展分段栈（A-03）。
    fn parse_assignment(&mut self) -> Option<Expression> {
        self.guarded(|p| p.parse_assignment_chain())?
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
    /// then / else 分支都靠递归实现，是递归入口之一：经 [`Parser::guarded`] 扩展分段栈（A-02）。
    fn parse_ternary(&mut self) -> Option<Expression> {
        self.guarded(|p| p.parse_ternary_chain())?
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
    /// （[`Parser::looks_like_call`] 的 `is_call_angle` 扫描），不会被这一层看到。
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
    /// 右结合靠自递归实现，是递归入口之一：经 [`Parser::guarded`] 扩展分段栈（A-01）。
    fn parse_unary(&mut self) -> Option<Expression> {
        self.guarded(|p| p.parse_unary_inner())?
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
}

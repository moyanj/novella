//! lambda 表达式解析：单参数简写、形参列表、体（表达式 / 块）。

use novella_ast::{
    expr::{Expression, Lambda, LambdaBody, LambdaParams},
    types::TypeExpr,
};
use novella_core::{
    Span,
    token::{Token, TokenKind},
};

use crate::Parser;

impl Parser {
    // ---------- lambda ----------

    /// 解析 lambda（对应文法 `lambdaExpression`）。
    ///
    /// 前置：游标位于 `async` 或 `(`，或已确认标识符后紧跟 `=>` 时的标识符。
    pub(crate) fn parse_lambda(&mut self) -> Option<Expression> {
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
    pub(crate) fn parse_single_param_lambda(
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
    pub(crate) fn finish_single_param_lambda(
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
    pub(crate) fn parse_lambda_body(&mut self) -> Option<LambdaBody> {
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
}

//! 原子表达式：字面量、标识符、括号 / 元组、数组 / 对象字面量、块与各类关键�字打头形式。

use novella_ast::expr::{Expression, Lambda, LambdaParams, Literal, PropertyAssignment};
use novella_core::{Span, token::TokenKind};

use crate::Parser;

impl Parser {
    pub(crate) fn parse_primary(&mut self) -> Option<Expression> {
        self.guarded(|p| p.parse_primary_atom())?
    }

    /// [`Parser::parse_primary`] 的实体（分段栈扩展由外层包装负责）。
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
                let mut expr = self.parse_postfix()?;
                let end = expr.span().end;
                // `Expression` 实现了自定义 `Drop`（见 novella-ast 的 drop_guard），
                // 直接从枚举里移出字段会触发 E0509；改用占位值替换（`mem::replace`）
                // 换取内部字段的所有权，占位值随 `expr` 一起释放。
                let (target, type_args, args) = match &mut expr {
                    Expression::Call {
                        callee,
                        type_args,
                        args,
                        ..
                    } => {
                        let target = std::mem::replace(
                            callee.as_mut(),
                            Expression::This {
                                span: Span { start: end, end },
                            },
                        );
                        (target, std::mem::take(type_args), std::mem::take(args))
                    }
                    other => {
                        let target = std::mem::replace(
                            other,
                            Expression::This {
                                span: Span { start: end, end },
                            },
                        );
                        (target, vec![], vec![])
                    }
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
                let checkpoint = self.checkpoint();
                let type_args = if self.check(TokenKind::LessThan) {
                    self.parse_call_type_args()
                } else {
                    Some(vec![])
                };
                if let Some(type_args) = type_args
                    && self.check(TokenKind::OpenBrace)
                    && !self.in_match_subject
                    && !(self.with_stack > 0 && self.with_shield == 0)
                {
                    let (properties, span) = self.parse_object_body(token.span.start)?;
                    return Some(Expression::StructInit {
                        name: token.slice,
                        type_args,
                        properties,
                        span,
                    });
                }
                self.rollback(checkpoint);
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
}

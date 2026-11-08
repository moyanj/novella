//! 类型系统解析（对应文法 `typeExpression` 及其派生规则）。
//!
//! 归属 [`Parser`] 的 `impl` 块：类型表达式与表达式互相递归
//! （`statementBlock` 内的表达式 ↔ 函数类型的返回类型、lambda 的返回注解），
//! 拆成独立 `impl` 只为让文件按主题分段。
//!
//! 文法事实与取舍：
//! - `predefinedType` 只有 `null` 是独立 token，其余类型名（`int` / `uint8` / ...）
//!   在词法层都是普通标识符，因此这里一律产出 [`TypeExpr::Reference`]，
//!   由语义分析按名字归类到 [`PredefinedType`]；
//! - `arrowFunctionType`（`<T>(T) -> T`）与 `functionType`（`func (a: int) -> int`）
//!   合并为同一个 [`TypeExpr::Function`]；
//! - 联合类型折叠为扁平的 [`TypeExpr::Union`]，不保留二元嵌套；
//! - 类型位的箭头唯一是 `->`：`=>` 不是类型语法，遇到即报诊断并按 `->` 恢复
//!   （仅 lambda 返回注解 / match 类型模式等「类型之后紧跟外层 `=>`」的位置例外，
//!   由 [`Parser::allow_arrow_after_type`] 标记）。

use novella_ast::types::{FunctionType, Param, TypeExpr, TypeParameter};
use novella_core::{Span, token::TokenKind};

use crate::Parser;

impl Parser {
    /// 解析类型表达式（对应文法 `typeExpression`）。
    ///
    /// 递归入口之一：与 [`Parser::parse_expression`] 一样先登记嵌套深度、
    /// 超限即截断，保证恶意深层输入（如数千层嵌套括号类型）线性终止（审计 A-04/A-05）。
    pub(crate) fn parse_type_expr(&mut self) -> Option<TypeExpr> {
        let depth = self.enter_nesting();
        let result = if self.nesting_exceeded() {
            None
        } else {
            self.parse_type_union()
        };
        self.leave_nesting(depth);
        result
    }

    /// 解析联合类型（`typeExpression` 的顶层结构）。
    ///
    /// 解析到顶层 `|` 后折叠为扁平联合；`->` 之后的返回类型重新进入
    /// [`Parser::parse_type_expr`]，故箭头函数类型的结合方向由语法结构
    /// （`->` 右侧递归）自然保证。
    fn parse_type_union(&mut self) -> Option<TypeExpr> {
        let first = self.parse_type_arrow_or_primary()?;
        let mut types = vec![first];
        while self.eat(TokenKind::BitOr).is_some() {
            // `A | B`：右侧继续按箭头 / 主类型解析（联合不参与 `->` 的结合）
            match self.parse_type_arrow_or_primary() {
                Some(ty) => types.push(ty),
                None => break,
            }
        }
        if types.len() == 1 {
            return types.pop();
        }
        let start = types.first().map(|ty| ty.span().start).unwrap_or(0);
        let end = types.last().map(|ty| ty.span().end).unwrap_or(start);
        Some(TypeExpr::Union {
            types,
            span: Span { start, end },
        })
    }

    /// 解析一个「带返回箭头」的类型或主类型（`typeExpression` 的非 `|` 部分）。
    ///
    /// 箭头形态只认 `->`；`=>` 不是类型语法（E-03），在类型位遇到即报诊断
    /// 并按 `->` 恢复，除非当前位置允许类型后紧跟外层 `=>`（lambda 返回注解、
    /// match 类型模式）。
    fn parse_type_arrow_or_primary(&mut self) -> Option<TypeExpr> {
        // 形态 ①：泛型参数开头的箭头函数类型 `<T>(T) -> T`
        if self.check(TokenKind::LessThan) {
            let type_parameters = self.parse_type_parameters();
            let (params, params_span) = self.parse_function_type_params()?;
            self.expect_type_arrow()?;
            let return_type = self.parse_type_expr()?;
            let start = type_parameters
                .first()
                .map(|p| p.span.start)
                .unwrap_or(params_span.start);
            let end = return_type.span().end;
            return Some(TypeExpr::Function(FunctionType {
                type_parameters,
                params,
                return_type: Box::new(return_type),
                span: Span { start, end },
            }));
        }
        // 形态 ②：`func (a: int) -> int`
        if self.check(TokenKind::Func) {
            return self.parse_function_type();
        }
        // 形态 ③：括号开头。`(T, U) -> V` 是箭头函数类型，`(T, U)` 是元组类型，
        // `(T)` 是括号分组；分组 / 元组同样参与 `[]` / `*` 后缀（B-02）。
        if self.check(TokenKind::OpenParen) {
            let (elements, trailing_comma, paren_span) = self.parse_paren_type_list()?;
            if self.eat(TokenKind::ReturnArrow).is_some() {
                return self.finish_arrow_function_type(elements, paren_span);
            }
            // `=>` 不是类型语法：lambda 返回注解 / match 类型模式之外的类型位
            // 遇到即报诊断并按 `->` 恢复（E-03）
            if self.check(TokenKind::Arrow) && !self.allow_arrow_after_type {
                self.error_here("类型位不支持 `=>`，类型箭头应为 `->`");
                self.advance();
                return self.finish_arrow_function_type(elements, paren_span);
            }
            // 非函数形态：括号分组 / 元组（E-07：元组至少两个元素，
            // 单元素 `(T)` 视为分组，`()` / `(T,)` 报诊断）
            let base = match elements.len() {
                0 => {
                    self.error_here("空元组 `()` 不是合法类型（元组至少需要两个元素）");
                    TypeExpr::Tuple {
                        elements,
                        span: paren_span,
                    }
                }
                1 if !trailing_comma => {
                    // 括号分组：`(A | B)` —— 括号本身不进 AST（TypeExpr 无分组节点）
                    let mut elements = elements;
                    elements.pop().expect("len 已判定为 1")
                }
                1 => {
                    self.error_here("单元素元组 `(T,)` 不受支持（元组至少需要两个元素）");
                    TypeExpr::Tuple {
                        elements,
                        span: paren_span,
                    }
                }
                _ => TypeExpr::Tuple {
                    elements,
                    span: paren_span,
                },
            };
            return Some(self.parse_type_suffixes(base));
        }
        self.parse_primary_type()
    }

    /// 把 `(T, U)` 参数列表升格为箭头函数类型：解析 `->` 之后的返回类型并收口。
    ///
    /// 前置：参数列表已解析、箭头（`->` 或按 `->` 恢复的 `=>`）已消费。
    fn finish_arrow_function_type(
        &mut self,
        elements: Vec<TypeExpr>,
        paren_span: Span,
    ) -> Option<TypeExpr> {
        let params = elements
            .iter()
            .map(|ty| Param {
                name: String::new(),
                ty: Some(ty.clone()),
                default: None,
                span: ty.span(),
            })
            .collect();
        let return_type = self.parse_type_expr()?;
        let end = return_type.span().end;
        Some(TypeExpr::Function(FunctionType {
            type_parameters: vec![],
            params,
            return_type: Box::new(return_type),
            span: Span {
                start: paren_span.start,
                end,
            },
        }))
    }

    /// 消费类型位的返回箭头 `->`；遇到 `=>` 时报诊断并按 `->` 恢复（E-03）。
    fn expect_type_arrow(&mut self) -> Option<novella_core::token::Token> {
        if self.check(TokenKind::Arrow) {
            self.error_here("类型位不支持 `=>`，类型箭头应为 `->`");
            return self.advance();
        }
        self.expect(TokenKind::ReturnArrow)
    }

    /// 解析 `func (参数列表) -> 返回类型`（对应文法 `functionType`）。
    fn parse_function_type(&mut self) -> Option<TypeExpr> {
        let keyword = self.expect(TokenKind::Func)?;
        let start = keyword.span.start;
        let type_parameters = if self.check(TokenKind::LessThan) {
            self.parse_type_parameters()
        } else {
            vec![]
        };
        let (params, _) = self.parse_typed_params()?;
        self.expect_type_arrow()?;
        let return_type = self.parse_type_expr()?;
        let end = return_type.span().end;
        Some(TypeExpr::Function(FunctionType {
            type_parameters,
            params,
            return_type: Box::new(return_type),
            span: Span { start, end },
        }))
    }

    /// 解析箭头函数类型的参数列表：`(T, U)`，元素是类型而非具名参数。
    ///
    /// 若括号内出现 `名字: 类型` 形态（即文法 `parameterList`），转交
    /// [`Parser::parse_typed_params`]，让 `(a: int) -> int` 也能成立。
    fn parse_function_type_params(&mut self) -> Option<(Vec<Param>, Span)> {
        if self.looks_like_typed_params() {
            return self.parse_typed_params();
        }
        let (types, _, span) = self.parse_paren_type_list()?;
        let params = types
            .into_iter()
            .map(|ty| Param {
                name: String::new(),
                ty: Some(ty.clone()),
                default: None,
                span: ty.span(),
            })
            .collect();
        Some((params, span))
    }

    /// 前瞻判断 `(` 内是否为 `名字: 类型` 形式的具名参数列表。
    fn looks_like_typed_params(&self) -> bool {
        // `(` identifier `:` —— 只看前两个 token，足以区分 `(a: int)` 与 `(int, float)`
        self.check(TokenKind::OpenParen)
            && self.check_n(TokenKind::Identifier, 1)
            && self.check_n(TokenKind::Colon, 2)
    }

    /// 解析括号类型列表 `(T, U)`（文法 `'(' typeList? ')'`），
    /// 返回元素、是否有尾随逗号与整对括号的区间。
    fn parse_paren_type_list(&mut self) -> Option<(Vec<TypeExpr>, bool, Span)> {
        let open = self.expect(TokenKind::OpenParen)?;
        let start = open.span.start;
        let mut elements = vec![];
        let mut trailing_comma = false;
        if !self.check(TokenKind::CloseParen) {
            loop {
                elements.push(self.parse_type_expr()?);
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                // 记录尾随逗号 `(int,)`，供调用方区分单元素元组与括号分组（E-07）
                if self.check(TokenKind::CloseParen) {
                    trailing_comma = true;
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::CloseParen)?;
        Some((
            elements,
            trailing_comma,
            Span {
                start,
                end: close.span.end,
            },
        ))
    }

    /// 解析主类型（对应文法 `primaryType`）：`null` / 类型引用，
    /// 并处理 `[]` 与 `*` 后缀。后缀可叠加（`int*[]`、`int**`）。
    fn parse_primary_type(&mut self) -> Option<TypeExpr> {
        let ty = match self.peek().map(|token| token.kind) {
            Some(TokenKind::Null) => {
                let token = self.advance()?;
                TypeExpr::Predefined {
                    ty: novella_ast::types::PredefinedType::Null,
                    span: token.span,
                }
            }
            Some(TokenKind::Identifier) => self.parse_type_reference()?,
            _ => {
                self.error_here("此处应为类型表达式");
                return None;
            }
        };
        Some(self.parse_type_suffixes(ty))
    }

    /// 解析类型后缀 `[]`（数组）与 `*`（指针），可叠加（`int*[]`、`(A | B)*`）。
    ///
    /// 主类型与括号分组 / 元组共用此后缀循环（B-02）。
    fn parse_type_suffixes(&mut self, mut ty: TypeExpr) -> TypeExpr {
        loop {
            let start = ty.span().start;
            if self.check(TokenKind::OpenBracket) && self.check_n(TokenKind::CloseBracket, 1) {
                self.advance();
                let Some(close) = self.advance() else {
                    break;
                };
                ty = TypeExpr::Array {
                    element: Box::new(ty),
                    span: Span {
                        start,
                        end: close.span.end,
                    },
                };
                continue;
            }
            if let Some(star) = self.eat(TokenKind::Multiply) {
                ty = TypeExpr::Pointer {
                    element: Box::new(ty),
                    span: Span {
                        start,
                        end: star.span.end,
                    },
                };
                continue;
            }
            break;
        }
        ty
    }

    /// 解析类型引用 `Foo` / `Foo<T>`（对应文法 `typeReference`）。
    fn parse_type_reference(&mut self) -> Option<TypeExpr> {
        let name_token = self.expect(TokenKind::Identifier)?;
        let mut generic_args = vec![];
        let mut end = name_token.span.end;
        if self.eat(TokenKind::LessThan).is_some() {
            // 泛型实参之间用 `,` 分隔；`>>` 已由 angle 辅助函数按深度拆分
            loop {
                generic_args.push(self.parse_type_expr()?);
                if self.eat(TokenKind::Comma).is_some() {
                    continue;
                }
                break;
            }
            let close = self.expect_angle_close()?;
            end = close.span.end;
        }
        Some(TypeExpr::Reference {
            name: name_token.slice,
            generic_args,
            span: Span {
                start: name_token.span.start,
                end,
            },
        })
    }

    /// 解析类型参数列表 `<T, U: Bound>`（对应文法 `typeParameters`）。
    ///
    /// 前置：游标位于 `<`。
    pub(crate) fn parse_type_parameters(&mut self) -> Vec<TypeParameter> {
        let mut parameters = vec![];
        if self.eat(TokenKind::LessThan).is_none() {
            return parameters;
        }
        while let Some(name_token) = self.expect(TokenKind::Identifier) {
            let mut bound = None;
            let mut end = name_token.span.end;
            if self.eat(TokenKind::Colon).is_some()
                && let Some(ty) = self.parse_type_expr()
            {
                end = ty.span().end;
                bound = Some(ty);
            }
            parameters.push(TypeParameter {
                name: name_token.slice,
                bound,
                span: Span {
                    start: name_token.span.start,
                    end,
                },
            });
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        self.expect_angle_close();
        parameters
    }

    /// 解析具名参数列表 `(a: int, b: string = "x")`（对应文法 `parameterList`）。
    ///
    /// 类型可省略（`func foo(a)`）；默认值只允许出现在具名参数形态中。
    /// 返回参数列表与整对括号的区间。
    pub(crate) fn parse_typed_params(&mut self) -> Option<(Vec<Param>, Span)> {
        // with 表头内嵌套的参数列表属于「括号内部」：其中的 `as` 一律按 cast
        // 处理，不参与 with 绑定判定（E-02）
        self.with_shield += 1;
        let result = self.parse_typed_params_inner();
        self.with_shield -= 1;
        result
    }

    /// [`Parser::parse_typed_params`] 的实体（with 屏蔽由外层包装负责）。
    fn parse_typed_params_inner(&mut self) -> Option<(Vec<Param>, Span)> {
        let open = self.expect(TokenKind::OpenParen)?;
        let start = open.span.start;
        let mut params = vec![];
        if !self.check(TokenKind::CloseParen) {
            loop {
                let name_token = self.expect(TokenKind::Identifier)?;
                let mut ty = None;
                let mut default = None;
                let mut end = name_token.span.end;
                if self.eat(TokenKind::Colon).is_some() {
                    let parsed = self.parse_type_expr()?;
                    end = parsed.span().end;
                    ty = Some(parsed);
                }
                if self.eat(TokenKind::Assign).is_some() {
                    let expr = self.parse_expression()?;
                    end = expr.span().end;
                    default = Some(expr);
                }
                params.push(Param {
                    name: name_token.slice,
                    ty,
                    default,
                    span: Span {
                        start: name_token.span.start,
                        end,
                    },
                });
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                if self.check(TokenKind::CloseParen) {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::CloseParen)?;
        Some((
            params,
            Span {
                start,
                end: close.span.end,
            },
        ))
    }

    /// 解析可选返回类型注解：`-> T` 或 `: T`，两者等价地落到 `Option<TypeExpr>`。
    ///
    /// 返回 `(是否出现注解, 类型)`：出现注解但解析失败时第二项为 None，
    /// 由调用方决定如何报诊断。
    ///
    /// 注解的类型之后允许紧跟 `=>`（lambda 返回注解 `x: T => ...`、
    /// match 类型模式 `case : T => ...`）：这里的 `=>` 属于外层语法，
    /// 解析期间置 [`Parser::allow_arrow_after_type`] 抑制「类型位 `=>`」诊断。
    pub(crate) fn parse_return_type(&mut self) -> (bool, Option<TypeExpr>) {
        if self.eat(TokenKind::ReturnArrow).is_some() || self.eat(TokenKind::Colon).is_some() {
            self.allow_arrow_after_type = true;
            let ty = self.parse_type_expr();
            self.allow_arrow_after_type = false;
            return (true, ty);
        }
        (false, None)
    }
}

use novella_ast::program::{FunctionItem, Item, Program};
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
    fn bump(&mut self) -> Option<Token> {
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
        if self.check(kind) { self.bump() } else { None }
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

    // ---------- 顶层 item 分发 ----------

    /// 解析整个 token 流为程序（对应文法 `program`）。
    pub fn parse(&mut self) -> ParseResult {
        let mut items: Vec<Item> = vec![];
        while !self.at_eof() {
            let token = match self.bump() {
                Some(token) => token,
                None => break,
            };
            // 按 item 起始关键字分发；`decorator*` / `Pub?` 等前缀修饰
            // 由对应 parse_xxx 内部消化（`@` 独立分发为装饰器语句）。
            let item = match token.kind {
                TokenKind::From | TokenKind::Import => self.parse_import(),
                TokenKind::Type => self.parse_type_alias(),
                TokenKind::Trait => self.parse_trait(),
                TokenKind::Class => self.parse_class(),
                TokenKind::Func => self.parse_function(),
                TokenKind::Extern => self.parse_extern_function(),
                TokenKind::At => self.parse_decorator_statement(),
                _ => self.parse_statement(),
            };
            if let Some(item) = item {
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
    /// 前置：`trait` 关键字已被主循环消费。
    /// TODO(自己写)：`decorator* Trait identifier typeParameters? traitBody`
    /// → [`Item::Trait`]；注意文法允许声明前挂装饰器（`decorator*`）。
    fn parse_trait(&mut self) -> Option<Item> {
        // 骨架：待实现
        None
    }

    /// 解析类声明（对应文法 `classDeclaration`）。
    ///
    /// 前置：`class` 关键字已被主循环消费。
    /// TODO(自己写)：`decorator* Pub? Class identifier typeParameters? classHeritage? classBody`
    /// → [`Item::Class`]；注意 `Pub?` 修饰与 `decorator*` 前缀。
    fn parse_class(&mut self) -> Option<Item> {
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

    /// 解析装饰器语句（对应文法 `decoratorStatement`）。
    ///
    /// 前置：`@` 已被主循环消费。
    /// 两种形态：`decorator SemiColon`（独立装饰器语句，[`Item::Decorator`]）
    /// 与 `decorator block`（装饰块，[`Item::DecoratorBlock`]）。
    /// TODO(自己写)：`decorator: At identifier ('(' decoratorToken* ')')?`，
    /// 括号内为自由 token 流，原样保留到 `Decorator.tokens`。
    fn parse_decorator_statement(&mut self) -> Option<Item> {
        // 骨架：待实现
        None
    }

    /// 解析顶层语句（对应文法 `statement`，[`Item::Statement`]）。
    ///
    /// 主循环无法识别为 item 起始关键字的 token 均由此兜底。
    /// TODO(自己写)：按文法 `statement` 的 15 种形态（variable / return / break /
    /// continue / throw / yield / block / if / while / for / try / with / async /
    /// stream / expressionStatement）分发实现。
    fn parse_statement(&mut self) -> Option<Item> {
        // 骨架：待实现
        None
    }

    // ---------- 函数声明（最小可运行占位） ----------

    /// 解析函数声明（对应文法 `functionDeclaration`）。
    ///
    /// 前置：`func` 关键字已被主循环消费，游标指向函数名。
    /// 占位实现：仅读取函数名并构造骨架节点（签名与函数体待补充），
    /// 随后跳过声明剩余部分，避免占位阶段产生误导性错误。
    /// TODO(自己写)：按文法 `decorator* Pub? Func identifier typeParameters? '(' parameterList? ')'`
    /// `typeAnnotation? ReturnArrow? typeExpression? block` 补充完整解析。
    fn parse_function(&mut self) -> Option<Item> {
        let (name, span) = match self.peek() {
            Some(token) if token.kind == TokenKind::Identifier => (token.slice.clone(), token.span),
            _ => {
                self.error_here("`func` 后应有函数名");
                // 失败也恢复同步：跳过声明剩余部分，避免外层循环对后续 token 重复报错
                self.skip_to_function_end();
                return None;
            }
        };
        self.pos += 1;
        let item = Item::Function(FunctionItem {
            attributes: Vec::new(),
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

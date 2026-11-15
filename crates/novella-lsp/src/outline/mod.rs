//! AST 遍历：从语法树里抽取「可导航结构」（文档符号、折叠区间）。
//!
//! 只做**一次深度遍历**，把散落在各节点里的名字与区间收集成扁平的内部结构
//! （[`Outline`]），再由 [`super::features`] 转成 lsp-types 的形态。这样做的原因：
//! - `novella-ast` 没有 visitor 抽象，逐个节点写两遍遍历（符号一次、折叠一次）
//!   会重复且容易漏分支；
//! - LSP 类型不适合作为递归函数的中间产物（`DocumentSymbol` 需要 `deprecated`、
//!   `detail` 等一堆可选字段），先收敛成内部结构更易维护。
//!
//! 节点名字本身**不带 span**（`ClassItem.name` / `FunctionItem.name` 等都是纯
//! `String`），只有整个声明带 span。因此名字的源码区间由直接在声明区间内按
//! 「整词 + 前后无标识符字符」的方式查找（[`name::locate_name`]）。
//!
//! 分文件组织（同一次遍历的各阶段）：
//! - `name`：名字区间定位与签名摘要文本；
//! - `items`：顶层 item（import / trait / class / struct / 函数 / 装饰器）；
//! - `members`：类型成员（字段、方法）与类型表达式、类型形参、参数；
//! - `statements`：语句（块、控制流、变量声明、绑定模式、catch / with）；
//! - `expressions`：表达式遍历（lambda、match 等嵌套结构）；
//! - `types_text`：类型表达式 → 简短文本（供 `detail` 使用）。

mod expressions;
mod items;
mod members;
mod name;
mod statements;
mod types_text;

pub(crate) use types_text::type_expr_text;

use novella_ast::program::Program;
use novella_core::Span;

/// 符号种类（与 LSP `SymbolKind` 的映射在 [`crate::features`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlineKind {
    Function,
    Method,
    Class,
    Struct,
    Trait,
    TypeAlias,
    /// 类型参数（`<T>`）与函数参数
    TypeParameter,
    Parameter,
    Field,
    Variable,
    Import,
}

/// 一个可导航符号。
#[derive(Debug, Clone)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: OutlineKind,
    /// 整个声明的区间（跳转 / 选择范围用）
    pub span: Span,
    /// 名字本身的区间（在 `span` 内定位；定位失败时退化为 `span.start` 处的空区间）
    pub name_span: Span,
    /// 父符号在 [`Outline::symbols`] 中的下标，顶层为 `None`
    pub parent: Option<usize>,
    /// 附加说明（签名 / 类型），用于 `DocumentSymbol.detail`
    pub detail: Option<String>,
}

/// 可折叠区间及其种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldKind {
    /// 类 / struct / trait 体
    TypeBody,
    /// 函数体 / 方法体
    FunctionBody,
    /// 普通语句块、控制流块
    Block,
    /// 多行 `if` 链 / `for` 循环
    Conditional,
    /// 多行 `match` 分支
    Match,
    /// 多行 `try` / `catch` / `finally`
    TryBlock,
    /// 连续两行以上的行注释
    Comments,
    /// import 语句连续块
    Imports,
}

/// 一个折叠区间（行号 0-based，闭区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRegion {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: FoldKind,
}

/// 一次遍历的结果。
#[derive(Debug, Clone, Default)]
pub struct Outline {
    pub symbols: Vec<OutlineSymbol>,
    pub folds: Vec<FoldRegion>,
}

/// 遍历整份程序，产出符号树与折叠区间。
///
/// 需要源码文本才能精确定位名字（AST 不保存名字区间），
/// 行号换算由 [`crate::source::SemanticIndex`] 提供。
pub fn outline(program: &Program, source: &str, index: &crate::source::SemanticIndex) -> Outline {
    let mut walker = Walker {
        symbols: Vec::new(),
        folds: Vec::new(),
        source,
        index,
    };
    // import 连续块：一条 item 一条地扫，遇到非 import 就断开
    let mut comment_blanks: Vec<Span> = Vec::new();
    for item in &program.items {
        walker.item(item, None);
        if let novella_ast::program::Item::Import(import) = item {
            comment_blanks.push(import.span);
        }
    }
    walker.merge_imports(&comment_blanks);
    walker.collect_comment_folds();
    Outline {
        symbols: walker.symbols,
        folds: walker.folds,
    }
}

/// 遍历过程中的可变状态。
struct Walker<'a> {
    symbols: Vec<OutlineSymbol>,
    folds: Vec<FoldRegion>,
    source: &'a str,
    index: &'a crate::source::SemanticIndex,
}

impl Walker<'_> {
    /// 记录一个符号，返回它在 `symbols` 里的下标。
    pub(crate) fn push(
        &mut self,
        name: &str,
        kind: OutlineKind,
        span: Span,
        parent: Option<usize>,
        detail: Option<String>,
    ) -> usize {
        let name_span = locate_name(self.source, span, name);
        let index = self.symbols.len();
        self.symbols.push(OutlineSymbol {
            name: name.to_owned(),
            kind,
            span,
            name_span,
            parent,
            detail,
        });
        index
    }

    /// 记录折叠区间（单行区间不产生折叠；`end` 按闭区间折算到末字节所在行）。
    pub(crate) fn fold(&mut self, span: Span, kind: FoldKind) {
        let start_line = self.index.position(span.start).0;
        let end_line = self.index.position(span.end.saturating_sub(1)).0;
        if end_line > start_line {
            self.folds.push(FoldRegion {
                start_line,
                end_line,
                kind,
            });
        }
    }

    /// 连续的 import 语句合并成一个折叠区间。
    pub(crate) fn merge_imports(&mut self, spans: &[Span]) {
        if spans.len() < 2 {
            return;
        }
        let start_line = self.index.position(spans[0].start).0;
        let end_line = self
            .index
            .position(spans[spans.len() - 1].end.saturating_sub(1))
            .0;
        if end_line > start_line {
            self.folds.push(FoldRegion {
                start_line,
                end_line,
                kind: FoldKind::Imports,
            });
        }
    }

    /// 收集连续两行以上的行注释块。
    ///
    /// 只用词法结果（`LineComment` token）即可，不必回到 AST。
    pub(crate) fn collect_comment_folds(&mut self) {
        let mut run_start: Option<u32> = None;
        let mut run_end: u32 = 0;
        let flush = |run: &mut Option<u32>, end: u32, folds: &mut Vec<FoldRegion>| {
            if let Some(start) = run.take()
                && end > start
            {
                folds.push(FoldRegion {
                    start_line: start,
                    end_line: end,
                    kind: FoldKind::Comments,
                });
            }
        };
        let lexed = novella_lexer::lexer(self.source);
        for token in &lexed.tokens {
            if token.kind != novella_core::token::TokenKind::LineComment {
                continue;
            }
            let (line, _) = self.index.position(token.span.start);
            match run_start {
                Some(_) if line <= run_end + 1 => run_end = line,
                _ => {
                    flush(&mut run_start, run_end, &mut self.folds);
                    run_start = Some(line);
                    run_end = line;
                }
            }
        }
        flush(&mut run_start, run_end, &mut self.folds);
    }
}

use name::locate_name;

#[cfg(test)]
mod tests {
    use novella_ast::program::Program;

    use super::*;
    use novella_parser::{Parser, ParserInput};

    /// 解析一段源码（过滤行注释），返回 AST 与偏移索引。
    fn build(text: &str) -> (Program, crate::source::SemanticIndex) {
        let lexed = novella_lexer::lexer(text);
        let tokens = lexed.tokens;
        let mut parser = Parser::new(ParserInput {
            tokens,
            file: "TEST".to_string(),
        });
        let parsed = parser.parse();
        let index = crate::source::SemanticIndex::new(text);
        (parsed.program, index)
    }

    /// 类型 / 函数 / 字段应进入符号树，名字区间需精确落在声明处。
    #[test]
    fn outline_collects_declarations() {
        let source = concat!(
            "class Shape {\n",
            "    pub name: string;\n",
            "    pub func area(factor: int) -> int {\n",
            "        return factor;\n",
            "    }\n",
            "}\n",
            "func main() {\n",
            "    let count = 0;\n",
            "}\n",
        );
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);

        let names = outline
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"Shape"), "类应进符号树：{names:?}");
        assert!(names.contains(&"name"), "字段应进符号树：{names:?}");
        assert!(names.contains(&"area"), "方法应进符号树：{names:?}");
        assert!(names.contains(&"main"), "函数应进符号树：{names:?}");
        assert!(names.contains(&"count"), "局部变量应进符号树：{names:?}");

        // 名字区间必须落在源码里且与名字一致
        for symbol in &outline.symbols {
            let text = &source[symbol.name_span.start..symbol.name_span.end];
            assert_eq!(text, symbol.name, "名字区间应精确：{:?}", symbol);
        }
        // 方法挂在类下面
        let area = outline
            .symbols
            .iter()
            .find(|symbol| symbol.name == "area")
            .expect("存在 area");
        let parent = area.parent.map(|index| &outline.symbols[index]);
        assert_eq!(parent.map(|symbol| symbol.name.as_str()), Some("Shape"));
    }

    /// 多行类型体 / 函数体 / 语句块应产生折叠区间，且函数体区间被外层包裹。
    #[test]
    fn outline_folds_multiline_bodies() {
        let source = concat!(
            "class Shape {\n",
            "    func area() -> int {\n",
            "        if (true) {\n",
            "            return 1;\n",
            "        }\n",
            "        return 0;\n",
            "    }\n",
            "}\n",
        );
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);

        assert!(
            outline
                .folds
                .iter()
                .any(|fold| fold.kind == FoldKind::TypeBody),
            "应折叠类体"
        );
        assert!(
            outline
                .folds
                .iter()
                .any(|fold| fold.kind == FoldKind::FunctionBody),
            "应折叠方法体"
        );
        assert!(
            outline
                .folds
                .iter()
                .any(|fold| fold.kind == FoldKind::Conditional),
            "应折叠 if 链"
        );
        // 类体从第 1 行（0-based）延续到末行
        let type_body = outline
            .folds
            .iter()
            .find(|fold| fold.kind == FoldKind::TypeBody)
            .expect("类体折叠");
        assert_eq!(type_body.start_line, 0);
    }

    /// 单行结构不产生折叠，避免客户端出现无意义的折叠箭头。
    #[test]
    fn single_line_structure_is_not_folded() {
        let source = "func f() { return; }\n";
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);
        assert_eq!(outline.folds.len(), 0, "单行函数体不应折叠");
    }

    /// 连续行注释块合并为一个折叠区间。
    #[test]
    fn comment_block_is_folded() {
        let source = "// 第一行\n// 第二行\n// 第三行\nlet x = 1;\n";
        let (program, index) = build(source);
        let outline = outline(&program, source, &index);
        let comments = outline
            .folds
            .iter()
            .find(|fold| fold.kind == FoldKind::Comments)
            .expect("注释块折叠");
        assert_eq!((comments.start_line, comments.end_line), (0, 2));
    }
}

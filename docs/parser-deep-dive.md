# novella-parser 模块深度解析

> 本文是对 `crates/novella-parser` 的一次**逐行级别**的完整剖析，目标是让读者在阅读后对 parser 的工作流程、代码架构、设计取舍、上下游契约、错误恢复策略与边界处理达到「完全了解」的程度。
>
> - 适用代码基线：novella workspace（edition 2024），commit `cedd6b6` 之后的 parser 现状。
> - 所有行号均以本文撰写时的磁盘文件为准；文件若有改动，行号会漂移，但函数名与逻辑描述保持稳定。
> - 文中所有中文错误文本、常量数值、字段签名均逐字来自源码，可直接 grep 验证。
> - 本文**只描述现状**，不对代码做任何修改；如发现「注释与实现不一致」之处，会如实标出，但不擅自改动源码。

---

## 目录

1. [模块定位与职责边界](#1-模块定位与职责边界)
2. [文件结构与模块划分](#2-文件结构与模块划分)
3. [对外 API：输入、输出与核心状态](#3-对外-api输入输出与核心状态)
4. [整体工作流程总览](#4-整体工作流程总览)
5. [预处理阶段（preprocess）](#5-预处理阶段preprocess)
6. [递归防护机制（recursion）](#6-递归防护机制recursion)
7. [游标管理与前瞻/回滚](#7-游标管理与前瞻回滚)
8. [顶层 item 解析（lib.rs）](#8-顶层-item-解析librs)
9. [类型解析（types.rs）](#9-类型解析typesrs)
10. [语句解析（stmt.rs）](#10-语句解析stmtrs)
11. [表达式解析（expression/）](#11-表达式解析expression)
12. [歧义消解：上下文标记体系](#12-歧义消解上下文标记体系)
13. [错误恢复与诊断生成](#13-错误恢复与诊断生成)
14. [上下游契约](#14-上下游契约)
15. [回归测试矩阵](#15-回归测试矩阵)
16. [关键设计决策与权衡](#16-关键设计决策与权衡)
17. [注释与实现的不一致点](#17-注释与实现的不一致点)
18. [扩展指南：如何安全地改动 parser](#18-扩展指南如何安全地改动-parser)
19. [附录：符号速查表](#19-附录符号速查表)

---

## 1. 模块定位与职责边界

### 1.1 在编译管线中的位置

novella 是一门类 Rust/TypeScript 混合风格的实验性语言，其编译器采用经典的单向依赖链：

```
core ← lexer ← parser ← sema ← hir ← codegen ← cli
core ← lexer ← novella-lsp（独立二进制，不经 cli）
ast ──(构建期)── novella-derive（proc-macro）
```

`novella-parser` 处在**语法分析层**：它的输入是 `novella-lexer` 产出的 `Vec<Token>`，输出是一棵 AST（`Program`）+ 一组语法诊断（`Vec<Diagnostic>`）。它**不接触** logos（词法库细节）、**不做**语义检查、**不生成**中间代码。

依赖（`crates/novella-parser/Cargo.toml:1-13`）：

| 依赖 | 用途 |
|------|------|
| `novella-core` | 提供 `Token` / `TokenKind` / `Span` / `Diagnostic` / `LineIndex` 等共享基础类型 |
| `novella-ast` | 提供 AST 节点定义（`Program` / `Item` / `Statement` / `Expression` / `TypeExpr` 等） |
| `stacker` | 提供分段栈能力，防御深层递归输入打穿线程栈 |
| `novella-lexer`（dev-dependency） | 仅供集成测试把源码转成 token 流 |

### 1.2 职责清单

parser 承担且仅承担以下职责：

1. **token 预处理**：过滤注释 token、把 `from "m" import` 中的 `import` 降级为普通标识符（见第 5 章）。
2. **语法结构识别**：按文法产生式把 token 流组装成 AST。
3. **语法错误诊断**：在遇到不合法 token 时产出 `SyntaxError` 级别的 `Diagnostic`，并尽可能恢复继续解析。
4. **上下文敏感消歧**：通过一组计数器/布尔标记（见第 12 章）解决 `as`、`<...>`、`{`、`=>` 等 token 在不同语法位置的含义冲突。
5. **递归深度防护**：所有递归入口统一走 `guarded`，深层合法输入可正常解析，病态输入有明确上限诊断。

parser **不做**的事：

- 不做词法分析（那是 lexer 的事；下游不得重跑 lexer）。
- 不做类型检查、名称解析、作用域分析（那是 sema 的事）。
- 不做字面量求值（AST 字面量保留源码原文，含 `f/r/b` 前缀与进制前缀，解析延后到 sema）。
- 不二次过滤注释（只有 `preprocess` 一处；`Module.tokens` 保留完整 token 流）。

### 1.3 与 ANTLR 文法的关系

项目中有 ANTLR4 文法文件 `grammar/NovellaLexer.g4` 与 `grammar/NovellaParser.g4`，但**实际的 parser 是手写的递归下降解析器**。二者的关系是：

- 文法文件是「以代码为基准对齐」的产物，用于文档化与工具生态（如编辑器语法生成），不是 parser 的生成源。
- AST 节点的 doc 注释标注「对应文法 xxx」产生式名；改动代码后必须同步 `.g4`。
- 当代码与文法不一致时，**以代码为准**，不得凭文法去要求代码。

这一点在 `docs/AGENTS.md` 与 `grammar/README.md` 中均有明确约定。

---

## 2. 文件结构与模块划分

### 2.1 文件清单

```
crates/novella-parser/
├── Cargo.toml                       # 依赖声明（13 行）
├── src/
│   ├── lib.rs                       # 核心：Parser 结构、顶层 item 解析、公共工具（1112 行）
│   ├── preprocess.rs                # token 预处理（46 行）
│   ├── recursion.rs                 # 递归防护（87 行）
│   ├── stmt.rs                      # 语句解析（727 行）
│   ├── types.rs                     # 类型解析（456 行）
│   └── expression/
│       ├── mod.rs                   # 子模块声明（17 行）
│       ├── helper.rs                # 通用工具（62 行）
│       ├── stairs.rs                # 运算符优先级阶梯（371 行）
│       ├── postfix.rs               # 后缀解析（284 行）
│       ├── primary.rs               # 原子表达式与字面量（376 行）
│       ├── lambda.rs                # lambda 表达式（108 行）
│       └── match_expr.rs            # match 表达式（172 行）
└── tests/
    └── boundary_regression.rs       # 边界与回归测试（550 行）
```

### 2.2 划分逻辑

整个 crate 采用「**一个 `Parser` 结构体 + 多个 `impl Parser` 分文件**」的组织方式。所有解析函数都是 `Parser` 的方法，通过 `impl` 块分散在不同文件中：

- `lib.rs`：`Parser` 的定义、构造、游标原语、诊断原语、顶层 item 解析、公共字符串/token 工具函数。这是唯一持有 `pub struct Parser` 的文件，也是 `parse()` 入口所在。
- `stmt.rs`：语句族解析（`parse_block` / `parse_statement` 及其分支）。
- `types.rs`：类型表达式解析。
- `expression/` 子目录：表达式解析，按职责进一步拆成六个文件。子模块间通过 `pub(crate)` 方法互相调用（因为跨文件，`private` 不可见）。

这种划分遵循项目的「大文件按职责拆目录」约定：单文件超过约 600 行就拆成 `foo/mod.rs` + 子模块。`expression/mod.rs` 即为此而生，它只做子模块声明与文档注释。

### 2.3 调用关系概览

```
parse()  [lib.rs:249]
 ├─ parse_item()  [lib.rs:309]
 │   ├─ parse_directives() / parse_visibility()  [lib.rs]
 │   ├─ parse_import() / parse_type_alias() / parse_trait() / parse_class()
 │   │  parse_extern_function() / parse_function() / parse_struct()  [lib.rs]
 │   │   └─ parse_member_block() → parse_member()  [lib.rs]
 │   │       └─ parse_type_expr()  [types.rs]
 │   └─ parse_statement()  [stmt.rs]  ← 顶层兜底（顶层 let/const/表达式语句）
 ├─ recover_after_item()  [lib.rs:273]
 └─ （循环直至 EOF，组装 Program）

表达式侧（被语句/类型/成员解析调用）：
parse_expression()  [stairs.rs:23]
 └─ parse_assignment → ... → parse_unary → parse_postfix → parse_primary
     ├─ primary 分支：new / match / lambda / 元组 / 数组 / 对象 / 标识符 ...
     └─ postfix 分支：成员 / 索引 / 调用 / ++ / --

递归防护：以上所有递归入口均通过 recursion.rs::guarded() 包装。
```

---

## 3. 对外 API：输入、输出与核心状态

### 3.1 输入结构 `ParserInput`

```rust
// crates/novella-parser/src/lib.rs:34-37
pub struct ParserInput {
    pub file: String,        // 文件名，用于诊断回显（location.file）
    pub tokens: Vec<Token>,  // lexer 产出的 token 流（可含 LineComment）
}
```

注意：parser 接受的是**完整 token 流**，包括 `LineComment`。注释的过滤在 `Parser::new` 内部的 `preprocess` 完成（见第 5 章）。因此调用方传入含注释的 token 流是正常且预期的。

`file` 字段是诊断定位的关键：它会被填入每一条 `Diagnostic` 的 `location.file`。LSP 场景传入 `"TEST"` 占位（`crates/novella-lsp/src/document.rs:60-70`），CLI 场景传入用户的原始路径。

### 3.2 输出结构 `ParseResult`

```rust
// crates/novella-parser/src/lib.rs:28-31
pub struct ParseResult {
    pub program: Program,            // 解析得到的 AST
    pub diagnostics: Vec<Diagnostic>,// 语法诊断列表（可为空）
}
```

`ParseResult` 不携带「是否失败」标志。判断语法是否成功，看 `diagnostics` 是否为空即可。sema 的 `ParsedSource` 另有一个 `syntax_failed` 布尔，由 loader 根据 `diagnostics` 是否非空计算得出（见第 14 章）。

> 值得强调：parser 的诊断是「尽力而为」的产物。即使有语法错误，parser 也会尽力产出一份 AST（可能不完整），供后续工具（如 LSP outline）使用。这与「遇错即停」的策略不同。

### 3.3 核心状态 `Parser`

```rust
// crates/novella-parser/src/lib.rs:48-75
pub struct Parser {
    tokens: Vec<Token>,                       // 预处理后的 token 流
    pos: usize,                               // 当前游标（指向 tokens[pos]）
    file: String,                             // 文件名（诊断用）

    diagnostics: Vec<Diagnostic>,             // 累积的诊断（parse() 末尾被 take 走）

    // —— 递归防护相关 ——
    stack_segments: usize,                    // 当前已新开的栈分段数
    stack_exhausted: bool,                    // 是否已报告过「超出工程上限」（只报一次）

    // —— 上下文消歧相关（详见第 12 章）——
    pub(crate) with_stack: u32,               // 处于 with 语句表头的嵌套深度
    with_shield: u32,                         // 「护盾」：>0 时屏蔽某些消歧规则
    allow_arrow_after_type: bool,             // 类型位是否允许后续出现 -> 作为返回箭头
    inside_match: u32,                        // 处于 match 分支体内的深度
    in_match_subject: bool,                   // 正在解析 match 主语（subject）
}
```

字段逐个说明：

| 字段 | 类型 | 作用 |
|------|------|------|
| `tokens` | `Vec<Token>` | 预处理后的 token 流。注意 `split_shift_at` 会**就地修改**它（把 `>>` 拆成两个 `>`）。 |
| `pos` | `usize` | 当前读取位置。所有 `peek`/`advance`/`expect` 都围绕它。解析过程中单调前进，但 `rollback` 会回退。 |
| `file` | `String` | 诊断回显文件名。 |
| `diagnostics` | `Vec<Diagnostic>` | 诊断累积器。`checkpoint`/`rollback` 会截断它，以撤销试探分支产生的诊断。 |
| `stack_segments` | `usize` | 递归分段计数，见第 6 章。 |
| `stack_exhausted` | `bool` | 「超出上限」诊断的一次性开关，避免每个递归入口都报同一条。 |
| `with_stack` | `u32` | with 表头深度。用来区分 `with x as y` 的 `as` 与 cast 的 `as`。 |
| `with_shield` | `u32` | 护盾计数。在解析参数列表、元组、实参等「嵌套在 with 表头内部但又不属于表头本身」的位置临时 `+1`，使消歧规则失效。 |
| `allow_arrow_after_type` | `bool` | 在 `case : T =>` 与 lambda 返回注解等场景临时开启，允许类型之后紧跟 `=>` 或 `->`。 |
| `inside_match` | `u32` | match 分支体深度。>0 时 `parse_expression` 在 `case`/`default`/`}` 处停止，防止吞掉后续分支。 |
| `in_match_subject` | `bool` | 正在解析 match 主语。开启时禁止标识符后的 `{` 被误判为 StructInit。 |

### 3.4 构造函数 `Parser::new`

```rust
// crates/novella-parser/src/lib.rs:78-92
pub fn new(input: ParserInput) -> Self {
    let tokens = preprocess(input.tokens);   // ① 预处理
    Self {
        tokens,
        pos: 0,
        file: input.file,
        diagnostics: Vec::new(),
        stack_segments: 0,
        stack_exhausted: false,
        with_stack: 0,
        with_shield: 0,
        allow_arrow_after_type: false,
        inside_match: 0,
        in_match_subject: false,
    }
}
```

构造函数本身极简：唯一有实际动作的是 `preprocess`，其余全是计数器归零。

> **注释与实现差异**：`lib.rs:44-46` 的结构体文档注释称「构造时含 `>>` 拆分」，但实际 `>>`/`<<` 的拆分发生在 `split_shift_at` 的**按需调用**中（见 7.4 节），并非在 `new` 里一次性完成。这是文档滞后，阅读时以实现为准。

---

## 4. 整体工作流程总览

### 4.1 一次完整解析的生命周期

```
[调用方] lexer(src) → Vec<Token> (+ errors)
        ↓
   Parser::new(ParserInput { file, tokens })
        ↓  (内部 preprocess: 降级 import + 过滤注释)
   parser.parse()
        ↓
   ┌─────────────────────────────────────┐
   │ while !at_eof():                    │
   │   before = pos                      │
   │   item = parse_item()               │  ← 可能产出 Some(Item) 或 None
   │   if let Some(item): items.push     │
   │   recover_after_item(before)        │  ← 若游标未前进，强制跳过当前行
   └─────────────────────────────────────┘
        ↓
   Program { items, span: 0..last_end }
   ParseResult { program, diagnostics: take(diagnostics) }
```

几个关键点：

1. **顶层循环以 EOF 为终止条件**，`at_eof()` 检查 `pos >= tokens.len() - 1`（EOF token 永远在末尾）。
2. **每层 item 解析后都调用 `recover_after_item`**，这是死循环防护：如果某个 `parse_item` 因为错误没有推进游标，`recover_after_item` 会强制跳过当前行的剩余 token，保证循环收敛。
3. **`Program.span` 取 `0..last_token.span.end`**，即整个文件的字节区间。
4. 诊断通过 `std::mem::take` 从 `Parser` 移出，`parse()` 之后 `Parser` 的诊断列表为空。

### 4.2 `parse()` 的实现

```rust
// crates/novella-parser/src/lib.rs:249-269
pub fn parse(&mut self) -> ParseResult {
    let mut items = Vec::new();
    while !self.at_eof() {
        let before = self.pos;
        if let Some(item) = self.parse_item() {
            items.push(item);
        }
        self.recover_after_item(before);
    }
    let end = self.tokens.last().map(|t| t.span.end).unwrap_or(0);
    ParseResult {
        program: Program {
            items,
            span: Span { start: 0, end },
        },
        diagnostics: std::mem::take(&mut self.diagnostics),
    }
}
```

### 4.3 `recover_after_item`：死循环防护

```rust
// crates/novella-parser/src/lib.rs:273-291（逻辑摘要）
fn recover_after_item(&mut self, before: usize) {
    if self.pos > before {
        return;                       // 正常推进，无需恢复
    }
    // 游标没动 → parse_item 因错误卡住 → 跳过当前行剩余 token
    let line = self.peek().map(|t| t.line);
    while !self.at_eof() {
        if let Some(cur_line) = self.peek().map(|t| t.line) {
            if Some(cur_line) != line {
                break;                // 换行即停，让下一行重新作为 item 起点
            }
        }
        self.advance();
    }
    // 末尾兜底：万一上面一次都没消费（例如 EOF 前的单行），至少推进一个
    if self.pos == before && !self.at_eof() {
        self.advance();
    }
}
```

设计意图：语法错误不应导致解析器在错误的 token 上无限打转。「跳到下一行」是一个简单但有效的恢复策略——novella 的顶层 item 通常独占行，换行是天然的同步点。

### 4.4 数据流总结

```
源码 &str
  │ lexer()
  ▼
Vec<Token>  (含 LineComment、EOF，可能含词法错误——但错误 token 不进流)
  │ Parser::new → preprocess
  ▼
Vec<Token>  (无 LineComment；from-import 的 import 已降级为 Identifier)
  │ parse()
  ▼
ParseResult { Program{items, span}, diagnostics }
  │ sema loader 收进 ParsedSource
  ▼
后续语义分析（单一出口，不重跑 parser）
```

---
## 5. 预处理阶段（preprocess）

### 5.1 入口与总体结构

```rust
// crates/novella-parser/src/preprocess.rs:19-25
pub fn preprocess(tokens: Vec<Token>) -> Vec<Token> {
    let tokens = downgrade_from_import(tokens);
    tokens
        .into_iter()
        .retain(|token| token.kind != TokenKind::LineComment)
        .collect()
}
```

预处理只做两件事：

1. **`downgrade_from_import`**：把 `from "..." import ...` 语句中的 `import` token 降级为 `Identifier`。
2. **过滤 `LineComment`**：注释 token 在此处被永久移除，parser 后续完全不感知注释。

### 5.2 为什么过滤注释只发生在这里

项目约定（见 `docs/AGENTS.md`）：

> 注释在语法层透明：`LineComment` 是正式 token（供高亮等消费），由 `novella-parser/src/preprocess.rs` 的 `retain` **一处**过滤；`Module.tokens` 保留完整流（含注释），sema/loader 不得再过滤一次。parser 不感知注释。

这意味着：

- lexer 产出的 token 流**包含** `LineComment`（因为 LSP 语义高亮需要它）。
- sema 的 `Module.tokens` 保存**完整流**（含注释），供 JSON 输出与工具消费。
- 只有 parser 内部使用的流**不含**注释。
- 任何地方都不得二次过滤——否则会让依赖完整 token 流的功能（如高亮、诊断回显）出错。

### 5.3 `downgrade_from_import`：消解 from-import 的歧义

```rust
// crates/novella-parser/src/preprocess.rs:34-46（逻辑摘要）
fn downgrade_from_import(mut tokens: Vec<Token>) -> Vec<Token> {
    let mut i = 0;
    while i + 2 < tokens.len() {
        if tokens[i].kind == TokenKind::From
            && tokens[i + 1].kind == TokenKind::StringLiteral
            && tokens[i + 2].kind == TokenKind::Import
        {
            tokens[i + 2].kind = TokenKind::Identifier;
            // i += 3; 继续扫描
        }
        i += 1;
    }
    tokens
}
```

**问题背景**：`import` 在 novella 里既是关键字（直接导入 `import "x";`）又是 from-导入语法的一部分（`from "m" import { a, b };`）。在 from-导入中，紧跟在字符串字面量后的 `import` 实际上应被当作一个普通标识符处理，以便 `parse_import` 用统一的逻辑解析（把 `from ... import ...` 的导入列表当作表达式/名字序列）。

**处理方式**：预处理阶段做一次简单的三元组模式匹配 `From, StringLiteral, Import`，把第三个 token 的 `kind` 改为 `Identifier`。注意只改 `kind`，`slice` 保持 `"import"` 原文不变——这样 AST 里的名字文本仍是正确的。

**影响范围**：这个改写只对 parser 内部流生效；`Module.tokens` 里的原始 token 仍是 `Import`，因为 sema 保存的是 lexer 的原始产物，而非 parser 预处理后的流。

### 5.4 预处理的边界与限制

- `downgrade_from_import` 的匹配非常保守：必须严格是 `From → StringLiteral → Import` 三连。若中间隔着注释，会被 `retain` 之前的处理漏掉吗？不会——因为顺序是先 `downgrade_from_import`（此时注释还在），再过滤注释。novella 的注释是行注释（`//`），行注释后必然换行，`from` 与字符串不会同行隔注释，因此该保守匹配是安全的。
- `preprocess` 不检查 EOF 的完整性——它假设 lexer 已正确追加 EOF token。

---

## 6. 递归防护机制（recursion）

### 6.1 为什么需要分段栈

novella 的 AST 是深层 `Box` 递归链（`Expression` / `Statement` / `TypeExpr`）。递归下降解析器在解析深层嵌套输入（如数万层括号）时，会消耗大量线程栈空间。默认主线程栈通常只有几 MiB，几千层递归就可能溢出导致进程 abort。

项目通过两条防线应对：

1. **parser 侧**：所有递归入口走 `recursion.rs::guarded()`，用 `stacker::maybe_grow` 在栈逼近红区时切到堆上新分段。
2. **AST 侧**：`Expression` / `Statement` / `TypeExpr` 在 `novella-ast/src/drop_guard.rs` 实现自定义 `Drop`，避免编译器默认析构下钻打穿栈。

本文只讲 parser 侧；AST 侧详见 `docs/AGENTS.md` 的 NOTES 部分。

### 6.2 常量定义

```rust
// crates/novella-parser/src/recursion.rs
const STACK_SEGMENT: usize = 2 * 1024 * 1024;       // :24  每段 2 MiB
const RED_ZONE: usize = 512 * 1024;                 // :29  红区 512 KiB
pub(crate) const MAX_STACK_BYTES: usize = 1024 * 1024 * 1024; // :49  总预算 1 GiB
const MAX_STACK_SEGMENTS: usize = MAX_STACK_BYTES / STACK_SEGMENT; // :52  = 512 段
```

**重要提示**：`recursion.rs` 的文件头注释多处写「512 KiB 段 / 256 MiB 预算」，与上述**实际常量值（2 MiB 段 / 1 GiB 预算）不一致**。这是历史遗留的文档滞后。阅读与计算时以实现为准，不要根据注释推断数值。本文如实记录此不一致，但不改动源码。

### 6.3 `guarded` 的完整逻辑

```rust
// crates/novella-parser/src/recursion.rs:63-86
pub(crate) fn guarded<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> Option<T> {
    let entry = self.stack_segments;                              // ① 记录进入时的分段数
    let will_switch = stacker::remaining_stack()
        .map(|rem| rem < RED_ZONE)
        .unwrap_or(false);                                        // ② 是否即将切换分段

    if will_switch && entry + 1 >= MAX_STACK_SEGMENTS             // ③ 预算检查
        && !self.stack_exhausted
    {
        self.stack_exhausted = true;                              // ④ 一次性诊断
        self.error_here("语法嵌套层级超出工程上限");
        return None;
    }

    let result = stacker::maybe_grow(RED_ZONE, STACK_SEGMENT, || f(self)); // ⑤ 核心调用

    self.stack_segments = entry;                                  // ⑥ 退出恢复计数
    Some(result)
}
```

逐点说明：

**① `entry = self.stack_segments`**：进入递归前记录当前已用分段数。这是「预算」的基准。

**② `will_switch`**：通过 `stacker::remaining_stack()` 查询当前栈剩余空间。若小于红区（512 KiB），说明下一个 `maybe_grow` 很可能触发分段切换。`unwrap_or(false)` 处理查询失败的平台（保守地不切换）。

**③ 预算检查**：只有当「即将切换」且「`entry + 1 >= MAX_STACK_SEGMENTS`（即本次切换会达到或超过 512 段上限）」且「尚未报告过上限诊断」三者同时成立时，才拦截。这个设计的精妙之处在于：**预算按实际新开分段数计量**，而不是按递归调用次数。浅层的海量调用（如一个超长但扁平的表达式）不会误触上限。

**④ 一次性诊断**：`stack_exhausted = true` 确保「超出工程上限」只报一次。否则每个递归入口都会报同一条，产生诊断洪水。

**⑤ `stacker::maybe_grow(RED_ZONE, STACK_SEGMENT, ...)`**：参数顺序是**红区在前、段大小在后**。这是 `stacker` 库的 API 约定，容易记反，源码注释中特别强调。当剩余栈 < 红区时，`stacker` 会在堆上分配一个 `STACK_SEGMENT` 大小的新栈段并在其上执行闭包；否则直接在当前栈执行。

**⑥ 退出恢复**：无论闭包是否正常返回，`stack_segments` 都恢复到 `entry`。这配合 `checkpoint`/`rollback` 使用——推测分支若在深处消耗了分段，回滚时必须把计数也回滚（见 7.3 节）。

### 6.4 受保护的递归入口清单

项目约定：**所有递归入口都必须写成「守卫包装 + `_inner` 实体」两层**。包装层调用 `self.guarded(...)`，实体函数命名加 `_inner` 后缀。具体清单：

| 包装函数 | 位置 | `_inner` 实体 |
|----------|------|---------------|
| `parse_expression` | `expression/stairs.rs:23` | 直接调 `parse_assignment`（其自身递归由 `guarded` 间接覆盖） |
| `parse_assignment` | `expression/stairs.rs:40` | 右结合自递归（每层经 `parse_ternary` 间接 guarded） |
| `parse_unary` | `expression/stairs.rs:330` | `parse_unary_inner` (:335) |
| `parse_primary` | `expression/primary.rs:9` | `parse_primary_atom` |
| `parse_type_expr` | `types.rs:28` | `parse_type_union` |
| `parse_typed_params` | `types.rs:382` | `parse_typed_params_inner` (:392) |
| `parse_block` | `stmt.rs:25` | `parse_block_inner` (:33) |
| `parse_statement` | `stmt.rs:58` | `parse_statement_inner` (:63) |
| `parse_binding_pattern` | `stmt.rs:187` | `parse_binding_pattern_inner` (:192) |
| `parse_match` | `expression/match_expr.rs:18` | 主体函数即实体 |

> 注意：`parse_expression` 的 guarded 是整个表达式解析的顶层守卫；其下游的 `parse_assignment → parse_ternary → ... → parse_primary` 形成一条深的调用链，但只有最外层 `parse_expression` 与少数内部递归点（如 `parse_unary`、`parse_primary`）显式 guarded。这已经足够——因为 `stacker::maybe_grow` 的栈段切换是「整个链一起搬过去」的，一次 guarded 就能让整条下游链在充足栈空间中运行。

### 6.5 递归防护的实测验证

回归测试 `crates/novella-parser/tests/boundary_regression.rs` 的 A 类测试专门验证这套机制：

- `a_deep_nesting_is_parsed_not_rejected` (:78-130)：500 层 `!`、300 层三元、300 层赋值、300 层类型括号，逐层剥 AST 断言深度正确。
- `a_deep_nesting_survives_small_thread_stack` (:140-157)：在 **256 KiB 栈**的线程里解析 8000 层括号，零诊断。这直接证明分段栈生效——若没有 `stacker`，256 KiB 栈连几十层递归都撑不住。
- `a_excessive_depth_reports_upper_bound` (:161-178)：20000 层括号触发「超出工程上限」诊断，且只报一次。

---

## 7. 游标管理与前瞻/回滚

### 7.1 基本游标原语

以下方法定义在 `crates/novella-parser/src/lib.rs:96-157`：

| 方法 | 签名 | 作用 |
|------|------|------|
| `peek` | `fn peek(&self) -> Option<&Token>` | 返回 `tokens.get(pos)`，不移动游标 |
| `peek_n` | `fn peek_n(&self, n: usize) -> Option<&Token>` | 返回 `tokens.get(pos + n)` |
| `at_eof` | `fn at_eof(&self) -> bool` | `pos + 1 >= tokens.len()`（即当前 token 是 EOF） |
| `advance` | `fn advance(&mut self) -> Option<Token>` | 返回当前 token 的克隆并 `pos += 1` |
| `check` | `fn check(&self, kind: TokenKind) -> bool` | 当前 token 的 kind 是否等于 `kind` |
| `check_n` | `fn check_n(&self, n, kind) -> bool` | `peek_n(n)` 的 kind 是否匹配 |
| `eat` | `fn eat(&mut self, kind) -> bool` | 若 `check(kind)` 则 `advance()` 并返回 true，否则 false |
| `expect` | `fn expect(&mut self, kind) -> Option<Token>` | 若匹配则消费并返回；否则报错并返回 `None` |

`expect` 的错误文本格式：

```
此处应为 `{kind}`，实际遇到 {found}
```

其中 `{kind}` 用 `Display` 输出（对 `TokenKind` 是 Debug 名，如 `Identifier`、`Semicolon`），`{found}` 由 `describe_token` 生成（见 8.12 节）。

### 7.2 `expect` 的错误定位

`expect` 失败时不移动游标，直接在当前 token 处记一条诊断。调用方拿到 `None` 后通常用 `?` 提前返回，表示该解析分支失败。顶层由 `recover_after_item` 兜底推进游标，避免死循环。

### 7.3 `checkpoint` / `rollback`：试探性解析的基石

```rust
// crates/novella-parser/src/lib.rs:166-181
pub(crate) fn checkpoint(&self) -> (usize, usize, usize, bool) {
    (
        self.pos,                  // 游标位置
        self.diagnostics.len(),    // 诊断数量
        self.stack_segments,       // 递归分段计数
        self.stack_exhausted,      // 「上限已报」标志
    )
}

pub(crate) fn rollback(&mut self, checkpoint: (usize, usize, usize, bool)) {
    self.pos = checkpoint.0;
    self.diagnostics.truncate(checkpoint.1);
    self.stack_segments = checkpoint.2;
    self.stack_exhausted = checkpoint.3;
}
```

**为什么必须保存四个值而非仅仅 `pos`**：

- **`diagnostics.len()`**：试探分支（如尝试解析 lambda、泛型调用实参）在失败前可能已经记录了诊断。回滚时必须截断这些诊断，否则试探失败的「假错误」会泄漏到最终结果里。
- **`stack_segments`**：试探分支若在深处触发了栈分段切换，消耗了分段预算。回滚时必须恢复计数，否则真正路径上的解析会「少算」可用预算，可能在合法深度下误报上限。
- **`stack_exhausted`**：试探分支可能触发了唯一一次「超出工程上限」诊断并置位。回滚时必须恢复标志，否则真正路径上即使真的超限也无法再报（诊断被吞）。

源码注释（`lib.rs:163-165`）明确写道：

> 必须包含 `stack_segments` / `stack_exhausted`：推测分支若在深处耗尽了分段预算或消耗了唯一一次「超出工程上限」诊断，回滚时不恢复这些状态，真正路径上的解析会在**不报诊断**的情况下静默截断。

### 7.4 `split_shift_at`：尖括号的按需拆分

这是 parser 里一个精巧的设计。考虑泛型嵌套：

```novella
Vec<Vec<i32>>
```

词法器会把它切成 `Vec < Vec < i32 >>`，其中 `>>` 是一个 `RightShiftArithmetic` token。但语法上我们需要两个独立的 `>` 来闭合两层泛型。

`split_shift_at` 在需要闭合尖括号时，**就地**把复合移位 token 拆成两个单字符 token：

```rust
// crates/novella-parser/src/lib.rs:188-201
pub(crate) fn split_shift_at(&mut self, pos: usize) -> bool {
    let Some(token) = self.tokens.get(pos).cloned() else { return false; };
    let (open, close) = match token.kind {
        TokenKind::RightShiftArithmetic => (TokenKind::MoreThan, TokenKind::MoreThan),
        TokenKind::LeftShiftArithmetic  => (TokenKind::LessThan, TokenKind::LessThan),
        _ => return false,
    };
    let (first, second) = split_token_pair(&token, open, close);
    self.tokens[pos] = first;
    self.tokens.insert(pos + 1, second);
    true
}
```

`expect_angle_close`（`lib.rs:204-207`）在期待 `>` 时先调用 `split_shift_at(self.pos)`，再 `expect(MoreThan)`。这样 `>>` 被拆成 `> >`，闭合一层后游标指向第二个 `>`，正好闭合外层。

`split_token_pair`（`lib.rs:1049-1081`）负责物理拆分：对 `slice` 按字符切半（`>>` → `>` + `>`），两个新 token 各持有半个 span，第二个 token 的 `column` 平移相应字符数。都是单字节 ASCII 运算符，字符切半与字节切半一致。

**为何是「按需」而非「构造时全局拆分」**：如果全局把所有 `>>`/`<<` 拆成两个 `>`/`<`，那么表达式中的右移运算 `a >> b` 会被错误拆开。按需拆分只在「期待尖括号闭合」的上下文触发，完美区分了「移位运算符」与「泛型闭合」两种语义。这也正是 3.4 节提到的「注释说构造时拆分、实现却是按需」的原因——实现后来演进为更精确的按需策略，注释未同步。

### 7.5 `error_here` 与 `error_at_span`

```rust
// lib.rs:212-223
fn error_here(&mut self, message: impl Into<String>) {
    let mut diag = Diagnostic::error(message).with_code(DiagnosticCode::SyntaxError);
    if let Some(token) = self.peek() {
        diag = diag.with_location(DiagnosticLocation {
            file: Some(self.file.clone()),
            line: token.line,
            column: token.column,
            span: Some(token.span),
        });
    }
    self.diagnostics.push(diag);
}
```

`error_here` 在当前 token 处记诊断（不移动游标）。若流已结束（`peek()` 为 `None`，理论上不会发生因为 EOF 永远存在），诊断无位置。

`error_at_span`（`lib.rs:227-244`）用于「问题出在过去某个 token」的场景（如重复的 `default` 分支，错误应定位到第二个 `default` 而非当前游标）。它通过线性扫描 token 流，找到 `span.start` 匹配的 token 以取出其 line/column：

```rust
let (line, column) = self.tokens.iter()
    .find(|t| t.span.start == span.start)
    .map(|t| (t.line, t.column))
    .unwrap_or((0, 0));
```

若找不到（理论上不会），回退到 `(0, 0)`。两条路径都给诊断打上 `DiagnosticCode::SyntaxError` 标记，并填入 `file`、`line`、`column`、`span` 四元组，供 CLI 以 rustc 风格渲染。

---
## 8. 顶层 item 解析（lib.rs）

### 8.1 `parse_item`：顶层分发中枢

`parse_item`（`lib.rs:309-407`）是所有顶层声明的总入口。它按**固定顺序**执行三步：

```
① parse_directives()   → 收集 @directive 列表
② parse_visibility()   → 读取 pub/pri/pkg 可见性
③ 按当前 token kind 分发到具体解析器
```

**第一步：编译器指令**（`parse_directives`）。收集零个或多个 `@name(...)`。若编译器指令后面既不是 `;`、也不是 `{`、也不是声明关键字，报错：

```
编译器指令后应为 `;`、块 `{...}` 或声明（func / class / trait）
```

**第二步：可见性**（`parse_visibility`）。读取 `pub`/`pri`/`pkg` 三选一，映射为 `Scope::Public`/`Private`/`Package`。若可见性后面不是合法声明起始，报错：

```
`pub` / `pri` / `pkg` 后应为 class / struct / trait / func / extern / let / const 声明
```

**第三步：按 kind 分发**（核心 match）。分发表如下：

| token kind | 解析函数 | 产出 Item |
|------------|----------|-----------|
| `From` / `Import` | `parse_import` | `Item::Import` |
| `Type` | `parse_type_alias` | `Item::TypeAlias` |
| `Trait` | `parse_trait` | `Item::Trait` |
| `Class` | `parse_class` | `Item::Class` |
| `Struct` | `parse_struct` | `Item::Struct` |
| `Func` | `parse_function` | `Item::Function` |
| `Async`（后跟 `Func`） | `parse_function` | `Item::Function` |
| `Extern` | `parse_extern_function` | `Item::ExternFunction`（实际变体名以 AST 为准） |
| `Let` / `Const` | `parse_variable_statement` | `Item::Statement(Box<Statement>)` |
| 其他 | `parse_statement` | `Item::Statement(Box<Statement>)` |

**`async func` 的特殊处理**：`Async` 后跟 `Func` 时，`parse_item` 把当前 token 视为 `Func`（逻辑上归并），由 `parse_function` 统一处理并保留 `is_async` 标志。item 的 `span.start` 应指向 `async` 的起点（回归测试 `d_07_d_09_item_span_starts_at_modifiers` 验证 `async func` 的 span.start = 2，即跳过前导空白后的 `async` 位置）。

**起始 token 的消费责任**：各分支自行消费其起始 token（如 `parse_import` 消费 `from`/`import`，`parse_function` 消费 `func`/`async func`）。`parse_item` 本身不预先消费。

**顶层 let/const 的包装**：顶层 `let`/`const` 声明不走 `Item::Variable`（AST 的 `Item` 没有这个变体），而是包装为 `Item::Statement(Box<Statement>)`，复用语句解析器。这是 AST 设计的刻意选择——顶层变量声明与语句级变量声明共享同一套 AST 节点。

### 8.2 `parse_import`：导入语句

`parse_import`（`lib.rs:432-518`）处理两种形态：

```
import "path";                          // 直接导入
from "module" import { a, b };          // 从模块导入（import 已被 preprocess 降级为 Identifier）
from "module" import alias:item;        // 带别名
```

关键细节：

1. **路径前缀**：支持 `别名:路径` 形式，如 `import "std:exception";`。`parse_import` 把路径拆成 `prefix = Some("std")` 与 `path = "exception"`（回归测试 `d_08_import_prefix` 验证 `"std:exception"` → `prefix=std, path=exception`）。
2. **去引号**：路径字符串经 `unquote`（见 8.12 节）去掉引号与 `f/r/b` 前缀。
3. **from-import 的降级依赖**：`from "m" import` 中的 `import` 已被 `preprocess` 改成 `Identifier`，因此 `parse_import` 在 from 分支里看到的是 `Identifier` 而非 `Import` token。这正是第 5 章预处理的目的。

### 8.3 `parse_type_alias`：类型别名

```rust
// lib.rs:525-540
type Name<T> = TypeExpr;
```

解析名称（Identifier）、可选类型参数（`<T, U: Bound>`，复用 `parse_type_parameters`）、`=`，然后 `parse_type_expr`。产出 `Item::TypeAlias`。

### 8.4 `parse_trait`：trait 声明

```rust
// lib.rs:549-566
trait Name<T> { member* }
```

解析名称、类型参数、`parse_member_block`。可见性与编译器指令由 `parse_item` 传入。产出 `Item::Trait`。

### 8.5 `parse_class`：class 声明

```rust
// lib.rs:575-594
class Name(...) { member* }        // 括号 heritage
class Name: Base { member* }       // 冒号 heritage
```

`parse_class_parent`（`lib.rs:653-676`）支持两种继承写法：

- `(...)` 括号形式：把括号内内容解析为 heritage/构造参数。
- `:` 冒号形式：解析一个类型和/或 trait 列表作为父类/实现的接口。

`parse_class` 名称、heritage、member block，产出 `Item::Class`。

### 8.6 `parse_member_block` / `parse_member`：类/trait/struct 成员

`parse_member_block`（`lib.rs:679-694`）解析 `{ ... }` 包围的成员列表。循环调用 `parse_member`，若某次解析未推进游标，调用 `recover_to_statement_boundary` 恢复。返回 `(Vec<Member>, end_span)`。

`parse_member`（`lib.rs:701-803`）统一处理：

- 编译器指令
- 可见性（pub/pri/pkg）
- `static` 修饰
- `weak` 修饰

然后按成员类型分发：字段 vs 方法。

**两条关键诊断**：

1. **E-05**（`lib.rs` 中 `parse_member`）：方法声明必须以 `func` 开头。省略形态（如 `constructor() {}` 或 `run() {}`）报错：

```
方法声明须以 `func` 开头（`constructor(...)` / `name(...)` 的省略形态不受支持）
```

2. **E-06**：`weak` 不能修饰方法：

```
`weak` 不能修饰方法（weak 仅用于字段 / 变量）
```

字段成员可携带 `weak`；方法成员不可。这是语法层的早筛，语义层（sema）还有更细的 weak 规则。

回归测试 `e_05_method_requires_func_keyword`（:517-525）与 `e_06_weak_method_rejected`（:529-534）分别验证这两条诊断。

### 8.7 `parse_directive` 系列

`parse_directive`（`lib.rs:813-856`）解析单个 `@name(...)`：

- `@name`：无参编译器指令。
- `@name(arg1, arg2)`：带括号参数。参数内容用表达式解析器解析。
- 括号未闭合报：

```
编译器指令括号未闭合
```

`parse_directives`（`lib.rs:863-873`）循环收集多个编译器指令，返回 `Vec<CompilerDirective>`。

`parse_directive_statement`（`lib.rs:882-912`）处理编译器指令作为独立声明的形态：

```
@foo;              // 指令语句
@foo { ... }       // 指令块
```

**只允许一个编译器指令**，否则报：

```
指令语句 / 指令块只允许一个编译器指令
```

编译器指令参数中的表达式在 `with_shield` 保护下解析（避免 `as` 被误判），这与表达式侧的护盾机制一致。

### 8.8 `parse_function`：函数声明

```rust
// lib.rs:926-982
[async] func Name<T>(params) [-> ReturnType] { body }
```

处理要点：

- `async` 前缀：若当前是 `Async`，消费并置 `is_async = true`，然后期待 `Func`。
- 名称、类型参数（`parse_type_parameters`）、参数列表（`parse_typed_params`，来自 `types.rs`）。
- 返回类型：`parse_return_type`（接受 `->` 或 `:`，来自 `types.rs`）；显式注解保存为 `Some`，缺省为 `None`，不在 parser 中合成 `void`。普通有体函数由 sema 推导返回类型；AST JSON 中缺省注解为 `null`。
- 函数体：必须是 `{ ... }` 块（通过 `parse_block`）。`extern` 函数没有体（见 8.9）。
- 失败路径：调用 `skip_to_function_end`（见 8.11）跳过到函数结尾，避免在错误处卡住。

产出 `Item::Function`，函数体的 `block` 类型为 `Option<Box<Statement>>`——`Box` 用于抑制 `Item` 枚举的尺寸差异（否则触发 clippy `large_enum_variant`）。

### 8.9 `parse_extern_function`：外部函数

```rust
// lib.rs:607-645
extern func Name(params) [-> ReturnType];
```

与 `parse_function` 的区别：

- `is_extern = true`
- `block = None`（无函数体）
- 缺省返回注解保留为 `None`，由 sema 将无体的外部函数返回类型定为 `Void`

外部函数用于声明 FFI 边界，由 sema/codegen 进一步处理。

### 8.10 `parse_struct`：struct 声明

```rust
// lib.rs:991-1010
struct Name<T> [heritage] { field* }
```

复用 `parse_member_block` 解析字段成员，保留 heritage（继承/泛型约束）。产出 `Item::Struct`。

回归测试 `d_06_struct_heritage_kept`（:456-462）验证 heritage 被正确保留。

### 8.11 `skip_to_function_end`：函数级错误恢复

```rust
// lib.rs:1015-1042（逻辑摘要）
fn skip_to_function_end(&mut self) {
    let mut brace_depth = 0usize;
    while !self.at_eof() {
        match self.peek().kind {
            OpenBrace  => { brace_depth += 1; self.advance(); }
            CloseBrace => {
                if brace_depth == 0 { self.advance(); return; }  // 函数体结束
                brace_depth -= 1; self.advance();
            }
            Semicolon if brace_depth == 0 => { self.advance(); return; } // extern 声明结束
            _ => { self.advance(); }
        }
    }
}
```

用 `brace_depth` 跟踪花括号嵌套，跳到 `;`（无体函数）或平衡块结束。这是函数解析失败时的「大步恢复」策略，比逐 token 跳过更精确。

### 8.12 公共工具函数

| 函数 | 位置 | 作用 |
|------|------|------|
| `split_token_pair` | `lib.rs:1049-1081` | 把一个 token 按字符切半，生成两个新 kind 的 token，第二个的 column 平移。供 `split_shift_at` 调用。 |
| `unquote` | `lib.rs:1086-1094` | 去掉字符串的字面量前缀（`f`/`r`/`b`）与引号，返回内容原文。 |
| `namespace_alias` | `lib.rs:1097-1103` | 取路径末段、去扩展名，作为默认命名空间别名。 |
| `describe_token` | `lib.rs:1106-1112` | 生成 `expect` 错误里的 `{found}` 文本：`Eof` → `"文件结尾"`、`Identifier` → `"标识符"`、其余 → `` `{Debug名}` ``。 |

`describe_token` 的特例处理让错误信息对最终用户更友好（"实际遇到 文件结尾" 比 "实际遇到 Eof" 更自然）。

---
## 9. 类型解析（types.rs）

### 9.1 类型表达式的语法层级

novella 的类型表达式支持丰富的形态：

```
T | U                          // 联合类型（| 扁平折叠）
(T, U) -> V                    // 函数类型
func(a: T) -> U                // func 关键字函数类型
<T>(T) -> T                    // 泛型函数类型
a.b.C<T>                       // 限定名 + 泛型实参
T[]                            // 数组类型
*T                             // 指针类型
Null                           // 空类型
()                             // 空元组（非法，报错）
(T,)                           // 单元素元组（非法，报错）
```

### 9.2 `parse_type_expr` 入口与联合折叠

```rust
// crates/novella-parser/src/types.rs:28-30
pub(crate) fn parse_type_expr(&mut self) -> Option<TypeExpr> {
    self.guarded(|this| this.parse_type_union())
}
```

`parse_type_union`（`:37-56`）解析第一个类型，然后循环 `|` 把后续类型加入列表，最后**扁平折叠**成一个 `TypeExpr::Union`（而非嵌套的二元联合）。这样 `A | B | C` 产生一个含三个成员的 Union 节点，AST 更扁平、更利于 sema 处理。

### 9.3 `parse_type_arrow_or_primary`：三种函数类型形态

这是类型解析的核心函数（`:63-130`），处理三种「箭头类型」：

1. **泛型函数类型**：`<T>(T) -> T`。以 `<` 开头，解析类型参数列表，再解析参数元组，期待 `->`，解析返回类型。
2. **`func` 关键字函数类型**：`func(a: T, b: U) -> V`。以 `Func` token 开头，调用 `parse_function_type`。
3. **元组/分组后的箭头类型**：`(T, U) -> V`。以 `(` 开头，解析括号内的类型列表，若后跟 `->` 则构成函数类型；若只有单个类型且无箭头，则去括号返回内部类型（`(T)` 等价于 `T`，不进 AST）。

**空元组与单元素元组错误**：

- `()` → `空元组 `()` 不是合法类型（元组至少需要两个元素）`
- `(T,)` → `单元素元组 `(T,)` 不受支持（元组至少需要两个元素）`

**`=>` 在类型位的错误处理**：类型箭头唯一合法写法是 `->`。若遇到 `=>` 且不在允许的例外场景（见下文），报：

```
类型位不支持 `=>`，类型箭头应为 `->`
```

然后按 `->` 恢复继续解析（消费 `=>` 当作 `->`），以最大限度保留 AST。`allow_arrow_after_type` 标志控制这个例外：在 lambda 返回注解（`x: T => ...`）与 match 类型模式（`case : T =>`）中临时置 `true`，此时 `=>` 被允许。

### 9.4 `expect_type_arrow`

```rust
// types.rs:163-169
fn expect_type_arrow(&mut self) {
    if self.check(TokenKind::Arrow) {
        self.advance();
    } else if self.check(TokenKind::FatArrow) && self.allow_arrow_after_type {
        self.advance();                      // 例外：允许 => 作为类型箭头
    } else if self.check(TokenKind::FatArrow) {
        self.error_here("类型位不支持 `=>`，类型箭头应为 `->`");
        self.advance();                      // 按 -> 恢复
    } else {
        self.expect(TokenKind::Arrow);       // 常规缺失箭头，走 expect 报错
    }
}
```

`allow_arrow_after_type` 的开关正是「类型位箭头歧义」的消解点。回归测试 `e_03_arrow_in_type_position_rejected`（:504-513）验证：

- `type F = (A) => B;` → 报 `类型位不支持 =>`
- `func f(x: func (a: i32) => i32) {}` → 报诊断
- `let g = (x: i32): i32 => x;` → 零诊断（lambda 返回注解是例外）

### 9.5 `parse_function_type` 与参数识别

`parse_function_type`（`:172-190`）解析 `func(...) -> T` 形态。参数列表由 `parse_function_type_params` 处理。

`looks_like_typed_params`（`:196-219`）通过简单的前瞻判断括号内是否是「具名参数」：`(` 后紧跟 `Identifier` 再跟 `:` 则视为具名参数列表。否则视为普通类型列表（如 `(T, U)`）。这个前瞻避免了在括号类型上误用参数解析。

### 9.6 `parse_paren_type_list`

`parse_paren_type_list`（`:223-250`）解析 `(T, U, V)` 形式的类型列表，返回 `(Vec<TypeExpr>, has_trailing_comma)`。尾随逗号信息用于上层判断是否为单元素元组（`(T,)` 要报错）。

### 9.7 `parse_primary_type` 与后缀

`parse_primary_type`（`:254-270`）只接受两种 token：

- `Null` → `TypeExpr::Predefined(Null)` 或类似节点
- `Identifier` → 交给 `parse_type_reference`

其余 token 报：

```
此处应为类型表达式
```

**类型名一律 `Reference`**：项目约定「类型名一律 `Reference`，`Predefined` 留给 sema」。也就是说 parser 不区分 `i32` 这样的内建类型与用户定义类型——它们都产出 `Reference`。内建类型识别是 sema 的职责。

`parse_type_suffixes`（`:275-305`）在 primary 类型后叠加后缀：

- `[]` → `TypeExpr::Array`
- `*` → `TypeExpr::Pointer`

后缀可叠加（`T[][]*`）。

### 9.8 `parse_type_reference`：限定名与泛型

`parse_type_reference`（`:311-343`）解析形如 `a.b.C<T, U>` 的类型引用：

1. 消费首个 `Identifier`。
2. 循环 `.` + `Identifier`，构建 qualifier 链。
3. 若跟 `<`，解析泛型实参列表，用 `expect_angle_close`（见 7.4 节）闭合——这保证 `Vec<Vec<i32>>` 里的 `>>` 被正确拆分。

### 9.9 `parse_type_parameters` / `parse_typed_params`

`parse_type_parameters`（`:348-376`）解析 `<T, U: Bound>` 形式的泛型参数列表。每个参数可有 `: Bound` 约束（本身是一个 `TypeExpr`）。

`parse_typed_params`（`:382-389`）是「具名参数列表」（如函数参数 `(x: T, y: U)`）的 guarded 包装：

```rust
pub(crate) fn parse_typed_params(&mut self) -> Option<Vec<Param>> {
    self.guarded(|this| {
        this.with_shield += 1;                    // 进入护盾
        let result = this.parse_typed_params_inner();
        this.with_shield -= 1;                    // 退出护盾
        result
    })
}
```

**为什么参数列表要加护盾**：在 `with` 表头内部解析参数时，`as` 可能被误判为 with 绑定（见第 12 章）。护盾使 `with_stack > 0 && with_shield == 0` 的条件失效，从而让 `as` 正确解析为 cast 或其他含义。

`parse_typed_params_inner`（`:392-437`）是实体，解析每个参数的名称、类型注解、可选默认值（`Param.default`）。

### 9.10 `parse_return_type`

```rust
// types.rs:447-455
pub(crate) fn parse_return_type(&mut self) -> Option<TypeExpr> {
    if self.check(TokenKind::Arrow) || self.check(TokenKind::Colon) {
        self.advance();
        self.allow_arrow_after_type = true;        // 临时开启例外
        let ty = self.parse_type_expr();
        self.allow_arrow_after_type = false;       // 恢复
        ty
    } else {
        None
    }
}
```

返回类型既接受 `->`（函数声明风格）也接受 `:`（lambda 注解风格）。期间置位 `allow_arrow_after_type`，使嵌套的 `=>` 被正确对待。

> 注意：`parse_return_type` 用简单的 `true → parse → false` 而非保存/恢复旧值。由于该标志在正常流程中总是从 `false` 开始，这没问题；但若未来出现嵌套调用（返回类型中又含 lambda 返回注解），内层退出时会把标志提前清零。这是一个潜在的边界，阅读时留意。

---

## 10. 语句解析（stmt.rs）

### 10.1 语句族总览

`parse_statement_inner`（`:63-104`）的分发表：

| token kind | 解析函数 | 产出 Statement |
|------------|----------|----------------|
| `{` | `parse_block` | `Block` |
| `;` | — | `Empty` |
| `let` / `const` | `parse_variable_statement` | `Variable` / `VariableList` |
| `if` | `parse_if` | `If` |
| `while` | `parse_while` | `While` |
| `for` | `parse_for` | `For` |
| `try` | `parse_try` | `Try` |
| `with` | `parse_with` | `With` |
| `unsafe` | `parse_unsafe` | `Unsafe` |
| `match` | `parse_match_statement` | `Match`（表达式语句） |
| `return` | — | `Return` |
| `break` | — | `Break` |
| `continue` | — | `Continue` |
| `throw` | — | `Throw` |
| `yield` | — | `Yield` |
| 其他 | `parse_expression` + `expect_statement_end` | 表达式语句 |

**空语句产出 `Statement::Empty`**：语句位（块内、循环体等）的孤分号产出 `Empty` 节点，供 sema/工具链区分「空语句」与「语句缺失」（审计 C-02）。但**顶层** `;` 不产出节点（`parse_item` 的兜底分支会跳过）。

### 10.2 `parse_block`

```rust
// stmt.rs:25-30
pub(crate) fn parse_block(&mut self) -> Option<Box<Statement>> {
    self.guarded(|this| this.parse_block_inner())
}
```

`parse_block_inner`（`:33-55`）解析 `{`，然后循环 `parse_statement` 直到 `}`。每次调用后检查游标是否推进，未推进则 `recover_to_statement_boundary`。产出 `Statement::Block(Block { stmts, span })`。

### 10.3 变量声明与 `VariableList` 决策

`parse_variable_statement`（`:113-181`）处理：

```
let x: T = expr;                    // 单声明
let a = 1, b = 2;                   // 多声明 → Statement::VariableList
const N: i32 = 10;
```

**关键决策（审计 C-06）**：多绑定声明产出 `Statement::VariableList` 而非包一层 `Block`。原因：Block 会引入不存在的嵌套作用域，导致 sema 的作用域分析出错。多个声明属于**同一作用域**，按序求值，所以用扁平的 `VariableList` 承载。回归测试 `c_06_multi_binding_is_variable_list`（:410-416）验证产出含 2 个 declaration 的 `VariableList`。

`parse_binding_pattern`（`:187-217`）支持递归括号解构：

```
let (a, b) = pair;
let (a, (b, c)) = nested;
```

产出 `BindingPattern::Identifier` 或 `BindingPattern::Tuple`（递归）。它是 guarded 包装（`parse_binding_pattern_inner` 为实体），因为解构可任意深。

### 10.4 `if` / `while` / `for`

**`parse_if`**（`:222-285`）：

```
if (cond) stmt [else stmt]
```

`parse_parenthesized_condition` 解析括号条件。`parse_statement_body` 统一处理 then/else 分支——分支可以是块，也可以是单条语句（包括空语句）。

**`parse_while`**（`:288-300`）：`while (cond) stmt`。

**`parse_for`**（`:303-334`）支持两种形态：

```
for (init; cond; step) stmt        // 传统 for
for (x in iterable) stmt           // for-in
```

消歧靠 `looks_like_for_in`（`:341-357`）：扫描括号内是否有一个**同层**（depth == 0）的 `in` token。遇到顶层 `;` 或 `)` 即返回 false（说明是传统 for）；嵌套括号/方括号内的 `in` 被忽略（depth > 0）。回归测试 `c_03_for_in_requires_top_level_in`（:358-393）验证同层 `in` 才判 ForIn，嵌套 `in` 不误判。

`parse_for_loop_condition`（`:360-383`）解析 cond/step 部分。

`parse_for_init`（`:386-450`）解析 init 部分，既接受表达式也接受**裸声明**（bare declaration）：

```
for (i = 0; i < 10; i = i + 1)     // init 是表达式
for (let i = 0; ...)               // init 是声明
for (i: i32 = 0; ...)              // init 是 bare declaration（无 let/const）
```

`looks_like_bare_declaration`（`:457-475`）做前瞻：见到首个 `Identifier` 后若紧跟 `:` 或 `=` 判定为声明；若紧跟 `(`/`[`/`.`/`{` 判定为表达式。文法约定 for 循环 init 段声明不带 `const/let`，统一按 `Let` 承载（见 `ForInit` 注释）。回归测试 `b_06`（:263-276）验证 `for (arr[i] = 0; ...)` 的 init 是 `ForInit::Expression`。

### 10.5 `try` / `with` / `unsafe`

**`parse_try`**（`:480-518`）：

```
try { ... } catch (e) { ... } catch { ... } finally { ... }
```

支持多个 catch 子句与可选 finally。catch 参数可是 `BindingPattern`。

**`parse_with`**（`:525-563`）：

```
with expr [as binding] { body }
```

**表头无括号**是 novella 的文法规定。若写成 `with (sysCtx) { ... }`，报迁移诊断：

```
with 表头无需括号（文法为 `with expr as name { ... }`），括号会被当作表达式分组
```

（括号不是语法错误——它会被当作表达式分组正常解析，只是给出迁移提示。）解析 expr 期间置 `with_stack += 1`，这关系到 `as` 的消歧（见第 12 章）。绑定支持标识符与元组解构（回归测试 `c_01_with_tuple_binding`，:328-339）。回归测试 `b_01`（:184-200）验证无括号形态零诊断、括号形态报迁移诊断。

**`parse_unsafe`**（`:566-576`）：`unsafe { ... }`，块内是普通 `Statement` 序列，语法层不做白名单过滤（unsafe 规则由 sema 的 `unsafe_rules` 处理）。

### 10.6 `match` 语句与跳转语句

`parse_match_statement`（`:581-585`）直接复用 `parse_match`（表达式解析器的 match），产出 `Statement::Expression` 包裹 match 表达式。

跳转语句：

- `parse_return` / `parse_break` / `parse_continue`（`:590-659`）
- `parse_throw`：`throw expr;`，AST 中 `Throw.expr` 为 `Option<Expression>`（可裸 `throw`）
- `parse_yield`：`yield expr?;`

### 10.7 语句结束与恢复

`expect_statement_end`（`:662-673`）：语句必须以 `;` 结束，但**紧随 `}` 或 EOF 时容忍省略**。这是为了支持 `if (x) { ... }` 后不写分号的常见风格。

`recover_to_statement_boundary`（`:711-726`）：跳到下一个 `;`、`}` 或行首（line 变化处）。这是块内语句解析失败时的同步点。

`skip_balanced_block`（`:681-708`，标注 `#[allow(dead_code)]`）：跳过平衡的花括号块，找不到配对 `}` 时报：

```
此处缺少与之配对的 `}`
```

该方法当前未被调用（故 `allow(dead_code)`），是预留的恢复工具。

---
## 11. 表达式解析（expression/ 六个子模块）

表达式解析是 parser 中规模最大、机制最精细的部分，按职责拆在 `crates/novella-parser/src/expression/` 目录下六个文件。它们都是同一个 `impl Parser` 的分文件实现，跨文件调用通过 `pub(crate)` 可见性协调。

### 11.0 六个子模块分工

| 文件 | 职责 |
|------|------|
| `expression/mod.rs` | 模块声明与重导出（17 行） |
| `expression/helper.rs` | 表达式通用工具：`is_type_token` / `assign_op` / `binary` |
| `expression/stairs.rs` | 运算符优先级阶梯（赋值→三元→…→一元→后缀） |
| `expression/postfix.rs` | 后缀链：成员/索引/调用/自增自减，及泛型调用的歧义消解 |
| `expression/primary.rs` | 原子表达式：字面量/new/match/lambda/括号/数组/对象/标识符 |
| `expression/lambda.rs` | lambda 参数与函数体 |
| `expression/match_expr.rs` | match 表达式与 case 分支 |

### 11.1 `helper.rs`：通用工具

**`is_type_token`**（`helper.rs:12-30`）。判断一个 token 是否可能出现在「类型位」——这是泛型调用消歧的关键（见 11.4）。覆盖：

```
Identifier | Null | Func | Colon |
OpenParen | CloseParen | OpenBracket | CloseBracket |
LessThan | MoreThan | Comma | Multiply | BitOr | ReturnArrow
```

包含 `Colon`（具名参数）、`Multiply`（指针类型 `*T`）、`BitOr`（联合类型 `|`）、`ReturnArrow`（函数类型 `->`）——这些都是合法类型语法的组成部分。

**`assign_op`**（`:33-48`）。把赋值类 token 映射到 AST 的 `AssignOp` 枚举，共 11 种：

| token | AssignOp |
|-------|----------|
| `Assign` | `Assign` |
| `PlusAssign` | `AddAssign` |
| `MinusAssign` | `SubAssign` |
| `MultiplyAssign` | `MulAssign` |
| `DivideAssign` | `DivAssign` |
| `ModulusAssign` | `ModAssign` |
| `BitAndAssign` | `BitAndAssign` |
| `BitOrAssign` | `BitOrAssign` |
| `BitXorAssign` | `BitXorAssign` |
| `LeftShiftArithmeticAssign` | `LeftShiftAssign` |
| `RightShiftArithmeticAssign` | `RightShiftAssign` |

（具体枚举拼写以 AST 为准；这里记录的是映射关系与 11 种这个数量。）

**`binary`**（`:51-62`）。构造 `Expression::Binary { op, left, right, span }` 的辅助函数，`span = left.span.start .. right.span.end`。所有二元运算层级共用它。

### 11.2 `stairs.rs`：运算符优先级阶梯

这是表达式解析的骨架。`parse_expression`（`:23-35`）是总入口：

```rust
pub(crate) fn parse_expression(&mut self) -> Option<Expression> {
    // match 分支体内，遇到 case/default/} 时表达式自然结束
    if self.inside_match > 0
        && matches!(self.peek().kind, Case | Default | CloseBrace) {
        return None;
    }
    self.guarded(|this| this.parse_assignment())
}
```

`inside_match` 标志让表达式解析在 match 分支分隔符处停下——这是「match 分支体靠上下文标记自然终止」的实现（见第 12 章）。

**完整优先级链**（从低到高，即调用顺序——外层最低优先级，内层最高优先级）：

```
parse_assignment       (:40-62)   赋值，右结合（自递归 parse_assignment）
  └ parse_ternary      (:67-90)    ? : 三元，右结合
      └ parse_coalesce             ?? 空值合并
          └ parse_logical_or       || / keyOr
              └ parse_logical_and  && / keyAnd
                  └ parse_bit_or   |
                      └ parse_bit_xor  ^
                          └ parse_bit_and  &
                              └ parse_equality     == != === !==
                                  └ parse_relational   < > <= >=
                                      └ parse_shift   << >>
                                          └ parse_cast (:253-274)  as
                                              └ parse_additive    + -
                                                  └ parse_multiplicative  * / %
                                                      └ parse_is (:310-325)  is
                                                          └ parse_unary (:330-370)  + - !/ ~ & * await
                                                              └ parse_postfix
                                                                  └ parse_primary
```

**每一层的通用形态**：解析左侧操作数，然后 `while` 循环匹配当前层级的运算符 token，匹配则消费、解析右侧（更高优先级）操作数、用 `binary` 组装。除赋值与三元是右结合外，其余都是左结合。

**`parse_assignment`**（`:40-62`）特殊之处：左侧解析到 `parse_ternary`，然后若遇赋值 token，右侧**递归调用 `parse_assignment`** 实现右结合（`a = b = c` 解析为 `a = (b = c)`）。左侧值本身作为赋值目标表达式传给 sema 校验合法性。

**`parse_ternary`**（`:67-90`）：解析 `cond ? then : else`，`else` 分支递归调用 `parse_ternary` 实现右结合。

**`parse_cast`**（`:253-274`）——最需要留意的一层：

```rust
while self.check(TokenKind::As)
    && !(self.with_stack > 0
         && self.with_shield == 0
         && self.looks_like_with_binding())
{
    self.advance();
    let ty = self.parse_type_expr();     // as 后面是类型
    // 组装 Cast 表达式
}
```

这就是 `with expr as binding` 与 `expr as Type` 消歧的核心条件。三个条件同时满足时把 `as` 留给 with 绑定：

1. `with_stack > 0`——正在解析 with 表头；
2. `with_shield == 0`——不在护盾区（如括号、参数列表、实参列表内）；
3. `looks_like_with_binding()`——前瞻确认 `as` 后面像绑定而非类型。

**`looks_like_with_binding`**（`:103-123`）的前瞻逻辑：

- `as (` 后跟 `Identifier` 再跟 `,` 或 `)` → 元组绑定（如 `as (a, b)`）；
- `as` + `Identifier` 后跟 `)` / `:` / `{` → 单绑定（`as name)`、`as name: T`、`as name {`）。

只有这种「表头裸位绑定」形态才让 `as` 被 with 占用；嵌套括号或实参列表内的 `as` 仍是 cast。

**`parse_unary_inner`**（`:335-370`）：前缀一元运算，右结合。映射：

- `+` → `UnaryOp::Plus`
- `-` → `UnaryOp::Neg`
- `!` / `~` → `UnaryOp::Not`（或按 token 区分逻辑非/按位取反）
- `&` → `UnaryOp::Borrow`（借用）
- `*` → `UnaryOp::Deref`（解引用）
- `await` → `UnaryOp::Await`

### 11.3 `postfix.rs`：后缀链与调用歧义

`parse_postfix`（`:17-108`）在 primary 基础上循环累积后缀：

- `.` + `Identifier` → 成员访问 `Expression::Member`
- `[` expr `]` → 索引 `Expression::Index`
- `(` args `)` → 调用 `Expression::Call`
- `<` type args `>` `(` args `)` → 带显式泛型实参的调用
- `++` / `--` → 后置自增/自减

**调用的试探回滚机制**（`:52-77`）：判断 `(` 或 `<` 是否真的是调用后缀，用 checkpoint + 试解析 + 失败回滚。由于试解析会修改已构造的 `expr`，代码用 `expr.clone()` 保存快照，再用 `std::mem::replace` 放一个占位表达式进 `expr`，试解析成功后替换回来、失败则 rollback 并恢复快照。这套机制与 AST 的自定义 `Drop`（GuardedClone）兼容——深层表达式的 clone 也在分段栈保护下进行。

**`parse_call_suffix`**（`:114-145`）：解析一次调用后缀。先尝试解析泛型实参 `<...>`，若失败则回滚 `pos` 与 `diagnostics`（用 `checkpoint` 记录的长度），把 `<` 当作普通比较运算符留给外层解析。

**`looks_like_call`**（`:151-167`）：判断 `<` 是否开启泛型调用实参。

- 若当前是 `(` → 直接返回 true（普通调用）。
- 若当前是 `<` → 调用 `scan_call_angle` 扫描匹配的 `>`，然后检查：尖括号内部每个 token 都满足 `is_type_token`，且闭合 `>` 之后紧跟 `(`（否则不是调用，而是比较运算链式比较）。

**`scan_call_angle`**（`:173-231`）：扫描泛型尖括号的匹配闭合位置，处理嵌套 `<`/`>` 以及 `>>`/`<<` 移位 token 的拆分（与 `split_shift_at` 同源的按字符切半逻辑）。

**`parse_call_type_args`**（`:234-245`）：解析 `<T, U>` 形式的显式泛型实参列表。

**`parse_arguments`**（`:248-255`）：解析调用实参列表 `(a, b, c)`，期间 `with_shield += 1 / -= 1`，使实参内的 `as` 不被误判为 with 绑定。支持尾随逗号（`:258-274`）。

**`last_end`**（`:277-283`）：取上一个已消费 token 的 `span.end`，用于构造Call表达式的 span 终点。

### 11.4 `primary.rs`：原子表达式

`parse_primary`（`:9-11`）是 guarded 包装，实体 `parse_primary_atom`（`:17-172`）按 token kind 分发：

| token | 产出 |
|-------|------|
| `Null` | `Literal::Null` |
| `BooleanLiteral` | `Literal::Bool` |
| `StringLiteral` / `FormatString` / `RawString` / `ByteString` | `Literal::String`（保留 `f/r/b` 前缀原文，值语义延后到 sema） |
| `NumberLiteral` | `Literal::Number`（含 hex/oct/bin 前缀原文） |
| `New` | 构造表达式（见下） |
| `Match` | `parse_match` |
| `Async` | async lambda 或异步块 |
| `OpenParen` | `parse_paren_or_lambda`（见下） |
| `OpenBracket` | 数组字面量 |
| `OpenBrace` | 对象字面量 / 块 |
| `This` | `Expression::This` |
| `Identifier` | 标识符 / StructInit / 单参 lambda（见下） |

**`new` 分支**（`:57-103`）：解析 `new Target(...)`。因为 `Expression` 实现了自定义 `Drop`（见项目约定），Rust 禁止直接移出结构体字段（E0509），所以用 `std::mem::replace` 取字段值。逻辑：

- 先把 `new` 后面的后缀表达式整体解析出来。
- 若该后缀表达式是 `Call`，取其 `callee` 为构造 target，`type_args` 与 `args` 提升为构造实参。
- 否则整体作为 target，`args` 为空。

这解释了回归测试 `b_08_c_04_d_01_new_expression`（:287-322）的两个断言：

- `new getFoo()()` → target 是内层 `Call`（`getFoo()`），外层 `()` 是构造调用；
- `new Vec<i32>(1)` → 保留 `type_args = [i32]`、`args = [1]` 各一个。

**`Identifier` 分支**（`:115-165`）三种去向：

1. **StructInit**：标识符后跟 `{` 且满足 `!in_match_subject && !(with_stack > 0 && with_shield == 0)` 时，解析为结构体初始化 `Name { field: value, ... }`。两个条件分别防止：match subject 的 `{` 被抢走、with 表头的 `{`（绑定体起始）被误判为 StructInit。
2. **单参 lambda**：`x => expr` 或 `x: T => expr`。后者先试探 `parse_return_type` + `Arrow`，失败则 rollback 回普通标识符。
3. **普通标识符** `Expression::Identifier`。

回归测试 `b_05_d_02_single_param_lambda`（:242-259）验证 `x: i32 => x` 保留 `return_type`、`async x => x` 保留 `is_async`。

**`parse_paren_or_lambda`**（`:181-188`）：遇到 `(` 时，先试探是否为 lambda 参数列表（`try_parse_paren_lambda`，`:192-215`）——解析到 `=>` 则构造 lambda，否则整体 rollback 交给 `parse_paren_or_tuple`。这是典型的「试探 + 回滚」消歧。

**`parse_paren_or_tuple`**（`:222-262`）：解析括号表达式或元组，期间 `with_shield += 1`。两条错误（审计 E-07，回归测试 `e_07_tuple_arity`，:538-550）：

- `()` → `空元组 `()` 不是合法表达式（元组至少需要两个元素）`
- `(a,)` → `单元素元组 `(a,)` 不受支持（元组至少需要两个元素）`

`:267-286` 的逗号列表解析返回 `(Vec<Expression>, has_trailing_comma)`。

**数组与对象字面量**（`:289-375`）：

- 数组 `[a, b, c]`：逗号分隔的表达式列表。
- 对象 `{ key: value, "str key": value }`：键值对，key 可是标识符或字符串字面量。字符串 key 经 `crate::unquote` 去引号（复用 lib.rs 的工具函数）。

### 11.5 `lambda.rs`：lambda 参数与体

`parse_lambda`（`:20-45`）：处理完整 lambda 与简写：

- `async x => x` / `async x: T => x`（async 单参简写）
- `(x, y: i32) => y`（多参）
- `() => 1`（零参）

流程：消费可选 `async` → 识别单参简写（`parse_single_param_lambda`）或走 `parse_lambda_params`（`parse_typed_params`）→ 可选 `parse_return_type` → `expect(Arrow)` → `parse_lambda_body`。

`parse_single_param_lambda`（`:52-61`）/ `finish_single_param_lambda`（`:66-86`）：处理单参简写，组装 `LambdaParams::Single`。

`parse_lambda_params`（`:89-92`）：复用 `types.rs::parse_typed_params`，所以 lambda 参数与函数参数共享同一套解析逻辑（包括护盾）。

`parse_lambda_body`（`:95-107`）：函数体可是 `{ ... }` 块，也可是单个表达式。

回归测试 `b_03_b_04_lambda_forms`（:227-238）验证 `() => 1` 与 `(x, y: i32) => y` 都被正确解析。

### 11.6 `match_expr.rs`：match 表达式与消歧

`parse_match`（`:18-20`）是 guarded 包装，主体（`:23-86`）解析：

```
match subject {
    case pattern [if guard] => result,
    case : Type => result,          // 类型模式
    default => result,
}
```

**`in_match_subject` 机制**：解析 match subject 时临时置 `in_match_subject = true`，防止 subject 后面的 `{` 被 `parse_primary` 的标识符分支误判为 StructInit（见 11.4）。subject 解析完后恢复。

**case 序列的规则与诊断**：

- 重复 `default` → `match 中只允许一个 default 分支`
- `default` 之后的 `case` → `case 分支不能出现在 default 之后`
- 陌生 token → `此处应为 `case` / `default` 或 `}``
- `recover_to_next_match_arm`（`:89-103`）：某个分支解析失败时跳到下一个 case/default，避免卡死。

回归测试 `c_05_match_default_diagnostics`（:397-406）验证前两条诊断。

**`parse_match_case`**（`:106-140`）：

- 普通模式：`case pattern`，可选 `if guard`。
- 类型模式：`case : T`——临时置 `allow_arrow_after_type = true`，使类型后的 `=>` 被接受为分支箭头（否则会触发「类型位不支持 =>」错误）。
- 分支体解析期间置 `inside_match += 1`，让嵌套表达式在 case/default/} 处自然停下（见 11.2）。

**`parse_match_case_expression`**（`:146-155`）：空分支体报 `此处应为表达式`。

**`parse_match_body`**（`:158-171`）：分支体可是 `=> expression` 或 `=> { block }`。

---
## 12. 歧义消解：上下文标记体系

parser 面临多个「同一 token 在不同上下文有不同含义」的歧义。它没有采用「全局猜测 + 回溯」的暴力策略，而是用一组**显式上下文标记**做局部消解。这套机制是理解 parser 设计精髓的关键。

### 12.1 五个标记的分工

| 标记 | 类型 | 解决的歧义 |
|------|------|-----------|
| `with_stack` | `u32` | with 表头的 `as`（绑定 vs cast）、`{`（表头体 vs StructInit） |
| `with_shield` | `u32` | 屏蔽 `with_stack` 规则，用于表头内的嵌套位置（参数/实参/元组） |
| `allow_arrow_after_type` | `bool` | 类型位的 `->` vs `=>`、lambda/match 返回注解例外 |
| `inside_match` | `u32` | match 分支体的边界（`case`/`default`/`}` 处停止表达式解析） |
| `in_match_subject` | `bool` | match 主语后的 `{` 是 match 体而非 StructInit |

### 12.2 消解案例一：`with expr as binding` 的 `as`

这是最复杂的消歧。`as` 在 novella 中既是 cast 运算符（`x as T`），又是 with 绑定引入符（`with open() as f`）。

规则（`parse_cast` 的循环条件）：

```
当 with_stack > 0 && with_shield == 0 && looks_like_with_binding() 时：
    不把 as 当 cast —— 让 parse_with 消费它作为绑定
否则：
    把 as 当 cast 解析
```

`with_shield` 的作用是处理「表头里的嵌套结构」：

```
with foo(x as T) as y { ... }
     ^^^^^^^^  ← 这里在实参列表内，with_shield > 0，所以 x as T 是 cast
              ^^^^^  ← 这里在表头顶层，with_shield == 0，所以 as y 是绑定
```

`parse_arguments`、`parse_typed_params`、`parse_paren_or_tuple` 都在进入时 `with_shield += 1`、退出时 `-= 1`，从而让这些嵌套位置的表头消歧规则失效。

### 12.3 消解案例二：`match subject {` 的 `{`

`match x { ... }` 里，`x` 后的 `{` 是 match 体起始，不是结构体初始化。解析器在解析 subject 前置 `in_match_subject = true`，`parse_primary` 的标识符分支据此禁止 StructInit 解析（见 11.5 节）。

### 12.4 消解案例三：match 分支体的边界

`case 1 => expr case 2 => expr` 中，若 `parse_expression` 不做限制，expr 的解析可能吞掉后面的 `case`。`inside_match > 0` 时 `parse_expression` 在 `Case`/`Default`/`CloseBrace` 处返回 `None`，让分支自然结束。

### 12.5 消解案例四：类型位的箭头

类型位唯一合法箭头是 `->`，但 lambda 返回注解（`(x: T): U => x`）与 match 类型模式（`case : T =>`）中，`=>` 是外层 lambda/match 语法的一部分，不是类型箭头。`allow_arrow_after_type` 在这两处临时置 `true`，让 `expect_type_arrow` 接受 `=>`（见 9.4 节）。

### 12.6 消解案例五：泛型调用 vs 比较运算

`f<T>(x)` vs `a < b > (c)` 的歧义靠 `looks_like_call` 的严格判定：`<...>` 后必须紧跟 `(`，且尖括号内全是 `is_type_token` 集合中的 token。这个判定保守但准确，配合 `checkpoint`/`rollback` 实现安全的试探。

### 12.7 设计哲学

项目约定（见 AGENTS.md NOTES）：

> 新增同类歧义应复用这些上下文标记，不要在 `parse_primary` 里堆特判。

这条约定点明了设计哲学：**上下文标记是「声明式」的消歧，而 parse_primary 里的特判是「命令式」的硬编码**。前者可组合、可推理、局部化；后者会让解析逻辑腐化成一团 `if`。这也是 parser 能保持清晰架构的关键。

---

## 13. 错误恢复与诊断生成

### 13.1 三层恢复策略

parser 采用分层恢复，粒度由细到粗：

| 层级 | 机制 | 作用范围 | 位置 |
|------|------|----------|------|
| 细粒度 | `checkpoint`/`rollback` | 试探分支失败（lambda、泛型调用、括号） | `lib.rs:166-181` |
| 中粒度 | `recover_to_statement_boundary` | 语句解析失败 | `stmt.rs:711-726` |
| 粗粒度 | `recover_after_item` | 顶层 item 解析失败 | `lib.rs:273-291` |
| 函数级 | `skip_to_function_end` | 函数解析失败 | `lib.rs:1015-1042` |
| match 级 | `recover_to_next_match_arm` | match 分支失败 | `match_expr.rs:89-103` |

### 13.2 诊断结构

所有诊断都是 `novella_core::diagnostic::Diagnostic`，带：

- `message`：中文错误信息（本文各章已列出关键文本）。
- `code`：`DiagnosticCode::SyntaxError`。
- `location`：`{ file, line, column, span }` 四元组。

行号 1-based，列号 0-based 字节偏移（对齐 ANTLR `getLine()` / `getCharPositionInLine()`）。CLI 的 `render_diagnostic` 以 rustc 风格渲染：

```
error: <message>
  --> file:line:column
   |
 N | <源码行>
   |     ^^^^
```

展示列号 1-based，caret 按字符渲染（多字节字符不错位）。

### 13.3 诊断的单一出口

项目约定：

> 诊断只有一份：JSON 的 `diagnostics` 是唯一诊断树，禁止再加 `sema_diagnostics` 之类的并列字段。

parser 的诊断进入 `ParsedSource.diagnostics`，由 sema loader 汇总到统一的 `DiagnosticReport.modules` 中，与词法/语义诊断按 `span.start` 合并排序。下游不得自行再跑 parser 产生第二套诊断。

### 13.4 错误恢复的「尽力而为」原则

parser 在遇到错误时总是尽量：

1. **记录诊断**（当前 token 处，或指定 span 处）。
2. **继续解析**（消费错误 token 或跳到同步点），产出尽可能完整的 AST。

例如 `expect_type_arrow` 遇到 `=>` 时，报错后仍按 `->` 消费它继续解析，保留后续类型信息。这种策略让 LSP 在用户写错代码时仍能给出 outline、补全等能力。

---

## 14. 上下游契约

### 14.1 输入侧：lexer → parser

```
lexer(src: &str) → LexerResult { tokens: Vec<Token>, errors: Vec<TokenError> }
```

关键契约（`crates/novella-lexer/src/lib.rs:476-532`）：

- lexer 产出的 token 流**始终以 EOF 结尾**，EOF 的 span 指向输入末尾 `{start:len, end:len}`，line/column 由 `LineIndex::locate` 定位。
- 可恢复的词法错误（非法字符、未闭合字符串）**只进 `errors`，不进 `tokens`**；`TokenKind::Error` 作为占位随错误记录。parser 假设收到的 token 流在词法层面是可解析的。
- `TokenKind` 是稳定公共种类（`crates/novella-core/src/token.rs:15-242`），`RawToken`（logos 私有枚举）通过 `From<RawToken> for TokenKind` 穷尽映射。parser 只见 `TokenKind`，不接触 logos。
- `Token` 结构（`token.rs:252-259`）：`{ kind, span, slice, line, column }`。行号 1-based、列号 0-based 字节偏移。

### 14.2 输出侧：parser → sema

parser 的产物被 sema 的模块加载器收纳：

```rust
// crates/novella-sema/src/module/mod.rs:26-32
pub struct ParsedSource {
    pub source: String,
    pub tokens: Vec<Token>,       // 完整 token 流（含注释）
    pub program: Program,         // parser 产出的 AST
    pub diagnostics: Vec<Diagnostic>,
    pub syntax_failed: bool,      // 根据 diagnostics 是否非空计算
}
```

**「全流程唯一一次」契约**（`crates/novella-sema/src/module/loader.rs:20-72`）：`load_modules` 对每个模块做**且仅做一次**词法 + 语法分析。`AnalyzedModule`（`crates/novella-sema/src/api.rs:100-119`）明确：

> 这是语法阶段产物的唯一出口：CLI 输出 JSON、codegen 取 AST 一律从这里取，不再自行调用 `novella_lexer` / `novella_parser`。

下游（CLI/codegen）重跑 lexer/parser 是**反模式**——会让入口文件解析两次并产生两套诊断。

### 14.3 LSP 侧的特殊用法

LSP 是 parser 的一个特殊消费者（`crates/novella-lsp/src/document.rs:60-70`）：

- `SourceDoc::new` 自己跑 lexer，然后 `Parser::new(ParserInput { tokens, file: "TEST".to_string() })` 供 outline 提取。
- 注释由 parser 的 `preprocess` 过滤，LSP 不二次过滤。
- `crates/novella-lsp/src/server/analysis.rs:34-52` 把 doc 的 lex + syntax 诊断合成 `ParsedSource` 注入 overlay，复用同一套 sema 流程。

`ParserInput` 的调用方（CodeGraph blast radius）：

- `crates/novella-sema/src/module/loader.rs`
- `crates/novella-lsp/src/document.rs`
- `crates/novella-lsp/src/outline/mod.rs`
- `crates/novella-sema/src/typecheck/expr/strings.rs`（字符串字面量解析的辅助场景）

`Parser` 本身只在 `lib.rs` 内被 `parse()` 调用；`ParseResult` 有 6 处引用。

### 14.4 与文法文件的契约

- `grammar/NovellaParser.g4` 以代码为基准对齐；AST 节点的 doc 标注对应产生式名。
- 改动 parser 语法后必须同步 `.g4`；反之不得凭 `.g4` 要求代码。
- `.antlr/` 生成产物（`.java`/`.interp`/`.tokens`）已 gitignore，不得提交。

---

## 15. 回归测试矩阵

`crates/novella-parser/tests/boundary_regression.rs`（550 行）是 parser 行为的「活规格」。测试辅助函数：

- `parse`（:16-28）：lexer → 断言无词法错误 → **retain 过滤 LineComment** → `Parser::new(file:"TEST").parse()`。注意测试自己也过滤了注释——这与 `preprocess` 的过滤重复但无害（parser 内部会再过滤一次），测试这样写是为了模拟 sema loader 传入干净流的场景。
- `parse_clean`（:31-39）：断言零诊断。
- `assert_has_diagnostic`（:49-55）：按 message 子串匹配。
- `function_stmts`（:58-69）：取首个 `Item::Function` 的 Block stmts。

### A 类：递归防护

| 测试 | 位置 | 验证内容 |
|------|------|----------|
| `a_deep_nesting_is_parsed_not_rejected` | :78-130 | 500 层 `!`、300 层三元、300 层赋值、300 层类型括号，逐层剥 AST 断言深度 |
| `a_deep_nesting_survives_small_thread_stack` | :140-157 | 8000 层括号在 256 KiB 栈线程中解析，零诊断（证明分段栈生效） |
| `a_excessive_depth_reports_upper_bound` | :161-178 | 20000 层括号报「超出工程上限」 |

### B 类：语法形态

| 测试 | 位置 | 验证内容 |
|------|------|----------|
| `b_01_with_header_without_parens` | :184-200 | 无括号 with 零诊断；`with (sysCtx)` 报迁移诊断 |
| `b_02_type_suffix_after_paren` | :204-223 | `(A \| B)*` / `(i32, i32)[]` 类型后缀 |
| `b_03_b_04_lambda_forms` | :227-238 | `() => 1`、`(x, y: i32) => y` |
| `b_05_d_02_single_param_lambda` | :242-259 | `x: i32 => x` 保留 return_type；`async x => x` 保留 is_async |
| `b_06` | :263-276 | `for (arr[i] = 0; ...)` init 为 `ForInit::Expression` |
| `b_07` | :280-282 | `g<func (a: i32) -> i32>(id)` 复杂泛型实参 |
| `b_08_c_04_d_01_new_expression` | :287-322 | `new getFoo()()` target 为 Call；`new Vec<i32>(1)` 保留 type_args/args |

### C 类：语句与边界

| 测试 | 位置 | 验证内容 |
|------|------|----------|
| `c_01_with_tuple_binding` | :328-339 | with 元组绑定 → `BindingPattern::Tuple` |
| `c_02_empty_statement` | :343-352 | `while (x);` body 为 `Statement::Empty` |
| `c_03_for_in_requires_top_level_in` | :358-393 | 同层 `in` 判 ForIn，嵌套 `in` 不误判 |
| `c_05_match_default_diagnostics` | :397-406 | 重复 default / default 后 case 的诊断 |
| `c_06_multi_binding_is_variable_list` | :410-416 | 多绑定 → `VariableList`（2 declarations） |

### D 类：item 与 AST 保真

| 测试 | 位置 | 验证内容 |
|------|------|----------|
| `d_03_field_attributes_kept` | :422-431 | 字段编译器指令保留 |
| `d_04_trait_scope` | :435-441 | trait 可见性保留 |
| `d_05_extern_attributes` | :445-452 | extern 函数编译器指令保留 |
| `d_06_struct_heritage_kept` | :456-462 | struct heritage 保留 |
| `d_07_d_09_item_span_starts_at_modifiers` | :466-477 | `async func` 的 span.start = 2（含修饰符起点） |
| `d_08_import_prefix` | :481-497 | `"std:exception"` → `prefix=std, path=exception` |

### E 类：诊断文本

| 测试 | 位置 | 验证内容 |
|------|------|----------|
| `e_03_arrow_in_type_position_rejected` | :504-513 | 类型位 `=>` 报错；lambda 返回注解例外 |
| `e_05_method_requires_func_keyword` | :517-525 | 省略 `func` 的方法声明报错 |
| `e_06_weak_method_rejected` | :529-534 | `weak` 修饰方法报错 |
| `e_07_tuple_arity` | :538-550 | `()` / `(a,)` 在表达式与类型位均报错 |

---

## 16. 关键设计决策与权衡

### 16.1 手写递归下降 vs ANTLR 生成

**决策**：手写递归下降。

**理由**：

- 需要精细的上下文消歧（`with`/`match`/类型箭头），生成式解析器难以插入这些标记。
- 需要与 AST 的自定义 `Drop` / span 布局紧密配合。
- 需要 `checkpoint`/`rollback` 的细粒度试探控制。
- 文法文件作为文档产物保留，以代码为基准。

**代价**：代码量大（约 3800 行），维护成本高；必须手写所有错误恢复逻辑。

### 16.2 token 预处理 vs 解析期处理

**决策**：在 `Parser::new` 一次性做注释过滤与 import 降级。

**理由**：让主解析逻辑完全不感知这些「噪音」，保持解析分支清晰。

**代价**：`Module.tokens`（完整流）与 parser 内部流（过滤后）不同，需要明确约定避免下游混淆。项目用「注释过滤只在 preprocess 一处」的硬规则来管理这个复杂度。

### 16.3 上下文标记 vs 全局回溯

**决策**：用 `with_stack`/`with_shield`/`inside_match` 等显式标记消歧，而非依赖大规模回溯。

**理由**：局部、可组合、性能可预测。全局回溯在深层嵌套输入下会指数爆炸。

**代价**：新增语法结构时可能需要引入新标记，增加状态复杂度。

### 16.4 `VariableList` vs 包 `Block`

**决策**：多绑定声明用扁平的 `VariableList`，不包 `Block`。

**理由**（审计 C-06）：`Block` 引入不存在的嵌套作用域，会破坏 sema 的作用域分析。

**代价**：sema 需专门处理 `VariableList` 节点（按序求值、同一作用域）。

### 16.5 分段栈防护 vs 限制嵌套深度

**决策**：用 `stacker` 支持深层合法输入，仅在病态深度（512 段 × 2 MiB = 1 GiB 栈空间）时报上限。

**理由**：合法的深层输入（数万层括号）应被支持，而非简单拒绝。

**代价**：引入 `stacker` 依赖与 `guarded` 包装样板；AST 侧也必须配套自定义 `Drop`。

### 16.6 诊断「尽力而为」vs 遇错即停

**决策**：遇错记录诊断后尽力恢复继续。

**理由**：LSP 等工具需要在有语法错误时仍产出可用的 AST（outline、补全）。

**代价**：错误恢复逻辑复杂，且产出的 AST 可能不完整；调用方需检查 `diagnostics` 而非只看 AST。

---

## 17. 注释与实现的不一致点

以下是精读过程中发现的**文档滞后**之处，如实记录但不改源码：

### 17.1 `lib.rs:44-46`：>> 拆分的时机

- **注释称**：`Parser` 构造时含 `>>` 拆分。
- **实际**：拆分发生在 `split_shift_at`（`lib.rs:188-201`）的按需调用中，由 `expect_angle_close` 等只在期待尖括号闭合时触发。全局拆分会破坏表达式里的右移运算，所以按需策略是正确演进，注释未同步。

### 17.2 `preprocess.rs` 文件头注释：类型箭头的处理位置

- **注释称**：类型位箭头统一为 `->`，`=>` 报错（暗示在预处理阶段处理）。
- **实际**：该逻辑在 `types.rs::expect_type_arrow`（`:163-169`）实现，预处理阶段不碰箭头。

### 17.3 `recursion.rs` 常量注释 vs 实际值

- **注释多处写**：「512 KiB 段 / 256 MiB 预算」。
- **实际常量**：`STACK_SEGMENT = 2 MiB`（`:24`），`MAX_STACK_BYTES = 1 GiB`（`:49`），`MAX_STACK_SEGMENTS = 512`（`:52`）。
- **影响**：若根据注释估算「超限深度」，会得到错误结论。以常量为准。

> 这三点不影响运行行为，但阅读源码时容易产生误解。本文将其集中列出，供后续维护者参考（若要修，应改注释而非改代码）。

---

## 18. 扩展指南：如何安全地改动 parser

### 18.1 新增语法结构的检查清单

1. **AST 先行**：在 `crates/novella-ast/src/` 定义节点，携带 `span: Span` 字段并 `#[derive(Spanned)]`；若是递归 `Box`/`Vec` 字段，同步 `drop_guard.rs` 与 `GuardedClone`。
2. **TokenKind 同步**：若需新 token，在 `crates/novella-core/src/token.rs` 加变体，并在 lexer 的 `RawToken` 与 `From` 映射中同步（漏了编译报错）。
3. **解析器实现**：在对应分文件加 `parse_xxx` 方法；若是递归入口，写成「`parse_xxx`（guarded 包装）+ `parse_xxx_inner`（实体）」两层。
4. **消歧复用**：遇到 token 歧义，优先复用现有上下文标记；确需新标记时，在 `Parser` 加字段并确保 `checkpoint`/`rollback` 覆盖（若该标记影响试探解析）。
5. **诊断文本**：用中文，定位当前 token 用 `error_here`，定位历史 token 用 `error_at_span`，错误码用 `DiagnosticCode::SyntaxError`。
6. **文法同步**：更新 `grammar/NovellaParser.g4` 对应产生式，并给 AST 节点 doc 标注产生式名。
7. **测试**：在 `boundary_regression.rs` 加用例（正常形态 `parse_clean` + 错误形态 `assert_has_diagnostic`）。
8. **文档**：若改变了公开契约或架构，同步本文与 `grammar/README.md`。

### 18.2 修改递归入口的注意点

- `guarded` 的闭包签名是 `FnOnce(&mut Self) -> T`，注意借用形式（`|this| this.xxx()`）。
- `stacker::maybe_grow` 的参数顺序是 `(red_zone, stack_size)`，别写反。
- 所有可能递归的入口都必须 guarded；漏掉一个就可能在某些深层输入下栈溢出。
- 若新增了影响试探解析的状态字段，考虑是否要加进 `checkpoint`/`rollback` 的元组。

### 18.3 修改消歧规则的注意点

- `with_shield` 的 `+1/-1` 必须成对；推荐用 guarded 闭包包裹，保证 panic 安全（当前代码多为手动加减，正常流程安全）。
- `allow_arrow_after_type` 当前是「临时置 true → parse → 置 false」而非保存/恢复旧值（见 9.10 节的潜在边界）。改动嵌套场景时留意。
- 消歧前瞻（如 `looks_like_with_binding`）要保守：宁可少识别一种合法形态，也不要误吞其他语法。

### 18.4 反模式清单（项目约定）

- ❌ 让下游重跑 lexer/parser——`analyze` 已把产物放进 `SemaResult.modules`。
- ❌ 在 `preprocess.rs` 之外过滤注释——注释过滤只有一处。
- ❌ 把 `RawToken`/logos 泄漏到 parser 或下游。
- ❌ 只改 `TokenKind` 不改 `RawToken`（或反之）。
- ❌ 在 `parse_primary` 里堆特判解决歧义——应复用上下文标记。
- ❌ 给递归入口漏写 guarded 包装。
- ❌ 修改 AST 递归字段却不同步 `drop_guard.rs`。

---

## 19. 附录：符号速查表

### 19.1 lib.rs

| 符号 | 行号 | 类别 |
|------|------|------|
| `ParseResult` | :28 | struct |
| `ParserInput` | :34 | struct |
| `Parser` | :48 | struct |
| `Parser::new` | :78 | fn |
| `peek` / `peek_n` / `at_eof` / `advance` | :96-130 | fn |
| `check` / `check_n` / `eat` / `expect` | :131-157 | fn |
| `checkpoint` / `rollback` | :166 / :176 | fn |
| `split_shift_at` / `expect_angle_close` | :188 / :204 | fn |
| `error_here` / `error_at_span` | :212 / :227 | fn |
| `parse` | :249 | fn |
| `recover_after_item` | :273 | fn |
| `parse_item` | :309 | fn |
| `parse_import` | :432 | fn |
| `parse_type_alias` | :525 | fn |
| `parse_trait` | :549 | fn |
| `parse_class` | :575 | fn |
| `parse_extern_function` | :607 | fn |
| `parse_class_parent` | :653 | fn |
| `parse_member_block` / `parse_member` | :679 / :701 | fn |
| `parse_directive` / `parse_directives` / `parse_directive_statement` | :813 / :863 / :882 | fn |
| `parse_function` | :926 | fn |
| `parse_struct` | :991 | fn |
| `skip_to_function_end` | :1015 | fn |
| `split_token_pair` / `unquote` / `namespace_alias` / `describe_token` | :1049 / :1086 / :1097 / :1106 | fn |

### 19.2 其他文件

| 符号 | 位置 |
|------|------|
| `preprocess` / `downgrade_from_import` | `preprocess.rs:19` / `:34` |
| `STACK_SEGMENT` / `RED_ZONE` / `MAX_STACK_BYTES` / `MAX_STACK_SEGMENTS` | `recursion.rs:24` / `:29` / `:49` / `:52` |
| `guarded` | `recursion.rs:63` |
| `parse_type_expr` / `parse_type_union` | `types.rs:28` / `:37` |
| `parse_type_arrow_or_primary` | `types.rs:63` |
| `expect_type_arrow` / `parse_function_type` | `types.rs:163` / `:172` |
| `parse_primary_type` / `parse_type_suffixes` / `parse_type_reference` | `types.rs:254` / `:275` / `:311` |
| `parse_typed_params` / `parse_typed_params_inner` | `types.rs:382` / `:392` |
| `parse_return_type` | `types.rs:447` |
| `parse_block` / `parse_block_inner` | `stmt.rs:25` / `:33` |
| `parse_statement` / `parse_statement_inner` | `stmt.rs:58` / `:63` |
| `parse_variable_statement` / `parse_binding_pattern` | `stmt.rs:113` / `:187` |
| `parse_if` / `parse_while` / `parse_for` | `stmt.rs:222` / `:288` / `:303` |
| `looks_like_for_in` / `parse_for_init` | `stmt.rs:341` / `:386` |
| `parse_try` / `parse_with` / `parse_unsafe` | `stmt.rs:480` / `:525` / `:566` |
| `expect_statement_end` / `recover_to_statement_boundary` | `stmt.rs:662` / `:711` |
| `is_type_token` / `assign_op` / `binary` | `expression/helper.rs:12` / `:33` / `:51` |
| `parse_expression` / `parse_assignment` / `parse_ternary` | `expression/stairs.rs:23` / `:40` / `:67` |
| `looks_like_with_binding` / `parse_cast` / `parse_unary` | `expression/stairs.rs:103` / `:253` / `:330` |
| `parse_postfix` / `parse_call_suffix` / `looks_like_call` | `expression/postfix.rs:17` / `:114` / `:151` |
| `scan_call_angle` / `parse_arguments` | `expression/postfix.rs:173` / `:248` |
| `parse_primary` / `parse_paren_or_lambda` / `parse_paren_or_tuple` | `expression/primary.rs:9` / `:181` / `:222` |
| `parse_lambda` / `parse_single_param_lambda` / `parse_lambda_body` | `expression/lambda.rs:20` / `:52` / `:95` |
| `parse_match` / `parse_match_case` / `parse_match_body` | `expression/match_expr.rs:18` / `:106` / `:158` |

---

## 结语

novella-parser 是一个**手写递归下降解析器**的教科书式实现，其设计围绕三个核心命题展开：

1. **清晰的分层**：token 预处理 → 顶层 item → 语句 → 表达式阶梯 → 原子，每层职责单一。
2. **显式的上下文管理**：用一组计数器/布尔标记而非隐式全局状态消解语法歧义，配合 `checkpoint`/`rollback` 实现安全试探。
3. **鲁棒的边界处理**：三层错误恢复 + 分段栈递归防护 + 「尽力而为」的诊断策略，保证从合法深层输入到病态输入都有明确行为。

理解这份代码，关键在于抓住「**一个 Parser 结构体贯穿始终，所有解析都是它的方法，上下文标记是这个结构体的记忆**」这一主线。一旦建立这个心智模型，即便代码分散在十几个文件里，也能清晰地追踪任何一个语法结构的解析路径。

---

*文档完*

# novella 项目知识库

**生成时间:** 2026-09-05 · **commit:** 351f7a6 · **分支:** main

## OVERVIEW

novella 语言编译器。Rust workspace（edition 2024，Apache-2.0，8 个 crate），ANTLR4 文法与代码对齐（词法↔`TokenKind`，语法↔AST）。当前为早期阶段：**词法分析可用**，AST 部分落地，parser 骨架实现中（已支持装饰器、函数声明、`let`/`const` 变量声明、`continue` 语句等部分结构），sema/codegen 仍为空壳占位。

## STRUCTURE

```
novella/
├── crates/
│   ├── novella-core/    # 共享基础：Span / Spanned<T> / TokenKind / Token / Diagnostic
│   ├── novella-derive/  # proc-macro：#[derive(Spanned)] 等 AST 派生宏
│   ├── novella-ast/     # AST 节点定义（expr / stmt / types / program）
│   ├── novella-lexer/   # 词法器（logos），RawToken → TokenKind 转换
│   ├── novella-parser/  # 手写递归下降解析器（骨架，逐条语句实现中）
│   ├── novella-sema/    # 空壳（占位 add()）
│   ├── novella-codegen/ # 空壳（占位 add()）
│   ├── novella-cli/     # 二进制（包名 novella）：clap CLI，仅 compile 子命令
│   └── novella-lsp/     # 语言服务器（lib + 二进制）：薄 LSP，输出词法级语义高亮
├── editors/vscode/      # VS Code 扩展（esbuild 打包；启动 novella-lsp，不含语法规则）
├── grammar/             # ANTLR4 文法（以代码为基准对齐）：NovellaLexer.g4 / NovellaParser.g4
├── Cargo.toml           # workspace（members = crates/*，resolver = 2）
└── Justfile             # 命令入口
```

依赖链（自上而下单向，禁止反向）：

```
core ← ast ← parser ← sema ← codegen ← cli
core ← lexer ← parser
core ← lexer ← novella-lsp（独立二进制，不经 cli）
ast ──(构建期)── novella-derive（proc-macro，展开引用 ::novella_core::Span）
```

## WHERE TO LOOK

| 任务 | 位置 | 备注 |
|------|------|------|
| 加/改词法单元 | `crates/novella-core/src/token.rs` + `crates/novella-lexer/src/lib.rs` | 双枚举必须同步（见 CONVENTIONS） |
| 加 AST 节点 | `crates/novella-ast/src/` | 按 `expr/stmt/types/program` 分类，参考文法产生式对照；新节点须携带 `span` 字段并 `#[derive(Spanned)]`（见 CONVENTIONS） |
| 诊断/错误显示 | `crates/novella-cli/src/error.rs` + `crates/novella-core/src/diagnostic.rs` | 通用诊断结构（各阶段产出）→ `render_diagnostic` rustc 风格渲染 |
| CLI 子命令 | `crates/novella-cli/src/cli.rs` | clap derive |
46	| 文法（ANTLR4） | `grammar/NovellaLexer.g4` + `grammar/NovellaParser.g4` | 以代码为基准对齐：词法↔`TokenKind`，语法↔AST 节点；改动代码后须同步（见 `grammar/README.md`） |
| 编辑器语法高亮 | `crates/novella-lsp/src/` + `editors/vscode/` | 薄 LSP：`semantic.rs` 做类别映射与 UTF-16 偏移编码，`protocol.rs` 做报文帧，`server.rs` 做分发 |
| 调整高亮类别 | `crates/novella-lsp/src/semantic.rs` | `token_type_index` 为穷尽 match；改动映射须同步 `TOKEN_TYPES`（数组顺序即 legend 下标） |
| 打包 VS Code 扩展 | `editors/vscode/` | `just lsp-pack`；源码 `src/extension.js` 经 esbuild 打入 `dist/`，vsix 自包含无需 node_modules |
| 占用位 crates | sema / codegen | `pub fn add(u64, u64) -> u64` 待替换 |

## CODE MAP

| 符号 | 位置 | 说明 |
|------|------|------|
| `Span`, `Spanned<T>` | `novella-core/src/lib.rs` | 源码位置区间（字节偏移）/ 通用位置包装（AST 已改为节点内嵌 span，`Spanned` 暂保留备用） |
| `Spanned`（derive 宏） | `novella-derive/src/lib.rs` | 为携带 `span` 字段的节点生成 `span()` 方法；struct 取字段、enum 具名变体取字段、newtype 变体委托内部节点 |
| `Diagnostic` / `Severity` / `DiagnosticLocation` | `novella-core/src/diagnostic.rs` | 与阶段无关的通用诊断结构（serde），各阶段产出、CLI 渲染 |
| `TokenKind`（107 变体） | `novella-core/src/token.rs` | 词法单元种类，纯数据投影，带 `Display`（输出 Debug 文本）；含 `LineComment`（行注释） |
| `Token` | `novella-core/src/token.rs` | kind + span + slice + line + column |
| `RawToken` | `novella-lexer/src/lib.rs` | logos 私有枚举；`From<RawToken> for TokenKind` 穷尽映射 |
| `lexer(&str) -> LexerResult` | `novella-lexer/src/lib.rs` | 入口：产出含 EOF 的 token 流与词法错误列表；错误仅入 `errors` |
| `Expression` / `Literal` | `novella-ast/src/expr.rs` | 完整表达式节点集（字面量/标识符/括号/元组/数组/对象/this/一元/二元/赋值/成员/索引/调用/构造/宏调用/await/is/三元/lambda/match），运算符由 `UnaryOp`/`BinaryOp`/`AssignOp` 集中定义 |
| `Statement` / `IfStmt` / `ForStmt` | `novella-ast/src/stmt.rs` | 含 `BindingPattern`（标识符/元组解构递归）、`Continue`；`TryStmt` / `WithStmt` 亦在此 |
| `TypeExpr` / `PredefinedType` | `novella-ast/src/types.rs` | 类型表达式（Predefined/Reference/Array/Tuple/Function/Union）；`Param` 含 `default` 默认值 |
| `Program` / `Item` / `Scope` | `novella-ast/src/program.rs` | 顶层 item；`Scope` 注意拼写 `Pubilc` |
| `token_type_index` / `encode` | `novella-lsp/src/semantic.rs` | `TokenKind` → legend 下标（穷尽 match）/ token 流 → semantic tokens（字节偏移换算为 UTF-16 码元） |
| `Server::handle` | `novella-lsp/src/server.rs` | JSON-RPC 报文分发：请求必回复、通知不回复 |
| `read_message` / `write_message` | `novella-lsp/src/protocol.rs` | stdio 上的 `Content-Length` 报文帧编解码 |

## CONVENTIONS

- 注释、doc、commit、文档均中文。
- **TokenKind ↔ RawToken 双枚举**：`token.rs` 定义稳定公共种类（doc 注释标注对应字面量），`lexer` 的 `RawToken` 是 logos 专属投影，靠 `From` 的 match 穷尽性保证新增变体不漏转（漏了编译报错）。
- **下游不接触 logos**：parser 等阶段只见 `novella_core::token::Token`。
- **AST 注释对齐文法**：每个节点 doc 标注 `对应文法 xxx`（以 `grammar/*.g4` 产生式名为准）；反之文法只描述 AST 存在的结构，两侧不一致时以代码为准。
- **AST 节点全量挂 span**：每个 AST 节点（struct 或 enum 变体）携带 `span: novella_core::Span` 字段，纯值枚举（运算符、`Literal`、`PredefinedType`、`Scope` 等）除外；`span()` 方法由 `#[derive(Spanned)]` 自动生成，字段名必须为 `span`，enum 单字段 newtype 变体自动委托内部节点。
- **类型全量 serde**：AST / token 均派生 `Serialize, Deserialize`，用于 JSON 输出。
- **for 循环 init 语义**：文法中 init 段声明不带 `const/let`，统一按 `Let` 承载（见 `ForInit` 注释）。
- **值语义延后**：AST 字面量保留源码原文（含 `f/r/b`、进制前缀），解析在语义分析阶段。
- **空壳 crate 用占位**：sema / codegen 目前允许 `add()` 占位（parser 已进入实现阶段，不再用占位）。
- **注释在语法层透明**：`LineComment` 是正式 token（供高亮等消费），CLI 在喂给 parser 前过滤；parser 不感知注释。
- **LSP 偏移一律 UTF-16**：novella 内部用字节偏移，LSP 语义 token 要求 UTF-16 码元；换算集中在 `novella-lsp/src/semantic.rs`，新增 LSP 能力时须复用该换算而不要另写一套。
- **高亮规则不重复维护**：编辑器的染色类别映射以 `TokenKind` 为唯一来源，禁止在插件侧另写词法/关键字表。

## ANTI-PATTERNS（本项目）

- ❌ 把 `RawToken` / logos 类型泄漏到 core 或下游 crate——必须经 `TokenKind` 转换层。
- ❌ 只改 `RawToken` 不改 `TokenKind`（或反之）——两个枚举必须同步演进。
- ❌ 在 `token.rs` 之外维护并列的字面量/关键字集合——`TokenKind` 是唯一事实源。
- ❌ 在 `novella-lsp` 或 VS Code 插件里重复维护词法/关键字规则——高亮必须复用 `novella-lexer`。
- ❌ 输出 LSP 语义 token 时直接用字节偏移——含中文/emoji 的行会整体错位，必须换算为 UTF-16 码元。
- ❌ 把 `grammar/.antlr` 生成产物（`.java`/.interp/.tokens）提交——已 gitignore。
- ❌ 只改代码不同步 `grammar/*.g4`——文法以代码为基准，`TokenKind` / AST 变更后必须同步；反之也不得凭文法去要求代码。
- ❌ 修改 `Span`/`Token` 字段时不更新 JSON 消费者（cli 输出 `serde_json`）。
- ❌ 除非明确要求，否则不写测试

## COMMANDS

```bash
just check        # cargo check --workspace --all-targets
just build        # cargo build --workspace
just test         # cargo test --workspace
just lint         # clippy --workspace --all-targets -D warnings
just fmt          # cargo fmt --all        （CI 用 just fmt-check）
just doc          # cargo doc --workspace --no-deps --open
just book         # mdbook build docs      （book-serve 预览）
cargo run -p novella -- compile <file> -o <out.s>   # lexer → parser → JSON（tokens + program + diagnostics）

just lsp-build    # cargo build --release -p novella-lsp
just lsp-run      # cargo run -p novella-lsp（stdio 通道，等待编辑器连接）
just lsp-pack     # 打包 VS Code 扩展（esbuild 打包 + vsce 产出 vsix）
```

## NOTES

- 错误显示为 rustc 风格：`error: <msg>` + `--> 文件:行:列` + 源码行 + caret 指示，由 cli `render_diagnostic` 统一渲染；展示列号 1-based，caret 按字符渲染（多字节字符不错位）。
- `novella-codegen` 输出默认 `output.s`（CLI 默认值），当前实际写 JSON 文本。
- lexer 的 `TokenError` / `LexerResult` 已启用：`lexer()` 返回 token 流与错误列表，可恢复错误（非法字符、未闭合字符串）只进 `errors`，不中断后续分析；错误种类结构化（`LexError` 枚举），消息由 `Display` 生成，经 `TokenError::to_diagnostic` 转通用诊断。
- EOF token 的 span 指向输入末尾 `{start:len, end:len}`，line/column 为末尾处行列；行号 1-based、列号 0-based（字节偏移），对齐 ANTLR `getLine()` / `getCharPositionInLine()`。
- `TokenKind::Error` 保留为非法字符 / 未闭合字符串的占位（`to_token` 的 None 分支），随错误记录进 `errors`。
- **注释 token 化**：lexer 把 `// ...`（到行尾，不含换行）产出为 `TokenKind::LineComment`（此前由 `logos(skip)` 丢弃），故 CLI 的 JSON 输出 `tokens` 现包含注释 token；CLI 在构造 parser 前过滤注释，JSON 输出仍保留。若将来 parser 需要 doc comment，应改为 parser 内部吞注释并移除该过滤。
- **块注释未支持**：`/* ... */` 在本项目被刻意砍掉（文法中已无 `MultiLineComment`）；`/*` 目前会被切分为 `Divide` + `Multiply`。
- LSP 语义 token 的偏移单位为 **UTF-16 码元**（见 CONVENTIONS）；跨行 token 会被按行拆分为多个语义 token——当前词法下（行注释与字面量都不含换行）不会触发，属防御性实现。
- `FunctionItem.block` 类型为 `Option<Box<Statement>>`：`Box` 用于抑制 `Item` 枚举的尺寸差异（否则触发 clippy `large_enum_variant`）；类方法成员的 `block` 仍为 `Option<Block>`。
- VS Code 扩展以 esbuild 打包为自包含单文件（`dist/extension.js`），vsix 不携带 `node_modules`；`editors/vscode/pnpm-workspace.yaml` 中 `allowBuilds: esbuild: false` 是**必需**的——允许其 postinstall 会把 `bin/esbuild` 替换成 ELF 原生二进制，导致 pnpm 执行 scripts 时按 JS 解析而报语法错误。
- `novella-lsp` 与 `novella-cli` 同为二进制 crate，未登记进 `workspace.dependencies`。
- `Scope` 枚举拼写为 `Pubilc`（历史笔误），改名前需同步全部引用与 JSON 输出。
- `[profile.release]` 开启 `lto = true`。
- release build 是 LTO 全量链接，调试构建慢属预期。

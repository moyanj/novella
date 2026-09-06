# novella 项目知识库

**生成时间:** 2026-09-05 · **commit:** 351f7a6 · **分支:** main

## OVERVIEW

novella 语言编译器。Rust workspace（edition 2024，Apache-2.0，7 个 crate 流水线架构），ANTLR4 文法作语法参考。当前为早期阶段：**词法分析可用**，AST 部分落地，parser/sema/codegen 均为空壳占位。

## STRUCTURE

```
novella/
├── crates/
│   ├── novella-core/    # 共享基础：Span / Spanned<T> / TokenKind / Token
│   ├── novella-ast/     # AST 节点定义（expr / stmt / types / program）
│   ├── novella-lexer/   # 词法器（logos），RawToken → TokenKind 转换
│   ├── novella-parser/  # 空壳（占位 add()）
│   ├── novella-sema/    # 空壳（占位 add()）
│   ├── novella-codegen/ # 空壳（占位 add()）
│   └── novella-cli/     # 二进制（包名 novella）：clap CLI，仅 compile 子命令
├── grammar/             # ANTLR4 语法参考：NovellaLexer.g4 / NovellaParser.g4
├── Cargo.toml           # workspace（members = crates/*，resolver = 2）
└── Justfile             # 命令入口
```

依赖链（自上而下单向，禁止反向）：

```
core ← ast
core ← lexer ← parser ← sema ← codegen ← cli
```

## WHERE TO LOOK

| 任务 | 位置 | 备注 |
|------|------|------|
| 加/改词法单元 | `crates/novella-core/src/token.rs` + `crates/novella-lexer/src/lib.rs` | 双枚举必须同步（见 CONVENTIONS） |
| 加 AST 节点 | `crates/novella-ast/src/` | 按 `expr/stmt/types/program` 分类，参考文法产生式对照 |
| 诊断/错误显示 | `crates/novella-cli/src/error.rs` | 目前仅 `error_header` |
| CLI 子命令 | `crates/novella-cli/src/cli.rs` | clap derive |
| 语法设计参考 | `grammar/NovellaParser.g4` | 仅参考，未与代码对齐 |
| 占用位 crates | parser / sema / codegen | `pub fn add(u64, u64) -> u64` 待替换 |

## CODE MAP

| 符号 | 位置 | 说明 |
|------|------|------|
| `Span`, `Spanned<T>` | `novella-core/src/lib.rs` | 位置区间 / 携带位置的节点外包 |
| `TokenKind`（106 变体） | `novella-core/src/token.rs` | 词法单元种类，纯数据投影，带 `Display`（输出 Debug 文本） |
| `Token` | `novella-core/src/token.rs` | kind + span + slice + line + column |
| `RawToken` | `novella-lexer/src/lib.rs` | logos 私有枚举；`From<RawToken> for TokenKind` 穷尽映射 |
| `lexer(&str) -> Vec<Token>` | `novella-lexer/src/lib.rs` | 入口：产出含 EOF 的 token 流；收集到的错误仅入 `errors` 后丢弃 |
| `Expression` / `Literal` | `novella-ast/src/expr.rs` | 仅元组 + 字面量两类节点 |
| `Statement` / `IfStmt` / `ForStmt` | `novella-ast/src/stmt.rs` | 含 `BindingPattern`（标识符/元组解构递归） |
| `TypeExpr` / `PredefinedType` | `novella-ast/src/types.rs` | 类型表达式；`Param` 暂不承载默认值 |
| `Program` / `Item` / `Scope` | `novella-ast/src/program.rs` | 顶层 item；`Scope` 注意拼写 `Pubilc` |

## CONVENTIONS

- **全程简体中文**：注释、doc、commit、文档均中文。
- **TokenKind ↔ RawToken 双枚举**：`token.rs` 定义稳定公共种类（doc 注释标注对应字面量），`lexer` 的 `RawToken` 是 logos 专属投影，靠 `From` 的 match 穷尽性保证新增变体不漏转（漏了编译报错）。
- **下游不接触 logos**：parser 等阶段只见 `novella_core::token::Token`。
- **AST 注释对齐文法**：每个节点 doc 标注 `对应文法 xxx`（以 `grammar/*.g4` 产生式名为准）。
- **类型全量 serde**：AST / token 均派生 `Serialize, Deserialize`，用于 JSON 输出。
- **for 循环 init 语义**：文法中 init 段声明不带 `const/let`，统一按 `Let` 承载（见 `ForInit` 注释）。
- **值语义延后**：AST 字面量保留源码原文（含 `f/r/b`、进制前缀），解析在语义分析阶段。
- **空壳 crate 用占位**：parser/sema/codegen 目前允许 `add()` 占位。

## ANTI-PATTERNS（本项目）

- ❌ 把 `RawToken` / logos 类型泄漏到 core 或下游 crate——必须经 `TokenKind` 转换层。
- ❌ 只改 `RawToken` 不改 `TokenKind`（或反之）——两个枚举必须同步演进。
- ❌ 在 `token.rs` 之外维护并列的字面量/关键字集合——`TokenKind` 是唯一事实源。
- ❌ 把 `grammar/.antlr` 生成产物（`.java`/.interp/.tokens）提交——已 gitignore。
- ❌ 把 `grammar/*.g4` 当作已生效文法——README 明示未对齐，改动前先核对代码现状。
- ❌ 修改 `Span`/`Token` 字段时不更新 JSON 消费者（cli 输出 `serde_json`）。

## COMMANDS

```bash
just check        # cargo check --workspace --all-targets
just build        # cargo build --workspace
just test         # cargo test --workspace
just lint         # clippy --workspace --all-targets -D warnings
just fmt          # cargo fmt --all        （CI 用 just fmt-check）
just doc          # cargo doc --workspace --no-deps --open
just book         # mdbook build docs      （book-serve 预览）
cargo run -p novella -- compile <file> -o <out.s>   # lexer → JSON
```

## NOTES

- `novella-codegen` 输出默认 `output.s`（CLI 默认值），当前实际写 JSON 文本。
- lexer 的 `TokenError` / `LexerResult` 已定义但未使用：`lexer()` 只返回 `Vec<Token>`，错误静默丢弃——待诊断设计时接管。
- EOF token 的 span 固定 `{start:0, end:0}`，line/column 为 0。
- `TokenKind::Error` 保留为非法字符占位（`to_token` 的 None 分支），但同样被丢弃。
- `Scope` 枚举拼写为 `Pubilc`（历史笔误），改名前需同步全部引用与 JSON 输出。
- `workspace.dependencies` 未登记 `novella-ast` 与 `novella-cli`：前者自带 path 依赖，后者是二进制 crate 无此需要。
- `[profile.release]` 开启 `lto = true`。
- release build 是 LTO 全量链接，调试构建慢属预期。
# novella 项目知识库

**生成时间:** 2026-11-14 · **commit:** cedd6b6（文件划分与 API 可见性重构后）· **分支:** main

## OVERVIEW

novella 语言编译器。Rust workspace（edition 2024，Apache-2.0），ANTLR4 文法与代码对齐（词法↔`TokenKind`，语法↔AST）。sema 覆盖导入、名称解析与类型检查（详见 `docs/sema-coverage.md`）。HIR 和 LLVM 后端可将标量程序编译为 Linux x86-64 原生程序，最小示例见 `examples/native-main.nov`。

## STRUCTURE

```
novella/
├── crates/
│   ├── novella-core/    # 共享基础：Span / Spanned<T> / TokenKind / Token / Diagnostic
│   │                    #   + line_index（行列）/ path（模块路径规范化）/ decor（诊断装饰）
│   ├── novella-derive/  # proc-macro：#[derive(Spanned)] 等 AST 派生宏
│   ├── novella-ast/     # AST 节点定义（expr/ 目录 / stmt / types / program）
│   ├── novella-lexer/   # 词法器（logos），RawToken → TokenKind 转换
│   ├── novella-parser/  # 手写递归下降解析器（含 token 预处理与上下文消歧）
│   ├── novella-sema/    # 语义分析：module/ + resolve + symbol + ty + check/ + typecheck/
│   ├── novella-hir/     # 高层中间表示：sema 结果 → 定型指令流（ty/body/item/build）
│   ├── novella-codegen/ # LLVM 目标文件发射，仅消费 HIR
│   ├── novella-cli/     # 二进制（包名 novella）：check、compile、导出与链接
│   ├── novella-lsp/     # 语言服务器（lib + 二进制）：outline/ + features/ + server/
│   └── novella-syntax-gen/ # TextMate 语法生成器（TokenKind → editors/vscode 的 tmLanguage）
├── editors/vscode/      # VS Code 扩展（esbuild 打包；启动 novella-lsp，不含语法规则）
├── grammar/             # ANTLR4 文法（以代码为基准对齐）：NovellaLexer.g4 / NovellaParser.g4
├── Cargo.toml           # workspace（members = crates/*，resolver = 2）
└── Justfile             # 命令入口
```

依赖链（自上而下单向，禁止反向）：

```
core ← ast ← parser ← sema ← hir ← codegen ← cli
core ← lexer ← parser
core ← lexer ← novella-lsp（独立二进制，不经 cli）
ast ──(构建期)── novella-derive（proc-macro，展开引用 ::novella_core::Span）
```

## WHERE TO LOOK

| 任务 | 位置 | 备注 |
|------|------|------|
| 加/改词法单元 | `crates/novella-core/src/token.rs` + `crates/novella-lexer/src/lib.rs` | 双枚举必须同步（见 CONVENTIONS） |
| 加/改语法结构 | `crates/novella-parser/src/`（lib/stmt/expression/ 目录/types）+ `grammar/NovellaParser.g4` | 分层：`lib.rs` 顶层 item、`stmt.rs` 语句、`expression/` 表达式优先级、`types.rs` 类型 |
| 加 AST 节点 | `crates/novella-ast/src/` | 按 `expr/`（mod/literal/ops/lambda/match_node）、`stmt`/`types`/`program` 分类，参考文法产生式对照；新节点须携带 `span` 字段并 `#[derive(Spanned)]`（见 CONVENTIONS） |
| 诊断/错误显示 | `crates/novella-cli/src/error.rs` + `crates/novella-core/src/diagnostic.rs` | 通用诊断结构（各阶段产出）→ `render_diagnostic` rustc 风格渲染 |
| CLI 子命令 | `crates/novella-cli/src/cli.rs` | clap derive |
46	| 文法（ANTLR4） | `grammar/NovellaLexer.g4` + `grammar/NovellaParser.g4` | 以代码为基准对齐：词法↔`TokenKind`，语法↔AST 节点；改动代码后须同步（见 `grammar/README.md`） |
| 编辑器语法高亮 | `crates/novella-lsp/src/` + `editors/vscode/` | 公开面仅 `server::run`；`semantic.rs` 类别映射与 UTF-16 编码，`protocol.rs` 报文帧，`server/dispatch.rs` 分发 |
| 加一项 LSP 能力 | `crates/novella-lsp/src/features/`（转换层）+ `server/syntax.rs` 或 `server/lexical.rs`（挂到分发） | `features/` 按能力一文件；`server/` 只做参数解码与 JSON 包装 |
| 调整高亮类别 | `crates/novella-lsp/src/semantic.rs` | `token_type_index` 为穷尽 match；改动映射须同步 `TOKEN_TYPES`（数组顺序即 legend 下标） |
| 打包 VS Code 扩展 | `editors/vscode/` | `just lsp-pack`；源码 `src/extension.js` 经 esbuild 打入 `dist/`，vsix 自包含无需 node_modules |
| 原生后端 | `crates/novella-codegen/src/` | `emit_object` 只消费 HIR；目标为 Linux x86-64 |
| HIR 降级入口 | `crates/novella-hir/src/build.rs`（`build_hir`） | AST → `HirProgram`；类型取自 `sema.expression_types` / `annotation_types` / `declaration_types`，无法定型即 `BuildError` |
| 语义分析入口 | `crates/novella-sema/src/lib.rs`（`analyze` / `analyze_with`） | 五阶段管线：模块加载→声明收集→导入绑定→类型注册→逐模块检查；实现在 `module/`（mod/loader/resolve_path）、`resolve.rs`、`symbol.rs`、`ty.rs`、`check/`（mod/type_defs/decl）、`typecheck/`（mod/body/expr/member/call/unsafe_rules）。**词法/语法只在 loader 内各跑一次**，产物经 `SemaResult.modules`（`ModuleOutput`）交还下游 |
| sema 规则覆盖状态 | `docs/sema-coverage.md` | 唯一权威矩阵：每条 design 规则 / 每个 AST 变体一行，标注 已实现/部分/缺失/待决策；补全计划见 `docs/sema-completion-plan.md`，`docs/sema-phase1.md` 为历史计划 |

## CODE MAP

| 符号 | 位置 | 说明 |
|------|------|------|
| `Span`, `Spanned<T>` | `novella-core/src/lib.rs` | 源码位置区间（字节偏移）/ 通用位置包装（AST 已改为节点内嵌 span，`Spanned` 暂保留备用） |
| `Spanned`（derive 宏） | `novella-derive/src/lib.rs` | 为携带 `span` 字段的节点生成 `span()` 方法；struct 取字段、enum 具名变体取字段、newtype 变体委托内部节点 |
| `AnalyzedModule` / `ModuleOutput` | `novella-sema/src/lib.rs` | 每模块产物（`id`/`path`/`source`/`tokens`/`program`/`syntax_failed`/`load_failed`）；`get`/`entry`/`find_by_path`/`find_entry`（路径两侧经 `module_key` 规范化） |
| `TypeView` / `ExpressionTypes` | `novella-sema/src/lib.rs`（投影在 `ty.rs` 的 `Ty::view`） | 表达式类型表，键 `(ModuleId, Span)`；`Unknown` 与 `Error` 分别投影为 `TypeView::Unknown` / `TypeView::Error`，External 已删除，函数类型投影为 `TypeView::Function(Box<FunctionView>)`（含 `params`/`ret`/`receiver`，不再因 `name: None` 退化） |
| `Diagnostic` / `Severity` / `DiagnosticLocation` | `novella-core/src/diagnostic.rs` | 与阶段无关的通用诊断结构（serde），各阶段产出、CLI 渲染 |
| `TokenKind` | `novella-core/src/token.rs` | 词法单元种类，纯数据投影，带 `Display`（输出 Debug 文本）；含 `LineComment`（行注释） |
| `Token` | `novella-core/src/token.rs` | kind + span + slice + line + column |
| `RawToken` | `novella-lexer/src/lib.rs` | logos 私有枚举；`From<RawToken> for TokenKind` 穷尽映射 |
| `lexer(&str) -> LexerResult` | `novella-lexer/src/lib.rs` | 入口：产出含 EOF 的 token 流与词法错误列表；错误仅入 `errors` |
| `Expression` / `Literal` | `novella-ast/src/expr/`（mod/literal/ops/lambda/match_node） | 完整表达式节点集（字面量/标识符/括号/元组/数组/对象/this/一元/二元/赋值/成员/索引/调用/构造/await/is/三元/lambda/match），运算符由 `UnaryOp`/`BinaryOp`/`AssignOp` 集中定义 |
| `Statement` / `IfStmt` / `ForStmt` | `novella-ast/src/stmt.rs` | 含 `BindingPattern`（标识符/元组解构递归）、`Continue`；`TryStmt` / `WithStmt` 亦在此 |
| `TypeExpr` / `PredefinedType` | `novella-ast/src/types.rs` | 类型表达式（Predefined/Reference/Array/Tuple/Function/Union）；`Param` 含 `default` 默认值 |
| `Program` / `Item` / `Scope` | `novella-ast/src/program.rs` | 顶层 item；`Scope` = `Public` / `Private` / `Package`（`from_token` 认 `Pub` / `Pri` / `Pkg`） |
| `Parser::parse` / `Parser::parse_item` | `novella-parser/src/lib.rs` | 入口：出 `ParseResult { program, diagnostics, aborted }`；顶层按行同步恢复 |
| `preprocess` | `novella-parser/src/preprocess.rs` | 喂给语法分析前的 token 重写：`from "m" import` 的 `import` 降级为标识符（类型位箭头唯一为 `->`，类型位遇 `=>` 由类型解析就近报错，不再做 token 改判） |
| `parse_expression` 优先级链 | `novella-parser/src/expression/`（stairs/postfix/primary/lambda/match_expr/helper） | 赋值→三目→`??`→`or`→`and`→位或→异或→与→相等→关系→移位→`as`→加减→乘除→`is`→一元→后缀→primary |
| `parse_statement` / `parse_block` | `novella-parser/src/stmt.rs` | 语句族；块是恢复点，失败调 `recover_to_statement_boundary` |
| `parse_type_expr` | `novella-parser/src/types.rs` | 联合 / 元组 / 数组 / 指针 / 函数 / 箭头函数类型；类型名一律 `Reference`（`Predefined` 留给 sema） |
| `token_type_index` / `encode` | `novella-lsp/src/semantic.rs` | `TokenKind` → legend 下标（穷尽 match）/ token 流 → semantic tokens（字节偏移换算为 UTF-16 码元） |
| `Server::handle` | `novella-lsp/src/server/dispatch.rs` | JSON-RPC 报文分发：请求必回复、通知不回复 |
| `LineIndex` | `novella-core/src/line_index.rs` | 字节偏移 ⇄ (1-based 行, 0-based 字节列)，全项目唯一实现（lexer / sema / LSP 共用） |
| `normalize_path` / `module_key` | `novella-core/src/path.rs` | 模块路径词法归一化 / 模块唯一键（canonicalize 失败退回归一化） |
| `decorate` / `shift_span_in_place` | `novella-core/src/decor.rs` | 诊断位置（file/line/column）填充与 span 平移 |
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
- **后端边界**：无法发射的 HIR 报错，不做静默兜底。
- **sema 诊断定位**：`Module::error/warning` 由 span 经 `LineIndex` 回推行列；全部诊断唯一存储于 `DiagnosticReport.modules`；`ModuleDiagnostics` 保留真实 `syntax_diagnostic_count`，通过 `syntax_diagnostics()` / `semantic_diagnostics()` 分阶段读取，通过 `ordered()` 统一排序。
- **注释在语法层透明**：`LineComment` 是正式 token（供高亮等消费），由 `novella-parser/src/preprocess.rs` 的 `retain` **一处**过滤；`Module.tokens` 保留完整流（含注释），sema/loader 不得再过滤一次。parser 不感知注释。
- **LSP 偏移一律 UTF-16**：novella 内部用字节偏移，LSP 语义 token 要求 UTF-16 码元；换算集中在 `novella-lsp/src/semantic.rs`，新增 LSP 能力时须复用该换算而不要另写一套。
- **高亮规则不重复维护**：编辑器的染色类别映射以 `TokenKind` 为唯一来源，禁止在插件侧另写词法/关键字表。
- **位置换算只在 core**：行列换算用 `novella_core::line_index::LineIndex`，模块路径用 `novella_core::path::module_key`，诊断填空用 `novella_core::decor`——三个 crate 不得各写一份（LSP 的 UTF-16 列在其上叠加，见 `novella-lsp/src/source.rs`）。
- **crate 公开面最小化**：sema 只导出 `analyze` / `analyze_with` / `SemaResult`（含 `DiagnosticReport` / `BindingTable` / `TypeHierarchy`）与其值类型、加载器；LSP 只导出 `server::run`。新增 `pub` 前先确认有 crate 外调用方，否则 `pub(crate)`。
- **大文件按职责拆目录**：单文件超约 600 行即拆成 `foo/mod.rs` + 若干子模块；`mod.rs` 只放类型定义、模块声明与重导出。

## ANTI-PATTERNS（本项目）

- ❌ 把 `RawToken` / logos 类型泄漏到 core 或下游 crate——必须经 `TokenKind` 转换层。
- ❌ 只改 `RawToken` 不改 `TokenKind`（或反之）——两个枚举必须同步演进。
- ❌ 在 `token.rs` 之外维护并列的字面量/关键字集合——`TokenKind` 是唯一事实源。
- ❌ 在 `novella-lsp` 或 VS Code 插件里重复维护词法/关键字规则——高亮必须复用 `novella-lexer`。
- ❌ 输出 LSP 语义 token 时直接用字节偏移——含中文/emoji 的行会整体错位，必须换算为 UTF-16 码元。
- ❌ 把 `grammar/.antlr` 生成产物（`.java`/.interp/.tokens）提交——已 gitignore。
- ❌ 只改代码不同步 `grammar/*.g4`——文法以代码为基准，`TokenKind` / AST 变更后必须同步；反之也不得凭文法去要求代码。
- ❌ 修改 `Span`/`Token` 字段时不更新 JSON 消费者（cli 输出 `serde_json`）。
- ❌ 在 lexer / sema / LSP 里各写一份行表或路径归一化——必须复用 `novella-core`。
- ❌ 下游（CLI / codegen）自己再跑一遍 lexer / parser——`analyze` 已把语法产物放进 `SemaResult.modules`，重跑会让入口文件解析两次并产生两套诊断。
- ❌ 在 `preprocess.rs` 之外再过滤注释 token——注释过滤只有一处。
- ❌ 因为「方便测试」或「以后可能用」就把内部模块设成 `pub mod`——公开面即兼容承诺。
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
cargo run -p novella -- compile examples/native-main.nov   # 单次 analyze → HIR → LLVM → a.out
cargo run -p novella -- compile <file> --emit ast -o <out.json>   # tokens + program + diagnostics

just lsp-build    # cargo build --release -p novella-lsp
just lsp-run      # cargo run -p novella-lsp（stdio 通道，等待编辑器连接）
just lsp-pack     # 打包 VS Code 扩展（esbuild 打包 + vsce 产出 vsix）
```

## NOTES

- 错误显示为 rustc 风格：`error: <msg>` + `--> 文件:行:列` + 源码行 + caret 指示，由 cli `render_diagnostic` 统一渲染；展示列号 1-based，caret 按字符渲染（多字节字符不错位）。
- CLI 默认链接生成 `a.out`；`--emit obj` 写 ELF 目标文件，`--emit ast` / `--emit hir` 写 JSON。
- lexer 的 `TokenError` / `LexerResult` 已启用：`lexer()` 返回 token 流与错误列表，可恢复错误（非法字符、未闭合字符串）只进 `errors`，不中断后续分析；错误种类结构化（`LexError` 枚举），消息由 `Display` 生成，经 `TokenError::to_diagnostic` 转通用诊断。
- EOF token 的 span 指向输入末尾 `{start:len, end:len}`，line/column 为末尾处行列；行号 1-based、列号 0-based（字节偏移），对齐 ANTLR `getLine()` / `getCharPositionInLine()`。
- `TokenKind::Error` 保留为非法字符 / 未闭合字符串的占位（`to_token` 的 None 分支），随错误记录进 `errors`。
- **注释 token 化**：lexer 把 `// ...`（到行尾，不含换行）产出为 `TokenKind::LineComment`（此前由 `logos(skip)` 丢弃），故 CLI 的 JSON 输出 `tokens` 现包含注释 token；sema 的 loader 保存完整 token 流，filter 只在 `preprocess.rs` 一处完成。若将来 parser 需要 doc comment，应改为 parser 内部吞注释并移除该过滤。
- **块注释未支持**：`/* ... */` 在本项目被刻意砍掉（文法中已无 `MultiLineComment`）；`/*` 目前会被切分为 `Divide` + `Multiply`。
- **`Symbol.name_span` 与 `Symbol.span` 的区别**：`span` 覆盖整条声明（含函数体），`name_span` 只覆盖标识符；`Definition.span` 与类型摘要的定位一律取 `name_span`，否则编辑器跳转/重命名会选中 `() -> int { ... }` 而不是名字。
- LSP 语义 token 的偏移单位为 **UTF-16 码元**（见 CONVENTIONS）；跨行 token 会被按行拆分为多个语义 token——当前词法下（行注释与字面量都不含换行）不会触发，属防御性实现。
- `FunctionItem.block` 类型为 `Option<Box<Statement>>`：`Box` 用于抑制 `Item` 枚举的尺寸差异（否则触发 clippy `large_enum_variant`）；类方法成员的 `block` 仍为 `Option<Block>`。
- VS Code 扩展以 esbuild 打包为自包含单文件（`dist/extension.js`），vsix 不携带 `node_modules`；`editors/vscode/pnpm-workspace.yaml` 中 `allowBuilds: esbuild: false` 是**必需**的——允许其 postinstall 会把 `bin/esbuild` 替换成 ELF 原生二进制，导致 pnpm 执行 scripts 时按 JS 解析而报语法错误。
- `novella-lsp` 与 `novella-cli` 同为二进制 crate，未登记进 `workspace.dependencies`。
- `Scope` 枚举已按 `Public` / `Private` / `Package` 拼写，token 侧仍是 `Pub` / `Pri` / `Pkg`，转换只在 `Scope::from_token` 一处。
- **parser 递归防护**：原先 `MAX_NESTING` / `MAX_EXPRESSION_NESTING`（均 128）已删除，改为**按需扩展分段栈**（`rust-lang/stacker`）。所有递归入口（`parse_expression` / `parse_assignment` / `parse_ternary` / `parse_unary` / `parse_primary` / `parse_type_expr` / `parse_block` / `parse_statement` / `parse_binding_pattern` / `parse_match`）一律写成「守卫包装 + `_inner` 实体」两层，包装层调用 `self.guarded(...)`（见 `crates/novella-parser/src/recursion.rs`）：剩余栈逼近红区（`RED_ZONE`）时由 `stacker::maybe_grow` 切到堆上新分段（`STACK_SEGMENT`）。合法的深层输入（数万层括号 / 嵌套块）可正常解析；只有病态输入耗尽 `MAX_STACK_SEGMENTS`（`1 << 20`，按**守卫调用次数**计）才报「语法嵌套层级超出工程上限」。注意 `maybe_grow` 参数顺序是 `(red_zone, stack_size)`。`checkpoint()/rollback()` 保存/恢复递归预算，避免推测分支消耗配额。
- **AST 析构同样需要分段栈**：`Expression` / `Statement` / `TypeExpr` 是深层 `Box` 递归链，编译器默认析构会逐层下钻把栈打穿（实测 2000 层嵌套 `unsafe` 块即 abort）。因此三者在 `crates/novella-ast/src/drop_guard.rs` 中实现自定义 `Drop`：每层只在 `with_stack` 段内摘取**直接子节点**放进局部 `Vec`，再逐个释放，使析构深度由「真实栈深度」变成「分段栈切换次数」。**新增 `Box<...>` / `Vec<...>` 递归字段（含 `IfStmt` / `ForStmt` / `TryStmt` / `Match` / `Lambda` 等中间结构里的）必须同步该模块**——其中的 `match` 穷尽所有变体、不留 `_` 兜底，漏改会编译失败而不是静默退回递归析构。
- **语法歧义的消解位置**：`with expr as binding` 的 `as` 靠 `with_stack` + `with_shield` + `looks_like_with_binding()` 判定（表头裸位绑定优先，嵌套括号/实参内仍是 cast；with 表头为无括号形态，旧 `with (expr)` 写法报迁移诊断），不走全局猜测；`match subject {` 的 `{` 靠 `in_match_subject` 禁止被 StructInit 抢走；match 分支体靠 `inside_match` 让表达式解析在 `case` / `default` / `}` 处自然停下。新增同类歧义应复用这些上下文标记，不要在 `parse_primary` 里堆特判。
- `[profile.release]` 开启 `lto = true`。
- release build 是 LTO 全量链接，调试构建慢属预期。
- **诊断只有一份**：JSON 的 `diagnostics` 是唯一诊断树（`{ global, modules: [{ id, path, diagnostics }] }`），每模块内词法/语法/语义诊断按 `location.span.start` 合并排序；禁止再加 `sema_diagnostics` 之类的并列字段，否则同一错误会出现两次。
- **入口路径语义**：`Module` 同时持 `key`（`module_key` 规范化，用于去重/overlay/crate 根查找/相对导入解析基准）与 `path`（调用方原始路径，用于诊断回显）；诊断的 `location.file` 与 `sema_diagnostics[].path` 一律用 `path`，因此 CLI 输出里看到的是命令行传入的写法（如 `test.nov`）而不是规范化绝对路径。
- **表达式类型登记**：`typecheck/expr.rs` 的 `check_expr` 是登记 `Sema::expression_types` 的唯一出口（`check_expr` 包一层记录后调 `check_expr_inner`）；`check_expr_with_hint` 的两个提示分支（空数组 / `null`→标注）绕过该出口，必须另行补记。新增表达式分支无需改动登记逻辑。
- **函数签名名义键**：`ty::FunctionSig` 有 `module` + `name: Option<String>`；顶层函数使用原名，方法在 `build_method_sig` 中使用 `类型::方法`，lambda 与占位签名保持 `None`。`name` 用于合成 HIR 符号键，**不决定类型投影**：`Ty::view()` 对任何 `FunctionSig` 都产出 `TypeView::Function(Box<FunctionView>)`。匿名签名键不能直接作为 lambda 实体的唯一键（实体键使用模块与源码位置，见 `docs/hir-handover.md`）。
- **实例方法的 `self`**：`build_method_sig` 将实例方法首个无注解的 `self` 定型为所属 Struct / Class，并置 `is_method`；`FunctionSig::view()` 跳过该形参并置 `FunctionView::receiver = true`。HIR 成员访问统一查表达式类型表，不用局部类型兜底。
- **HIR 降级边界**：实现按职责拆在 novella-hir/src/build/；条件使用 HirCond，循环 prelude 每轮执行，Loop.step 在正常尾部和 continue 后执行，break/return 跳过。Binary 必须携带两侧类型，TypeTest 必须携带输入类型；复杂左值先定位一次，复合赋值先读旧值再求 RHS。struct 接收者槽位为 Pointer(Struct)。默认实参与字段初值在定义环境展开；?? 保留非空指针而不解引用。入口拒绝任何阶段错误；运行时未定义的异常、挂起、动态派发等明确 fail-fast。覆盖与验证见 docs/hir-handover.md。

## 2026-11-15 sema 补全后的定位与契约

- 当前覆盖以 docs/sema-coverage.md 为准，sema-phase1.md 仅为历史。已确认决策与
  本轮跨层授权不再重复询问；宏能力已删除；@cfg 已实现，其他未定义指令/yield 明确 error，不以 warning 放行。
- sema 的公开数据在 api.rs，阶段编排在 analysis.rs，内部状态在 state.rs；
  ty/、resolve/、check/type_defs/、typecheck/expr/、body/、unsafe_rules/ 按职责拆分。
  公开入口仍从 novella_sema 根导出，新增 analyze_with_target 接收 32/64 位配置。
- Applied/Future/Object 的公开类型身份不得擦除；类型参数含模块与声明位置。
  SemaResult.exhaustive_matches 供返回路径与 HIR 共用，别名定义位置保留符号类型。
- FieldMember/MethodMember.name_span 仅覆盖成员名字；StructInit.type_args 保留显式
  实参；Throw.expr 为 Option；AST/serde 的消费者须同步。
- 普通赋值 RHS→定位 LHS→写回；复合赋值定位→读旧值→RHS→写回。sema 与 HIR
  均遵循该顺序。HIR 整数常量为 i128 载荷，保留 u64 最大值。
- AST GuardedClone 与既有 Drop 共用分段栈；新增递归字段仍须同步 clone/drop。
  try/finally 的确定赋值同时追踪正常、返回、循环跳转与调用可能抛异常的状态。

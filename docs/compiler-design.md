# Novella 编译器设计

> 版本：1.2 · 状态：与代码对齐的现行实现设计 · 基准 commit：cedd6b6（文件划分与 API 可见性重构后）
>
> 本文从**编译器视角**描述 novella 编译器：crate 架构、编译管线、词法/语法/
> 语义/代码生成各阶段的实现机制、诊断渲染、LSP 集成与「代码 ↔ 文法 ↔ 高亮」
> 三重对齐机制。语言本身的语法与语义规则（「语言长什么样」）见
> **[`design.md`](design.md)**——编译器实现的是该文档定义的规则，两侧以
> **代码为事实源**。

---

## 1. 编译管线总览

```
源文件
  │
  ▼
┌─────────────────────────────────────────────────────────────┐
│ novella-lexer   logos 规则 + LineIndex 行列定位              │
│ → LexerResult { tokens: Vec<Token>, errors: Vec<TokenError> }│
└─────────────────────────────────────────────────────────────┘
  │  注释在喂给 parser 前被过滤（JSON 输出仍保留）
  ▼
┌─────────────────────────────────────────────────────────────┐
│ novella-parser  手写递归下降（骨架逐条实现中）                │
│ → ParseResult { program: Program, diagnostics: Vec<Diagnostic> } │
└─────────────────────────────────────────────────────────────┘
  │
  ▼
┌─────────────────────────────────────────────────────────────┐
│ novella-sema    类型检查 / unsafe 上下文 / 路径别名 / main 判定 │
│ → SemaResult { diagnostics, bindings, types, modules,        │
│                expression_types }                            │
└─────────────────────────────────────────────────────────────┘
  │
  ▼
┌─────────────────────────────────────────────────────────────┐
│ novella-hir     AST → 定型指令流（类型取自 expression_types） │
│ → HirProgram（fail-fast：无法定型的构造直接报错）             │
└─────────────────────────────────────────────────────────────┘
  │
  ▼
┌─────────────────────────────────────────────────────────────┐
│ novella-codegen LLVM（inkwell）目标代码（x86-64 Linux ELF） │
└─────────────────────────────────────────────────────────────┘
  │
  ▼
CLI（novella-cli）：render_diagnostic（rustc 风格）+ JSON 输出
LSP（novella-lsp）：并入词法阶段做语义高亮（独立二进制，不经 cli）
```

阶段错误均可恢复：词法错误进 `errors`、语法错误进 `diagnostics`，均不中断同
一文件的后续分析；CLI 最终统一渲染全部诊断。

## 2. Crate 架构与依赖链

Rust workspace（edition 2024，8 个 crate + LSP 独立二进制）：

```
crates/
├── novella-core/    # 共享基础：Span / Spanned<T> / Token / TokenKind / Diagnostic
│                   #   + line_index.rs / path.rs / decor.rs（位置原语，全项目唯一实现）
├── novella-derive/  # proc-macro：#[derive(Spanned)]（AST 构建期依赖）
├── novella-ast/     # AST 节点（expr/ 目录 / stmt / types / program）
├── novella-lexer/   # 词法器（logos），RawToken → TokenKind
├── novella-parser/  # 手写递归下降（expression/ 目录 + lib/stmt/types）
├── novella-sema/    # 语义分析（module/ + resolve/ + symbol + ty/ + check/ + typecheck/）
├── novella-hir/     # 高层中间表示：sema 结果 → 定型指令流（ty/body/item/build）
├── novella-codegen/ # 代码生成（经 inkwell 消费 HIR）
├── novella-cli/     # 二进制 novella：clap CLI，compile 子命令
├── novella-lsp/     # 语言服务器（outline/ + features/ + server/），公开面仅 server::run
└── novella-syntax-gen/ # TextMate 语法生成器（TokenKind → VS Code 扩展）
```

依赖（自上而下单向，禁止反向）：

```
core ← ast ← parser ← sema ← hir ← codegen ← cli
core ← lexer ← parser
core ← lexer ← novella-lsp（独立二进制，不经 cli）
ast ──(构建期)── novella-derive（proc-macro，展开引用 ::novella_core::Span）
```

关键工程约束：

- 下游（parser / sema / lsp）**不得接触 logos** 类型——一律经
  `TokenKind` 转换层；
- `novella-lsp` 与 `novella-cli` 同为二进制 crate，未登记进
  `workspace.dependencies`；
- `[profile.release]` 开启 `lto = true`，release 全量链接较慢属预期。

## 3. 词法分析（novella-lexer / novella-core）

### 3.1 双层枚举：RawToken ↔ TokenKind

- `RawToken`（`novella-lexer`）是 logos 私有投影，**只存在于 lexer crate**；
  105 个业务变体与 `TokenKind`（`novella-core`）一一对应（`TokenKind` 另含
  合成项 `Eof`/`Error`，合计 107，见 §12）；
- `impl From<RawToken> for TokenKind` 用 match 穷尽性保证：新增 lexer 变体时
  漏转即编译报错；
- parser / sema / LSP 只消费 `novella_core::token::Token`（`{ kind, span,
  slice, line, column }`），不被 logos 绑定。

### 3.2 匹配策略与错误通道

- logos 与 ANTLR 同为「最长匹配优先，同长取声明在前」；`**`（幂）等多字符
  规则先于单字符声明；
- 空白 / 换行经 `#[logos(skip ...)]` 丢弃；`LineComment` 是正式 token
  （供高亮消费），parser 前由调用方过滤；
- 词法错误（`LexError::InvalidCharacter` / `UnterminatedString`）只进
  `LexerResult.errors`，错误 token **不进** `tokens` 流；
- EOF token：span 指向输入末尾 `{start: len, end: len}`，行列按末尾处；
  行号 1-based、列号 0-based（字节偏移），对齐 ANTLR `getLine()` /
  `getCharPositionInLine()`。

### 3.3 行列定位

`LineIndex` 一次性扫描建立每行起始偏移表，`locate()` 经
`partition_point` 在 O(log n) 内定位行列，避免逐 token 全量重扫的 O(n²)。

### 3.4 已确认变更

- 新增关键字 `unsafe`、`struct`：`RawToken` 规则 + `From` 映射；
- 删除 `**`（Power）与 `**=`（PowerAssign）：两个规则 + 两个映射一并移除，
  消除「前置 `**` 需拆两层解引用」的特例（详见 `design.md` §3.4 说明）。

## 4. 语法分析（novella-parser）

### 4.1 机制

手写递归下降，结构：

- `Parser { tokens: Vec<Token>, pos: usize, diagnostics: Vec<Diagnostic> }`；
- 游标原语：`peek` / `peek_n` / `advance` / `at_eof`（消费后不回头）；
- token 判定：`check`（当前）/ `check_n`（第 n 个，暂未用）/ `eat`（命中消费）/
  `expect`（未命中记录「expected {kind}」诊断并返回 None）；
- 诊断：`error_here`（当前 token 处）/ `error_at`（回指指定 token），均为
  可恢复错误——`parse_item` 失败后继续解析后续 item；
- 恢复辅助：`skip_to_statement_end`（跳到 `;`）/ `skip_to_function_end`
  （跳过到匹配的 `}` 或 `;`），占位实现用于避免误导性连锁报错。

### 4.2 当前状态

文法中的全部产生式均可产出 AST；声明、表达式、类型与语句均已实现解析，
不再有 parse_xxx 占位入口。恢复点位于顶层 item 与语句块，递归入口按需扩展
分段栈。语法可解析不代表静态或运行时已支持，具体边界见覆盖矩阵。

### 4.3 已确认落地任务

- **表达式层**：一元前缀新增 `&`（`BitAnd`）→ `UnaryOp::AddressOf`、
  `*`（`Multiply`）→ `UnaryOp::DerefPointer`；`**p` 在词法层为两个 `*`，
  **无特殊拆分逻辑**；新增 `as` 分支（`TokenKind::As`，位于加减之后、移位
  之前）→ `Expression::Cast`；
- **类型层**：`parse_type_expr` 支持后缀 `*` 循环构建 `TypeExpr::Pointer`
  （`u8**` = 两个 `Multiply`，天然两层）；
- **语句层**：`parse_statement` 增加 `unsafe { }` 分支 →
  `Statement::Unsafe { block, span }`；
- **声明层**：`parse_item` 增加 `struct` 分支 →
  `Item::Struct(StructItem)`；struct 构造 `TypeName{ ... }` 在表达式层识别为
  `Expression::StructInit`（复用 objectLiteral 字段形态）；
- 校验约束同步设计：`pub` 修饰范围增加 `struct`；指令可修饰范围增加
  `struct`（design.md §4.2）。

## 5. AST 约定（novella-ast / novella-derive）

- **全量挂 span**：除纯值枚举（运算符、`Literal`、`PredefinedType`、`Scope`）
  外每个 AST 节点携带 `span` 字段；
- `#[derive(Spanned)]`（`novella-derive` proc-macro）：struct 取 `span` 字段、
  enum 具名变体取 `span` 字段、单字段 newtype 变体委托内部节点，生成统一的
  `span()` 方法；
- **全量 serde**（`Serialize, Deserialize`）：AST 与 token 均为 JSON 输出
  服务；
- **尺寸控制**：`FunctionItem.block: Option<Box<Statement>>`、
  `Statement` 递归字段用 `Box`，避免 clippy `large-enum-variant`；
- **注释文档对齐文法**：每个节点 doc 标注对应文法产生式名（以 g4 为准），
  两侧不一致以代码为准。

### 5.1 已确认 AST 变更

| 位置 | 变更 |
|------|------|
| `types.rs` | 新增 `TypeExpr::Pointer { element: Box<TypeExpr>, span }` |
| `expr.rs` | `UnaryOp` 增 `AddressOf` / `DerefPointer`；`Expression` 增 `Cast { expr, ty, span }` 及 struct 构造节点；删 `BinaryOp::Power`、`AssignOp::PowerAssign` |
| `stmt.rs` | `Statement` 增 `Unsafe { block: Box<Block>, span }` |
| `program.rs` | `Item` 增 `Struct(StructItem)` |

## 6. 语义分析（novella-sema）

现状与验收以 [sema-coverage.md](sema-coverage.md) 为准；
analysis.rs 编排模块加载 → collect/imports → 类型签名注册 → 继承/trait →
显式顶层注解 → 字段/顶层声明依赖定型 → 函数体检查 → 泛型约束复核。
api.rs 保存公开契约，state.rs 保存内部状态；ty/、resolve/、check/type_defs/
与 typecheck/ 的私有子模块按职责拆分，原有 crate 依赖方向保持不变。

**公开面**（crate 外可引用）只有三族：输入（`analyze` / `analyze_with` / `analyze_with_target`、
`FileLoader` / `FsLoader` / `OverlayLoader`）、输出（`SemaResult`，内含
`DiagnosticReport` / `BindingTable` / `TypeHierarchy` / `ModuleOutput` /
`ExpressionTypes` 五个只读视图）、值类型（`Definition` / `DefinitionKind` /
`TypeSummary` / `TypeSummaryKind` / `ModuleDiagnostics` / `ModuleId` /
`AnalyzedModule` / `TypeView`）。符号表、作用域树、类型注册表与全部检查
函数均为 `pub(crate)`。

**词法与语法只做一次**：`lexer` / `parser` 只在 `analyze` 内部对每个模块各
调用一次（`module/loader.rs`），产物经 `ModuleOutput` 交还下游。下游（CLI、
codegen）**不得自行再跑 lexer / parser**，否则入口文件会被解析两次并出现
两套互不相同的诊断。`ModuleOutput` 提供 `get(id)` / `entry()` /
`find_by_path(path)` / `find_entry(entry)`，路径匹配两侧都经
`module_key` 规范化，`./a.nov`、绝对路径、符号链接写法命中同一模块。

`AnalyzedModule` 字段：`id` / `path`（调用方原始路径，对外回显）/
`source` / `tokens`（完整 token 流，含注释）/ `program` /
`syntax_failed` / `load_failed`。
全部诊断仅存于 `SemaResult.diagnostics.modules[id]`；`syntax_diagnostics()` 与
`semantic_diagnostics()` 提供不重叠的阶段切片，`ordered()` 提供源码顺序，
`DiagnosticReport::has_errors()` 覆盖所有阶段及全局加载错误。模块内部的 `key`（规范化路径）只用于去重、
overlay 查找与相对导入解析，不外泄。

`ExpressionTypes` 是表达式级类型表，键 `(ModuleId, Span)`，值为 `TypeView`
（内部 `Ty` 的公开投影）。登记由 typecheck/expr/dispatch.rs 的 check_expr 与 hints.rs 的
check_expr_with_hint 包装层统一完成，两者共用同一记录键空间，结构下推不漏子节点。
Unknown（允许的无注解形参等）投影为 `TypeView::Unknown`，
Error（已报错）独立投影为 `TypeView::Error`；External 已删除，std 必须加载声明；所有函数签名（含匿名 lambda）均投影为
`TypeView::Function`，名义键只决定实体身份，不决定函数类型投影。

以下为既定规则清单（来自 `design.md` §6.5 / §5 / §4）：

1. **类型检查**：解析字面量值（进制、字符串转义）、`PredefinedType` 归类、
   class/struct 引用解析、泛型实例化；
2. **unsafe 上下文检查**：语法树遍历维护「unsafe 块深度」，S1–S10
   （指针算术要求在块内、指针互转要求 unsafe、`void*` 限制等）；
3. **struct 语义**：值拷贝（含 class 字段指针拷贝）、无继承 / 构造器校验、
   `==` 禁用、struct 方法 `this` 可信指针豁免；
4. **路径别名**：解析导入路径 `prefix:`（`@` = crate 根 / `std` / `super`），
   未知别名报错；
5. **入口判定**：顶层 `func main()`（无参、`void`/`int` 返回）存在性检查，
   可执行目标缺失时报「缺少 main」；
6. **特殊方法登记**：class/（trait 方法签名）中 `__` 前缀方法按名称登记
   调用时机（`__init` / `__destroy` / `__str` …），struct 拒绝 `__` 前缀。

## 7. 高层中间表示（novella-hir）

职责：把 SemaResult 降级为定型的块、控制流和顺序指令。只依赖 core、ast、sema，
不引入 LLVM / inkwell 类型；后端不再重建 AST 求值顺序或解析类型名。

- 类型来源为 sema.expression_types、annotation_types、declaration_types，分别提供
  表达式、已解析注解、声明最终类型；字面量由使用上下文递归定型。
- build_hir 遇到词法、语法、语义或加载错误立即拒绝，不跳过坏模块。
  Unknown / Error、未定型类型、联合、泛型及未支持语义返回 BuildError。
- Binary 携带两侧输入类型和结果类型；TypeTest 携带输入/目标类型。
  地址、间接写入、元组构造/提取、数组构造/长度均有明确指令。
- 条件用 HirCond { prelude, value }，循环每轮执行 prelude。
  Loop.step 在正常尾部和 continue 后执行，break / return 跳过。
- 普通赋值先 RHS 后定位 LHS；复合赋值定位一次后先读旧值再求 RHS；局部读取保存快照。
- struct self 由 sema 定型为属主，在 HIR 参数槽位以 Pointer(Struct) 传递，
  保证方法能修改原实例；签名的 receiver 不占显式 params。
  方法键为 m{module}::类型::方法，lambda 实体键包含模块和源码位置。
- 具名类型与字段访问共用父到子的跨模块字段布局；默认字段/实参使用定义环境，
  extern 产出独立声明项。用户已确认 p ?? backup 返回指针，非空时不解引用。

P1 已覆盖数组 for-in、元组/数组与递归解构、短路、默认实参、struct 实例调用、
match 顺序、无捕获 lambda 和复杂左值。P2 已完成扩展冒烟与 workspace 质量检查。
异常、异步、new、class 动态派发、闭包捕获、非 cfg 指令、静态/弱字段、trait 成员、
动态对象等后续期语义明确拒绝。string/数组布局和运行时 ABI 尚待后端阶段确定。
具体降级契约、拒绝边界及验证证据见 [hir-handover.md](hir-handover.md)。

## 8. 代码生成（novella-codegen）

接收 `HirProgram`，用 inkwell 0.11 对接 LLVM 23.1，生成 `x86_64-unknown-linux-gnu`
ELF 目标文件。先登记整个导入闭包的函数，再发射函数体。局部槽位统一在函数入口分配，
初始化留在 HIR 指令位置；`continue` 进入 `step`，`break` / `return` 跳过 `step`。

- 支持整数、布尔、标量取址和间接读写、分支、循环与跨模块直接调用。
- 整数加减乘按位宽回绕；除零、有符号最小值除以 `-1`（含取余）或移位量越界时陷阱，避免发射可执行的 LLVM 未定义操作。
- `main` 返回 `int` 作为进程状态，`void` 通过入口包装返回 `0`；其他 Novella 符号仅在目标文件内部可见。
- 浮点、指针算术、聚合、字符串、间接调用、全局与 `extern` ABI 仍未支持；发射时遇到不支持的类型或指令即报错。

## 9. CLI 与诊断渲染（novella-cli）

- `check <file>` 只做静态检查，不要求 `main`。`compile <file>` 默认经 HIR 生成目标文件并调用 `clang`，输出 `a.out`；`--emit obj` 输出 `output.o`。
- `--emit ast` 保留旧 JSON 契约（`tokens` + `program` + `diagnostics`）；`--emit hir` 输出已通过入口检查的 HIR。两者均支持 `-o`。
- 每个模块只分析一次。任一阶段失败均非零退出，不写入部分输出；链接和写入在输出目录内暂存，成功后才替换目标。
- 渲染：`render_diagnostic`（`cli/error.rs`）输出 rustc 风格
  `error: <msg>` / `--> 文件:行:列` + 源码行 + caret；展示列号 1-based、
  caret 按字符渲染（中文/emoji 不错位）。

## 10. LSP（novella-lsp）

- LSP：stdin/stdout 上的 `Content-Length` 报文帧，JSON-RPC 分发
  （请求必回复、通知不回复）。三层能力：词法级（语义高亮 / 文档高亮 / 词法
  诊断）、语法级（文档符号 / 折叠 / 补全 / 悬浮 / 签名帮助 / 选择范围 /
  内联提示 / 快速修复）、语义级（诊断推送与跨模块跳转定义 / 引用 / 重命名）；
- **模块划分**：`lib.rs` 只 `pub mod server`；`outline/` 做 AST → 符号树与
  折叠区间的投影，`features/` 按能力一文件做 lsp-types 转换，`server/` 的
  `dispatch` / `capabilities` / `lexical` / `syntax` / `navigation` / `sync` /
  `query` / `adapter` / `run` 分别负责路由、能力声明、能力挂载、文档同步、
  参数解码与主循环；
- **位置原语复用 core**：行表用 `novella_core::line_index::LineIndex`
  （`source.rs` 只叠加 UTF-16 列前缀和），模块键用
  `novella_core::path::module_key`；
- **复用编译器词法器**：`token_type_index` 穷尽匹配 `TokenKind` → legend 下标
  （`TOKEN_TYPES` 数组顺序即 legend），注释/关键字/数字/字符串/运算符分类；
- **偏移一律 UTF-16 码元**：内部字节偏移换算集中在 `source.rs`（`SemanticIndex`
  的按行 UTF-16 前缀和），含中文/emoji 的行整体不错位；跨行 token 按行拆分（当前词法下不触发，
  防御性实现）；
- 高亮规则不重复维护：编辑器侧不维护词法/关键字表，全部复用 lexer。

### 10.1 已确认变更

`token_type_index`：删 `Power` / `PowerAssign` 两行映射；新增
`Unsafe` / `Struct` → `KEYWORD`（无需加 legend 类别）。

## 11. 代码 ↔ 文法 ↔ 高亮三重对齐机制

| 对齐对 | 机制 | 备注 |
|--------|------|------|
| `TokenKind` ↔ `RawToken` | `From` match 穷尽性 | 漏变体即编译错 |
| `TokenKind` ↔ `NovellaLexer.g4` | 规则名逐一对应 + 文档表 | ANTLR 校验命令见下 |
| AST ↔ `NovellaParser.g4` | 文法不保留 AST 没有的结构 | 以代码为准 |
| `TokenKind` ↔ LSP legend | `token_type_index` 穷尽 match | 新关键字 → `KEYWORD` |

任何 token / 语法结构的增删，改动面固定为四类：core token、lexer RawToken、
grammar、LSP（+ 文档）。删除 token 属**破坏性 JSON 变更**（旧 token JSON 含
该变体无法反序列化）；当前产物为开发期快照，无长期格式承诺，直接替换。

文法校验（`grammar` 目录）：

```bash
antlr4 -o target/antlr-align/gen grammar/NovellaLexer.g4
antlr4 -lib target/antlr-align/gen/grammar -o target/antlr-align/gen grammar/NovellaParser.g4
```

## 12. 落地清单（已确认变更，按序实施）

> 进度注：2026 已完成的词法/文法/AST 落地步骤标 ✅，其余待实施。

| 步 | 文件 | 改动 | 状态 |
|----|------|------|------|
| 1 | `crates/novella-core/src/token.rs` | 删 `Power`/`PowerAssign`/`Stream`；增 `Unsafe`、`Struct`、`Weak` | ✅ |
| 2 | `crates/novella-lexer/src/lib.rs` | 删 3 规则+映射；增 `unsafe`、`struct`、`weak` 规则+映射 | ✅ |
| 3 | `crates/novella-ast/src/types.rs` | 增 `TypeExpr::Pointer` | ✅ |
| 4 | `crates/novella-ast/src/expr.rs` | 增 `UnaryOp::AddressOf`/`DerefPointer`、`Expression::Cast`、`Expression::StructInit`；删 `BinaryOp::Power`、`AssignOp::PowerAssign` | ✅ |
| 5 | `crates/novella-ast/src/stmt.rs` | 增 `Statement::Unsafe` | ✅ |
| 6 | `crates/novella-ast/src/program.rs` | 增 `Item::Struct`、`StructItem`；`MemberModifiers.is_weak` | ✅ |
| 7 | `grammar/NovellaLexer.g4` | 删 `Power`/`PowerAssign`/`Stream`；增 `Unsafe`、`Struct`、`Weak` | ✅ |
| 8 | `grammar/NovellaParser.g4` | 删幂行；增指针后缀、`as`、`unsafeStatement`、`structDeclaration`、struct 构造、weak 修饰 | ✅ |
| 9 | `crates/novella-parser/src/lib.rs` | 类型后缀、前缀取址/解引用、`as` 分支、unsafe 语句、struct 声明与构造 | ⬜ parser 骨架实现 |
| 10 | `crates/novella-lsp/src/semantic.rs` | 删 3 映射；增 `Unsafe`、`Struct`、`Weak` 映射 | ✅ |
| 11 | `novella-sema` | 语义规则（§6 清单）实现 | ✅ 第一期 |
| 12 | `grammar/README.md` 等 | 同步更新（含 `Power`/`Stream` 移除说明） | ⬜ |

依赖：1→2→（lexer 依赖 core）；3/4/5/6 相互独立；7 依赖 1；8 依赖 3/4/5/6；
9 依赖 2/3/4/5/6；10 依赖 1；11 依赖 9。

> 工程说明：stream 已确认砍掉（含关键字）；`weak` 仅字段修饰（无空安全保证，
> 读过期对象未定义行为）。

## 13. 工程注意与已知限制

- 词法变体数维持 105（删 `Power`/`PowerAssign`/`Stream` 3 个、增
  `Unsafe`/`Struct`/`Weak` 3 个，数量不变），加 `Eof` / `Error` 合成项共
  107；凡改 token 需同步四处：token.rs / lexer lib.rs / grammar /
  semantic.rs；
- Parser 仍为骨架：表达式 / 类型 / 多数语句形态未落地（§4.2），本轮变更
  与骨架并存，实现时按 design.md 备选顺序保持优先级；
- `novella-derive` 对 enum 变体要求携带 `span` 字段或为单字段 newtype（见
  derive 文档），新增 `Statement::Unsafe` / `TypeExpr::Pointer` 满足该约束；
- lexer 中 `LineComment` 需在 CLI 喂 parser 前过滤，JSON 输出保留（已如此）；
- LSP 偏移为 UTF-16 码元，新增高亮必须复用 `semantic.rs` 的编码换算。

## 14. 常用命令

```bash
just check        # cargo check --workspace --all-targets
just build        # cargo build --workspace
just test         # cargo test --workspace
just lint         # clippy --workspace --all-targets -D warnings
just fmt          # cargo fmt --all
just doc          # cargo doc --workspace --no-deps --open
cargo run -p novella -- compile <file> --emit ast -o out.json
just lsp-build / lsp-run / lsp-pack
```

> 说明：`just book`（mdbook build docs）依赖 mdbook 站点结构，当前 docs 为
> 平铺 md（本文件 + design.md），如需站点预览需另行充实 `book.toml`。

### 本轮 sema / 消费者契约

TypeView 新增 Applied(base,args)、Future(inner)、Object(fields)，类型参数带声明身份。
SemaResult.exhaustive_matches 提供 (ModuleId,Span) 的已证明穷尽事实，供返回路径检查
与 HIR 共用。AnalysisTarget / PointerWidth 只接收 32/64 位，既有入口默认宿主。
FieldMember / MethodMember 新增 name_span，StructInit 新增 type_args，Throw.expr 改为
Option；AST 的 serde 输出随之变化，parser、分段 clone/drop、LSP walker 均已同步。
IntConst / UntypedInt 使用 i128 载荷以保留合法 u64 最大值。开发期契约直接替换，
无迁移层；下游穷尽匹配须同步新增变体。

sema 不擦除后端尚不支持的类型；HIR 对 Applied/Future/Object、异常展开与挂起等
逐项返回带位置的 BuildError。静态检查通过不表示 LLVM/运行时实现已经交付。

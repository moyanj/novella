# Novella 编译器：工作流程与代码架构

> 这是 2026-11-21 的实现快照。泛型和 trait 的当前阶段职责及支持范围见 [编译器设计](compiler-design.md) 与 [HIR 交接文档](hir-handover.md)。

> 2026-11-21 · 对应当前 workspace（Rust edition 2024，11 个 crate）
>
> 这篇文档想把整个编译器讲清楚：一份源码进来，经过了哪些阶段，每个阶段把数据变成了什么，
> 为什么非要这么变。它是各篇 deep-dive 的总纲——每个阶段的逐行细节在文末「文档导航」里。

---

## 导读

novella 不是一个跑一下就出结果的脚本，而是一条由 11 个 crate、6 个编译阶段、
2 个二进制前端（CLI 和 LSP）组成的工具链。理解它的难点不在单个模块，而在模块之间：
**每一层拿什么当输入、交出什么、把什么擦掉了、又把什么固化了**。

抓住这几件事，整个程序就是一条清晰的线：

- 单向依赖链：`core ← ast ← parser ← sema ← hir ← codegen ← cli`（另有 `core ← lexer ← parser`、`core ← lexer ← lsp`；
  `novella-derive` 是构建期给 AST 用的 proc-macro）。
- 每层只做一件事：lexer 不解析、parser 不检查、sema 不生成、HIR 不碰 LLVM、codegen 不碰 AST。
- 词法/语法只跑一次：在 sema 内部完成，产物随 `SemaResult` 交给所有下游。
- 诊断只有一份：所有阶段、所有模块的诊断存进同一棵 `DiagnosticReport`。
- 错误策略分两段：前端（lexer/parser/sema）可恢复、尽量多报；后端（HIR/codegen）一遇无法处理就整体失败。

---

# 一、程序是什么

## 1. 定位

novella 是一门实验性系统编程语言的编译器。语言本身是 TS 与 Rust 的混合风格：

- TS 血统：`let/const`、对象字面量、`match/case`、`as` 转换、lambda、`??` 空值合并；
- Rust 血统：`u8..u64/i8..i64/usize/isize` 宽度类型、`pub/pri/pkg`、`trait`、`unsafe`、`with` 资源管理；
- 内存分层是它的核心定位：安全路径（默认，对象/集合/`with` 管内存，看不到指针）与
  底层路径（显式 opt-in，裸指针 + `unsafe` 块，面向内核/驱动/FFI）。

编译产物当前是 `x86_64-unknown-linux-gnu` 的 ELF，经 `clang` 链接成可执行程序。
构建需要 LLVM 23.1（inkwell `llvm23-1-prefer-dynamic` + `target-x86`）和 clang。

这是开发期编译器：语言设计（class 引用计数、异常、async、泛型后端、标准库）
远比已实现能力大。`check` 通过 ≠ 能降 HIR ≠ 能生成原生代码，每层都有明确的支持集与拒绝集，
见第 18 章和 `sema-coverage.md`。

## 2. 一条命令的完整旅程

以 `cargo run -p novella -- compile examples/native-main.nov` 为例，看源码怎么变成 `a.out`：

```
源码 native-main.nov
  │
  │ ① CLI 解析参数（clap），构造 AnalysisOptions
  │    compile → module_only=false（要求 main）；native → 64 位 + linux/x86_64/unix
  ▼
  │ ② analyze_with_options(FsLoader, entry, options)   [sema/analysis.rs:68]
  │
  ├─ Phase 0  load_modules                              [module/loader.rs:20]
  │    worklist 读文件 → lexer(&source) → Parser::new(含 preprocess) → parse()
  │    → cfg 裁剪 → 扫 import 扩展 worklist（循环导入安全）
  ├─ Phase 1  collect_module ×N                         [resolve/collect.rs:9]
  │    声明骨架 + 符号登记（函数签名先用 name:None 占位）
  ├─ Phase 2a bind_imports ×N                           [resolve/imports.rs:6]
  │    具名/命名空间导入绑定 + 可见性检查
  ├─ Phase 2b build_type_defs ×N                        [check/type_defs/]
  │    字段/方法/真签名解析，回填 Phase 1 占位
  ├─ Phase 2c resolve_class_parents + globals + fields
  │    继承链/trait + 全局变量与字段按需定型
  ├─ Phase 2d check_module ×N                           [check/decl.rs:15]
  │    函数体/方法体/顶层语句类型检查 + main 入口 + 泛型约束复核
  ▼
  │ ③ 装配 SemaResult（诊断、绑定表、类型表、模块产物）
  │    CLI 渲染 global + modules[*].ordered() 全部诊断
  │    has_errors() → 非零退出，不写任何输出
  ▼
  │ ④ build_hir(&sema)                                  [hir/build/module.rs:9]
  │    先登记全体函数签名 → 逐模块降级 → lambda 归位 → HirProgram
  │    （入口拒绝任何阶段错误；无法定型/不支持即 BuildError）
  ▼
  │ ⑤ emit_object(&hir)                                 [codegen/lib.rs:85]
  │    第一遍登记函数声明 → 第二遍发射函数体 → void-main 包装 → verify → ELF 字节
  ▼
  │ ⑥ write_output：暂存目录内 clang 链接 → fs::rename 原子替换 → a.out
  ▼
可执行程序（./a.out 退出码为 42）
```

这条链路有两个关键性质：

- **每个模块只分析一次**。lex+parse 只在 Phase 0 内跑，AST/tokens 随 `SemaResult.modules`
  交下游；CLI/HIR/codegen/LSP 都不重跑，否则入口文件解析两次、出现两套诊断。
- **诊断先行、失败即止**。CLI 渲染完全部诊断后才决定是否继续；有 error 就不进 HIR/后端，
  也不写部分输出。

`check` 子命令只走到 ③（`module_only=true`，不要求 main）；`--emit ast` 复用旧 JSON 契约
（`tokens`+`program`+`diagnostics`），也只走到 ③。

---

# 二、代码架构

## 3. Workspace 与依赖链

`Cargo.toml`：`members = ["crates/*"]`，`resolver = "2"`，`[profile.release] lto = true`。
11 个 crate 分四组：

**数据基础层**
- `novella-core`：`Span`/`Spanned<T>`/`TokenKind`/`Token`/`Diagnostic` + `line_index`/`path`/`decor`。
  全项目共享基础与位置原语的唯一实现处。
- `novella-derive`：proc-macro，提供 `#[derive(Spanned)]`，给带 `span` 字段的 AST 节点生成 `span()`。
  只在构建期被 `novella-ast` 使用，展开引用 `::novella_core::Span`。

**前端层**
- `novella-ast`：AST 节点（`expr/`/`stmt`/`types`/`program`），全量挂 span + 全量 serde。
- `novella-lexer`：logos 词法器，`RawToken → TokenKind` 转换层。
- `novella-parser`：手写递归下降解析器（`expression/` 子目录 + lib/stmt/types）。
- `novella-sema`：语义分析（六阶段管线），AST 之后唯一的语义权威。

**后端层**
- `novella-hir`：定型中间表示，后端无关，只依赖 core/ast/sema + serde。
- `novella-codegen`：inkwell/LLVM 目标文件发射，只消费 HIR。

**二进制与工具**
- `novella-cli`：二进制 `novella`，clap 子命令 `check`/`compile`，诊断渲染 + 链接。
- `novella-lsp`：语言服务器，独立二进制，公开面仅 `server::run`，复用 lexer 做高亮。
- `novella-syntax-gen`：TextMate 语法生成器（`TokenKind` → VS Code 冷启动着色）。

依赖链（自上而下单向）：

```
core ← ast ← parser ← sema ← hir ← codegen ← cli
core ← lexer ← parser
core ← lexer ← novella-lsp（独立二进制，不经 cli）
ast ──(构建期)── novella-derive（proc-macro，展开引用 ::novella_core::Span）
```

几个工程事实：下游（parser/sema/lsp）只见 `novella_core::token::Token`，不接触 logos 类型；
`novella-lsp` 与 `novella-cli` 同为二进制 crate，未登记进 `workspace.dependencies`；
release 开启全量 LTO，构建慢属预期。

## 4. 目录结构

```
novella/
├── crates/
│   ├── novella-core/src/
│   │   ├── lib.rs          # Span / Spanned<T> crate 根
│   │   ├── token.rs        # TokenKind（唯一事实源）+ Token + Display
│   │   ├── diagnostic.rs   # Diagnostic / Severity / DiagnosticLocation / DiagnosticCode
│   │   ├── line_index.rs   # 字节偏移 ⇄ (1-based 行, 0-based 字节列)
│   │   ├── path.rs         # normalize_path / module_key
│   │   └── decor.rs        # decorate（诊断位置填充）
│   ├── novella-derive/src/lib.rs        # #[derive(Spanned)]
│   ├── novella-ast/src/
│   │   ├── lib.rs / program.rs / stmt.rs / types.rs
│   │   ├── expr/           # mod / literal / ops / lambda / match_node
│   │   └── drop_guard.rs   # 深层递归 Box 链的自定义 Drop（分段栈）
│   ├── novella-lexer/src/lib.rs         # RawToken + lexer() + LexError
│   ├── novella-parser/src/
│   │   ├── lib.rs          # Parser 定义、游标/诊断原语、顶层 item
│   │   ├── preprocess.rs   # 过滤注释 + from-import 的 import 降级
│   │   ├── recursion.rs    # guarded() 递归分段栈
│   │   ├── stmt.rs / types.rs
│   │   └── expression/     # stairs/postfix/primary/lambda/match_expr/helper
│   ├── novella-sema/src/
│   │   ├── lib.rs / api.rs / analysis.rs / state.rs / symbol.rs / declarations.rs
│   │   ├── module/         # mod / loader / resolve_path / cfg
│   │   ├── resolve/        # mod(collect) / imports / types
│   │   ├── ty/             # mod / relations / generics / literals
│   │   ├── check/          # mod / decl / type_defs/ / fields / globals
│   │   └── typecheck/      # mod / body/ / expr/ / flow / matching / lambda / member / call
│   ├── novella-hir/src/
│   │   ├── lib.rs / ty.rs / body.rs / item.rs / build.rs
│   │   └── build/          # module/types/layout/context/expr/operators/lvalue/
│   │                       # stmt/loops/call/aggregate/coalesce/helpers/error
│   ├── novella-codegen/src/
│   │   ├── lib.rs          # emit_object / entry / value_type / 两遍扫描
│   │   ├── body.rs         # Body 状态机、槽位、指令发射、控制流
│   │   └── operators.rs    # unary / cast / binary / trap_if
│   ├── novella-cli/src/    # main.rs / cli.rs / error.rs
│   ├── novella-lsp/src/
│   │   ├── lib.rs          # 只 pub mod server
│   │   ├── source.rs       # 字节偏移 ⇄ UTF-16（SemanticIndex）
│   │   ├── semantic.rs     # token_type_index（穷尽 match → legend）
│   │   ├── outline/        # AST → 符号树 / 折叠区间
│   │   ├── features/       # 各 LSP 能力一文件（lsp-types 转换）
│   │   ├── nav.rs          # 跳转/引用/重命名（消费 bindings）
│   │   └── server/         # dispatch/capabilities/sync/analysis/run/...
│   └── novella-syntax-gen/src/
├── editors/vscode/         # VS Code 扩展（esbuild 打包，自包含 vsix）
├── grammar/                # ANTLR4 文法（以代码为基准对齐）
├── docs/                   # 本文 + 各 deep-dive + 设计文档
└── Justfile                # 命令入口（check/build/test/lint/fmt/lsp-*）
```

## 5. 数据模型主干：每层擦掉了什么、固化了什么

这是理解编译器的钥匙——看数据在每层「变形」时发生了什么：

| 层 | 输入 → 输出 | 擦掉了什么 | 固化/添加了什么 |
|----|------------|-----------|----------------|
| **lexer** | `&str` → `LexerResult{tokens, errors}` | 字符细节、空白 | `Token{kind, span, slice, line, column}`；EOF；错误进 errors 不进流 |
| **preprocess** | `Vec<Token>` → `Vec<Token>` | 注释 token | `from "m" import` 的 `import` 降级为标识符 |
| **parser** | `Vec<Token>` → `ParseResult{program, diagnostics}` | token 线性序列 | AST 树；歧义消解为确定节点类型；可恢复诊断 |
| **sema** | `Program` → `SemaResult` | ——（保留 AST） | 类型表、绑定表、声明索引、模块产物；诊断树；词法语法只跑一次 |
| **HIR** | `SemaResult` → `HirProgram` | **表达式树** | 定型类型、显式求值顺序、稳定 `HirFunctionId`；不支持即 fail-fast |
| **codegen** | `HirProgram` → `Vec<u8>`（ELF） | HIR 抽象 | LLVM IR → 机器码字节；只发射标量子集；错误即 fail-fast |
| **CLI** | ELF 字节 → `a.out` | —— | clang 链接 + 原子替换输出 |

一句话记住每层的「人格」：lexer 认字、parser 认句、sema 认意、HIR 定型、codegen 落码、CLI 打包。

### 5.1 核心类型逐层速览

**Token（`novella-core/src/token.rs`）**：`{ kind: TokenKind, span: Span, slice: &'static str, line: usize, column: usize }`。
`TokenKind` 是纯数据投影（105 个业务变体 + `Eof`/`Error` 合成项 = 107），带 `Display` 输出 Debug 文本；
`LineComment`（行注释）是正式 token，供高亮消费，由 parser 前一处过滤。

**AST（`novella-ast`）**：
- `Program { items: Vec<Item>, span }`，`Item` 含 import/type_alias/class/struct/trait/function/extern/statement/directive；
- `Expression`（字面量/标识符/括号/元组/数组/对象/this/一元/二元/赋值/成员/索引/调用/构造/await/is/三元/lambda/match）；
- `Statement`（含 `BindingPattern` 递归解构、`Unsafe`、`With`、`Try` 等）；
- `TypeExpr`（Predefined/Reference/Array/Tuple/Function/Union/Pointer）；
- 除纯值枚举外每个节点带 `span` 字段，`#[derive(Spanned)]` 生成 `span()`；全量 serde。
- 深层递归 `Box` 链由 `drop_guard.rs` 的自定义 `Drop` 分段释放（否则 2000 层嵌套析构会爆栈）。

**SemaResult（`novella-sema/src/api.rs`）**：

```rust
pub struct SemaResult {
    pub declarations: DeclarationIndex,                    // 按名字/位置定位声明
    pub exhaustive_matches: HashSet<(ModuleId, Span)>,     // 已证明穷尽的 match
    pub diagnostics: DiagnosticReport,                     // 唯一诊断树
    pub bindings: BindingTable,                            // references + definitions（LSP 跳转）
    pub types: TypeHierarchy,                              // 类型摘要（outline）
    pub modules: ModuleOutput,                             // 每模块 AST/tokens/源文本
    pub expression_types: ExpressionTypes,                 // 表达式类型表（键 (ModuleId, Span)）
    pub annotation_types: ExpressionTypes,                 // 类型标注
    pub declaration_types: ExpressionTypes,                // 声明最终类型
}
```

`TypeView` 是内部 `Ty` 的只读投影（`Ty::view()`），供下游消费；`Unknown`/`Error` 独立投影，
函数签名投影为 `TypeView::Function(Box<FunctionView>)` 且携带完整结构（`name: None` 不退化）。

**HirProgram（`novella-hir/src/item.rs`）**：`modules[*].items[*]` 为函数/extern/类型定义；
函数体是 `HirBlock → HirStmt → HirInstr` 三层，指令操作数只能是已求值的 `HirOperand`
（Local/常量/函数指针），没有「待求值的表达式」。`HirType` 是后端可消费的闭集
（Void/Boolean/String/Int/Float/Pointer/Array/Tuple/Function/Struct/Class/Trait），
不含 Unknown/字面量/Union/TypeParam/Error。

---

# 三、工作流程逐阶段

## 6. 词法分析（novella-lexer）

- **双层枚举**：`RawToken`（logos 私有投影，只在 lexer）与 `TokenKind`（core 稳定公共种类）一一对应；
  `impl From<RawToken> for TokenKind` 用 match 穷尽性保证——新增变体漏转即编译报错。
- **匹配策略**：最长匹配优先、同长取声明在前（多字符运算符先于单字符声明）；空白/换行 skip。
- **错误通道**：`LexError::{InvalidCharacter, UnterminatedString}` 只进 `LexerResult.errors`，
  不进 token 流；错误 token 以 `TokenKind::Error` 占位随错误记录。可恢复，不中断后续分析。
- **EOF token**：span 指向输入末尾 `{start: len, end: len}`，行列按末尾处；行号 1-based、列号 0-based（字节偏移）。
- **行列定位**：`LineIndex::build` 一次扫描建每行起点表，`locate()` 用 `partition_point` O(log n) 定位，
  避免逐 token 重扫的 O(n²)。

## 7. 预处理与语法分析（novella-parser）

### 7.1 预处理（`preprocess.rs`）

`Parser::new` 内一处完成：过滤 `LineComment`（注释透明，只在 core token 流保留）、
把 `from "m" import` 中的 `import` 降级为普通标识符（避免被当成 import item）。

### 7.2 解析机制

- 手写递归下降：`Parser { tokens, pos, file, diagnostics, ... }`；
- 游标原语 `peek`/`peek_n`/`advance`/`at_eof`，token 判定 `check`/`eat`/`expect`；
- 顶层循环 `while !at_eof() { parse_item(); recover_after_item(); }`——`recover_after_item`
  是死循环防护：若某 item 解析未推进游标，强制跳到下一行（顶层 item 通常独占行，换行是天然同步点）。
- 诊断可恢复：失败已记录诊断，继续解析后续 item，尽力产出 AST（供 LSP outline 消费）。

### 7.3 表达式优先级链（`expression/stairs.rs`）

赋值 → 三目 → `??` → `or` → `and` → 位或 → 异或 → 与 → 相等 → 关系 → 移位 → `as` →
加减 → 乘除 → `is` → 一元 → 后缀 → primary。后缀层处理成员/索引/调用/`++`/`--`。

### 7.4 递归防护与歧义消解

- **递归防护**（`recursion.rs`）：所有递归入口写成「`guarded` 包装 + `_inner` 实体」两层，
  剩余栈逼近红区时用 `stacker::maybe_grow` 切堆上新分段。合法深嵌套可解析；
  病态输入耗尽 `MAX_STACK_SEGMENTS`（`1 << 20`，按守卫调用次数计）才报「语法嵌套层级超出工程上限」。
  `checkpoint()/rollback()` 保存/恢复递归预算，避免推测分支消耗配额。详见第 16 章。
- **歧义消解**（上下文标记）：`with expr as binding` 的 `as` 靠 `with_stack` + `with_shield` +
  `looks_like_with_binding()` 判定（表头裸绑定优先，嵌套括号/实参内仍是 cast）；
  `match subject {` 的 `{` 靠 `in_match_subject` 禁止被 StructInit 抢走；
  match 分支体靠 `inside_match` 让表达式在 `case`/`default`/`}` 处自然停下。
- ANTLR 文法（`grammar/*.g4`）是「以代码为基准对齐」的文档产物，不是 parser 的生成源；
  不一致时以代码为准。

## 8. AST 层（novella-ast / novella-derive）

- **全量挂 span**：除纯值枚举（运算符、`Literal`、`PredefinedType`、`Scope`）外每个节点带 `span`；
  `#[derive(Spanned)]`（struct 取字段、enum 具名变体取字段、单字段 newtype 委托内部）生成 `span()`。
- **全量 serde**：AST/token 均派生 `Serialize, Deserialize`，服务 CLI `--emit ast` 的 JSON 输出。
- **尺寸控制**：`FunctionItem.block: Option<Box<Statement>>`、`Statement` 递归字段用 `Box`，
  避免 clippy `large-enum-variant`。
- **析构防护**（`drop_guard.rs`）：`Expression`/`Statement`/`TypeExpr` 是深层 `Box` 递归链，
  自定义 `Drop` 在分段栈内每层只摘取直接子节点放进局部 `Vec` 再逐个释放，
  把析构深度从「真实栈深度」变成「分段栈切换次数」。详见第 16 章。
- **值语义延后**：AST 字面量保留源码原文（含 `f/r/b`、进制前缀），解析在 sema 阶段。

## 9. 语义分析（novella-sema）——六阶段管线

这是整个程序最复杂的一层。入口层层收窄配置：
`analyze` → `analyze_with`（FsLoader）→ `analyze_with_target`（带指针宽度）→ `analyze_with_options`（完全配置）。
编排中枢在 `analysis.rs:68-217`，内部可变状态是 `Sema`（`state.rs`）。

### 9.1 六个阶段

```
Phase 0  load_modules（module/loader.rs:20）
         worklist 读文件 → lex → parse → cfg 裁剪 → 建模块全局 Scope → 扫 import 扩展 worklist
Phase 1  collect_module ×N（resolve/collect.rs:9）
         声明骨架入 type_defs、符号入 ScopeTree（函数签名 name:None 占位）
Phase 2a bind_imports ×N（resolve/imports.rs:6）
         具名/命名空间绑定、可见性检查、std:exception 特例
Phase 2b build_type_defs ×N（check/type_defs/）
         解析字段/方法/真签名，回填 Phase 1 占位；继承/trait 分类
Phase 2c resolve_class_parents（全局一次）+ globals::collect + fields::check_fields + globals::check_all
         继承链解析、字段与全局变量按需定型（三态环检测）
Phase 2d check_module ×N（check/decl.rs:15）
         函数体/方法体/顶层语句检查、main 入口、返回落尾、泛型约束统一复核
         ↓
         装配 SemaResult（analysis.rs:150-216）
```

每阶段之间检查 `loader.is_cancelled()`；中途取消则加全局诊断「分析已取消，产物不可使用」。

### 9.2 四个核心机制

1. **签名占位与回填分离**。Phase 1 只登记名字/种类（`name: None` 占位），Phase 2b 才解析真签名回填。
   这是循环导入与前向引用正确的根本——所有顶层符号就绪后才有跨模块签名解析。
2. **`Unknown`/`Error` 双档 + `is_opaque()` 级联抑制**。前面已报错的地方用占位类型继续走完分析，
   检查时一律放行，避免一个错误炸出一片次生错误。
3. **位置即身份**。泛型参数身份是 `T@m3:123`（模块 + 声明起点字节偏移）；
   局部变量确定赋值用「定义处 Span」作键——同名遮蔽、跨模块同名泛型都正确。
4. **跨阶段解耦**。`is_subclass` 需要继承链，但 `assignable` 在 `ty/` 里，于是用闭包注入
   `&mut dyn FnMut(&Ty,&Ty)->bool`，`ty/` 不依赖 `check/`。

### 9.3 Phase 0 细节：模块加载与路径

- `ModuleId = usize`，入口恒为 0；`Module` 同时持 `key`（`module_key` 规范化，用于去重/overlay/crate 根/
  相对导入基准）与 `path`（调用方原始路径，用于诊断回显）——**比较用 key、展示用 path**。
- 循环导入安全：路径→id 映射在**入队时**登记（处理模块前），二次遇到同 key 直接复用 id。
- 词法/语法只在 loader 内各跑一次；`loader.parsed_source()` 可复用 LSP 的未变更快照，避免重复 lex+parse。
- **cfg 裁剪时机**：在词法/语法之后、声明收集与 import 扫描之前——语法错误照报，但关闭的 item
  不进符号表、其中的 import 不会被加载。编辑器侧原始 AST 仍完整保留。
- 导入别名：`@:`=crate 根（找 `nov.toml`）、`super:`=父级、`std:`=标准库、`::` 相对当前模块；
  `:` 只作别名分隔符，路径段只用 `/`。

### 9.4 类型与检查要点

- `Ty` 保留 `Unknown`/`Error`/`IntLiteral`/`Union`/`TypeParam`/`Applied`/`Future`/`Object` 等形态，
  通过 `Ty::view()` 投影为 `TypeView`；不擦除后端不支持的类型身份，由 HIR 逐项报错。
- 表达式类型登记**唯一出口**是 `typecheck/expr/dispatch.rs` 的 `check_expr`（包一层记录后调 inner）；
  `hints.rs` 的 `check_expr_with_hint` 两个提示分支绕过出口，必须另行补记。
- 实例方法 `self` 在 `build_method_sig` 中定型为所属 Struct/Class 并置 `is_method`；
  `FunctionSig::view()` 跳过该形参并置 `receiver=true`。
- unsafe 规则（S1–S10）：语法树遍历维护「unsafe 块深度」，指针算术/互转/`void*` 限制等在此检查。
- 每条设计规则的实现状态以 `sema-coverage.md` 为唯一权威矩阵。

## 10. HIR 降级（novella-hir）

职责一句话：把语义正确但仍是源码形状的 AST，降级成类型已落地、控制流已线性化、符号键已稳定、
后端可直接逐条发射的指令流；任何无法固化的构造立即 fail-fast，绝不静默兜底。

### 10.1 五条不变量

1. **HIR 无表达式树**。块→语句→指令三层，操作数只能是已求值 `HirOperand`；所有表达式最终落到 Local。
2. **类型单一事实源**。三张类型投影表（`expression_types`/`annotation_types`/`declaration_types`，键 `(ModuleId,Span)`）
   是唯一来源；HIR 不重解析类型名、不自建类型环境，查不到就 `BuildError`。
3. **副作用顺序显式化**。靠指令物理顺序与 dst 槽位表达，不依赖 AST 遍历隐含顺序。
   普通赋值先 RHS 后定位 LHS；复合赋值定位一次→读旧值→RHS→写回。
4. **符号键稳定唯一**。顶层 `m{module}::{name}`、方法 `m{module}::{类型}::{方法}`、
   lambda `m{module}::lambda#{span.start}`；后端直接拿它做符号名与调用解析。
5. **fail-fast**。不支持的构造/类型/求值顺序问题一律 `BuildError`，无部分结果、无近似实现。

### 10.2 `build_hir` 流程

- 入口校验：`SemaResult` 含任何阶段错误立即拒绝，不跳过坏模块。
- **先登记签名再降级函数体**：`collect_declarations` 先把所有函数签名存入 builders，
  因此调用后定义的函数或递归调用都能立即查到签名——HIR 不需要前向声明的第二遍扫描。
- 逐模块 `build_module`：`collect_declarations` → 遍历 items → `finish_types`（类型定义 + 继承布局）→
  `method_functions`（方法体）→ pending lambdas。
- **lambda 归位**：函数体降级中遇 lambda 推入 `pending`，最后按 `source_module` 归回所属模块并按 id 去重
  （lambda 可能在 A 模块降级时首次展开，但定义归属源码所在的 B 模块）。
- 条件用 `HirCond{prelude, value}`；循环每轮执行 prelude，`Loop.step` 在正常尾部和 continue 后执行，
  break/return 跳过。
- `??` 保留非空指针而不解引用；struct 接收者在参数槽以 `Pointer(Struct)` 传递，使方法能修改原实例。

## 11. 代码生成（novella-codegen）

- **极小的标量后端**：仅 803 行 Rust（`lib.rs` 188 + `body.rs` 450 + `operators.rs` 165）。
- 只支持整数（i8/i16/i32/i64 含无符号与 usize/isize）、布尔（i1）、指向整数/布尔的指针；
  只支持无接收者、非异步、非泛型的函数；只支持直接调用。
- **浮点、字符串、数组、元组、struct/class、全局变量、extern、间接调用、字段/索引、TypeTest 全部发射期报错。**
  HIR 能降级成功 ≠ codegen 能生成代码——这是分层设计中最重要的边界认知。
- **两遍扫描**：第一遍登记所有函数声明（`add_function`，含对外可见的 main）；第二遍发射函数体。
- **槽位机制**：所有 `alloca` 用独立 builder 插到入口块首部，循环里的 `Alloc` 不会每轮重新分配栈。
- **void-main 包装**：若入口返回 void，生成真正的 `int main()` 调用入口并返回 0；返回 int 则直接作为进程状态。
- **算术陷阱**：除零、有符号最小值除以 -1（含取余）、移位量越界时发射 `llvm.trap`，避免 LLVM 未定义操作。
- `verify()` 后 `write_to_memory_buffer(Object)` 产出 ELF 字节；错误经上下文链包装为 `CodegenError`。

## 12. CLI 编排（novella-cli）

`run()`（`main.rs:31`）流程见第 2 章。要点：

- **子命令**：`check <file>`（只静态检查，不要求 main）、`compile <file>`（默认链接为 `a.out`）。
- **emit 模式**：`ast`（tokens+program+diagnostics JSON）、`hir`（定型 HIR JSON）、`obj`（ELF 目标文件）、
  `exe`（链接可执行）。`--emit ast` 时 `module_only=true`（不要求 main）。
- **原生目标锁定**：`emit exe/obj` 时强制 64 位 + `linux`/`x86_64`/`unix`，指定其他目标直接报错。
- **诊断先行**：渲染 global + 各模块 `ordered()` 后，`has_errors()` 为真即非零退出，不写输出。
- **原子输出**：暂存目录与输出同文件系统，链接/写入成功后 `fs::rename` 替换，失败不留部分产物。
- `render_diagnostic` 输出 rustc 风格 `error: <msg>` + `--> 文件:行:列` + 源码行 + caret；
  展示列 1-based、caret 按字符渲染（中文/emoji 不错位）。

## 13. LSP 服务（novella-lsp）

独立二进制，公开面仅 `server::run`。stdin/stdout 上 `Content-Length` 报文帧，JSON-RPC 分发
（请求必回复、通知不回复）。

- **线程模型**（`server/run.rs:29`）：接收线程只读协议帧、状态线程独占文档与输出、工作线程独占分析。
  新编辑协作取消旧计算；导航在版本未定型时返回空结果，不使用旧 span。
- **分析任务**（`server/analysis.rs:65`）：状态线程构造 `AnalysisJob`（overlay + 已解析快照 + 配置），
  工作线程对每个 root 跑 `analyze_with_options`。`CancellableLoader` 注入 `is_cancelled()`，
  通过 `parsed_source()` 复用未变更文档的 lex+parse 产物。
- **能力三层**：词法级（语义高亮/文档高亮/词法诊断）、语法级（outline/折叠/补全/悬浮/签名帮助/
  选择范围/内联提示/快速修复）、语义级（诊断推送与跨模块跳转/引用/重命名）。
- **位置换算**：内部字节偏移，LSP 要求 UTF-16 码元；换算集中在 `source.rs` 的 `SemanticIndex`
  （按行 UTF-16 前缀和），含中文/emoji 的行整体不错位；跨行 token 按行拆分（防御性实现）。
- **高亮规则唯一来源**：`semantic.rs` 的 `token_type_index` 穷尽匹配 `TokenKind` → legend 下标
  （`TOKEN_TYPES` 数组顺序即下标），编辑器侧不维护任何词法/关键字表。

---

# 四、正确性机制

## 14. 诊断系统——只有一份

```
DiagnosticReport
├── global: Vec<Diagnostic>                  // 全局/加载错误（无模块归属）
└── modules: [{ id, path, diagnostics }]      // 每模块一个合并后的诊断 Vec
```

- 每模块的词法/语法/语义诊断按 `location.span.start` **合并排序**存进同一个 Vec；
  `syntax_diagnostic_count` 记录前缀中词法/语法的条数。
- 视图切分：`syntax_diagnostics()`（前 count 条）、`semantic_diagnostics()`（其余）、
  `ordered()`（统一排序）。`has_errors()` 覆盖所有阶段及全局错误。
- 错误码：NVL3001 名称、NVL3002 类型、NVL3003 声明、NVL3004 unsafe、NVL3009 警告。
- HIR 的 `BuildError` 不自行渲染，由 CLI `lower()` 转成通用诊断并回填 module/span，
  复用同一 `render_diagnostic`。

## 15. 位置与路径——只在 core

- `LineIndex`（`novella-core/src/line_index.rs`）：行号 1-based、列号 0-based（字节偏移），
  对齐 ANTLR `getLine()`/`getCharPositionInLine()`；`locate()` O(log n)。
- `module_key`（`novella-core/src/path.rs`）：模块路径词法归一化 + 唯一键；canonicalize 失败退回归一化。
- `decor`（`novella-core/src/decor.rs`）：诊断位置（file/line/column）填充与 span 平移。
- LSP 在其上叠加 UTF-16 列换算（`source.rs`），不另写行表。
- 入口路径语义：诊断 `location.file` 一律用调用方原始 `path`，因此 CLI 输出里看到的是命令行写法
  （如 `test.nov`）而非规范化绝对路径。

## 16. 递归防护——两层防线

novella 有大量合法深嵌套（数万层括号/嵌套块），编译器用两层防线应对：

1. **解析/克隆期**：`guarded()` 包装 + `stacker::maybe_grow` 切分段栈（parser 的 `recursion.rs`、
   sema 的递归入口、AST 的 GuardedClone）。`checkpoint()/rollback()` 保证推测分支不耗配额。
2. **析构期**：`drop_guard.rs` 的自定义 `Drop` 把深层 `Box` 链析构深度降为分段栈切换次数
   （否则实测 2000 层嵌套 `unsafe` 块析构即 abort）。

新增任何递归字段时，clone/drop 的穷尽 match 必须同步，漏改编译失败而非静默退化。

## 17. 错误策略分层——前端宽容、后端严厉

| 阶段 | 遇错策略 | 占位机制 | 结果 |
|------|---------|---------|------|
| lexer | 可恢复 | `TokenKind::Error` | 错误进 errors，token 流继续 |
| parser | 可恢复 | 尽力产出 AST | 诊断累积，跳到下一行恢复 |
| sema | 可恢复 | `Ty::Unknown`/`Ty::Error` + `is_opaque()` | 尽量多报，最后一次性产出 |
| HIR | **fail-fast** | 无 | 第一个无法降级处即 `BuildError` |
| codegen | **fail-fast** | 无 | 不支持的类型/指令即 `CodegenError` |
| CLI | 汇总后 fail-fast | 无 | 有 error 就不写输出、非零退出 |

这个分层是刻意的：前端要一次报全帮用户改错，后端要保证发出的代码一定正确。

## 18. 能力边界

**已实现**（可端到端）：整数/布尔标量、算术与比较、if/loop/break/continue/return、
跨模块直接调用、`main` 返回 int 作进程状态（void 经包装返回 0）、数组 for-in、元组/数组与递归解构、
短路、默认实参、struct 实例调用、match 顺序、无捕获 lambda、复杂左值。

**明确拒绝或未实现**（分阶段）：
- **HIR 拒绝**：Applied/Future/Object 类型、异常展开、挂起（await/yield）、动态派发、
  class `new`、闭包捕获、非 cfg 指令、静态/弱字段、trait 成员、动态对象；泛型、Union 不支持。
- **codegen 拒绝**：浮点、字符串、指针算术、聚合、间接调用、全局变量、extern ABI、字段/索引、TypeTest。
- **语言层待落地**：class 引用计数、异常运行时、async 运行时、完整标准库（`stdlib-design.md`
  状态「待评审，未实现」）、编译器指令展开（sema 报「尚未实现」）。

`check` 通过只代表静态检查过了，不代表 HIR/LLVM/运行时已实现。

---

# 附录

## A. 常用命令

```bash
just check       # cargo check --workspace --all-targets
just build       # cargo build --workspace
just test        # cargo test --workspace
just lint        # cargo clippy --workspace --all-targets -- -D warnings
just fmt         # cargo fmt --all（CI 用 fmt-check）
just doc         # cargo doc --workspace --no-deps --open

cargo run -p novella -- compile examples/native-main.nov              # 分析→HIR→LLVM→链接 a.out
cargo run -p novella -- check examples/native-main.nov                # 只静态检查
cargo run -p novella -- compile examples/native-main.nov --emit ast -o out.json
cargo run -p novella -- compile examples/native-main.nov --emit hir -o out.hir.json
cargo run -p novella -- compile examples/native-main.nov --emit obj -o out.o

just lsp-build / lsp-run / lsp-pack        # 语言服务器构建/运行/VS Code 扩展打包
just gen-syntax / gen-syntax-check         # TextMate 语法生成/校验
```

## B. 文档导航表

| 想深入 | 读这份文档 |
|--------|-----------|
| 语言长什么样、语义规则 | `design.md` |
| 编译器整体设计（简短版） | `compiler-design.md` |
| parser 逐行、歧义消解、错误恢复 | `parser-deep-dive.md` |
| sema 六阶段、符号/类型/检查全机制 | `sema-deep-dive.md` |
| 每条语言规则的实现状态 | `sema-coverage.md` |
| HIR 定型、求值顺序、符号键、fail-fast | `hir-deep-dive.md` |
| HIR→codegen 契约速查 | `hir-handover.md` |
| LLVM 发射、槽位、算术陷阱、能力矩阵 | `codegen-deep-dive.md` |
| 标准库设计（未实现） | `stdlib-design.md` |
| 项目结构与编码约定 | 根目录 `AGENTS.md` |

## C. TokenKind 的多处映射

`TokenKind` 在四个地方被映射或消费，改动 token 时这些地方会一起动：

| 对齐对 | 机制 | 改动时同步 |
|--------|------|-----------|
| `TokenKind` ↔ `RawToken` | `From` match 穷尽性，漏变体即编译错 | `core/token.rs` + `lexer/lib.rs` |
| `TokenKind` ↔ `NovellaLexer.g4` | 规则名逐一对应 + 文档表 | `grammar/NovellaLexer.g4` |
| AST ↔ `NovellaParser.g4` | 文法不保留 AST 没有的结构，以代码为准 | `grammar/NovellaParser.g4` |
| `TokenKind` ↔ LSP legend | `token_type_index` 穷尽 match | `lsp/semantic.rs`（新关键字 → `KEYWORD`） |

另外：删除 token 会破坏 JSON 反序列化（旧 token JSON 无法读入）；改 `Span`/`Token` 字段要同步
`--emit ast` 的 JSON 消费者。

## D. 各层改动会波及哪里

| 改动 | 波及范围 |
|------|---------|
| 加/改词法单元 | `core/token.rs` → `lexer/lib.rs`（RawToken + From）→ `grammar/*.g4` → `lsp/semantic.rs` |
| 加/改语法结构 | `parser/src/`（lib/stmt/expression/types）→ `grammar/NovellaParser.g4` → AST 节点 |
| 加 AST 节点 | `ast/src/`（带 `span` + `#[derive(Spanned)]`）→ 同步 `drop_guard.rs`/GuardedClone → 文法 → JSON 消费者 |
| 加一条 sema 规则 | `sema/typecheck/` 或 `check/` → 更新 `sema-coverage.md` |
| 加一种表达式类型登记 | 走 `check_expr` 唯一出口；提示分支需补记 |
| 加一项 LSP 能力 | `features/` 一文件 + `server/` 挂载；复用 core 位置原语与 UTF-16 换算 |
| 加一种 HIR 指令/类型 | `hir/body.rs` 或 `ty.rs` → `build/` 降级 → `codegen` 发射（否则发射期报错） |
| 加 codegen 类型支持 | `value_type()` 映射 → `body.rs`/`operators.rs` 分支 → 能力矩阵文档 |

## E. 一句话总结

> novella 是一条单向、分层的编译流水线：lexer 认字 → parser 认句 → sema 认意（六阶段、诊断唯一、
> 词法语法一次）→ HIR 定型（无表达式树、求值顺序显式、fail-fast）→ codegen 落码（极小标量后端）
> → CLI 打包；LSP 作为独立前端复用 lexer 与 SemaResult 提供编辑体验。
> 读懂「每层擦掉了什么、固化了什么、错误在哪层怎么处理」，就读懂了整个程序。

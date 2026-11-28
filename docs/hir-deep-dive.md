# novella-hir 深度技术文档

> 泛型单态化、trait 见证与拥有型 `dyn` 已在此快照之后实现，当前契约见 [HIR 交接文档](hir-handover.md#3-泛型与-trait-的原生契约)。

> 历史快照说明（2026-11-22）：HIR 已新增共享 BuildTarget、no_main、运行时需求和
> export_name，并支持 GlobalAddress/AggregateConst 及静态全局降级。本文逐行分析保留
> 编写时状态，关于全局不支持及仅 Linux 后端的旧描述以 [裸机支持](freestanding.md)
> 和当前类型定义为准。历史使用的 `hir_smoke` 调试 example 已删除，当前通过
> `cargo run -p novella -- compile <file> --emit hir -o <output.json>` 导出 HIR。

> 文档版本：2026-11-21
> 适用代码：novella 编译器 workspace（`crates/novella-hir`，edition 2024）
> 配套文档：`docs/hir-handover.md`（契约速查）、`docs/compiler-design.md`（整体设计 §7–9）、`docs/sema-deep-dive.md`（上游语义分析）、`docs/parser-deep-dive.md`（语法歧义消解）
>
> 本文中的行号均相对于仓库根目录 `/home/moyan/projects/novella`，路径形如 `crates/novella-hir/src/ty.rs:20-46`。所有 Rust 代码片段均按当前磁盘源码逐字重建；凡属"设计意图/权衡"的说明，均标注了来源（源码注释、契约文档或明确的推断），不会把推断伪装成事实。

---

## 导读：这份文档怎么读

### 面向谁

这份文档写给三类读者：

1. **要改 HIR 的人**：你需要知道每一条 IR 指令为什么存在、它的字段从哪来、下游怎么消费它、改一个变体要同步哪些地方。第 3–14 章是你的主战场。
2. **要理解编译器全貌的人**：你想知道 AST 经过 sema 之后，为什么还要再降一层 HIR，这一层到底"擦掉"了什么、"固化"了什么。第 1 章和第 15–16 章回答这个问题。
3. **要审查正确性的人**：你在意求值顺序、副作用时机、跨模块符号键、fail-fast 边界。第 10、12、13、18、19 章集中讨论这些。

### HIR 在编译器中的位置

novella 的编译管线是一条单向依赖链：

```
core ← ast ← parser ← sema ← hir ← codegen ← cli
core ← lexer ← parser
```

HIR（`novella-hir`）位于 **sema 之后、codegen 之前**。它不是可选装饰，而是两者之间唯一的、被完整定型的中间契约：

- **对上**，它只吃 `novella_sema::SemaResult`——一份已经完成模块加载、名称解析、类型检查、诊断收集的语义结果。
- **对下**，它只产出 `HirProgram`——一棵不再含有任何"待求值表达式"、任何"待解析类型名"、任何"待消解语法歧义"的指令树。`novella-codegen` 只消费 HIR，绝不回头碰 AST 或 sema。

一句话概括 HIR 的职责：

> **把"语义上正确但仍是源码形状"的 AST，降级成"语义已固化、类型已落地、控制流已线性化、符号键已稳定"的、后端可直接逐条发射的指令流；任何无法在这一步固化的构造，立即 fail-fast，绝不静默兜底。**

### 阅读路线

- **快速建立心智模型**：读第 1 章（定位与架构）+ 第 4 章（body 数据模型）+ 第 6 章（build_hir 总流程），约 30 分钟。
- **逐文件精读**：按第 2 → 3 → 4 → 5 → 6 → 7 → …章顺序，每章对应一个或几个源文件。
- **带着问题查**：用目录跳到对应章节；第 20 章有符号速查表。
- **看真实输出**：第 17 章给出从一段 novella 源码降级出的真实 HIR JSON 片段（由 `examples/hir_smoke.rs` 实际运行产出）。

### 一个贯穿全文的 mental model

读 HIR 代码时，脑子里始终放着这条"固化流水线"：

```
AST 表达式树  ──类型查询(sema 表)──▶  定型 + 落地字面量
              ──控制流展开──▶      块 / 语句 / 指令 三层
              ──符号命名──▶        稳定 HirFunctionId
              ──求值顺序显式化──▶   每条指令有明确 dst 槽位
              ──不支持构造──▶      BuildError（fail-fast）
```

HIR 里**没有"表达式"这个位置概念的延续**——所有表达式都被翻译成"先算到某个 Local、再用这个 Local"的指令序列。这是理解整个 crate 的钥匙。

---

## 第 1 章 模块定位与整体架构

### 1.1 定位：HIR 是什么，不是什么

`crates/novella-hir/src/lib.rs` 的开头注释（1–24 行）直接写明了定位。逐条拆解：

**（1）只依赖 core + ast + sema，不出现 inkwell / LLVM 类型。**

这是一条硬边界。HIR 是**后端无关**的中间表示：它描述"要做什么"（加、比较、分支、调用、取字段地址），但不描述"LLVM 怎么发射"。`Cargo.toml` 里 `novella-hir` 的依赖只有 `novella-core`、`novella-ast`、`novella-sema` 三个 workspace crate 加 `serde`；`inkwell` 只出现在 `novella-codegen`。这样做的好处是：HIR 可以独立序列化成 JSON（`--emit hir`）、可以独立测试、可以未来接第二后端（比如解释器或 WASM）而不动语义降级逻辑。

**（2）语法层上下文在此全部消除。**

parser 为了消解歧义引入了一堆"上下文标记"——`with_stack`、`in_match_subject`、`inside_match`、`as` 的 `looks_like_with_binding()` 判定（见 `docs/parser-deep-dive.md` 第 12 章）。这些是**语法阶段的内部状态**，到了 HIR 一点都不该留下。例如 `with expr as binding` 的 `as` 到底是"绑定"还是"cast"，在 parser 阶段就已经判定完毕并反映在 AST 节点类型上；HIR 看到的已经是确定的 `Statement::With` 或 cast 表达式，不再需要任何上下文猜测。

**（3）类型一律是已解析的 `HirType`。**

`TypeView` 里那些"还没定型"的形态——`Unknown`、`Null`（字面量）、`IntLiteral`、`FloatLiteral`、`Union`、`TypeParam`、`Error`、以及 Applied/Future/Object 这类"语义上合法但后端不支持"的类型——**不允许原样进入 HIR**。它们在 `HirType::from_view_with` 这一步被就地处理：要么根据 `preferred` 提示落地成具体类型（字面量），要么转成 `BuildError`（其余）。这条规则是"HIR 是已定型表示"的技术保证。

**（4）函数统一用稳定的 `HirFunctionId`。**

顶层函数用 `m{module}::{name}`（模块号 + 名字），方法用 `m{module}::{类型}::{方法}`，lambda 用 `m{module}::lambda#{span.start}`。这些键在降级期生成，后端用它做符号名、做调用目标解析。它们必须**稳定**（同一实体在任意位置引用得到同一个键）且**唯一**（不同实体不撞键，这就是为什么两个"长得一模一样"的 lambda 也要靠源码位置区分）。

**（5）fail-fast。**

任何无法降级的情况——类型缺失、类型不支持、构造未实现、求值顺序无法保证——都在构造期变成 `BuildError` 返回，不产生"部分 HIR"、不静默退化、不加兜底。这与 sema 的"错误恢复优先"形成鲜明对比：**sema 尽量多报错误，HIR 一遇到无法处理就整体失败**。

### 1.2 目录结构与文件清单

HIR crate 的全部源文件（按职责分组）：

```
crates/novella-hir/
├── Cargo.toml
├── src/
│   ├── lib.rs          (35 行)   crate 根：设计约束文档 + 模块声明 + 公开重导出
│   ├── ty.rs           (334 行)  类型层：HirType / IntTy / FloatTy / FunctionSignature / HirFunctionId
│   ├── body.rs         (349 行)  指令层：HirBlock / HirStmt / HirInstr / HirOperand / 运算符枚举
│   ├── item.rs         (148 行)  程序层：HirProgram / HirModule / HirItem / HirFunction / HirTypeDef
│   └── build.rs        (119 行)  构建子系统根：三张类型表说明 + 两个 Builder 结构 + 子模块声明
│   └── build/
│       ├── module.rs       (324 行)  入口 build_hir + 模块/函数降级 + 声明校验
│       ├── types.rs        (223 行)  类型定义、方法函数、继承布局的类型侧
│       ├── layout.rs        (55 行)  父到子字段展开（唯一字段索引来源）
│       ├── context.rs      (148 行)  BodyBuilder 的上下文操作与类型查询
│       ├── expr.rs         (279 行)  表达式降级主分派
│       ├── operators.rs    (331 行)  一元/二元运算、短路、赋值、左值槽
│       ├── lvalue.rs       (166 行)  地址构造与间接读
│       ├── stmt.rs         (401 行)  语句与控制流降级
│       ├── loops.rs         (87 行)  for-in 降级
│       ├── call.rs         (167 行)  调用与默认实参展开
│       ├── aggregate.rs    (315 行)  struct 初始化 / match / lambda / 字面量
│       ├── coalesce.rs      (55 行)  `??` 空合并
│       ├── helpers.rs      (196 行)  可见性、运算符映射、类型转换判定、字面量解析
│       └── error.rs         (98 行)  BuildError / BuildErrorKind
└── examples/
    └── hir_smoke.rs    (165 行)  端到端冒烟：内存文件系统喂源码，打印降级结果
```

源码合计约 **3830 行**（不含示例）。从行数分布能看出重心：`stmt.rs`（401）、`ty.rs`（334）、`operators.rs`（331）、`module.rs`（324）、`aggregate.rs`（315）是五个最大的文件，分别对应控制流、类型固化、运算与副作用、顶层编排、复杂表达式——这也正是 HIR 最难的四类问题。

`Cargo.toml` 关键点：

```toml
[package]
name = "novella-hir"
# ...
[dependencies]
novella-core = { workspace = true }
novella-ast = { workspace = true }
novella-sema = { workspace = true }
serde = { workspace = true, features = ["derive"] }

[dev-dependencies]
serde_json = "1.0.154"
```

注意：**没有 `novella-codegen`、没有 `inkwell`、没有 `novella-parser`**。HIR 甚至不直接依赖 lexer/parser——它拿到的 token/AST 都是 sema 通过 `SemaResult.modules` 交过来的。这保证了"每模块只解析一次"（见 `docs/compiler-design.md` §9 和根 `AGENTS.md` 的 ANTI-PATTERNS：下游不得自己再跑一遍 lexer/parser）。

### 1.3 设计哲学：五条不变量

把散落在源码注释和契约文档里的原则提炼成五条不变量。改动 HIR 时，破坏任何一条都需要重新审视整个契约。

**不变量 1：HIR 无表达式树。**
`body.rs:1-15` 明确说明：块 → 语句 → 指令三层结构，指令的操作数只能是**已求值**的 `HirOperand`（Local、常量、函数指针等），绝不能是"待求值的 AST 表达式"。任何表达式最终都必须落到某个 Local 里。这条不变量是后端能线性发射的前提。

**不变量 2：类型单一事实源。**
`build.rs` 开头注释强调：三张类型投影表（`expression_types`、`annotation_types`、`declaration_types`）是 HIR 类型的**唯一来源**，键是 `(ModuleId, Span)`。HIR **不重新解析类型名**、不维护自己的类型环境、不从 AST 文本重新推断。查不到就 `MissingExpressionType` / `MissingType` 报错。

**不变量 3：副作用顺序显式化。**
求值顺序不能依赖 AST 遍历的隐含顺序，必须靠指令在块中的**物理顺序**和明确的 dst 槽位表达。典型例子（`docs/compiler-design.md:250-256`）：普通赋值先算 RHS 再定位 LHS；复合赋值先定位 LHS、再读旧值、再算 RHS；局部读取生成快照。`??`、短路、match 都靠"分支 + 结果槽"保证只求值该求值的部分。

**不变量 4：符号键稳定唯一。**
所有函数/方法有确定的 `HirFunctionId`。lambda 用源码位置区分，方法用"类型::方法"区分，跨模块用模块号前缀区分。后端直接拿这个键做符号名和调用解析，因此它既是身份也是名字。

**不变量 5：fail-fast，无静默兜底。**
`error.rs` 注释：BuildError 只在"语义已通过但后端不支持"或"类型集合不一致"时发生。HIR 不假装支持某构造、不退化成"近似实现"、不产出部分结果。`build_hir` 一旦遇到第一个无法降级的点就整体返回错误。

### 1.4 完整编译管线中的位置

从 CLI 到原生程序，HIR 处在正中间。以 `novella compile <file>` 为例（`crates/novella-cli/src/main.rs` 的 `run()`）：

```
1. cli::Cli::parse()                       解析命令行
2. analyze(&args, module_only, native)     → SemaResult
   └─ analyze_with_options(&mut FsLoader, &args.file, options)
      （词法/语法/语义一次完成，全部产物与诊断都在 SemaResult）
3. 渲染 diagnostics.global + diagnostics.modules[*].ordered()
4. if result.diagnostics.has_errors() → 返回失败，不写任何输出
5. match emit:
   ├─ Ast  → ast_json(result)              tokens+program+diagnostics JSON
   ├─ Hir  → lower(result)? → serde_json   HIR JSON
   └─ Obj/Exe → lower(result)?             → HirProgram
                ├─ emit_object(&hir)         LLVM → ELF 目标文件
                ├─ Obj: 写 output.o
                └─ Exe: clang 链接 → a.out
```

其中 `lower()`（`main.rs:127-151`）是 HIR 的调用点：

```rust
fn lower(result: &SemaResult) -> Option<HirProgram> {
    match build_hir(result) {
        Ok(hir) => Some(hir),
        Err(error) => {
            let mut diagnostic = Diagnostic::error(error.to_string());
            let module = error.module.and_then(|id| result.modules.get(id));
            if let Some(module) = module {
                let (line, column) = error.span.map_or((1, 0), |span| {
                    LineIndex::build(&module.source).locate(span.start)
                });
                diagnostic = diagnostic.with_location(DiagnosticLocation {
                    file: Some(module.path.display().to_string()),
                    line,
                    column,
                    span: error.span,
                });
            }
            eprintln!("{}", render_diagnostic(&diagnostic, module.map(|m| m.source.as_str())));
            None
        }
    }
}
```

这个函数做了三件值得注意的事：

- **把 `BuildError` 转成通用的 rustc 风格诊断**，复用 CLI 统一的渲染器（`render_diagnostic`）。HIR 自己不渲染诊断，只提供 `kind` + 可选的 module/span。
- **位置回填**：用 `LineIndex::build(&module.source).locate(span.start)` 把字节偏移换成 1-based 行、0-based 列（列换算规则见 `novella-core/src/line_index.rs`）。
- **模块可能为空**：`error.module` 是 `Option<ModuleId>`，全局性错误（如"输入含错误"）没有具体位置，此时诊断只有消息没有 location。

`analyze()` 还有一个与 HIR 直接相关的约束（`main.rs:86-125`）：当 `emit` 是原生目标（`Exe`/`Obj`）时，强制 `AnalysisOptions` 的 target 为 64 位 + `linux`/`x86_64`/`unix`，否则报 `"原生后端只支持 x86_64-unknown-linux-gnu，不能指定 --{flag}={value}"`。这说明 HIR 虽然逻辑上后端无关，但**当前唯一的消费方 codegen 锁定了 x86-64 Linux**；类型层的 `IntTy::Usize`/`Isize` 的 `bit_width()` 也是按 64 位记的（见 `ty.rs:91-98`）。

### 1.5 数据流总览

把 HIR 内部的数据流画成一张图（后面各章会逐块放大）：

```
                       SemaResult
                          │
        ┌─────────────────┼──────────────────┐
        │                 │                  │
  diagnostics       modules/modules      types/b tables
  (has_errors?)     (AST per module)     (类型投影/声明索引)
        │                 │                  │
        └──────── 入口校验 ─┴──────────────────┘
                          │
                  build_hir(sema)                    [build/module.rs]
                          │
              ┌───────────┴───────────┐
              │ 逐模块 build_module    │
              │  ├ collect_declarations（先登记签名）
              │  ├ 遍历 items → 函数/extern/类型
              │  ├ finish_types（类型定义 + 继承布局）
              │  ├ method_functions（方法体）
              │  └ pending lambdas
              └───────────┬───────────┘
                          │
              lambda 归位（按 source_module）
                          │
                    HirProgram
                          │
        ┌─────────────────┴─────────────────┐
        │                                   │
  --emit hir                          emit_object(codegen)
  (serde_json)                        (LLVM IR → ELF)
```

注意两个容易忽略的点：

1. **先登记签名再降级函数体**：`collect_declarations` 在遍历 items 之前先把所有顶层函数的签名存进 `ModuleBuilder.signatures` / `top_signatures`。这样函数体里调用一个"定义在后面"的函数，或者递归调用自己，都能立即查到签名——这是 HIR 不需要"前向声明两遍扫描"的原因。
2. **lambda 在模块降级之后统一归位**：函数体降级过程中遇到 lambda 会 push 到 `pending`，`build_hir` 最后把它们按 `source_module` 归回所属模块并按 id 去重。原因是 lambda 可能在被 A 模块的函数降级时首次展开，但它的"定义归属"是源码所在的模块 B（见 `build/module.rs:9-51`）。

---

## 第 2 章 公开契约：`lib.rs`

### 2.1 crate 根内容

`crates/novella-hir/src/lib.rs` 只有 35 行，但它是整个 crate 的"宪法"。结构如下：

1. **1–24 行：模块级文档注释**——即 1.1 节引用的五条设计约束。这不是普通注释，而是给维护者的契约声明。
2. **模块声明**（`pub mod` / `mod`）：`ty`、`body`、`item` 是公开数据层，`build` 是构建子系统。
3. **公开重导出**：把常用类型从根重新导出，使调用方可以 `use novella_hir::{build_hir, HirProgram, HirType, ...}`，而不必写全 `novella_hir::build::...`。

公开重导出清单（逐字）：

```rust
pub use body::{
    HirBinaryOp, HirBlock, HirCond, HirInstr, HirOperand, HirStmt, HirUnaryOp, LocalId,
};
pub use build::{BuildError, BuildErrorKind, build_hir};
pub use item::{
    HirField, HirFunction, HirGlobal, HirItem, HirMethod, HirModule, HirProgram, HirTypeDef,
    HirVisibility, LocalSlot,
};
pub use ty::{FloatTy, FunctionSignature, HirFunctionId, HirType, IntTy};
```

这份清单就是整个 crate 的公开 API 面。凡是没在这里出现的类型，外部 crate 都拿不到（除非它本身是 `pub mod` 里的 `pub` 项，但按项目约定"crate 公开面最小化"，内部实现都用 `pub(crate)`）。**新增 `pub` 前要先确认有 crate 外调用方**——这是根 `AGENTS.md` 的明确约定。

### 2.2 依赖边界与可见性

从重导出可以看出 HIR 的分层：

- **数据层（ty / body / item）**：纯数据结构 + 少量转换方法，无副作用、无构建逻辑。它们可以被 codegen 直接 `use`。
- **构建层（build）**：唯一入口是 `build_hir`；`BuildError`/`BuildErrorKind` 是错误契约。构建内部的所有子模块（`module`、`types`、`expr`…）都是 `mod`（私有），外部看不到。
- **根（lib.rs）**：只做"声明 + 重导出"，不写逻辑。

这种"薄根 + 分层"的结构与 sema（`api.rs` 公开数据、`analysis.rs` 编排、其余内部）和 LSP（`server::run` 唯一入口）保持一致，是项目统一的 crate 组织风格。

### 2.3 与外部的三个接口点

HIR 与外部世界只有三个接口：

| 接口 | 方向 | 位置 | 说明 |
|------|------|------|------|
| `build_hir(&SemaResult) -> Result<HirProgram, BuildError>` | 输入 | `build/module.rs:9` | 唯一构建入口，消费 sema 结果 |
| `HirProgram` 的 serde 序列化 | 输出 | 各数据类型的 `#[derive(Serialize, Deserialize)]` | `--emit hir` 直接 `serde_json::to_string_pretty(&hir)` |
| `HirProgram` 的字段结构 | 输出 | `item.rs` | codegen 直接遍历 `modules[*].items[*]` |

第三点值得强调：codegen 与 HIR 之间**没有适配器层**。`novella-codegen` 直接 `use novella_hir::{HirProgram, HirItem, HirFunction, HirType, IntTy, ...}` 并遍历字段。这意味着 `item.rs` / `body.rs` / `ty.rs` 的结构就是事实上的 ABI——改一个字段或变体，codegen 必须同步改。这也是为什么文档要逐字段记录这些数据模型（第 3–5 章）。

<!-- CONTINUE -->

---

## 第 3 章 类型层 `ty.rs`

### 3.1 为什么需要单独的类型层

sema 的类型系统（`Ty`）是为**语义检查**设计的：它保留 `Unknown`、`Error` 用于级联抑制，保留 `IntLiteral`/`FloatLiteral` 用于类型推断，保留 `Union`/`TypeParam` 用于类型和泛型检查，保留 `Applied`/`Future`/`Object` 这类"语义合法"的类型身份。这些形态对 sema 是必要的，但对后端是灾难——LLVM 发射时不可能有"未知类型"或"字面量类型"的槽位。

`ty.rs` 的作用就是定义一个**后端可消费的类型闭集 `HirType`**，并提供从 `TypeView`（sema 对外的只读投影）到 `HirType` 的**唯一转换入口** `HirType::from_view_with`。这一层把"语义正确但形态未定"的类型，收敛成"确定且可发射"的类型。

### 3.2 `HirType`（ty.rs:20-46）

```rust
pub enum HirType {
    Void,
    Boolean,
    String,
    Int(IntTy),
    Float(FloatTy),
    Pointer(Box<HirType>),
    Array(Box<HirType>),
    Tuple(Vec<HirType>),
    Function(Box<FunctionSignature>),
    Struct(ModuleId, String),
    Class(ModuleId, String),
    Trait(ModuleId, String),
}
```

12 个变体的语义分组：

- **无值/标量**：`Void`（无返回值，仅函数返回类型/void 调用结果）、`Boolean`、`Int(IntTy)`、`Float(FloatTy)`、`String`。
- **复合**：`Pointer(Box<HirType>)`（指向任意 HIR 类型）、`Array(Box<HirType>)`（同构数组）、`Tuple(Vec<HirType>)`（异构定长元组）、`Function(Box<FunctionSignature>)`（函数值类型，携带完整签名）。
- **名义类型**：`Struct(ModuleId, String)` / `Class(ModuleId, String)` / `Trait(ModuleId, String)`。第二个字段是类型名；第一个字段是**定义所在模块 id**，这是跨模块唯一性的关键——两个模块各自定义 `Point` 是两个不同的 HIR 类型。

**与 `TypeView` 的对照**：`HirType` 不含 `Unknown`、`Null`、`IntLiteral`、`FloatLiteral`、`Union`、`TypeParam`、`Error`，也不含 sema 内部的 `Applied`/`Future`/`Object`。这些要么是"未定型"（字面量、Null），要么在 HIR 阶段明确不支持（Union、泛型、不透明类型），要么已被 sema 归类为错误。

**`Function` 携带完整签名**这一点很关键：HIR 里的函数值不是"指向某个函数类型的引用"，而是带着参数表、返回类型、`receiver` 标记、异步标记等的完整 `FunctionSignature`。这样后端拿到一个 `FunctionPtr(id)` 时，能从签名表查到它的调用约定。

### 3.3 `IntTy` 与 `FloatTy`（ty.rs:53-140）

```rust
pub enum IntTy { U8, U16, U32, U64, I8, I16, I32, I64, Usize, Isize }
```

**`from_name`（70-85 行）**把 sema 的 `IntTy` 名字映射过来：`"int"` → `I32`，`"float"` → `F32` 等。注意这里 HIR 的整数类型_names_是"名字字符串"驱动的——因为 sema 的 `TypeView::Int` 携带的是名字而不是枚举，HIR 按名字重建自己的枚举。

**`bit_width`（91-98 行）**返回位宽，`usize`/`isize` 按 **64 位**记（与 codegen 锁定的 x86-64 目标一致）。

**`is_signed`（101-106 行）**区分有符号/无符号，影响 codegen 发射有符号除法还是无符号除法、比较指令选择等。

```rust
pub enum FloatTy { F16, F32, F64 }
```

`FloatTy::from_name`（121-130）和 `bit_width`（133-140）同理。**当前 codegen 的 `value_type`（`codegen/src/lib.rs:67-81`）只接受 `Int` 和 `Boolean`（以及指向它们的 Pointer），遇到 `Float` 直接报"暂不支持值类型"**——所以浮点虽然在数据模型和 sema 里完整存在，但当前后端还发射不了。这是"HIR 支持 ≠ codegen 支持"的典型例子。

### 3.4 `HirFunctionId`（ty.rs:148-158）

```rust
pub struct HirFunctionId(pub String);
impl HirFunctionId {
    pub fn anonymous(module: ModuleId) -> Self {
        HirFunctionId(format!("m{module}::anon"))
    }
}
```

`Display` 直接输出 `.0`。命名规则汇总（分散在多处，集中列出）：

| 实体 | 键格式 | 生成位置 |
|------|--------|----------|
| 顶层函数 | `m{module}::{name}` | `build/module.rs:179` `function_id()`、`build/context.rs:142-147` |
| 方法 | `m{module}::{类型}::{方法}` | `build/types.rs` `module_function_id()` / sema 的 `build_method_sig` |
| lambda | `m{module}::lambda#{span.start}` | `build/aggregate.rs:205-265` `build_lambda` |
| extern 函数 | `m{module}::{name}` | 同顶层函数 |
| 匿名/占位签名 | `m{module}::anon` 或 `m{module}::anon#{params.len()}` | `HirFunctionId::anonymous()` / `synth_function_id()` |

注意 `synth_function_id`（`context.rs:142-147`）在 `view.name` 为 `None` 时生成 `m{self.module}::anon#{params.len()}`——它只是**占位**，不标识真实实体。同一函数有名字时始终用名字路径，不会因为查询路径不同而变键。

### 3.5 `FunctionSignature`（ty.rs:166-184）

```rust
pub struct FunctionSignature {
    pub id: HirFunctionId,
    pub params: Vec<HirType>,
    pub ret: HirType,
    pub type_param_count: usize,
    pub is_async: bool,
    pub is_extern: bool,
    pub receiver: bool,
}
```

字段语义逐条说明：

- **`id`**：函数身份键，见上表。
- **`params`**：显式参数类型列表。**`self` 不在 `params` 里**——`receiver` 单独标记。
- **`ret`**：返回类型，`Void` 表示无值返回。
- **`type_param_count`**：泛型参数数量。HIR 阶段泛型不支持，`from_view_with` 遇到非 0 就报 `GenericUnsupported`。这个字段的存在是为了忠实记录 sema 的签名信息，同时让不支持检测有地方可查。
- **`is_async`**：异步标记。同理，`true` 在转换时报 `Unimplemented`。
- **`is_extern`**：外部函数标记，用于 `HirItem::ExternFunction`。
- **`receiver`**：是否有隐式接收者。源码注释（ty.rs 附近）：`self` 不在 params 里，`receiver` 表示隐式接收者，**codegen 生成符号时把它插到参数表最前面**。

`receiver` 与"方法"的关系：实例方法的 `signature.receiver == true`，静态方法是 `false`。`method_functions`（`types.rs:72-143`）在降级实例方法时，会额外 `declare_param("self", receiver_ty)` 把接收者放进 `params` 槽位（`HirFunction.params` 里有 `self`，但 `signature.params` 里没有）——这个看似矛盾的设计其实刻意区分了"调用约定层面的签名"和"函数体局部槽位"：后端从签名拼符号参数表时插 receiver，函数体降级时 receiver 作为第一个 LocalSlot 存在。

### 3.6 `HirType::from_view_with`（ty.rs:200-290）——类型固化的核心

这是 `ty.rs` 最重要的函数，也是 HIR "类型单一出口"的具体实现。签名（推断自源码）：

```rust
pub fn from_view(view: &TypeView) -> Result<Self, BuildErrorKind>   // 200-205：调 from_view_with(view, None)
pub fn from_view_with(
    view: &TypeView,
    preferred: Option<&HirType>,
) -> Result<Self, BuildErrorKind>   // 208-290
```

它穷尽 `match` `TypeView` 的每个变体（`TypeView` 定义见 `novella-sema/src/api.rs:159-191`）：

| `TypeView` 变体 | 转换行为 | 对应错误 |
|-----------------|----------|----------|
| `Void` / `Boolean` / `String` | 直接映射到同名 `HirType` | — |
| `Int(name)` / `Float(name)` | 经 `IntTy::from_name` / `FloatTy::from_name` | `UnknownTypeName{name}`（名字不认识） |
| `Pointer(inner)` / `Array(inner)` / `Tuple(items)` | 递归 `from_view_with(inner, preferred)`（数组/元组用 preferred 的元素提示） | 递归传播 |
| `Function(view)` | 调 `FunctionSignature::from_view_with` | — |
| `Struct`/`Class`/`Trait`（名义键） | 映射到 `HirType::{Struct,Class,Trait}(ModuleId, String)` | — |
| `Unknown` | — | `OpaqueType`（语义未定型，不可进入 HIR） |
| `Error` | — | `InvalidInput`（上游已有错误） |
| `Null` | 仅当 `preferred` 是 `Pointer` / `Class` 时返回该 preferred，否则 `UntypedLiteral{what: "null"}` | `UntypedLiteral` / 成功落地 |
| `IntLiteral` | 仅当 `preferred` 是 `HirType::Int(_)` 时返回该 Int 类型，否则 `UntypedLiteral` | `UntypedLiteral` |
| `FloatLiteral` | 仅当 `preferred` 是 `HirType::Float(_)` 时返回该 Float 类型，否则 `UntypedLiteral` | `UntypedLiteral` |
| `Union` | — | `UnionUnsupported` |
| `TypeParam` | — | `GenericUnsupported` |
| `Applied` / `Future` / `Object` | — | `OpaqueType`（语义合法但后端不支持） |

**`preferred` 参数的意义**：这是"下推提示"——调用方（如 `build_typed_expression`）知道期望类型（比如 `let x: i32 = 42` 里的 `i32`），把它传进来，让 `IntLiteral`/`FloatLiteral`/`Null` 落地到这个期望类型上。没有 preferred 时，未定型字面量无法独立成为 HIR 类型，直接报错。这对应 sema 的 `hints.rs`（类型下推表）设计——sema 已经算出了"这个字面量应该是什么类型"，HIR 只需要消费这个结果。

**为什么 `Unknown` 和 `Error` 分开报错**：`Unknown` 是"还没推断出来"（静默级联抑制），`Error` 是"确实有错"。HIR 遇到两者都不能降级，但错误类别不同，便于诊断文案区分（见 2026-11-15 契约修订："TypeView::Error 与 Unknown 分离"）。

### 3.7 `FunctionSignature::from_view_with`（ty.rs:298-333）

```rust
pub fn from_view(view: &FunctionView) -> Result<Self, BuildErrorKind>   // 298-303
pub fn from_view_with(
    view: &FunctionView,
    function_id: impl FnOnce(&FunctionView) -> HirFunctionId,
) -> Result<Self, BuildErrorKind>    // 306-333（签名按源码重建）
```

处理流程：

1. `type_param_count != 0` → `GenericUnsupported`。
2. `is_async` → `Unimplemented`（HIR 不支持 async）。
3. `params` 和 `ret` 分别 `HirType::from_view_with(None)`——函数签名里的类型必须是确定的，不依赖 preferred（`None`）。
4. 最后 `id = function_id(view)`——**id 通过回调注入**，而不是在类型层硬编码命名规则。

这个回调设计很重要：`HirType::from_view_with` 被很多上下文调用（顶层函数声明、方法签名、表达式类型查询、默认实参……），不同上下文对"匿名函数如何命名"的策略不同。通过回调（`synth_function_id` / `module_function_id` / `function_id_of`），类型层只负责"形状转换"，命名策略由调用方决定。这避免了在 `ty.rs` 里写一堆 `if name.is_none()` 特判。

---

## 第 4 章 指令层 `body.rs`

### 4.1 两级结构：块与语句/指令

`body.rs` 的开头注释（1–15 行）解释了为什么 HIR 不用表达式树：

> 块 → 语句 → 指令。指令的操作数只能是已求值的 `HirOperand`，不含待求值表达式。

这个设计的直接后果是：**每个表达式在 HIR 里都变成一串指令 + 一个结果 Local**。例如 `a + b` 不是 `Binary` 节点套两个子表达式，而是：

```
Load dst=1 src=<a的槽>
Load dst=2 src=<b的槽>
Binary{Add, lhs=Local(1), rhs=Local(2), lhs_ty=I32, rhs_ty=I32, dst=3, dst_ty=I32}
```

后端看到 `Binary` 时，两个操作数都已经在寄存器/槽位里，直接发射 `add` 即可，不需要递归求值子树。这让 codegen 的 `instruction()` 成为一个大的平坦 `match`，没有递归下降。

### 4.2 `LocalId` 与 `HirOperand`（body.rs:25-70）

```rust
pub struct LocalId(pub u32);
pub enum HirOperand {
    Local(LocalId),
    IntConst(i128),
    UntypedInt { value: i128, span: Span },
    FloatConst(f64),
    UntypedFloat { value: f64, span: Span },
    BoolConst(bool),
    StringConst(String),
    Function(HirFunctionId),
    Null,
    Unit,
    FunctionPtr(HirFunctionId),
    WithReceiver { receiver: Box<HirOperand>, value: Box<HirOperand> },
}
```

**`LocalId`**：注释说明"构造期单调递增分配，codegen 用它建 alloca 并以 LocalId 反查；不做跨函数唯一（避免与寄存器分配耦合）"。也就是说 LocalId 只在**单个函数体内**唯一，每个函数的编号从 0 重新开始。codegen 在每个函数入口为一个 LocalId 分配一个栈槽（`alloca`），之后用 `LocalId → alloca` 的映射查。

**`HirOperand` 各变体**：

- `Local(id)`：指向某个局部槽位的**值**（读）。
- `IntConst(i128)`：已定型的整数常量。**注意用 `i128` 而不是 `i64`**——这是为了精确表示 `u64::MAX`（18446744073709551615）等大整数；契约文档明确 "IntConst/UntypedInt 为 i128，JSON 中 18446744073709551615 精确"。
- `UntypedInt{value, span}`：`coerce` 之前的未定型整数，带源码位置用于诊断。
- `FloatConst(f64)` / `UntypedFloat{value, span}`：浮点对应物。
- `BoolConst(bool)` / `StringConst(String)` / `Null` / `Unit`：字面量值。
- `Function(HirFunctionId)` / `FunctionPtr(HirFunctionId)`：函数引用。两者的区别从使用处看，`Function` 用于值语义的函数实体引用，`FunctionPtr` 用于调用目标（call 的 callee 字段恒为 `FunctionPtr`）。实践中 `build_expression` 的 Identifier 分支产 `FunctionPtr`，`build_call` 的 callee 也用 `FunctionPtr`。
- `WithReceiver{receiver, value}`：`with` 表达式里的接收者绑定——把接收者操作数和一个值打包，供后续隐式成员访问消费。这是 `with` 语义在 HIR 层的残留（但语法层的 `as` 消歧已消除）。

### 4.3 `HirBlock` 与 `HirStmt`（body.rs:72-132）

```rust
pub struct HirBlock { pub stmts: Vec<HirStmt> }
impl HirBlock {
    pub fn new() -> Self { ... }
    pub fn push(&mut self, stmt: HirStmt) { ... }
    pub fn is_empty(&self) -> bool { ... }
}

pub enum HirStmt {
    Instr(HirInstr),
    Block(HirBlock),
    If { cond: HirCond, then_block: HirBlock, else_block: HirBlock },
    Loop { cond: Option<HirCond>, body: HirBlock, step: HirBlock },
    Break,
    Continue,
    EnterScope,
    LeaveScope,
    Nop,
}
```

**`HirBlock`** 就是语句序列，`is_empty()` 的注释说 "codegen 可据此省掉空 basic block"。

**`HirStmt` 的九个变体**：

- `Instr(HirInstr)`：单条指令。
- `Block(HirBlock)`：嵌套块（用于 `{}` 词法块、`with`、`unsafe`、C-for 的外层包装等）。
- `If { cond, then_block, else_block }`：条件分支。`else_block` 总是存在（没有 else 时是空块或 `Nop`），这样后端不用处理 `Option`。
- `Loop { cond: Option<HirCond>, body, step }`：循环。`cond=None` 表示无限循环；`cond=Some(...)` 的 `prelude` 每轮执行。`step` 在**正常尾部**和 `continue` 时执行，`break`/`return` 跳过。
- `Break` / `Continue`：跳转。构造期由 `loop_depth` 保证不会出现在循环外。
- `EnterScope` / `LeaveScope`：词法作用域标记。HIR 保留它们是给后端/调试用的（局部变量生命周期），构造期成对出现；codegen 的 `block()` 对它们返回 `true`（穿透，不发射实际 IR）。
- `Nop`：空语句，用于占位（比如 `if` 没有 else 分支时填一个）。

**控制流的"块化"表达**是 HIR 的关键设计：没有 `goto`、没有 basic block 的显式标签，而是用嵌套 `HirBlock` 表达结构。codegen 在发射时把每个 `HirBlock` 映射到一个 LLVM basic block，`If`/`Loop` 负责建分支边——具体见第 15 章。

### 4.4 `HirCond`（body.rs:296-310）

```rust
pub struct HirCond { pub prelude: HirBlock, pub value: HirOperand }
impl HirCond {
    pub fn new(value: HirOperand) -> Self { ... }   // 最常见形态：无前置语句
}
```

**条件 = "前置语句块 + 最终条件值"**。这是为了正确处理"条件表达式有副作用"的情况。例如 `while (i < n)` 的条件本身没有副作用，但 `while (f() > 0)` 的 `f()` 有——如果把它放在循环外只求值一次就错了。`prelude` 承载"每轮要重新求值"的部分，`value` 是最后的布尔操作数。

源码注释明确："条件前置语句每轮随循环执行（避免只求值一次的 bug）"。这个 bug 是 HIR 设计阶段真实踩过的坑，因此把 `prelude` 作为条件的一部分固化在数据结构里，而不是靠调用方记得重新求值。

`HirCond` 同时用于 `HirStmt::If` 和 `HirStmt::Loop` 的 `cond` 字段——两者共享同一套"条件求值"语义。

### 4.5 `HirInstr`（body.rs:136-274）——指令全集

`HirInstr` 有 22 个变体，按职责分组列出（字段名逐字来自源码）：

**局部槽位与内存**：

```rust
Alloc { id: LocalId, ty: HirType, init: Option<HirOperand> }
Store { id: LocalId, value: HirOperand }
Load { dst: LocalId, src: LocalId }
```

`Alloc` 在局部槽里创建一个值并可选初始化；`Store` 覆盖已有槽；`Load` 把槽里的值读到新 Local（快照）。注意 `Store` 的 `id` 是槽位 id 而 `value` 是操作数；`Load` 的 `src` 也是槽位 id（这里"Local"既当名字也当槽位号用）。

**地址与间接访问**：

```rust
LocalAddress { local: LocalId, ty: HirType, dst: LocalId }
FieldAddress { target: HirOperand, target_ty: HirType, index: usize, ty: HirType, dst: LocalId }
IndexAddress { target: HirOperand, index: HirOperand, element_ty: HirType, dst: LocalId }
StoreIndirect { address: HirOperand, value: HirOperand, ty: HirType }
```

`LocalAddress` 取局部变量的地址；`FieldAddress` 取结构体字段地址（`index` 是**布局索引**，由 `layout::instance_fields` 统一计算）；`IndexAddress` 取数组元素地址；`StoreIndirect` 通过地址写入。间接读取没有单独指令，用 `Unary{Deref}` 表达（见下）。

**聚合构造与提取**：

```rust
Tuple { elements: Vec<HirOperand>, ty: HirType, dst: LocalId }
Array { elements: Vec<HirOperand>, element_ty: HirType, dst: LocalId }
TupleExtract { target: HirOperand, index: usize, ty: HirType, dst: LocalId }
ArrayLength { target: HirOperand, dst: LocalId }
```

元组/数组构造把一组已求值操作数拼成聚合值；提取和取长度是独立指令。**注意 `IndexLoad` 用于数组索引读，`TupleExtract` 用于元组字段提取**——两者分开是因为元组元素类型异构、数组同构。

数组的**写**不经过专用指令：`UpdatePlace::Address` 生成地址后由 `StoreIndirect` 落值，自定义类型的下标写入走 `__setitem` 协议调用。规模上限由 `build/requirements.rs` 的 `IndexLoad` 分支统计。

**调用与一元/二元运算**：

```rust
Call { callee: HirOperand, args: Vec<HirOperand>, dst: Option<LocalId>, dst_ty: Option<HirType> }
Unary { op: HirUnaryOp, operand: HirOperand, dst: LocalId, dst_ty: HirType }
Binary { op: HirBinaryOp, lhs: HirOperand, rhs: HirOperand,
         lhs_ty: HirType, rhs_ty: HirType, dst: LocalId, dst_ty: HirType }
Cast { operand: HirOperand, from: HirType, to: HirType, dst: LocalId }
TypeTest { operand: HirOperand, operand_ty: HirType, target: HirType, dst: LocalId }
```

- `Call` 的 `dst`/`dst_ty` 是 `Option`——void 调用没有结果槽。
- `Binary` **同时携带 `lhs_ty`/`rhs_ty`/`dst_ty`**。这是契约文档强调的关键："Binary 必须携带两侧输入类型"——后端需要知道操作数类型才能选对 LLVM 指令（比如有符号/无符号除法、指针算术的位移类型），即使两个操作数都是 `Local`（Local 本身不携带类型）。
- `TypeTest` 携带 `operand_ty`（输入类型）和目标类型——`is` 表达式需要运行时类型检查的信息。

**聚合字段读写**：

```rust
FieldLoad { target: HirOperand, target_ty: HirType, field: String, index: usize, ty: HirType, dst: LocalId }
FieldStore { target: HirOperand, target_ty: HirType, field: String, index: usize, value: HirOperand }
IndexLoad { target: HirOperand, element_ty: HirType, index: HirOperand, dst: LocalId }
```

`FieldLoad`/`FieldStore` 的 `index` 是布局索引（source field 名 `field` 保留用于调试），`ty` 是字段类型。`IndexLoad` 带 `element_ty` 供后端确定元素大小。

**返回**：

```rust
Return(Option<HirOperand>)
```

**指令全集对照表**（便于速查）：

| 组 | 变体 |
|----|------|
| 局部槽 | Alloc, Store, Load |
| 地址 | LocalAddress, FieldAddress, IndexAddress, StoreIndirect |
| 聚合 | Tuple, Array, TupleExtract, ArrayLength, FieldLoad, FieldStore, IndexLoad |
| 运算 | Unary, Binary, Cast, TypeTest |
| 调用 | Call |
| 控制 | Return |

### 4.6 运算符枚举

```rust
pub enum HirUnaryOp { Identity, AddressOf, Deref, Neg, Not, BitNot }   // 278-291
pub enum HirBinaryOp {                                                // 323-349
    Add, Sub, Mul, Div, Rem, Shl, Shr, BitAnd, BitOr, BitXor,
    Eq, Ne, Lt, Le, Gt, Ge, LogAnd, LogOr, PtrDiff,
}
```

`HirBinaryOp` 包含 `LogAnd`/`LogOr`，但它们的实际降级走 `build_short_circuit`（分支），不发射 `Binary{LogAnd}` 指令——枚举里的存在是为了穷尽 AST 的二元运算符映射（`binary_op_of` 返回 `None` 时由 `build_binary` 分流）。`PtrDiff` 是"指针减指针"的专用运算，结果类型是 `Isize`。


---

## 第 5 章 程序层 `item.rs`

### 5.1 三层容器（item.rs:15-39）

```rust
pub struct HirProgram { pub modules: Vec<HirModule> }
impl HirProgram {
    pub fn module(&self, id: ModuleId) -> Option<&HirModule> { self.modules.get(id) }
}

pub struct HirModule {
    pub id: ModuleId,
    pub path: String,
    pub items: Vec<HirItem>,
    pub types: Vec<HirTypeDef>,
}
```

**`HirProgram`** 是 HIR 的根，唯一的 `modules: Vec<HirModule>`。关键约定：**入口模块是下标 0，与 `SemaResult.modules` 完全对齐**（源码注释 15 行）。因此 `HirModule.id`（`ModuleId`，本质是 usize）同时是 `modules` 的下标；`HirProgram::module(id)` 直接 `self.modules.get(id)`，O(1) 查询。

**`HirModule`**：

- `id`：模块 id，即下标。
- `path`：调用方传入的**原始路径**（`AnalyzedModule::path.display().to_string()`）。注意不是规范化的 `key`——这与 sema 的"入口路径语义"一致（诊断回显用原始写法）。用途是符号名/调试信息。
- `items`：顶层项列表（函数 / extern / 全局变量）。
- `types`：具名类型定义列表（struct / class / trait）。

### 5.2 `HirItem` 与函数项（item.rs:41-91）

```rust
pub enum HirItem {
    Function(HirFunction),
    ExternFunction(HirFunction),
    Global(HirGlobal),
}

pub struct HirFunction {
    pub source_module: ModuleId,
    pub is_lambda: bool,
    pub id: HirFunctionId,
    pub signature: FunctionSignature,
    pub params: Vec<LocalSlot>,
    pub body: HirBlock,
    pub visibility: HirVisibility,
}

pub struct LocalSlot { pub id: LocalId, pub name: String, pub ty: HirType }

pub struct HirGlobal {
    pub name: String,
    pub ty: HirType,
    pub init: Option<HirOperand>,
    pub is_mutable: bool,
    pub visibility: HirVisibility,
}
```

- **`HirItem::Function` / `ExternFunction`**：共享 `HirFunction` 结构，区别只在变体——extern 函数没有函数体（body 是空块）。`build_module` 里 extern 项的 visibility 被强制设为 `Public`。
- **`HirFunction.source_module` / `is_lambda`**：2026-11-15 契约修订加入。`source_module` 记录**定义归属模块**，`is_lambda` 标记是否 lambda。原因：`build_hir` 最后一步要把 lambda 归位到定义模块——如果 lambda 在 A 模块的函数里出现但源码属于 B，`source_module` 才是正确的归属。
- **`params: Vec<LocalSlot>`**：函数体的参数槽位。实例方法的 `self` 也在这里（见 3.5 节的 receiver 讨论）。`LocalSlot.name` 保留源码参数名（用于调试和默认实参的名字匹配）。
- **`body: HirBlock`**：函数体指令块。
- **`visibility`**：可见性，见下。

**`HirGlobal`**：全局变量结构存在，但当前 codegen 报"暂不支持全局变量"（`compiler-design.md:283`）——又一个"HIR 有数据模型但后端未支持"的例子。

### 5.3 可见性与类型定义（item.rs:93-148）

```rust
pub enum HirVisibility { Public, Private, Package }

pub enum HirTypeDef {
    Struct { name: String, fields: Vec<HirField>, methods: Vec<HirMethod> },
    Class  { name: String, parent: Option<(ModuleId, String)>, fields: Vec<HirField>, methods: Vec<HirMethod> },
    Trait  { name: String, methods: Vec<HirMethod> },
}

pub struct HirMethod { pub name: String, pub signature: FunctionSignature, pub is_static: bool }
pub struct HirField { pub name: String, pub ty: HirType, pub is_weak: bool, pub is_static: bool }
```

- `HirVisibility` 三档直接对应源码的 `pub` / `pri` / `pkg`（`Scope::from_token` 在 AST 层已经转换好）。
- `HirTypeDef::Class.parent` 是 `Option<(ModuleId, String)>`——父类用"模块 + 名字"的名义键，支持跨模块继承。
- `HirMethod.is_static` 区分静态方法（无 receiver）和实例方法（有 receiver）。
- `HirField.is_weak` / `is_static`：**当前都必须是 false**——`layout.rs` 遇到 `is_static` 字段报 "static field storage" 未实现，`validate_declaration` 遇到 weak 成员报未实现。字段结构保留这两个标志是为了忠实记录 sema 的信息并让不支持检测有地方可查。

### 5.4 序列化形态

所有数据模型都派生 `Serialize, Deserialize`。JSON 形态采用 Rust enum 的外部标记（externally tagged）默认表示：

```json
{ "Function": { "source_module": 0, "is_lambda": false, "id": "m0::sum", ... } }
{ "Int": "I32" }
{ "Struct": [0, "Point"] }
{ "Instr": { "Binary": { "op": "Add", "lhs": {"Local": 1}, ... } } }
{ "Break" }
```

这种形态对机器友好（无歧义），但对人略啰嗦。第 17 章给出大段真实输出，读几段就习惯了。

---

## 第 6 章 构建子系统总览 `build.rs`

### 6.1 三张类型投影表的契约

`build.rs` 开头（`build.rs:1-31` 附近的模块文档）明确：HIR 从 sema 的三张投影表取类型，键均为 `(ModuleId, Span)`：

- `sema.expression_types`：每个**表达式**的定型结果。HIR 里所有 `dst_ty`、局部类型的查询都走这里。
- `sema.annotation_types`：每个**类型标注**（`TypeExpr`）的解析结果。用于 `as` cast 目标、`is` 类型测试目标、变量标注等。
- `sema.declaration_types`：每个**声明**（函数、变量等）的类型。用于函数签名、变量初始化类型等。

**HIR 不重新解析类型名**。看到 AST 里的 `TypeExpr` 时，它不会去查符号表解析 `Point` 是哪个模块的类型，而是直接 `sema.annotation_types.get(module, annotation.span())` 拿 `TypeView`。这保证了"语义分析是类型的唯一权威"——HIR 永远不会和 sema 的类型判断不一致。

### 6.2 两个 Builder

```rust
pub(crate) struct ModuleBuilder<'a> {          // build.rs:74-90
    pub(crate) sema: &'a SemaResult,
    pub(crate) module: ModuleId,
    pub(crate) program: &'a Program,           // 当前模块的 AST
    pub(crate) signatures: HashMap<HirFunctionId, FunctionSignature>,
    pub(crate) functions: Vec<(String, HirFunctionId)>,
    pub(crate) top_signatures: HashMap<String, FunctionSignature>,
    pub(crate) pending: Vec<Vec<HirFunction>>, // 每个函数的 lambda 列表
    pub(crate) struct_items: Vec<&'a Item>,
    pub(crate) class_items: Vec<&'a Item>,
    pub(crate) trait_items: Vec<&'a Item>,
}

pub(crate) struct BodyBuilder<'a> {            // build.rs:93-119
    pub(crate) sema: &'a SemaResult,
    pub(crate) module: ModuleId,
    pub(crate) functions: Vec<(String, HirFunctionId)>,  // 持副本
    pub(crate) next_local: u32,
    pub(crate) local_types: Vec<HirType>,
    pub(crate) locals: Vec<(String, LocalId)>,
    pub(crate) capture_names: HashSet<String>,
    pub(crate) loop_depth: usize,
    pub(crate) return_type: Option<HirType>,
    pub(crate) params: Vec<LocalSlot>,
    pub(crate) with_receiver: Option<(HirType, HirOperand)>,
    pub(crate) lambdas: Vec<HirFunction>,
}
```

两个 Builder 的分工：

- **`ModuleBuilder`** 管模块级状态：所有函数签名（供前向调用）、待处理的 lambda 批次、聚合类型项列表。它持有 `&Program`（当前模块 AST）和 `&SemaResult`。
- **`BodyBuilder`** 管函数体级状态：局部编号分配器、名字 → Local 映射、循环深度、返回类型、`with_receiver` 上下文、当前函数产出的 lambda 列表。

**为什么 `BodyBuilder.functions` 是副本而非引用**：源码注释（`build.rs:93-119`）说明——降级过程中还要 `push` lambda 到 `pending`，而 `ModuleBuilder` 的 `functions` 是共享的；`BodyBuilder` 持有副本避免借用冲突。这是一个典型的 Rust 所有权驱动的架构决策。

### 6.3 `BuildError`（error.rs）

```rust
pub struct BuildError {
    pub kind: BuildErrorKind,
    pub module: Option<ModuleId>,
    pub span: Option<Span>,
}

pub enum BuildErrorKind {
    InvalidInput,
    OpaqueType,
    UntypedLiteral { what: String },
    UnknownTypeName { name: String },
    UnionUnsupported,
    GenericUnsupported,
    Unimplemented { node: String },
    MissingType { what: String },
    MissingExpressionType,
    ImplicitCast { from: HirType, to: HirType },
}
```

`Display` 输出 `"HIR 降级失败：{kind}"`。`at(kind)` 构造无位置错误；`with_site(module, span)` 附加位置（只填第一个非空值）。

**错误分类的语义**（error.rs 注释）：BuildError 只在两类情况下产生——"语义已通过但后端不支持"（`Unimplemented`、`UnionUnsupported`、`GenericUnsupported`、`OpaqueType` 等）或"类型集合不一致"（`MissingType`、`MissingExpressionType`、`ImplicitCast`、`UntypedLiteral`）。`InvalidInput` 用于"上游有错误"（`has_errors()` 为 true、模块 syntax_failed/load_failed）。

**`ImplicitCast{from, to}` 的意义**：`coerce`（`stmt.rs:250-292`）在遇到需要隐式转换但 `castable(from, to)` 为 false 时报这个错——HIR 只允许显式 Cast 指令或已判定的安全隐式转换，绝不允许后端"悄悄"转换。

### 6.4 构建子模块的责任矩阵

| 文件 | 职责 | 主要入口 |
|------|------|----------|
| `module.rs` | 顶层编排、模块/函数降级、声明校验 | `build_hir`、`build_module`、`build_function` |
| `types.rs` | 类型定义、方法函数、类型摘要 | `finish_types`、`method_functions`、`annotation_type` |
| `layout.rs` | 继承布局展开（唯一字段索引来源） | `instance_fields` |
| `context.rs` | BodyBuilder 的上下文操作与类型查询 | `expr_type`、`in_definition_scope`、`with_scope` |
| `expr.rs` | 表达式降级主分派 | `build_expression`、`build_typed_expression` |
| `operators.rs` | 一元/二元、短路、赋值、左值槽 | `build_unary`、`build_binary`、`build_assignment` |
| `lvalue.rs` | 地址构造与间接读 | `build_address`、`read_address` |
| `stmt.rs` | 语句与控制流 | `build_statement`、`emit_statement` |
| `loops.rs` | for-in 循环 | `emit_for_in` |
| `call.rs` | 调用与默认实参 | `build_call`、`append_default_arguments` |
| `aggregate.rs` | struct 初始化、match、lambda、字面量 | `build_struct_init`、`build_match`、`build_lambda`、`build_literal` |
| `coalesce.rs` | `??` 空合并 | `build_coalesce` |
| `helpers.rs` | 可见性、运算符映射、转换判定、字面量解析 | `binary_op_of`、`castable`、`parse_int_literal` |

---

## 第 7 章 入口 `build_hir` 与模块降级（module.rs）

### 7.1 `build_hir`（module.rs:9-51）——总流程

```rust
pub fn build_hir(sema: &SemaResult) -> Result<HirProgram, BuildError> {
    // 1. 全局错误检查
    if sema.diagnostics.has_errors() {
        return Err(BuildError::at(BuildErrorKind::InvalidInput));
    }
    // 2. 逐模块检查加载/语法失败
    for module in &sema.modules.modules {
        if module.syntax_failed || module.load_failed {
            return Err(BuildError::at(...).with_site(module.id, ...));
        }
    }
    // 3. 逐模块降级
    let mut modules = Vec::new();
    for module in &sema.modules.modules {
        modules.push(build_module(sema, module)?);
    }
    // 4. lambda 归位：收集所有 is_lambda 的函数项，去重，按 source_module 放回
    // 5. 返回 HirProgram { modules }
}
```

四个阶段逐一说明：

**阶段 1：全局错误检查。**
`sema.diagnostics.has_errors()` 是整个诊断树的统一判定。只要有任何词法/语法/语义错误，HIR 直接拒绝——**不跳过坏模块、不尝试部分降级**。这与 sema 的"级联抑制尽量多报"形成对比：HIR 的输入必须是"语义完全通过"的程序。

**阶段 2：模块级加载/语法失败检查。**
即使 `has_errors()` 为 false（比如某个模块只有 warning），仍要检查每个 `AnalyzedModule` 的 `syntax_failed` / `load_failed` 标志。这是防御性检查——理论上这些标志为真时一定已有诊断，但 HIR 不信任这一点，显式检查。

**阶段 3：逐模块降级。**
调用 `build_module`，任何一个模块失败即整体 `?` 返回——fail-fast。

**阶段 4：lambda 归位。**
这是最容易被忽略的一步。`build_module` 降级函数体时遇到 lambda 会把它放进 `pending` 并在模块 `items` 末尾追加，但 lambda 的 `source_module` 才是它的真实归属。如果 lambda 在 A 模块的函数里首次展开（比如 A 调用 B 的函数，B 的函数里有 lambda），HIR 要把它放回 B。实现上：

- 遍历所有已生成的 `HirModule.items`，把 `HirItem::Function(f)` 且 `f.is_lambda` 的项收集起来。
- 按 `f.id` 去重（`HashSet`）——同一个 lambda 可能在多处展开。
- 从每个模块的 `items` 中移除 lambda，再按 `f.source_module` 放回对应模块。
- 这样保证 lambda 实体只存在一份，且归属正确。

**为什么这样设计**：`docs/hir-handover.md` 和 2026-11-15 契约修订强调 "HirFunction.source_module/is_lambda 显式记录 lambda 属主"。这是为了支持跨模块调用场景——函数体降级是"按模块顺序"进行的，lambda 可能在意外的模块上下文里被展开。归位步骤消除了这个顺序依赖。

### 7.2 `build_module`（module.rs:54-125）

```rust
fn build_module(sema: &SemaResult, module: &AnalyzedModule) -> Result<HirModule, BuildError> {
    let mut builder = ModuleBuilder::new(sema, module.id, &module.program);
    builder.collect_declarations()?;             // ① 先登记全部顶层声明
    let mut items = Vec::new();
    for item in &module.program.items {
        match item {
            Item::Function(f) if f.is_extern => items.push(HirItem::ExternFunction(...)),
            Item::Function(f) => items.push(HirItem::Function(builder.build_function(...)?)),
            Item::Import(_) | Item::TypeAlias(_) => {}   // 跳过：类型信息已在 sema 表里
            Item::Struct(_) | Item::Class(_) | Item::Trait(_) => {}  // 走 finish_types
            Item::Directive(_) | Item::DirectiveBlock(_) | Item::Statement(_) => {
                return Err(Unimplemented { node: ... })
            }
        }
    }
    let types = builder.finish_types()?;         // ② 类型定义 + 继承布局
    items.extend(builder.method_functions()?);   // ③ 方法体降级为独立函数项
    items.extend(builder.pending.into_iter().flatten());  // ④ 本模块的 lambda
    Ok(HirModule { id: module.id, path: module.path.display().to_string(), items, types })
}
```

**关键顺序：先登记声明，后降级函数体。**

`collect_declarations()`（144-172 行）遍历所有 `Item::Function`，为每个函数计算 `HirFunctionId` 和 `FunctionSignature`，存入三张 map：

- `signatures: HashMap<HirFunctionId, FunctionSignature>`——按稳定键查。
- `functions: Vec<(String, HirFunctionId)>`——按源码名字查（用于 Identifier 表达式解析）。
- `top_signatures: HashMap<String, FunctionSignature>`——按名字查签名（用于 `build_function` 取自己的签名）。

这个"预登记"让函数体里的调用（包括前向调用和递归）都能立即拿到签名，无需多遍扫描。

**`Item` 各变体的处理**：

- `Extern Function` → `HirItem::ExternFunction`，visibility 强制 `Public`（外部符号必须可见）。
- 普通 `Function` → `build_function`。
- `Import` / `TypeAlias` → 跳过。Import 的语义（名字绑定）已在 sema 的 `BindingTable` 里；TypeAlias 的类型信息已在 `declaration_types` 里。HIR 不需要为它们产出任何项。
- `Struct` / `Class` / `Trait` → 跳过（在 `finish_types` 里统一处理）。
- `CompilerDirective` / `DirectiveBlock` / `Statement`（顶层语句）→ `Unimplemented`。当前不支持顶层编译器指令/语句。
- match 是穷尽的——**新增 AST Item 变体时，这里会编译失败**，迫使维护者显式决定如何处理。这是项目"穷尽 match 防漏"约定在 HIR 入口的体现。

### 7.3 `function_signature` 与 `build_function`

**`function_signature`（180-205 行）**：

```rust
fn function_signature(&self, function: &Function) -> Result<FunctionSignature, BuildError> {
    let view = self.sema.declaration_types.get(self.module, function.span)
        .ok_or_else(|| ... MissingType ...)?;
    let HirType::Function(sig) = HirType::from_view(view)? else { ... };
    Ok(FunctionSignature { id: self.function_id(&function.name), ..*sig })
}
```

从 `declaration_types` 取函数的 `TypeView`（应该是 `Function`），转成 `HirType::Function(sig)`，然后**覆盖 `sig.id` 为真实的函数键**——因为 sema 的 `FunctionSig` 对匿名/lambda 可能是 `name=None`，而 HIR 的顶层函数必须有确定的 `m{module}::{name}` 键。

**`build_function`（208-259 行）**：

```rust
fn build_function(&mut self, function: &Function, is_extern: bool) -> Result<HirFunction, BuildError> {
    let signature = self.top_signatures.get(&function.name).cloned()...;
    let mut body_builder = BodyBuilder::new(self.sema, self.module, self.functions.clone());
    body_builder.return_type = Some(signature.ret.clone());
    // 形参先 declare_param（不发 Alloc，codegen 从 get_nth_param 拷入）
    for (param, ty) in function.params.iter().zip(&signature.params) {
        body_builder.declare_param(&param.name, ty.clone());
    }
    let body = match &function.block {
        Some(block) => body_builder.build_statement(&Statement::Block(...))?,
        None => HirBlock::new(),   // extern 无体
    };
    let lambdas = std::mem::take(&mut body_builder.lambdas);
    self.pending.push(lambdas);
    Ok(HirFunction {
        source_module: self.module,
        is_lambda: false,
        id: signature.id.clone(),
        signature,
        params: body_builder.parameter_slots(),
        body,
        visibility: visibility_of(function.scope),
    })
}
```

**形参为什么不发 `Alloc`**：源码注释明确——codegen 在函数入口从 LLVM 的 `get_nth_param(i)` 拿到参数值，直接存进为 LocalId 分配的槽位。`declare_param` 只建立名字 → LocalId 映射和 LocalSlot 记录，不发初始化指令。这与普通局部变量（`emit_variable` 发 `Alloc`）不同。

### 7.4 `validate_declaration`（module.rs:263-324）——不支持构造的闸门

这个函数在 `collect_declarations` 里对每个声明调用，集中拒绝当前 HIR 不支持的构造：

- **Trait 有 members** → `Unimplemented{"trait members"}`。空 trait 允许（降级成 `HirTypeDef::Trait` 带空 methods）。
- **泛型 type alias** → `Unimplemented{"generic type alias"}`。
- **带编译器指令的类型 / 泛型类型** → `Unimplemented`。
- **weak / 带编译器指令 / 泛型的成员** → `Unimplemented`。

这些检查与 `ty.rs` 的类型转换检查形成两层防线：声明层的结构性不支持在这里拦，表达式/类型层面的不支持在 `HirType::from_view_with` 拦。


---

## 第 8 章 类型定义与继承布局（types.rs / layout.rs）

### 8.1 `annotation_type`（types.rs:6-19）——类型标注的唯一下降口

```rust
pub(crate) fn annotation_type(
    sema: &SemaResult,
    module: ModuleId,
    annotation: &TypeExpr,
) -> Result<HirType, BuildError> {
    let view = sema.annotation_types.get(module, annotation.span())
        .ok_or_else(|| ...)?;
    HirType::from_view(view)
}
```

这是所有 **AST `TypeExpr` → `HirType`** 转换的统一出口。用在：

- `as` cast 的目标类型（`expr.rs` 的 Cast 分支）
- `is` 类型测试的目标类型（`expr.rs` 的 Is 分支）
- match 的 Type pattern（`aggregate.rs` 的 `build_match`）
- 变量/参数标注（但变量的初始化类型主要走 `expression_types` / `declaration_types`）

**关键点**：它只消费 `sema.annotation_types`，不解析 AST 里的类型名文本。看到 `i32`、`Point`、`func(i32)->i32` 这些标注时，HIR 查表拿 `TypeView`，不自己做名字解析。这保证了 HIR 与 sema 的类型判断完全一致。

### 8.2 `finish_types`（types.rs:25-65）——生成类型定义

`finish_types` 遍历 `ModuleBuilder` 收集的 `struct_items`、`class_items`、`trait_items`，为每个聚合类型生成 `HirTypeDef`：

```rust
// 伪代码（按源码逻辑重建）
for item in &self.struct_items {
    let summary = type_summary(sema, module, name)?;       // 从 sema.types.summaries
    let fields = instance_fields(sema, module, name, span)?; // 继承布局展开
    let methods = hir_methods(&summary)?;                   // 方法签名列表
    types.push(HirTypeDef::Struct { name, fields, methods });
}
for item in &self.class_items {
    let parent = summary.parent.map(|p| (p.module, p.name));
    // 同上，但 HirTypeDef::Class 带 parent
}
for item in &self.trait_items {
    // 空 trait：methods 为空（有成员的 trait 已被 validate_declaration 拒绝）
    types.push(HirTypeDef::Trait { name, methods: vec![] });
}
```

**三个信息来源**：

- `sema.types.summaries`（`TypeSummary` / `MethodSummary`）：类型的摘要信息，包括父类、方法签名列表。
- `instance_fields`（layout.rs）：字段布局（含继承展开）。
- `hir_methods`：把 `MethodSummary` 转成 `HirMethod`。

### 8.3 `method_functions`（types.rs:72-143）——方法体降级为独立函数项

这是 HIR 设计里一个精妙的决定：**所有方法体都降级成模块级的独立 `HirItem::Function`，而不是塞在 `HirTypeDef` 里**。

流程（对每个 struct/class 的 `Method` member 且 `block.is_some()`）：

1. `method_signature`（146-170）：从 `sema.types.summaries` 找对应 `MethodSummary`，用 `FunctionSignature::from_view` 转签名（id 通过 `module_function_id` 回调注入 → `m{module}::{类型}::{方法}`）。
2. 新建 `BodyBuilder`，`return_type = signature.ret`。
3. **receiver 处理**：若 `signature.receiver == true`，`declare_param("self", receiver_ty)`，其中 receiver 类型：
   - struct → `HirType::Pointer(Struct{module, name})`（**结构体实例方法用指针传递 self**，避免拷贝，也支持修改字段）。
   - class → `HirType::Class{module, name}`（class 是引用语义，self 直接是 Class 类型）。
4. 遍历 AST 参数，跳过无注解的 `self`（方法声明里 `self` 可能出现在参数表），并**重新对齐下标**——因为 `signature.params` 不含 receiver，而 AST params 可能含。
5. `build_statement(Statement::Block)` 降级函数体。
6. 返回 `HirItem::Function`（注意不是 `HirTypeDef` 的一部分）。

源码注释明确："方法体也要降级为独立函数项，codegen 只需遍历模块函数项。"这大大简化了后端——codegen 不需要专门处理"类型内嵌方法"，所有可调用实体统一在 `items` 里。

`hir_methods`（194-214）则只产出**方法签名元数据**（`HirMethod{name, signature, is_static}`）放进 `HirTypeDef.methods`，供类型信息查询；真正的函数体在 `items` 里。二者通过相同的 `HirFunctionId`（`m{module}::{类型}::{方法}`）关联。

**`receiver` 不在 `signature.params`**：`FunctionView::receiver` 已把 self 从参数表剥离（见 2026-11-15 契约修订），`hir_methods` 忠实保留这一点。而 `HirFunction.params` 里有 self（作为 LocalSlot）——再次印证 3.5 节的"签名 vs 槽位"区分。

### 8.4 `instance_fields`（layout.rs:5-55）——唯一字段索引来源

这是保证"类型定义里的字段顺序"与"字段访问时的 index"一致的关键函数：

```rust
pub(crate) fn instance_fields(
    sema: &SemaResult,
    module: ModuleId,
    name: &str,
    span: Span,
) -> Result<Vec<HirField>, BuildError> {
    // 1. 沿 sema.types.summaries 的 parent 链向上收集（子 → 父）
    let mut chain = vec![...];   // 当前类型
    let mut visited = HashSet::new();
    while let Some(parent) = ... {
        if !visited.insert(parent) {
            return Err(Unimplemented { node: "cyclic inheritance" });
        }
        chain.push(parent);
    }
    // 2. 反转成父 → 子，展开字段
    let fields: Vec<HirField> = chain.into_iter().rev()
        .flat_map(|ty| ty.fields_from_summary(...))
        .map(|field| {
            if field.is_static { return Err(Unimplemented { node: "static field storage" }); }
            Ok(HirField {
                name: field.name,
                ty: HirType::from_view(&field.ty)?,
                is_weak: field.is_weak,
                is_static: false,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(fields)
}
```

**关键点**：

1. **父 → 子展开**：子类字段排在父类字段之后。这样 `FieldAddress.index` 在类型定义和访问点计算方式完全一致——两边都调 `instance_fields`，按返回顺序取索引。
2. **跨模块继承**：parent 是 `(ModuleId, String)`，查 `sema.types.summaries` 时用模块 id 定位，所以父类可以在另一个模块。
3. **环检测**：`visited` 集合防止继承环导致无限循环，遇到环报 `Unimplemented{"cyclic inheritance"}`。
4. **static 字段拒绝**：`is_static` 字段报 "static field storage" 未实现。
5. **索引唯一来源**：源码注释明确"字段索引只有一个来源，类型定义与字段访问共用"。`build_struct_init` 和 `field_slot`（`operators.rs`）都调 `instance_fields`，绝不各算一套——这避免了"定义时索引 0 是 x，访问时索引 0 是 y"的灾难。

**`field_slot` 的查找**（`operators.rs:256-288`）：拿到 `instance_fields` 后 `rev().find(|f| f.name == name)`——**反转后查找意味着子类字段优先**（同名遮蔽父类）。找到返回 `(index, ty)`，index 是 `instance_fields` 里的位置。

---

## 第 9 章 上下文管理 `context.rs`

`context.rs` 是 `BodyBuilder` 的 `impl` 块，管函数体降级过程中的"局部上下文"。理解它是理解表达式/语句降级的前提。

### 9.1 局部编号与名字解析

```rust
fn fresh_local(&mut self, ty: HirType) -> LocalId {          // 50-55
    let id = LocalId(self.next_local);
    self.next_local += 1;
    self.local_types.push(ty);
    id
}

fn declare_param(&mut self, name: &str, ty: HirType) { ... }  // 58-63
fn parameter_slots(&self) -> Vec<LocalSlot> { ... }            // 66-68
fn declare_var(&mut self, name: &str, ty: HirType) -> LocalId { ... }  // 71-75

fn lookup_local(&self, name: &str) -> Option<LocalId> {       // 83-89
    self.locals.iter().rev().find(|(n, _)| n == name).map(|(_, id)| *id)
}
fn local_type(&self, id: LocalId) -> &HirType { ... }          // 78-80
```

- `fresh_local` 是**唯一**的 LocalId 分配入口——所有指令的 `dst` 都从这里分配，保证函数体内单调递增、无重复。它同时把类型记进 `local_types`，供 `local_type(id)` 反查。
- `lookup_local` 用 `rev().find`——**后声明的遮蔽先声明的**，正确处理词法遮蔽（shadowing）。
- `declare_param` / `declare_var` 都往 `locals` 里压 `(name, id)`；区别是 param 还进 `params` 列表（成为 `HirFunction.params`），var 不进。

### 9.2 词法作用域（with_scope）

```rust
fn with_scope<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {  // 92-100
    let mark = self.locals.len();
    let result = f(self);
    self.locals.truncate(mark);   // 无论成功失败都恢复
    result
}
```

用一个 `mark = locals.len()` 做快照，闭包执行后 `truncate` 恢复。用于 `{}` 块、`unsafe` 块、`for` 的 init 段、`with` 绑定——所有"退出作用域后绑定失效"的场合。注意它在 `result` 算出之后才 truncate，所以即使失败也恢复（因为 `truncate` 在 `result` 之后、返回之前执行；但严格说如果 `f` panic 则不会恢复——不过 panic 本身就会终止降级）。

### 9.3 定义环境切换（in_definition_scope）——默认实参/字段初值的核心

```rust
fn in_definition_scope<T>(
    &mut self,
    module: ModuleId,
    locals: Vec<(String, LocalId)>,
    f: impl FnOnce(&mut Self) -> Result<T>,
) -> Result<T> {                                              // 7-25
    // 保存当前 module / locals / functions / capture_names / with_receiver
    let saved = (...);
    // 替换成定义处的环境
    self.module = module;
    self.locals = locals;
    // ...
    let result = f(self);
    // 还原
    self.module = saved.0;
    // ...
    result
}
```

**为什么需要这个**：默认实参和字段初值在**定义处**的词法环境求值，而不是调用处。例如：

```novella
func f(a: i32, b: i32 = a) { ... }   // b 的默认值引用 a
```

在调用 `f(5)` 展开默认实参 `b` 时，必须在 `f` 的定义环境里解析 `a`（指向 `f` 的参数 a），而不是调用者的 `a`。`in_definition_scope` 临时把 `BodyBuilder` 的 `module`、`locals`、`functions`、`capture_names`、`with_receiver` 换成定义处的快照，执行 `f` 后再还原。

源码注释强调："保留调用方临时值编号与已发射指令"——即 `next_local`、`local_types`、当前 block 不变，只在名字解析和模块上下文上切换。这样默认实参里生成的临时 Local 编号不会与调用方冲突。

这个函数在两处被调用：

- `call.rs::append_default_arguments`：`in_definition_scope(module, bindings, |b| b.build_typed_expression(default, ...))`。
- `aggregate.rs::build_struct_init`：字段缺省 initializer 的求值。

### 9.4 类型查询（expr_type / expr_type_pref）

```rust
fn expr_type(&mut self, span: Span) -> Result<HirType, BuildError> {   // 107-117
    let view = self.sema.expression_types.get(self.module, span)
        .ok_or_else(|| BuildError::at(MissingExpressionType).with_site(...))?;
    HirType::from_view(view, |v| self.synth_function_id(v))
}

fn expr_type_pref(&mut self, span: Span, preferred: Option<&HirType>)
    -> Result<HirType, BuildError> {                                    // 125-139
    let view = self.sema.expression_types.get(self.module, span)
        .ok_or_else(|| ...)?;
    HirType::from_view_with(view, preferred, |v| self.synth_function_id(v))
}
```

源码注释明确："这是 HIR 所有 dst_ty/局部类型的唯一事实来源。" 任何需要知道"这个表达式是什么类型"的地方，都调这两个函数查 sema 表，绝不猜测。

**`expr_type` vs `expr_type_pref`**：后者多一个 `preferred` 下推提示，用于未定型字面量落地。例如 `Binary` 的 `result_ty = expr_type_pref(span, preferred)`——如果调用方知道期望类型（比如赋值目标类型），传给 `preferred` 让字面量落地。

**`synth_function_id`（142-147）**：作为 `from_view` 的 id 回调——`view.name` 为 `Some` 时生成 `m{module}::{name}`，为 `None` 时生成 `m{self.module}::anon#{params.len()}`（占位）。注意这里的 module 用 `view.module`（函数定义所在模块）而非 `self.module`（当前查询模块）——但 fallback 的 anonymous 用 `self.module`。

### 9.5 with_receiver 上下文

`with_receiver: Option<(HirType, HirOperand)>` 保存当前 `with expr as binding` 的接收者。`emit_with`（`stmt.rs`）设置它，`implicit_member`（`operators.rs`）消费它，`emit_with` 结束时还原。这是 `with` 隐式成员访问（`with pair as p { p.x }`）能在 HIR 里工作的机制。


---

## 第 10 章 表达式降级主分派（expr.rs）

### 10.1 两个入口与"下推提示"

```rust
pub(crate) fn build_expression(        // expr.rs:11-17
    builder: &mut BodyBuilder,
    expr: &Expression,
    block: &mut HirBlock,
) -> Result<HirOperand, BuildError> {
    build_expression_inner(builder, expr, None, block)
}

pub(crate) fn build_typed_expression(  // expr.rs:20-29
    builder: &mut BodyBuilder,
    expr: &Expression,
    expected: &HirType,
    block: &mut HirBlock,
) -> Result<HirOperand, BuildError> {
    let value = build_expression_inner(builder, expr, Some(expected), block)?;
    let ty = builder.expr_type_pref(expr.span(), Some(expected))?;
    coerce(builder, value, &ty, block)
}
```

- `build_expression`：无期望类型，用于"结果类型由表达式自己决定"的场合（比如 `if` 条件、独立表达式语句）。
- `build_typed_expression`：带期望类型 `expected`，做两件事——把 `expected` 作为 preferred 传给内层降级（让字面量落地到期望类型），再对结果做 `coerce`（确保最终操作数类型与 sema 表一致）。

**`preferred` 的双重作用**：既影响 `expr_type_pref` 的类型查询（字面量落地），又通过 `build_expression_inner` 传递下去（某些分支直接用它，比如 Tuple 元素类型、Array 元素类型）。

### 10.2 `build_expression_inner` 全变体分派（expr.rs:31-278）

这是 HIR 表达式降级的"总开关"，穷尽 `Expression` 的所有变体。逐分支说明：

**（1）`Literal` → `build_literal`**（见 13.4 节）。产出 `IntConst`/`UntypedInt`/`FloatConst`/`UntypedFloat`/`BoolConst`/`StringConst`/`Null`。

**（2）`Identifier`**——最复杂的分支之一，按优先级依次尝试：

1. **查局部**：`lookup_local(name)`。找到后：
   - 若局部类型是 `Pointer(Struct)` 且名字是 `self` → `read_address` 取值（解引用，得到结构体值用于字段访问）。
   - 否则 → 发 `Load{dst: fresh, src: local}` **生成快照**并返回 `Local(dst)`。这是"局部读取生成快照"不变量的落地点——每次读局部变量都产生新的 Local，绝不复用同一个 Local 代表"变量当前值"（避免后续赋值影响已读的值）。
2. **lambda 捕获检查**：`capture_names.contains(name)` → `Unimplemented{"lambda capture"}`。lambda 捕获明确不支持。
3. **函数名匹配**：`self.functions` 里按名字找到 → 返回 `FunctionPtr(id)`（函数当值使用）。
4. **表达式类型表里是具名非 receiver 的 Function** → 也返回 `FunctionPtr`。这处理"通过 sema 类型推断识别出的函数值"的情况。
5. **隐式成员**：`implicit_member(name)`（见 10.3）→ `with`/`self` 的隐式字段访问。
6. 都不匹配 → `Unimplemented{"identifier"}`。

**（3）`Paren`** → 递归 `build_expression_inner(inner, preferred, block)`。括号不产生任何 HIR 指令。

**（4）`Unary`** → `build_unary`（第 11 章）。

**（5）`Binary`** → `build_binary`（第 11 章）。

**（6）`Assignment`** → `build_assignment`（第 11 章）。

**（7）`Call`**：若 `type_args` 非空 → `GenericUnsupported`；否则 `build_call`（第 12 章）。

**（8）`Ternary`（三元 `cond ? a : b`）**：典型的"分支 + 结果槽"模式：

```
result = fresh_local(ty)             // Alloc
Alloc{id: result, ty, init: None}
cond = build_expression(condition)
then_block: value_a → Store{id: result, value: a}
else_block: value_b → Store{id: result, value: b}
If{cond, then_block, else_block}
返回 Local(result)
```

结果槽在所有分支之前分配，两个分支各自把值写进去——保证"只求值被选中的分支"。

**（9）`Is`（`expr is Type`）**：
```
operand = build_expression(expr)
operand_ty = expr_type(expr.span())
target_ty = annotation_type(target)      // 目标类型从 sema 标注表
TypeTest{operand, operand_ty, target: target_ty, dst: fresh}
```

**（10）`Cast`（`expr as Type`）**：
```
target_ty = annotation_type(target)
expr_ty = expr_type_pref(expr.span(), Some(&target_ty))
value = build_expression_inner(expr, Some(&target_ty))
coerce(value, &expr_ty)              // 生成 Cast 指令或检查可转换
```

**（11）`Match`** → `build_match`（13.2 节）。

**（12）`MemberAccess`**：
- 先查表达式类型：若是 `Function` 且 `!receiver`（静态方法/函数值的成员访问）→ `FunctionPtr`。
- 若是 `Function` 且 `receiver == true`（**绑定方法值**）→ `Unimplemented{"bound method value"}`。即 `obj.method`（不调用，当函数值用）不支持。
- 否则（字段访问）→ 求值 target + `FieldLoad`（见 11.4 节的 `field_slot`）。

**（13）`IndexAccess`**：
```
target = build_expression(target)
element_ty = expr_type_pref(expr.span(), preferred)
index = build_typed_expression(index, &Usize)    // 索引定型为 usize
IndexLoad{target, element_ty, index, dst: fresh}
```

**（14）`StructInit`** → `build_struct_init`（13.1 节）。

**（15）`Lambda`** → `build_lambda`（13.3 节）。

**（16）`New` / `Await`** → `Unimplemented`。动态对象创建、异步等待不支持。

**（17）`Tuple`**：
```
tuple_ty = expr_type_pref(span, preferred)    // 必须是 Tuple
elements = zip(items, tuple_ty.elements).map(|(item, ty)| build_typed_expression(item, ty))
Tuple{elements, ty: tuple_ty, dst: fresh}
```

**（18）`ArrayLiteral`**：
```
arr_ty = HirType::Array(element_ty)  // element_ty 从 expr_type_pref 取
elements = items.map(|item| build_typed_expression(item, &element_ty))
Array{elements, element_ty, dst: fresh}
```

**（19）`ObjectLiteral`** → `Unimplemented`。动态对象字面量不支持（只有具名 struct init 支持）。

**（20）`This`**：查 `self` 局部。若类型是 `Pointer(Struct)` → `read_address` 取值（用于 `self.x` 字段访问）；否则直接 `Load` 快照。

**match 穷尽性**：`Expression` 是 enum，这个 match 穷尽所有变体。**新增 AST 表达式变体时，这里会编译失败**——这是项目"穷尽 match 防漏"约定的又一次体现。每个不支持的构造都有显式的 `Unimplemented`，没有 `_` 兜底。

### 10.3 隐式成员访问（`implicit_member`，operators.rs:291-330）

`with expr as binding { ... }` 里，绑定名（如 `binding`）可以直接访问接收者的字段（`binding.x`）——这里的 `x` 在 AST 里可能就是个裸 `Identifier`，HIR 要靠 `with_receiver` 上下文解析：

```
1. 若 with_receiver = Some((ty, value))：
   field_slot(ty, name) → (index, field_ty)
   FieldLoad{target: value, target_ty: ty, field: name, index, ty: field_ty, dst}
2. 否则查 self 局部：
   self_ty = local_type(self)
   field_slot(self_ty, name) → 同上
3. 都失败 → 返回 None（调用方继续尝试其他解析）
```

`field_slot`（256-288）先解 Pointer 包装（结构体 self 是 `Pointer(Struct)`），然后从 `instance_fields` 里 `rev().find`（子类字段优先）取 index/ty。target 非 Struct/Class → `Unimplemented{"field access on non-aggregate"}`。

---

## 第 11 章 运算符、短路与赋值（operators.rs）

### 11.1 `build_unary`（operators.rs:6-73）

**`AddressOf`**（取地址 `&expr`）→ `build_address`（见 11.5 节）。

**`PostIncrement` / `PostDecrement`（`x++` / `x--`）**——注意"返回旧值"语义：

```
address = build_address(operand)                  // 定位一次
old = read_address(address)                       // 读旧值
one = IntConst(1) 或 FloatConst(1.0)
rhs_ty = address 指向 Pointer 时 Isize 否则 old 的类型
new = Binary{Add/Sub, lhs: old, rhs: one, lhs_ty, rhs_ty, dst: fresh, dst_ty}
StoreIndirect{address, value: new}
返回 old                                          // 关键：返回旧值
```

**`DerefPointer`（`*p`）**：
```
operand_ty = expr_type(operand.span()) 成功则用，否则用 preferred ty.clone()
value = build_typed_expression(operand, &operand_ty)
op 映射：Plus→直接返回 value，Minus→Neg，Not→Not，BitNot→BitNot，DerefPointer→Deref
Unary{op, operand: value, dst: fresh, dst_ty: ty}
```

一元运算映射表：

| AST 一元 op | HIR 处理 |
|-------------|----------|
| `Plus`（`+x`） | 直接返回操作数（无指令） |
| `Minus`（`-x`） | `Unary{Neg}` |
| `Not`（`!x`） | `Unary{Not}` |
| `BitNot`（`~x`） | `Unary{BitNot}` |
| `AddressOf`（`&x`） | `build_address` |
| `DerefPointer`（`*p`） | `Unary{Deref}` |
| `PostIncrement`/`PostDecrement` | 读旧值 + Binary + StoreIndirect，返回旧值 |

### 11.2 `build_binary`（operators.rs:75-152）——类型驱动的运算符降级

这是 HIR 最需要小心的函数之一，因为它要同时处理：普通算术、比较、指针算术、指针差、身份比较、短路、`??`。

**分流**：
- `LogicalAnd` / `LogicalOr` → `build_short_circuit`
- `NullCoalesce` → `build_coalesce`

**其余运算符的类型解析**（顺序很重要）：

```rust
let hir_op = binary_op_of(op).ok_or_else(...)?;        // AST op → HirBinaryOp
let result_ty = builder.expr_type_pref(span, preferred)?;  // 结果类型
// 比较运算优先取已定型一侧（bool 结果不能定型操作数）
let value_ty = if pointer_math {
    left_ty                                          // 指针算术用左侧指针类型
} else if is_comparison(op) {
    expr_type(left.span).or_else(expr_type(right.span))  // 第一个成功的具体类型
} else {
    result_ty.clone()
};
```

**身份比较（`===` / `!==`）的拒绝**：
```
if matches!(op, IdentityEquals|IdentityNotEquals)
   && !matches!(value_ty, Boolean|Int|Float|Pointer|Class) {
    return Err(Unimplemented{"identity comparison of aggregates / strings"});
}
```
即标量/指针/class 可以身份比较，聚合与 string 不行。

**指针算术**：`pointer_math = !comparison && (left_ty is Pointer || right_ty is Pointer)`。此时：
- 位移量类型用 `Isize`。
- 若两侧都是 Pointer 且 op 是 Sub → `hir_op = PtrDiff`（指针差，结果 `Isize`）。
- 注释强调"指针算术保留整数位移不转指针"。

**最终发射**：
```
lhs = build_typed_expression(left, &value_ty)
rhs_ty = if pointer_math && 两侧都 Pointer { Isize } else { right_ty 或 Isize }
rhs = build_typed_expression(right, &rhs_ty)
Binary{op: hir_op, lhs, rhs, lhs_ty: value_ty, rhs_ty, dst: fresh, dst_ty: result_ty}
```

**为什么 Binary 要带 `lhs_ty`/`rhs_ty`**：后端看 `Binary{Add, Local(3), Local(4)}` 时，Local 本身不携带类型（只有编号）。没有这两个字段，后端不知道是 i32 加还是 i64 加、是有符号除还是无符号除、位移量是 i32 还是 isize。契约文档为此专门规定"Binary 必须携带两侧输入类型"。

### 11.3 `build_short_circuit`（operators.rs:154-190）——短路与 or

`and` / `or` 不发射 `Binary{LogAnd/LogOr}`，而是用分支实现短路：

```
dst = Alloc(Boolean)                        // 结果槽
lhs = build_expression(left)

rhs_block:
    rhs = build_expression(right)
    Store{id: dst, value: rhs}

stop_block:
    Store{id: dst, value: BoolConst(op == LogicalOr)}   // and→false, or→true

If {
    cond: lhs,
    then_block: if op == And { rhs_block } else { stop_block },
    else_block: if op == And { stop_block } else { rhs_block },
}
返回 Local(dst)
```

- `and`：左为真才求右边；左为假 → 短路结果 `false`。
- `or`：左为真 → 短路结果 `true`；左为假才求右边。

**关键点**：`stop_block` 的常量是 `BoolConst(op == LogicalOr)`——`or` 短路返回 `true`，`and` 短路返回 `false`。结果槽在所有分支前分配，两条路径各自 Store。

### 11.4 `build_assignment`（operators.rs:193-250）——求值顺序的核心

这是 HIR 保证副作用顺序的关键函数：

```rust
let ty = builder.expr_type(target.span())?;
let early_rhs = if is_compound {
    None                                          // 复合赋值：不在定位 LHS 前求 RHS
} else {
    Some(builder.build_typed_expression(value, &ty, block)?)  // 普通赋值：先求 RHS
};
let address = build_address(target, block)?;      // 定位 LHS（只一次）
let previous = if is_compound {
    Some(read_address(address, ty, block)?)        // 复合赋值：RHS 前读旧值
} else {
    None
};
let rhs_ty = if is_compound && 地址指向 Pointer { Isize } else { ty.clone() };
let rhs = early_rhs.unwrap_or(build_typed_expression(value, &rhs_ty, block)?);
if let Some(prev) = previous {
    let combined = Binary{compound_op_of(op), lhs: prev, rhs, dst: fresh, dst_ty: ty};
    StoreIndirect{address, value: combined};
}
返回 stored
```

**求值顺序对照**（契约文档明确要求）：

| 赋值类型 | 顺序 |
|----------|------|
| 普通 `x = expr` | ① 求 RHS `expr` → ② 定位 LHS `x` → ③ 写回 |
| 复合 `x += expr` | ① 定位 LHS → ② 读旧值 → ③ 求 RHS → ④ 结合 → ⑤ 写回 |

**为什么普通赋值先 RHS**：契约文档规定——如果 LHS 是 `a[i]` 且 `i` 会被 RHS 修改，先求 RHS 保证用修改前的 `i` 定位。源码注释："普通赋值 value→index（RHS 先），复合赋值 index→value（定位+RHS前读旧值后求RHS）。"

### 11.5 左值构造（lvalue.rs）

`build_address`（44-130）把表达式转成"地址操作数"，是赋值的 LHS、`&expr`、自增自减的基础：

| AST 形态 | 处理 |
|----------|------|
| `Paren` | 递归 |
| `Identifier` | 查局部（self Pointer 直接返回地址）；或 with_receiver / self 隐式字段的 `field_address`；否则 Unimplemented |
| `This` | 查 self 局部 |
| `MemberAccess` | target 是 Struct → `aggregate_address`（递归）取字段地址；否则先求值得到指针值再 `field_address` |
| `IndexAccess` | 求元素类型 + 求值 target + 索引定型 Usize → `IndexAddress` |
| `DerefPointer` | `build_expression(operand)`（`*p` 的地址就是 p 的值） |
| 其他（右值） | `aggregate_address` 里求值 + 物化临时槽 `Alloc` + `local_address`——"右值不误作地址" |

`local_address`（7-12）→ `LocalAddress{local, ty, dst: fresh Pointer(ty)}`。
`field_address`（132-149）→ `FieldAddress{target, target_ty, index, ty, dst}`。
`read_address`（151-165）→ `fresh_local(ty)` + `Unary{Deref, operand: address, dst, dst_ty: ty}`——把地址转成值。

**`aggregate_address` 的"右值物化"**：当 LHS 是复杂表达式（如函数调用返回的 struct）时，先求值到临时槽，再取这个临时槽的地址。这保证写操作有合法目标，且副作用只发生一次。


---

## 第 12 章 调用与默认实参（call.rs）

### 12.1 `build_call`（call.rs:6-100）——五种调用形态

HIR 的调用降级要同时处理：普通函数调用、静态方法调用、struct 实例方法调用、函数值调用、带 receiver 的方法调用。核心流程：

```rust
// 1. 取 callee 类型，必须是 Function
let HirType::Function(sig) = builder.expr_type(callee.span())? else {
    return Err(MissingType { what: "调用签名" });
};
if sig.params.len() < args.len() {
    return Err(Unimplemented { node: "omitted default arguments" });
}

// 2. receiver 处理
let mut operands = Vec::new();
let callee_operand = if sig.receiver {
    // 确定接收者表达式
    let target = match callee {
        Expression::MemberAccess { target, .. } => target,      // obj.method()
        Expression::Identifier(..) => Expression::This(...),    // 方法内省略 self 调用
        _ => return Err(Unimplemented { node: "indirect bound method call" }),
    };
    let target_ty = builder.expr_type(target.span())?;
    if matches!(target_ty, HirType::Class(..)) {
        return Err(Unimplemented { node: "class method dispatch" });  // 动态派发不支持
    }
    let receiver = match &target_ty {
        // self 局部
        _ if is_self => HirOperand::Local(self_id),
        HirType::Struct(..) => build_address(...)                // struct 取地址
            （Pointer(Struct) 槽位），
        HirType::Pointer(..) => build_expression(target),        // 已经是指针
        _ => return Err(Unimplemented { ... }),
    };
    operands.push(receiver);
    HirOperand::FunctionPtr(sig.id.clone())                     // 静态绑定：直接按键调用
} else {
    build_expression(callee, block)?                             // 函数值调用：求值得到 FunctionPtr
};

// 3. 实参：逐项按签名参数类型定型
for (arg, param_ty) in args.iter().zip(&sig.params) {
    operands.push(build_typed_expression(arg, param_ty, block)?);
}

// 4. 补齐默认实参
if args.len() < sig.params.len() {
    append_default_arguments(...)?;
}

// 5. 结果槽
let dst = if sig.ret == HirType::Void { None } else { Some(builder.fresh_local(sig.ret.clone())) };
block.push(HirStmt::Instr(HirInstr::Call {
    callee: callee_operand,
    args: operands,
    dst,
    dst_ty: (sig.ret != Void).then_some(sig.ret.clone()),
}));
if dst.is_some() { Local(dst) } else { Unit }
```

**关键设计决策**：

- **方法调用是静态绑定的**：`sig.receiver == true` 时，callee 直接是 `FunctionPtr(signature.id)`，接收者作为**第一个实参**插入。这意味着 `point.norm2()` 被降级成 `m0::Point::norm2(&point)`——没有运行时方法查找，符号键在降级期就确定了。这与 class 的动态派发形成对比：后者明确 `Unimplemented{"class method dispatch"}`。
- **struct receiver 传地址**：`HirType::Struct` 的接收者走 `build_address`（得到 `Pointer(Struct)`），与 8.3 节"实例方法 self 槽位是 `Pointer(Struct)`"一致。
- **class 方法拒绝**：class 是引用语义，理论上 receiver 直接是 `Class` 值，但当前 codegen 不支持 Class 类型，所以 HIR 层就拒绝动态派发。
- **`params.len() < args.len()`**：实参比形参多（且没有可变参数）是错误。

**"省略 self 调用"**：方法体内写 `norm2()`（不带 `obj.`）时，AST 里 callee 是 `Identifier`，HIR 把它解释为 `Expression::This`，从 self 局部取接收者。

### 12.2 `append_default_arguments`（call.rs:102-166）——定义环境按序展开

这是 HIR 里"定义环境"机制最复杂的应用。问题背景：调用 `add(2)` 时，`add(a: i32, b: i32 = a)` 的 `b` 缺省值 `a` 必须在 `add` 的定义环境里求值（引用 `add` 的参数 a = 2），而不是调用者环境。

```rust
// 1. 从 callee 表达式类型里取函数名义键
let view = sema.expression_types.get(module, callee_span)?;
let FunctionView { module: def_module, name: Some(name), .. } = view else {
    return Err(Unimplemented { node: "defaults of indirect function" });
};
// 2. 用名义键定位原始声明的参数表
let id = sema.declarations.named(def_module, name)?;       // DeclarationId
let mut params = sema.declarations.parameters(&sema.modules, id)?;  // &[Param]（含 default AST）
if receiver && params.first().map(|p| p.name.as_str()) == Some("self") {
    params = &params[1..];                                  // 去掉 self
}

// 3. 建立"定义环境"绑定：把已有实参（含 receiver）按参数名绑到 Local
let mut bindings: Vec<(String, LocalId)> = ...;
if receiver {
    bindings.push(("self".into(), operands[0] 的 Local));
}
for (i, param) in params.iter().enumerate() {
    if i < provided {
        // 已提供：复用调用方已求值的操作数，绑到参数名
        bindings.push((param.name.clone(), 已有 Local));
    } else {
        // 缺省：在定义环境求值 param.default
        let value = builder.in_definition_scope(def_module, bindings.clone(), |b| {
            b.build_typed_expression(param.default.as_ref()?, &param_ty, block)
        })?;
        let slot = builder.fresh_local(param_ty);
        block.push(Alloc { id: slot, ty: param_ty, init: Some(value) });
        bindings.push((param.name.clone(), slot));
        operands.push(Local(slot));
    }
}
```

**为什么必须"按序"**：源码注释明确——默认实参在定义模块环境里按参数顺序展开，且**能引用此前参数的快照**。例如 `func f(a: i32, b: i32 = a, c: i32 = b + 1)`，`c` 的默认值引用 `b`，而 `b` 可能是默认值。按序展开保证 `b` 的槽位在求 `c` 之前已经建立并绑定。

**为什么间接函数（函数值）的默认实参不支持**：`FunctionView.name` 是 `None` 时无法通过 `declarations.named` 定位到原始声明，因此报 `Unimplemented{"defaults of indirect function"}`。这是"默认实参必须与具名实体声明关联"的约定（2026-11-15 契约修订明确）。

**`in_definition_scope` 的作用**：见 9.3 节。它临时切换 `BodyBuilder.module` / `locals` / `functions` / `captures` / `with_receiver` 到定义处环境，但保留调用方的 `next_local` 和已发射指令——所以默认实参里创建的临时槽位编号不会与调用方冲突，且指令物理上仍在当前调用点发射（顺序正确）。

---

## 第 13 章 聚合、match、lambda 与字面量（aggregate.rs / coalesce.rs）

### 13.1 `build_struct_init`（aggregate.rs:7-80）——显式字段 + 缺省字段

```rust
let ty = builder.expr_type(node.span())?;
let HirType::Struct(module, name) = ty else { Err(MissingType) };
let fields = instance_fields(sema, module, &name, node.span())?;  // 父→子布局
let mut slots: Vec<(usize, HirOperand)> = Vec::new();

// 1. 显式字段（源码顺序）
for prop in &node.properties {
    let value_expr = prop.value.as_ref().unwrap_or(&prop.name_expr);  // {x} 简写为 {x: x}
    let (index, field_ty) = field_slot(&fields, &prop.name, block)?;
    let value = build_typed_expression(value_expr, &field_ty, block)?;
    slots.push((index, value));
}

// 2. 缺省字段（声明顺序），在定义环境求值 initializer
let id = sema.declarations.named(module, &name)?;
let members = sema.declarations.members(&sema.modules, id)?;
for (index, field) in fields.iter().enumerate() {
    if slots 已含 index { continue; }
    let init = find field.initializer in members
        .ok_or(MissingType{what: "字段初值"})?;
    let value = builder.in_definition_scope(module, vec![], |b| {
        b.build_typed_expression(init, &field.ty, block)
    })?;
    slots.push((index, value));
}

// 3. 分配 struct 槽位并逐字段写
let id = builder.fresh_local(ty);
block.push(Alloc { id, ty: struct_ty.clone(), init: None });
for (index, value) in slots {
    block.push(FieldStore { target: Local(id), target_ty: struct_ty.clone(),
                            field: fields[index].name.clone(), index, value });
}
Ok(Local(id))
```

**两个顺序保证**：

- **显式字段按源码顺序求值**（副作用顺序与书写一致）。
- **缺省字段按声明顺序求值**（`fields` 的顺序即父→子的声明顺序）。

**字段简写**：`Point { x, y }` 里 `x` 没有显式值，用同名 `Identifier` 表达式求值——这条规则在这里实现（`prop.value.as_ref().unwrap_or(&prop.name_expr)`）。

**定义环境**：缺省字段初值用 `in_definition_scope(module, vec![], ...)` —— struct 没有 receiver 绑定，所以 bindings 为空，但仍切换 module 到定义模块。这处理跨模块 struct 的字段初值引用定义模块的全局函数/类型。

### 13.2 `build_match`（aggregate.rs:83-169）——分支 + 结果槽 + 穷尽性

```rust
let result_ty = builder.expr_type_pref(node.span, preferred)?;
let dst = if result_ty == Void { None } else { Some(builder.fresh_local(result_ty.clone())) };
if let Some(id) = dst { block.push(Alloc { id, ty: result_ty, init: None }); }

let exhaustive = sema.exhaustive_matches.contains(&(module, node.span));
if dst.is_some() && node.default.is_none() && !exhaustive {
    return Err(Unimplemented { node: "value match without default" });
}

// subject 只求值一次
let subject = build_expression(node.subject, block)?;

// 构造尾部：default 块 / 穷尽时最后一个 case
let mut tail = match (&node.default, exhaustive) {
    (Some(default), _) => match_body_block(default, dst, block)?,
    (None, true) => /* 最后一个 case 作为 tail */,
    (None, false) => HirBlock::new(),  // Void match 无 default 允许
};

// 逆序构造嵌套 If：每个 case 命中则进 then，否则进 tail
for case in node.cases.iter().rev() {
    let (test, prelude) = match &case.pattern {
        Pattern::Expr(expr) => {
            let pat = build_typed_expression(expr, &subject_ty, &mut prelude)?;
            Binary { op: Eq, lhs: subject, rhs: pat, dst: test_id, dst_ty: Boolean }
        }
        Pattern::Type(type_expr) => {
            let target = annotation_type(type_expr)?;
            TypeTest { operand: subject, operand_ty: subject_ty, target, dst: test_id }
        }
    };
    let then_block = match_body_block(&case.body, dst, ...)?;
    tail = HirBlock::new();
    tail.push(If { cond: HirCond { prelude, value: Local(test_id) },
                   then_block, else_block: tail });
}
block.push(HirStmt::Block(tail));
```

**关键语义保证**：

- **subject 只求值一次**：先求值到 `subject` 操作数，所有 case 的 `Eq`/`TypeTest` 都引用它，不会重复求值（这对有副作用的 subject 表达式至关重要）。
- **case 按序短路**：逆序构造嵌套 `If`——第一个 case 的条件在前，命中就进 then，未命中才进 else（下一个 case）。所以只有"前面都没命中"才会求值后面的 case 条件（含副作用）。冒烟示例 `matching()` 就验证了这一点：`case pattern(&counter)` 两次调用，`counter` 最终为 2（第一个 case 命中后第二个不求值）。
- **值 match 必须 default 或穷尽**：`dst.is_some() && no default && !exhaustive` → 报错。Void match（所有分支都是块，无结果值）允许无 default。
- **穷尽性来自 sema**：`sema.exhaustive_matches`（见 16 章）是语义分析证明的，HIR 直接消费，不重新判断。

`match_body_block`（171-198）：

- `PatternBody::Expr(expr)`：求值 + `Store{dst, value}`（有结果槽时）；无结果槽时只求值（表达式语句）。
- `PatternBody::Block{block}`：有结果槽 → `Unimplemented{"block branch in value match"}`（块分支不能产生值）；无结果槽 → `emit_block`。

### 13.3 `build_lambda`（aggregate.rs:205-265）——无捕获独立函数项

```rust
let ty = builder.expr_type_pref(node.span, None)?;
let HirType::Function(sig) = ty else { Err(...) };
let signature = FunctionSignature {
    id: HirFunctionId(format!("m{}::lambda#{}", module, node.span.start)),
    ..sig
};

// 捕获检查
let mut capture_names = builder.locals 的名字集合;
capture_names.extend(builder.capture_names.clone());  // 继承外层 lambda 的捕获集合

let mut inner = BodyBuilder::new(sema, module, builder.functions.clone());
inner.return_type = Some(signature.ret.clone());
inner.capture_names = capture_names;                  // 引用任何外层局部 → Unimplemented
// 参数（Single / List）
match &node.params { Single(p) => inner.declare_param(p), List(ps) => for p in ps { ... } }
// 函数体
match &node.body {
    Expr(e) => { let v = inner.build_typed_expression(e, &ret)?; block.push(Return(Some(v))); }
    Block(b) => inner.build_statement(b, block)?,
}
builder.lambdas.push(HirFunction {
    source_module: module,
    is_lambda: true,
    id: signature.id.clone(),
    signature,
    params: inner.parameter_slots(),
    body: block,
    visibility: HirVisibility::Private,
});
Ok(HirOperand::FunctionPtr(signature.id))
```

**关键点**：

- **lambda 键含源码位置**：`m{module}::lambda#{span.start}`。冒烟示例里两个"完全一样"的 lambda `(v: i32): i32 => v + 1` 分别得到 `m0::lambda#1904` 和 `m0::lambda#1965`——靠位置区分，不撞键。
- **无捕获**：`capture_names` 收集当前所有局部名，lambda 体内引用任何一个都报 `Unimplemented{"lambda capture"}`。这是明确的语义限制（闭包不支持）。
- **lambda 是独立函数项**：push 到 `builder.lambdas`，经 `pending` → `build_hir` 归位到 `source_module`（见 7.1 节）。
- **表达式体 lambda**：包成 `Return(Some(value))`，与块体 lambda 统一。

### 13.4 `build_literal`（aggregate.rs:268-314）——字面量定型与拒绝

```rust
match literal {
    Null => Ok(HirOperand::Null),
    Boolean(v) => Ok(BoolConst(*v)),
    String(s) => {
        // 拒绝 f-string / 字节串
        if s.prefix 含 'f' 或 'b' { return Err(Unimplemented { node: "格式化字符串"/"字节串" }); }
        // 用 core 的解码器处理普通串和 r/R 原始串
        Ok(StringConst(novella_core::string::decode_string_literal(s.text)?))
    }
    Number(text) => {
        let text = text.replace('_', "");
        if !is_radix && (text.contains('.') || text.contains('e')) {
            Ok(UntypedFloat { value: text.parse::<f64>()?, span })
        } else {
            Ok(UntypedInt { value: parse_int_literal(&text)?, span })
        }
    }
}
```

**`parse_int_literal`（helpers.rs:107-123）**：支持可选负号 + `0x`/`0o`/`0b` 前缀 + 十进制，解析为 `i128`。用 `i128` 是为了精确承载 `u64::MAX`（契约文档特别指出 JSON 里 18446744073709551615 精确无损）。

**未定型字面量的落地**：`build_literal` 只产出 `UntypedInt`/`UntypedFloat`；真正落地成 `IntConst`/`FloatConst` 是在 `coerce`（`stmt.rs:250-292`）里根据 `expr_type_pref` 的结果做的——**原地替换操作数，不发射任何指令**。

### 13.5 `build_coalesce`（coalesce.rs:6-54）——`??` 空合并

用户特别确认的语义（契约文档明确）：**`p ?? backup` 非空时返回指针 `p` 本身，空时才执行 `backup`，不隐式解引用**。

```rust
let ty = builder.expr_type_pref(span, preferred)?;
let left_view = sema.expression_types.get(module, left.span());

// 左侧是纯 null 字面量：直接求右侧（常量折叠）
if matches!(left_view, Some(TypeView::Null)) {
    build_expression(left, block)?;   // 仍求值以触发副作用/校验
    return build_typed_expression(right, &ty, block);
}

// 一般情况：用分支
let value = build_typed_expression(left, &ty, block)?;          // 左侧定型（Pointer/Class）
let test = Binary { op: Ne, lhs: value, rhs: Null, dst: test_id, dst_ty: Boolean };
let dst = builder.fresh_local(ty);
block.push(Alloc { id: dst, ty, init: None });

let mut present = HirBlock::new();
present.push(Store { id: dst, value });                          // 非空：存左侧（指针本身）
let mut absent = HirBlock::new();
let backup = build_typed_expression(right, &ty, &mut absent)?;
absent.push(Store { id: dst, value: backup });                   // 空：存 backup
block.push(If { cond: HirCond::new(HirBlock::new(), Local(test_id)), then: present, else: absent });
Ok(Local(dst))
```

**短路保证**：`backup` 在 `absent` 块里求值——只有 `test` 为假（左侧为 null）时才执行。左侧非 null 时 `present` 块只存左侧值，不碰 backup。结果槽里存的是**指针**，不解引用。冒烟示例 `shortcircuit()` 验证了：`present ?? fallback(...)` 不调用 fallback，`absent ?? fallback(...)` 才调用，最终 `counter==1`。


---

## 第 14 章 语句与控制流降级（stmt.rs / loops.rs）

### 14.1 `build_statement` / `emit_statement`（stmt.rs:9-127）

`build_statement` 只是包装，实体 `emit_statement` 穷尽 `Statement` 变体：

| Statement 变体 | HIR 降级 |
|----------------|----------|
| `Variable` | `emit_variable`（见 14.3） |
| `VariableList` | 逐个 `emit_variable` |
| `Empty` | `HirStmt::Nop` |
| `Return` | `build_typed_expression(value, &return_type)` → `HirInstr::Return(Some(v))`；无值 → `Return(None)` |
| `Break` / `Continue` | `loop_depth == 0` → `Unimplemented`；否则 `HirStmt::Break` / `Continue` |
| `Block` | `with_scope` 包 `emit_block` → `HirStmt::Block` |
| `If` | `emit_if`（14.4） |
| `While` | 条件求进 `HirCond.prelude`（每轮执行），`loop_depth±1`，`Loop{cond:Some(...), body, step:空}` |
| `For` | `emit_for` → `emit_c_for` / `emit_for_in` |
| `Unsafe` | `with_scope` + `EnterScope`/`LeaveScope` + `emit_block` → `HirStmt::Block` |
| `Expression` | `build_expression`（结果丢弃，副作用保留） |
| `Throw` / `Yield` / `Try` | `Unimplemented`（异常展开与挂起明确不支持） |
| `With` | `emit_with`（14.2） |

**match 穷尽**：新增 `Statement` 变体时这里编译失败，迫使显式决定——与表达式分派一致。

### 14.2 `emit_with`（stmt.rs:133-160）

```
receiver_ty = binding.annotation 或 expr_type(receiver.span)
receiver_value = build_typed_expression(receiver, &receiver_ty)
builder.with_receiver = Some((receiver_ty, receiver_value))
with_scope {
    bind_pattern(binding.pattern, Some(receiver_value))   // 绑定名 → 接收者
    emit_block(body)                                       // 块内可用隐式成员
}
builder.with_receiver = 还原为旧值
→ HirStmt::Block
```

**语法上下文在此彻底消除**：parser 阶段的 `with_stack` / `looks_like_with_binding()` 消歧（见 `docs/parser-deep-dive.md` §12.2）在这里已经完成使命——HIR 看到的 `as` 若被语法层判为绑定，就是 `Statement::With`；判为 cast 的则是 cast 表达式。`with_receiver` 只是 HIR 内部为"隐式成员访问"保留的语义上下文，与语法消歧状态无关。

**接收者只求值一次**：`receiver_value` 先算好，绑定和块都引用它，不会重复求值。

### 14.3 `emit_variable` 与 `bind_pattern`（stmt.rs:175-247）

```rust
// emit_variable：类型来自 declaration_types（不是 annotation_types）
let ty = sema.declaration_types.get(module, variable.span)? → HirType;
let init = variable.init.as_ref().map(|e| build_typed_expression(e, &ty, block)).transpose()?;
bind_pattern(&variable.pattern, init, block)?;
```

**先求 init 再 bind_pattern**：源码注释明确——这样遮蔽（shadowing）时初始化表达式引用的是**外层**同名绑定，而不是正在声明的新绑定。例如 `let x = x + 1;` 右边的 `x` 是外层 x。

**`bind_pattern`（202-247）**：

- `BindingPattern::Ident(name)` → `declare_var(name, ty)` + `HirInstr::Alloc{id, ty, init}`。`init` 是这个变量槽的初始值（可能 `None`）。
- `BindingPattern::Tuple(patterns)` → 类型必须是 `HirType::Tuple`，元素数匹配，逐元素 `TupleExtract{target: init, index, ty, dst}`，再递归 `bind_pattern`。**关键：`init`（元组值）只求值一次**，所有元素从同一个槽里提取。

### 14.4 `coerce`（stmt.rs:250-292）——未定型字面量落地与转换闸门

```rust
fn coerce(builder, operand, target_ty: &HirType, block) -> Result<HirOperand> {
    // 1. 未定型字面量原地落地（不发射指令）
    match operand {
        UntypedInt{value,..}   => match target_ty {
            Int(_)   => return IntConst(value),
            Float(_) => return FloatConst(value as f64),
            _ => {}
        },
        UntypedFloat{value,..} => match target_ty {
            Float(_) => return FloatConst(value),
            Int(_)   => return IntConst(value as i128),
            _ => {}
        },
        _ => {}
    }
    // 2. 类型相同：直接返回
    if same_value_type(&from, target_ty) { return operand; }
    // 3. 不可转换：ImplicitCast
    if !castable(&from, target_ty) { return Err(ImplicitCast{from, to}); }
    // 4. 发射 Cast 指令
    block.push(Cast{operand, from, to: target_ty, dst: fresh});
    Local(dst)
}
```

**"原地落地不发射指令"**是重点：`UntypedInt(42)` 遇到期望 `I32` 直接变成 `IntConst(42)`，不产生任何 HIR 指令。这是 sema 类型下推（hints）在 HIR 的兑现。

**`castable`（helpers.rs:31-43）** 允许的隐式转换：Int↔Int、Float↔Float、Int↔Float、Int↔Pointer、Boolean↔Int、Pointer↔Pointer、Class↔Class。其余必须同类型，否则 `ImplicitCast` 错误——HIR 不做静默强转。

### 14.5 `emit_if`（stmt.rs:297-336）——逆序构造嵌套

`if / elif / else` 被降级成嵌套 `If`。**从后往前构造**：

```
tail = 最后 else 的 block（或 Nop）
for branch in branches.rev():
    if 是 ElseIf:
        cond_prelude, cond_value = 求值该分支条件（前置指令摊到当前块）
        wrapper = HirBlock::new()
        wrapper.push(If { cond: HirCond{cond_prelude, cond_value},
                          then_block: branch.block, else_block: tail })
        tail = wrapper
结果：tail 是嵌套好的 If 链
摊平 tail.stmts 到当前块（条件前置指令留在当前块，保证求值顺序）
```

**为什么摊平 wrapper**：每个 elif 的条件求值会产生前置指令（如函数调用），这些指令必须留在当前块按序执行，而不是藏进内层块导致顺序错乱。摊平后条件前置在"进入这个 If 之前"执行。

### 14.6 `emit_c_for`（stmt.rs:351-400）

```rust
with_scope {
    emit init (Declaration → emit_variable, 或 Expression → build_expression)
    // 条件
    let cond = match condition {
        Some(e) => HirCond { prelude: 求值 e 的前置块, value: 条件值 },
        None    => HirCond { prelude: 空, value: BoolConst(true) },  // 无限循环
    };
    loop_depth += 1;
    let body = emit_block(body);
    loop_depth -= 1;
    let step = emit update 表达式到新块;
    Loop { cond: Some(cond), body, step }
}
```

**C-for 的 init / 条件 / update 都归到同一个 `Loop`**：init 在循环外执行一次（在 `with_scope` 块里），条件每轮求值，update 放 `Loop.step`（正常尾部和 `continue` 执行，`break` 跳过）。

### 14.7 `emit_for_in`（loops.rs:6-86）——数组遍历

```rust
let array_ty = expr_type(right.span);
let HirType::Array(elem_ty) = array_ty else { Unimplemented "for-in of non-array" };
let array = build_expression(right);                       // 数组只求一次
let index = Alloc(Usize, init: IntConst(0));               // 索引槽

// prelude：每轮重新读长度（长度可能变化）
prelude:
    len = ArrayLength{ target: array, dst }
    test = Binary{ op: Lt, lhs: index, rhs: len, dst, dst_ty: Boolean }

// body：按索引取元素并绑定
loop_body:
    element = IndexLoad{ target: array, element_ty, index, dst }
    bind_pattern(left, Some(element))                      // 支持 (a,b) 元组解构
    emit_statement(body)

// step：索引 +1
step:
    next = Binary{ op: Add, lhs: index, rhs: 1, dst }
    Store{ id: index, value: next }

Loop { cond: HirCond{prelude, value: Local(test)}, body: loop_body, step }
```

**关键保证**：

- **数组只求一次**，但**长度每轮重新读**（`ArrayLength` 在 prelude 里）——如果循环体修改了数组长度，能正确反映。
- **`continue` 统一跳 `step`**（索引递增），不会漏掉——这正是 `Loop.step` 语义的价值。
- **元组解构**：冒烟示例 `for ((a,b) in pairs)` 通过 `bind_pattern` 的 Tuple 分支实现。

---

## 第 15 章 codegen 如何消费 HIR

> 本章基于对 `crates/novella-codegen/src/` 的精读（lib.rs 188 行、body.rs 450 行、operators.rs 165 行）。codegen 只依赖 `novella-hir` + `inkwell 0.11.0`（`llvm23-1-prefer-dynamic`、`target-x86` features），**不依赖 ast/sema/parser**。顶层注释（lib.rs:1）："从已定型的 HIR 发射 Linux x86-64 目标文件；链接由调用方负责。"

### 15.1 入口 `emit_object`（lib.rs:85-187）

```rust
pub fn emit_object(program: &HirProgram) -> Result<Vec<u8>>
type Result<Value> = std::result::Result<Value, CodegenError>;
pub struct CodegenError(String);   // Display 前缀 "代码生成失败：{}"
```

流程七步：

1. `entry(program)` 判定入口函数。
2. 初始化 x86 target：`Target::initialize_x86`；`TargetTriple::create("x86_64-unknown-linux-gnu")`；`create_target_machine(&triple, "generic", "", OptimizationLevel::None, RelocMode::PIC, CodeModel::Default)`，失败报 "无法创建 x86-64 Linux 目标机器"。
3. `Context::create()` + `create_module("novella")` + `set_triple` / `set_data_layout`。
4. **第一遍**：登记所有函数的 LLVM 签名（建函数表）。
5. **第二遍**：发射函数体。
6. void 入口补 `main` wrapper。
7. `module.verify()` + `write_to_memory_buffer(FileType::Object)` → `Vec<u8>`。

**入口判定 `entry`（lib.rs:37-65）**：

```rust
let module = program.modules.first()
    .ok_or_else(|| CodegenError("没有入口模块".into()))?;
let name = format!("m{}::main", module.id);
module.items.iter().find_map(|item| match item {
    HirItem::Function(function)
        if function.id.0 == name
        && function.signature.params.is_empty()
        && matches!(function.signature.ret, HirType::Void | HirType::Int(IntTy::I32)) => Some(function),
    _ => None,
}).ok_or_else(|| CodegenError(format!("{}：入口模块必须定义无参 `main`，返回 int 或 void", module.path)))
```

只取**第一个模块**的 `m{id}::main`；参数必须为空；返回必须是 `Void` 或 `Int(I32)`。

### 15.2 两遍扫描与函数表

**第一遍（lib.rs:105-163）** 遍历所有模块的 `items`：

- `HirItem::Function(f)` → 纳入函数表。
- `HirItem::ExternFunction(f)` → `Err("{}：暂不支持外部函数 `{}`")`。
- `HirItem::Global(g)` → `Err("{}：暂不支持全局变量 `{}`")`。
- 限制检查：`if f.signature.receiver || f.signature.is_async || f.signature.type_param_count != 0` → `Err("暂不支持带接收者、异步或泛型函数")`。**注意：这意味着即使 HIR 允许实例方法（receiver=true，见 8.3 节），当前 codegen 也全部拒绝**——这是"HIR 支持 ≠ codegen 支持"的最明显例证。
- 函数类型：`params` 逐项 `value_type` → `BasicMetadataTypeEnum::from`；ret=Void → `context.void_type().fn_type(&params, false)`，否则 `value_type(ret).fn_type(&params, false)`（均非变参）。
- 链接名：入口且 ret≠Void → `("main", Linkage::External)`；否则 `(function.id.0.as_str(), Linkage::Internal)`。重复键 → `Err("重复的 HIR 函数符号")`。
- 插入：`functions.insert(f.id.clone(), (module.add_function(name, ty, Some(linkage)), &f.signature))`。

错误统一用 `contextualize` 包装成 `"{source.path}：{function.id}：{error.0}"`。

**第二遍（lib.rs:165-173）**：仅对 `HirItem::Function` 调 `body::emit(&context, &module, &functions, function)`。

**void 入口 wrapper（lib.rs:174-180）**：若入口返回 Void，新增 `main() -> i32`（External），函数体调用真实入口 `functions[&entry.id].0` 然后 `return 0`。

### 15.3 `HirType` → LLVM 类型（value_type，lib.rs:67-82）

```rust
fn value_type<'ctx>(context: &'ctx Context, ty: &HirType) -> Result<BasicTypeEnum<'ctx>> {
    match ty {
        HirType::Int(kind) => Ok(match kind {
            IntTy::U8 | IntTy::I8 => context.i8_type(),
            IntTy::U16 | IntTy::I16 => context.i16_type(),
            IntTy::U32 | IntTy::I32 => context.i32_type(),
            IntTy::U64 | IntTy::I64 | IntTy::Usize | IntTy::Isize => context.i64_type(),
        }.into()),
        HirType::Boolean => Ok(context.bool_type().into()),
        HirType::Pointer(inner) if matches!(**inner, HirType::Int(_) | HirType::Boolean) =>
            Ok(context.ptr_type(AddressSpace::default()).into()),
        _ => Err(CodegenError(format!("暂不支持值类型 {ty:?}"))),
    }
}
```

映射表：

| HIR 类型 | LLVM 类型 |
|----------|-----------|
| Int(U8/I8) | i8 |
| Int(U16/I16) | i16 |
| Int(U32/I32) | i32 |
| Int(U64/I64/Usize/Isize) | i64（固定 64 位） |
| Boolean | i1 |
| Pointer(Int(_) \| Boolean) | 默认地址空间 ptr |
| Void | 仅作函数返回特殊处理，不进 value_type |
| Float / String / Array / Tuple / Function / Struct / Class / Trait / 其它 Pointer | `Err("暂不支持值类型 {ty:?}")` |

**codegen 实际只支持整数、布尔、指向整数/布尔的指针**——这是理解 codegen 报错的关键：HIR 数据模型完整支持 struct/tuple/array/float，但 LLVM 后端目前只落地了一个标量子集。`HirModule.types`、`HirMethod`、`HirField` 完全不被消费。

### 15.4 函数体发射（body.rs）

`body::emit`（39-44）：

```rust
pub(crate) fn emit<'ctx>(context: &'ctx Context, module: &Module<'ctx>,
    functions: &Functions<'ctx, '_>, hir: &HirFunction) -> Result<()> {
    let function = functions[&hir.id].0;
    // append "entry" basic block
    let mut body = Body { ... };
    if function.count_params() as usize != hir.params.len() {
        return Err(CodegenError("HIR 形参与函数签名不一致".into()));
    }
    for param in &hir.params {
        let value = function.get_nth_param(index)...;
        body.save(param.id, &param.ty, value)?;   // 形参从 get_nth_param 拷入槽位
    }
    // ... 发射 body ...
    if body 贯穿 && ret != Void { Err("有返回值的函数存在未返回的路径") }
    else if ret == Void { build_return(None) }
}
```

`Body` 字段（27-37）：`context` / `module` / `builder` / `function` / `functions` / `ret` / `entry` / `slots: HashMap<LocalId, (PointerValue, HirType)>` / `loops: Vec<LoopTargets>`；`LoopTargets`（20-25）：`exit` / `step` / `has_break` / `has_continue`。

**槽位管理**：

- `allocate`（81-95）：在函数入口首指令前（无则末尾）`build_alloca(value_type(ty), "slot")`；重复定义报 "重复定义 HIR 槽位 {}"。注释："槽位只在入口分配；循环里的初始化仍留在原指令位置。"
- `save`（97-104）：先校验 `value.get_type() != value_type(ty)` → "HIR 槽位 {} 的结果类型不匹配"，再 `allocate` + `store`。
- `slot`（75-79）：未定义 → "HIR 槽位 {} 尚未定义"。
- 所有指令结果经 `save` 落槽——这与 HIR "所有表达式落到 Local" 的不变量一一对应。

**`HirOperand`（operand，106-141）**：

- `Local(id)` → 查槽位 + 类型校验（"HIR 槽位 {} 类型不匹配：{actual:?} / {ty:?}"）+ `build_load("load")`。
- `IntConst(value)` → `value_type(ty).into_int_type().const_int(*value as u64, false)`（仅 Int）。
- `BoolConst` → `bool_type().const_int(u64::from(*value), false)`（仅 Boolean）。
- `Null` → `pointer.const_null()`（仅 Pointer）。
- 其余（`FloatConst` / `StringConst` / `Untyped*` / `Unit` / `Function` / `WithReceiver`）→ `Err("暂不支持该 HIR 操作数或类型不匹配：{operand:?} / {ty:?}")`。`Function`/`FunctionPtr` 仅在 `Call` 的 callee 特判处理。

**`HirInstr`（instruction，143-290，返回 bool=是否贯穿）**：

| 指令 | 发射方式 |
|------|----------|
| `Alloc{id,ty,init}` | 先求 init 操作数，再 allocate，再 store |
| `Load{dst,src}` | 取 src 槽位类型 → operand(Local(src)) → save(dst) |
| `Store{id,value}` | slot(id) 取指针/类型 → operand(value,ty) → store |
| `LocalAddress{local,ty,dst}` | 校验槽位类型（"局部变量取址类型不匹配"）→ save(dst, Pointer(ty), pointer) |
| `StoreIndirect{address,value,ty}` | address 以 Pointer(ty) 求 operand，value 以 ty 求 operand，store |
| `Unary{Deref,...}` | operand(operand, &Pointer(dst_ty)) → build_load("deref") → save |
| 其它 `Unary` | `self.unary(...)` → save |
| `Binary` | `self.binary(op, (lhs,lhs_ty), (rhs,rhs_ty))` → save(dst,dst_ty) |
| `Cast` | `self.cast(...)` → save(dst,to) |
| `Call` | 见下 |
| `Return(v)` | operand(v,self.ret)；校验 `value.is_none() != (*ret == Void)` → "返回值与函数签名不一致"；build_return；返回 false |
| `FieldAddress`/`FieldLoad`/`FieldStore`/`Tuple`/`TupleExtract`/`Array`/`ArrayLength`/`IndexAddress`/`IndexLoad`/`TypeTest` | `Err("暂不支持 HIR 指令 {instruction:?}")` |

**`Call`（227-259）** 逐字核心：

```rust
let (HirOperand::Function(id) | HirOperand::FunctionPtr(id)) = callee else {
    return Err(CodegenError("暂不支持间接调用".into()));
};
let (function, signature) = functions.get(id)
    .ok_or_else(|| CodegenError(format!("未定义 HIR 函数 `{id}`")))?;
if args.len() != signature.params.len() {
    return Err(CodegenError(format!("`{id}` 实参个数与签名不一致")));
}
// 实参用 signature.params 逐项 operand；build_call
// dst Some 时用 dst_ty.as_ref() == Some(&signature.ret) 过滤，否则 "`{id}` 调用的返回类型与 HIR 不一致"
```

**运算符（operators.rs）**：

- `unary`（12-29）：Identity 原样、Neg→`build_int_neg("neg")`、Not|BitNot→`build_not("not")`；其余 `Err("暂不支持一元运算 {op:?} / {ty:?}")`。AddressOf 不支持（由 `LocalAddress` 承担）。
- `cast`（31-49）：from==to 原样；仅 Int→Int 用 `build_int_cast_sign_flag(value, ty, kind.is_signed(), "cast")`（符号性取**源类型**）；其余 `Err("暂不支持转换 {from:?} → {to:?}")`。
- `binary`（51-145）：要求 `lhs_ty == rhs_ty`；指针仅 Eq/Ne → `build_int_compare("ptr.cmp")`；否则必须 IntValue，signed 由 lhs_ty 决定。
  - Add/Sub/Mul 按位宽回绕。
  - Div/Rem 用 trap_if 检 rhs==0、signed 下 lhs==minimum && rhs==-1，再选有/无符号 div/rem。
  - Shl/Shr 检 rhs >= bit_width 后 trap，右移按 signed 选。
  - BitAnd|LogAnd→and、BitOr|LogOr→or、BitXor→xor。
  - 比较按 (op, signed) 选 EQ/NE/SLT/ULT/SLE/ULE/SGT/UGT/SGE/UGE。
  - `PtrDiff` → unsupported。
- `trap_if`（147-164）：`Intrinsic::find("llvm.trap")`，否则 `Err("LLVM 未提供 llvm.trap")`；生成 `arithmetic.trap`（call trap + unreachable）/ `arithmetic.ok` 块。

**`HirStmt`（block，292-325）**：`Instr`→instruction；`Block`→递归 block；`If`→branch；`Loop`→loop_block；`Break`/`Continue`→`self.loops.last_mut().ok_or_else(|| CodegenError("循环外出现跳转".into()))?`，Break 置 has_break 跳 exit，Continue 置 has_continue 跳 step，`build_unconditional_branch`，返回 false；`EnterScope`/`LeaveScope`/`Nop`→true（不发射）。任一 stmt 不贯穿则整块 false。

**`HirCond`（branch，327-373）**：先发射 `cond.prelude`（不贯穿则整个 if 不贯穿），value 以 Boolean 求 operand；常量条件折叠（非零→then，零→else/贯穿）；否则建 "then"/"else"/"merge"，`build_conditional_branch`，贯穿分支跳 merge，双不贯穿对 merge `build_unreachable`，返回 `then_falls || else_falls`。

**`loop_block`（375-449）**：建 "loop.head"/"loop.body"/"loop.step"/"loop.exit"，push `LoopTargets{exit, step:step_label, ...}`，先 branch head。
- 有 cond 时**每轮**发射 `cond.prelude`（393-397）；常量 0→exit(false,true)、常量非 0→body(true,false)、动态→条件分支(true,true)；无 cond→body(true,false)。
- body 贯穿则 branch step_label。
- `let step_reachable = body_falls || targets.has_continue; targets.step = head;`（433-434）——**这句是把 step 执行后接回 head 的关键**；可达则发射 step 并 branch head，否则 unreachable。
- pop 后 exit 可达性 = `head_can_exit || targets.has_break`，否则 unreachable。

**语义闭环**：`continue` 跳原 `step_label`→执行 step→回 head（所以 step 一定执行）；`break`/`return` 直接跳 exit，跳过 step。这精确兑现了 HIR 的 `Loop.step` 契约（第 4.3 节）。

### 15.5 槽位与符号键的消费

- `HirFunctionId(pub String)`（ty.rs:148）：函数表键 `HashMap<HirFunctionId, (FunctionValue, &FunctionSignature)>`（lib.rs:105、156-162；body.rs:17-18）；用于入口判定、链接名、`Call` 查表、错误信息拼接。重复键 `Err("重复的 HIR 函数符号")`，缺失 `Err("未定义 HIR 函数 `{id}`")`。
- `LocalId(pub u32)`（body.rs:23-28）：同一函数体内唯一，不跨函数。`LocalSlot.name` 不被 codegen 读取。
- `HirFunction.source_module` / `is_lambda` / `visibility` 不被 codegen 读取——这些字段服务于 HIR 内部归位和调试，不是后端契约。

### 15.6 HIR→LLVM 边界约定（fail-fast 全景）

codegen 层所有失败都是 `CodegenError`，**无静默兜底**：

- 入口："没有入口模块" / "…必须定义无参 `main`，返回 int 或 void" / void 包装。
- 目标："无法创建 x86-64 Linux 目标机器"。
- 项："暂不支持外部函数" / "暂不支持全局变量" / "重复的 HIR 函数符号"。
- 签名："暂不支持带接收者、异步或泛型函数" / "HIR 形参与函数签名不一致" / "有返回值的函数存在未返回的路径" / "返回值与函数签名不一致"。
- 类型/槽位："暂不支持值类型" / 槽位未定义/重复/类型不符 / "局部变量取址类型不匹配" / "暂不支持该 HIR 操作数或类型不匹配"。
- 控制流："循环外出现跳转"。
- 指令："暂不支持间接调用" / "未定义 HIR 函数" / 实参数与返回类型不符 / 整组 Field*/Tuple*/Array*/Index*/TypeTest 不支持 / unary/cast/binary 不支持组合 / "LLVM 未提供 llvm.trap"。
- `module.verify()` / `write_to_memory_buffer` 失败转 CodegenError。

**边界要点总结**：只消费定型 HIR；仅 Linux x86-64 + PIC + `OptimizationLevel::None` 无优化；指针仅指向整数/bool，无结构/数组/字符串/浮点布局；栈槽统一在入口分配，循环初始化留原位；条件 prelude 每轮发射；除零/有符号最小÷-1/移位越界用 `llvm.trap`。


---

## 第 16 章 输入契约：sema 向 HIR 提供什么

> 本章基于对 `novella-sema` 公开 API 的精读。HIR 只依赖 sema 的**只读投影**，完全不接触内部 `Ty` / 符号表 / 作用域树。这是理解 HIR "为什么不重新解析类型"的关键。

### 16.1 公开面：只读视图的边界

`novella-sema/src/lib.rs` 的 doc（L16-28）明确公开面只有三族：

- 入口 `analyze` / `analyze_with` / `analyze_with_target`，加载器 `FileLoader` / `FsLoader` / `OverlayLoader`；
- 结果 `SemaResult`，由 `DiagnosticReport` / `BindingTable` / `TypeHierarchy` / `ModuleOutput` 四个只读视图组成；
- 值类型 `Definition` / `DefinitionKind` / `TypeSummary` / `TypeSummaryKind` / `ModuleDiagnostics` / `AnalyzedModule`。

L24-26 逐字："**词法 / 语法只在 `analyze` 内执行一次**：`SemaResult.modules` 给出每个模块的完整产物（源码、token 流、AST；诊断统一在 DiagnosticReport），下游（CLI / codegen）直接消费，不得自行再跑 lexer / parser。" L28："其余（符号表、作用域树、类型注册表、检查函数）一律 `pub(crate)`。"

L34：`pub use declarations::{DeclarationId, DeclarationIndex};` —— 这是 HIR 取默认实参/字段初值时用的声明索引。

### 16.2 `SemaResult`（api.rs:241-263）——HIR 的直接输入

```rust
pub struct SemaResult {
    pub declarations: crate::DeclarationIndex,
    /// 已证明穷尽的 match；下游可据此降级无 default 的值分支。
    pub exhaustive_matches: HashSet<(ModuleId, Span)>,
    /// 诊断（按模块归组 + 全局）
    pub diagnostics: DiagnosticReport,
    /// 引用 → 定义绑定表
    pub bindings: BindingTable,
    /// 具名类型层级
    pub types: TypeHierarchy,
    /// 模块产物视图（源码 / token 流 / AST）；全部诊断见 diagnostics。
    pub modules: ModuleOutput,
    /// 表达式级语义类型（键为 `(模块, span)`）
    pub expression_types: ExpressionTypes,
    /// 类型注解的解析结果（含别名展开与限定名；键为注解 span）。
    pub annotation_types: ExpressionTypes,
    /// 声明的最终类型（函数签名 / 变量绑定；键为声明 span）。
    pub declaration_types: ExpressionTypes,
}
```

doc（L243-244）："各字段都是**只读视图**：语义阶段内部状态（作用域树、符号表、类型注册表、加载器）不外泄，下游只能查询模块产物、诊断、绑定与类型层级。"

HIR 实际消费的字段（按重要性）：

| SemaResult 字段 | HIR 用途 |
|-----------------|----------|
| `diagnostics` | 入口闸门 `has_errors()` |
| `modules` | 遍历模块、取 `id/path/program/source`，检测 `syntax_failed/load_failed` |
| `expression_types` | 每个表达式的定型结果（dst_ty、局部类型、callee 签名） |
| `annotation_types` | 类型注解的解析结果（cast/is/字段类型等） |
| `declaration_types` | 函数签名、变量绑定类型 |
| `types.summaries` | 类型字段/方法/继承链 |
| `declarations` | 默认实参、字段初值的声明定位 |
| `exhaustive_matches` | match 穷尽性证明 |
| `bindings` | HIR **不直接消费**（保留供 LSP 等） |

### 16.3 `DiagnosticReport` 与 `has_errors` 闸门

```rust
pub struct DiagnosticReport {
    pub modules: Vec<ModuleDiagnostics>,  // 已加载模块的诊断，入口为第一个
    pub global: Vec<Diagnostic>,          // 全局诊断（入口读取失败等）
}
impl DiagnosticReport {
    pub fn has_errors(&self) -> bool {
        self.global.iter()
            .chain(self.modules.iter().flat_map(|m| m.diagnostics.iter()))
            .any(|d| d.severity == Severity::Error)
    }
}
```

`ModuleDiagnostics`（api.rs:15-47）把词法/语法/语义诊断存**一份**：

```rust
pub struct ModuleDiagnostics {
    pub path: PathBuf,
    pub diagnostics: Vec<Diagnostic>,       // 全部诊断，按产生顺序
    pub syntax_diagnostic_count: usize,     // 前缀中词法/语法诊断条数
}
```

- `syntax_diagnostics()` = `&diagnostics[..syntax_diagnostic_count]`
- `semantic_diagnostics()` = `&diagnostics[syntax_diagnostic_count..]`
- `ordered()` = 按 `location.span.start` 排序

**HIR 用 `has_errors()` 做总闸门**（build/module.rs:5-12）：任一 error 级诊断存在就返回 `InvalidInput`，不降级任何坏模块。这是"诊断只有一份"约定的下游兑现——HIR 不收集第二套错误。

### 16.4 `TypeView`（api.rs:153-191）——sema 类型的只读投影

```rust
pub enum TypeView {
    Applied(Box<TypeView>, Vec<TypeView>),   // 具名泛型实例：基类型 + 实参
    Future(Box<TypeView>),
    Object(BTreeMap<String, TypeView>),
    Void, Boolean, String,
    Int(&'static str),                        // 已定型整数：`int` 归一化为 `i32`
    Float(&'static str),                      // 已定型浮点：`float` 归一化为 `f32`
    IntLiteral, FloatLiteral,                 // 未定型字面量（HIR 用期望类型落地）
    Null,
    Pointer(Box<TypeView>), Array(Box<TypeView>), Tuple(Vec<TypeView>),
    Union(Vec<TypeView>),
    Function(Box<FunctionView>),
    Struct(ModuleId, String), Class(ModuleId, String), Trait(ModuleId, String),
    TypeParam(String),
    Unknown,                                  // 语言允许的无注解状态
    Error,                                    // 来源处已报错（与 Unknown 分离）
}
```

doc（L155-157）："内部 `Ty` 走 `Rc` / 内部定义类型，不能直接导出；本枚举只带名义键（`Struct(模块, 名)` 等）与结构化子类型，足以供 codegen 做布局 / 调用约定推导。`Unknown` 保留未定型状态，已诊断错误独立投影为 `TypeView::Error`。"

**`TypeView` 与 `HirType` 的对照**（这是第 3 章 `from_view_with` 的输入）：

| TypeView | HirType 转换 |
|----------|--------------|
| `Void` / `Boolean` / `String` | 同名直接转 |
| `Int(name)` / `Float(name)` | `IntTy::from_name` / `FloatTy::from_name` |
| `Pointer` / `Array` / `Tuple` | 递归（Array/Tuple 用 preferred 提示） |
| `Function(Box<FunctionView>)` | `FunctionSignature::from_view_with` |
| `Struct/Class/Trait(ModuleId, String)` | 同名名义键 |
| `IntLiteral` / `FloatLiteral` | 仅带 preferred 时落地为 Int/Float，否则 `UntypedLiteral` |
| `Null` | 仅 preferred 是 Pointer/Class 时 Ok，否则 `UntypedLiteral` |
| `Applied` / `Future` / `Object` | `Err(Unimplemented)` —— 后端不支持 |
| `Union` | `Err(UnionUnsupported)` |
| `TypeParam` | `Err(GenericUnsupported)` |
| `Unknown` | `Err(OpaqueType)` |
| `Error` | `Err(InvalidInput)` |

### 16.5 `FunctionView`（api.rs:193-224）——函数签名的投影

```rust
pub struct FunctionView {
    pub module: ModuleId,
    /// 函数名：顶层函数使用原名，方法使用 `类型::方法`；lambda / 占位签名为 `None`
    pub name: Option<String>,
    pub params: Vec<TypeView>,
    pub ret: Box<TypeView>,
    pub type_param_count: usize,
    pub is_async: bool,
    pub is_extern: bool,
    /// 是否带隐式接收者（实例方法）
    pub receiver: bool,
}
```

**`name` 的三种取值**（HIR 合成 `HirFunctionId` 的依据）：

- 顶层函数 → 原名（`add`），HIR 合成 `m{module}::add`；
- 方法 → `类型::方法`（`Point::norm2`），HIR 合成 `m{module}::Point::norm2`；
- lambda / 占位 → `None`，HIR 用源码位置合成 `m{module}::lambda#{span.start}`（lambda）或 `m{module}::anon#{params.len()}`（占位，**不标识唯一实体**）。

**`receiver` 的意义**（L218-223 逐字）："`self` 不占 `params` 位置：语义检查对 `self` 不解析类型，若把它当作普通形参投影，`params` 里就会出现 `TypeView::Unknown`，下游只能整体拒绝。因此投影时略去 `self` 占位形参，改由本标记表达；接收者类型即方法所属的具名类型（`TypeSummary` 中的定义）。"

这直接解释了 HIR 里两个看似矛盾的设计：

1. `FunctionSignature.receiver = true` 时 `params` 不含 self —— 因为 sema 已经剥离；
2. `method_functions`（types.rs）却要为 self 单独 `declare_param("self", receiver_ty)` —— 因为**函数体的局部槽位**需要 self，但**签名**不含它。codegen 直接拒绝 receiver=true 的函数（第 15.2 节），所以 self 槽位目前只在 HIR 层完整。

### 16.6 四张类型表的键与登记出口

`ExpressionTypes`（api.rs:226-239）：

```rust
pub struct ExpressionTypes { pub types: HashMap<(ModuleId, Span), TypeView> }
impl ExpressionTypes {
    pub fn get(&self, module: ModuleId, span: Span) -> Option<&TypeView> {
        self.types.get(&(module, span))
    }
}
```

doc（L228-229）："键为 `(模块 id, 表达式 span)`，值为该表达式的语义类型。只覆盖**进入类型检查的表达式**（函数体 / 方法体 / 字段初始化 / 顶层语句）；语法失败的模块为空。"

四张表对照：

| 表 | 键 | 值 | 登记出口 |
|----|----|----|----------|
| `expression_types` | (模块, **表达式** span) | 表达式最终类型 | `check_expr_recorded`（dispatch.rs:14-25）+ `check_expr_with_hint`（hints.rs:21-39） |
| `annotation_types` | (模块, **注解 TypeExpr** span) | 别名展开+限定名解析结果 | `resolve_type`（resolve/types.rs:11-24） |
| `declaration_types` | (模块, **声明** span) | 函数签名 / 变量绑定类型 | `build_function_sig`（registration.rs:443-451） |
| `exhaustive_matches` | (模块, **match 节点** span) | 已证明穷尽 | `typecheck/matching.rs:98-108` |

**三张类型表由内部 `Ty` 统一投影**（analysis.rs:193-213）：

```rust
expression_types: ExpressionTypes { types: sema.expression_types.into_iter()
    .map(|(key, ty)| (key, ty.view())).collect() },
annotation_types: ExpressionTypes { types: sema.annotation_types.into_iter()
    .map(|(key, ty)| (key, ty.view())).collect() },
declaration_types: ExpressionTypes { types: sema.declaration_types.into_iter()
    .map(|(key, ty)| (key, ty.view())).collect() },
```

**`expression_types` 的唯一登记出口**（typecheck/expr/dispatch.rs:4-25）：

```rust
pub(crate) fn check_expr(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> Ty {
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        check_expr_recorded(sema, ctx, expr)
    })
}
pub(super) fn check_expr_recorded(sema: &mut Sema, ctx: &mut BodyCtx, expr: &Expression) -> Ty {
    let ty = check_expr_inner(sema, ctx, expr);
    if matches!(expr, Expression::Call { .. } | Expression::New { .. } | Expression::Await { .. }) {
        ctx.record_possible_exception();
    }
    sema.expression_types.insert((ctx.module, expr.span()), ty.clone());  // 唯一登记点
    ty
}
```

doc（L4-7）："登记点必须放在**唯一的函数出口**（而不是各分支里），否则新增分支时极易漏记；`check_expr_with_hint` 等其它入口最终也会回到这里。" 第二出口 `check_expr_with_hint`（hints.rs）用于类型下推（空数组 / null→注解 / `&&||` / `??` / cast / 三元），它走 `record_expression_type` 单独登记（L21-39）。

**HIR 的意义**：`ExprType::expr_type(span)`（context.rs:107-117）就是这个表的查询封装——查询失败 `MissingExpressionType`，绝不猜类型。这保证 HIR 的每个 `dst_ty` 都有 sema 背书。

### 16.7 `DeclarationIndex` 与 `DeclarationId`——定义环境定位

```rust
// declarations.rs:10-15
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeclarationId { pub module: ModuleId, pub span: Span }

// declarations.rs:17-27
#[derive(Debug, Default)]
pub struct DeclarationIndex {
    named: HashMap<(ModuleId, String), DeclarationId>,
    locations: HashMap<DeclarationId, Location>,  // Location { item, member }
}
```

方法：

- `named(module, name) -> Option<DeclarationId>`（L116-118）：按"模块+名字"定位声明；
- `parameters(modules, id) -> Option<&[Param]>`（L119-135）：由 locations 找 item，取 `Item::Function` 的 `type.params` 或 `ClassMember::Method` 的 `params`；
- `members(modules, id) -> Option<&[ClassMember]>`（L136-147）：取 type item 的 members。

**HIR 的两个用法**：

1. `append_default_arguments`（call.rs:111-121）：`view.name` → `declarations.named(def_module, name)` → `declarations.parameters(...)` 拿到含 `default` 的原始 `Param` 列表；
2. `build_struct_init`（aggregate.rs:35-45）：`declarations.named(module, name)` → `declarations.members(...)` 拿字段初值。

这是"默认实参/字段初值在定义环境求值"的物理基础：没有 `DeclarationIndex`，HIR 就只能看到类型投影，拿不到 AST 里的 `default` 表达式。

### 16.8 `TypeHierarchy` 与 `TypeSummary`——继承布局

```rust
pub struct TypeHierarchy { pub summaries: Vec<TypeSummary> }
pub struct TypeSummary {
    pub module: ModuleId, pub name: String, pub kind: TypeSummaryKind,
    pub span: Span,
    pub parent: Option<(ModuleId, String)>,    // 仅 class 可能有
    pub fields: Vec<FieldSummary>,             // 声明顺序
    pub methods: Vec<MethodSummary>,           // 按名字排序
}
pub struct FieldSummary {
    pub name: String, pub ty: TypeView,
    pub has_default: bool, pub is_weak: bool, pub is_static: bool,
}
pub struct MethodSummary {
    pub name: String,
    pub signature: FunctionView,   // this 不占参数位；name 恒 Some(方法名)
    pub is_static: bool,
}
pub enum TypeSummaryKind { Struct, Class, Trait, Alias }
```

**HIR 用法**：

- `finish_types`（types.rs）用 `type_summary` 按 module+name 找 `TypeSummary`，生成 `HirTypeDef`；
- `instance_fields`（layout.rs）沿 `parent` 链向上收集，再 `rev()` 得到父→子字段顺序；
- `hir_methods`（types.rs:194-214）把 `MethodSummary.signature`（FunctionView）经 `FunctionSignature::from_view` 转成 HIR 签名；
- `method_signature`（types.rs:146-170）同理。

doc 明确（types.rs:176-191）："摘要是 sema 从类型注册表投影出来的，直接以 `TypeView` 给出每个字段的类型 —— HIR 不需要（也无法）自己重新解析一遍类型表达式。"

### 16.9 self 定型的完整链路

实例方法的 self 从 AST 到 HIR 经历三步，是跨层协作的典范：

**第一步：sema 定型**（check/type_defs/registration.rs:305-322）——`build_method_sig` 形参按 enumerate 生成：

```rust
// 无注解且是首个 self 且非 static → 属主类型
None if index == 0 && p.name == "self" && !method.modifiers.is_static => owner.clone()
```

校验（L328-354）：静态或非首位报 "self 只能是实例方法的第一个形参"；显式标注但 `!ty_eq` 报 "self 的类型标注必须是方法所属类型"。

名义键（L381-403）：`name = Some(format!("{name}::{}", method.name))`，`is_method = !method.modifiers.is_static`。

**第二步：投影剥离**（ty/mod.rs:145-164）——`FunctionSig::view()`：

```rust
for (index, param) in self.params.iter().enumerate() {
    if self.is_method && param.name == "self" && index == 0 { continue; }  // 跳过 self
    params.push(param.ty.view());
}
FunctionView { ..., receiver: self.is_method }   // receiver 标记
```

**第三步：HIR 重建槽位**（types.rs:method_functions）——实例方法 `signature.receiver == true`，`signature.params` 不含 self，但函数体需要 self 局部：

```rust
if signature.receiver {
    let receiver_ty = /* Struct → Pointer(Struct), Class → Class */;
    inner.declare_param("self", receiver_ty);
}
// 遍历 AST params 时跳过无注解 self 并重新对齐下标
```

**结果**：`m0::Point::norm2` 的 HIR `params = [LocalSlot{id:0, name:"self", ty:Pointer(Struct(0,"Point"))}]`，但 `signature.params = []`、`signature.receiver = true`。这与冒烟示例 JSON 完全一致。

### 16.10 `analyze` 系列入口与管线

```rust
pub fn analyze(entry: &Path) -> SemaResult { analyze_with(&mut FsLoader, entry) }              // L17-19
pub fn analyze_with(loader: &mut dyn FileLoader, entry: &Path) -> SemaResult {                 // L22-24
    analyze_with_target(loader, entry, AnalysisTarget::default())
}
pub fn analyze_with_target(loader: &mut dyn FileLoader, entry: &Path, target: AnalysisTarget)   // L52-65
    -> SemaResult { analyze_with_options(loader, entry, AnalysisOptions { target, ..Default::default() }) }
pub fn analyze_with_options(loader: &mut dyn FileLoader, entry: &Path, options: AnalysisOptions) // L68-72
    -> SemaResult { /* 实际管线 */ }
```

管线（`analysis.rs` L73-142）：

- **Phase 0** `module::load_modules(...)` —— 含词法/语法，返回 `(modules, global_diagnostics, module_by_path)`；
- **Phase 1** `resolve::collect_module` —— 逐模块收集顶层声明（先于导入绑定，保证跨模块引用可见）；
- **Phase 2a** `resolve::bind_imports`；
- **Phase 2b** `check::build_type_defs` —— 字段/方法签名建类型注册表；
- **Phase 2c** `resolve_class_parents` + `check::globals::collect` + `check::fields::check_fields` + `check::globals::check_all`；
- **Phase 2d** `check::check_module` —— 声明级检查 + 函数体/顶层语句类型检查；
- 之后校验 `generic_uses` 的 `type_param_bounds`；
- 每步检查 `loader.is_cancelled()`，取消则 `Diagnostic::error("分析已取消，产物不可使用")`；
- **收尾**（L150-216）：构造 `SemaResult` 的只读投影，最后 `result.declarations = DeclarationIndex::build(&result.modules, &result.bindings)`。

**HIR 的入口位置**：CLI 在 `analyze` 之后先渲染诊断、`has_errors()` 为真就退出；否则才 `build_hir(&result)`。HIR 从不自己触发 analyze。

### 16.11 消费点速查（文件路径:行号）

| 消费点 | sema 字段 |
|--------|-----------|
| `build/module.rs:5-51` | `diagnostics.has_errors()`、`modules.modules`、`AnalyzedModule.id/path/program/source`、`syntax_failed/load_failed` |
| `build/module.rs:179-205` | `declaration_types.get(module, function.span)` |
| `build/context.rs:104-147` | `expression_types.get(module, span)` |
| `build/types.rs:5-19` | `annotation_types.get(module, annotation.span())` |
| `build/types.rs:21-65,145-214` | `types.summaries`（TypeSummary/FieldSummary/MethodSummary） |
| `build/call.rs:111-121` | `expression_types` + `declarations.named/parameters` |
| `build/aggregate.rs:35-45` | `declarations.named/members` |
| `build/aggregate.rs:102-110` | `exhaustive_matches.contains(&(module, node.span))` |
| `build/coalesce.rs:16` | `expression_types.get(module, left.span())` |
| `build/stmt.rs:182` | `declaration_types.get(module, variable.span)` |

**所有查表键都是 `(ModuleId, Span)`**；Span 取自 AST 节点的 `span()`（表达式/注解）或声明的 `span`（签名/变量）。`ModuleId` 就是 `usize`（module/mod.rs:21-22），即模块在 `SemaResult.modules` 中的下标，入口恒为 0。


---

## 第 17 章 真实 HIR 示例（从 `hir_smoke` 生成）

> 本章所有 JSON 片段都来自实际运行 `cargo run -q -p novella-hir --example hir_smoke -- --json` 的输出（181157 字节，exit=0）。源程序见 `crates/novella-hir/examples/hir_smoke.rs`（165 行）。这些片段展示了前面各章描述的降级规则在真实输出中的样子。

### 17.1 顶层结构

```json
{
  "modules": [
    {
      "id": 0,
      "path": "/virtual/smoke.nov",
      "items": [ /* 14 项 */ ],
      "types": [ /* 1 项：Struct Point */ ]
    }
  ]
}
```

14 个函数项的 id：

```
m0::main, m0::add, m0::classify, m0::fallback, m0::shortcircuit,
m0::pairs, m0::pattern, m0::matching, m0::coverage, m0::sum,
m0::Point::norm2, m0::Point::origin, m0::lambda#1904, m0::lambda#1965
```

注意 `m0::lambda#1904` 与 `m0::lambda#1965` —— 两个源码完全相同的 lambda，靠 `span.start` 区分（第 13.3 节）。

### 17.2 `m0::main` —— Call 的形态（第 12 章）

`func main() { let result: i32 = sum(10); ... }` 中第一条语句：

```json
{
  "Instr": {
    "Call": {
      "callee": { "FunctionPtr": "m0::sum" },
      "args": [ { "IntConst": 10 } ],
      "dst": 0,
      "dst_ty": { "Int": "I32" }
    }
  }
}
```

- `callee` 是 `FunctionPtr("m0::sum")` —— 静态符号键调用（第 15.4 节 codegen 直接查表）。
- `IntConst(10)` 是 `UntypedInt` 落地后的结果（第 14.4 节 coerce）。
- `dst: 0` 是构造期分配的第一个局部槽（第 9.1 节 `fresh_local`）。

### 17.3 `m0::sum` —— 循环与 `HirCond.prelude`（第 14.6 节）

源码：`while (i < n) { total += i; i = i + 1; }`

```json
{
  "Block": {
    "stmts": [
      { "Instr": { "Alloc": { "id": 1, "ty": { "Int": "I32" }, "init": { "IntConst": 0 } } } },
      { "Instr": { "Alloc": { "id": 2, "ty": { "Int": "I32" }, "init": { "IntConst": 0 } } } },
      {
        "Loop": {
          "cond": {
            "prelude": {
              "stmts": [
                { "Instr": { "Load": { "dst": 3, "src": 2 } } },     // i
                { "Instr": { "Load": { "dst": 4, "src": 0 } } },     // n (形参)
                {
                  "Instr": { "Binary": {
                    "op": "Lt",
                    "lhs": { "Local": 3 }, "rhs": { "Local": 4 },
                    "lhs_ty": { "Int": "I32" }, "rhs_ty": { "Int": "I32" },
                    "dst": 5, "dst_ty": "Boolean"
                  } }
                }
              ]
            },
            "value": { "Local": 5 }
          },
          "body": {
            "stmts": [
              { "Block": { "stmts": [
                { "Instr": { "LocalAddress": { "local": 1, "ty": { "Int": "I32" }, "dst": 6 } } },
                { "Instr": { "Unary": { "op": "Deref", "operand": { ... }, ... } } },
                ...
              ] } }
            ]
          },
          "step": { "stmts": [] }     // while 循环没有 step
        }
      }
    ]
  }
}
```

**验证要点**：

- 条件求值的两条 `Load`（读 `i` 和 `n`）放在 **`HirCond.prelude`** 里，而**不是**循环体外——codegen 的 `loop_block` 每轮发射 prelude（第 15.4 节），保证 `i` 每轮都重新读取。
- `Binary` 携带 `lhs_ty` / `rhs_ty` / `dst_ty` —— 第 4.5 节的不变量在 JSON 中清晰可见。
- `while` 的 `step` 为空；C-for 和 for-in 的 step 非空。

### 17.4 `m0::classify` —— `if / elif / else` 与嵌套 If（第 14.5 节）

源码：`if (n < 0) { return -1; } elif (n == 0) { return 0; } else { return 1; }`

```json
{
  "stmts": [
    { "Instr": { "Load": { "dst": 3, "src": 0 } } },
    { "Instr": { "Binary": {
      "op": "Lt", "lhs": { "Local": 3 }, "rhs": { "IntConst": 0 },
      "lhs_ty": { "Int": "I32" }, "rhs_ty": { "Int": "I32" },
      "dst": 4, "dst_ty": "Boolean"
    } } },
    { "If": {
      "cond": { "prelude": { "stmts": [] }, "value": { "Local": 4 } },
      "then_block": { "stmts": [
        { "Block": { "stmts": [
          { "Instr": { "Unary": { "op": "Neg", "operand": { "IntConst": 1 },
                                  "dst": 5, "dst_ty": { "Int": "I32" } } } },
          { "Instr": { "Return": { "Local": 5 } } }
        ] } }
      ] },
      "else_block": { "stmts": [ /* 嵌套的第二个 If（elif） */ ] }
    } }
  ]
}
```

- 条件前置指令（`Load` + `Binary`）摊平在**当前块**，位于 `If` 之前——正是 `emit_if` 的摊平规则（第 14.5 节）。
- `elif` 变成 `else_block` 里的嵌套 `If`，`else` 是链尾。

### 17.5 `m0::Point::norm2` —— 实例方法的 self 槽位（第 8.3 / 16.9 节）

```json
{
  "id": "m0::Point::norm2",
  "signature": {
    "id": "m0::Point::norm2",
    "params": [],
    "ret": { "Int": "I32" },
    "type_param_count": 0,
    "is_async": false, "is_extern": false,
    "receiver": true
  },
  "params": [
    { "id": 0, "name": "self", "ty": { "Pointer": { "Struct": [0, "Point"] } } }
  ],
  "body": { "stmts": [ /* 见下 */ ] }
}
```

**注意签名与槽位的不对称**：`signature.params = []`（self 被 sema 剥离，第 16.9 节），但 `params` 里有一个 self 槽位（HIR 为函数体重建，类型 `Pointer(Struct(0,"Point"))`）。

函数体（`return self.x + self.y;`）：

```json
{ "Instr": { "Unary": { "op": "Deref", "operand": { "Local": 0 },
                         "dst": 1, "dst_ty": { "Struct": [0, "Point"] } } } },
{ "Instr": { "FieldLoad": {
    "target": { "Local": 1 }, "target_ty": { "Struct": [0, "Point"] },
    "field": "x", "index": 0, "ty": { "Int": "I32" }, "dst": 2
} } },
{ "Instr": { "Unary": { "op": "Deref", "operand": { "Local": 0 },
                         "dst": 3, "dst_ty": { "Struct": [0, "Point"] } } } },
{ "Instr": { "FieldLoad": {
    "target": { "Local": 3 }, "target_ty": { "Struct": [0, "Point"] },
    "field": "y", "index": 1, "ty": { "Int": "I32" }, "dst": 4
} } },
{ "Instr": { "Binary": { "op": "Add", "lhs": { "Local": 2 }, "rhs": { "Local": 4 }, ... } } },
{ "Instr": { "Return": { "Local": ... } } }
```

- `self.x` 先 `Deref` self 指针得到 struct 值，再 `FieldLoad{index:0}`。
- 字段索引 `0` / `1` 来自 `instance_fields` 的父→子布局（第 8.4 节）。
- **codegen 目前不支持 receiver=true 的函数**（第 15.2 节），所以这段 HIR 能生成 JSON，但不能发射 LLVM——这正是"HIR 支持 ≠ codegen 支持"的实例。

### 17.6 `HirTypeDef` —— Struct Point

```json
{
  "Struct": {
    "name": "Point",
    "fields": [
      { "name": "x", "ty": { "Int": "I32" }, "is_weak": false, "is_static": false },
      { "name": "y", "ty": { "Int": "I32" }, "is_weak": false, "is_static": false }
    ],
    "methods": [
      { "name": "norm2", "signature": { "id": "m0::Point::norm2", "params": [],
          "ret": { "Int": "I32" }, ..., "receiver": true }, "is_static": false },
      { "name": "origin", "signature": { "id": "m0::Point::origin", "params": [],
          "ret": { "Struct": [0, "Point"] }, ..., "receiver": false }, "is_static": true }
    ]
  }
}
```

### 17.7 `m0::matching` —— match → 嵌套 If + 结果槽（第 13.2 节）

源码：`let result: i32 = match (1 as i32) { case pattern(&counter) => 10 case pattern(&counter) => 20 default => 30 };`

```json
{ "Instr": { "Alloc": { "id": 0, "ty": { "Int": "I32" }, "init": { "IntConst": 0 } } } },
{ "Block": { "stmts": [
  "EnterScope",
  { "Instr": { "Alloc": { "id": 1, "ty": { "Int": "I32" }, "init": null } } },  // 结果槽
  { "Block": { "stmts": [
    { "If": {
      "cond": {
        "prelude": {
          "stmts": [
            { "Instr": { "LocalAddress": { "local": 0, "dst": 6 } } },
            { "Instr": { "Call": { "callee": { "FunctionPtr": "m0::pattern" },
                                   "args": [{ "Local": 6 }], "dst": 7, "dst_ty": {...} } } },
            { "Instr": { "Binary": {
              "op": "Eq", "lhs": { "IntConst": 1 }, "rhs": { "Local": 7 },
              "dst": 5, "dst_ty": "Boolean"
            } } }
          ]
        },
        "value": { "Local": 5 }
      },
      "then_block": { "stmts": [
        { "Instr": { "Store": { "id": 1, "value": { "IntConst": 10 } } } }
      ] },
      "else_block": { "stmts": [ /* 第二个 case 的 If + default 的 Store(30) */ ] }
    } }
  ] } }
] } }
```

**验证要点**：

- 结果槽 `id=1` 在 match 开始时 `Alloc(init: null)`，每个分支 `Store` 对应值。
- **case 条件表达式 `pattern(&counter)` 放在 `HirCond.prelude` 里**——只有到达这个 case（前面的都没命中）才求值。冒烟验证：两 case 都命中第一个，`counter` 最终 1（第二个 case 的 `pattern` 未调用），`result = counter*100 + 10 = 110`。
- `subject`（`1 as i32`）落地为 `IntConst(1)` 常量，无需单独槽位。

### 17.8 默认实参 `add(a, b = a)` 的调用点

源码 `add(2)` 在 `coverage()` 里，降级为调用点补齐 default：

```json
// coverage() 中（示意，从 add 的函数体反推参数槽）
{ "Instr": { "Call": {
  "callee": { "FunctionPtr": "m0::add" },
  "args": [ { "IntConst": 2 }, { "Local": <a 的快照> } ],
  "dst": ..., "dst_ty": { "Int": "I32" }
} } }
```

- `add` 自身函数体（17.2 的 `m0::add`）里 `a` 是槽 0、`b` 是槽 1，`return a + b` 先 `Load` 两个槽再 `Binary Add`。
- 调用 `add(2)` 时 `append_default_arguments` 在**定义环境**求值 `b = a`，把 `a=2` 的快照作为第二个实参（第 12.2 节）。

### 17.9 冒烟验证结果

契约文档（docs/hir-handover.md）记录的解释器验证：

| 函数 | 结果 |
|------|------|
| `sum(10)` | 45 |
| `coverage()` | 26 |
| `shortcircuit()` | 24 |
| `matching()` | 110 |
| `cross()` | 23 |

以及 11 项拒绝路径非零退出、workspace 102 测试通过、`fmt` / `clippy` / `check` / `diff` 全绿。这证明 HIR 降级后的指令流语义正确——但注意：**这是解释器验证，不等于 LLVM/机器码/ABI 已验证**（hir-handover.md:92 明确）。


---

## 第 18 章 设计不变量、fail-fast 与行为契约

### 18.1 七条不可破坏的设计不变量

调研全部源码后，以下约束是 HIR 正确性的基石（改动时必须遵守）：

1. **类型唯一事实源**：HIR 的类型只来自 sema 的三张投影表（`expression_types` / `annotation_types` / `declaration_types`）和 `types.summaries`。绝不重新解析类型名，绝不猜类型。查询失败一律 `MissingExpressionType` / `MissingType` fail-fast。
2. **HIR 无表达式树**：所有表达式都降到 `HirBlock` / `HirStmt` / `HirInstr` 序列，每个中间结果落 `LocalId` 槽。codegen 永远不需要"求值表达式"，只需顺序发射指令。
3. **未定型输入零容忍**：`TypeView::Unknown` / `Error` / `IntLiteral` / `FloatLiteral` / `Null` / 联合 / 泛型形参 / Applied/Future/Object 不得原样进入 HIR。`HirType::from_view_with` 负责在构造期拦截，转成 `BuildError`。
4. **稳定函数身份**：每个函数有唯一 `HirFunctionId`——顶层 `m{module}::{name}`、方法 `m{module}::{Type}::{method}`、lambda `m{module}::lambda#{span.start}`、匿名占位 `m{module}::anon#{params.len()}`。函数表按此键建，重复即错。
5. **整体 fail-fast**：`build_hir` 遇到第一个无法降级的构造就返回 `BuildError`，不收集多个错误、不产生部分 `HirProgram`。调用方（CLI）先检查 `diagnostics.has_errors()`。
6. **defined-scope 求值**：默认实参和字段初值必须在定义模块环境求值（第 9.3 节 `in_definition_scope`），按序展开，能引用此前参数的快照。
7. **求值顺序保真**：普通赋值先 RHS 后定位 LHS；复合赋值定位一次、RHS 前读旧值；自增自减返回旧值；局部读生成快照；条件 prelude 每轮执行；case 只在前项未命中时求值；`??` 非空不执行 backup。

### 18.2 fail-fast 的两道闸门

HIR 的错误分两层，各自对应不同的失败阶段：

**闸门一：入口闸门（`build_hir`）**：

```rust
if sema.diagnostics.has_errors() { return Err(BuildError::at(InvalidInput)); }
for module in &sema.modules.modules {
    if module.syntax_failed || module.load_failed {
        return Err(BuildError::at(InvalidInput).with_site(module.id, Span { start: 0, end: 0 }));
    }
}
```

这是"语义已通过"的前提检查。前置诊断（词法/语法/语义错误）由 sema 唯一存储，HIR 只读取 `has_errors()`。

**闸门二：降级闸门（各 build 函数）**：`BuildErrorKind`（error.rs:51-75）全部变体：

| 变体 | 含义 |
|------|------|
| `InvalidInput` | 上游诊断有错或坏模块（闸门一） |
| `OpaqueType` | `TypeView::Unknown` 进入类型位置 |
| `UntypedLiteral{what}` | 未定型字面量未落地 |
| `UnknownTypeName{name}` | 类型名无法解析 |
| `UnionUnsupported` | 联合类型不支持 |
| `GenericUnsupported` | 泛型参数/泛型调用不支持 |
| `Unimplemented{node}` | 明确不支持的语言构造 |
| `MissingType{what}` | 类型表查询失败 |
| `MissingExpressionType` | 表达式类型表查询失败 |
| `ImplicitCast{from, to}` | 需要隐式转换但不可转换 |

`BuildError` 结构（error.rs）：

```rust
pub struct BuildError {
    pub kind: BuildErrorKind,
    pub module: Option<ModuleId>,
    pub span: Option<Span>,
}
```

`with_site(module, span)` 只填第一个非空值（保留最初的错误位置）。Display → `"HIR 降级失败：{kind}"`。CLI 的 `lower()`（main.rs:127-151）把它转成 rustc 风格诊断：用 `error.module` 找源码、`error.span` 经 `LineIndex::locate` 推行列。

### 18.3 不支持构造的完整清单（契约文档）

**明确拒绝**（`Unimplemented` / 特定错误）：

- 异常：`try` / `catch` / `finally` / `throw`
- 异步/挂起：`async` / `await` / `yield`
- 动态派发：`new` / class 动态方法 / 绑定方法值 / 间接调用带 receiver
- 编译器指令：`CompilerDirective` / `DirectiveBlock` / 带 attributes 的声明
- 闭包捕获：lambda 引用外层局部
- 类型系统：泛型 / 联合 / 不透明类型 / Applied / Future / Object / TypeParam
- 字面量：格式化串（f-string）/ 字节串（b-string）/ 未定义转义
- 聚合：动态对象字面量 / 非数组 for-in / 静态字段存储 / weak 字段 / 有成员 trait / 聚合或 string 身份比较
- match：无 default 且非穷尽的值 match / 值 match 中的块分支
- 调用：带省略默认实参的间接函数（FunctionView.name=None）/ 显式泛型实参
- 类型特例：trait 有 members / 泛型 type alias / 带编译器指令的类型或成员

**HIR 支持但 codegen 拒绝**（第 15 章）：float、string、array、tuple、struct/class 值、指针指向结构/数组、extern、global、receiver=true（所有方法）、`FieldLoad/Store`、`Tuple*`、`Array*`、`Index*`、`TypeTest`、`PtrDiff`、间接调用。

### 18.4 求值顺序的完整对照

| 场景 | 顺序 | 实现位置 |
|------|------|----------|
| 普通赋值 `a = f()` | **先 RHS**（`f()`），再定位 LHS（index） | operators.rs:build_assignment |
| 复合赋值 `a[i] += f()` | **先定位 index**，再**读旧值**，再求 RHS `f()`，最后 Binary+Store | operators.rs:build_assignment |
| 自增 `p++` | 求地址 → 读旧值 → Binary(Add, old, 1) → StoreIndirect → 返回 old | operators.rs:build_unary |
| 局部标识符 `x` | `Load` 生成快照 Local（读时值固定） | expr.rs:Identifier |
| `self.x`（struct） | 若 self 是 `Pointer(Struct)`，`Deref` 后 `FieldLoad` | expr.rs:implicit_member |
| 条件 `while (f())` | `f()` 在 `HirCond.prelude`，每轮执行 | stmt.rs:While / codegen loop_block |
| `if / elif` | 条件前置指令摊平到当前块，按序执行 | stmt.rs:emit_if |
| match case | 逆序构造嵌套，但求值顺序是正向（第一个 case 先测） | aggregate.rs:build_match |
| `p ?? backup` | 左侧定型 → `Ne(null)` 测试 → 非空存左侧 / 空求 backup | coalesce.rs |
| 默认实参 `f(a, b=a)` | 调用点按序求值，缺省值在定义环境展开 | call.rs:append_default_arguments |
| `for-in` | 数组求一次，长度每轮读 | loops.rs |

### 18.5 `HirCond` 的深层含义：为什么 prelude 必须每轮执行

这是一个容易出错的设计点。`HirCond` 定义（body.rs:298）：

```rust
pub struct HirCond {
    /// 条件求值的前置语句
    pub prelude: HirBlock,
    /// 最终的条件值（布尔操作数）
    pub value: HirOperand,
}
```

**问题场景**：`while (i < n)` 的条件表达式 `i < n` 需要两条 `Load` 指令读 `i` 和 `n`。如果把这些 `Load` 放在循环体外只执行一次，`i` 的值就不会更新，循环死循环。

**HIR 的解决**：`Load` 放进 `prelude`，`value` 只是 `Local(test)`。`codegen::loop_block` 每轮先发射 `cond.prelude`（body.rs:393-397），再判断 `value`。`if` 的 `branch` 也在入口发射 prelude（327-373）。

**契约**：**所有条件求值的副作用都必须在 prelude 里**，HIR 构造方不能把条件求值的指令放在块外。codegen 必须每轮发射 prelude，不能优化成"只算一次"——除非条件是常量（`HirCond::prelude.is_empty()` 且 `value` 是常量时），此时 codegen 做常量折叠。

### 18.6 `Loop.step` 的深层含义：continue 必须执行 step

`HirStmt::Loop`（body.rs:100-132）：

```rust
Loop {
    cond: Option<HirCond>,   // None = 无限循环
    body: HirBlock,
    step: HirBlock,          // 正常尾部和 continue 执行；break/return 跳过
}
```

**问题场景**：C-for `for (i=0; i<10; i++) { if (x) continue; }`。如果 codegen 把 `continue` 直接跳到循环条件（head），`i++` 就被跳过，可能死循环。

**HIR 的解决**：`continue` 跳 `step`，step 执行后再回 head。codegen 实现（body.rs:433-434）：

```rust
let step_reachable = body_falls || targets.has_continue;
targets.step = head;    // step 执行后接回 head
```

`break` 和 `return` 直接跳 exit，跳过 step。

**契约**：后端实现循环时，必须为 `continue` 提供独立的 step 跳转目标，不能简单跳 head。

### 18.7 HIR 与解释器的验证关系

契约文档（hir-handover.md:92）明确："HIR 解释器验证不代表 LLVM/机器码/ABI/RC 已验证。" HIR 层验证了降级语义（指令序列执行结果正确），但：

- LLVM IR 发射正确性由 codegen 的 `module.verify()` 保证；
- 机器码/ABI 由目标文件 + 链接器验证；
- 字符串/数组布局与运行时支持尚未实现（codegen 不支持这些类型）；
- 引用计数/内存管理目前没有运行时。


---

## 第 19 章 扩展指南：如何安全地改动 HIR

### 19.1 新增一种表达式/语句降级

检查清单（基于各分派点的穷尽 match）：

1. **AST 层**：新节点必须带 `span` 并 `#[derive(Spanned)]`，更新语法文法和 sema 类型检查（见 `docs/sema-deep-dive.md`）。
2. **sema 层**：新的表达式必须经 `check_expr_recorded` 或 `check_expr_with_hint` 登记类型——否则 HIR 的 `expr_type(span)` 会 `MissingExpressionType`。
3. **HIR 表达式分派**（expr.rs:31-278 `build_expression_inner`）：加一个 match 分支，决定产出什么 `HirOperand`、发射什么 `HirInstr`。如果需要新指令，转第 19.2 节。
4. **HIR 语句分派**（stmt.rs:19-127 `emit_statement`）：加一个 match 分支，产出 `HirStmt`。
5. **更新契约文档** `docs/hir-handover.md` 的覆盖表和拒绝清单。
6. **更新本文档**相应章节。

**不要**在 `build_expression_inner` 之外新建第二条表达式降级路径——所有表达式都必须经过 `build_expression` / `build_typed_expression` 入口，否则 `preferred` 提示和 coerce 逻辑会被绕过。

### 19.2 新增一种 HIR 指令

1. 在 body.rs 的 `HirInstr` 枚举加变体，字段必须带**完整类型信息**（参考 `Binary` 的 `lhs_ty/rhs_ty/dst_ty`，`TypeTest` 的 `operand_ty/target`）。这是"HIR 自描述"不变量的要求——codegen 不能反推类型。
2. serde 自动派生（HIR 全量 serde），`--emit hir` 的 JSON 结构随之变化——检查下游消费者。
3. codegen 的 `instruction()`（body.rs:143-290）加分支；如果不支持，明确 `Err("暂不支持 HIR 指令 {instruction:?}")`，不要静默忽略。
4. 若需要新的 `HirOperand` 变体，同理更新 `operand()`（body.rs:106-141）。
5. 更新本文档第 4.5 节指令全集表和第 15.4 节 codegen 消费表。

### 19.3 新增一种 HIR 类型

1. 在 ty.rs 的 `HirType` 加变体。
2. `from_view_with`（ty.rs:208-290）加 `TypeView → HirType` 的转换分支——如果对应 TypeView 变体不存在，说明要先扩展 sema 的 TypeView。
3. `IntTy`/`FloatTy` 的 `bit_width` / `is_signed`（若适用）。
4. codegen 的 `value_type`（lib.rs:67-82）加 LLVM 映射，或明确拒绝（默认 `_ => Err` 已覆盖，不会漏）。
5. helpers.rs 的 `same_value_type` / `castable` / `type_label` 视情况更新。
6. 更新本文档第 3.2 节和第 15.3 节映射表。

**注意**：`HirType` 变体在 match 中是穷尽的（codegen 的 `value_type` 有 `_ => Err`，但 `from_view_with` 穷尽 TypeView）——新增变体时编译器会提示所有需要更新的 match，逐一处理。

### 19.4 修改求值顺序的注意点

求值顺序是 HIR 最微妙的契约（第 18.4 节）。改动涉及：

- `build_assignment`（operators.rs:193-250）：普通赋值 vs 复合赋值的 RHS/定位/读旧值顺序；
- `build_unary`（operators.rs:6-73）：自增自减返回旧值；
- `build_match`（aggregate.rs:83-169）：case 条件的短路求值；
- `emit_if`（stmt.rs:297-336）：条件前置指令的摊平；
- `HirCond.prelude` 与 `Loop.step`：codegen 必须配合（第 18.5 / 18.6 节）。

**验证手段**：用 `examples/hir_smoke.rs` 这样的副作用计数器（如 `pattern(&counter)`、`fallback(&counter, ...)`）构造冒烟测试——JSON 里的指令顺序能直接看出求值顺序是否正确。

### 19.5 反模式清单

- ❌ 在 HIR 里重新解析类型名——类型只从 sema 投影表取。
- ❌ 用 `TypeView::Unknown` 或 `Error` 静默兜底——必须 fail-fast。
- ❌ 在 HIR 之外新建表达式降级路径——绕过 preferred/coerce。
- ❌ 让 codegen 猜测类型——每个指令/操作数自带完整类型。
- ❌ 依赖 lambda 的签名 id 做唯一实体键——`anon#N` 只是签名占位。
- ❌ 把 `with_receiver` / `loop_depth` 等构建期状态泄漏到 HIR 数据结构——它们是 `BodyBuilder` 的内部字段，产物里不应出现。
- ❌ 在 codegen 中把 `continue` 直接跳 head 跳过 step——破坏 `Loop.step` 契约。
- ❌ 把条件求值指令放在 `HirCond.prelude` 之外——会变成只求值一次。
- ❌ 下游（CLI/codegen）重跑 lexer/parser——`SemaResult.modules` 已有完整产物。
- ❌ 在 `preprocess.rs` 之外过滤注释 token——注释过滤只有一处（项目约定）。

### 19.6 与项目约定的对齐

根据 `AGENTS.md` / 根 `AGENTS.md`：

- 注释、doc、文档均中文（本文档遵守）；
- 大文件按职责拆目录（HIR 的 `build/` 目录即是——单文件超约 600 行拆分）；
- 遇到阻塞点立即停下报告，不猜测（HIR 的 fail-fast 是这一约定的代码化）；
- 不写无价值测试（HIR 的验证靠冒烟示例与契约文档记录的 102 测试）；
- 默认不兼容改动直接替换并在交付说明中注明（契约文档记录"公开 API/serde 改动无迁移直接替换"）。

---

## 第 20 章 符号速查表

### 20.1 `lib.rs` 与 `build.rs`

| 符号 | 位置 | 说明 |
|------|------|------|
| `build_hir` | build/module.rs:9-51（从 build.rs 重导出） | 唯一入口：`&SemaResult → Result<HirProgram, BuildError>` |
| `BuildError` / `BuildErrorKind` | build/error.rs | 降级失败，10 种 kind |
| `ModuleBuilder<'a>` | build.rs:74-90 | 模块级构建状态 |
| `BodyBuilder<'a>` | build.rs:93-119 | 函数体级构建状态 |
| `translate_*` / `emit_*` | build/ 各子模块 | 见 6.4 责任矩阵 |

### 20.2 数据模型

| 符号 | 位置 | 说明 |
|------|------|------|
| `HirType` | ty.rs:20-46 | 定型类型（12 变体） |
| `IntTy` / `FloatTy` | ty.rs:53-140 | 定宽整数/浮点 |
| `HirFunctionId` | ty.rs:148 | 稳定函数键 |
| `FunctionSignature` | ty.rs:166-184 | 函数签名（receiver 不占 params） |
| `HirOperand` | body.rs:32-70 | 已求值操作数（12 变体） |
| `HirBlock` / `HirStmt` / `HirInstr` | body.rs:74/100/136 | 块/语句/指令 |
| `HirCond` | body.rs:298 | `{prelude, value}`，prelude 每轮执行 |
| `HirUnaryOp` / `HirBinaryOp` | body.rs:278/323 | 运算符枚举 |
| `LocalId` | body.rs:28 | 函数体内唯一槽位号 |
| `HirProgram` / `HirModule` / `HirItem` | item.rs:17/30/43 | 程序/模块/顶层项 |
| `HirFunction` / `LocalSlot` / `HirGlobal` | item.rs:57/75/84 | 函数/形参槽/全局 |
| `HirTypeDef` / `HirMethod` / `HirField` | item.rs:106/132/141 | 类型定义/方法/字段 |
| `HirVisibility` | item.rs:95 | Public/Private/Package |

### 20.3 构建子模块

| 文件 | 主要函数 | 职责 |
|------|----------|------|
| `build/module.rs` | `build_hir` / `build_module` / `collect_declarations` / `build_function` / `validate_declaration` | 入口、模块编排、声明登记 |
| `build/context.rs` | `BodyBuilder::new` / `fresh_local` / `expr_type` / `in_definition_scope` / `with_scope` | 构建状态与作用域 |
| `build/types.rs` | `annotation_type` / `finish_types` / `method_functions` / `hir_methods` / `module_function_id` | 类型定义与方法函数 |
| `build/layout.rs` | `instance_fields` | 父→子字段布局 |
| `build/expr.rs` | `build_expression` / `build_typed_expression` / `build_expression_inner` | 表达式主分派 |
| `build/operators.rs` | `build_unary` / `build_binary` / `build_short_circuit` / `build_assignment` / `field_slot` / `implicit_member` | 运算符、短路、赋值 |
| `build/call.rs` | `build_call` / `append_default_arguments` | 调用与默认实参 |
| `build/lvalue.rs` | `local_address` / `aggregate_address` / `build_address` / `field_address` / `read_address` | 左值定位 |
| `build/aggregate.rs` | `build_struct_init` / `build_match` / `match_body_block` / `build_lambda` / `build_literal` | 聚合、match、lambda、字面量 |
| `build/coalesce.rs` | `build_coalesce` | `??` 空合并 |
| `build/stmt.rs` | `build_statement` / `emit_statement` / `emit_with` / `emit_variable` / `bind_pattern` / `coerce` / `emit_if` / `emit_for` / `emit_c_for` | 语句与控制流 |
| `build/loops.rs` | `emit_for_in` | 数组 for-in |
| `build/helpers.rs` | `visibility_of` / `castable` / `binary_op_of` / `compound_op_of` / `parse_int_literal` / `same_value_type` / `function_id_of` | 助手函数 |

### 20.4 codegen 消费点

| 符号 | 位置 | 说明 |
|------|------|------|
| `emit_object` | codegen/src/lib.rs:85-187 | 公共入口：`&HirProgram → Vec<u8>` (ELF) |
| `entry` | codegen/src/lib.rs:37-65 | 入口函数判定 |
| `value_type` | codegen/src/lib.rs:67-82 | HirType → LLVM 类型（仅标量） |
| `body::emit` | codegen/src/body.rs:39-44 | 函数体发射 |
| `Body::instruction` | codegen/src/body.rs:143-290 | 指令发射（bool=是否贯穿） |
| `Body::block` | codegen/src/body.rs:292-325 | 语句块发射 |
| `Body::branch` | codegen/src/body.rs:327-373 | 条件分支 |
| `Body::loop_block` | codegen/src/body.rs:375-449 | 循环（step 接回 head） |
| `operators::binary` | codegen/src/operators.rs:51-145 | 二元运算 + 陷阱 |
| `operators::trap_if` | codegen/src/operators.rs:147-164 | llvm.trap |

### 20.5 相关文档

| 文档 | 关系 |
|------|------|
| `docs/hir-handover.md` | HIR 契约权威（覆盖表、拒绝清单、验证结果） |
| `docs/compiler-design.md` §7-9 | 编译器总体设计中的 HIR §7、codegen §8、CLI §9 |
| `docs/sema-deep-dive.md` | 输入侧：sema 如何产生 HIR 消费的类型表 |
| `docs/parser-deep-dive.md` | 上游：语法消歧（HIR 里已消除） |
| `docs/sema-coverage.md` | 语义规则覆盖矩阵 |

---

## 结语

novella-hir 是整个编译器里承上启下的关键层：它把 sema 的**语义事实**（定型类型、声明绑定、穷尽性证明）翻译成 codegen 可以直接发射的**顺序指令流**，同时把所有不支持的构造挡在构造期（fail-fast）。

理解 HIR 的三个核心视角：

1. **数据模型**：`HirType`（完全定型、无语法上下文）、`HirBlock→HirStmt→HirInstr`（非表达式树）、`HirProgram→HirModule→HirItem`（稳定符号键）——见第 3-5 章。
2. **降级流程**：`build_hir` 两遍（先登记声明，再降级函数体），`ModuleBuilder`/`BodyBuilder` 持有构建状态，各子模块按职责分工——见第 6-14 章。
3. **边界契约**：上游消费 sema 只读投影（第 16 章），下游被 codegen 只消费标量子集（第 15 章），求值顺序/条件 prelude/Loop.step 是跨层不变量（第 18 章）。

任何对 HIR 的修改都应回到这三条：**类型从 sema 取、表达式降到指令、不支持就报错**。这是整个中间表示层保持简洁可验证的根本原因。


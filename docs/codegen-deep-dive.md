# novella-codegen 深度技术文档

> 历史快照说明（2026-11-22）：本文的逐行分析保留编写时状态。后端现已支持静态全局、
> x86-64 裸机 small/kernel 目标、显式 C ABI 导出和无 libc runtime；本轮现状以
> [裸机支持](freestanding.md) 与 [编译器设计](compiler-design.md) 为准。下文关于仅 Linux、
> 不支持全局及固定入口的旧描述不适用于新目标。§19–§23 给出的"struct/数组/字符串/
> trait 一律拒绝"同样已过时：2026-11-27 起 HIR 单态化与 `dyn` 见证表已接通，
> [`examples/dyn-traits.nov`](design.md) 等程序可原生编译运行。

> 文档版本：2026-11-21
> 适用代码：novella 编译器 workspace（`crates/novella-codegen`，edition 2024）
> 工具链实测：inkwell 0.11.0 / LLVM 23.1.4 / clang 23.1.4（`llvm-config --version` = 23.1.4）
> 配套文档：`docs/hir-deep-dive.md`（上游 HIR 契约）、`docs/hir-handover.md`（HIR 交接速查）、`docs/compiler-design.md` §8–9（整体设计中的后端与 CLI）、`docs/design.md`（语言语义）
>
> 本文中所有行号均相对于仓库根目录 `/home/moyan/projects/novella`，路径形如 `crates/novella-codegen/src/lib.rs:85-188`。
> 所有 Rust 代码片段均按当前磁盘源码逐字重建；凡属"设计意图 / 权衡 / 推断"的说明，均明确标注来源（源码注释、契约文档，或本文作者的推断），不把推断伪装成事实。
> codegen 是整条编译链的**最末端执行器**，本文力求把它从入口到机器码字节的每一个分支、每一处类型判定、每一条 fail-fast 边界都讲清楚。

---

## 导读：这份文档怎么读

### 面向谁

这份文档写给四类读者：

1. **要改 codegen 的人**：你需要知道三个源文件各自负责什么、`emit_object` 两遍扫描为什么这样设计、每一条 `HirInstr` 如何映射成 LLVM 指令、新增一个类型/指令要同步哪些 `match` 分支。第 2–16 章是你的主战场。
2. **要理解编译器全貌的人**：你想知道 HIR 交到后端之后，那份"已定型指令流"究竟如何变成 ELF 目标文件、为什么后端只支持这么小一个子集、链接是谁做的。第 1 章与第 21–23 章回答这个问题。
3. **要审查正确性的人**：你在意求值顺序、槽位分配时机、算术陷阱、控制流可达性、错误上下文链。第 10–19 章集中讨论这些。
4. **要扩展后端的人**：你想加浮点、字符串、聚合、间接调用支持。第 24–26 章给出扩展指南与已知技术债清单。

### codegen 在编译器中的位置

novella 的编译管线是一条单向依赖链（`AGENTS.md` OVERVIEW / `docs/compiler-design.md` §1）：

```
core ← ast ← parser ← sema ← hir ← codegen ← cli
core ← lexer ← parser
core ← lexer ← novella-lsp
```

`novella-codegen` 位于 **HIR 之后、CLI 之前**，是"最后一次形态转换"：

```
SemaResult ──build_hir──▶ HirProgram ──emit_object──▶ Vec<u8> (ELF .o) ──clang──▶ a.out
   (语义结果)              (定型指令流)               (目标文件字节)        (可执行程序)
```

它的**输入只有一个**：`&HirProgram`。它**绝不**回头碰 AST、`SemaResult`、lexer 或 parser——这一点是 `AGENTS.md` 明令的架构约束（"下游不自己再跑一遍 lexer / parser"、"codegen 只消费 HIR"）。

一句话概括 codegen 的职责：

> **把已完全定型、求值顺序已显式化、符号键已稳定的 `HirProgram`，通过 inkwell/LLVM 逐条发射成 `x86_64-unknown-linux-gnu` 的 ELF 目标文件字节；遇到任何无法在这个极小后端里生成的类型/指令/构造，立即 fail-fast 报错，绝不静默兜底。**

### 一个必须先建立的认知：这个后端非常小

这是理解整个 codegen 的钥匙。它不是通用 LLVM 后端，而是一个**刻意保守的标量后端**：

- 只支持 **整数**（i8/i16/i32/i64，含无符号与 usize/isize）、**布尔**（i1）、**指向整数/布尔的指针**这三种值类型；
- 只支持**无接收者、非异步、非泛型**的函数；
- 只支持**直接调用**（函数/函数指针操作数最终都解析到已登记符号，间接调用 `build_call` 被拒绝）；
- **浮点、字符串、数组、元组、struct/class、全局变量、extern 函数、间接调用、字段/索引访问、TypeTest 全部在发射期报错**；
- 控制流只支持 **if / loop / break / continue / return**，无 switch、无异常。

HIR 层能产出远比这丰富的指令（见 `crates/novella-hir/src/body.rs`），但 codegen 只实现了其中的一个子集。**HIR 能降级成功 ≠ codegen 能生成代码**——这是这套分层设计里最重要的边界认知（`docs/hir-handover.md` §5 也明确："解释器验证针对 HIR 求值与控制流，不代表 LLVM、机器码、内存 ABI 或引用计数已验证"）。

### 阅读路线

- **快速建立心智模型**：读第 1 章（定位）+ 第 5 章（`emit_object` 两遍扫描）+ 第 9 章（槽位机制），约 20 分钟。
- **逐文件精读**：第 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13 → 14 章，每章对应一个函数或一组函数。
- **带着问题查**：第 20 章有 fail-fast 全清单，第 28 章有符号速查表，第 23 章有端到端逐指令走查。
- **看真实源码**：第 29 章给出三个源文件的索引与关键代码片段定位。

### 贯穿全文的 mental model

读 codegen 代码时，脑子里始终放着这条"逐条发射流水线"：

```
HirProgram
  │  entry()             —— 定位入口（模块 0 的 m{id}::main）
  │  第一遍：登记声明      —— 所有函数先 add_function（含外部可见的 main）
  │  第二遍：发射函数体    —— Body 状态机：alloca + store + 控制流基本块
  │  void-main 包装       —— 生成真正的 int main() 调用 void 入口并返回 0
  │  verify()             —— LLVM IR 结构校验
  └─ write_to_memory_buffer(Object) —— 产出 ELF 字节
```

关键心法：

- **codegen 不解释表达式**：HIR 里每个值都已经在 `LocalId` 槽位里，codegen 只做 `load`/`store`/运算，不做求解。
- **所有 `alloca` 都在入口块首部**：`allocate()` 用一个独立的 builder 把 `alloca` 插到 `entry` 块第一条指令之前（`crates/novella-codegen/src/body.rs:81-95`），所以循环里的 `Alloc` 不会每轮重新分配栈。
- **返回值靠"是否 fall-through"判断**：`block()`/`instruction()` 返回 `bool` 表示"控制流是否继续往下走"，`false` 意味着已经 `return`/`break`/`continue`，从而触发 `unreachable` 收尾。

---

## 目录

1. [模块总览与文件地图](#1-模块总览与文件地图)
2. [技术栈与依赖：inkwell / LLVM / target-x86](#2-技术栈与依赖inkwell--llvm--target-x86)
3. [公共 API 全景：CodegenError 与 emit_object](#3-公共-api-全景codegenerror-与-emit_object)
4. [入口定位：entry()](#4-入口定位entry)
5. [两遍扫描架构总览](#5-两遍扫描架构总览)
6. [第一遍：目标机初始化与函数声明登记](#6-第一遍目标机初始化与函数声明登记)
7. [类型映射：value_type()](#7-类型映射value_type)
8. [函数体发射入口：body::emit()](#8-函数体发射入口bodyemit)
9. [Body 状态机与槽位管理](#9-body-状态机与槽位管理)
10. [操作数求值：operand()](#10-操作数求值operand)
11. [指令发射：instruction() 逐条解析](#11-指令发射instruction-逐条解析)
12. [标量运算：operators.rs](#12-标量运算operatorsrs)
13. [算术陷阱：trap_if() 与 llvm.trap](#13-算术陷阱trap_if-与-llvmtrap)
14. [控制流发射：block / branch / loop_block](#14-控制流发射block--branch--loop_block)
15. [void 返回 main 的包装](#15-void-返回-main-的包装)
16. [验证与目标文件发射](#16-验证与目标文件发射)
17. [错误处理与上下文链](#17-错误处理与上下文链)
18. [求值顺序与可达性：正确性专题](#18-求值顺序与可达性正确性专题)
19. [与 HIR 的契约：消费什么、拒绝什么](#19-与-hir-的契约消费什么拒绝什么)
20. [fail-fast 全清单](#20-fail-fast-全清单)
21. [与 CLI 的集成：emit obj / exe 与链接](#21-与-cli-的集成emit-obj--exe-与链接)
22. [端到端走查：native-main.nov 的每一条 IR](#22-端到端走查native-mainnov-的每一条-ir)
23. [已实现能力矩阵 vs 未实现矩阵](#23-已实现能力矩阵-vs-未实现矩阵)
24. [设计哲学与权衡分析](#24-设计哲学与权衡分析)
25. [已知限制与技术债](#25-已知限制与技术债)
26. [扩展指南：如何新增一个类型或指令](#26-扩展指南如何新增一个类型或指令)
27. [符号速查表](#27-符号速查表)
28. [附录：完整源码索引与验证命令](#28-附录完整源码索引与验证命令)

---

## 1. 模块总览与文件地图

### 1.1 目录结构

`crates/novella-codegen` 是整个 workspace 里**最小**的一个 crates 之一（仅 803 行 Rust）：

```
crates/novella-codegen/
├── Cargo.toml          # 9 行：inkwell 0.11 + novella-hir
└── src/
    ├── lib.rs          # 188 行：crate 根、公共 API emit_object、入口定位、类型映射、两遍扫描
    ├── body.rs         # 450 行：Body 状态机、槽位管理、指令发射、控制流发射
    └── operators.rs    # 165 行：一元/二元/转换运算、算术陷阱
```

行数统计（`wc -l` 实测）：

| 文件 | 行数 | 职责 |
|------|------|------|
| `crates/novella-codegen/src/lib.rs` | 188 | crate 根、`CodegenError`、`entry()`、`value_type()`、`emit_object()`、两遍扫描编排、void-main 包装 |
| `crates/novella-codegen/src/body.rs` | 450 | `Functions` 类型别名、`LoopTargets`、`Body` 结构、`emit()`、槽位管理、操作数求值、指令分派、控制流发射 |
| `crates/novella-codegen/src/operators.rs` | 165 | `unary()` / `cast()` / `binary()` / `trap_if()`（作为 `Body` 的 `impl` 分散在独立文件） |
| **合计** | **803** | — |

### 1.2 三个文件的职责边界

```
lib.rs（编排层 / 公共面）
  │  定义 CodegenError、Result<T>
  │  定义 entry()（入口定位）
  │  定义 value_type()（HirType → LLVM 类型，被 body.rs / operators.rs 复用）
  │  定义 emit_object()（唯一的 pub 函数，编排两遍扫描）
  │
  ├── body.rs（函数体发射层）
  │     │  定义 Functions 别名、LoopTargets、Body 结构
  │     │  定义 emit()（单函数发射入口，被 lib.rs 第一遍之后调用）
  │     │  定义 Body 的 impl：槽位、operand、instruction、block、branch、loop_block
  │     └── 依赖 operators.rs 提供的运算 impl
  │
  └── operators.rs（运算层）
        └── 为 Body 再开一个 impl 块：unary / cast / binary / trap_if
```

**关键观察**：`operators.rs` 不是独立模块类型，而是给 `Body`（定义在 `body.rs`）追加 `impl` 块。这种"类型定义与实现分文件"的写法让 `body.rs` 的控制流逻辑与 `operators.rs` 的算术逻辑各自内聚，但共享同一个 `Body` 状态（builder、context、slots）。`operators.rs` 通过 `crate::body::Body` 与 `crate::{CodegenError, Result, value_type}` 反向引用 crate 根（`crates/novella-codegen/src/operators.rs:8-9`）。

### 1.3 公开面

整个 crate 对外只暴露三样东西（crate 根 `crates/novella-codegen/src/lib.rs:3-4` 只声明了两个私有 `mod`）：

- `pub struct CodegenError(String)` —— 唯一的错误类型（`:20-29`）
- `pub fn emit_object(&HirProgram) -> Result<Vec<u8>>` —— 唯一的公共函数（`:85`）
- `type Result<T>` 是私有别名（`:17`），但作为 `emit_object` 的返回类型的一部分被外部间接使用

`body` 与 `operators` 都是私有模块；`Body`、`Functions`、`LoopTargets` 都是 `pub(crate)` 或私有。这符合 `AGENTS.md` 的"crate 公开面最小化"约定。

---

## 2. 技术栈与依赖：inkwell / LLVM / target-x86

### 2.1 Cargo.toml 解析

```toml
[package]
name = "novella-codegen"
version.workspace = true
edition.workspace = true       # edition 2024
license.workspace = true       # Apache-2.0

[dependencies]
inkwell = { version = "0.11.0", default-features = false, features = ["llvm23-1-prefer-dynamic", "target-x86"] }
novella-hir = { version = "0.1.0", path = "../novella-hir" }
```

（`crates/novella-codegen/Cargo.toml` 全文）

两个依赖各有明确职责：

| 依赖 | 版本 | 作用 |
|------|------|------|
| `inkwell` | 0.11.0 | LLVM 的 Rust 安全封装。提供 `Context` / `Module` / `Builder` / `TargetMachine` / `IntPredicate` / `Intrinsic` 等 API |
| `novella-hir` | path | 唯一的输入类型来源：`HirProgram` / `HirFunction` / `HirItem` / `HirType` / `IntTy` / `HirInstr` 等 |

### 2.2 feature 标志的含义

- **`default-features = false`**：关闭 inkwell 的默认 feature，避免拉入不需要的 LLVM 组件。
- **`llvm23-1-prefer-dynamic`**：绑定到 LLVM 23.1.x 系列，并且**优先动态链接** LLVM 库（`prefer-dynamic`）。这解释了为什么运行时需要系统里有 LLVM 23.1 的共享库。实测 `llvm-config --version` = **23.1.4**，`clang --version` = **23.1.4**，与 feature 指定的 23.1 系列一致（`docs/compiler-design.md` §8 写的是"inkwell 0.11 对接 LLVM 23.1"）。
- **`target-x86`**：启用 X86 目标后端支持，对应代码里 `Target::initialize_x86(...)`（`crates/novella-codegen/src/lib.rs:87`）。没有这个 feature，`initialize_x86` 不可用。

> **推断（本文作者）**：`prefer-dynamic` 选择动态链接是为了减小最终 `novella` 二进制体积、避免把整个 LLVM 静态链进 CLI；代价是分发时需要目标机器装有匹配的 LLVM 运行库。这一点在源码里没有注释，属于从 feature 名的语义推断。

### 2.3 为什么 LLVM 绑定只出现在 codegen

`crates/novella-hir/src/lib.rs:3-13` 的设计约束明确写着：

> HIR 只依赖 `novella-core` + `novella-ast` + `novella-sema`，**不出现 inkwell / LLVM 类型**：LLVM 版本绑定留在 codegen 一处，前端不随 LLVM 版本浮动。

这条约束的直接后果：

- HIR 里的类型是**自描述的**（`HirType::Int(IntTy)`、`HirType::Pointer(...)`），codegen 负责把它们翻译成 LLVM 类型；
- LLVM 升级（比如 23.1 → 24）只需要改 codegen 的 `Cargo.toml` feature 与少量 inkwell API 调用，前端零改动；
- `novella-sema` / `novella-hir` 可以被不依赖 LLVM 的工具（LSP、JSON 导出、HIR 解释器）复用。

这就是"边界清晰"的实际价值。

### 2.4 workspace 层面的集成

在根 `Cargo.toml` 中，`novella-codegen` 被登记进 `[workspace.dependencies]`：

```toml
novella-codegen = { path = "crates/novella-codegen" }
```

但 CLI crate 是直接 `use novella_codegen::emit_object`（`crates/novella-cli/src/main.rs:78`），走的是 workspace path 依赖。注意 `AGENTS.md` NOTES 提到"novella-lsp 与 novella-cli 同为二进制 crate，未登记进 workspace.dependencies"——codegen 是库 crate，所以登记了。

`[profile.release]` 开启 `lto = true`，意味着 release 构建时 codegen + inkwell + LLVM 会参与全量链接优化，调试构建慢属预期（`AGENTS.md` NOTES）。

---

## 3. 公共 API 全景：CodegenError 与 emit_object

### 3.1 CodegenError

```rust
/// 目标代码生成失败，可包含出错模块与函数符号。
#[derive(Debug)]
pub struct CodegenError(String);

impl std::fmt::Display for CodegenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "代码生成失败：{}", self.0)
    }
}

impl std::error::Error for CodegenError {}

impl From<inkwell::builder::BuilderError> for CodegenError {
    fn from(error: inkwell::builder::BuilderError) -> Self {
        Self(error.to_string())
    }
}
```

（`crates/novella-codegen/src/lib.rs:19-35`）

几个设计点：

1. **单字段 newtype**：内部只有一个 `String`，没有结构化的错误种类枚举。这与 HIR 的 `BuildError`（有 `BuildErrorKind` + 位置）形成对比——codegen 的错误更"终端"，只需要给人看的消息，不需要程序再分类处理。
2. **Display 前缀固定为"代码生成失败："**：所有错误经过 CLI 的 `render_diagnostic` 渲染时会带上这个前缀（CLI 把 `Box<dyn Error>` 的 `to_string()` 包成 `Diagnostic::error`，见 `crates/novella-cli/src/main.rs:21-26`）。
3. **`From<BuilderError>`**：inkwell 的 builder 操作（`build_store` / `build_call` / `build_return` 等）返回 `Result<_, BuilderError>`，这个 `From` 让 `?` 能自动把它们提升为 `CodegenError`。注意它只覆盖 `BuilderError`；`Target::from_triple`、`module.verify`、`write_to_memory_buffer` 的错误是 `String` 或 LLVM 错误类型，需要手动 `.map_err(|error| CodegenError(error.to_string()))`。
4. **内部元组字段是私有的**：外部无法构造 `CodegenError`（所有构造都在 crate 内），只能通过 `Display` 读取消息。`Result<T>` 别名定义为 `type Result<Value> = std::result::Result<Value, CodegenError>`（`:17`）。

### 3.2 emit_object 签名与契约

```rust
/// 验证 LLVM 模块并发射 ELF 目标文件字节。
pub fn emit_object(program: &HirProgram) -> Result<Vec<u8>> {
```

（`crates/novella-codegen/src/lib.rs:84-85`）

- **输入**：`&HirProgram`——一份完整的、已经通过 `build_hir` 降级且入口检查的 HIR 程序。
- **输出**：`Vec<u8>`——一个完整的 ELF 目标文件（`.o`）的字节内容。调用方（CLI）负责把它写到磁盘或交给链接器。
- **失败**：任何不支持的构造、类型不匹配、重复符号、LLVM 校验失败、目标文件写出失败都返回 `Err(CodegenError)`。

**重要契约**：`emit_object` 不写文件、不链接。它是纯函数式的（除了 LLVM 全局的 `Target::initialize_x86` 初始化）：输入 HIR，输出字节。所有 I/O（写 `.o`、调 `clang` 链接）都在 CLI 层完成（第 21 章详述）。

### 3.3 谁调用 emit_object

CodeGraph 的 blast radius 显示：`emit_object` 在 workspace 内**只有一个调用方**——`crates/novella-cli/src/main.rs:78`：

```rust
Emit::Obj | Emit::Exe => {
    let object = novella_codegen::emit_object(&hir)?;
    write_output(&output, &object, matches!(emit, Emit::Exe))?;
}
```

也就是说，codegen 的唯一消费者是 CLI 的 `compile` 子命令（`--emit obj` 或 `--emit exe`）。`--emit hir` 只做 HIR JSON 导出，不进 codegen；`--emit ast` 更早就返回了。

---

## 4. 入口定位：entry()

### 4.1 源码

```rust
fn entry(program: &HirProgram) -> Result<&HirFunction> {
    let module = program
        .modules
        .first()
        .ok_or_else(|| CodegenError("没有入口模块".into()))?;
    let name = format!("m{}::main", module.id);
    module
        .items
        .iter()
        .find_map(|item| match item {
            HirItem::Function(function)
                if function.id.0 == name
                    && function.signature.params.is_empty()
                    && matches!(
                        function.signature.ret,
                        HirType::Void | HirType::Int(IntTy::I32)
                    ) =>
            {
                Some(function)
            }
            _ => None,
        })
        .ok_or_else(|| {
            CodegenError(format!(
                "{}：入口模块必须定义无参 `main`，返回 int 或 void",
                module.path
            ))
        })
}
```

（`crates/novella-codegen/src/lib.rs:37-65`）

### 4.2 逐条解析

`entry()` 是 `emit_object` 的第一个动作（`:86`），它做三件事：

**第一步：取入口模块**

```rust
let module = program.modules.first()
```

入口模块约定为 `HirProgram::modules` 的**下标 0**。这与 `crates/novella-hir/src/item.rs:15` 的契约一致："入口模块为下标 0，与 `SemaResult.modules` 对齐"，也与 `HirProgram::module(id)` 用 `self.modules.get(id)` 直接索引的约定一致。空程序 → `"没有入口模块"`。

**第二步：构造期望的符号名**

```rust
let name = format!("m{}::main", module.id);
```

注意这里用的是 `module.id` 而不是硬编码 `0`。因为入口模块 id 按约定就是 0，所以实际得到 `m0::main`。这与 HIR 的符号命名规则一致（`crates/novella-hir/src/ty.rs:143`：`HirFunctionId` 是 `模块::名字`，顶层函数用 `m{module}::名称`）。

**第三步：在入口模块的 items 里查找匹配的函数**

条件有三条，全部满足才算入口：

| 条件 | 代码 | 含义 |
|------|------|------|
| 是 `HirItem::Function` | `HirItem::Function(function)` | 必须有函数体；`ExternFunction` 不算 |
| id 精确匹配 | `function.id.0 == name` | 符号名必须是 `m0::main` |
| 无参数 | `function.signature.params.is_empty()` | `main()` 不能有形参 |
| 返回类型合法 | `matches!(ret, Void \| Int(I32))` | 只能是 `void` 或 `i32`（Novella 里的 `int`） |

找不到 → 错误 `"{path}：入口模块必须定义无参 `main`，返回 int 或 void"`，其中 `path` 来自 `HirModule::path`（调用方传入的原始路径，用于诊断回显）。

### 4.3 设计要点与潜在陷阱

1. **只查入口模块（下标 0）**：即使别的模块里有一个合法的 `m1::main`，也不会被当作入口。这与"入口文件是唯一的"这一编译模型一致。
2. **`int` 被归一化为 `IntTy::I32`**：`IntTy::from_name("int")` 返回 `I32`（`crates/novella-hir/src/ty.rs:72`），所以源码里写 `func main() -> int` 会变成 `HirType::Int(IntTy::I32)`，正好命中入口检查。
3. **`entry()` 的返回值在两个地方被用**：
   - 第一遍扫描时判断哪个函数是入口（决定符号名与 linkage，见第 6 章）；
   - 第二遍之后判断是否需要 void-main 包装（见第 15 章）。
4. **入口不匹配是 codegen 阶段报的，不是 sema 阶段**：`docs/compiler-design.md` §9 说 `check <file>` 不要求 `main`——所以一个没有 main 的程序 `check` 能通过，但 `compile` 会在 `entry()` 这里失败。这是"静态检查"与"可生成代码"两个门槛的差异。
5. **潜在陷阱（本文作者观察）**：`entry()` 返回的 `&HirFunction` 借用了 `program`，而 `emit_object` 后续又遍历 `program.modules`。Rust 的借用检查允许这种共享借用并存（都是不可变借用），所以没有生命周期问题。

---

## 5. 两遍扫描架构总览

`emit_object` 的核心是一个**两遍扫描（two-pass）**结构，这是编译器后端处理"前向引用"的经典手法。

### 5.1 为什么需要两遍

HIR 的调用指令 `HirInstr::Call` 携带的是 `HirOperand::Function(id)` 或 `HirOperand::FunctionPtr(id)`——一个**符号 id**，而不是 LLVM 的 `FunctionValue`。函数体发射时（`body::emit`）需要把 id 解析成真实的 `FunctionValue` 才能 `build_call`。如果边发射边登记，那么"调用一个定义在后面的函数"就会失败。

两遍扫描解决这个顺序问题：

```
第一遍（lib.rs:105-164）：遍历所有模块/项，把每个函数的 (HirFunctionId → FunctionValue) 登记到 functions: HashMap
第二遍（lib.rs:165-173）：再次遍历，对每个 HirItem::Function 调 body::emit，此时任意函数符号都已可查
```

### 5.2 流程图

```
emit_object(program)
   │
   ├─ entry(program)?                         【入口定位】
   │
   ├─ Target::initialize_x86 / from_triple / create_target_machine   【目标机】
   │
   ├─ Context::create / create_module("novella") / set_triple / set_data_layout
   │
   ├─╔═ 第一遍：for source in modules, for item in source.items ═╗
   │  ║  HirItem::Function / ExternFunction / Global 分派         ║
   │  ║  → 拒绝 extern / global / receiver / async / generic      ║
   │  ║  → 参数类型映射、函数类型构造                              ║
   │  ║  → 决定符号名 + linkage（入口 int main → External）        ║
   │  ║  → functions.insert(id, (FunctionValue, &signature))      ║
   │  ╚══════════════════════════════════════════════════════════╝
   │
   ├─╔═ 第二遍：for source, for item if Function ══════════════╗
   │  ║  body::emit(context, module, &functions, function)      ║
   │  ║  → 构造 Body、发射入口块、save 形参、发射函数体           ║
   │  ╚══════════════════════════════════════════════════════════╝
   │
   ├─ if entry.ret == Void: 生成 int main() 包装（调用 void 入口，return 0）
   │
   ├─ module.verify()?                        【LLVM IR 校验】
   │
   └─ machine.write_to_memory_buffer(&module, FileType::Object) → Vec<u8>   【发射 ELF】
```

### 5.3 两遍之间的共享状态

两遍共享一个局部变量：

```rust
let mut functions = HashMap::new();
```

它的类型是 `HashMap<HirFunctionId, (FunctionValue<'ctx>, &'hir FunctionSignature)>`——这个类型在 `body.rs` 被提取为别名 `Functions`（`crates/novella-codegen/src/body.rs:17-18`）：

```rust
pub(crate) type Functions<'ctx, 'hir> =
    HashMap<HirFunctionId, (FunctionValue<'ctx>, &'hir FunctionSignature)>;
```

注意别名里带着 `&'hir FunctionSignature`——第二遍查询时不仅拿到 `FunctionValue`，还拿到签名引用，用于校验实参个数与返回类型（`crates/novella-codegen/src/body.rs:236-258`）。

---

## 6. 第一遍：目标机初始化与函数声明登记

### 6.1 目标机初始化

```rust
Target::initialize_x86(&InitializationConfig::default());
let triple = TargetTriple::create("x86_64-unknown-linux-gnu");
let target = Target::from_triple(&triple).map_err(|error| CodegenError(error.to_string()))?;
let machine = target
    .create_target_machine(
        &triple,
        "generic",
        "",
        OptimizationLevel::None,
        RelocMode::PIC,
        CodeModel::Default,
    )
    .ok_or_else(|| CodegenError("无法创建 x86-64 Linux 目标机器".into()))?;
```

（`crates/novella-codegen/src/lib.rs:87-99`）

逐项解释：

| 调用 | 参数 | 含义 |
|------|------|------|
| `Target::initialize_x86` | `InitializationConfig::default()` | 注册 X86 后端。必须在使用 X86 target 前调用一次（进程级） |
| `TargetTriple::create` | `"x86_64-unknown-linux-gnu"` | 目标三元组：x86-64 架构 + Linux OS + GNU ABI。**硬编码**，对应 CLI 层对 `--target-*` 的严格限制（第 21 章） |
| `Target::from_triple` | `&triple` | 根据三元组找已注册的 target |
| `create_target_machine` | `triple, "generic", "", OptimizationLevel::None, RelocMode::PIC, CodeModel::Default` | 创建目标机器：CPU 用通用（generic）、无 CPU 特性字符串、不优化、位置无关代码（PIC）、默认代码模型 |

**为什么 `RelocMode::PIC`**：生成位置无关代码，使得目标文件既可以被链接成可执行文件，也可以被链接成共享库。这与 CLI 用 `clang --target=... object -o a.out` 链接的方式兼容（`crates/novella-cli/src/main.rs:181-187`）。

**为什么 `OptimizationLevel::None`**：codegen 不做优化，优化职责完全交给 LLVM/链接器（如果将来启用）或不做。保持发射逻辑简单可预测。

### 6.2 LLVM 上下文与模块

```rust
let context = Context::create();
let module = context.create_module("novella");
module.set_triple(&triple);
module.set_data_layout(&machine.get_target_data().get_data_layout());
```

（`crates/novella-codegen/src/lib.rs:100-103`）

- `Context` 是 LLVM 的对象工厂与类型/常量池，所有 LLVM 类型都绑定到这个 context（这正是 `value_type<'ctx>(context: &'ctx Context, ...)` 需要显式生命周期的原因）。
- `Module` 名固定为 `"novella"`。
- `set_triple` 让模块记录目标三元组。
- `set_data_layout` 从目标机器取数据布局（指针对齐、整数端序等）写入模块——这对后续 `write_to_memory_buffer` 生成正确的目标文件很重要。

### 6.3 声明登记的循环体

```rust
let mut functions = HashMap::new();
for source in &program.modules {
    for item in &source.items {
        let function = match item {
            HirItem::Function(function) => function,
            HirItem::ExternFunction(function) => {
                return Err(CodegenError(format!(
                    "{}：暂不支持外部函数 `{}`",
                    source.path, function.id
                )));
            }
            HirItem::Global(global) => {
                return Err(CodegenError(format!(
                    "{}：暂不支持全局变量 `{}`",
                    source.path, global.name
                )));
            }
        };
        ...
    }
}
```

（`crates/novella-codegen/src/lib.rs:105-122`）

**三种 item 的分派**：

- `HirItem::Function` → 继续处理；
- `HirItem::ExternFunction` → 立即报错 `"{path}：暂不支持外部函数 `{id}`"`；
- `HirItem::Global` → 立即报错 `"{path}：暂不支持全局变量 `{name}`"`。

注意这是**遇到即返回**（`return Err`），不是收集所有错误。因为 `emit_object` 要么产出完整目标文件，要么什么都不产出。

### 6.4 函数级前置拒绝

```rust
let contextualize = |error: CodegenError| {
    CodegenError(format!("{}：{}：{}", source.path, function.id, error.0))
};
if function.signature.receiver
    || function.signature.is_async
    || function.signature.type_param_count != 0
{
    return Err(contextualize(CodegenError(
        "暂不支持带接收者、异步或泛型函数".into(),
    )));
}
```

（`crates/novella-codegen/src/lib.rs:123-133`）

`contextualize` 是一个闭包，把底层错误包成 `"模块路径：函数符号：原始错误"` 三段式。它被复用到后续多个错误点（参数类型映射、函数类型构造、重复符号），而函数体发射的错误在第二遍用类似格式手动拼接（`:168-170`）。

三类被拒函数：

| 字段 | 含义 | 为什么拒绝 |
|------|------|-----------|
| `signature.receiver` | 实例方法（带隐藏 `self`） | 需要按 ABI 插入接收者参数与布局，未实现 |
| `signature.is_async` | async 函数 | 需要状态机/挂起协议，未实现 |
| `signature.type_param_count != 0` | 泛型函数 | 需要单态化，未实现 |

这与 `docs/hir-handover.md` §3 的"显式拒绝"清单一致。

> **注意**：这里检查的 `receiver` / `is_async` / `type_param_count` 三个字段，在 HIR 降级阶段其实已经大概率被拒（`FunctionSignature::from_view` 在 `crates/novella-hir/src/ty.rs:310-317` 对 `type_param_count != 0` 和 `is_async` 直接返回 `BuildError`）。codegen 这里再查一次属于**防御性二次校验**（本文作者推断），保证即使 HIR 构造有疏漏也不会发射出错误的 ABI。

### 6.5 参数与函数类型构造

```rust
let params = function
    .signature
    .params
    .iter()
    .map(|ty| value_type(&context, ty).map(BasicMetadataTypeEnum::from))
    .collect::<Result<Vec<_>>>()
    .map_err(contextualize)?;
let ty = match &function.signature.ret {
    HirType::Void => context.void_type().fn_type(&params, false),
    ty => value_type(&context, ty)
        .map_err(contextualize)?
        .fn_type(&params, false),
};
```

（`crates/novella-codegen/src/lib.rs:134-146`）

- 每个参数类型经 `value_type` 映射成 `BasicTypeEnum`，再 `.into()` 成 `BasicMetadataTypeEnum`（`fn_type` 需要这个类型）。
- 返回类型：`Void` → `context.void_type().fn_type(...)`；其他 → `value_type(...).fn_type(...)`。
- `fn_type(&params, false)` 的第二个参数 `false` 表示**非可变参数**（不是 varargs）。
- 任何 `value_type` 失败都经 `contextualize` 变成带路径/符号的错误。

### 6.6 符号名与 linkage 的抉择

```rust
let (name, linkage) = if function.id == entry.id && entry.signature.ret != HirType::Void
{
    ("main", Linkage::External)
} else {
    (function.id.0.as_str(), Linkage::Internal)
};
```

（`crates/novella-codegen/src/lib.rs:147-152`）

这是整个后端最关键的 ABI 决策之一：

| 情形 | LLVM 符号名 | Linkage | 效果 |
|------|------------|---------|------|
| 是入口函数 **且** 返回 `int`（非 void） | `"main"` | `External` | 直接作为 C ABI 的 `main`，进程退出码 = 返回值 |
| 其他所有函数（含 void 入口、所有普通函数） | `function.id.0`（如 `m0::foo`） | `Internal` | 目标文件内部私有符号，链接后对外不可见 |

**为什么要区分 void 入口**：C ABI 的 `main` 必须返回 `int`。如果 Novella 的 main 返回 `void`，不能直接命名为 `main`（否则 LLVM 模块会有一个返回 void 的 `main`，链接器/启动代码会出错）。所以 void 入口保持内部符号名，稍后（第 15 章）生成一个真正的 `int main()` 包装它。

**为什么其他函数要 Internal**：避免 Novella 函数名污染全局符号空间、避免跨模块符号冲突。`docs/compiler-design.md` §8 明确："其他 Novella 符号仅在目标文件内部可见"。

> **细节观察**：`function.id.0.as_str()` 作为符号名包含 `::`（如 `m0::foo`）。LLVM 允许任意字符串作符号名（会被引号包裹），所以这不是问题，只是调试符号看起来不寻常。

### 6.7 重复符号检查与登记

```rust
if functions.contains_key(&function.id) {
    return Err(contextualize(CodegenError("重复的 HIR 函数符号".into())));
}
functions.insert(
    function.id.clone(),
    (
        module.add_function(name, ty, Some(linkage)),
        &function.signature,
    ),
);
```

（`crates/novella-codegen/src/lib.rs:153-163`）

- 用 `HirFunctionId`（字符串 newtype，实现了 `Hash + Eq`，见 `crates/novella-hir/src/ty.rs:147`）作 HashMap 键。
- 重复键 → `"{path}：{id}：重复的 HIR 函数符号"`。HIR 构造期应保证 id 唯一（顶层函数用名字、方法/ lambda 用属主/位置键），这里再查一次是防御。
- `module.add_function(name, ty, Some(linkage))` 真正向 LLVM 模块添加函数声明。
- 存入的值是 `(FunctionValue, &signature)` 元组——签名引用用于第二遍校验调用。

---

## 7. 类型映射：value_type()

### 7.1 源码

```rust
fn value_type<'ctx>(context: &'ctx Context, ty: &HirType) -> Result<BasicTypeEnum<'ctx>> {
    match ty {
        HirType::Int(kind) => Ok(match kind {
            IntTy::U8 | IntTy::I8 => context.i8_type(),
            IntTy::U16 | IntTy::I16 => context.i16_type(),
            IntTy::U32 | IntTy::I32 => context.i32_type(),
            IntTy::U64 | IntTy::I64 | IntTy::Usize | IntTy::Isize => context.i64_type(),
        }
        .into()),
        HirType::Boolean => Ok(context.bool_type().into()),
        HirType::Pointer(inner) if matches!(**inner, HirType::Int(_) | HirType::Boolean) => {
            Ok(context.ptr_type(AddressSpace::default()).into())
        }
        _ => Err(CodegenError(format!("暂不支持值类型 {ty:?}"))),
    }
}
```

（`crates/novella-codegen/src/lib.rs:67-82`）

### 7.2 映射表

| HIR 类型 | LLVM 类型 | 说明 |
|----------|-----------|------|
| `Int(U8)` / `Int(I8)` | `i8` | 有符号与无符号共享同一 LLVM 整数类型；符号性体现在运算指令选择（`sdiv` vs `udiv`） |
| `Int(U16)` / `Int(I16)` | `i16` | 同上 |
| `Int(U32)` / `Int(I32)` | `i32` | `int` 归一化为 `I32` |
| `Int(U64)` / `Int(I64)` / `Int(Usize)` / `Int(Isize)` | `i64` | x86-64 下 usize/isize 为 64 位 |
| `Boolean` | `i1`（`context.bool_type()`） | LLVM 里 bool 就是 1 位整数 |
| `Pointer(inner)`，其中 `inner` 是 `Int(_)` 或 `Boolean` | `ptr`（默认地址空间） | 只支持指向标量整数的指针；被指类型信息在映射时被丢弃（LLVM 23 的 opaque pointer） |
| **其他所有** `HirType` | — | `Err("暂不支持值类型 {ty:?}")` |

### 7.3 关键权衡分析

**（1）有符号/无符号共享 LLVM 类型**

LLVM 的整数类型不带符号性，符号语义由**指令**决定（如 `sdiv`/`udiv`、`slt`/`ult`）。所以 `value_type` 只需按位宽映射，符号性由 `IntTy::is_signed()` 在运算时判断（第 12 章）。这是 LLVM 的标准做法。

**（2）指针映射丢弃被指类型**

`HirType::Pointer(inner)` 的 `inner` 只用于**判断是否允许**（guard），实际映射成 LLVM 的 `ptr`（opaque pointer，LLVM 15+ 的默认）。这意味着：

- `Pointer(Int(I32))` 和 `Pointer(Boolean)` 都映射成同一个 `ptr` 类型；
- 类型检查在 HIR 层（`operand()` 里比较 `HirType` 的 `actual != ty`）完成，LLVM 层只看到裸指针。

这与 `docs/hir-handover.md` 里"Isize/Usize 按 64 位"、"string 与数组布局待定"的定位一致。

**（3）指针只接受整数/布尔被指类型**

```rust
HirType::Pointer(inner) if matches!(**inner, HirType::Int(_) | HirType::Boolean)
```

`Pointer(Pointer(...))`、`Pointer(Struct(...))`、`Pointer(Array(...))` 等一律落到 `_` 分支报错。这是"这个后端只支持标量取址"的直接体现。

**（4）`value_type` 被三个地方复用**

- `lib.rs` 第一遍：参数类型、返回类型映射（`:134-146`）；
- `body.rs`：`allocate`（`:92`）、`operand`（`:122/${...}`，实际是 `:125`/`:135`）、`save`（`:98`）、`LocalAddress`/`StoreIndirect`/`Deref`（`:172`/`:178`/`:190`）等；
- `operators.rs`：`cast`（`:42`）的转型目标类型。

它是整个 crate 的"类型闸门"——任何新增类型支持，第一站就是改这里。

**（5）`&'ctx Context` 生命周期**

返回的 `BasicTypeEnum<'ctx>` 借用了 context，所以 `value_type` 的生命周期参数 `'ctx` 把输入 context 与输出类型绑定。这保证了类型不会比 context 活得更久（LLVM 的类型归属 context）。

---

## 8. 函数体发射入口：body::emit()

### 8.1 源码

```rust
pub(crate) fn emit<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    functions: &Functions<'ctx, '_>,
    hir: &HirFunction,
) -> Result<()> {
    let function = functions[&hir.id].0;
    let entry = context.append_basic_block(function, "entry");
    let mut body = Body {
        context,
        module,
        builder: context.create_builder(),
        function,
        functions,
        ret: &hir.signature.ret,
        entry,
        slots: HashMap::new(),
        loops: Vec::new(),
    };
    body.builder.position_at_end(entry);
    if function.count_params() as usize != hir.params.len() {
        return Err(CodegenError("HIR 形参与函数签名不一致".into()));
    }
    for (param, value) in hir.params.iter().zip(function.get_param_iter()) {
        body.save(param.id, &param.ty, value)?;
    }
    if body.block(&hir.body)? {
        if *body.ret != HirType::Void {
            return Err(CodegenError("有返回值的函数存在未返回的路径".into()));
        }
        body.builder.build_return(None)?;
    }
    Ok(())
}
```

（`crates/novella-codegen/src/body.rs:39-72`）

### 8.2 执行步骤

1. **取 LLVM 函数对象**：`functions[&hir.id].0`——第一遍登记的 `FunctionValue`。用 `[]` 索引，若不存在会 panic（但第一遍已保证所有函数都登记）。
2. **创建入口基本块**：`append_basic_block(function, "entry")`。
3. **构造 `Body` 状态机**：填入 context、module、新 builder、function、functions 引用、返回类型、入口块、空的 slots/loops。
4. **定位 builder 到入口块末尾**：`position_at_end(entry)`。
5. **形参一致性校验**：`count_params() == hir.params.len()`，不等 → `"HIR 形参与函数签名不一致"`。这是防御性校验——正常情况下 LLVM 函数类型由 `signature.params` 构造，必然相等。
6. **形参落地**：`zip(function.get_param_iter())` 把每个 LLVM 参数值通过 `body.save(param.id, &param.ty, value)` 存入一个新分配的 `alloca` 槽位。这一步非常关键：**形参不是直接用 `FunctionValue` 的参数值，而是先 store 到槽位**。
   - 为什么？因为 HIR 里形参是 `LocalSlot`（有 `LocalId`），后续所有对形参的读写都走 `Local(id)` 操作数（→ `build_load` from 槽位）。把参数统一放进槽位，让"参数"和"局部变量"在 codegen 眼里完全一致。
   - 也支持对形参再赋值（`Store` 指令写槽位）。
7. **发射函数体**：`body.block(&hir.body)?` 返回 `bool` 表示"是否 fall-through"。
8. **漏返回检查**：
   - 如果函数体 fall-through（`true`）且返回类型非 Void → 报错 `"有返回值的函数存在未返回的路径"`。这保证了非 void 函数的所有路径都显式 `return`。
   - 如果 fall-through 且返回类型是 Void → 补一条 `build_return(None)`（即 `ret void`）。
   - 如果函数体不 fall-through（`false`，即最后一条是 return/break/continue），什么都不做——控制流已经在发射时处理。

### 8.3 `emit` 与第二遍的关系

`emit_object` 第二遍（`:165-173`）对每个 `HirItem::Function` 调一次 `body::emit`：

```rust
for source in &program.modules {
    for item in &source.items {
        if let HirItem::Function(function) = item {
            body::emit(&context, &module, &functions, function).map_err(|error| {
                CodegenError(format!("{}：{}：{}", source.path, function.id, error.0))
            })?;
        }
    }
}
```

每个函数的发射错误同样被 `contextualize` 成三段式。注意第二遍只处理 `HirItem::Function`——`ExternFunction` 和 `Global` 在第一遍就已经报错退出了，所以走不到这里。

---

## 9. Body 状态机与槽位管理

### 9.1 Body 结构

```rust
pub(crate) struct Body<'ctx, 'hir> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: &'hir Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    pub(crate) function: FunctionValue<'ctx>,
    functions: &'hir Functions<'ctx, 'hir>,
    ret: &'hir HirType,
    entry: BasicBlock<'ctx>,
    slots: HashMap<LocalId, (PointerValue<'ctx>, HirType)>,
    loops: Vec<LoopTargets<'ctx>>,
}
```

（`crates/novella-codegen/src/body.rs:27-37`）

字段逐项：

| 字段 | 类型 | 作用 |
|------|------|------|
| `context` | `&Context` | LLVM 类型/常量工厂，供 `value_type` 等使用 |
| `module` | `&Module` | 当前 LLVM 模块，`trap_if` 里声明 `llvm.trap` intrinsic 用 |
| `builder` | `Builder` | **当前插入点**的 IR 构建器。`position_at_end` 决定后续指令发射位置 |
| `function` | `FunctionValue` | 当前正在发射的函数，用于 `append_basic_block` |
| `functions` | `&Functions` | 全局函数符号表（第一遍产物），供 `Call` 解析符号 |
| `ret` | `&HirType` | 当前函数返回类型，供 `Return` 校验 |
| `entry` | `BasicBlock` | 入口块，用于把 `alloca` 前置插入 |
| `slots` | `HashMap<LocalId, (PointerValue, HirType)>` | **局部槽位表**：LocalId → (alloca 指针, HIR 类型) |
| `loops` | `Vec<LoopTargets>` | 循环目标栈，支持嵌套循环的 break/continue |

`LoopTargets`：

```rust
struct LoopTargets<'ctx> {
    exit: BasicBlock<'ctx>,
    step: BasicBlock<'ctx>,
    has_break: bool,
    has_continue: bool,
}
```

（`crates/novella-codegen/src/body.rs:20-25`）

- `exit`：`break` 的目标块。
- `step`：`continue` 的目标块（循环增量块）。
- `has_break` / `has_continue`：本循环内是否出现过 break/continue，用于最后判断 `exit`/`step` 块是否可达（第 14 章详述）。

用 `Vec` 当栈，`break`/`continue` 总是作用于 `loops.last_mut()`（最近一层循环），天然支持嵌套。

### 9.2 槽位三件套：slot / allocate / save

**slot（查询）**：

```rust
fn slot(&self, id: LocalId) -> Result<&(PointerValue<'ctx>, HirType)> {
    self.slots
        .get(&id)
        .ok_or_else(|| CodegenError(format!("HIR 槽位 {} 尚未定义", id.0)))
}
```

（`crates/novella-codegen/src/body.rs:75-79`）

查询不存在的槽位 → `"HIR 槽位 {id} 尚未定义"`。这是 HIR 构造正确性校验。

**allocate（分配）**：

```rust
fn allocate(&mut self, id: LocalId, ty: &HirType) -> Result<PointerValue<'ctx>> {
    if self.slots.contains_key(&id) {
        return Err(CodegenError(format!("重复定义 HIR 槽位 {}", id.0)));
    }
    // 槽位只在入口分配；循环里的初始化仍留在原指令位置。
    let allocator = self.context.create_builder();
    if let Some(first) = self.entry.get_first_instruction() {
        allocator.position_before(&first);
    } else {
        allocator.position_at_end(self.entry);
    }
    let pointer = allocator.build_alloca(value_type(self.context, ty)?, "slot")?;
    self.slots.insert(id, (pointer, ty.clone()));
    Ok(pointer)
}
```

（`crates/novella-codegen/src/body.rs:81-95`）

这是**整个后端最精妙的一处设计**。逐点解析：

1. **重复检查**：同一 `LocalId` 只能 allocate 一次，否则 `"重复定义 HIR 槽位"`。
2. **独立的 allocator builder**：`allocate` 不复用 `self.builder`，而是 `context.create_builder()` 新建一个。因为 `self.builder` 的插入点可能已经移到某个循环体块里，而 `alloca` 必须放在入口块——用一个独立 builder 指向入口块，才能把 `alloca` 插到正确位置。
3. **前置插入技巧**：
   ```rust
   if let Some(first) = self.entry.get_first_instruction() {
       allocator.position_before(&first);
   } else {
       allocator.position_at_end(self.entry);
   }
   ```
   每次都在入口块**第一条现有指令之前**插入新的 `alloca`。结果是：随着多次调用，`alloca` 会不断被插到最前面，最终所有 `alloca` 集中在入口块顶部（顺序与 allocate 调用顺序相反，但这不影响正确性）。
4. **源码注释明确意图**：`// 槽位只在入口分配；循环里的初始化仍留在原指令位置。`——即使 `HirInstr::Alloc` 出现在循环体内（比如 for 循环体里的 `let y = ...`），`alloca` 也只分配一次（在入口），只有 `store` 初始化留在循环体的指令位置。这避免了每轮循环都增长栈帧。
5. **alloca 名字固定为 `"slot"`**：LLVM 会自动去重成 `slot`, `slot1`, `slot2`…
6. **存入的元组**：`(pointer, ty.clone())`——同时保存 alloca 指针与 HIR 类型，用于后续类型校验。

**save（写入）**：

```rust
fn save(&mut self, id: LocalId, ty: &HirType, value: BasicValueEnum<'ctx>) -> Result<()> {
    if value.get_type() != value_type(self.context, ty)? {
        return Err(CodegenError(format!("HIR 槽位 {} 的结果类型不匹配", id.0)));
    }
    let pointer = self.allocate(id, ty)?;
    self.builder.build_store(pointer, value)?;
    Ok(())
}
```

（`crates/novella-codegen/src/body.rs:97-104`）

1. **类型校验**：`value.get_type() != value_type(ty)` → 报错。这保证 HIR 声称的类型与实际 LLVM 值类型一致。
2. **allocate + store**：先分配槽位，再用**当前** `self.builder`（位于实际发射位置）写入。注意 `alloca` 在入口、`store` 在当前位置——这正是"声明前置、初始化就地"的实现。

> **注意一个微妙之处**：`save` 总是调用 `allocate`，而 `allocate` 对重复 id 报错。所以 `save` 只能用于**首次写入**一个槽位。后续写入必须走 `HirInstr::Store`（直接 `build_store` 到已有槽位，见第 11 章）。这就是为什么 HIR 里"首次赋值"是 `Alloc`/形参 save，而"重新赋值"是 `Store`。

### 9.3 槽位机制的完整数据流

```
HirInstr::Alloc { id, ty, init }      ─┐
形参 param.id / param.ty              ─┼─▶ allocate(id, ty) ─▶ [entry 块顶部] alloca slot
HirInstr::Load { dst, src }           ─┘                            │
                                                                    ▼
                                                      save(id, ty, value) ─▶ [当前位置] store
                                                                    │
                                                                    ▼
                                              slots.insert(id, (ptr, ty))
                                                                    │
                     ┌──────────────────────────────────────────────┘
                     ▼
HirOperand::Local(id) ──▶ slot(id) ──▶ build_load(ptr) ──▶ 值
```

理解这套机制后，`operand()` 里 `Local(id)` 的处理（第 10 章）就顺理成章了：读一个局部变量 = 从 `slots[id]` 取指针 + `build_load`。

---

## 10. 操作数求值：operand()

### 10.1 源码

```rust
pub(crate) fn operand(
    &self,
    operand: &HirOperand,
    ty: &HirType,
) -> Result<BasicValueEnum<'ctx>> {
    match (operand, ty) {
        (HirOperand::Local(id), _) => {
            let (pointer, actual) = self.slot(*id)?;
            if actual != ty {
                return Err(CodegenError(format!(
                    "HIR 槽位 {} 类型不匹配：{actual:?} / {ty:?}",
                    id.0
                )));
            }
            Ok(self
                .builder
                .build_load(value_type(self.context, ty)?, *pointer, "load")?)
        }
        (HirOperand::IntConst(value), HirType::Int(_)) => Ok(value_type(self.context, ty)?
            .into_int_type()
            .const_int(*value as u64, false)
            .into()),
        (HirOperand::BoolConst(value), HirType::Boolean) => Ok(self
            .context
            .bool_type()
            .const_int(u64::from(*value), false)
            .into()),
        (HirOperand::Null, HirType::Pointer(_)) => Ok(value_type(self.context, ty)?
            .into_pointer_type()
            .const_null()
            .into()),
        _ => Err(CodegenError(format!(
            "暂不支持该 HIR 操作数或类型不匹配：{operand:?} / {ty:?}"
        ))),
    }
}
```

（`crates/novella-codegen/src/body.rs:106-141`）

### 10.2 支持的四种操作数

`operand(operand, ty)` 的语义是：把 HIR 操作数 `operand` 求值成 LLVM 值，并断言其结果类型应为 `ty`（`ty` 是由上下文——指令、签名、槽位——提供的期望类型）。

| 操作数 | 期望类型 | LLVM 产出 | 错误处理 |
|--------|----------|-----------|----------|
| `Local(id)` | 任意 | `build_load(value_type(ty), pointer, "load")` | 槽位不存在 → "尚未定义"；`actual != ty` → "槽位类型不匹配" |
| `IntConst(value)` | `Int(_)` | `const_int(*value as u64, false)` | 其他类型组合 → 通配错误 |
| `BoolConst(value)` | `Boolean` | `bool_type().const_int(0/1, false)` | 同上 |
| `Null` | `Pointer(_)` | `ptr.const_null()` | 同上 |

### 10.3 深入细节

**（1）`Local` 的类型双重校验**

```rust
let (pointer, actual) = self.slot(*id)?;
if actual != ty { ... 类型不匹配 ... }
build_load(value_type(self.context, ty)?, *pointer, "load")
```

- 第一层：`slot` 查存在性。
- 第二层：比较槽位创建时记录的 `actual: HirType` 与期望 `ty`。这是 HIR 层的类型一致性校验（比 LLVM 的裸 `ptr` 更强）。
- 第三层：`value_type(ty)` 生成加载类型（因为 LLVM opaque pointer 不携带类型，`build_load` 必须显式给出类型）。

**（2）`IntConst` 的 i128 → u64 转换**

```rust
const_int(*value as u64, false)
```

HIR 的 `IntConst` 是 `i128`（`crates/novella-hir/src/body.rs:36`），目的是保留 u64 最大值（如 `18446744073709551615`，见 `docs/hir-handover.md` 的 2026-11-15 补全同步）。codegen 用 `as u64` 转换：

- 对非负且 ≤ u64::MAX 的值，这是精确的；
- `const_int(u64, false)` 的 `false` 表示不做符号扩展（值已经是目标位宽的字面位模式）。

LLVM 的 `const_int` 只接受 u64，所以 i128 → u64 是必要的。**潜在限制（本文作者观察）**：如果 HIR 里出现超过 u64::MAX 的 i128 常量（比如 u128 值），这里会静默截断。但当前 HIR 类型系统里最大的整数是 u64/i64（`IntTy` 没有 u128），所以实际不会触发。

**（3）常量只支持整数、布尔、null**

`HirOperand` 还有 `UntypedInt` / `FloatConst` / `UntypedFloat` / `StringConst` / `Function` / `Unit` / `FunctionPtr` / `WithReceiver` 等变体（`crates/novella-hir/src/body.rs:32-70`），codegen 的 `operand()` **都不支持**，落到通配错误。这意味着：

- 浮点常量 → 不支持（后端无浮点）；
- 字符串常量 → 不支持；
- 函数引用 `Function(id)` / `FunctionPtr(id)` → 不由 `operand()` 处理，而是在 `HirInstr::Call` 里**单独解析**（第 11.9 节）；
- `Unit` / `WithReceiver` → 不支持。

**（4）`operand` 是只读的 `&self`**

它不修改 `Body` 状态（只 `build_load`，用当前 builder 插入点）。所有写操作在 `instruction()` / `save` 里。

### 10.4 一个"用错期望类型"的例子

假设 HIR 里有一条 `HirInstr::Store { id: x, value: IntConst(1) }`，而槽位 `x` 的类型是 `Boolean`。`instruction()` 处理 `Store` 时（`crates/novella-codegen/src/body.rs:160-164`）：

```rust
let (pointer, ty) = self.slot(*id)?;   // ty = Boolean
self.builder.build_store(*pointer, self.operand(value, ty)?)?;
```

`operand(IntConst(1), Boolean)` → 不匹配 `(IntConst, Int(_))` 也不匹配 `(BoolConst, Boolean)` → 落到 `_` → `"暂不支持该 HIR 操作数或类型不匹配：IntConst(1) / Boolean"`。这类错误是 HIR 构造 bug 的信号（类型推导出了差错）。

---

## 11. 指令发射：instruction() 逐条解析

`instruction()`（`crates/novella-codegen/src/body.rs:143-290`）是 codegen 的"主分派器"，对每条 `HirInstr` 做模式匹配并发射 LLVM 指令。返回 `bool`：**`true` = 控制流继续往下；`false` = 控制流已转移（return）。**

### 11.1 概览表

| HirInstr | 发射行为 | 支持? |
|----------|----------|-------|
| `Alloc { id, ty, init }` | 可选求值 init → allocate → 可选 store | ✅ |
| `Load { dst, src }` | 读 src 槽位 → save 到 dst | ✅ |
| `Store { id, value }` | operand 求值 → build_store 到已有槽位 | ✅ |
| `LocalAddress { local, ty, dst }` | 取槽位指针 → save 成 Pointer 槽位 | ✅ |
| `StoreIndirect { address, value, ty }` | 解析指针 → build_store | ✅ |
| `Unary { Deref, ... }` | 解析指针 → build_load | ✅ |
| `Unary { 其他 op }` | 调 `operators::unary` | ✅（部分 op） |
| `Binary { ... }` | 调 `operators::binary` | ✅（部分 op） |
| `Cast { ... }` | 调 `operators::cast` | ✅（仅整数） |
| `Call { ... }` | 解析符号 → build_call → 可选 save 返回值 | ✅（仅直接调用） |
| `Return(value)` | 校验 → build_return | ✅，返回 `false` |
| `FieldAddress / FieldLoad / FieldStore` | — | ❌ 报错 |
| `Tuple / TupleExtract / Array / ArrayLength` | — | ❌ 报错 |
| `IndexAddress / IndexLoad / IndexStore` | — | ❌ 报错 |
| `TypeTest` | — | ❌ 报错 |

### 11.2 Alloc

```rust
HirInstr::Alloc { id, ty, init } => {
    let value = init
        .as_ref()
        .map(|operand| self.operand(operand, ty))
        .transpose()?;
    let pointer = self.allocate(*id, ty)?;
    if let Some(value) = value {
        self.builder.build_store(pointer, value)?;
    }
}
```

（`crates/novella-codegen/src/body.rs:145-154`）

- 先求值 `init`（如果有），注意**求值发生在 allocate 之前**——`operand` 可能产生 `build_load`，它被插入到当前 builder 位置。
- 再 `allocate`（把 alloca 前置到入口块）。
- 最后在当前位置 `store` 初值。

**求值顺序意义**：如果 `init` 引用了其他局部变量，load 指令在当前位置发射；alloca 在入口。这是正确的"声明前置、初始化就地"。

### 11.3 Load

```rust
HirInstr::Load { dst, src } => {
    let ty = self.slot(*src)?.1.clone();
    let value = self.operand(&HirOperand::Local(*src), &ty)?;
    self.save(*dst, &ty, value)?;
}
```

（`crates/novella-codegen/src/body.rs:155-159`）

- 从 `src` 槽位取类型 → `operand(Local(src), ty)` 读值 → `save(dst, ty, value)` 写入新槽位。
- 相当于"把局部变量快照到另一个局部变量"。HIR 构造期用这条指令保证副作用顺序（比如实参求值前先 load 快照，避免后续调用修改）。

### 11.4 Store

```rust
HirInstr::Store { id, value } => {
    let (pointer, ty) = self.slot(*id)?;
    self.builder
        .build_store(*pointer, self.operand(value, ty)?)?;
}
```

（`crates/novella-codegen/src/body.rs:160-164`）

直接写到**已有**槽位（不 allocate，与 `save` 不同）。类型从槽位记录取，传给 `operand` 做校验。

### 11.5 LocalAddress

```rust
HirInstr::LocalAddress { local, ty, dst } => {
    let (pointer, actual) = self.slot(*local)?;
    if actual != ty {
        return Err(CodegenError("局部变量取址类型不匹配".into()));
    }
    self.save(
        *dst,
        &HirType::Pointer(Box::new(ty.clone())),
        (*pointer).into(),
    )?;
}
```

（`crates/novella-codegen/src/body.rs:165-175`）

- 取局部变量槽位的 alloca 指针本身，作为一个**值**存到 `dst` 槽位（类型 `Pointer(ty)`）。
- `save` 会校验 `value.get_type()`（这里是把 `PointerValue` 转 `BasicValueEnum`）等于 `value_type(Pointer(ty))`（= `ptr`），通过。
- 这就是 Novella 的 `&x`（取址）在 HIR 里的实现。对应 `HirUnaryOp::AddressOf` 其实在 HIR 构造期被降级成了 `LocalAddress` 指令（codegen 的 `unary()` 里**没有** `AddressOf` 分支，见第 12 章）。

### 11.6 StoreIndirect

```rust
HirInstr::StoreIndirect { address, value, ty } => {
    let address = self
        .operand(address, &HirType::Pointer(Box::new(ty.clone())))?
        .into_pointer_value();
    self.builder
        .build_store(address, self.operand(value, ty)?)?;
}
```

（`crates/novella-codegen/src/body.rs:176-182`）

- `address` 操作数按 `Pointer(ty)` 求值 → `into_pointer_value()`。
- 在当前位置 `build_store(address, value)`。
- 对应 `*p = value`（解引用写）。求值顺序由 HIR 保证（address 与 value 都已求好值）。

### 11.7 Unary（含 Deref 特判）

```rust
HirInstr::Unary {
    op: HirUnaryOp::Deref,
    operand,
    dst,
    dst_ty,
} => {
    let address = self
        .operand(operand, &HirType::Pointer(Box::new(dst_ty.clone())))?
        .into_pointer_value();
    let value =
        self.builder
            .build_load(value_type(self.context, dst_ty)?, address, "deref")?;
    self.save(*dst, dst_ty, value)?;
}
HirInstr::Unary {
    op,
    operand,
    dst,
    dst_ty,
} => {
    let value = self.unary(*op, operand, dst_ty)?;
    self.save(*dst, dst_ty, value)?;
}
```

（`crates/novella-codegen/src/body.rs:183-205`）

- `Deref` 被**特判**（放在第一个 match 臂），因为它需要访问指针地址并 `build_load`，与一元运算的"对值直接运算"不同。
- 其他 op 委托给 `operators::unary`（第 12.1 节）。
- 两者的结果都 `save` 到 `dst` 槽位。

### 11.8 Binary / Cast

```rust
HirInstr::Binary { op, lhs, rhs, lhs_ty, rhs_ty, dst, dst_ty } => {
    let value = self.binary(*op, (lhs, lhs_ty), (rhs, rhs_ty))?;
    self.save(*dst, dst_ty, value)?;
}
HirInstr::Cast { operand, from, to, dst } => {
    let value = self.cast(operand, from, to)?;
    self.save(*dst, to, value)?;
}
```

（`crates/novella-codegen/src/body.rs:206-226`）

两个薄封装，实际逻辑在 `operators.rs`。注意 `Binary` 携带 `lhs_ty`/`rhs_ty`/`dst_ty` 三种类型——这是 HIR 的关键设计（比较运算从 bool 结果反推不出输入宽度，必须显式携带，见 `crates/novella-hir/src/body.rs:209-219`）。

### 11.9 Call（直接调用）

```rust
HirInstr::Call { callee, args, dst, dst_ty } => {
    let (HirOperand::Function(id) | HirOperand::FunctionPtr(id)) = callee else {
        return Err(CodegenError("暂不支持间接调用".into()));
    };
    let &(callee, signature) = self
        .functions
        .get(id)
        .ok_or_else(|| CodegenError(format!("未定义 HIR 函数 `{id}`")))?;
    if args.len() != signature.params.len() {
        return Err(CodegenError(format!("`{id}` 实参个数与签名不一致")));
    }
    let args = args
        .iter()
        .zip(&signature.params)
        .map(|(operand, ty)| self.operand(operand, ty).map(Into::into))
        .collect::<Result<Vec<_>>>()?;
    let call = self.builder.build_call(callee, &args, "call")?;
    if let Some(dst) = dst {
        let value = call
            .try_as_basic_value()
            .basic()
            .filter(|_| dst_ty.as_ref() == Some(&signature.ret))
            .ok_or_else(|| CodegenError(format!("`{id}` 调用的返回类型与 HIR 不一致")))?;
        self.save(*dst, &signature.ret, value)?;
    }
}
```

（`crates/novella-codegen/src/body.rs:227-259`）

逐点：

1. **callee 形态限制**：只接受 `HirOperand::Function(id)` 或 `HirOperand::FunctionPtr(id)`，两者都携带符号 id。其他形态（比如一个存了函数指针的局部变量 `Local(id)`）→ `"暂不支持间接调用"`。
   - 注意：即使 `FunctionPtr(id)` 名字带"Ptr"，codegen 仍把它解析成静态符号——HIR 里函数值在后端尚未实现为真正的间接调用。
2. **符号解析**：`functions.get(id)` 查第一遍登记表，未找到 → `"未定义 HIR 函数 `{id}`"`。
3. **实参个数校验**：`args.len() == signature.params.len()`，不等 → 报错。
4. **实参求值**：`zip(&signature.params)` 逐参数组，用 `operand(operand, ty)` 求值并按期望类型校验，`.map(Into::into)` 转成 `BasicMetadataValueEnum`（`build_call` 需要）。
5. **发起调用**：`build_call(callee, &args, "call")`。
6. **返回值处理**（如果有 `dst`）：
   - `call.try_as_basic_value().basic()` 取基本返回值（`void` 调用会得到 `Either::Right(InstructionValue)`，`.basic()` 返回 `None`）。
   - `.filter(|_| dst_ty.as_ref() == Some(&signature.ret))` 校验 HIR 声称的 `dst_ty` 与实际签名返回类型一致。
   - 取不到或类型不符 → `"调用的返回类型与 HIR 不一致"`。
   - 存到 `dst` 槽位（用 `signature.ret` 作类型，而非 `dst_ty`——因为已经断言二者相等）。

**为什么 `dst_ty` 是 `Option<HirType>`**：`HirInstr::Call` 的定义里 `dst_ty: Option<HirType>`（`crates/novella-hir/src/body.rs:200`），`None` 表示 void 返回（结果丢弃）。codegen 里通过 `if let Some(dst)` 处理——但注意代码只检查了 `dst`，没直接检查 `dst_ty` 的 None；它靠 `.filter(|_| dst_ty.as_ref() == Some(&signature.ret))` 间接保证：如果 `dst` 是 Some 但 `dst_ty` 是 None，filter 失败 → 报错。这是自洽的。

### 11.10 Return

```rust
HirInstr::Return(value) => {
    let value = value
        .as_ref()
        .map(|operand| self.operand(operand, self.ret))
        .transpose()?;
    if value.is_none() != (*self.ret == HirType::Void) {
        return Err(CodegenError("返回值与函数签名不一致".into()));
    }
    self.builder.build_return(
        value
            .as_ref()
            .map(|value| value as &dyn inkwell::values::BasicValue),
    )?;
    return Ok(false);
}
```

（`crates/novella-codegen/src/body.rs:260-274`）

- 用 `self.ret`（函数返回类型）作期望类型求值可选返回值。
- **一致性校验**：`value.is_none() != (ret == Void)`——即"有值 ⟺ 非 void"，否则 `"返回值与函数签名不一致"`。
- `build_return(Some(&value))` 或 `build_return(None)`。
- **返回 `Ok(false)`**：告诉 `block()` 控制流不再 fall-through。这是唯一一个让 `instruction()` 返回 `false` 的分支（除错误路径）。

### 11.11 不支持指令的统一拒绝

```rust
HirInstr::FieldAddress { .. }
| HirInstr::FieldLoad { .. }
| HirInstr::FieldStore { .. }
| HirInstr::Tuple { .. }
| HirInstr::TupleExtract { .. }
| HirInstr::Array { .. }
| HirInstr::ArrayLength { .. }
| HirInstr::IndexAddress { .. }
| HirInstr::IndexLoad { .. }
| HirInstr::IndexStore { .. }
| HirInstr::TypeTest { .. } => {
    return Err(CodegenError(format!("暂不支持 HIR 指令 {instruction:?}")));
}
```

（`crates/novella-codegen/src/body.rs:275-287`）

11 条指令被统一拒绝，错误消息带上完整的指令 Debug 文本（便于调试是哪个具体指令）。这个 `match` 是**穷尽的**——`HirInstr` 的所有 21 个变体都被覆盖（HIR 新增变体时这里会编译失败，符合项目的"穷尽映射"约定）。

---

## 12. 标量运算：operators.rs

`operators.rs` 为 `Body` 实现三个运算方法与一个陷阱辅助方法。

### 12.1 unary()

```rust
pub(crate) fn unary(
    &self,
    op: HirUnaryOp,
    operand: &HirOperand,
    ty: &HirType,
) -> Result<BasicValueEnum<'ctx>> {
    let value = self.operand(operand, ty)?;
    match (op, value) {
        (HirUnaryOp::Identity, _) => Ok(value),
        (HirUnaryOp::Neg, BasicValueEnum::IntValue(value)) => {
            Ok(self.builder.build_int_neg(value, "neg")?.into())
        }
        (HirUnaryOp::Not | HirUnaryOp::BitNot, BasicValueEnum::IntValue(value)) => {
            Ok(self.builder.build_not(value, "not")?.into())
        }
        _ => Err(CodegenError(format!("暂不支持一元运算 {op:?} / {ty:?}"))),
    }
}
```

（`crates/novella-codegen/src/operators.rs:12-29`）

| HIR 一元运算 | LLVM 指令 | 备注 |
|--------------|-----------|------|
| `Identity`（源码 `+x`） | 无（原值返回） | 恒等，直接搬运 |
| `Neg` | `build_int_neg` → `sub 0, x` | 整数取负 |
| `Not` / `BitNot` | `build_not` → `xor x, -1` | 逻辑取反与按位取反在 LLVM 里是同一条按位取反（对 i1 就是逻辑非） |
| `AddressOf` | — | ❌ HIR 构造期已降级为 `LocalAddress` 指令，不会到这里 |
| `Deref` | — | ❌ 在 `instruction()` 里特判处理（第 11.7 节） |

**关键观察**：`Not` 和 `BitNot` 映射到同一条 LLVM `build_not`。对布尔（i1）这是逻辑非，对整数是按位取反——LLVM 里二者位模式相同。这与 HIR 把两个 op 分开（保留语义区别）但后端合并实现（位模式相同）的分层思路一致。

`match` 的第二元是 `BasicValueEnum`，所以如果 `value` 不是 `IntValue`（比如指针），会落到 `_` 报错——即"不支持对指针做 Neg/Not"。

### 12.2 cast()

```rust
pub(crate) fn cast(
    &self,
    operand: &HirOperand,
    from: &HirType,
    to: &HirType,
) -> Result<BasicValueEnum<'ctx>> {
    let value = self.operand(operand, from)?;
    if from == to {
        return Ok(value);
    }
    if let (HirType::Int(kind), HirType::Int(_)) = (from, to) {
        let ty = value_type(self.context, to)?.into_int_type();
        return Ok(self
            .builder
            .build_int_cast_sign_flag(value.into_int_value(), ty, kind.is_signed(), "cast")?
            .into());
    }
    Err(CodegenError(format!("暂不支持转换 {from:?} → {to:?}")))
}
```

（`crates/novella-codegen/src/operators.rs:31-49`）

- `from == to` → 恒等转换，原值返回（HIR 里可能有冗余 cast）。
- **仅支持整数 → 整数**：`build_int_cast_sign_flag(value, target_int_type, is_signed, "cast")`。
  - 第三个参数 `kind.is_signed()` 用的是**源类型** `from` 的 `IntTy` 的符号性——这决定了扩展时是符号扩展（`sext`）还是零扩展（`zext`）。这是正确选择：把 `i8 -1` 转成 `i32` 应符号扩展成 `-1`，把 `u8 255` 转成 `i32` 应零扩展成 `255`。
  - `build_int_cast_sign_flag` 是 inkwell 对 LLVM 的 `sext`/`zext`/`trunc` 的统一封装，LLVM 会根据源/目标位宽自动选择扩展或截断。
- 非整数转换（比如 `Boolean → Int`、`Int → Pointer`）→ 报错。

> **细节观察**：`Boolean → Int` 未支持（`HirType::Boolean` 不匹配 `HirType::Int(kind)`）。如果 Novella 允许 `bool as int`，这条 cast 会在 codegen 报错——这是当前后端的一个限制（本文作者观察）。

### 12.3 binary()

这是最大、最复杂的一个方法（`crates/novella-codegen/src/operators.rs:51-145`）。签名（逐字）：

```rust
pub(crate) fn binary(
    &self,
    op: HirBinaryOp,
    (lhs, lhs_ty): (&HirOperand, &HirType),
    (rhs, rhs_ty): (&HirOperand, &HirType),
) -> Result<BasicValueEnum<'ctx>> {
```

（`crates/novella-codegen/src/operators.rs:51-57`）

**前置校验**：

```rust
use HirBinaryOp::*;
use IntPredicate::*;

let unsupported =
    || CodegenError(format!("暂不支持二元运算 {op:?} / {lhs_ty:?} / {rhs_ty:?}"));
if lhs_ty != rhs_ty {
    return Err(unsupported());
}
let lhs = self.operand(lhs, lhs_ty)?;
let rhs = self.operand(rhs, rhs_ty)?;
```

- `lhs_ty == rhs_ty` 强制要求两侧类型相同（HIR 构造期应保证，如 `i32 + i32`）。
- 求值两侧到 LLVM 值。

**指针比较特判**：

```rust
if let (BasicValueEnum::PointerValue(lhs), BasicValueEnum::PointerValue(rhs)) = (lhs, rhs) {
    let predicate = match op {
        Eq => EQ,
        Ne => NE,
        _ => return Err(unsupported()),
    };
    return Ok(self
        .builder
        .build_int_compare(predicate, lhs, rhs, "ptr.cmp")?
        .into());
}
```

（`:67-77`）

- 两个指针值 → 只支持 `Eq`/`Ne`（用 `build_int_compare EQ/NE`，LLVM 允许对指针做整数谓词比较）。
- 指针的大小比较（`<`、`>`）不支持 → 报错。这与 `docs/hir-handover.md` 提到的"指针 ± 整数、PtrDiff"等标量指针运算的定位一致——但注意**指针算术本身（Add/Sub）在 codegen 里并未支持**（`Add` 分支只处理 IntValue，见下），HIR 生成的 `PtrDiff`/指针加减指令会在这里报错（第 20 章详述）。

**整数运算主体**：

```rust
let (BasicValueEnum::IntValue(lhs), BasicValueEnum::IntValue(rhs)) = (lhs, rhs) else {
    return Err(unsupported());
};
let signed = matches!(lhs_ty, HirType::Int(kind) if kind.is_signed());
let builder = &self.builder;
let result = match op { ... };
```

（`:78-82`）

- 只接受两个 `IntValue`，否则报错。
- `signed` 从 `lhs_ty` 判定（因为已断言 `lhs_ty == rhs_ty`）。

**运算分派表**：

| op | LLVM 指令 | 符号相关 | 陷阱 |
|----|-----------|----------|------|
| `Add` | `build_int_add` ("add") | 无关 | 无（LLVM 整数加法自然回绕） |
| `Sub` | `build_int_sub` ("sub") | 无关 | 无 |
| `Mul` | `build_int_mul` ("mul") | 无关 | 无 |
| `Div` | `build_int_signed_div` / `build_int_unsigned_div` ("div") | ✅ | 有（见 12.3.1） |
| `Rem` | `build_int_signed_rem` / `build_int_unsigned_rem` ("rem") | ✅ | 有（同上） |
| `Shl` | `build_left_shift` ("shl") | 无关 | 有（移位量越界） |
| `Shr` | `build_right_shift(lhs, rhs, signed, "shr")` | ✅（算术/逻辑右移） | 有（移位量越界） |
| `BitAnd` / `LogAnd` | `build_and` ("and") | 无关 | 无 |
| `BitOr` / `LogOr` | `build_or` ("or") | 无关 | 无 |
| `BitXor` | `build_xor` ("xor") | 无关 | 无 |
| `Eq` / `Ne` / `Lt` / `Le` / `Gt` / `Ge` | `build_int_compare(predicate, ...)` ("cmp") | ✅（选择 SLT/ULT 等） | 无 |
| `PtrDiff` | — | — | ❌ 报错 |

（`crates/novella-codegen/src/operators.rs:83-145`）

**关于 `LogAnd` / `LogOr`**：它们映射到按位 `and`/`or`。这**假定短路已经在 HIR 构造期展开成控制流**（`HirBinaryOp` 的 doc 明确："逻辑与（短路已在构造期展开成控制流，这里的操作数已求值）"，见 `crates/novella-hir/src/body.rs:343-345`）。所以 codegen 这里的 `LogAnd`/`LogOr` 处理的是已经求值好的两个 bool 位的按位与/或——正确。

**比较谓词选择**：

```rust
let predicate = match (op, signed) {
    (Eq, _) => EQ,
    (Ne, _) => NE,
    (Lt, true) => SLT,
    (Lt, false) => ULT,
    (Le, true) => SLE,
    (Le, false) => ULE,
    (Gt, true) => SGT,
    (Gt, false) => UGT,
    (Ge, true) => SGE,
    (Ge, false) => UGE,
    _ => return Err(unsupported()),
};
builder.build_int_compare(predicate, lhs, rhs, "cmp")?
```

（`crates/novella-codegen/src/operators.rs:126-141`）

有符号用 `SLT/SLE/SGT/SGE`，无符号用 `ULT/ULE/UGT/UGE`，相等比较与符号无关。

### 12.4 Div/Rem 的陷阱逻辑（12.3.1）

```rust
Div | Rem => {
    let ty = rhs.get_type();
    let zero = builder.build_int_compare(EQ, rhs, ty.const_zero(), "zero")?;
    let invalid = if signed {
        let minimum = ty.const_int(1_u64 << (ty.get_bit_width() - 1), false);
        let min = builder.build_int_compare(EQ, lhs, minimum, "min")?;
        let minus_one =
            builder.build_int_compare(EQ, rhs, ty.const_all_ones(), "minus.one")?;
        let overflow = builder.build_and(min, minus_one, "overflow")?;
        builder.build_or(zero, overflow, "invalid.divisor")?
    } else {
        zero
    };
    self.trap_if(invalid)?;
    match (op, signed) {
        (Div, true) => builder.build_int_signed_div(lhs, rhs, "div")?,
        (Div, false) => builder.build_int_unsigned_div(lhs, rhs, "div")?,
        (Rem, true) => builder.build_int_signed_rem(lhs, rhs, "rem")?,
        _ => builder.build_int_unsigned_rem(lhs, rhs, "rem")?,
    }
}
```

（`crates/novella-codegen/src/operators.rs:87-107`）

**要防两种情况**（`docs/compiler-design.md` §8 明确列出）：

1. **除零**：`rhs == 0` → 陷阱。
2. **有符号最小值 ÷ -1**（含取余）：`lhs == MIN` 且 `rhs == -1` → 陷阱（因为数学结果溢出，LLVM 会触发未定义行为）。

实现细节：

- `ty.const_zero()`：零常量。
- `ty.const_int(1_u64 << (bit_width - 1), false)`：有符号最小值（如 i32 是 `0x80000000`）。
- `ty.const_all_ones()`：全 1 位模式 = -1（二进制补码）。
- `build_and(min, minus_one)` + `build_or(zero, ...)`：合成"除零 或 溢出"的无效条件。
- `trap_if(invalid)` 插入条件陷阱（第 13 章）。
- 无符号只需检查除零（`zero`），不需要溢出检查。

**为什么不直接依赖 LLVM 的 `udiv`/`sdiv` 行为**：LLVM 文档规定 `sdiv MIN, -1` 是 poison/undefined，除数 0 也是 undefined。如果不显式陷阱，编译器可能假设这些情况不会发生而做激进优化，导致不可预测行为。codegen 用显式 `trap_if` 把它们变成确定性陷阱（`llvm.trap` → 运行时 abort/UB 陷阱）。

### 12.5 移位陷阱

```rust
Shl | Shr => {
    let ty = rhs.get_type();
    let invalid = builder.build_int_compare(
        UGE,
        rhs,
        ty.const_int(u64::from(ty.get_bit_width()), false),
        "invalid.shift",
    )?;
    self.trap_if(invalid)?;
    if op == Shl {
        builder.build_left_shift(lhs, rhs, "shl")?
    } else {
        builder.build_right_shift(lhs, rhs, signed, "shr")?
    }
}
```

（`crates/novella-codegen/src/operators.rs:108-122`）

- 检查 `rhs >= bit_width`（用 `UGE` 无符号比较，移位量视为无符号）→ 陷阱。LLVM 里移位量 ≥ 位宽是 undefined。
- `Shr` 的第三个参数 `signed` 决定算术右移（`ashr`，有符号）还是逻辑右移（`lshr`，无符号）。

**注意一个潜在遗漏（本文作者观察）**：`Shl` 对有符号数左移溢出（如 `i32::MAX << 1`）在 LLVM 里也会产生 poison，但代码只检查了移位量越界，没检查左移溢出。这可能是一个未被覆盖的 UB 边界，但当前设计选择了"加减乘回绕、除法/移位量陷阱"的折中策略（`docs/compiler-design.md` §8）。

---

## 13. 算术陷阱：trap_if() 与 llvm.trap

```rust
fn trap_if(&self, invalid: IntValue<'ctx>) -> Result<()> {
    let trap = Intrinsic::find("llvm.trap")
        .and_then(|intrinsic| intrinsic.get_declaration(self.module, &[]))
        .ok_or_else(|| CodegenError("LLVM 未提供 llvm.trap".into()))?;
    let failure = self
        .context
        .append_basic_block(self.function, "arithmetic.trap");
    let next = self
        .context
        .append_basic_block(self.function, "arithmetic.ok");
    self.builder
        .build_conditional_branch(invalid, failure, next)?;
    self.builder.position_at_end(failure);
    self.builder.build_call(trap, &[], "")?;
    self.builder.build_unreachable()?;
    self.builder.position_at_end(next);
    Ok(())
}
```

（`crates/novella-codegen/src/operators.rs:147-164`）

### 13.1 工作机制

`trap_if` 生成一个标准"条件陷阱"控制流模式：

```
           [当前块]
        条件分支 invalid?
         /        \
   true /          \ false
      ▼              ▼
[arithmetic.trap]  [arithmetic.ok]
  call llvm.trap      (继续发射后续指令)
  unreachable              │
                           ▼
                    (builder 停在 ok 块)
```

步骤：

1. **声明 intrinsic**：`Intrinsic::find("llvm.trap")` 找到 LLVM 的 trap intrinsic，`get_declaration(self.module, &[])` 在当前模块里获取/创建它的函数声明。找不到 → `"LLVM 未提供 llvm.trap"`。
2. **建两个基本块**：`arithmetic.trap`（失败路径）和 `arithmetic.ok`（继续路径）。
3. **条件分支**：`build_conditional_branch(invalid, failure, next)`——`invalid` 为真跳 trap 块，否则跳 ok 块。
4. **trap 块**：调 `llvm.trap`（运行时会触发非法指令/abort），紧跟 `build_unreachable()`（告诉 LLVM 这里不会返回）。
5. **重置插入点**：`position_at_end(next)`——让后续指令发射到 ok 块。

### 13.2 设计意义

- **`llvm.trap` 是 LLVM 标准 intrinsic**：编译到目标平台后通常是一条 `ud2`（x86）之类的陷阱指令，运行时立即终止。这是"fail-fast"哲学在**运行期**的延伸——编译期拒绝不了的算术 UB，在运行期变成确定性的陷阱，而非静默的未定义行为。
- **每个除法/移位点都会插入两个基本块**：这是代码体积的代价，换取正确性。因为 `OptimizationLevel::None`，这些块不会被优化合并。
- **`trap_if` 借用 `&self`**：它不修改 Body 状态（只操作 builder 的插入点与模块），但注意它**改变了 builder 的当前位置**（最后停在 ok 块）——这是通过 `Builder` 的内部可变性（inkwell 的 Builder 是 `RefCell` 式设计）实现的，不需要 `&mut self`。

### 13.3 调用点

`trap_if` 只在两处被调用：

- `binary` 的 `Div | Rem` 分支（除零/溢出，`:100`）
- `binary` 的 `Shl | Shr` 分支（移位量越界，`:116`）

加减乘不做陷阱（自然回绕）。

---

## 14. 控制流发射：block / branch / loop_block

`block()` 是控制流发射的主递归函数，它遍历 `HirBlock.stmts`，对每条 `HirStmt` 分派。

### 14.1 block()

```rust
fn block(&mut self, block: &HirBlock) -> Result<bool> {
    for stmt in &block.stmts {
        let falls_through = match stmt {
            HirStmt::Instr(instruction) => self.instruction(instruction)?,
            HirStmt::Block(block) => self.block(block)?,
            HirStmt::If { cond, then_block, else_block } =>
                self.branch(cond, then_block, else_block.as_ref())?,
            HirStmt::Loop { cond, body, step } =>
                self.loop_block(cond.as_ref(), body, step)?,
            HirStmt::Break | HirStmt::Continue => {
                let targets = self
                    .loops
                    .last_mut()
                    .ok_or_else(|| CodegenError("循环外出现跳转".into()))?;
                let target = if matches!(stmt, HirStmt::Break) {
                    targets.has_break = true;
                    targets.exit
                } else {
                    targets.has_continue = true;
                    targets.step
                };
                self.builder.build_unconditional_branch(target)?;
                false
            }
            HirStmt::EnterScope | HirStmt::LeaveScope | HirStmt::Nop => true,
        };
        if !falls_through {
            return Ok(false);
        }
    }
    Ok(true)
}
```

（`crates/novella-codegen/src/body.rs:292-325`）

**返回值语义**：`bool` = "这个块执行完后，控制流是否会继续到下一条语句"。

| HirStmt | 处理 | fall-through |
|---------|------|--------------|
| `Instr(instr)` | `instruction(instr)` | 取决于指令（`Return` → false，其他 → true） |
| `Block(block)` | 递归 `block` | 取决于子块 |
| `If {..}` | `branch` | 取决于两分支是否都 fall-through |
| `Loop {..}` | `loop_block` | 取决于 exit 是否可达 |
| `Break` | 标记 `has_break`，跳 `targets.exit` | false |
| `Continue` | 标记 `has_continue`，跳 `targets.step` | false |
| `EnterScope` / `LeaveScope` / `Nop` | 无操作 | true |

**关键**：一旦某条语句返回 `false`，`block()` 立即 `return Ok(false)`——后续的语句不会发射（它们在不可达代码里）。这是"控制流终止传播"。

**`EnterScope` / `LeaveScope` 为何无操作**：HIR 的注释（`crates/novella-hir/src/body.rs:124-129`）说明这是为将来插入析构/引用计数预留的标记，"当前 codegen 未实现自动内存管理，故两端只记录作用域深度，不做任何发射"。所以 codegen 直接忽略。

**`Break`/`Continue` 的循环外检查**：`loops.last_mut()` 为空 → `"循环外出现跳转"`。HIR 构造期应保证不产生这种指令，这是二次校验。

### 14.2 branch()（if 发射）

```rust
fn branch(
    &mut self,
    cond: &HirCond,
    then_block: &HirBlock,
    else_block: Option<&HirBlock>,
) -> Result<bool> {
    if !self.block(&cond.prelude)? {
        return Ok(false);
    }
    let value = self
        .operand(&cond.value, &HirType::Boolean)?
        .into_int_value();
    if let Some(constant) = value.get_zero_extended_constant() {
        return if constant != 0 {
            self.block(then_block)
        } else if let Some(block) = else_block {
            self.block(block)
        } else {
            Ok(true)
        };
    }
    let then_label = self.context.append_basic_block(self.function, "then");
    let else_label = self.context.append_basic_block(self.function, "else");
    let merge = self.context.append_basic_block(self.function, "merge");
    self.builder
        .build_conditional_branch(value, then_label, else_label)?;

    self.builder.position_at_end(then_label);
    let then_falls = self.block(then_block)?;
    if then_falls {
        self.builder.build_unconditional_branch(merge)?;
    }
    self.builder.position_at_end(else_label);
    let else_falls = if let Some(block) = else_block {
        self.block(block)?
    } else {
        true
    };
    if else_falls {
        self.builder.build_unconditional_branch(merge)?;
    }
    self.builder.position_at_end(merge);
    if !then_falls && !else_falls {
        self.builder.build_unreachable()?;
    }
    Ok(then_falls || else_falls)
}
```

（`crates/novella-codegen/src/body.rs:327-373`）

**步骤解析**：

1. **先发射条件前奏 `cond.prelude`**：如果前奏本身终止了控制流 → 整个 if 终止（`false`）。这体现了 HIR 的"先执行、后判断"建模（`HirCond.prelude`，第 18 章详述）。
2. **求值条件**：`operand(cond.value, Boolean)` → `into_int_value()`（必须是 i1）。
3. **常量折叠**（`:339-347`）：
   ```rust
   if let Some(constant) = value.get_zero_extended_constant() {
       return if constant != 0 { self.block(then_block) }
           else if let Some(block) = else_block { self.block(block) }
           else { Ok(true) };
   }
   ```
   如果条件是编译期常量，**不生成任何分支基本块**——直接发射选中分支的块。这既优化代码，也避免了"常量条件下另一分支不可达"的 LLVM IR 校验问题。
   - 条件为真 → 发射 then 块。
   - 条件为假且有 else → 发射 else 块。
   - 条件为假且无 else → `Ok(true)`（空操作，继续往下）。
4. **非常量条件**：建三个块 `then` / `else` / `merge`，`build_conditional_branch(value, then, else)`。
5. **发射 then 块**：定位到 then_label，`block(then_block)`。若 fall-through → 补 `br merge`。
6. **发射 else 块**：定位到 else_label，`block(else_block)`（无 else 则视为 fall-through）。若 fall-through → 补 `br merge`。
7. **汇聚**：定位到 merge_label。若两个分支都不 fall-through（都 return/break）→ merge 块不可达，插入 `unreachable`（否则 LLVM 会抱怨空块没有 terminator）。
8. **返回** `then_falls || else_falls`——只要有一个分支能到达 merge，if 之后就可达。

**设计要点**：三个基本块的命名（`then`/`else`/`merge`）是固定的，多个 if 会产生多个同名块（LLVM 自动加后缀）。常量折叠是一个重要的正确性保障——否则 `if true { return 1; }` 会让 LLVM 认为 else 路径"漏掉返回"。

### 14.3 loop_block()（循环发射）

这是最复杂的控制流函数（`crates/novella-codegen/src/body.rs:375-449`）：

```rust
fn loop_block(
    &mut self,
    cond: Option<&HirCond>,
    body: &HirBlock,
    step: &HirBlock,
) -> Result<bool> {
    let head = self.context.append_basic_block(self.function, "loop.head");
    let body_label = self.context.append_basic_block(self.function, "loop.body");
    let step_label = self.context.append_basic_block(self.function, "loop.step");
    let exit = self.context.append_basic_block(self.function, "loop.exit");
    self.loops.push(LoopTargets {
        exit,
        step: step_label,
        has_break: false,
        has_continue: false,
    });
    self.builder.build_unconditional_branch(head)?;
    self.builder.position_at_end(head);
    let (head_falls, head_can_exit) = if let Some(cond) = cond {
        if self.block(&cond.prelude)? {
            let value = self
                .operand(&cond.value, &HirType::Boolean)?
                .into_int_value();
            match value.get_zero_extended_constant() {
                Some(0) => {
                    self.builder.build_unconditional_branch(exit)?;
                    (false, true)
                }
                Some(_) => {
                    self.builder.build_unconditional_branch(body_label)?;
                    (true, false)
                }
                None => {
                    self.builder
                        .build_conditional_branch(value, body_label, exit)?;
                    (true, true)
                }
            }
        } else {
            (false, false)
        }
    } else {
        self.builder.build_unconditional_branch(body_label)?;
        (true, false)
    };

    self.builder.position_at_end(body_label);
    let body_falls = if head_falls {
        self.block(body)?
    } else {
        self.builder.build_unreachable()?;
        false
    };
    if body_falls {
        self.builder.build_unconditional_branch(step_label)?;
    }
    self.builder.position_at_end(step_label);
    let targets = self.loops.last_mut().expect("已压入循环目标");
    let step_reachable = body_falls || targets.has_continue;
    targets.step = head;
    if step_reachable {
        if self.block(step)? {
            self.builder.build_unconditional_branch(head)?;
        }
    } else {
        self.builder.build_unreachable()?;
    }
    let targets = self.loops.pop().expect("已压入循环目标");
    self.builder.position_at_end(exit);
    let exit_reachable = head_can_exit || targets.has_break;
    if !exit_reachable {
        self.builder.build_unreachable()?;
    }
    Ok(exit_reachable)
}
```

**四个基本块**：

| 块名 | 作用 |
|------|------|
| `loop.head` | 条件判断（每轮执行 cond.prelude + 条件） |
| `loop.body` | 循环体 |
| `loop.step` | 增量块（C-for 的 `i++`；continue 的目标） |
| `loop.exit` | 循环出口（break 的目标 / 条件为假的出口） |

**状态推入**：`loops.push(LoopTargets { exit, step: step_label, has_break: false, has_continue: false })`。注意此时 `step` 指向 `step_label`——body 里的 `continue` 会跳到这里。

**条件处理（返回 `(head_falls, head_can_exit)`）**：

- 有 `cond`：
  - 先发射 `cond.prelude`；若前奏终止 → `(false, false)`。
  - 求值条件常量：
    - `Some(0)`（恒假）→ 跳 exit，`(false, true)`——body 不可达，但 exit 可达。
    - `Some(_)`（恒真）→ 跳 body，`(true, false)`——body 可达，但条件永远不会自然退出（exit 只能靠 break）。
    - `None`（动态）→ 条件分支 body/exit，`(true, true)`。
- 无 `cond`（`for (;;)` 无限循环）→ 无条件跳 body，`(true, false)`。

**body 发射**：

```rust
let body_falls = if head_falls { self.block(body)? } else { build_unreachable(); false };
if body_falls { build_unconditional_branch(step_label)?; }
```

- 只有 head 可达时才发射 body，否则插入 unreachable。
- body 正常结束（无 break/continue/return）→ 跳 step。

**step 发射（关键细节）**：

```rust
self.builder.position_at_end(step_label);
let targets = self.loops.last_mut().expect("已压入循环目标");
let step_reachable = body_falls || targets.has_continue;
targets.step = head;
if step_reachable {
    if self.block(step)? { build_unconditional_branch(head)?; }
} else {
    build_unreachable()?;
}
```

- `step_reachable = body_falls || has_continue`：step 块有两种进入方式——body 正常结束（body_falls）或遇到 continue（has_continue）。
- **`targets.step = head`**：在发射 step 块之前，把当前循环的 continue 目标从 `step_label` 改为 `head`。含义是：step 块内部如果出现 continue（针对当前循环），它应该直接跳回 head（跳过 step 剩余部分）。同时 step 块正常结束后也会 `br head`（下一行显式发射）。
  - **本文作者分析**：这个重定向实际上是防御性的——HIR 的 step 块通常只含增量指令，不含 continue。但 `HirStmt::Continue` 可能通过嵌套结构出现在 step 块里（取决于 HIR 构造），此时跳 head 是合理语义。这一点源码无注释，属于对代码行为的推断。
- step 块结束后若 fall-through → `br head`（回到条件判断）。

**exit 处理**：

```rust
let targets = self.loops.pop().expect("已压入循环目标");
self.builder.position_at_end(exit);
let exit_reachable = head_can_exit || targets.has_break;
if !exit_reachable { build_unreachable()?; }
Ok(exit_reachable)
```

- `exit_reachable = head_can_exit || has_break`：exit 的两种进入方式——条件自然为假（head_can_exit）或遇到 break（has_break）。
- 不可达 → unreachable（避免空块无 terminator）。

**返回值** `exit_reachable`：循环整体是否 fall-through 到后续语句。无限循环无 break → false（后续语句不可达）。

### 14.4 控制流正确性保障总结

| 情况 | 处理 |
|------|------|
| 常量条件 if | 不生成分支块，直接发射选中分支 |
| 条件前奏终止 | if/loop 整体终止 |
| 两分支都 return | merge 块插入 unreachable |
| 恒真循环无 break | exit 块 unreachable，loop 返回 false |
| 恒假循环 | body 块 unreachable，直接走 exit |
| body 只有 continue | step 可达（靠 has_continue），body→step 的 br 不发射 |
| 循环外 break/continue | 报错 |
| step 块不可达 | step 块 unreachable |

这套"可达性跟踪 + unreachable 填充"保证了生成的 LLVM IR 里每个基本块都有合法的 terminator，能通过 `module.verify()`。

---

## 15. void 返回 main 的包装

```rust
if entry.signature.ret == HirType::Void {
    let main = module.add_function("main", context.i32_type().fn_type(&[], false), None);
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(main, "entry"));
    builder.build_call(functions[&entry.id].0, &[], "")?;
    builder.build_return(Some(&context.i32_type().const_zero()))?;
}
```

（`crates/novella-codegen/src/lib.rs:174-180`）

### 15.1 为什么需要包装

C ABI 规定 `main` 必须返回 `int`。当 Novella 的入口函数返回 `void` 时（第 6.6 节），它被登记为**内部符号**（`function.id.0`，如 `m0::main`），不能叫 `main`。此时需要生成一个真正的 `main`：

```
int main() {
    m0::main();     // 调用 void 的 Novella 入口（内部符号）
    return 0;       // 进程正常退出
}
```

### 15.2 实现细节

- `module.add_function("main", i32 fn_type(&[], false), None)`：添加 `main`，`None` 表示用默认 linkage（LLVM 会给 `main` 外部可见性）。
- 用**独立的 builder**（不复用 Body 的），因为这是在两遍扫描之后、函数体之外新增的包装函数。
- `build_call(functions[&entry.id].0, &[], "")`：调用 void 入口的 `FunctionValue`（从第一遍登记表取）。
- `build_return(Some(&i32 0))`：返回 0。

### 15.3 int 返回 main 的情形

如果入口返回 `Int(I32)`，第一遍已经把它命名为 `"main"` 且 `Linkage::External`（第 6.6 节），这里不生成包装——Novella main 的返回值直接成为进程退出码。

`docs/compiler-design.md` §8 概括为："`main` 返回 `int` 作为进程状态，`void` 通过入口包装返回 `0`"。

### 15.4 一个边界观察

`build_call` 的结果没有检查 `try_as_basic_value`——因为 void 入口返回 void，不需要处理返回值。如果这里被调用的入口意外有返回值（不可能，因为进入这个分支的前提是 `ret == Void`），也不会出错——只是忽略返回值。

---

## 16. 验证与目标文件发射

```rust
module
    .verify()
    .map_err(|error| CodegenError(error.to_string()))?;
// 语言级优化：只在级别允许时跑模块级 pass pipeline，`-O0` 保持逐个指令的忠实映射。
optimize_module(&module, &machine, program.target.opt_level)?;
let buffer = machine
    .write_to_memory_buffer(&module, FileType::Object)
    .map_err(|error| CodegenError(error.to_string()))?;
```

（`crates/novella-codegen/src/lib.rs:491-498`）

### 16.1 module.verify()

LLVM 的模块校验器检查 IR 的结构完整性，包括：

- 每个基本块都有 terminator；
- 指令类型合法（比如 `br` 的条件必须是 i1）；
- 函数签名与调用点一致；
- 支配关系（dominator）正确——所有值在使用前已定义。

校验失败 → `CodegenError(error.to_string())`。这是发射前的最后一道防线：即使 codegen 逻辑有 bug 产生了非法 IR，也在这里被截住，而不是产出坏目标文件。

**这也是为什么第 14 章如此小心地填充 `unreachable`**——空的基本块（无 terminator）会导致 verify 失败。

### 16.2 write_to_memory_buffer

```rust
machine.write_to_memory_buffer(&module, FileType::Object)
```

- `FileType::Object`：生成目标文件（ELF `.o`），而非汇编（`FileType::Assembly`）或 LLVM IR 文本。
- 返回 `MemoryBuffer`，`.as_slice().to_vec()` 拷贝成 `Vec<u8>`。
- 这个函数内部会跑 LLVM 的指令选择、寄存器分配、目标代码发射，最终产出机器码字节。

### 16.3 为什么返回字节而非写文件

如第 3.3 节所述，`emit_object` 是纯计算——它不碰文件系统。这样设计的好处：

- CLI 可以在临时目录里暂存字节，成功后才 rename（原子写入，见第 21 章）；
- `--emit obj` 和 `--emit exe` 共用同一份字节（前者直接写，后者交给 clang 链接）；
- 便于测试（内存中校验字节）。

### 16.4 输出的 ELF 内容

对于 `native-main.nov`（`func main() -> int { return 42; }`），产出的 ELF 目标文件包含：

- 一个外部可见的 `main` 符号（返回 i32）；
- x86-64 机器码：`mov eax, 42; ret` 之类（具体取决于 LLVM 的代码生成）；
- `.text` 段、符号表、重定位信息等标准 ELF 结构。

CLI 用 `clang --target=x86_64-unknown-linux-gnu object -o a.out` 链接成可执行程序（第 21 章）。

### 16.5 优化管道（优化级别）

`verify()` 之后、`write_to_memory_buffer` 之前，`emit_artifact` 按
`program.target.opt_level` 跑一遍模块级 pass 管道（`optimize_module`）：

- `-O0` 直接跳过，HIR 到机器码保持逐指令忠实映射，便于调试；
- `-O1` / `-O2` / `-O3` 分别执行 `default<O1>` / `default<O2>` / `default<O3>`；
- `-Os` 执行 `default<O2>`，并关闭循环展开、循环向量化、SLP 向量化与循环交错。

管道失败一律上抛 `CodegenError("优化管道 … 执行失败")`，不静默降级；优化不改变语言语义
与运行时依赖契约。级别的来源、优先级、各档具体 pass 清单与复现命令见
[目标与自定义目标](./target.md#优化级别)。

---

## 17. 错误处理与上下文链

### 17.1 三段式错误上下文

codegen 的错误消息普遍采用 `"模块路径：函数符号：具体错误"` 的三段式：

```
{path}：{function.id}：{error}
```

有三处构造：

1. **第一遍函数级错误**（`lib.rs:123-125`）——`contextualize` 闭包：
   ```rust
   let contextualize = |error: CodegenError| {
       CodegenError(format!("{}：{}：{}", source.path, function.id, error.0))
   };
   ```
   用于：参数类型映射失败（`:140`）、函数类型构造失败（`:144`）、重复符号（`:154`）、receiver/async/generic 拒绝（`:130`）。

2. **第一遍 item 级错误**（`lib.rs:111/118`）——只有两段（路径 + 具体错误），因为此时还没拿到 function：
   - `"{path}：暂不支持外部函数 `{id}`"`
   - `"{path}：暂不支持全局变量 `{name}`"`

3. **第二遍函数体发射错误**（`lib.rs:168-170`）——手动拼接：
   ```rust
   body::emit(...).map_err(|error| {
       CodegenError(format!("{}：{}：{}", source.path, function.id, error.0))
   })?;
   ```

### 17.2 错误的传播路径

```
inkwell BuilderError / LLVM error String / codegen 内部 CodegenError
        │
        ├─ ? 经 From<BuilderError> 自动转换（builder 操作）
        ├─ .map_err(contextualize) 加上下文（第一遍）
        ├─ .map_err(|e| ...) 加上下文（第二遍）
        ▼
   emit_object → Err(CodegenError)
        │
        ▼
   CLI main.rs:21-26：Box<dyn Error> → Diagnostic::error(to_string()) → render_diagnostic
        │
        ▼
   终端输出："error: 代码生成失败：{path}：{id}：{具体错误}"
```

### 17.3 与 HIR 错误的对比

| 维度 | HIR `BuildError` | codegen `CodegenError` |
|------|------------------|------------------------|
| 结构 | 有 `BuildErrorKind` 枚举 + `module` + `span` | 只有一个 `String` |
| 位置 | 精确到源码 span（行列） | 只有模块路径 + 函数符号，无源码位置 |
| 分类 | 机器可判定的错误种类 | 纯文本，面向人 |
| 消费者 | CLI 的 `lower()` 用它构造带位置的诊断（`crates/novella-cli/src/main.rs:127-151`） | CLI 直接 `?` 传播，用 `render_diagnostic` 渲染无位置诊断 |

**为什么 codegen 错误没有 span**：HIR 指令不携带源码位置（`HirInstr` 没有 span 字段），所以 codegen 报错时无法定位到源码行。这是分层设计的代价——位置信息在 HIR 降级时被丢弃了。CLI 能给出的最好定位就是"哪个模块的哪个函数"。

### 17.4 遇到即返回（fail-fast）

codegen 的所有错误路径都是 `return Err` 或 `?`——**不收集、不累积、不跳过**。第一个不支持的构造立即终止整个 `emit_object`。这与 `AGENTS.md` 的"不可恢复错误 fail-fast 向上抛出，禁止静默吞没"一致，也与 HIR 的"不跳过坏模块、不返回残缺程序"一脉相承。

---

## 18. 求值顺序与可达性：正确性专题

### 18.1 codegen 不做求值顺序决策

这是理解 codegen 正确性的根本前提：**HIR 已经把求值顺序完全显式化**。`crates/novella-hir/src/body.rs:1-15` 的 doc 明确：

> HIR 在构造期就把这些都做掉了：求值顺序已经显式化为语句序列（`HirStmt` 逐条执行）；每个中间值都落在 `LocalId` 上；`HirOperand` 只表示已经求好值的东西。

所以 codegen 的规则很简单：**按 `HirBlock.stmts` 的顺序逐条发射**。没有表达式递归、没有优先级判断、没有副作用重排。

例外是新引入 LLVM 控制流的场景：

- `trap_if` 插入的分支（除零检查先于除法）——这是 codegen 自己引入的控制流，但语义上"先检查后运算"是正确的。
- `branch`/`loop_block` 的 `cond.prelude`——HIR 显式要求"先执行前奏再判断"，codegen 严格遵循。

### 18.2 局部快照机制

HIR 用 `HirInstr::Load { dst, src }` 把局部值快照到新槽位（`crates/novella-hir/src/body.rs:193`），codegen 实现为 `load src → store dst`（第 11.3 节）。这保证了"先读的值不受后续副作用影响"——比如函数调用实参求值时，先 load 每个实参到独立槽位，再发起 call（否则 call 可能修改某个后续实参依赖的变量）。

### 18.3 复杂左值的一次性定位

`docs/hir-handover.md` §2 记录："复合赋值定位一次、在 RHS 前读取旧值；局部读取生成快照，避免后续实参修改先前读值"。codegen 侧通过 `LocalAddress`/`StoreIndirect`/`Deref` 等指令的"地址先求好值再使用"实现——`operand()` 返回的是已求值的 LLVM 值，不存在重复求值。

### 18.4 可达性跟踪的三层 bool

codegen 用 `bool` 返回值在三个层级传播可达性：

1. `instruction()` → `bool`：`false` 表示该指令终止控制流（仅 `Return`）。
2. `block()` → `bool`：`false` 表示块内某处终止了控制流，后续语句不可达。
3. `branch()` / `loop_block()` → `bool`：综合各分支/路径的可达性。

这套机制确保了：

- 不可达代码不被发射（`block()` 遇到 `false` 立即返回）；
- 不可达的基本块被填充 `unreachable`（避免 verify 失败）；
- 非 void 函数漏返回能在 `body::emit` 里被检测（函数体 fall-through 且 ret ≠ Void → 报错）。

### 18.5 cond.prelude 的每轮求值

`HirCond` 的设计（`crates/novella-hir/src/body.rs:293-303`）把条件拆成 `prelude`（前置语句）+ `value`（条件值）。codegen 在 `loop_block` 的 `head` 块里发射 prelude（`:394`），而 `head` 是每轮循环都会经过的块——所以 **prelude 每轮都执行**。这正好符合 HIR 的注释："把前置指令提前发射会造成只求值一次的 bug"（`crates/novella-hir/src/body.rs:295-296`），codegen 的这个结构避免了该 bug。

### 18.6 continue → step，break/return 跳过 step

`docs/compiler-design.md` §8 与 `docs/hir-handover.md` §2 都强调：

> continue 进入 step，break / return 跳过 step。

codegen 实现：

- `HirStmt::Continue` → 跳 `targets.step`（= `step_label`，第 14.3 节）；
- `HirStmt::Break` → 跳 `targets.exit`；
- `HirInstr::Return` → `build_return`（直接返回，不经过 step）。

而 body 正常结束 → `br step_label`。所以 step 在"正常迭代"和"continue"时都执行，在"break/return"时跳过。这与 C 语言 for 循环的增量语义一致。

---

## 19. 与 HIR 的契约：消费什么、拒绝什么

### 19.1 codegen 依赖的 HIR 契约

codegen 对 HIR 做了若干**未显式校验的假设**（依赖 HIR 构造期保证）：

| 假设 | 来源契约 | codegen 是否校验 |
|------|----------|------------------|
| 入口模块是 `modules[0]` | `HirProgram` doc（`item.rs:15`） | 部分（取 first，但不校验 id==0） |
| 入口函数 id 是 `m{module}::main` | HIR 符号命名规则 | 是（`entry()` 比对 id） |
| `HirFunctionId` 唯一 | HIR 构造期分配 | 是（重复符号检查） |
| 形参类型与 LLVM 函数类型一致 | `emit_object` 用同一份 signature 构造 | 是（`count_params` 校验） |
| `LocalId` 在函数内先定义后使用 | HIR 构造期单调分配 | 是（`slot()` 校验） |
| Binary 的 `lhs_ty == rhs_ty` | HIR 类型检查 | 是（`binary()` 校验） |
| 短路/三元已展开为控制流 | HIR 构造期 | 否（信任） |
| 求值顺序已显式化 | HIR 构造期 | 否（信任） |
| 所有类型已定型（无 Unknown/字面量态） | HIR fail-fast | 部分（`value_type` 拒绝未知类型） |

### 19.2 codegen 消费的 HIR 子集

```
HirProgram
  └─ modules[0..n]
       └─ items[]
            └─ HirItem::Function          ✅ 唯一处理的 item
                 ├─ signature             ✅ 用于构造 LLVM 函数类型
                 │    ├─ id               ✅ 符号名 / 查找键
                 │    ├─ params           ✅ 参数类型
                 │    ├─ ret              ✅ 返回类型 + 入口判定
                 │    ├─ receiver         ✅ 拒绝非 false
                 │    ├─ is_async         ✅ 拒绝非 false
                 │    ├─ type_param_count ✅ 拒绝非 0
                 │    └─ is_extern        ❌ 未直接检查（extern 走 HirItem::ExternFunction 分支）
                 ├─ params: Vec<LocalSlot> ✅ 形参落地到槽位
                 └─ body: HirBlock        ✅ 逐条发射
```

### 19.3 codegen 拒绝的 HIR 形态（按层）

**Item 层**：`ExternFunction`、`Global`。

**Signature 层**：`receiver`、`is_async`、`type_param_count != 0`。

**Type 层**（`value_type` 拒绝）：`Void`（作值类型时）、`String`、`Float`、`Array`、`Tuple`、`Function`、`Struct`、`Class`、`Trait`、`Pointer(非标量)`。

**Operand 层**（`operand` 拒绝）：`UntypedInt`、`FloatConst`、`UntypedFloat`、`StringConst`、`Unit`、`WithReceiver`（`Function`/`FunctionPtr` 在 Call 里特判）。

**Instr 层**（`instruction` 拒绝）：11 条（第 11.11 节）。

**Operator 层**：
- `unary` 拒绝：`AddressOf`（不会到这）、`Deref`（特判）、非 IntValue；
- `cast` 拒绝：非 Int→Int；
- `binary` 拒绝：`PtrDiff`、指针大小比较、非 IntValue/PointerValue。

### 19.4 这个契约的哲学

codegen 与 HIR 之间是一份**"能生成就生成，不能就报错"**的诚实契约：

- HIR 可以产出比 codegen 能消化的更丰富的指令（比如 `Tuple`、`Array`）——这不是 bug，而是为将来的后端能力预留；
- codegen 对无法生成的部分**明确拒绝**，而不是静默降级（比如不把 `Tuple` 当成某种假值），这符合 `AGENTS.md` 的"后端边界：无法发射的 HIR 报错，不做静默兜底"；
- 结果是：`build_hir` 成功只意味着"HIR 合法"，`emit_object` 成功才意味着"能生成机器码"。两个门槛分离。

---

## 20. fail-fast 全清单

把 codegen 所有显式报错点汇总（按源码顺序），便于审查与调试：

| # | 错误消息 | 位置 | 触发条件 |
|---|----------|------|----------|
| 1 | `没有入口模块` | `lib.rs:41` | `program.modules` 为空 |
| 2 | `{path}：入口模块必须定义无参 \`main\`，返回 int 或 void` | `lib.rs:60-63` | 入口函数缺失/有参/返回类型非法 |
| 3 | `暂不支持值类型 {ty:?}` | `lib.rs:80` | `value_type` 遇到不支持类型 |
| 4 | `无法创建 x86-64 Linux 目标机器` | `lib.rs:99` | `create_target_machine` 返回 None |
| 5 | `{path}：暂不支持外部函数 \`{id}\`` | `lib.rs:111-114` | 遇到 `ExternFunction` |
| 6 | `{path}：暂不支持全局变量 \`{name}\`` | `lib.rs:117-120` | 遇到 `Global` |
| 7 | `{path}：{id}：暂不支持带接收者、异步或泛型函数` | `lib.rs:130-132` | signature 三标志之一 |
| 8 | `{path}：{id}：重复的 HIR 函数符号` | `lib.rs:154` | 重复 HirFunctionId |
| 9 | `HIR 形参与函数签名不一致` | `body.rs:60` | count_params ≠ params.len() |
| 10 | `HIR 槽位 {} 尚未定义` | `body.rs:78` | slot 查询失败 |
| 11 | `重复定义 HIR 槽位 {}` | `body.rs:83` | allocate 重复 id |
| 12 | `HIR 槽位 {} 的结果类型不匹配` | `body.rs:99` | save 类型校验失败 |
| 13 | `HIR 槽位 {} 类型不匹配：{actual:?} / {ty:?}` | `body.rs:115-118` | operand Local 类型不符 |
| 14 | `暂不支持该 HIR 操作数或类型不匹配：{operand:?} / {ty:?}` | `body.rs:137-139` | operand 通配分支 |
| 15 | `局部变量取址类型不匹配` | `body.rs:168` | LocalAddress 类型不符 |
| 16 | `暂不支持间接调用` | `body.rs:234` | Call 的 callee 非 Function/FunctionPtr |
| 17 | `未定义 HIR 函数 \`{id}\`` | `body.rs:239` | Call 符号未登记 |
| 18 | `\`{id}\` 实参个数与签名不一致` | `body.rs:241` | 实参数量不符 |
| 19 | `\`{id}\` 调用的返回类型与 HIR 不一致` | `body.rs:255` | 返回值类型校验失败 |
| 20 | `返回值与函数签名不一致` | `body.rs:266` | Return 有值性 ≠ 非 void |
| 21 | `暂不支持 HIR 指令 {instruction:?}` | `body.rs:286` | 11 条未实现指令 |
| 22 | `有返回值的函数存在未返回的路径` | `body.rs:67` | 非 void 函数 fall-through |
| 23 | `循环外出现跳转` | `body.rs:307` | break/continue 无循环上下文 |
| 24 | `暂不支持一元运算 {op:?} / {ty:?}` | `operators.rs:27` | unary 通配分支 |
| 25 | `暂不支持转换 {from:?} → {to:?}` | `operators.rs:48` | 非整数 cast |
| 26 | `暂不支持二元运算 {op:?} / {lhs_ty:?} / {rhs_ty:?}` | `operators.rs:61` | binary 通配分支 |
| 27 | `LLVM 未提供 llvm.trap` | `operators.rs:150` | trap intrinsic 缺失 |
| 28 | verify 失败的 LLVM 错误文本 | `lib.rs:183` | IR 结构非法 |

**观察**：绝大多数错误都是"防御性校验"或"不支持构造"——真正可能由用户输入触发的是 #2（没有合法 main）、#5/#6（extern/global）、#7（方法/异步/泛型）、#21（用了聚合/字符串等高级特性）。其余多是 HIR 构造 bug 的信号。

---

## 21. 与 CLI 的集成：emit obj / exe 与链接

### 21.1 CLI 的 Emit 枚举

`crates/novella-cli/src/cli.rs:43-63`：

```rust
#[derive(Clone, Copy, ValueEnum)]
pub enum Emit {
    Exe,
    Obj,
    Ast,
    Hir,
}

impl Emit {
    pub fn default_output(self) -> &'static str {
        match self {
            Self::Exe => "a.out",
            Self::Obj => "output.o",
            Self::Ast => "output.ast.json",
            Self::Hir => "output.hir.json",
        }
    }
    pub fn is_native(self) -> bool {
        matches!(self, Self::Exe | Self::Obj)
    }
}
```

`compile` 子命令支持 `--emit <exe|obj|ast|hir>`，默认 `exe`（`cli.rs:21`）。

### 21.2 run() 中的分支

`crates/novella-cli/src/main.rs:31-84` 的完整流程：

1. 解析命令，决定 `output`（emit + 路径）；
2. `native = emit.is_native()`（只有 exe/obj 触发）；
3. `analyze(&args, module_only, native)` — 语义分析（native 时强制目标平台）；
4. 渲染诊断，有错则返回；
5. `Emit::Ast` → 直接输出 JSON，返回（不进 HIR）；
6. `lower(&result)` → `build_hir`，失败返回；
7. `Emit::Hir` → 输出 HIR JSON；
8. `Emit::Obj | Emit::Exe` → `emit_object(&hir)` → `write_output(...)`。

### 21.3 目标平台限制

`crates/novella-cli/src/main.rs:97-114`：

```rust
if native {
    options.target.pointer_width = novella_sema::PointerWidth::Bits64;
    for (flag, value, expected) in [
        ("target-os", args.target_os.as_deref(), "linux"),
        ("target-arch", args.target_arch.as_deref(), "x86_64"),
        ("target-family", args.target_family.as_deref(), "unix"),
    ] {
        if let Some(value) = value.filter(|value| *value != expected) {
            return Err(format!(
                "原生后端只支持 x86_64-unknown-linux-gnu，不能指定 --{flag}={value}"
            ).into());
        }
    }
    options.cfg.target_os = "linux".into();
    options.cfg.target_arch = "x86_64".into();
    options.cfg.target_family = "unix".into();
}
```

要点：

- native 编译时**强制指针宽度 64 位**、**强制 cfg 为 linux/x86_64/unix**；
- 如果用户显式指定不一致的 `--target-*` 参数 → 报错；
- 这与 codegen 里硬编码的 `TargetTriple::create("x86_64-unknown-linux-gnu")`（`lib.rs:88`）一致——CLI 层与 codegen 层的目标假设必须对上，否则 HIR 的指针宽度与后端不一致。

### 21.4 write_output：暂存 + 链接

`crates/novella-cli/src/main.rs:169-203`：

```rust
fn write_output(path: &Path, bytes: &[u8], link: bool) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let result = (|| -> Result<(), Box<dyn Error>> {
        // 临时目录与输出同文件系统；只在生成或链接成功后替换最终文件。
        let staging = tempfile::tempdir_in(parent)?;
        let artifact = staging.path().join("artifact");
        if link {
            let object = staging.path().join("program.o");
            fs::write(&object, bytes)?;
            let result = Command::new("clang")
                .arg("--target=x86_64-unknown-linux-gnu")
                .arg(&object)
                .arg("-o")
                .arg(&artifact)
                .output()
                .map_err(|error| format!("无法运行链接驱动 clang：{error}"))?;
            if !result.status.success() {
                return Err(format!(
                    "clang 链接失败（{}）：{}",
                    result.status,
                    String::from_utf8_lossy(&result.stderr).trim()
                ).into());
            }
        } else {
            fs::write(&artifact, bytes)?;
        }
        fs::rename(artifact, path)?;
        Ok(())
    })();
    result.map_err(|error| format!("无法生成 '{}'：{error}", path.display()).into())
}
```

**要点**：

1. **原子写入**：在输出目录的临时子目录（`tempfile::tempdir_in(parent)`）里生成，成功后 `fs::rename` 到目标路径。失败不会留下半成品。
2. **`link = emit == Exe`**：
   - `Exe`：把字节写成临时 `program.o`，调 `clang --target=x86_64-unknown-linux-gnu program.o -o artifact` 链接。
   - `Obj`：直接把字节写成 artifact（不链接）。
3. **clang 作为链接驱动**：clang 会调用系统链接器（ld/lld），并把 C 运行时/启动代码链进来，提供真正的 `main` 入口约定。
4. **错误处理**：clang 找不到 → `"无法运行链接驱动 clang"`；clang 非零退出 → `"clang 链接失败（{status}）：{stderr}"`。

**codegen 与 CLI 的职责边界**（再次强调）：

```
codegen:  HirProgram → ELF .o 字节        （纯计算，无 I/O）
CLI:      字节 → 临时文件 → [clang 链接] → 原子 rename 到目标路径   （I/O + 外部工具）
```

---

## 22. 端到端走查：native-main.nov 的每一条 IR

### 22.1 输入

`examples/native-main.nov`：

```novella
func main() -> int {
    return 42;
}
```

### 22.2 降级为 HIR（概念形态）

经过 sema + build_hir，得到大致如下的 HIR（概念重构，非精确 JSON）：

```
HirProgram {
  modules: [
    HirModule {
      id: 0,
      path: "examples/native-main.nov",
      items: [
        HirItem::Function(HirFunction {
          id: HirFunctionId("m0::main"),
          signature: FunctionSignature {
            id: "m0::main",
            params: [],
            ret: HirType::Int(IntTy::I32),
            type_param_count: 0, is_async: false, is_extern: false, receiver: false,
          },
          params: [],
          body: HirBlock { stmts: [
            HirStmt::Instr(HirInstr::Return(Some(HirOperand::IntConst(42))))
          ]},
          visibility: Public,
        })
      ],
      types: [],
    }
  ]
}
```

### 22.3 emit_object 逐步执行

**step 1 — entry()**

- `modules.first()` → 模块 0。
- `name = "m0::main"`。
- 找到 `m0::main`：无参、返回 `Int(I32)` → 匹配。返回该函数的引用。

**step 2 — 目标机 / context / module**

- `initialize_x86`、triple `x86_64-unknown-linux-gnu`、target machine（generic/PIC/O0）。
- `Context`、`Module("novella")`、set_triple、set_data_layout。

**step 3 — 第一遍（登记声明）**

- item 是 `HirItem::Function` → 通过。
- 无 receiver/async/generic → 通过。
- `params = []`（无参数）。
- `ty = i32 ()`（返回 Int(I32) → `value_type(I32)` = i32，`fn_type([], false)`）。
- 符号名决策：`function.id == entry.id`（是）且 `ret != Void`（是，Int(I32)）→ `("main", External)`。
- `functions.insert("m0::main", (add_function("main", i32(), External), &signature))`。

**step 4 — 第二遍（发射函数体）**

- `body::emit`：
  - `function = functions["m0::main"].0`（即 LLVM 的 `main`）。
  - 创建 `entry` 基本块，builder 定位到它。
  - `count_params() == 0 == params.len()` → 通过。
  - 无形参，跳过 save 循环。
  - `block(&hir.body)`：
    - 唯一语句 `Return(Some(IntConst(42)))` → `instruction`：
      - `value = operand(IntConst(42), self.ret=I32)` → `i32 const 42`。
      - `value.is_none() (=false) != (ret==Void) (=false)` → `false != false` = false，不报错。
      - `build_return(Some(42))`。
      - 返回 `Ok(false)`。
    - `block` 收到 false → `return Ok(false)`。
  - `body::emit` 收到 false → 不做漏返回检查，直接 `Ok(())`。

生成的 LLVM IR（概念形态）：

```llvm
; ModuleID = 'novella'
target triple = "x86_64-unknown-linux-gnu"

define external i32 @main() {
entry:
  ret i32 42
}
```

**step 5 — void main 包装**

- `entry.signature.ret == Void`？否（Int(I32)）→ 跳过。

**step 6 — verify + 发射**

- `module.verify()` → 通过。
- `write_to_memory_buffer(Object)` → ELF 字节。

**step 7 — CLI 链接**

- `write_output(a.out, bytes, link=true)`：
  - 临时目录写 `program.o`。
  - `clang --target=x86_64-unknown-linux-gnu program.o -o a.out`。
  - rename 到 `a.out`。

**运行**：`./a.out; echo $?` → `42`。

### 22.4 void main 的走查（对比）

如果源码是 `func main() { ... }`（返回 void）：

- 第一遍：入口函数因 `ret == Void` → 符号名为 `m0::main`、`Linkage::Internal`（不叫 main）。
- 第二遍：`body::emit` 发射 `m0::main`，末尾 `build_return(None)`（或函数体没有 return 时补一个）。
- 包装：`add_function("main", i32())`，`build_call(m0::main)`、`build_return(0)`。

IR：

```llvm
define internal void @m0.main() {
entry:
  ret void
}

define i32 @main() {
entry:
  call void @m0.main()
  ret i32 0
}
```

（符号名里的 `::` 在 LLVM 文本里会显示为带引号或转义，此处为可读性简化。）

---

## 23. 已实现能力矩阵 vs 未实现矩阵

### 23.1 已实现（可生成机器码）

| 能力 | 源码位置 |
|------|----------|
| int 类型全部变体（u8/i8/u16/i16/u32/i32/u64/i64/usize/isize） | `lib.rs:69-75` |
| bool 类型 | `lib.rs:76` |
| 指向整数/布尔的指针 | `lib.rs:77-79` |
| 无参 int main / void main（包装） | `lib.rs:147-152, 174-180` |
| 局部变量 alloca（入口前置）+ store/load | `body.rs:81-104` |
| 形参落地到槽位 | `body.rs:62-64` |
| 取址 `&x`、解引用 `*p`、间接写 `*p = v` | `body.rs:165-195` |
| 整数加减乘（回绕） | `operators.rs:84-86` |
| 整数除/余（含除零与 MIN/-1 陷阱） | `operators.rs:87-107` |
| 移位（含越界陷阱，shr 区分算术/逻辑） | `operators.rs:108-122` |
| 位运算 / 逻辑按位运算 / 比较 | `operators.rs:123-141` |
| 指针相等/不等比较 | `operators.rs:67-77` |
| 一元 Identity/Neg/Not/BitNot | `operators.rs:19-28` |
| 整数间 cast（sext/zext/trunc） | `operators.rs:41-47` |
| 直接函数调用（含跨模块、void/非 void 返回） | `body.rs:227-259` |
| if / if-else（含常量折叠） | `body.rs:327-373` |
| while / for / for-in / 无限循环（统一 Loop） | `body.rs:375-449` |
| break / continue（嵌套循环） | `body.rs:303-317` |
| return | `body.rs:260-274` |
| LLVM IR verify + ELF 目标文件发射 | `lib.rs:181-188` |

### 23.2 未实现（发射即报错）

| 能力 | 报错位置 | 备注 |
|------|----------|------|
| 外部函数 `extern` | `lib.rs:110-115` | HIR 能产出，codegen 拒绝 |
| 全局变量 | `lib.rs:116-121` | 同上 |
| 实例方法（receiver） | `lib.rs:126-133` | 含 struct/class 方法 |
| async 函数 | `lib.rs:126-133` | 需状态机 |
| 泛型函数 | `lib.rs:126-133` | 需单态化 |
| 浮点类型与运算（float/f16/f32/f64） | `lib.rs:80`, `operators.rs:27/48/79` | HIR 支持 |
| 字符串 | `lib.rs:80`, `body.rs:137` | 布局/运行时 ABI 未定 |
| 数组 / 元组 | `lib.rs:80`, `body.rs:275-287` | 11 条指令拒绝 |
| struct / class / trait | `lib.rs:80`, `body.rs:275-287` | 字段访问/虚表未实现 |
| 字段读写（FieldAddress/Load/Store） | `body.rs:275-277` | — |
| 索引读写（IndexAddress/Load/Store） | `body.rs:282-284` | — |
| Tuple / ArrayLength / TypeTest | `body.rs:278-285` | — |
| 间接调用（函数指针变量） | `body.rs:234` | `FunctionPtr(id)` 仍按静态符号处理 |
| 指针算术（指针 ± 整数、PtrDiff） | `operators.rs:78/142` | Add/Sub 只处理 IntValue；PtrDiff 直接报错 |
| 非整数 cast（bool↔int、int↔ptr 等） | `operators.rs:48` | 仅 Int→Int |
| 浮点常量、字符串常量、Unit、WithReceiver | `body.rs:137-139` | operand 拒绝 |

### 23.3 "HIR 有但 codegen 无"的完整清单

这是最容易踩坑的地方。HIR 的 P1/P2 覆盖（`docs/hir-handover.md` §2）包括短路、三元、默认实参、struct 实例调用、match、lambda、解构、for-in、字符串转义等——这些在 **HIR 层**都实现了，但其中相当一部分用到了 codegen 不支持的指令：

- **struct 实例调用** → 产生 `FieldAddress`/`FieldStore`/receiver → codegen 拒绝；
- **数组 / 元组 / 解构** → `Array`/`Tuple`/`TupleExtract`/`Index*` → codegen 拒绝；
- **字符串** → `StringConst` → codegen 拒绝；
- **match** → 若分支产生聚合或 TypeTest → 可能拒绝；
- **lambda（无捕获）** → 降级为独立 `HirFunction`，若其体只用标量 → **可以**生成；
- **默认实参 / 短路 / 三元 / for-in（标量）** → 降级为标量指令 + 控制流 → **可能**可以生成（取决于元素类型）。

**结论**：只有"标量 + 控制流 + 直接调用"形态的程序能完整编译到机器码。`examples/native-main.nov` 正是这个最小集合的代表。

---

## 24. 设计哲学与权衡分析

### 24.1 极小后端，诚实边界

codegen 最突出的设计哲学是**"做得到的做扎实，做不到的明确拒绝"**：

- 不追求覆盖面，而是把标量子集做对（除零陷阱、可达性、求值顺序都有严谨处理）；
- 不支持的东西立即报错，不静默降级——这与 `AGENTS.md` 的"后端边界：无法发射的 HIR 报错，不做静默兜底"完全一致。

这种哲学的价值在于：**编译成功的程序行为是可预测的**。不会出现"编译过了但运行时悄悄用了错误 ABI"的情况。

### 24.2 分层隔离 LLVM 版本

LLVM 绑定被严格限制在 codegen crate（`inkwell 0.11 / llvm23-1`）。前端（sema/hir）完全不感知 LLVM。好处：

- LLVM 升级只动 codegen 的 Cargo feature 与少量 API；
- LSP/JSON 导出/HIR 工具不需要 LLVM 运行时。

### 24.3 两遍扫描：经典但必要

两遍扫描解决前向引用，代价是遍历两次 + 一张符号表 HashMap。对于编译器后端这是标准做法，没有过度设计。

### 24.4 alloca 前置：简单有效的栈管理

用独立 builder 把所有 `alloca` 插到入口块顶部，是一个**用极小代价换取正确性**的技巧：避免了循环内重复分配栈空间，同时让"变量声明"的 LLVM 表达统一。这个模式在工业编译器里也很常见（虽然更激进的后端会用 SSA + 寄存器分配替代 alloca）。

### 24.5 常量折叠作为正确性工具

`branch`/`loop_block` 对常量条件的处理不只是优化——它保证了"恒真 if + 单分支 return"不会让 LLVM verify 失败（否则另一路径会被视为漏返回）。这是"用简单手段解决正确性问题"的又一例。

### 24.6 陷阱而非静默 UB

除零、MIN/-1、移位越界都插入 `llvm.trap`。这体现了"宁可运行时明确失败，也不产生未定义行为"的态度，与整个项目的 fail-fast 文化一致。

### 24.7 纯函数式 emit_object

`emit_object` 返回字节而不写文件/链接，把 I/O 职责交给 CLI。这让：

- 原子写入、链接驱动选择、输出路径逻辑都在一处（CLI）；
- codegen 可独立测试（内存中检查字节）。

### 24.8 权衡的代价

诚实边界也有代价：

- **可用语言子集小**：用户写的大部分 Novella 特性（字符串、数组、struct）暂时无法编译到原生程序；
- **错误信息无源码位置**：HIR 丢弃了 span，codegen 报错只能定位到函数级；
- **无优化**：`OptimizationLevel::None` + 大量 alloca，生成的机器码不是最优（但正确性优先）。

这些都是**有意识的阶段性选择**，而非缺陷——`docs/compiler-design.md` §8 与 `docs/hir-handover.md` 都把它们列为"后续工作"。

---

## 25. 已知限制与技术债

以下是从源码与文档中梳理的、当前 codegen 的局限（部分为本文作者观察，标注来源）：

1. **浮点完全不支持**（源码事实）：`FloatTy` 在 HIR 有定义，但 `value_type`/`operand`/`operators` 都不处理。加浮点需要扩展类型映射、常量求值、运算指令、cast。

2. **指针算术不支持**（源码事实）：HIR 的 `PtrDiff`、指针 ± 整数在 codegen 报错。`binary()` 的 `Add`/`Sub` 只接受 `IntValue`，指针只接受 Eq/Ne。

3. **`cast` 不支持 bool↔int、int↔ptr**（本文作者观察）：只处理 Int→Int。

4. **`IntConst` 的 i128→u64 截断**（本文作者观察）：超过 u64::MAX 的常量会截断，但当前 HIR 类型系统无 u128，实际不触发。

5. **左移溢出未陷阱**（本文作者观察）：只检查移位量越界，未检查有符号左移溢出（LLVM poison 边界）。

6. **`targets.step = head` 的意图无注释**（本文作者观察）：step 块发射前的 continue 目标重定向，源码无注释说明，属于防御性处理，语义需进一步确认。

7. **`entry()` 用 `module.id` 构造名字但取 `modules.first()`**（本文作者观察）：依赖"入口模块 id == 0"的隐含约定。若 HIR 构造违反此约定（first 的 id ≠ 0），会找错符号名。当前 HIR 契约保证 id == 下标，所以安全。

8. **`extern` 函数完全拒绝，无声明产出**（源码事实）：`HirItem::ExternFunction` 直接报错，而非生成 LLVM declaration。这意味着即使只是声明一个外部函数（比如 printf）也不行。

9. **`llvm.trap` 依赖**（源码事实）：每个除法/移位插入两个基本块，代码膨胀；`OptimizationLevel::None` 下不合并。

10. **无源码位置**（设计使然）：codegen 错误无 span，调试体验受限。

11. **单一目标平台**（源码事实）：硬编码 x86_64-unknown-linux-gnu，无 cross-target 能力；CLI 层也强制锁定。

12. **无调试信息**（本文作者观察）：没有生成 DWARF 调试信息（`Builder` 未设置 debug location），生成的可执行文件无法源码级调试。

13. **`optimize` 未暴露**（源码事实）：`OptimizationLevel::None` 硬编码，无 `-O` 选项。

---

## 26. 扩展指南：如何新增一个类型或指令

### 26.1 新增一个值类型（例如 Float）

1. **`value_type()`**（`lib.rs:67-82`）：加 `HirType::Float(kind) => ...` 映射（`context.f32_type()` 等）。
2. **`operand()`**（`body.rs:106-141`）：加 `FloatConst` 的常量构造（`const_float`）。
3. **`unary()` / `binary()` / `cast()`**（`operators.rs`）：加浮点运算（`build_float_neg`、`build_float_add` 等）、浮点谓词（`FloatPredicate`）、浮点 cast。
4. **移除相关 fail-fast**：`operators.rs:27/48/79/61` 的 `unsupported()` 分支会自然放行匹配的情形。
5. **更新文档**：`docs/compiler-design.md` §8、`docs/hir-handover.md`、本文第 23 章。

### 26.2 新增一条 HIR 指令（例如 field 访问）

1. **`instruction()`**（`body.rs:143-290`）：从 11 条拒绝列表（`body.rs:275-287`）中移除对应变体，加具体实现。
2. **类型映射**：若涉及新类型（struct 布局），扩展 `value_type` 与 HIR 类型表示。
3. **布局来源**：字段下标/偏移来自 `HirTypeDef`（`crates/novella-hir/src/item.rs:101-148`），需要建立"名义键 → LLVM struct 类型"的映射（可能要新增一张全局类型表，类似 `functions`）。
4. **`getelementptr`**：用 `build_struct_gep` 实现地址计算，配合 `build_load`/`build_store`。
5. **更新拒绝清单与文档**：第 20、23、25 章。

### 26.3 新增一个控制流构造（例如 switch / match）

1. **`body.rs` 的 `block()`**：加新的 `HirStmt` 分支（需要 HIR 侧先加变体）。
2. **可达性**：维护 `bool` 返回值语义——若新构造有多个出口，参考 `branch()` 的 `then_falls || else_falls`。
3. **基本块 terminator**：确保所有新建块都有 terminator（`br`/`switch`/`unreachable`），否则 verify 失败。
4. **常量折叠**：参考 `branch()` 对常量条件的处理，避免不可达路径导致 verify 失败。

### 26.4 新增一个目标平台

1. **CLI 层**（`main.rs:97-114`）：放宽 `--target-*` 限制。
2. **codegen 层**（`lib.rs:87-99`）：把硬编码 triple 改成参数化，`initialize_x86` 换成对应后端 feature。
3. **HIR 层**：`IntTy::bit_width()` 的 usize/isize 当前硬编码 64 位（`crates/novella-hir/src/ty.rs:87-98`），跨平台需改为从目标数据布局取值（源码注释已注明这一点）。
4. **Cargo feature**：inkwell 需加对应 target feature（如 `target-aarch64`）。

### 26.5 扩展时的不变量检查

任何扩展都必须维持：

- **穷尽 match**：`value_type` / `instruction` / `operand` / 各 operator 的 `match` 覆盖所有变体（HIR 新增变体时编译应失败，而非静默落到 `_` 兜底）——这是项目的明确约定。
- **verify 通过**：新增控制流后必须 `module.verify()` 通过。
- **fail-fast 不退化**：不能为了让某个构造"能通过"而静默生成错误代码。
- **求值顺序不变**：不能重排 HIR 指令。
- **文档同步**：`AGENTS.md` 要求"发现文档过时应同步更新"。

---

## 27. 符号速查表

### 27.1 codegen 内部符号

| 符号 | 文件:行 | 类型/签名 | 作用 |
|------|---------|-----------|------|
| `CodegenError` | `lib.rs:20-29` | `struct(String)` | 唯一错误类型 |
| `Result<T>` | `lib.rs:17` | type alias | `Result<T, CodegenError>` |
| `entry` | `lib.rs:37-65` | `fn(&HirProgram) -> Result<&HirFunction>` | 入口定位 |
| `value_type` | `lib.rs:67-82` | `fn(&Context, &HirType) -> Result<BasicTypeEnum>` | HIR→LLVM 类型映射 |
| `emit_object` | `lib.rs:85-188` | `pub fn(&HirProgram) -> Result<Vec<u8>>` | 唯一公共 API |
| `Functions` | `body.rs:17-18` | type alias | 函数符号表类型 |
| `LoopTargets` | `body.rs:20-25` | `struct` | 循环 break/continue 目标 |
| `Body` | `body.rs:27-37` | `struct` | 函数体发射状态机 |
| `body::emit` | `body.rs:39-72` | `fn(...)` | 单函数发射入口 |
| `Body::slot` | `body.rs:75-79` | `fn` | 槽位查询 |
| `Body::allocate` | `body.rs:81-95` | `fn` | 入口前置 alloca |
| `Body::save` | `body.rs:97-104` | `fn` | 首次写槽位 |
| `Body::operand` | `body.rs:106-141` | `fn` | 操作数求值 |
| `Body::instruction` | `body.rs:143-290` | `fn -> Result<bool>` | 指令分派 |
| `Body::block` | `body.rs:292-325` | `fn -> Result<bool>` | 块发射 |
| `Body::branch` | `body.rs:327-373` | `fn -> Result<bool>` | if 发射 |
| `Body::loop_block` | `body.rs:375-449` | `fn -> Result<bool>` | 循环发射 |
| `Body::unary` | `operators.rs:12-29` | `fn` | 一元运算 |
| `Body::cast` | `operators.rs:31-49` | `fn` | 整数转换 |
| `Body::binary` | `operators.rs:51-145` | `fn` | 二元运算 |
| `Body::trap_if` | `operators.rs:147-164` | `fn` | 条件陷阱 |

### 27.2 关键依赖的 HIR 符号

| 符号 | 位置 | 作用 |
|------|------|------|
| `HirProgram` | `novella-hir/src/item.rs:17-19` | codegen 输入根 |
| `HirModule` | `novella-hir/src/item.rs:30-39` | 模块（id/path/items/types） |
| `HirItem` | `novella-hir/src/item.rs:43-50` | Function/ExternFunction/Global |
| `HirFunction` | `novella-hir/src/item.rs:57-71` | 函数定义 |
| `FunctionSignature` | `novella-hir/src/ty.rs:166-184` | 函数签名 |
| `HirFunctionId` | `novella-hir/src/ty.rs:148` | 稳定符号键 `模块::名字` |
| `HirType` | `novella-hir/src/ty.rs:21-46` | 后端可见类型 |
| `IntTy` | `novella-hir/src/ty.rs:53-64` | 整数种类 |
| `HirBlock` / `HirStmt` / `HirInstr` | `novella-hir/src/body.rs:74/100/136` | 函数体三层结构 |
| `HirOperand` | `novella-hir/src/body.rs:32-70` | 已求值操作数 |
| `HirCond` | `novella-hir/src/body.rs:298-303` | 条件（prelude + value） |
| `LocalId` | `novella-hir/src/body.rs:28` | 局部槽位 id |

---

## 28. 附录：完整源码索引与验证命令

### 28.1 三个源文件的"一页速览"

**`lib.rs`（188 行）**

```
1-4      文档注释 + mod 声明
6-15     imports
17       Result 别名
19-35    CodegenError + Display + Error + From<BuilderError>
37-65    entry()：入口定位
67-82    value_type()：类型映射
84-188   emit_object()：
           85   签名
           86   entry
           87-99  目标机
           100-103 context/module
           105-164 第一遍（声明登记）
           165-173 第二遍（函数体发射）
           174-180 void main 包装
           181-188 verify + 写目标文件
```

**`body.rs`（450 行）**

```
1-15     文档注释 + imports
17-18    Functions 别名
20-25    LoopTargets
27-37    Body 结构
39-72    emit()：单函数发射入口
75-79    slot()
81-95    allocate()
97-104   save()
106-141  operand()
143-290  instruction()：21 条 HirInstr 分派
292-325  block()
327-373  branch()
375-449  loop_block()
```

**`operators.rs`（165 行）**

```
1-9      文档注释 + imports
11-29    unary()
31-49    cast()
51-145   binary()
147-164  trap_if()
```

### 28.2 验证命令

```bash
# 编译整个 workspace（含 codegen）
just check                         # cargo check --workspace --all-targets

# 完整编译并运行最小示例（端到端：sema → HIR → LLVM → ELF → clang 链接 → 执行）
cargo run -p novella -- compile examples/native-main.nov
./a.out                            # 应输出退出码 42
echo $?                            # 42

# 只生成目标文件（不链接）
cargo run -p novella -- compile examples/native-main.nov --emit obj -o output.o
file output.o                      # 应显示 ELF 64-bit x86-64 relocatable
nm output.o                        # 应看到 main 符号（External）

# 导出 HIR（看 codegen 的输入长什么样）
cargo run -p novella -- compile examples/native-main.nov --emit hir -o output.hir.json

# 质量检查
just fmt                           # cargo fmt --all
just lint                          # clippy --workspace --all-targets -D warnings
just test                          # cargo test --workspace

# 工具链版本核对
llvm-config --version              # 23.1.4（与 inkwell llvm23-1 feature 匹配）
clang --version                    # 23.1.4
rtk cargo build -p novella-codegen # 单独构建 codegen（按 AGENTS.md 用 rtk 前缀）
```

### 28.3 与其他文档的交叉引用

- **HIR 数据模型的完整定义**：`docs/hir-deep-dive.md`、`crates/novella-hir/src/{ty,body,item}.rs`
- **HIR 降级契约与拒绝边界**：`docs/hir-handover.md`
- **编译器整体架构与管线**：`docs/compiler-design.md` §1、§7–9
- **语言语义规则**：`docs/design.md`
- **项目约定（注释/命名/公开面/后端边界）**：根目录 `AGENTS.md`

---

## 结语

`novella-codegen` 是一个**小而精**的后端：803 行代码实现了从定型 HIR 到 x86-64 ELF 目标文件的完整通路。它的价值不在于覆盖面广，而在于：

1. **边界诚实**：不支持的构造明确报错，绝不静默兜底；
2. **正确性扎实**：求值顺序、alloca 前置、可达性跟踪、算术陷阱都有严谨处理；
3. **架构清晰**：两遍扫描、类型闸门、Body 状态机、运算分层的职责划分干净；
4. **版本隔离**：LLVM 绑定被锁在单一 crate，前端不受 LLVM 升级影响。

理解它的钥匙是三句话：

- **它不解释表达式**——HIR 已经把一切都算好、排好序了；
- **它把所有 alloca 放入口**——用独立 builder 前置插入，循环不会重复分配栈；
- **它靠 bool 返回值传播可达性**——`false` 意味着控制流已终止，触发 unreachable 收尾。

掌握了这三点，再顺着第 5 章的两遍扫描流程图读三个源文件，整个 codegen 的工作流程、代码架构与设计权衡就尽在掌握了。

---

> **文档声明**：本文基于 2026-11-21 磁盘源码逐行重建，所有代码片段为逐字引用。
> 标注"本文作者观察/推断"的段落是对代码行为的分析，非源码原文结论；
> 涉及未来规划的内容以 `docs/compiler-design.md`、`docs/hir-handover.md` 等权威文档为准。

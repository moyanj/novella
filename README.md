<div align="center">

# Novella

**一门面向系统编程的静态类型语言：语法像 TypeScript，内存模型像 Rust，直接编译成原生机器码。**

[设计文档](docs/design.md) · [快速上手](#快速上手) · [路线图](ROADMAP.md) · [语义覆盖](docs/sema-coverage.md) · [性能对比](PERFORMANCE.md)

</div>

---

## 为什么是 Novella

大多数系统编程语言要么让你彻底接近裸金属，要么把你关在托管运行时里。Novella 想让这两件事发生在**同一门语言**里，但分层明确：

| | 默认（安全路径） | 显式 opt-in（底层路径） |
|---|---|---|
| 类型 | `class`（引用语义，自动引用计数） | `struct`（值语义，逐位拷贝） |
| 内存 | 由语言与运行时管理，看不到指针 | 裸指针 `T*` + `unsafe { }` |
| 典型用途 | 日常业务对象、字符串、集合 | 内核、驱动、寄存器访问、FFI |

日常代码里你写不到一个指针；需要碰内存时，穿指针的每一次访问都必须落在 `unsafe { }` 块里——**不碰内存就不付代价，碰了内存就跑不掉检查**。

## 一段代码

```novella
class Account {
    pub id: i64;
    pub balance: i64;

    pub func __init(self, id: i64, balance: i64) {
        this.id = id;
        this.balance = balance;
    }

    pub func deposit(self, amount: i64) {
        this.balance += amount;
    }
}

struct Point {
    pub x: i32;
    pub y: i32;
}

func distanceSquared(a: Point, b: Point) -> i32 {
    let dx: i32 = a.x - b.x;
    let dy: i32 = a.y - b.y;
    return dx * dx + dy * dy;
}

func main() -> int {
    let account = new Account(1, 100);
    account.deposit(50);
    let line = f"account {account.id}: {account.balance}";
    if (line.charCount() == 0) { return 3; }

    let p = Point{ x: 1, y: 2 };
    let q = Point{ x: 4, y: 6 };
    if (distanceSquared(p, q) != 25) { return 1; }

    let raw: i32* = &p.x;          // 取址是安全操作
    unsafe { *raw = 10; }          // 穿指针写：必须显式 unsafe
    if (p.x != 10) { return 2; }

    return 0;
}
```

> 上文这段代码已用当前工具链实测：`novella compile` 生成原生程序并以 `0` 退出。
> 语言全貌见 [`examples/novella-syntax-demo.nov`](examples/novella-syntax-demo.nov)，最小可运行程序见 [`examples/native-main.nov`](examples/native-main.nov)。

## 语言速览

**现代语法，熟悉的表亲。** `let` / `const`、对象字面量、`match` / `case`、lambda、`??` 空值合并、`f"..."` 插值字符串——你会写 TS 就会读 Novella。

```novella
let name = "novella";
let greeting = f"hello, {name}";
let size = greeting.charCount();
let raw = r"原始 \n 字符串";        // r"" 不做转义
let bytes = b"A\x00\xff";          // b"" 是 u8[]
```

**明确的位宽。** `u8..u64` / `i8..i64` / `f16..f64` / `usize` / `isize` 直接映射目标平台原生类型；`int` ≡ `i32`、`float` ≡ `f32`。数值只有「同符号无损扩宽」能隐式发生，窄化和跨符号转换必须写 `as`。

**两套数据模型。**

```novella
class Node {                       // 引用语义：堆分配、原子引用计数
    pub value: i64;
    pub weak parent: Node;         // 弱引用：不保活，打破父子环
    pub func __init(self, v: i64) { this.value = v; }
    pub func __destroy(self) { }        // 强引用归零时自动调用
}

struct Vec2 {                      // 值语义：复制即逐位拷贝，无继承、无隐藏状态
    pub x: f32;
    pub y: f32;
}

let a = Vec2{ x: 1.0, y: 2.0 };    // struct 用字段初始化构造，不写 new
let b = a;                         // b 是 a 的副本
let n1 = new Node(1);
let n2 = n1;                       // class 是引用：n2 与 n1 指向同一个对象
```

**离开作用域就清理。**

```novella
with openFile("data.txt") as f: File {   // __enter() 在进入块时调用
    let line = f.readline();
}                                        // 离开块：__exit()，引用归零后 __destroy()
```

**错误就是异常对象。**

```novella
from "std:exception" import Exception;

class ZeroDivisionError (Exception) {          // 异常对象是 Exception 的 class 子类
    pub func __init(self) { }
}
class DomainError (ZeroDivisionError) { }      // 没有隐藏状态，可自由扩展

func div(a: i64, b: i64) -> i32 {
    if (b == 0) { throw new ZeroDivisionError(); }
    if (a < 0) { throw new DomainError(); }
    return (a / b) as i32;
}

func demo() -> int {
    try {
        return div(10, 0);
    } catch (e: ZeroDivisionError) {           // 子类先于父类，按声明顺序匹配
        return 1;
    } catch (e: Exception) {
        return 2;
    } finally {
        cleanup();                             // 正常、异常、return 路径都会执行
    }
}
```

> 当前 `throw` / `catch` / `finally` 可通过静态检查；异常展开（unwind）尚未接入原生后端。

**泛型、trait 与模式匹配。**

```novella
// 泛型声明：class / struct / trait / func / type 均可带类型参数，可附约束
class Box<T> {
    pub value: T;
}
trait Drawable {
    pub func draw(self) -> void;
}

type Maybe = int | null;           // 联合类型：值或空

func area(shape: Drawable) -> void {
    shape.draw();
}

func classify(code: int) -> string {
    return match code {
        case 200 => "ok"
        case 404 => "not found"
        default => "unknown"
    };
}
```

**需要裸金属时。**

```novella
let value: u64 = 1;
let p: u64* = &value;          // 取址：安全操作
let pp: u64** = &p;            // `**p` 就是两个 `*`，没有特例语法
unsafe {
    *p = 9;                    // 解引用
    p[2] = 3;                  // 下标 ≡ *(p + 2)
    let addr = p as usize;     // 指针 → 整数转换同样须 unsafe
}
if (p == null) { }             // 判空与指针比较是安全操作
```

> 当前后端尚不支持指针 → 整数转换，该行在 `compile` 时会报「暂不支持」，其余部分可生成原生代码。

## 特性一览

| 领域 | 已提供 |
|---|---|
| 词法 | 行注释、Unicode 标识符、四种进制数字、普通 / 格式化 / 原始 / 字节字符串、全部运算符 |
| 类型 | 宽度整数与浮点、指针（可嵌套）、数组、元组、联合、函数类型、类型别名、泛型约束 |
| 声明 | `class`（单继承）、`struct`（值类型）、`trait`、顶层函数与变量、`extern func`（C ABI） |
| 语句 | `if/elif/else`、`while`、`for`、`for-in`（`iter()` / `hasNext()` / `next()` 协议）、`try/catch/finally`、`with`、`unsafe`、`match` |
| 表达式 | 链式访问、lambda（含异步）、`is` 类型测试、`as` 转换、`??`、三元、全部复合赋值 |
| 内存 | class 自动 ARC（原子引用计数、弱引用、归零析构）、struct 值语义与递归释放、运行时对象头 |
| 执行模型 | `async func` + `await`（单线程协作式调度）、`throw` / `catch` / `finally` 异常模型 |
| 编译器指令 | `@cfg(...)` 条件编译、`@no_std`、`@no_alloc`、`@no_main`、`@export("symbol")` |
| 工具链 | CLI（`new` / `build` / `check` / `compile`）、LSP、VS Code 扩展、增量编译缓存 |

## 快速上手

### 环境要求

- Linux x86-64（当前唯一的原生目标）
- Rust 稳定工具链（edition 2024）
- LLVM 23.1（以动态库方式加载，需开发头文件）
- `clang`（宿主链接驱动）；裸机目标另需 `ld.lld`

### 构建

```sh
cargo build --release -p novella     # 产物：target/release/novella
```

### 第一次编译

```sh
novella new hello && cd hello
```

```
hello/
├── nov.toml            # [package] name / version
├── src/main.nov        # 入口文件
└── target/hello        # build 产物
```

```sh
novella build          # 编译当前项目，可执行文件在 target/<包名>
./target/hello; echo $?   # 0
```

单文件也可以直接编译：

```sh
novella check  examples/native-main.nov                 # 只做词法 / 语法 / 语义检查
novella compile examples/native-main.nov                # 生成 a.out
novella compile examples/native-main.nov -o hello --emit obj   # 只输出 ELF 目标文件
```

`--emit` 还接受 `ast` / `hir`，把 tokens + AST 或 HIR 以 JSON 写出，便于调试与工具集成。
同一项目内的多次构建会命中增量编译缓存，`novella build --verbose` 可查看缓存命中与各阶段耗时。

### 裸机 / 内核

```novella
@no_std;
@no_main;

@export("kernel_run")
func run() -> i32 {
    return 42;
}
```

```sh
novella compile kernel.nov \
  --target x86_64-unknown-none-elf --runtime none --code-model kernel \
  --linker ld.lld --entry _start --linker-script kernel.ld \
  --link-object boot.o -o kernel.elf
```

裸机模式下没有标准库、没有托管对象、没有隐式浮点上下文；启动代码由调用方提供。契约详见 [docs/freestanding.md](docs/freestanding.md)。

### 编辑器

LSP 与 VS Code 扩展位于 `editors/vscode/`：

```sh
just lsp-build     # 构建语言服务器
just lsp-pack      # 打包 .vsix
```

扩展通过 `novella-lsp` 提供诊断、悬停信息、跳转定义、引用查找、重命名与语义高亮；关键字与词法高亮由编译器自身生成，不重复维护。

## 项目结构

```
novella/
├── crates/
│   ├── novella-core/       # Span / Token / Diagnostic / 行表 / 路径归一化
│   ├── novella-ast/        # AST 节点定义
│   ├── novella-lexer/      # logos 词法器
│   ├── novella-parser/     # 手写递归下降解析器
│   ├── novella-sema/       # 语义分析：模块、名称解析、类型检查
│   ├── novella-hir/        # 定型中间表示
│   ├── novella-codegen/    # LLVM 目标文件发射
│   ├── novella-cli/        # 二进制 novella
│   ├── novella-lsp/        # 语言服务器
│   ├── novella-runtime/    # 运行时对象头、引用计数、分配器
│   └── novella-syntax-gen/ # TextMate 语法生成器
├── std/                    # 标准库源码（Novella 编写）
├── examples/               # 可编译示例
├── grammar/                # ANTLR4 文法（以代码为基准对齐）
├── editors/vscode/         # VS Code 扩展
└── docs/                   # 语言设计、编译器实现与交接文档
```

## 文档地图

| 文档 | 内容 |
|---|---|
| [docs/design.md](docs/design.md) | **语言视角**的权威设计：语法、类型系统、对象模型、异常、执行模型 |
| [docs/program-deep-dive.md](docs/program-deep-dive.md) | 编译器总纲：一份源码从词法到 ELF 的完整旅程 |
| [docs/sema-coverage.md](docs/sema-coverage.md) | 语义规则覆盖矩阵：每条规则当前是已实现 / 部分 / 缺失 |
| [docs/freestanding.md](docs/freestanding.md) | 裸机与内核目标契约 |
| [PERFORMANCE.md](PERFORMANCE.md) | 与 C / C++ / Rust / Go / Java / Node / LuaJIT / Python 的基准对比（测量快照） |
| [ROADMAP.md](ROADMAP.md) | 里程碑、当前基线与发布门槛 |

## 当前状态

Novella 仍处于**开发预览之前**的阶段。`check` 通过、能降级到 HIR、能生成原生代码是三层不同的能力，每层有明确的支持集与拒绝集（见 [docs/sema-coverage.md](docs/sema-coverage.md) 与 [ROADMAP.md](ROADMAP.md)）：

- ✅ 类型检查、CLI、LSP、增量编译缓存；
- ✅ 标量与指针、struct 值语义、class 自动 ARC 与弱引用、字符串的标准库实现、外部 FFI 与 C 回调；
- ✅ 裸机 x86-64 目标与 `@export` 入口；
- 🚧 浮点的完整代码生成，元组、泛型单态化、容器、闭包、异常展开（unwind）、async 状态机；
- 🚧 标准库仍在建设中，目前提供 `std:string`、`std:math`、`std:mem`、`std:bits`、`std:io`（`print` / `println`）与默认环境 `std:site`；`@no_std` 下可导入 core / alloc 层模块，`@no_alloc` 下只保留 core 层。

请不要把本文的语言设计草案当作已实现能力的承诺——两者以代码为唯一事实源。

## 贡献

仓库中 `*.nov` 的注释、Rust 的 doc 与 commit 信息统一使用中文；文法以代码为基准对齐，修改 `TokenKind` 或 AST 时须同步 `grammar/*.g4`。提交前请通过：

```sh
just check && just lint && just fmt-check
```

## 许可证

[Apache-2.0](LICENSE)

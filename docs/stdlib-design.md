# Novella 标准库设计（std）

> 2026-11-21 实现修订：当前范围仅为 UTF-16 的 `String` class 与默认导入的 `site`，
> 支持入口 `@no_std;`。当前源码与限制见 [std/README.md](../std/README.md)。
> 下文 UTF-8 struct、字符串 runtime 函数和其他模块属于旧草案，不作为当前实现契约。

> 版本：**0.2**（2026-11-15）· 状态：**待评审，未实现**
> v0.2 变更：标准库系统层改为**直接链接预编译目标文件**（§2.1）；`string` **不再是语言内建类型**，
> 由 `std:String` 提供（§5.0）；异常的占位实现不进入标准库（§7）。

本文只定义标准库的**来源、链接方式、分层、边界、命名与首批 API 签名**。凡本文标「待定」的
条目，实现前必须单独立项确认，不得凭示例反推。语言规则以 `design.md` 为准，
编译器机制以 `compiler-design.md` 为准。

---

## 1. 定位与硬约束

标准库分为两层：

| 层 | 位置 | 语言 | 是否参与 sema |
| --- | --- | --- | --- |
| **novella 层**（全部公开 API） | 仓库根 `std/*.nov` | novella | 是，完整类型检查 |
| **系统层**（目标平台实现） | 仓库根 `std/lib/<target-triple>/libnovella_rt.a` | 预编译目标文件 | 否，只经 `extern` 声明 + 直接链接 |

```
std/                                        ← 标准库根（同时是 crate 根，含 nov.toml）
├── nov.toml                                ← [package] name = "std"
├── string.nov                              ← 核心类型（非内建）
├── io.nov   math.nov  convert.nov  sync.nov
├── collections/
│   ├── vector.nov   map.nov   iterator.nov
└── lib/
    ├── x86_64-unknown-linux-gnu/
    │   ├── libnovella_rt.a
    │   └── manifest.toml
    └── aarch64-unknown-linux-gnu/ …
```

约束（来自项目既定决策，不可绕过）：

| 约束 | 依据 | 对标准库的含义 |
| --- | --- | --- |
| 模块 = 文件，`std:io` → `std/io.nov` | `design.md` §4.3 | 一个标准库模块就是一个 `.nov` 文件 |
| `type` 别名不可跨模块导入 | `design.md` §4.4 | 跨模块共享的 API 一律用 class / struct 表达 |
| 裸指针、指针算术、解引用只在 `unsafe` 内 | `design.md` §6 | 标准库内部要用 `unsafe` 包装，**用户侧不见 `unsafe`、不见 `T*`** |
| struct 无继承、`==` 非法、无特殊方法 | `design.md` §8 | struct 只做值载体 |
| 特殊方法仅 class 有（`__init`/`__destroy`/`__str`/`__enter`/`__exit`） | `design.md` §4.6.1 | 容器、资源类型是 class |
| `std:` 不得使用不透明外部符号当**类型** | `docs/sema-coverage.md` M09 / `design.md` §4.3 | `std` 的**类型**必须由 `.nov` 声明并参与完整类型检查；只有**函数/全局**可以是 `extern`，且必须给出 `.nov` 声明 |
| 不得凭空编造 API | `design.md` §4.3 | 本文之外的签名一律视为不存在 |
| **标准库不含异常模块** | 异常目前是占位实现 | 标准库不依赖 `throw` / `catch`（§7） |

## 2. 来源、加载与链接

### 2.1 模块加载

- 入口 `@no_std;` 关闭默认环境注入（`std:site`），并按 `std/modules.toml` 的分层限制导入：
  core 层始终可用，alloc 层要求托管运行时，std 层导入报错。更强的一档 `@no_alloc;` 隐含
  `@no_std;` 并禁止 alloc 层，只保留 core 层。两条指令都不禁用平台 C 启动环境或用户自己的
  外部声明。

- `std:<path>` → `$STD_ROOT/<path>.nov`，与用户模块同等对待：完整词法 / 语法 / 语义 /
  可见性检查，参与导入闭包、循环导入去重、诊断定位。
- `STD_ROOT` 的确定顺序：
  1. `analyze_with_options` 显式给出的 `std_root`（测试用内存加载器、交叉编译）；
  2. CLI `--std-root <dir>`；
  3. 工具链内嵌目录：与编译器二进制同级的 `share/novella/std`，开发期即仓库根 `std/`；
  4. 都没有 → 报「标准库未安装：找不到 STD_ROOT」。
- 安装包必须携带 `std/modules.toml`：`@no_std` 依赖它判断模块层级，缺失时 `std:` 导入报错。
- `std:` 模块的诊断回显 `std:<module>` 形式的文件路径。
- `std` 模块的顶层符号必须 `pub`；`pri` 只用于辅助函数，且不可被导入。

### 2.2 预编译目标文件直接链接

`std/*.nov` 中需要触及操作系统 / 分配器 / 时钟 / 字符串缓冲的函数，以 `extern` 声明、
由 `std/lib/<target>/libnovella_rt.a` 提供实现。**编译器不做 LLVM 侧 JIT / 内联替换，
只按符号名把目标文件交给链接器。**

```novella
// std/string.nov 中的系统层声明（示例形状）
extern func novella_rt_string_from_literal(ptr: u8*, len: usize) -> String;
```

规则：

1. **搜索路径**：`$STD_ROOT/lib/<target-triple>/`（`<target-triple>` 取自 `--target` /
   `AnalysisOptions.target`）+ `--link-search <dir>` 追加（可多次）。仅收 `.a`。
2. **目标目录缺失即报错**：`--target` 对应的目录不存在时，报
   `标准库无此目标的预编译产物：x86_64-…（缺少 std/lib/x86_64-…/libnovella_rt.a）`，
   不允许静默回退到宿主目标。宿主编译等价于 `--target <宿主 triple>`。
3. **链接顺序**：标准库 `.a` 在前、用户目标文件在后（`--link-object` / `--link-search` 产出的
   目标文件），重复扫描由 `manifests.keep_repeated` 指定的归档处理——否则 `libc` 一类
   还需要的标准库依赖会因单趟扫描而缺失。扫描顺序写死为：
   `libnovella_rt.a` → 全部用户目标文件（按命令行出现顺序）→ 用户 `--link-search` 找到的归档
   （按目录优先、文件名升序）。
4. **符号名**：默认与 `extern` 函数名相同；需要不同 C 名时使用 `@extern("c_name")` 指令
   （**尚无此指令**，见 §10 Q7）。在指令落地前，标准库只使用同名符号。
5. **manifest 校验**：`manifest.toml` 记录 `triple` / `abi_version` / 导出符号表 /
   `libc_requirement`；编译时逐项对照：triple 不一致、`abi_version` 与编译器期望不一致、
   标准库用到的符号不在导出表里，全部报错并指明缺哪个符号。校验在链接前完成，
   以便给出可读诊断而不是链接器噪声。
6. **HIR 不变**：`HirItem::ExternFunction` 已经是「只声明不建基本块」，无需新增 HIR 结构；
   新增的只有 codegen 的**链接清单**（来自 sema 的 `resolved_imports` 中属于 `std:` 的模块
   → 其 manifest 的归档）
7. **安全边界**：`extern` 调用按 `design.md` §6 必须在 `unsafe` 内，因此**只有 `std/*.nov`
   内部出现 `unsafe`**。安全层函数在函数体内 `unsafe { ... }` 后返回普通值，用户代码调用
   它们不需要 `unsafe`。这条规则是"extern 仍需 unsafe"与"用户不见 unsafe"同时成立的唯一写法。
8. **预编译产物不含 novella 语义**：`.a` 里只能是 C ABI 可见的符号；任何需要类型检查的
   逻辑都必须在 `.nov` 里重写，**不得**为了省事把 API 直接暴露成 extern。
9. **不带 `.a` 的平台**：`std:io` / `std:sync` 这类必须系统层的模块，在没有对应
   `<target>` 目录时**整个库不可用**（import 即报错）；纯 novella 模块（`std:math` 的
   纯函数部分）不受影响——每个模块在 manifest 里显式声明 `requires_rt = true/false`。

## 3. 分层（防循环依赖）

依赖只允许**向右**：

```
std:string ← math / convert ← io   |   collections ← 用户代码
（`std:string` 是唯一的零依赖类型模块；math/convert 依赖它的格式化）
```

- `String` 在 `std:string`，被所有其他模块依赖；`std:string` 本身不导入任何 `std:` 模块。
- `math` / `convert` 相互之间不得互相导入；共享的数值格式化放进 `std:string`
  的 `formatInt` / `formatFloat`（唯一一份实现，f-string 展开也调它）。
- `collections/*` 允许互相导入（`list` 与 `map` 共享 `iterator`）；同目录内出现强连通
  组件视为设计错误，用 `@:` 依赖图在 CI 中比对。
- 容器不使用 `T*`，元素按值存放。

## 4. 命名规约

| 类别 | 规约 | 示例 |
| --- | --- | --- |
| 模块 | 小写单词，单数名词 | `std:io`、`std:string`、`std:collections/list` |
| 类型 | 大驼峰 | `String`、`Vector`、`ConvertResult` |
| 函数 / 方法 | 小写驼峰 | `println`、`push`、`withCapacity` |
| 常量 | `const` + 大写下划线 | `MAX_I64`、`CHAR_WIDTH` |
| 系统层符号 | `novella_rt_` 前缀 + 小写下划线；**不出现**在任何公开 API 里 | `novella_rt_string_concat` |
| 副作用标记 | 无语言标记，**由文档注明** | `readLine` 会阻塞 |
| 参数顺序 | 接收者 → 主数据 → 可选配置 | `Map.put(key, value)` |

标准库内部用定宽名（`i32` / `f64` / `usize`），`usize` 只用于长度与索引。

## 5. API 草案

### 5.0 `std:string`（核心类型，**非内建**）

`string` 不是语言内建类型。以下定义是唯一的字符串类型：

```novella
pub struct String {
    pri data: u8*;     // UTF-8 字节，不保证以 0 结尾
    pri len: usize;    // 字节长度
}

// 系统层（std/lib/<target>/libnovella_rt.a）
extern func novella_rt_string_from_bytes(data: u8*, len: usize) -> String;
extern func novella_rt_string_release(s: String) -> void;
extern func novella_rt_string_copy(s: String) -> String;
extern func novella_rt_string_eq(a: String, b: String) -> boolean;
extern func novella_rt_string_concat(a: String, b: String) -> String;
extern func novella_rt_string_byte_at(s: String, index: usize) -> u8;
```

语义约定（**待确认项集中在这里**）：

- **字面量**：字符串字面量的类型是 `std:String`。codegen 把 `HirOperand::StringConst` 物化为
  `(data, len)` 常量对后调用 `novella_rt_string_from_bytes`；是否改用编译期常量对
  （`Constant::StructValue` 直接构值，免一次调用）列 Q9。
- **按值拷贝**：struct 赋值 / 传参复制的是 `data` 指针 + `len`，**不复制缓冲区**。
  这意味着 `a = b` 后两者别名同一块内存；`__destroy` 不可用（struct 无特殊方法），
  `pri` 字段只有当标准库内部代码能访问。内存所有权与释放策略是 **Q8**，未定之前
  `std:string` 只能用于"字面量与临时值"场景。
- **`==` 与 `+`**：`design.md` §8.5 规定 struct 不支持 `==`。`String` 需要开列名例外：
  `==` 展开为 `novella_rt_string_eq`，`+` 展开为 `novella_rt_string_concat`，`+=` 同理。
  这是**唯一**允许 struct 使用 `==` / `+` 的类型，例外必须写回 `design.md` §8.5 与
  §sema-coverage（矩阵加一行）。未获批前，标准库内部一律用 `novella_rt_string_eq` 显式比较。
- **字面量 `string` 关键字**：过渡期为 `type string = String;` 别名（同模块生效），
  最终形态见 §5.12 G8。

辅助函数（非系统层，纯 novella）：

```novella
pub func byteLength(s: String) -> usize;
pub func charCount(s: String) -> usize;               // UTF-8 码点数（Q1）
pub func codePointAt(s: String, index: usize) -> i32?; // 需要泛型可空（Q2），暂缓
```

### 5.1 错误报告策略（不依赖异常）

标准库**不使用 `throw`**（异常目前是占位实现）。三类报告方式：

1. **哨兵值**：数值/字符串返回约定值——`<= 0` 表示错误、`""` 表示失败。
2. **结果 struct**：需要携带"成功/失败 + 值 + 错误码"时返回值类型（如 `ConvertResult`）。
3. **`__str` 的约束**：class 的 `__str` 返回类型被编译器固定为 `Ty::String`；`String` 落地后
   该约束原样保留，只是 `Ty::String` 换成 `Ty::Struct(std, "String")`（见 §9 S0-3）。

不使用第三种以上的任何花招（无 `throws`、无错误传播运算符、无 `Result` 泛型）。

### 5.2 `std:math`

```novella
pub const PI: f64 = ...;
pub const E: f64 = ...;

pub func abs(x: i64) -> u64;
pub func min(a: i64, b: i64) -> i64;
pub func max(a: i64, b: i64) -> i64;
pub func floor(x: f64) -> f64;
pub func ceil(x: f64) -> f64;
pub func round(x: f64) -> f64;
pub func sqrt(x: f64) -> f64;
pub func fabs(x: f64) -> f64;
```

- 只有 `f64` 版本；`abs(i64) -> u64`（`i64::MIN` 取绝对值溢出，返回无符号值让调用方面对）。
- 纯函数，`requires_rt = false`，**不依赖 libm**：取整走 `x as i64`（硬件截断指令）再
  转回 `f64`，`sqrt` 为 Newton 迭代，其余只用 `+`、`-`、`*`、`/`、比较与字面量。
  早期版本用 `x % 1.0` 取整，会发射 `llvm.frem` 并在链接期退化为 libm 的 `fmod`；现已改写。
- **不能**用 `x / 1.0` 做整数化——IEEE-754 除法不取整（`2.7 / 1.0 == 2.7`），浮点余数
  `%` 又会引入 `frem`，两者都不是可用的取整路径。
- `x as i64` 对超出 `i64` 范围的值是**未定义行为**（x86 上 `cvttsd2si` 触发 SIGILL），
  因此每个取整函数都先用 `beyondIntegerRange(x)` 判断 `|x| >= 2^63`：这个区间外的 `f64`
  本身已无小数位，直接返回 `x` 即正确。新增同类取整函数时**必须先做该判断再转换**。
- `round` 的 `.5` 一律远离零（`2.5 -> 3`、`-2.5 -> -3`），沿用旧实现而非银行家舍入。
- 因此内置 Linux 目标的 `linker-args` 为空（不再链接 `-lm`），数学函数不引入任何外部符号。

### 5.3 `std:io`

**已实现部分**（`std/io.nov`，2026-11-22）：只做 stdout，系统边界直接用 libc
`write(2)` / `abort`，编码由 `String.writeUtf8` 承担（`data` 留在定义模块内，
不走 `pkg` 字段也不需要 `asBytes` 拷贝）：

```novella
pub func print(message: String);     // 全部字节，不追加换行
pub func println(message: String);   // 追加一个 LF；空串只输出换行
```

- 逐 UTF-16 码元编码为 UTF-8：代理对合成 4 字节，**孤立代理码元写 U+FFFD**。
- `null` 实参不写任何字节；缓冲区上界 `length() * 3 + 4`，分配失败调用 `abort`。
- 不显式 flush；裸机目标不提供本模块（§2.9）。

**其余仍是设计草案**：

```novella
extern func novella_rt_io_write_stdout(bytes: u8*, len: usize) -> i32;
extern func novella_rt_io_write_stderr(bytes: u8*, len: usize) -> i32;
extern func novella_rt_io_read_line(buf: u8*, cap: usize) -> isize;

pub func eprint(text: String) -> i32;
pub func eprintln(text: String) -> i32;
pub func readLine() -> String;            // EOF 返回空串（Q3）
pub func writeFile(path: String, text: String) -> i32;   // 0 成功
pub func readFile(path: String) -> String;               // 失败返回 ""（Q3）
pub func exists(path: String) -> boolean;
```

- 草案曾要求 `print` / `println` 返回 `i32`；已实现版本返回 `void`，因为
  `write(2)` 的部分写入与 EINTR 重试尚未定义重试策略，先不暴露不可靠的返回值。
- 草案里 "`std:io` 自己用 unsafe 按字节写出去" 的结论已被修正：`String.data` 是
  `pri` 且没有 `impl` 块语法，唯一的合法位置是 `string.nov` 内的成员方法，
  故编码落在 `String.writeUtf8`（§9 S0-2 的 `pkg` 字段与 `asBytes` 两条路都未采用）。
- 阻塞语义、文件路径规范化属于系统层契约，Q5 定案前只实现 stdout / stderr。

### 5.4 `std:convert`

```novella
pub struct ConvertResult {
    pub ok: boolean;
    pub value: i64;
    pub code: i32;
}

pub const CONVERT_OK: i32 = 0;
pub const CONVERT_EMPTY: i32 = 1;
pub const CONVERT_INVALID_DIGIT: i32 = 2;
pub const CONVERT_OVERFLOW: i32 = 3;

pub func parseInt(s: String) -> ConvertResult;
pub func parseFloat(s: String) -> f64;     // 失败返回 0.0，成功与否用 isValidFloat 判断（Q3）
pub func toString(x: i64) -> String;
pub func toString(x: u64) -> String;
pub func toString(x: f64) -> String;       // 小数位数 Q6
pub func toString(x: boolean) -> String;
```

- **不提供重载**：novella 没有函数重载（§5.12 G3）。上表的 `toString` 多个版本若无法同模块
  共存，则改名 `toStringInt` / `toStringUInt` / `toStringFloat` / `toStringBool`（Q6 定案）。

### 5.5 `std:collections`

以下为草案，`Vector<T>` 尚未实现。它是由 ARC 管理的可增长容器，与内联定长数组 `T[N]` 不同。

```novella
// std/collections/vector.nov
pub class Vector<T> {
    pub static func empty() -> Vector<T>;
    pub static func withCapacity(capacity: usize) -> Vector<T>;
    pub func length() -> usize;
    pub func __getitem(self, index: usize) -> T;        // 越界行为 Q3
    pub func __setitem(self, index: usize, value: T) -> void;
    pub func push(value: T) -> void;
    pub func pop() -> T;
    pub func insert(index: usize, value: T) -> void;
    pub func remove(index: usize) -> T;
    pub func clear() -> void;
}
```

- 底层缓冲区由 `Vector<T>` 内部管理，不暴露给调用方。
- 容器可按 [语言设计的迭代协议](design.md#for--in-迭代协议)提供 `iter()`，由迭代器实现 `hasNext() -> boolean` 和 `next() -> T`。前端已检查该协议，标准库 `Iterator<T>` 声明和非数组循环降级仍在后续阶段。
- `Map<K, V>` 依赖哈希与相等协议（Q4），泛型容器的运行依赖单态化（G6）。
- `Vector` 依赖 HIR 的泛型方法单态化，S4 前置条件见 §9。

### 5.6 `std:sync`（不实现）

运行时线程模型未定。前端点上**不存在** `std:sync`；import 报缺失。

### 5.7 可表述性缺口（实现前必须解决）

| # | 缺口 | 影响 |
| --- | --- | --- |
| G1 | `const` 只能修饰顶层 / class 成员，不能修饰方法 | `std:math` 常量 |
| G2 | 顶层 `pub const` 解析为 `Item::Statement`；sema 已能检查，但 **HIR 对顶层语句一律拒绝** | `PI` / `E` / `CONVERT_OK` 全部不可用 |
| G3 | 无函数重载 | `toString` 多版本、`Vector` 构造（使用静态工厂） |
| G4 | `void` 返回类型写法未实测 | 所有 void 函数 |
| G5 | Sema 已支持 trait 约束，哈希与相等协议待定 | `Map<K: Hashable>` |
| G6 | Sema 已支持调用实参推导，HIR 单态化待实现 | `Vector<T>` / `Iterator<T>` |
| G7 | `String` 无特殊方法；`s.length()` 只能写成 `length(s)` | `std:string` 全部为自由函数 |
| G8 | **`string` 关键字去留**：内建 `Ty::String` 删除后，`string` 是保留关键字还是可作普通标识符 | 迁移期所有 `.nov` 源码、`__str` 签名、`Ty::String` 的 15 处引用 |

### 5.8 不在 v1 范围

正则、网络、线程、异步 I/O、目录遍历、日期时间、JSON。都依赖尚未定义的运行时契约。

## 6. 与编译器的接口

1. **加载**：`std:` 已走普通模块路径；`StdExternal` 分支（`module/resolve_path.rs:99`）保留给
   `std:exception` 的占位，**与标准库无关**，标准库任何模块都不走它。
2. **HIR**：标准库模块与其他模块一样参与 `build_hir`。HIR 会拒绝的构造（泛型、trait 派发、
   **顶层语句**）必须先补齐 HIR 或延后对应标准库项；**不允许**用 `Unimplemented` 拒掉
   "标准库函数"来绕过。
3. **codegen**：新增链接清单（§2.2.6）。codegen 当前为空壳，这是 S0 的第一个实际实现。
4. **文档与诊断**：缺 `std` 模块时报 `标准库模块 'std:xxx' 不存在` 并提示 `--std-root`。

## 7. 异常搁置的影响面

| 涉及异常的设计点 | 搁置后的处理 |
| --- | --- |
| `throw` / `catch` 作为错误报告 | 一律用 §5.1 的哨兵值 / 结果 struct |
| `IOException` / `ParseError` 等派生异常 | 不存在，改错误码与结果类型 |
| `Exception` 的 `__str`、字段、构造 | 不写；`Exception` 仍是 `ModuleId::MAX` 名义根，标准库不引用 |
| HIR 的异常传播 | `design.md` 的 `try` 降级不在标准库验收范围 |

## 8. 测试与验收

- 每个标准库模块附 `std/tests/<module>.nov`（不入标准库导入闭包，仅由测试套件编译）。
- 验收线（v1 只放行满足全部条件的模块）：
  1. `from "std:x" import …` 零诊断；
  2. 全部顶层符号参与完整类型检查（不是 `External`）；
  3. HIR 能降级（`hir_smoke` 对 std 模块同样通过）；
  4. 系统层符号在 manifest 导出表中**全部存在**（链接前静态校验）；
  5. 模块在 `design.md` 有对应语义定义，否则归入 §5.8。
- **不验收**：运行时正确性、性能（属后端/解释器的测试）。

## 9. 分阶段交付

| 阶段 | 内容 | 前置条件 |
| --- | --- | --- |
| **S0-1** | codegen 最小实现：消费 `HirItem::ExternFunction` 生成 declaration、按 manifest 直接链接 `.a`、产出可执行文件 | `cargo check`；本阶段唯一验收物是一段 extern 调用能被链接器解析 |
| **S0-2** | `std/string.nov` 落地 + `std/lib/<target>/libnovella_rt.a` 最小实现（`string_eq` / `concat` / `from_bytes`） | S0-1；Q8 所有权协议先给临时约定 |
| **S0-3** | `string` 非内建迁移：`Ty::String` → `Ty::Struct(std, "String")`；`__str` 返回类型同步；字面量类型解析改 | S0-2；G8 定案；sema-coverage 补行 |
| S1 | `std:math` + `const` 的全局降级（G2） | HIR 支持 `Item::Statement` 的 const 声明 |
| S2 | `std:convert` | S0-3、G3/G4 |
| S3 | `std:io`（stdout/stderr） | S0-1 |
| S4 | `std:collections/list` | 泛型方法单态化 |
| S5 | `std:io` 文件读写、`Map`、`Iterator` | Q3/Q5、G5/G6 |
| S6 | `std:sync` | 线程模型 |

S0 是硬前置：**先有"直接链接"这条路，标准库才谈得上实现**。

## 10. 待确认清单

| # | 问题 | 影响 | 当前默认（可执行） |
| --- | --- | --- | --- |
| Q1 | `String` 的长度/下标按码点还是字节？ | `std:string` 全下标语义 | 字节 + 提供 `charCount` |
| Q2 | 泛型 `T?` 可空类型是否引入？ | `Map.get` | 暂不引入。迭代结束由 `hasNext()` 表达 |
| Q3 | 越界/缺失/解析失败的具体哨兵取值 | `Vector`、`Map`、`convert` | `<=0` / `""` / `ConvertResult` |
| Q4 | `Map` 键哈希/相等协议如何进 `__` 封闭清单 | `Map<K, V>` | 不实现 Map |
| Q5 | 系统层 I/O 契约（阻塞、错误映射） | `std:io` 文件函数 | 只做 stdout/stderr |
| Q6 | 函数重载是否引入 | `toString` 命名 | 改名 `toStringInt` 等 |
| Q7 | `extern` 的链接名重命名指令（`@extern("name")`）/ 链接清单传递方式 | `std` 与用户 extern 共用 | 同名符号，不做重命名 |
| Q8 | `String` 的内存所有权 / 释放协议 | `std:string` 能否用于长期值 | 临时值 + 字面量，释放由系统层在 scope 退出时进行 |
| Q9 | 字面量用 `from_bytes` 调用还是编译期常量对 | 性能、常量折叠 | 先调用 |
| Q10 | `.a` 从哪来（C 源码随仓库 → CI 交叉编译 vs 预置二进制） | S0-1 的工程形态 | C 源码随仓库 + CI 构建 |

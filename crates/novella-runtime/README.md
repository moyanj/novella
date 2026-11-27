# Novella runtime

runtime 只承载语言对象模型必须的底层机制。字符串的 UTF-16 数据处理、比较、拼接、
切片仍由 `std/string.nov` 实现，不在 runtime 重写一份。

对象管理代码使用 C11，Rust 包负责随编译器携带静态库。构建需要支持固定目标参数的
Clang 和 `ar`，可用 `NOVELLA_RUNTIME_CC`、`NOVELLA_RUNTIME_AR` 指定工具路径。
无需 Rust 目标库或额外 Cargo 依赖。Cargo 在 `OUT_DIR` 生成以下六个归档：

| 归档 | 目标与内容 |
| --- | --- |
| `libnovella_rt.a` | Linux x86-64 ARC + 独立 hosted provider 对象，兼容原有名称 |
| `libnovella_rt-bare-small.a` | `x86_64-unknown-none-elf`，small 代码模型，只有 ARC |
| `libnovella_rt-bare-kernel.a` | 同一裸机目标，kernel 代码模型，只有 ARC |
| `libnovella_support-linux.a` | Linux 的独立内存函数，可选链接 |
| `libnovella_support-bare-small.a` | 裸机 small 的独立内存函数 |
| `libnovella_support-bare-kernel.a` | 裸机 kernel 的独立内存函数 |

`STATIC_ARCHIVE` 和 `TARGET_TRIPLE` 保留 Linux 兼容行为。编译器按目标调用
`managed_archive(bare_metal: bool, kernel: bool) -> &'static [u8]` 和
`support_archive(bare_metal: bool, kernel: bool) -> &'static [u8]` 选择内嵌字节；
`bare_metal == false` 时忽略 `kernel`，目标配置层负责拒绝 Linux kernel 组合。

所有 C 对象使用固定 Clang triple、`-ffreestanding -fno-builtin`，禁用 stack protector
和 unwind tables。裸机额外使用 `-mno-red-zone -mgeneral-regs-only -fno-pic -fno-pie`，
并明确设置 `-mcmodel=small` 或 `-mcmodel=kernel`。构建脚本跟踪全部 C 源、公开头文件、
脚本自身和两个工具环境变量的变化，重建时清空归档，避免留下旧对象成员。

## 平台 hooks 与内存 support

ARC 核心没有 libc 或 libatomic 依赖，也不默认初始化为 libc 函数地址。最终程序必须提供：

```c
void* novella_platform_alloc(size_t size);
void novella_platform_free(void* ptr);
_Noreturn void novella_platform_panic(unsigned code);
```

`alloc` 返回满足基本 C 类型对齐的内存，失败返回空；`free` 接收配对的原始分配地址。
Linux managed 归档的独立 `hosted.c` provider 分别包装 libc `malloc`、`free`、`abort`，
裸机 managed 归档不包含它。静态链接器仅在需要 hooks 时抽取 provider 对象；
若 Linux 程序自行提供 hooks，应同时定义全部三个，以免与 provider 的完整定义冲突。

panic 错误码在公开头文件的 `novella_rt_error` 中固定如下，核心不格式化或输出诊断：

| 值 | 错误 |
| --- | --- |
| 1 | 无效对象对齐 |
| 2 | 对象布局溢出 |
| 3 | 控制块分配失败 |
| 4 | 载荷分配失败 |
| 5 | 无效强引用计数或溢出 |
| 6 | 无效 weak 计数或溢出 |
| 7 | 重复释放 weak |
| 8 | 释放正在析构的对象 |
| 9 | setter 回调为空 |
| 10 | 分配器已固定后再次调用 setter |
| 11 | 标准库致命错误（`novella_rt_abort`） |

panic hook 必须不返回；核心经 volatile 函数指针调用后仍执行 trap，避免错误实现意外返回。
`novella_rt_abort` 是标准库的统一终止出口（分配失败、长度溢出、越界等），经同一 hook 以错误码 11 终止。
无效裸指针或已经物理释放的对象不在运行时可诊断的范围内。

独立 `memory.c` 只导出 `memcpy`、`memmove`、`memset`、`memcmp`，不依赖平台 hooks 或
ARC，支持重叠移动、零长度以及 unsigned byte 比较语义。volatile 字节循环和
`-fno-builtin` 防止优化器将实现转换成递归调用。裸机链接器应按需加入 support；
Linux 默认使用 libc，避免自动链接 support 覆盖 libc 的内存函数。

## 分配器配置

`novella_rt_malloc(size)` / `novella_rt_free(ptr)` 是所有对象、控制块及语言 raw 缓冲区
共用的分配入口。raw malloc 保留失败返回空的行为，ARC 则转交对应 panic 错误码。
`novella_rt_free(NULL)` 无操作，且不会固定分配器。

可以在首次分配前调用原有 `novella_rt_set_allocator(allocate, deallocate)`；两个回调必须
同时非空。未配置时回落平台 hooks。原子锁只用于串行化 setter 与首次使用：首次
`malloc`/非空 `free` 在锁内解析出最终入口并成对发布，之后每次分配只做一次 acquire
读取，不再取锁。一旦首次使用便永久固定分配器，即使所有对象已经释放也不可替换。
后续 setter 触发错误码 10，避免活对象或
尚存 weak 控制块被另一分配器释放。回调在锁外调用，允许重入 runtime；
回调和平台 hooks 必须满足线程安全契约。不要在中断中重入可能被中断的配置锁持有者；
需要中断安全的内核应在外层屏蔽中断或禁止此类重入。分配器状态仅存在于实现内部，头文件
不再定义全局可写函数指针；已有调用方通过 setter 适配，不直接写 `global_malloc/free`。

## 对象管理 ABI

接口定义见 `include/novella_rt.h`：

| 接口 | 行为 |
| --- | --- |
| `novella_rt_alloc(size, alignment, destroy)` | 分配零初始化的载荷，返回独立对象身份，初始强引用计数为 1 |
| `novella_rt_retain(object)` | 增加强引用计数；空指针无操作 |
| `novella_rt_release(object)` | 减少强引用计数；最后一次释放同步执行析构回调，然后释放对象本体；空指针无操作 |
| `novella_rt_weak_create(object)` | 从有效强引用创建一份拥有的 weak 控制块引用；空对象返回空 |
| `novella_rt_weak_retain(weak)` | 复制 weak 所有权；空指针无操作 |
| `novella_rt_weak_release(weak)` | 释放一份 weak；最后一份 weak 且对象已析构时回收控制块；空指针无操作 |
| `novella_rt_weak_upgrade(weak)` | 原子取得一份强引用；目标已开始析构或 weak 为空则返回空 |

用户字段区位于隐藏对象头之后。头部指向独立控制块，控制块保存原始分配地址、
原子强/弱引用计数和析构回调；
返回地址满足请求的对齐，零长度对象仍具有独立身份。无效对齐、布局溢出、内存不足和
引用计数溢出交由平台 panic hook 终止执行。析构回调可以为空，只负责释放载荷拥有的资源，不能
再次释放、复活当前对象或自行释放对象本体。

调用者必须持有有效强引用才能访问或 retain 对象。计数原子化只保护不同强引用的并发
管理，不保证字段读写的线程安全，也不能让已释放的裸指针恢复有效。强引用环不会自动回收。

## 弱引用

weak 字段保存 `novella_rt_weak *`，它是单指针大小的不透明控制块句柄，与对象载荷指针
不可互换。字段零初始化表示空 weak。写入强引用时，先 `weak_create` 新句柄，再
`weak_release` 旧句柄；复制现有句柄时先 `weak_retain`，字段销毁时 `weak_release`。
读取目标必须调用 `weak_upgrade`，成功返回的载荷已拥有一份强引用，使用完必须 `release`。
目标销毁后，句柄不会物理改写为零，但任何读取/升级均返回空，不能解引用控制块。

强引用存在期间，控制块持有一份隐式 weak。最后一次强 `release` 将强计数置零，
同步执行析构并释放载荷，随后释放隐式 weak。外部 weak 可继续判活；最后一份 weak
释放时才回收控制块，因此 weak 不延长目标的载荷或资源生命周期。
`weak_upgrade` 使用比较交换，仅能增加非零强计数；与最后一次强释放竞争时，
或者先取得强引用并延长目标存活，或者观察到零返回空。析构回调内也不能升级自身。
析构回调可以释放指向自身的 weak，隐式 weak 保证回调结束前控制块仍存活。

创建 weak 要求调用方持有有效强引用；复制、升级或释放句柄要求调用方持有有效 weak。
同一 weak 字段槽位的并发读写仍需外部同步，不能一边覆写/释放其唯一句柄一边读取升级。
对象普通字段也不因原子计数而获得并发访问保证。

`examples/runtime-weak.nov` 保留 `@no_std` 与显式 C ABI 调用：父对象强持有子对象，
子对象保存父对象的 weak 控制块，先验证存活升级，再释放父对象并验证升级返回空，
最后释放子对象及 weak 控制块。原生运行输出 `APCZ`、退出码 `42`，其中 `P`、`C`
分别由父、子析构输出。语言字段的 codegen 同样必须采用控制块存储及读取升级约定。

`cargo test -p novella-runtime` 会编译并运行 `tests/weak.c`，直接链接本包构建的
静态库，覆盖空指针、对齐、弱复制、强引用转移、析构内升级失败、自身 weak 释放，
以及多线程 weak upgrade 与最后强 release 的竞争。

## 所有权与编译器边界

所有权描述谁负责保持资源存活并最终释放。Novella 的 class 采用设计文档 §7 的共享所有权：
赋值与传参共享同一个对象，每一份强引用需要对应的 retain/release，最后一份引用失效时析构。
String 对象拥有自己的 UTF-16 缓冲区，因此析构先释放缓冲区，runtime 再释放对象本体。

本包已实现这些底层操作；编译器尚未对 class 构造、赋值、返回及离开作用域自动生成完整 ARC。
当前可通过显式 `extern` 在 `unsafe` 中使用，不能据此认为 String 或任意 class 已能原生执行。
`examples/runtime-object.nov` 演示原生调用、函数指针析构回调以及两份引用的释放：输出 `AD`，
其中 `A` 位于最后一次释放前，`D` 由析构输出，进程退出码为 `42`。

```sh
cargo run -p novella -- compile examples/runtime-object.nov -o /tmp/runtime-object
```

CLI 在链接时提供静态库，链接器仅抽取被引用的 runtime 对象；未引用 runtime 的标量程序
不会含有这些实现。`build` 缓存命中也重新执行该链接步骤。`--emit obj` 只输出目标文件，
手动链接使用者需自行提供 runtime 静态库。`@no_std;` 禁用 Novella 标准库和 site 默认环境，
仍允许用户显式调用 runtime 或其他 C ABI 函数。

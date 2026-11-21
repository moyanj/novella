# Novella runtime

runtime 只承载语言对象模型必须的底层机制。字符串的 UTF-16 数据处理、比较、拼接、
切片仍由 `std/string.nov` 实现，不在 runtime 重写一份。

当前实现目标为 `x86_64-unknown-linux-gnu`，对象管理代码使用 C11，Rust 包负责随编译器
携带静态库。构建需要 `clang` 和 `ar`，可用 `NOVELLA_RUNTIME_CC`、`NOVELLA_RUNTIME_AR`
指定工具路径。静态库由 Cargo 构建脚本生成到包的 `OUT_DIR/libnovella_rt.a`，嵌入 CLI。

## 对象管理 ABI

接口定义见 `include/novella_rt.h`：

| 接口 | 行为 |
| --- | --- |
| `novella_rt_alloc(size, alignment, destroy)` | 分配零初始化的载荷，返回独立对象身份，初始强引用计数为 1 |
| `novella_rt_retain(object)` | 增加强引用计数；空指针无操作 |
| `novella_rt_release(object)` | 减少强引用计数；最后一次释放同步执行析构回调，然后释放对象本体；空指针无操作 |

用户字段区位于隐藏对象头之后。头部保存原始分配地址、原子强引用计数和析构回调；
返回地址满足请求的对齐，零长度对象仍具有独立身份。无效对齐、布局溢出、内存不足和
引用计数溢出打印原因后终止进程。析构回调可以为空，只负责释放载荷拥有的资源，不能
再次释放、复活当前对象或自行释放对象本体。

调用者必须持有有效强引用才能访问或 retain 对象。计数原子化只保护不同强引用的并发
管理，不保证字段读写的线程安全，也不能让已释放的裸指针恢复有效。强引用环不会自动回收。

## 弱引用

沿用语言设计 §7.4 的非拥有弱引用：字段保存对象载荷地址，赋值不 retain，字段销毁
不 release，读取也不自动取得临时强引用。因此无需额外的 runtime 函数或控制块。
最后一份强引用释放后，对象立即析构并回收，弱字段不会自动置空；非空检查不能证明
目标仍然存活。访问目标或将弱引用复制为强引用前，调用者必须另行保证目标存活，
也不能在另一线程可能释放最后一份强引用时直接 retain 弱目标。

`examples/runtime-weak.nov` 演示父对象强持有子对象、子对象弱指向父对象。释放父对象时，
子对象的局部强引用仍存活，父对象仍会立即析构；随后清空弱字段并释放子对象。
程序输出 `APCZ`、退出码 `42`，其中 `P`、`C` 分别来自父、子析构，表明弱回边没有形成
强引用环。子对象析构不释放其弱目标，也不访问已经销毁的父对象。

```sh
cargo run -p novella -- compile examples/runtime-weak.nov -o /tmp/runtime-weak
```

此例显式操作指针槽位；语言 `weak` 字段的类型检查和 HIR 标记已接通，自动 class ARC
与原生字段访问仍受下一节所述后端边界限制。

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

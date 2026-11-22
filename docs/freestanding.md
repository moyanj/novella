# 无 libc 的 x86-64 裸机与内核

## 三个独立开关

`@no_std;` 只控制 Novella 标准库与 site 注入，不承诺没有运行时、没有堆、没有 CRT 或 libc。
裸机目标为 `x86_64-unknown-none-elf`，必须在入口模块同时声明 `@no_std;` 和
`@no_main;`。`@no_main;` 禁止查找 Novella `main` 和生成 C `main` 包装，进程或机器的
启动环境由调用方提供。Linux 上原来的 `@no_std; func main() -> int` 仍是 hosted 程序。

运行时配置与入口配置独立：

| 选项 | 裸机默认值 | Linux 默认值 | 含义 |
| --- | --- | --- | --- |
| target | 显式选择 | `x86_64-unknown-linux-gnu` | 实际 LLVM 发射平台与条件编译环境 |
| runtime | `none` | `managed` | 是否允许托管对象与 ARC |
| code-model | `kernel` | `small` | 最高 2 GiB 内核地址或低地址静态模型 |

`runtime=none` 允许标量、裸指针、函数指针和不含托管成员的值类型；数组、字节字符串、
class 分配、所有权操作和 weak 操作需要 `managed`。纯类型声明不应该引入堆依赖。
静态全局支持零初始化、标量与指针常量，以及无托管 struct 的递归常量初始化；
全局取址可用于后续读写，定型函数指针支持间接调用和 C ABI 回调。不执行动态全局构造。
整数转裸指针仍要求 unsafe；顶层没有
unsafe 表达式语法，因此 MMIO 指针可先静态置空，再在启动函数的 unsafe 块中赋固定地址，
不能通过裸机目标绕过现有安全检查。

裸机全部函数使用无红区、无隐式浮点的整数执行环境；显式浮点表达式、浮点签名和浮点
extern 不受支持。没有 SSE/MMX/AVX 上下文初始化代码，内核不能通过本编译模式生成
浮点或向量运算。`kernel` 使用最高 2 GiB 地址，`small` 示例使用低于 2 GiB 的地址；
链接脚本和调用方必须满足相应地址约束。

## 入口与 C ABI 导出

```novella
@no_std;
@no_main;

@export("kernel_run")
func run() -> i32 {
    return 42;
}
```

`@export("symbol")` 标记有函数体的顶层函数，使用明确的 C ABI 符号。参数和返回值限于
标量、裸指针、函数指针及无返回值，不允许把 Novella class、数组或值聚合直接跨 ABI
传递。泛型、异步函数与 extern 声明不能导出；导出名字必须唯一并且不能与 extern
链接名冲突。普通未导出的函数保持内部符号。

机器入口一般是独立汇编符号，例如 `_start`。它建立调用约定所需的栈与 CPU 状态，再调用
C BSP 或 Novella 导出函数。`--entry` 是 ELF 入口符号，不能替代这些启动工作。

## 编译和链接

```sh
novella compile kernel.nov \
  --target x86_64-unknown-none-elf --runtime none --code-model kernel \
  --linker ld --entry _start --linker-script kernel.ld \
  --link-object boot.o --link-object platform.o -o kernel.elf
```

裸机链接直接调用 ELF 链接器，默认选择 `ld.lld`，不隐式回退到 clang 或宿主链接链路。
本机只有 GNU ld 时显式写 `--linker ld`。生成最终映像必须指定入口和链接脚本；
`--emit obj` 不需要入口脚本，也不执行链接。最终链接不添加 CRT、libc 或 libatomic，
只使用程序目标文件、明确指定的附加对象、独立内存 support，以及实际需要的 managed
运行时归档。未解析符号、动态加载依赖及目标不匹配均不能作为成功映像交付。

项目清单可写：

```toml
[package]
name = "kernel"

[build]
target = "x86_64-unknown-none-elf"
runtime = "managed"
code-model = "kernel"
linker = "ld"
entry = "_start"
linker-script = "kernel.ld"
link-object = ["boot.o", "platform.o"]
```

CLI 优先于清单；合并后再应用最终目标的默认值。清单文件路径相对项目根，CLI 路径
相对当前目录。清单中的链接器裸命令名仍通过 PATH 查找，带目录的链接器路径相对项目根。
CLI 的非空 `--link-object` 列表整体覆盖清单列表。CLI 和 LSP 使用同一份目标配置解析，
条件编译的 OS、架构和 family 必须与实际目标一致；裸机为 `none`、`x86_64`、`none`。
旧 `analyze_with_target` 仍可独立进行 32 位静态分析，但 HIR 入口会拒绝它与当前两个
64 位发射目标的位宽冲突，避免 `usize` 按不同位宽分析和生成。
LSP 每次分析重新读取磁盘清单，配置错误直接产生诊断；VS Code 客户端监听 `nov.toml`
的创建、修改和删除并通知服务器刷新，其他 LSP 客户端可发送 `workspace/didChangeWatchedFiles`。

项目缓存保存目标字节与运行时需求，命中后仍执行链接，因此链接脚本和附加目标文件的
最新内容会生效。链接失败应保留已有输出；缓存不是已经链接好的内核映像。

## managed 平台契约

无 libc 的 ARC 核心要求平台提供：

```c
void *novella_platform_alloc(size_t size);
void novella_platform_free(void *pointer);
_Noreturn void novella_platform_panic(unsigned code);
```

分配失败返回空指针，成功分配的地址满足基本 C 类型对齐；runtime 自行处理对象的更高
对齐要求。free 接收对应 alloc 返回的原始地址。panic 必须终止当前执行路径，可以记录
错误、停机或交给内核 panic；稳定错误码定义见 runtime 的公共头文件。

`novella_rt_malloc/free` 是所有对象、控制块和编译器生成字符串缓冲的共同入口。
`novella_rt_set_allocator` 可在首次使用前替换分配回调；首次分配或释放非空地址后永久
固定，后续替换会 panic，避免存活对象被另一套分配器释放。平台回调自己负责线程安全；
ARC 原子计数不保护用户字段的并发读写，也不提供完整内核堆或调度器。

Linux provider 使用 libc 包装相同 hooks；裸机归档不包含该 provider。
`memcpy`、`memmove`、`memset`、`memcmp` 在独立 support 归档中，`none` 也可以使用。
编译器发现其他没有实现的隐式辅助函数时明确拒绝，不将符号留给不存在的宿主库。

## 可重复启动检查

参见 [启动示例](../examples/freestanding/README.md)。示例固定 Limine **12.9.6**，
高半区映像使用 Limine base revision **3**，协议布局固定到官方
`limine-protocol` 提交 `fedf97facd1c473ee8720f8dfd5a71d03490d928`。
低地址 `small` 映像使用同一引导器的 multiboot2 协议并自行切换长模式，
因为 Limine 原生协议不支持低半区内核。

验证脚本分别启动 none、managed、分配失败和 small 映像，检查串口与 QEMU debug-exit，
并保留 ELF 程序头、未定义符号表和反汇编。`none` 检查静态数据/指针；`managed` 检查
class、数组、字节字符串、聚合内 ARC、weak 存活/失效、64 字节对齐、析构和分配回收配对；
OOM 启动检查失败路径真正进入平台 panic。BSP 的固定块堆仅用于演示，不是生产内核分配器。

## 迁移

原 hosted 程序无须改入口。原来把 `@no_std` 当成“不依赖 libc”的程序需要选择实际裸机
目标、声明 `@no_main`、导出 C ABI 函数，并提供启动对象与链接脚本。含托管数据的内核
显式选择 `managed` 并实现三个平台 hooks；无托管数据则保留默认 `none`。
不要把旧的 Linux runtime 归档混入裸机映像。直接包含 runtime 公共头文件的 C 代码继续
通过函数 API 使用分配器，不再依赖头文件里定义的可写全局变量。

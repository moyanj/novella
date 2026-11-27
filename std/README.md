# 标准库

当前标准库含 `string.nov`、`io.nov`、`math.nov` 与默认环境 `site.nov`。`String` 是 UTF-16 class；
`site` 提供 `String` 名字及普通类型别名 `string`，用户本地声明优先于默认环境。
标准库身份由配置的 std 根目录及类型声明确定；同名用户 class 不触发字符串特殊降级。
`docs/stdlib-design.md` 中其他模块和 runtime 静态库仍是早期设计草案。

## `std:io`

```novella
from "std:io" import print, println;

func main() -> int {
    println("hello, 世界 🎉");
    return 0;
}
```

- `print(message)` 写出全部字节；`println(message)` 再追加一个 LF，空串只输出换行。
  两者都不显式 flush，行为与 glibc 的 stdout 缓冲一致。
- 编码走 `String.writeUtf8`：逐个 UTF-16 码元编码为 UTF-8，代理对合成为一个
  四字节序列，**孤立代理码元替换为 U+FFFD**，因此中文与 emoji 都按 UTF-8 原样输出。
- `null` 实参不写任何字节（`String` 是可空值，成员调用前有显式判空）。
- 缓冲区用 runtime 分配器，上界为 `length() * 3 + 4` 字节；分配失败调用 `abort`。
- 系统边界是 libc 的 `write(2)` / `abort`，因此 io 依赖宿主 libc：
  裸机目标不提供本模块。

- `length()` 返回 UTF-16 码元数，`byteLength()` 返回缓冲区字节数。
- `codeUnitAt(index)` 与只读下标 `value[index]` 返回 `u16`；越界终止。
- `charCount()` 将有效代理对计为一个字符，孤立代理码元计为一个。
- `slice(start, length)` 按码元索引，允许切开代理对，复制数据并返回新 `String`。
  `slice(length(), 0)` 合法；无效范围终止。
- `equals`、`compare`、`concat` 用 Novella 源码实现；比较按 UTF-16 码元顺序。
  `+`、`+=` 调用 `concat`，`==`/`!=` 比较内容；与 `null` 比较保留引用判空语义。
- 缓冲区依赖平台 `malloc` / `free`；越界、分配失败或长度溢出调用 `abort`。
  `concat` 在加法与乘法前检查长度上界，`slice` 从已校验的原串长度推得分配上界。
- `fromInteger` / `fromUnsigned` / `fromBoolean` 提供十进制或 `true`/`false` 文本。
  整数转换涵盖 `i64` 最小值与 `u64` 最大值。
- `fromFloat32` / `fromFloat64` 调用 C11 固定 ABI `strfromd`，分别使用 `%.9g` / `%.17g`
  有效数字格式，覆盖负零、无穷大和 NaN；格式受进程当前 C 数值 locale 影响。
  Linux glibc 目标需要 glibc 2.25 及以上提供该符号。
- `unitAt(index)` 按 `u16` 读码元，不做越界校验（调用方保证 `index < length()`）。
- `writeUtf8(out)` 把全部码元按 UTF-8 写入 `out`，返回字节数，语义与 `std:io` 的
  编码一致；调用方须提供 `length() * 3 + 4` 字节空间。私有 `encodeUtf8` 承担实体。

普通/raw 字面量在 HIR 中显式降级为 String 构造指令；LLVM 发射通过 runtime 对象分配器
创建带析构回调的 String，并通过系统 `malloc` 分配 UTF-16 缓冲区。
f-string 的插值按源码顺序各求值一次，接受 String、整数、boolean、f32/f64，以及提供
`__str(self) -> String` 的 class/struct。裸整数插值按 i64/u64 定型，裸浮点插值按 f64 定型；
f16 和其他无转换契约的值明确报错。文本里的 `{{` / `}}` 表示字面花括号。
字节串 `b"..."` 降级为内联定长数组 `u8[N]`，不依赖 String 或 ARC。可运行示例见
`examples/string-literal.nov`、`examples/string-ops.nov` 和 `examples/string-format.nov`。

通用对象 runtime 提供分配、原子引用计数、weak 控制块和析构回调，见
[runtime 说明](../crates/novella-runtime/README.md)。class 局部由自动 ARC 管理；
构造时先分配零初始化对象，再按父到子声明顺序计算字段初值，最后调用 `__init`，
默认构造参数在定义模块环境展开。析构接收者不能再创建或捕获自身强引用；
仅允许直接访问自身字段，字段对象的方法仍可调用。自身实例方法的调用或提取会报错，
因为当前没有借用效果分析来证明这些方法不会通过返回、传参或赋值间接复活自身。

入口写 `@no_std;` 可关闭整个导入闭包的标准库加载、默认环境及标准库链接。
此时普通/raw/f-string 字面量和 `std:` 导入会报错，字节串仍可原生编译。
该指令不改变平台启动代码和 C 链接环境，也不限制用户自行声明 `extern`。

可增长的 ARC 容器 `Vector<T>` 仍在规划中，当前标准库没有提供它；内建 `T[N]` 不管理数组本体的引用计数。

# 字符串标准库

当前标准库仅含 `string.nov` 与默认环境 `site.nov`。`String` 是 UTF-16 class；
`site` 提供 `String` 名字及普通类型别名 `string`，用户本地声明优先于默认环境。
这里描述当前源码；`docs/stdlib-design.md` 中其他模块和运行时静态库仍是早期设计草案。

- `length()` 返回 UTF-16 码元数，`byteLength()` 返回缓冲区字节数。
- `codeUnitAt(index)` 返回 `u16`；`charCount()` 将有效代理对计为一个字符，孤立代理码元计为一个。
- `slice(start, length)` 按码元索引，允许切开代理对，复制数据并返回新 `String`。
- `equals`、`compare`、`concat` 用 Novella 源码实现；比较按 UTF-16 码元顺序。
- 缓冲区只依赖平台 `malloc` / `free`，越界、分配失败或长度溢出调用 `abort`，
  不依赖 `novella_rt_*` 字符串函数或额外 runtime 静态库。

源码通过语义检查；class 对象分配、引用所有权、析构调用和 LLVM 字符串发射尚未接通，
因此目前不能把该库当作已可原生执行的 String 实现。sema 已将普通/raw/f-string 定型为
标准库 String，HIR 普通/raw 字符串载荷为 UTF-16；格式化串与字节串的原生降级仍待实现。

入口写 `@no_std;` 可关闭整个导入闭包的标准库加载、默认环境及标准库链接。
此时普通/raw/f-string 字面量和 `std:` 导入会报错，字节串仍能做静态检查。
该指令不改变平台启动代码和 C 链接环境，也不限制用户自行声明 `extern`。

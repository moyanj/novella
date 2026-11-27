# String、ARC 与 HIR/sema 实现交接

更新时间：2026-11-21

本文记录继续实现后的工作树状态。初始 String 原生程序的引用计数崩溃已修复，class ARC、weak、UTF-16 String、格式化串和字节数组已接通 sema → HIR → LLVM → runtime/std。全部改动保持未提交、未推送；保留接手前的工作树变更。

## 目标与边界

- `std:String` 是标准库中的动态 class，拥有 UTF-16 缓冲区；字符串算法尽量由 Novella 标准库实现，runtime 只提供通用对象与引用计数机制。
- 默认加载 `std:site`；入口 `@no_std;` 关闭 site 注入，并按 `std/modules.toml` 只允许 core / alloc 层标准库模块，不禁用通用 runtime 或显式 extern ABI。
- 标准库 String 通过 `SemaResult.string_type` 的模块与名义类型身份识别，不以 `string.nov` basename 判断。用户同名 class 不触发特殊降级。
- 本轮未实现异常展开、异步状态机、动态派发、捕获闭包、泛型单态化或完整标准库容器；相关路径明确报错。

## ARC 所有权契约

最初崩溃的根因是 runtime 在 strong 归零后调用用户析构，而析构接收者参数仍 retain 已归零对象。现在实例接收者借用，普通拥有形参由被调用方 retain 并在退出时 release。

- 构造结果、函数返回值和 weak upgrade 结果均携带一份强所有权。
- 普通局部读取产生快照；赋值先保留新值再释放旧值，支持自赋值。
- 返回拥有槽位时转移所有权；返回借用接收者时建立返回值强引用。
- 具名局部在覆盖或词法块退出时释放；匿名临时在完整表达式结束时由 `HirStmt::ReleaseTemps` 清理。
- if/loop 条件临时在条件求值后清理；break/continue/return 清理离开的作用域。C-for init 绑定仅存活到循环结束。
- struct/tuple 按值复制时递归 retain/release 拥有成员；数组本身是引用对象，最后释放时逆序清理元素。
- class 析构包装执行：用户析构 → 本类字段逆序清理 → 父类析构包装；父类字段不重复释放。
- 析构时对象不可复活。sema 拒绝把析构 `self`/`this` 复制为拥有值、普通实参或返回值，也禁止调用该接收者的实例方法，防止经 helper 间接逃逸；允许字段访问和字段对象的方法调用。

## weak 契约

字段保存独立控制块 handle。写入先 create 新 handle，再释放旧 handle；读取原子 upgrade，成功返回一份强引用，目标开始析构后返回 null。无需物理清零所有 weak 字段，也不会访问已释放载荷。

runtime ABI 为 `novella_rt_weak_create/retain/release/upgrade`，均接受 null。强计数归零后载荷同步析构释放，控制块保留到最后 weak；隐式 weak 保护析构内部释放自身 weak 的情况。upgrade 与最后强 release 的竞态受原子计数保护，并发修改同一字段仍需外部同步。

weak 字段不可取裸地址，避免把控制块 handle 当成载荷指针。读取 null 后的成员解引用仍需调用方判空。

## String、格式化与数组

- 普通/raw 字符串物化为 UTF-16 String；`+`、`+=`、内容比较和索引调用标准库方法。
- `length()` 与索引按 UTF-16 码元，索引结果为只读 `u16`；`charCount()` 统计代理对形成的码点；`slice()` 按码元切片，允许孤立代理码元。
- f-string 使用 sema 保存的已检查插值 AST，HIR 不重新解析插值。支持整数、boolean、f32/f64、String 和 class `__str`；不增加格式说明符。
- 浮点格式化由标准库调用 C11 `strfromd` 固定 ABI，f32 使用 9 位、f64 使用 17 位有效数字；当前原生目标是 Linux x86-64，依赖 libc 对该符号的支持。无浮点目标通过 `@cfg(target_has_float)` 整体裁掉该方法与 extern 声明。
- `b"..."` 为独立 `u8[]`，支持 no_std；未导入 `std:string` 时，no_std 的普通字符串字面量是只读 UTF-16 静态指针，不分配、不触发 ARC；导入后与常规模式一致。数组载荷包含长度与连续元素，尺寸/对齐由 LLVM TargetData 计算，索引越界 trap；裸指针索引仍遵循 unsafe 契约。
- class new 展开字段默认初值及构造默认实参，求值使用定义模块环境。

## 实现入口

- sema：`crates/novella-sema/src/api.rs`、`typecheck/expr/strings.rs`、`typecheck/call.rs`、`typecheck/unsafe_rules/assignment.rs`。
- HIR：`crates/novella-hir/src/body.rs`、`build/stmt.rs`、`build/strings.rs`、`build/call.rs`、`build/lvalue.rs`。
- LLVM：`crates/novella-codegen/src/body/` 按指令、控制流、所有权和数组分工；`src/ownership.rs` 实现递归聚合引用操作与 class 析构包装。
- runtime：`crates/novella-runtime/src/object.c`、`include/novella_rt.h`、`tests/weak.c`。
- 标准库：`std/string.nov`、`std/README.md`。
- 固定原生回归：`crates/novella-cli/tests/native_arc_string.rs`。

## 已运行验证

| 原生示例 | 退出码 | stdout |
| --- | --- | --- |
| string-literal | 8 | 空 |
| string-ops | 11 | 空 |
| string-format | 0 | 空 |
| arc-weak | 0 | ABCED |
| arc-lifetime | 0 | ABCDEFGH |
| arc-aggregate | 0 | A |
| array-arc | 0 | ABCEDGFJHI |
| runtime-weak | 42 | APCZ |

CLI 集成测试还验证 no_std 普通字符串字面量使用静态数据、不抽取 runtime 符号；用户同名 String 不误匹配、标量 no_std ELF 不抽取 runtime 符号。

runtime C ABI 集成测试覆盖 null、对齐、weak copy、归零后不能升级、析构中释放自身 weak，以及四线程 upgrade/最后 release 竞争。已运行 ASan/UBSan/TSan；ARC 覆盖、返回、循环、聚合、数组与继承析构的额外原生程序通过 ASan/泄漏检查。继承验证包含子类无用户析构的情况，父析构仅执行一次。

workspace check、最终 workspace test（110 项测试、28 suites）、fmt 检查、严格 clippy（-D warnings）与 git diff --check 均已通过。固定 CLI 集成回归覆盖八个原生示例，并拒绝析构直接/间接复活及 weak 字段取址。继承 __str 的原生格式化路径亦已验证。

开发期 HIR serde、SemaResult 投影与 weak 字段内部表示无迁移层，直接替换。不得混用旧版把 weak 存作载荷裸指针的 ABI。

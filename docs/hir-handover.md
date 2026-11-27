# novella-hir 交接文档

> 更新：2026-11-27。原生降级支持泛型单态化、静态 trait 调用与拥有型 `dyn`。
> 2026-11-15 验证记录保留为历史，当前实现及验证见文末修订。
> 本文记录 HIR 交接时的状态。后续 Linux x86-64 原生后端与 CLI 的现状见 [编译器设计](compiler-design.md) 第 8、9 节。

## 1. 已确认的架构与语义

- `novella-hir` 只依赖 core、ast、sema；开发依赖 serde_json 仅用于冒烟输出，不引用 LLVM / inkwell。
- `HirBlock` → `HirStmt` → `HirInstr` 表示控制流与顺序指令，不保留待求值表达式树。
- `build_hir(&SemaResult)` 遇到词法、语法、语义或加载错误即返回 `BuildError`，不跳过坏模块、不返回残缺程序。
- `AnalyzedModule.required_for_codegen` 标记入口经显式导入、名字及类型依赖可达的模块。
  默认 `std:site` 环境中未使用的模块仍执行完整 sema 检查，但不降级、不链接；
  显式导入即保留其模块，不因符号未使用而跳过。入口 `@no_std;` 只允许 core / alloc 层
  标准库模块参与加载，`@no_alloc;`（隐含 `@no_std;`）只允许 core 层。
- 字符串名义身份来自 `SemaResult.string_type`，不以文件 basename 推断；`HirInstr::StringLiteral`
  保存解码后的 UTF-16 码元。构造、内容运算、索引、格式化及原生发射已接通。
- 类型读取 sema 的 `expression_types`、`annotation_types`、`declaration_types`，键均为 `(ModuleId, Span)`。分别提供表达式、已解析注解、变量最终绑定类型与顶层函数签名。HIR 不重新解析类型名或用初始化表达式冒充声明类型。
- Unknown / Error、联合、未完成单态化的类型以及无上下文的未定型类型明确拒绝；字面量可由声明、返回值、实参等上下文定型，数组与元组递归传递期望类型。
- 用户确认：`p ?? backup` 非空时返回指针 `p`，为空才执行并返回 `backup`；不隐式解引用，读取内容另写 `*`。sema 已同步保留指针结果并检查备用值类型。
- codegen 只消费 HIR，不直连 AST。String 为拥有 UTF-16 缓冲区的 class；定长数组类型携带长度，LLVM 布局为 `[N x T]`，没有运行时头部或分配。

## 2. P1 已完成

| 范围 | 降级约定 |
| --- | --- |
| 标量、一元/二元、转换 | 常量按上下文定型；Binary 携带 `lhs_ty` / `rhs_ty` / `dst_ty`，比较不再从 bool 结果反推输入宽度；TypeTest 携带输入类型 |
| 指针 | 地址与间接读写、指针 ± 整数、整数 + 指针、指针差；保留两侧类型，指针差使用 PtrDiff |
| 短路 | and/or、三元、`??` 用分支及明确分配的结果槽位，备用分支仅按条件执行 |
| 身份比较 | 标量、指针、class 的 `===` / `!==` 映射 Eq / Ne；聚合与 string 身份比较明确拒绝 |
| 左值与副作用 | 普通赋值先 RHS、后定位 LHS；复合赋值定位一次、在 RHS 前读取旧值；自增自减返回旧值；局部读取生成快照，避免后续实参修改先前读值 |
| 元组、数组、解构 | Tuple / Array / TupleExtract；递归嵌套解构，初始化只求一次；内建数组索引用 IndexAddress 及间接读写，自定义下标降级为方法调用 |
| 循环 | while / C-for / 数组 for-in 统一为 Loop；条件 prelude 每轮执行；step 是 Loop 的字段，正常尾部和 continue 都执行，break / return 跳过 |
| with | 接收者只求一次；绑定位于块作用域内，支持注解与递归元组解构；不添加资源进入/退出钩子 |
| 调用 | 函数、静态方法、函数值及 struct 直接实例调用；显式传接收者；默认实参在定义模块环境按序展开，能引用此前参数快照；sema 调用检查排除隐式 self |
| struct | self 槽位为 Pointer(Struct)，修改作用于原实例；显式字段按源码顺序求值，缺省字段按声明顺序在定义环境求值；支持字段简写与导入别名 |
| 继承布局 | 跨模块沿父链由父到子展开；类型定义与字段访问共用同一布局来源；不吞掉字段类型错误 |
| match | subject 一次；case 模式按源码顺序、仅未命中时求下一个；分支作用域独立；值 match 必须有 default；全块分支为 Void |
| lambda | 无捕获函数降级为独立项，实体键为 `m{module}::lambda#{span.start}`；同形函数不撞名，重复默认表达式展开复用同一实体并归属定义模块；捕获明确拒绝 |
| 外部声明 | extern 产出 HirItem::ExternFunction，HirFunction.link_name 保存真实 C 符号；内部查找仍用名义键；LLVM 支持标量/指针 C ABI 与传出函数地址作为回调 |
| 字符串 | 普通单双引号与 r/R 原始字符串；普通字符串解析换行、回车、制表、空字符、引号和反斜杠转义；未知转义明确拒绝 |

## 3. 泛型与 trait 的原生契约

- 以声明身份和类型实参区分实例；在降级体之前登记实例以支持递归。
  HIR 在选择函数 ABI 与字段布局前替换具名类型参数，并按实现归约关联类型。
  未使用的泛型模板不发射；无限生成新实例的程序达到工程上限时报错。
- 约束调用从已验证实现选择具体方法；泛型默认方法体在对应的 `Self`、trait 实参和
  方法实参环境下实例化。trait 出现在约束中不需要运行时见证参数。
- `DynPack` 保留源类型和按 trait 方法名排序的见证槽；`DynCall` 携带已解析签名、
  槽号和动态接收者。LLVM 以 `{数据指针, 表指针}` 保存动态值，只对数据指针执行 ARC。
  class 保留对象；struct 装箱的副本递归保留管理成员，最后由生成的析构函数释放。
  需启用分配与 ARC 的目标才能使用该表示。

## 4. 显式拒绝及后续期

以下内容不静默跳过，也不伪装为普通求值或 return：

| 构造 | 原因 / 后续工作 |
| --- | --- |
| try/catch/finally/throw | 异常传播与清理协议 |
| async/await/yield | 状态机、挂起与恢复协议 |
| class 动态派发、绑定方法值 | 直接实例调用与 new 已接通，动态方法表及绑定接收者表示尚未实现 |
| 编译器指令及指令块 | 尚无可消费的修饰结果；类型、字段、方法编译器指令同样拒绝 |
| 闭包捕获 | 环境布局与生命周期 |
| 联合及不透明类型 | 无具体可生成的值布局 |
| 动态对象字面量 | 尚无定型对象布局 |
| 非数组 for-in | Sema 已检查 `iter()`、`hasNext()` 和 `next()`，HIR 尚未降级 |
| 静态字段 | 静态存储 |
| 未证明穷尽的无 default 值 match、值/void 混合 | sema 拒绝；合法布尔穷尽 match 可由 exhaustive_matches 供 HIR 降级 |
| 未定义转义、聚合身份比较 | 明确拒绝；格式化串、字节串及 class String 的身份比较已接通 |
| 无实体声明的间接函数默认实参 | 函数值签名不携带默认表达式定义 |

class 的普通调用仍按静态类型选择方法；只有 `dyn` 值通过见证表派发。String 的拼接、内容比较和索引调用标准库方法，身份比较使用引用身份。

## 5. 实现位置

| 文件 | 职责 |
| --- | --- |
| build.rs | 模块声明、共享构造状态、公开重导出 |
| build/module.rs、error.rs | 入口、声明、错误与拒绝边界、lambda 实体归属 |
| build/types.rs、layout.rs | sema 注解投影、类型/方法、跨模块实例布局 |
| build/mono.rs | 具体实例登记与工作队列、代入、关联类型归约及见证方法选择 |
| build/dynamic.rs | 打包 `dyn` 值、稳定见证槽位 |
| build/context.rs | 槽位、作用域、定义环境与类型查询 |
| build/stmt.rs、loops.rs | 语句、with、解构及循环 |
| build/expr.rs、operators.rs、coalesce.rs | 表达式分派、运算与短路 |
| build/lvalue.rs | 一次定位、地址及间接读写 |
| build/call.rs、build/call/target.rs | 接收者、实参定型与默认实参展开；直接 / 动态调用目标选择 |
| build/aggregate.rs、helpers.rs | struct、match、lambda、常量与转换助手 |
| ty.rs `HirType::same_value_type` | 值类型比较：忽略函数实体名，供 HIR 与 codegen 判定槽位/实参等价 |
| ty.rs、body.rs、item.rs | 类型与签名、指令与控制流、模块与类型定义 |

## 6. P2 验证

```bash
cargo run -q -p novella -- compile examples/native-main.nov --emit hir -o target/native-main.hir.json
# 对任意磁盘文件导出 HIR；失败以非零状态退出，不输出部分 HIR
cargo run -q -p novella -- compile path/to/main.nov --emit hir -o target/main.hir.json
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --all-targets
git diff --check
```

- 当时扩展 hir_smoke 验证了标量、条件、while/C-for/数组 for-in、嵌套解构、方法、默认实参、match、lambda、带副作用索引和空合并，未新增测试 suite。该重复调试入口现已移除，当前统一通过 CLI 的 `--emit hir` 导出。
- 临时 HIR 解释器执行验证：sum(10)=45，coverage()=26，shortcircuit()=24（非空不执行备用表达式、空时执行一次），matching()=110（后续模式无副作用）；同形 lambda 的实体键唯一。额外验证数组内元组的 for-in 解构。
- 临时跨模块 fixture：导入别名、默认参数、字段简写/默认值、可变 struct 接收者，cross()=23；父子布局为 x、y；extern 项存在，指针算术类型与 PtrDiff 结构正确。重复展开同一跨模块默认 lambda 的实体归属与唯一性另外核对。
- 11 项拒绝路径：语法/词法错误、错误备用类型、闭包捕获、编译器指令、格式化串、throw、对象字面量、trait 成员、静态字段、无 default 值 match 均非零退出且无部分 JSON。
- workspace 现有 102 个测试（24 suites）通过；fmt、clippy（-D warnings）、all-targets check 与 diff 检查通过。

解释器验证针对 HIR 求值与控制流，不代表 LLVM、机器码、内存 ABI 或引用计数已验证。临时 fixture/解释器位于 /tmp，不是项目新增测试。

### 2026-11-27 泛型 / trait / dyn 原生验收

- `examples/generics.nov`：泛型 struct + 泛型方法（接受函数值）、泛型 class 继承
  `Base<T>`、泛型 `const`、递归泛型调用；原生退出 0。
- `examples/traits.nov`：struct / class 显式实现、trait 默认方法与宿主覆写、
  `T: A + B` 组合约束、关联类型投影返回；原生退出 0。
- `examples/dyn-traits.nov`：隐式与显式 `as dyn` 转换、struct 装箱拷贝与 class 保对象、
  同载荷 ARC 共享、默认方法经见证表派发；原生退出 0。
- 函数值经泛型实例化后调用（`combine(value, value)`）、具名函数与 lambda 互赋的
  名义 id 差异，均以 `HirType::same_value_type` 对齐 HIR 与 codegen 的槽位判等。
- 负例 `traits_bad.nov`（非对象安全 / 重复绑定 / 歧义）、`traits_overlap.nov`
  （实现重叠 + 签名不符）、`inline-cycle.nov`（无限值布局）均按预期拒绝；
  `projection_diagnostic.nov` 的下划线落在投影操作数本身。

## 6. 交付与迁移

保持未提交、未推送；未接入 inkwell，未修改 CLI 输出行为。开发期 HIR 无既有下游消费者，直接替换：删除冗余 Operand::Cond 和 LoopStep；Loop 增加 step；Binary 增加输入类型；TypeTest 增加输入类型；新增地址、元组和数组指令；SemaResult 新增两份类型投影表。后端实现必须按这些字段执行，不能把 continue 直接跳至条件从而漏掉 step。

## 2026-11-15 补全同步

- 普通赋值的双方副作用实测为 value() → index()；复合赋值为 index() → value()，
  地址只求一次，旧值读取仍在 RHS 前。详见 sema-coverage.md 本轮验收记录。
- IntConst 与 UntypedInt 改为 i128，JSON 中 18446744073709551615 保持精确；
  消费 JSON 时不要经 JavaScript Number 再序列化该数。
- 泛型实例、Future 与匿名结构对象保留公开类型身份，HIR 暂无对应布局/运行时则
  带位置拒绝。b/f 字符串现已接通；Exception 静态根不代表标准库/异常展开已实现。
- 新增 AST type_args/name_span 与可选 Throw.expr，开发期直接替换旧契约，无迁移层。

## 2026-11-15 声明与诊断契约修订

全部诊断唯一存于 DiagnosticReport，HIR 入口通过完整 has_errors 拒绝错误输入。
TypeView::Error 与 Unknown 分离，std 不再产生 External。
默认实参与字段初值通过 DeclarationIndex 和分析内 DeclarationId 定位，保留定义模块环境；
不再遍历全部 AST 或解释函数 id 字符串。HirFunction.source_module/is_lambda 显式记录 lambda 属主。
已求值 @cfg 不进入 HIR。

这些公开 API/serde 改动无迁移，直接替换；所有定义身份仅对本次分析快照有效。

## 2026-11-21 ARC、weak 与 String 原生修订

- new 支持字段默认初值和构造默认实参；直接实例调用借用接收者。
- HirStmt::ReleaseTemps 标记完整表达式结束；结构化块及循环跳转清理拥有值，C-for init 独立成块。
- class 使用强计数，定长数组、struct 和 tuple 复制递归维护拥有成员。weak 保存控制块；FieldLoad upgrade，FieldStore 先创建新 handle 再释放旧 handle，禁止 weak 取址。完整表达式和词法作用域退出时从活动所有权集合移除已释放的槽位；独占初始化结果可用 `Adopt` 移交给具名槽。
- 析构顺序为用户析构、本类字段逆序、父类包装；析构 self 不得逃逸，也不得调用自身实例方法以防间接复活。
- f-string 消费 sema 保存的已检查插值 AST，不重新解析。普通/raw 为 UTF-16 String，byte 为 u8[]；String 索引只读返回 u16，内容运算调用 std 方法。
- 原生验证及限制见 [String/ARC 交接](ai-handover-string-arc.md)；早期验证数字与拒绝清单是历史记录，以本次修订为准。

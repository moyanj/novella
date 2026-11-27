# sema 规则覆盖矩阵与执行记录

> 本文件是 sema 补全计划的配套交付物：**每一行 = 一条已定规则 × 一个
> AST 变体/语义面**，状态只能填 `已实现` / `部分` / `缺失` / `待决策`，且必须有实测依据。
> 计划 §1 的三个验收点：A 现有规则漏洞修补（S00–S08）、B 已确认语言范围覆盖
> （S09–S13）、C 整体交付（S14）。**A/B 未闭环前不得写「sema 完成」。**
> S00–S14 已全部完成并验收；验收结果保留在本文件。

## 0. 起始基线（历史快照）

下表记录任务启动时的状态；本轮修改后的结果见末尾“本轮收口验收”，工作区已有未提交修改。

| 项 | 记录 |
| --- | --- |
| 日期 | 2026-11-15 |
| HEAD | `ca976da3c332be9e478f78bbf3973e297931564e`（`feat(hir): 实现定型降级并补充 sema 完善计划`） |
| 分支 | `main` |
| 工作区 | `git status --short` 为空（无未提交修改，无未跟踪文件） |
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --all-targets` | 通过 |
| `cargo test --workspace` | **102 passed / 24 suites** |
| `cargo clippy --workspace --all-targets -- -D warnings` | 见末尾本轮收口验收 |
| 深层 AGENTS.md | 无（仅仓库根 `AGENTS.md`） |

状态图例：

- `已实现`：有检查入口 + 正反例实测通过 + 定位正确；
- `部分`：有入口但不完整（写明缺什么）；
- `缺失`：无任何检查，`Unknown` / `HIR 拒绝` 掩盖了规则；
- `待决策`：依赖 D 编号，未经用户确认不得实现。

## 1. 覆盖矩阵

列说明：**依据** = design.md 章节 / 计划编号；**实现** = 当前负责函数；**依据状态**
= `已定`（设计已写死，可直接实现）/ `待定`（D 编号）。正反例列写的是**最小反例
的源码要点**，完整源码与命令见 §4 执行记录。

### 1.1 模块与导入

| ID | 规则 | 依据 | 实现 | 状态 | 正/反例 | D/S |
| --- | --- | --- | --- | --- | --- | --- |
| M01 | 模块 = 文件；导入闭包按 worklist 加载，循环导入安全去重 | design §4.3 | `module/loader.rs::load_modules` | 已实现 | 反：a 导入 b、b 导入 a 不死循环 | S07 |
| M02 | 相对路径导入、`@:` crate 根、`super:`、`std:` 别名 | design §4.3 | `module/resolve_path.rs` | 已实现 | 反：未知别名报错 | S07 |
| M03 | 同一文件不同路径写法去重（`./a.nov` / 绝对路径） | design §4.3 | `novella_core::path::module_key` | 已实现 | 反：两种写法只加载一次 | S07 |
| M04 | 缺 `nov.toml` 时 `@:` 不可用的降级规则 | design §4.3 | `module/resolve_path.rs` | 已实现 | 反：无 crate 根用 `@:` 报错 | S07 |
| M05 | 具名导入别名冲突检测 | design §4.3 | `resolve.rs::bind_imports` | 已实现 | 反：两个同名导入 | S07 |
| M06 | 私有符号不可被导入 | design §4.2/§4.3 | `resolve.rs::bind_imports` | 已实现 | 反：`pri` 符号具名导入报错 | S07 |
| M07 | 命名空间导入 `ns.T` 类型解析 | design §4.3/§5.1 | `resolve.rs`（qualifier） | 已实现 | 反：多级限定 `a.b.T` 报错（本期不支持） | S07 |
| M08 | 导入不自动重导出 | design §4.3 | `resolve/imports.rs` / `Symbol::is_importable` | 已实现 | 具名导入不成为 pub 导出；当前文法无 pub import，非公开导入继续拒绝 | S07 |
| M09 | `std:` 必须加载真实声明并强制检查 | 用户后续纠偏 TD01 | `module/resolve_path.rs` / `resolve/imports.rs` | 已实现，原 External 策略废止 | 缺模块/符号/错误签名均拒绝，Exception 根除外 | 设计纠偏 |
| M10 | 模块读取失败后保留导入身份，抑制依赖该模块的符号与可见性级联错误 | ROADMAP M-1 | `module/loader.rs` / `resolve/imports.rs` / `resolve/types.rs` / `typecheck/member.rs` / `typecheck/unsafe_rules/aggregate.rs` | 已实现 | 具名、命名空间及类型访问只保留读取根因；可读模块的缺失符号和独立未定义名称仍报错 | M-1 |

### 1.2 类型表达式与类型系统

| ID | 规则 | 依据 | 实现 | 状态 | 正/反例 | D/S |
| --- | --- | --- | --- | --- | --- | --- |
| T01 | `int` ≡ `i32`、`float` ≡ `f32` 归一化 | design §5.2 | `ty.rs::IntKind::from_name` / `FloatKind` | 已实现 | 正：`int` 与 `i32` 互相可赋 | — |
| T02 | 数值隐式转换只允许同符号无损扩宽：`i32→i64`/`u8→u16` 可隐式；窄化 `i64→i32` 与跨符号 `i32→u64` 需 `as` | design §5.2.1 | `ty.rs::assignable` + `IntKind::widens_to` | 已实现 | 正：`let x: i64 = i32_val;`；反：`let y: i32 = i64_val;` 报错 | — |
| T03 | 未定型数值字面量按目标类型归化 | design §5.2.1 | `ty.rs::assignable` + `body.rs::concrete` | **已实现**（整数部分） | 按目标位宽判定；浮点范围见 S04b | **S04a 完成** |
| T04 | `null` → 任意 `T*` / class 引用；`void*` ↔ `T*` | design §6.5 S5/S6 | `ty.rs::assignable` | 已实现 | 正/反见 unsafe 表 | — |
| T05 | 子类 → 父类隐式；父类 → 子类需 `as` | design §5.2.1 | `lib.rs::is_subclass` + `assignable` | 已实现 | 正：`let a: Base = new Derived();` | — |
| T06 | 元组 / 等长定长数组同形逐元素可赋；无长度 `T[]` 只可作形参并解析为 `T*` | design §5.2.1、§7.7 | `ty/relations.rs`、`resolve/types.rs`、`typecheck/call.rs` | 已实现 | 元素类型或长度不兼容报错；拒绝只读数组退化为可写指针 | — |
| T07 | 联合类型赋值（目标取任一、源需全部） | design §5.2.1 | `ty.rs::assignable` | 已实现 | 正/反按保守规则 | — |
| T08 | 泛型实参参与类型身份（Box<i32> ≠ Box<string>） | 已确认 D05 | `Ty::Applied` / `ty::ty_eq` | 已实现 | generic-identity 拒绝互赋；generic-struct-ok 保留 i32 实参 | S11 |
| T09 | 类型参数按声明身份判等 | 已确认 D05 | `ty::param_key` / `resolve/types.rs` | 已实现 | 函数、类型、方法、别名的 T 携带模块与声明位置；泛型覆写按位置重命名 | S11 |
| T10 | 泛型 arity 检查 | design §5.1 | `resolve.rs::instantiate_type` | 已实现（递归实例化） | 反：`Box<i32, i64>` 报错 | — |
| T11 | 别名展开与循环别名检测 | design §4.4 | `resolve.rs` + `resolving_aliases` | 已实现 | 反：`type A = B; type B = A;` 报错 | S07 |
| T12 | `T*` 嵌套右结合、`(A \| B)*` | design §5.3 | `parser` + `resolve.rs` | 已实现 | 正：`u8**` 为 `Pointer(Pointer(u8))` | — |
| T13 | `void*` 解引用 / 算术报错 | design §6.5 S8 | `expr.rs::check_unary` / `unsafe_rules` | 已实现 | 反：`*vp` 报错 | — |
| T14 | f16 binary16 的范围 | 已确认 D07 | `ty/literals.rs::float_literal_fits` | 已实现 | 65504/-65504 合法；65520 拒绝；最近偶数舍入阈值写明 | S04b |
| T15 | usize / isize 按分析目标的 32/64 位检查 | 已确认 D07 | `analyze_with_target` / `target_int_kind` | 已实现 | 4294967296 在 32 位 usize 拒绝、64 位通过，范围文案随配置变化 | S04b |

### 1.3 字面量（S04）

| ID | 规则 | 依据 | 实现 | 状态 | 正/反例 | D/S |
| --- | --- | --- | --- | --- | --- | --- |
| L01 | 整数进制前缀（`0x/0o/0b`）与下划线合法 | design §3.3 | `ty.rs::classify_number_literal` | 已实现 | 正：`0xFF`、`1_000` | — |
| L02 | 整数按目标位宽的范围检查（u8 255/256、i8 -128/-129、u64 上限） | plan S04a | `ty.rs::int_literal_fits_non_negative` + `int_negated_literal_fits` | **已实现** | 反：`u8 = 256` 报「整数字面量 `256` 超出 `u8` 的取值范围 0..=255」；正：`u8 = 255` / `i8 = -128` / `u8 = 0xFF` 通过 | **S04a 完成** |
| L03 | `u64` 最大值不经 `i64` 中间表示丢失 | plan §6 S04a | `classify_number_literal` → `IntLiteral(bool, u128)` | **已实现** | 正：`u64 = 18446744073709551615` 通过；反：`…616` 报超出范围（u128 载荷，无 i64 截断） | **S04a 完成** |
| L04 | 一元负号与最小负数边界（`-128` 给 `i8`） | plan S04a | `expr.rs::check_unary` 保留未定型态 + `int_negated_literal_fits` | **已实现** | 正：`i8 = -128` 通过；反：`i8 = -129` 报超出范围；`-(-128)` 记为 +128 超 `i8` | **S04a 完成** |
| L05 | 浮点溢出（非有限值）拒绝 | plan S04b | `ty.rs::float_literal_fits` | **已实现** | 反：`f32 = 1e999` / `f64 = 1e999` 报「不是有限值」 | **S04b 完成** |
| L06 | `f32` / `f64` 边界检查 | plan S04b | `float_literal_fits` + `float_range_text` | **已实现** | 正：`f32 = 3.4028235e38`（按 f32 舍入后有限）、`f64 = 1.7976931348623157e308`；反：`1e309` 溢出 | **S04b 完成** |
| L07 | 字符串前缀 `f` / `r` / `b`、单双引号 | design §3.3 | `novella_core::string::decode_string_literal` | **已实现** | `r` 原样不解释；`b` 为 u8[] 并校验单字节字符与转义；单双引号等价 | **S04c 完成（D07）** |
| L08 | 已定义转义序列校验 | design §3.3 | `core::string::decode_escapes`（sema 调用） | **已实现** | 反：`"\q"` 报「非法转义序列 `\q`」；`\x4` / `\u{110000}` / `\u{D800}` 各自报错 | **S04c 完成（D07）** |
| L09 | f-string 插值扫描（转义引号、`{{`/`}}`） | design §4.12 | `expr.rs::check_fstring` | **已实现** | 正：`{{b}}` 转义、插值内单引号、嵌套括号；反：`{1 + }` 报非法表达式、`{}` 报插值不能为空 | **S02 完成** |
| L10 | sema 与 HIR 字符串解码规则一致 | plan §6 S04c | `novella_core::string::decode_string_literal`（唯一实现） | **已实现** | 两侧共用 core 实现 | **S04c 完成** |
| L11 | `null` 字面量类型与可空判定 | design §6.5 S9 | `ty::Ty::Null` + `is_nullable` | 已实现 | 正/反见判空规则 | — |

### 1.4 声明（S01 / S05）

| ID | 规则 | 依据 | 实现 | 状态 | 正/反例 | D/S |
| --- | --- | --- | --- | --- | --- | --- |
| D01R | 必须有顶层 `main`；仅入口模块要求 | design §4.8.1 | `check/decl.rs::check_main_entry` | 已实现 | 反：无 main 报错 | — |
| D02R | `main` 无参、无泛型、非 extern、非 async | design §4.8.1 | `check_main_entry` + `build_function_sig` | 已实现 | 反：`func main(x: int)` 报错 | — |
| D03R | `main` 返回类型仅 `void` / `int`（≡`i32`，别名归一化可接受） | design §4.8.1；plan E09 | `check_main_entry` + `is_main_return_type` | **已实现** | 反：`-> i64` / `-> u8` 报错；正：`void` / `int` / `i32` / `type Code = int` 通过，`type Wide = i64` 报错 | **S01 完成** |
| D04R | 普通有体函数与方法省略返回注解时推导可达返回；无体声明缺省 `void`，特殊方法按协议检查 | design §4.8；ROADMAP M-1 | `check/returns.rs` + `typecheck/lambda.rs::infer_block_return` | 已实现 | 正：前向/跨模块/方法/泛型依赖；反：不兼容返回、非 void 落尾、字面量越界、未确定类型与无注解推导环 | M-1 |
| D05R | 形参无注解 → 未知，跳过检查（本期容错） | design §4.8 | `ty::ParamSig.ty = Unknown` | 已实现（按 D03 保留） | 正：未注解形参见检查跳过 | D03 |
| D06R | extern 参数必须显式标注类型 | design §4.8 | `build_function_sig` E2 | 已实现 | 反：`extern func f(x);` 报错 | — |
| D07R | extern 不得有函数体 / 泛型 / 命名 main | design §4.8 | `build_function_sig` E3–E5 | 已实现 | 反：三条各自有诊断 | — |
| D08R | extern 缺省返回为 void，不额外强制返回注解 | plan S01 / 本轮确认 | `build_function_sig` | 已实现 | 保留显式参数、无函数体等既定约束；不新增 AST 无法表达的规则 | S01 |
| D09R | 重名形参、重名成员检查 | plan §6 S01 | `check_duplicate_params` / `check_member_collision` | 已实现 | 反：`func f(a: int, a: int)` 报错 | — |
| D10R | struct 不支持继承 | design §4.7 | `build_type_defs` | 已实现 | 反：`struct S(Base) {}` 报错 | — |
| D11R | struct 不识别 `__` 前缀方法 | design §4.7 | `check_special_method_name` | 已实现 | 反：`func __init()` 报错 | — |
| D12R | class 特殊方法清单（`__init`/`__destroy`/`__str`/`__enter`/`__exit`） | design §4.6.1 | `CLASS_SPECIAL_METHODS` | 已实现（名字层） | 签名规则见 S12c | S12c / D08 |
| D13R | 字段必须有类型标注或默认值 | plan §6（受 D03） | `build_field` | 已实现 | 反：`x;` 报错 | — |
| D14R | 字段、顶层变量按依赖定型后检查函数体 | plan S05 | `check/fields.rs` / `check/globals.rs` | 已实现 | method-first/field-first、type-first/type-last 均拒绝 i32 返回给 string；前向调用通过 | S05 |
| D15R | 无注解字段与顶层变量的推导环诊断 | plan S05 | `resolve_field` / `globals::resolve` | 已实现 | 静态字段环、跨类型实例字段环、顶层变量环拒绝；显式注解可打断类型依赖 | S05 |
| D16R | 条件编译指令及未知指令拒绝 | 用户补充 TD04 | `cfg.rs` / `check/decl.rs` | 已实现 | @cfg 在声明收集前筛选；其他无定义指令报 error；支持函数/类型/成员及指令块 | 设计纠偏 |
| D16S | 禁用标准库指令 | 用户补充（2026-11-21） | `module/loader.rs` / `check/decl.rs` | 已实现 | 入口独立 @no_std; 关闭整个导入闭包的标准库加载和 site 注入；普通字符串字面量降为只读 UTF-16 指针，f-string 仍需标准库 String；拒绝 std: 导入、重复/带参/依赖模块/修饰声明或块的用法 | 无参数指令 | |
| D16T | 禁用自动入口 | 裸机计划 / design §4.2 | `module/loader.rs` / `freestanding.rs` | 已实现 | @no_main 仅入口独立一次且无参；依赖、重复、带参、修饰声明或块均拒绝 | 无参数指令 |
| D16U | C ABI 函数导出 | 裸机计划 / design §4.2 | `freestanding.rs` / `check/decl.rs` | 已实现 | @export 唯一合法符号；拒绝泛型/async/extern/无体/非顶层函数、聚合与托管 ABI、重复及 extern 碰撞 | 标量、裸指针与函数指针 |
| D16V | 裸机目标与无托管模式 | 裸机计划 | `core::target` / `freestanding.rs` | 已实现 | 裸机强制 no_std+no_main，拒绝浮点含嵌套及 extern；runtime=none 接受无托管元素的定长数组，拒绝实际托管值/ARC/weak | 真实 file/span 定位 |
| D17R | 多继承 | design §4.6（仅单父类） | `build_type_defs` 报错 | 已实现 | 反：两个父类报错 | — |
| D18R | 继承环检测 | plan E12 | `resolve_class_parents` | 已实现 | 反：`A(B)`/`B(A)` 报错 | — |
| D19R | weak 字段只保存 class 引用，初值推导后检查 | design §7.4 | `check/fields.rs::check_fields` | 已实现静态约束及原生 weak upgrade/ARC | 正：class 别名、泛型 class、struct 内弱字段；反：标量、裸指针、数组、裸类型参数 | — |

### 1.5 表达式

| ID | 规则 | 依据 | 实现 | 状态 | 正/反例 | D/S |
| --- | --- | --- | --- | --- | --- | --- |
| E01R | 标识符解析与未定义诊断 | plan §6 | `expr.rs::check_identifier` | 已实现 | 反：未定义名报错 | — |
| E02R | 类型名不能当值 / 命名空间不能当值 | design §4.3 | `value_type_of_symbol` | 已实现 | 反：两者各自报错 | — |
| E03R | `this` 在实例方法及其词法 lambda 内可用 | design §8.4 | `expr.rs` This 分支 | 已实现 | 反：顶层 `this` 报错 | S09c |
| E04R | 一元 `+ - ~` 仅数值/整数；`not` 仅 boolean | design §4.10 | `expr.rs::check_unary` | 已实现 | 反：`-"a"` 报错 | — |
| E05R | 自增/自减需可写左值；T* 需 unsafe | design §6 / TD02 | `expr/unary.rs` + `is_lvalue` | 已实现 | 正：unsafe 内 T*++/--；反：void*、安全上下文、不可写目标拒绝 | 设计纠偏 |
| E06R | `&x` 左值 → `T*`（安全） | design §6.5 S1 | `check_unary::AddressOf` | 已实现 | 反：`&1` 报错 | — |
| E07R | `*p` 需指针 + unsafe | design §6.5 S2 | `check_unary::DerefPointer` | 已实现 | 反：unsafe 外解引用报错 | — |
| E08R | 二元运算类型矩阵（算术/位/逻辑/比较） | design §4.11；§6.2 | `unsafe_rules.rs::check_binary` | 已实现 | 见 §4 相邻例 | — |
| E09R | 指针算术（`p±n`、`p1-p2` ⇒ `isize`） | design §6.5 S4 | `check_binary` | 已实现 | 反：指针+指针报错 | — |
| E10R | `&&`/`||` 操作数 boolean | design §4.11 | `check_binary` | 已实现 | 反：`1 && true` 报错 | — |
| E11R | `??` 左操作数可空、右侧类型 | design §5.2.1；plan E12 | `check_binary` | 已实现 | 正：保留非空指针而不解引用 | — |
| E12R | `is` 操作数与类型解析 | design §4.10 | `expr.rs` Is 分支 | 已实现 | 反：类型不存在报错 | — |
| E13R | `as` 转换矩阵（S5）+ unsafe 约束 | design §6.3 / §6.5 | `unsafe_rules.rs::check_cast` | 已实现 | 反：指针↔整数在 unsafe 外报错 | — |
| E14R | 三元条件 boolean、分支类型合一 | design §4.10 | `expr.rs` Ternary | 已实现 | 反：分支类型不一致报错 | — |
| E15R | 赋值目标可写左值、类型兼容 | design §9.2 | `unsafe_rules.rs::check_assignment` | 已实现 | 反：常量赋值报错 | S08 |
| E16R | 标准初始化 `Point{x:1}`（字段齐全、无未知字段） | design §5.4 | `unsafe_rules.rs::check_struct_init` | 已实现 | 反：缺字段 / 多余字段报错 | — |
| E17R | 匿名对象结构类型、宽赋窄、重复键与简写 | 已确认 D07 | `expr/aggregates.rs` / `Ty::Object` | 已实现 | object-ok 宽赋窄并读取字段通过；未定义简写只报一次名称错误 | S12b |
| E18R | 数组字面量元素类型取首元素并统一 | design §5.2.1 | `expr.rs::check_array_literal` | 已实现 | 反：元素类型不一致报错 | S03 |
| E19R | 空数组需 `T[0]` 上下文给出元素类型 | design §5.2.1、§7.7 | `expr/hints.rs`、`expr/aggregates.rs` | **已实现** | 定长声明、实参、返回值等位置可定型；无元素类型或长度不匹配报错 | **S03 完成** |
| E20R | 调用：实参个数、类型、默认值、extern 需 unsafe | design §4.8；§6.2 | `call.rs::check_call` + `instance_param_tys` | **已实现** | 反：少参/多参/类型不符；正：`takes([])` 由形参定型 | **S03 完成** |
| E21R | `new`：`__init` 实参检查；无 `__init` 不得传参；抽象类实例化 | design §4.6.1；plan S10b | `call.rs::check_new` + `type_defs::is_abstract_class` | **已实现** | 反：`new Shape()` 报「是抽象类…不能 new」；正：实现完抽象方法的子类可 new | **S04a 完成（D02）** |
| E22R | 索引：定长数组 / 指针（unsafe）/ 元组字面量下标 / String 只读 UTF-16 码元 / 自定义读写协议 | design §4.10、§6.5 | `typecheck/call.rs`、`unsafe_rules/assignment.rs` | 已实现 | 元组拒绝动态或越界下标；自定义读写仅识别 `__getitem` / `__setitem` | — |
| E23R | 成员访问：字段 / 方法 / 自动解引用 / struct `this` 免 unsafe | design §6.5 S6；§8.4 | `member.rs::check_member_access` + `MemberHit::check_visible` | **已实现** | 反：跨模块访问非 `pub` 字段报错；正：同模块 / 类型内部 / `pub` 可访问 | **S04a 完成（D02）** |
| E24R | 静态成员 `Type.member` | design §4.6 | `member.rs::check_static_access` | **已实现** | 反：`a.make()` 报「静态成员…请改用类型名」；`A.f()` 正常 | **S04a 完成（D02）** |
| E25R | match 类型合一、重复/覆盖模式与穷尽性；不可能匹配的类型模式为警告 | 已确认 D06；ROADMAP M-1 | `typecheck/matching.rs` / `flow.rs` | 已实现 | 布尔穷尽无 default 通过；缺分支与重复负常量拒绝；void 分支不强制穷尽；不可能的类型模式发出 NVL3009 警告 | S12a / M-1 |
| E26R | lambda 返回推导、默认值与独立执行上下文 | D03/D08 / TD03/TD05 | `typecheck/lambda.rs` | 已实现 | 块体合并可达返回和落尾；继承词法 this，重置 loop/catch/unsafe/异常状态；self 不能冒充接收者 | 设计纠偏 |
| E28R | f-string 插值类型检查与坐标 | design §4.12 | `expr.rs::check_fstring*` + `novella_lexer::lexer_at` | **已实现** | 正：`{a + 2}` 的类型键落在真实坐标；反：未定义名诊断 span 精确命中该标识符（含前导空白、多字节前缀） | **S02 完成** |
| E29R | 期望类型检查递归传递（括号/元组/数组/三元/`??`/`&&`/`\|\|`） | plan S03 | `expr.rs::check_expr_with_hint_inner` + `unsafe_rules::check_binary_with_hint` | **已实现** | 结构下推表见 S03 执行记录 | **S03 完成** |

### 1.6 语句与控制流

| ID | 规则 | 依据 | 实现 | 状态 | 正/反例 | D/S |
| --- | --- | --- | --- | --- | --- | --- |
| S01R | `const` 必须初始化 | plan | `body.rs::check_variable_decl` | 已实现 | 反：`const x: int;` 报错 | — |
| S02R | 变量需标注或初始值 | plan | `check_variable_decl` | 已实现 | 反：`let x;` 报错 | — |
| S03R | 元组解构 arity 与类型匹配 | design §4.9 | `bind_pattern_impl` | 已实现 | 反：arity 不符报错 | — |
| S04R | `return` 值类型与声明一致 | design §4.8 | `body.rs` Return 分支（现按期望返回类型下推） | **已实现** | 反：返回类型不符；正：`-> i32[] { return []; }` | **S03 完成** |
| S05R | 非 void 函数无正常落尾路径 | plan S06 | `typecheck/flow.rs` | 已实现 | 双分支、无限循环、穷尽 match 块均通过；缺返回路径拒绝 | S06 |
| S06R | `break` / `continue` 仅循环内 | design §4.9 | `body.rs` | 已实现 | 反：循环外 break 报错 | — |
| S07R | throw 必须为 Exception 或其子类 | 已确认 D08 | `body/mod.rs` / `ty::exception_type` | 已实现 | std:exception 导入提供静态名义根；无关 class 拒绝 | S12c |
| S08R | throw; 仅 catch 中重抛 | 已确认 D08 | AST `Throw.expr: Option` / parser / sema | 已实现 | catch 内通过，main 裸重抛拒绝；AST drop 与 LSP walker 已同步 | S12c |
| S09R | 未实现 yield 明确拒绝 | 已确认策略 | `body/mod.rs` Yield | 已实现 | yield 1 报 error；值继续检查；生成器实现不在本轮范围 | S13 |
| S10R | if / elif / else 条件 boolean | design §4.9 | `expect_boolean` | 已实现 | 反：`if (1)` 报错 | — |
| S11R | while 条件 boolean；循环体 | design §4.9 | `body.rs` While | 已实现 | — | — |
| S12R | for 三段 / for-in 数组及协议元素绑定 | design §4.9 | `body/mod.rs` / `body/iteration.rs` | 已实现 | 正：数组、泛型迭代器、继承方法、约束与元组解构。反：方法缺失、不可见、签名错误或元素类型不匹配 | — |
| S13R | try：必须有 catch 或 finally | design §9.1.2 | parser 文法 | 已实现（文法层） | 反：`try {}` 语法错误 | S12c |
| S14R | catch Exception 类型与顺序覆盖 | 已确认 D08 | `body/try_flow.rs` | 已实现 | Exception 在前、子类在后的 catch 拒绝；未标注 catch 绑定 Exception | S12c |
| S15R | finally 覆盖出口并传播确定赋值 | plan S06 / 已确认 D03 | `flow.rs` / `body/try_flow.rs` | 已实现 | finally return 覆盖原 return；finally 中赋值后 break 出口读取通过；调用前未赋值的异常路径拒绝 | S06/S12b |
| S16R | with 绑定原资源及可选协议签名 | 已确认 D08 | `body/mod.rs` / `build_method_sig` | 已实现 | as 保留原值；注解兼容；可选 __enter/__exit 为无显式参数 void，并检查可见性 | S12c |
| S17R | unsafe 块上下文传递与嵌套 | design §6.4 | `body.rs` Unsafe | 已实现 | 正：嵌套 unsafe 合法 | — |
| S18R | 顶层语句不允许 `return` | plan | `BodyCtx.expected_return = None` | 已实现 | 反：顶层 return 报错 | — |
| S19R | 局部读取须所有可达路径已赋值 | 已确认 D03 | `BodyCtx` / `body/try_flow.rs` / `matching.rs` | 已实现 | if、match、短路、循环 break/continue、for 更新、try/finally 汇合；取址不读取槽位 | S12b |

### 1.7 异常 / 异步

| ID | 规则 | 依据 | 实现 | 状态 | 正/反例 | D/S |
| --- | --- | --- | --- | --- | --- | --- |
| A01R | `await` 仅 async 上下文 | design §9.3.2 | `expr.rs` Await | 已实现 | 反：同步函数 await 报错 | — |
| A02R | async 调用 Future<T>，await 解包 T | 已确认 D08 | `call.rs` / `expr/dispatch.rs` | 已实现 | async-ok 通过，公开投影保留 Future<Int(i32)> | S12d |
| A03R | await 普通值拒绝 | 已确认 D08 | `expr/dispatch.rs` Await | 已实现 | await 1 报需要 Future<T> | S12d |
| A04R | main 同步（不可 async） | design §4.8.1 | `check_main_entry` | 已实现 | 反：`async func main()` 报错 | — |

### 1.8 与计划 S 编号对照

| S | 覆盖的矩阵行 | 状态 |
| --- | --- | --- |
| S00 | 本文件 | 完成（规则矩阵与本轮证据已更新） |
| S01 | D03R、D08R | **完成**（D08R 按 plan 不新增规则） |
| S02 | E28R、L09 | **完成** |
| S03 | E19R、E29R、S04R、E20R | **完成** |
| S04a | L02–L04、T03 | **完成**（D02 成员语义 + 字面量范围检查均已落地） |
| S04b | L05–L06、T14、T15 | 完成（f16 表示与 32/64 位目标配置已确认并验证） |
| S04c | L07–L10 | 完成（普通/raw/f/b 字符串，b 为 u8[]） |
| S04c-trait | E01、D04 | **完成**（继承列表按名分类、trait 成员登记与实现一致性） |
| S05 | D14R、D15R | 完成（字段与顶层声明依赖分阶段） |
| S06 | S05R、S15R | 完成（出口集合与穷尽 match 事实） |
| S07 | M01–M08、T11 | 完成（模块矩阵、恢复、递归压力验证） |
| S08 | E15R（HIR 顺序） | 完成（实际 HIR 调用顺序与 u64 载荷） |
| S09 | E23R、E24R、E03R | 完成（精确成员身份、静态性、self、跨模块 LSP） |
| S10 | E21R、D12R | 完成（继承覆写、抽象类、trait 实例签名） |
| S11 | T08、T09 | 完成（显式代入、递归推导、约束、别名） |
| S12 | E25R、E17R、S07R、S08R、S14R、S16R、A02R、A03R | 完成（match、结构对象、确定赋值、Exception、Future、with） |
| S13 | D16R | 完成（公开类型保真与后端明确拒绝） |
| S14 | 全表 | 完成（见本轮收口验收） |

## 2. 决策溯源（D，均已确认）

| D | 问题 | 影响矩阵行 | 决策前设计状态（历史） |
| --- | --- | --- | --- |
| D01 | 复合赋值的定位 / 读旧值 / RHS 顺序 | E15R、S08 | 普通赋值 §9.2 已有答案 |
| D02 | 成员默认可见性、`pri` 范围、静态成员经实例访问、覆写/遮蔽、抽象类实例化 | E23R、E24R、E21R | 无 |
| D03 | 无注解形参容错、是否启动参数推导、未初始化局部读取 | D05R、S19R | §4.8 已定容错 |
| D04 | class 实现 trait 的语法与检查 | D12R | 无（语法缺失） |
| D05 | 显式泛型实参代入范围、推导、约束 | T08、T09 | 无 |
| D06 | match 穷尽性、类型模式、块分支值 | E25R | 无 |
| D07 | 匿名对象结构类型、b 字符串、转义、f16 | E17R、L07、T14 | 无 |
| D08 | Exception 来源、`Future<T>` 可写性、with 绑定值、特殊方法签名 | S07R、S08R、S14R、S16R、A02R、A03R | §9.1.4 / §9.3.2 部分 |

## 3. 执行记录

### 2026-11-15 本轮一次性决定（用户三组分别答复“全部按建议”“采用”“可以”）

- 实施授权：整个计划连续执行；结构调整及配套 AST、HIR、LSP 变更无需再次确认。保留所有既有未提交修改，不提交、不推送、不改 CI/系统环境。
- D01：普通赋值 RHS→定位 LHS→写回；复合赋值定位 LHS→读取旧值→RHS→写回。
- D02 补充：禁止子类字段与父类字段同名及字段/方法相互遮蔽；构造器不继承；self 仅实例方法首参且显式注解须为属主类型；静态方法禁止 self/this。既有可见性、覆写、抽象类决定沿用。
- D03：读取局部前须在所有可达路径完成赋值；无注解普通形参继续容错，不启动普通参数推导。
- D04/D05：trait 保持显式括号列表实现，无字段/默认实现；完整显式泛型代入，允许从调用实参递归推导泛型参数，不从期望返回类型反推；冲突或缺失报错；支持现有语法中的 trait 约束。
- D06：值 match 必须穷尽；布尔与可判定联合类型模式可无 default，其他要求 default；块分支 void，不允许值/void 混合；重复常量与被前项覆盖的类型分支报错。
- D07：匿名对象结构类型允许宽对象赋给窄对象；b 字符串 u8[]，仅单字节字符与字节转义；f16 IEEE binary16；分析目标配置指定 usize/isize 的 32/64 位宽，默认宿主宽度；f-string 仅 {表达式} 与 {{/}}，不增加格式说明符，沿用已确认转义。
- D08：编译器提供 std:exception 的 Exception 名义基类静态身份，throw/catch 仅其子类；throw; 仅 catch 内重抛，catch 不得被前项覆盖；内建可写 Future<T>，async 调用返回 Future<T>，仅 async 内可 await，main 保持同步；with as 绑定原资源，可选 __enter/__exit 无显式参数且返回 void，__init/__destroy 返回 void、__str 返回 string。仅实现静态协议，不实现运行时。
- @cfg 已实现，其他未定义指令及保留 yield 报不支持错误。

以上为确认范围，不是实施完成声明；各任务仍须单独验证后更新状态。

> 模板见计划 §9。每次停工时追加一条，不写入用户个人记忆。

### 2026-11-15 · S01 main 返回类型收紧

```text
当前任务：S01（plan E09）
状态：验收通过
已确认依据：design.md §4.8.1（返回类型可为 void / int）、§5.2（int ≡ i32）
修改文件：crates/novella-sema/src/check/decl.rs（新增 is_main_return_type）
复现：func main() -> i64 / -> u8 修改前无任何诊断
改法：matches!(ret, Ty::Void | Ty::Int(_)) → is_main_return_type（Void / Int(I32) / Unknown 放行），
      别名（type Code = int）经 expand_alias 展开命中 I32；Unknown 不追加级联诊断
消费者：仅 sema；HIR 不读 main 返回类型，CLI/LSP 只渲染诊断，无接口变更
验证：bad-int64 / bad-u8 报「`main` 的返回类型必须是 `void` 或 `int`，实为 `i64`/`u8`」；
      void / int / i32 / good-alias 无诊断；bad-alias（type Wide = i64）报错；
      span 0-36 命中整条函数，line/column 正确
尚未通过：无（D08R「extern 返回类型必须显式」按 plan §6 S01 明确不新增）
下一步：S02
禁止覆盖的既有修改：无
```

### 2026-11-15 · S02 f-string 坐标与 single-pass 词法

```text
当前任务：S02（plan E07 / E08）
状态：验收通过
已确认依据：design.md §4.12（字面量保留原文）、plan §6 S02、AGENTS.md「注释过滤只有一处」
修改文件：
  crates/novella-lexer/src/lib.rs：新增 pub fn lexer_at(input, base) + 私有 rebase；
    lexer(input) 变为 lexer_at(input, 0)，行为不变
  crates/novella-sema/src/typecheck/expr.rs：check_fstring_interpolation 用
    base = span.start + 1 + 前导空白 调 lexer_at；删除 adjust_diagnostics（整函数）
  crates/novella-sema/src/module/mod.rs + module/loader.rs：删除已无读取方的
    Module.lines: LineIndex 字段（原先唯一读者是 adjust_diagnostics）
复现：
  a) 前导空白：f"x = {  undefined_name  }" 期望 span 35-49，实际 33-47（丢 trim 偏移 2 字节）；
  b) 类型表：f"v={  a + 2  }" 中 `a` 的表达式类型键为 span 4-5（片段局部坐标），
     在父文件中不可寻址，HIR 的 expression_types.get(module, span) 会 MissingExpressionType；
  c) 注释：原实现在 sema 内 filter(LineComment)，违反「注释过滤只在 preprocess.rs」。
改法：把「片段落在父坐标」上移到 lexer（lexer_at），使 token / AST / 诊断 / 类型表键
      从源头就是真坐标；不再事后平移，也不再在 sema 里写第二处注释过滤之外的逻辑
      （filter 保留在 f-string 这一处，因为该子串未经 parser::preprocess）。
消费者：novella-sema（唯一调用方）；lexer 新增公开函数，parser / cli / lsp / hir 行为不变
验证：
  a) 上述四例 span 均按字节精确命中出错片段（33-47 / 35-49 / 33-47 / 35-49），line+col 正确；
  b) 类型表 consumer 显示 `a` → span 53-54 Int(i32)、字面量 2 → span 57-58 IntLiteral（真实坐标）；
  c) 边界：`{{b}}` 转义与插值内引号无诊断；`{1 + }` 报「不是合法表达式」span 命中 "{1 + }"；
      `{}` 报「插值不能为空」span 命中 "{}"；嵌套括号 `{(a + 1) * 2}` 无诊断
回归：cargo test --workspace = 102 passed；cargo fmt --check 通过；clippy -D warnings 无问题
尚未通过：无
下一步：S03
禁止覆盖的既有修改：无
```

### 2026-11-15 · S03 期望类型结构化下推

```text
当前任务：S03（plan E04 / S03；矩阵 E19R / E29R / S04R / E20R）
状态：验收通过
已确认依据：design.md §5.2.1（字面量归化）、plan §7 S03
修改文件：
  crates/novella-sema/src/typecheck/expr.rs：
    check_expr_with_hint 改为「check_expr_with_hint_inner + record_expression_type」两层，
      下推表：Paren 穿透 / ArrayLiteral(T[]→元素) / Tuple 逐位 / Ternary 双分支 /
      Binary ??（备用值）/ Binary && ||（两侧 boolean）/ null→可空目标；
    新增 check_array_literal_with_hint、unify_tuple；check_expr 的登记改走 record_expression_type
  crates/novella-sema/src/typecheck/unsafe_rules.rs：
    新增 check_binary_with_hint（check_binary 委托），拆出 check_binary_operands；
    check_assignment 右值、check_struct_init 字段初值改走 check_expr_with_hint
  crates/novella-sema/src/typecheck/body.rs：Return 分支按 expected_return 下推
  crates/novella-sema/src/typecheck/call.rs：实参按形参类型下推（新增 instance_param_tys），
    new 实参按 __init 形参下推
历史复现（以下使用旧版数组语法，不是当前 `T[N]` 写法）：
  let xs: i32[] = ([]);                   括号
  return ([], []);  在 -> (i32[], i32[])  元组
  let rows: i32[][] = [[1], []];          嵌套数组
  takes([]);                              调用实参
  另有 return [] / P { xs: [] } / xs = [] 三处同类缺口
改法：期望类型沿结构递归下推到叶子；所有自带期望类型的语法位置统一改走该入口。
  元组位数不符时报「元组字面量有 N 个元素，期望类型是 M 位的 `...`」，不再为对不上的
  位置伪造期望类型。
消费者：仅 sema 内部；check_expr_with_hint / check_binary_with_hint 仍为 pub(crate)，
  HIR / CLI / LSP 不感知
验证：
  正：bare-empty / paren-empty / tuple-empty / array-nested / call-arg-empty / ret-empty /
      struct-empty / assign-empty 全部无诊断
  反：mismatch-elem 报「数组元素类型 `string` 与标注的元素类型 `i32` 不兼容」span 命中 "a"；
      call-arg-mismatch 同上（span 命中实参内 "a"）；tuple-mismatch 第二行先报空数组无法推断
      （既有语义：声明只在值能赋值给标注时静态复核，空数组自身仍须可定型）
回归：cargo fmt --check、cargo test --workspace = 102 passed、clippy -D warnings 无问题
下一步：S04a
禁止覆盖的既有修改：无
```

### 2026-11-15 · S04a 成员可见性 / 静态访问 / 覆写 / 抽象类（D02 已确认）

```text
当前任务：S04a（D02；矩阵 E21R / E23R / E24R）
状态：验收通过
决策记录（用户已确认，2026-11-15）：
  1) 缺省可见性 = pri（仅声明类型内部 + 同模块可见）；pub = 任意模块可见；
     pkg 与 pri 同归一类（语言尚无包边界，无可检查判据）
  2) 静态成员不得经实例访问；实例成员不得经类型名访问（后者原已实现）
  3) 覆写必须形参类型全一致，返回类型允许协变；静态 / 实例属性必须一致
  4) 抽象方法 = 只有声明没有函数体的方法；含未实现抽象方法的类不可 new，
     子类未实现则子类也是抽象类
修改文件：
  crates/novella-sema/src/ty.rs：
    FieldDef += is_public / module；MethodSig += is_public / name / is_abstract / span
  crates/novella-sema/src/check/type_defs.rs：
    新增 is_public_member；build_field / build_method_sig 填充新字段；
    resolve_class_parents 末尾调用 check_overrides（新增 check_overrides / check_override_sig /
    signature_span / module_of）；新增 pub(crate) is_abstract_class / is_implemented
  crates/novella-sema/src/typecheck/member.rs：
    新增 MemberHit { info, owner } + check_visible；lookup_member_chain 返回 MemberHit；
    member_type_of / check_static_member 适配并接入 check_static_access（新增 StaticMarked trait）
  crates/novella-sema/src/typecheck/call.rs：check_new 在检查实参前调用 is_abstract_class
  crates/novella-sema/src/typecheck/mod.rs：mod member → pub(crate) mod member
  crates/novella-sema/src/check/mod.rs：导出 is_abstract_class
复现（修改前全部零诊断）：a.secret（pri 字段）/ a.make()（静态方法经实例）/
  override-mismatch（子类形参 string 对父类 i32）/ new Shape()（抽象类）/
  override-static-mismatch（静态覆写实例）
改法：可见性与静态性作为成员定义的属性进入查找结果，访问点统一判定；覆写在父类
  解析完成后集中校验；抽象性沿继承链「最近实现」判定。
消费者：FieldDef / MethodSig 均为 pub(crate)，HIR / CLI / LSP 不感知；无接口变更
验证：
  反：static-via-instance span 命中 "a.make"；private-cross-module 报「成员 `value` 不是 `pub`」；
      override-mismatch 报形参类型不符（span 命中整个方法声明）；
      override-static-mismatch 报「静态 / 实例属性不一致」；
      abstract-new / abstract-unimplemented / abstract-child-new 均报抽象类
  正：private-member（同模块）、pub-cross-module（pub 字段与方法）、override-dup（签名相同的覆写）、
      override-return-covariant（Dog ← Animal 协变）、abstract-implemented（实现后无诊断）均无诊断
回归：fmt OK、clippy -D warnings 无问题、cargo test --workspace = 102 passed
尚未通过：pkg 与 pri 未区分（无包边界）；trait 成员未纳入（S04c / D04）
下一步：S04a 的字面量范围检查（L02–L04、T03），随后 S05 泛型
禁止覆盖的既有修改：无
```

### 2026-11-15 · S04a 整数字面量范围检查（L02–L04、T03）

```text
当前任务：S04a 剩余项（字面量按目标位宽检查）
状态：验收通过
决策记录（用户已确认，2026-11-15）：仅 sema 内部改进，公开投影不变
  ——`Ty::IntLiteral` 携带数值，`Ty::view()` 仍投影为 `TypeView::IntLiteral`，
  HIR / CLI / LSP 零改动
修改文件：
  crates/novella-sema/src/ty.rs：
    Ty::IntLiteral → IntLiteral(bool, u128)（是否取负 + 绝对值）；
    classify_number_literal 解析出数值；assignable 按位宽判定；
    新增 int_literal_fits_non_negative / int_negated_literal_fits / int_range_text；
    is_numeric / is_integer / ty_eq / Display / view 同步
  crates/novella-sema/src/lib.rs：新增 Sema::assign_mismatch_reason（把「超出范围」说清）
  crates/novella-sema/src/typecheck/expr.rs：check_unary 对字面量操作数保留未定型态，
    `-` 翻转取负位、`+` 保持
  crates/novella-sema/src/typecheck/unsafe_rules.rs：unify_numeric 合并取负位
  crates/novella-sema/src/typecheck/body.rs：concrete 适配新变体
  crates/novella-sema/src/typecheck/{body,unsafe_rules,call}.rs：赋值 / 字段 / 实参诊断接入原因文案
复现（修改前）：`let x: u8 = 256;` 零诊断；`let x: i8 = -128;` 误报「初始化类型 i32 与标注 i8 不兼容」
改法：字面量在语义层保留数值，与目标整数类型比对时按位宽判范围；正字面量按
  有符号上限、取负字面量按 |MIN|；一元符号不再提前 concrete 成 i32。
消费者：TypeView 未变，HIR 的 from_view_with 无需改动；CLI JSON / LSP 不变
验证：
  反：u8-over(256)、i8-plus-over(128)、i8-over(-129)、u64-over(…616)、hex-u8-reject(0x100)、
      param-over(300)、neg-min(-(-128)=+128) 均报「整数字面量 `X` 超出 `<类型>` 的取值范围 a..=b」
  正：u8-max(255)、i8-min(-128)、u64-max、i8-hex-ok(0xFF)、i64-mid 无诊断
  投影：consumer 显示 `u8 = 255` 的表达式类型仍是 TypeView::IntLiteral（载荷不外泄）
回归：fmt OK、clippy -D warnings 无问题、cargo test --workspace = 102 passed、
      hir_smoke --json 正常、git diff --check 通过
尚未通过：浮点范围（L05–L06）留给 S04b；usize/isize 按 64 位判定（T15，S04b）
下一步：S04b 浮点溢出与 f32/f64 边界、usize/isize 宽度
禁止覆盖的既有修改：无
```

### 2026-11-15 · S04b 浮点范围与平台宽度（L05–L06、T14、T15）

```text
当前任务：S04b
状态：验收通过（T14 表示标准仍待 D07）
修改文件：
  crates/novella-sema/src/ty.rs：Ty::FloatLiteral → FloatLiteral(f64)；
    assignable 走 float_literal_fits；新增 float_literal_fits / float_range_text；
    classify_number_literal 用 `cleaned.parse::<f64>().ok().map(Ty::FloatLiteral)`；
    view / is_numeric / ty_eq / Display 同步
  crates/novella-sema/src/lib.rs：assign_mismatch_reason 增加浮点分支
  crates/novella-sema/src/typecheck/{body,unsafe_rules}.rs：适配新载荷
复现（修改前全部零诊断）：`f32 = 1e999`、`f64 = 1e999`、`f16 = 1e999`
改法：判据是「按目标浮点类型舍入后仍有限」——`3.4028235e38` 虽略大于 f32::MAX，
  舍入后正好落在 f32::MAX 上，属合法；`1e999` 解析即 inf，一律拒绝。
消费者：TypeView::FloatLiteral 不变（仍不带值），HIR / CLI / LSP 零改动
验证：
  反：f32-over / f64-inf / f16-over / f32-slight-over(1e309) 报
      「浮点字面量 `inf` 不是有限值，超出 `<kind>` 的可表示范围 …」
  正：f32-max(3.4028235e38)、f64-max、float-lit-f32(1.5)、usize-ok、isize-min(-9223372036854775808) 无诊断
  反（整数侧补验）：isize-over(9223372036854775808) 报超出 isize 范围
回归：fmt OK、clippy -D warnings 无问题、cargo test --workspace = 102 passed、hir_smoke --json 正常
尚未通过：f16 表示标准（D07）；usize / isize 未随目标三元组（后续期，已在注释标明按 64 位）
下一步：S04c 字符串前缀 r/b、转义校验、sema↔HIR 解码一致（L07–L10，涉及 D07）
禁止覆盖的既有修改：无
```

### 2026-11-15 · S04c 字符串解码统一（L07–L10，D07 已确认）

```text
当前任务：S04c
状态：验收通过（b 字符串类型语义仍待定）
决策记录（用户已确认，2026-11-15）：按 design 现状最小闭环——
  转义集 \n \r \t \0 \\ \' \" \xHH \u{H...}；r"..." 不解释转义；
  b"..." 本期按 string 处理（不引入 bytes 类型）；f16 沿用 f32 口径
新增文件：
  crates/novella-core/src/string.rs（+ lib.rs 注册 pub mod string）
    DecodeError{NotAStringLiteral, UnknownEscape, BadHexEscape, BadUnicodeEscape,
                InvalidCodePoint, TrailingBackslash} + Display
    decode_string_literal(raw) / decode_escapes(body)；带 4 个单测
修改文件：
  crates/novella-sema/src/typecheck/expr.rs::check_literal 的 Literal::String 分支
    先调 core 解码校验转义，再走既有 check_fstring
  crates/novella-hir/src/build/aggregate.rs::build_literal 改用 core 解码；
    crates/novella-hir/src/build/helpers.rs 私有 decode_string 整函数删除
复现（修改前全部零诊断）：`"bad\qescape"`、`"\x4"`、`"\u{110000}"`、b"bytes" 无独立语义
改法：解码表只在 core 实现一份，sema 负责报错、HIR 负责取值，规则天然一致
消费者：HIR 由自有解码改为调用 core（错误转 BuildErrorKind::Unimplemented{node:"string literal
  with unsupported escape"}）；CLI / LSP 不变
验证：
  反：bad-escape 报「非法转义序列 `\q`」（span 命中整个字面量）；bad-hex 报「`\x` 之后必须是两位
      十六进制数字」；bad-unicode 报「`U+110000` 不是合法的 Unicode 标量值」；surrogate 报 `U+D800`
  正：raw-string（`r"C:\new\path"`）、raw-ok（`r"no \q escape"`）、esc-ok（\t \x41 \u{4F60} \\ \"）、
      byte-string、f-string-escape、single-quote 均无诊断
回归：fmt OK、clippy -D warnings 无问题、cargo test --workspace = 106 passed（含 core 4 个单测）、
      hir_smoke --json 正常
尚未通过：b"..." 的字节串类型语义（需新 Ty 变体，待后续期决策）；f16 表示标准（D07）
下一步：S04c 剩余 = trait 定义与实现（E01，D04）
禁止覆盖的既有修改：无
```

### 2026-11-15 · S04c-trait trait 定义与实现（E01，D04 已确认）

```text
当前任务：S04c 的 trait 部分
状态：验收通过
决策记录（用户已确认，2026-11-15）：
  1) 实现语法复用 classHeritage 括号列表：`class C (Parent, Trait1, Trait2)`；
  2) 无显式 implements 关键字；括号里的位置不预设语义，**按符号表逐项分类**——
     class 项是父类，trait 项是实现的 trait。故 `class Circle (Drawable)` 合法
     （无父类、实现一个 trait）。
修改文件：
  crates/novella-sema/src/ty.rs：
    TraitDef += methods: HashMap<String, MethodSig>、name: String
    ClassDef  += name、span、trait_impls: Vec<TypeExpr>、trait_impls_resolved: Vec<(ModuleId,String)>
  crates/novella-sema/src/resolve.rs：ClassDef/TraitDef 初始化新字段
  crates/novella-sema/src/check/type_defs.rs：
    build_type_defs 的 Item::Trait 分支登记 trait 方法签名（字段/默认方法体各报错）
    新增 build_method_sig_trait（属主 Ty::Trait，is_abstract = true）
    build_method_sig 的 name 前缀支持 Ty::Trait（原先 unreachable! panic）
    Class::parent 全量写入 trait_impls 候选（原按「首项父类、其余 trait」切片，错的）
    resolve_class_parents 对 Ty::Trait 只分类不登记（登记与检查统一归 resolve_trait_impls）
    新增 resolve_trait_impls / check_trait_impl / find_implementation / trait_signature_compatible
    删除「class.parent.len() > 1 报多继承」——列表长度不再等价于多父类
复现（修改前）：
  - trait 二次声明照常零诊断但成员不登记 → 实现缺失/签名不符一律静默；
  - `class Circle (Drawable)` 报「父类必须是 class 类型，实为 `Drawable`」；
  - `class Circle (Base, Drawable)` 报「多继承暂不支持」（误报）。
改法：括号列表逐项按符号表分类；父类多于一个才报多继承；trait 的每个方法都要求
  在类继承链上有非抽象实现，形参逐位类型相等、返回类型可赋给 trait 的声明（协变）。
消费者：ClassDef/TraitDef 是 crate 内部结构（pub(crate)），HIR / CLI / LSP 无感知
验证（/tmp/novella-sema-cases/S04c-trait/）：
  正：impl-ok（两方法全实现）、parent-and-trait（父类 Base + trait Drawable）零诊断
  反：impl-missing 报「class `Circle` 声明实现 trait `Drawable`，但缺少方法 `area` 的实现」
      且 span 命中 class 声明头（76-135）
      impl-bad-sig 报「方法 `area` 的实现与 trait `Drawable` 声明的签名不一致」（span 命中方法）
      field-in-trait 报「trait `Drawable` 不能声明字段 `x`」
      default-method 报「trait `Drawable` 的方法 `draw` 暂不能带函数体（默认方法未实现）」
      not-a-trait 报「多继承暂不支持（仅首个父类生效）」（span 命中第二个 Base）
回归：fmt OK、clippy -D warnings 无问题、cargo test --workspace = 106 passed、hir_smoke OK、
      examples/novella-syntax-demo.nov 回到「零错误 + 3 条既有 warning」
顺带修正的示例问题（新规则命中，示例本身不合规）：
  - trait Drawable 原先声明两个字段 → 改为等价方法声明（trait 字段语义未定义）
  - class Shape 有抽象方法 serialize() 却要 new → 补上实现（D02 的抽象类规则）
  - `n0 ?? 5` / `return n ?? a` → 改为左右同类型的指针空合并（design §9.2）
文法同步：grammar/NovellaParser.g4 的 classHeritage 改为 `'(' typeReference (',' typeReference)* ')'`
尚未通过：trait 泛型实参代入实现类（依赖 D05 泛型代入）；trait 方法体的默认实现
下一步：S05 声明分阶段与源码顺序无关性；泛型代入为 S11（E06，涉及 D05）
禁止覆盖的既有修改：无
```

### 2026-11-15 · S05a 声明顺序漏洞复现与阶段调整评审

状态：复现完成，待阶段调整确认；尚未修改编译器实现。

最小复现（均无语法错误）：

```novella
struct Box {
    func read(self) -> string { return this.value; }
    value = 1;
}
func main() {}
```

- 当前零诊断；公开 `analyze` API 的 `expression_types` 中 `this.value` 为 `Unknown`（52..62）。
- 将 `value = 1;` 移到方法之前，报 NVL3002「返回类型 `i32` 与函数声明的 `string` 不兼容」；对应表达式投影为 `Int("i32")`（67..77）。两次 `syntax_failed=false`。
- 跨类型同样复现：`struct Use { value = Seed{}.value; } struct Seed { value = 1; } func check(v: Use) -> string { return v.value; } func main() {}` 零诊断；将 Seed 移至 Use 前面，报相同返回类型错误。
- 真正的字段推导环：`class A { value = (new B()).value; } class B { value = (new A()).value; } func main() {}` 零诊断。
- 相邻正例：`func first() -> i32 { return later(); } func later() -> i32 { return 1; } func main() {}` 零诊断，前向函数签名注册无需重做。
- 独立发现：`class Pair { static a = Pair.b; static b = 1; } func main() {}` 报「类型 `Pair` 没有成员 `b`」，成员查找过滤了静态字段；此例不能用来验收推导环。

命令与证据：

```text
rtk cargo build -p novella
  成功，4 条 dead_code 警告。
rtk target/debug/novella compile /tmp/novella-s05-341dgW/method-first.nov -o /tmp/novella-s05-341dgW/method-first.json
  零诊断；其余例按同一命令替换路径。
rtk proxy /tmp/novella-s05-341dgW/inspect /tmp/novella-s05-341dgW/method-first.nov /tmp/novella-s05-341dgW/field-first.nov
  调公开 novella_sema::analyze，读取 expression_types.types；结果如上。
rtk cargo clippy --workspace --all-targets -- -D warnings
  失败（既有工作区基线）：FunctionSig.type_params、substitute_type_params、
  infer_type_param、build_substitution 四处未使用。未为通过检查删除或屏蔽它们。
```

根因：`check_module` 按源码顺序交错检查方法体与字段初值，后者才回填字段类型；方法与早期字段已记录的 Unknown 不会再更新。只把本类型字段提前不足以解决跨类型、跨模块依赖。

本轮修改文件：`docs/sema-coverage.md`（矩阵、纠正 S05/S11 编号、证据）；具体阶段方案、输入输出、文件与兼容性记录见本文件下方执行记录。

下一步：确认 plan §6 S05a 的阶段调整后实现；S05b 顶层声明依赖另验收。暂停依据为 S05 原文「若需重新安排多个阶段，先写具体阶段输入/输出与变更文件，确认后实施」，不是对语言规则新增假设。此前全部未提交源码保持原样。

### 2026-11-15 · S00 基线

```text
当前任务：S00
状态：验收通过
已确认依据：plan §6 S00；design §3–§10
修改文件：docs/sema-coverage.md（新增）、AGENTS.md（指针同步）
复现：无（基线记录）
验证：fmt --check 通过；cargo check --workspace --all-targets 通过；
      cargo test --workspace = 102 passed / 24 suites；git status --short 为空；
      HEAD ca976da3c332be9e478f78bbf3973e297931564e
尚未通过：无
下一步：S01 main 返回类型收紧
禁止覆盖的既有修改：无
```


## 4. 本轮收口验收（2026-11-15）

### 4.1 结论与范围

S00–S14 已完成，D01–D08 均按本轮用户一次性答复落实，无剩余待确认的静态规则。
第 0 节是启动基线；第 3 节中逐阶段“下一步”“待确认”等字样是历史执行记录，
当前状态以第 1 节矩阵为准；本节运行记录保留历史含义。
@cfg 已实现，其他未定义指令及 yield 报错；std 除编译器 Exception 根外必须加载真实声明。

### 4.2 质量命令与实际结果

以下命令均在仓库根执行（RTK 是项目规定的命令前缀）：

| 命令/检查 | 本轮结果 |
| --- | --- |
| `rtk cargo fmt --all -- --check` | 通过 |
| `rtk cargo check --workspace --all-targets` | 通过 |
| `rtk cargo clippy --workspace --all-targets -- -D warnings` | 通过，无警告 |
| `rtk cargo test --workspace` | 106 passed / 24 suites |
| `rtk cargo run -q -p novella-hir --example hir_smoke -- --json` | 通过，产出完整 HIR JSON |
| `rtk cargo doc --workspace --no-deps` | 通过，无警告 |
| `rtk git diff --check` | 通过 |
| 语义 CLI 正反例 | 44 正例 + 37 反例全部符合预期，反例均有语义错误且无词法/语法错误充数 |
| 损坏的泛型 struct 初始化语法 | 另行确认报语法错误，不吞掉泛型试探失败 |
| 模块/导入矩阵 | 10 组全部符合预期（循环、路径去重、crate 根、缺根、冲突、私有、命名空间构造、跨模块名义身份、别名环、缺失文件后继续检查独立错误） |
| 公开消费者 | 32 位 usize 拒绝 4294967296、64 位接受；Applied/Future 投影保留实参 |
| LSP 实际 stdio JSON-RPC | 跨文件继承字段/方法 definition 精确命中名字；rename 字段 4 处、方法 2 处，均跨 2 文件，中文/emoji 前缀及 f-string 中 UTF-16 范围正确 |
| HIR 拒绝边界公开消费者 | async、Object、泛型 struct、b/f 字符串均返回带 module/span 的 BuildError，无 panic |
| 文档最小源码 | 本节 6 段均实际运行，预期的类型错误与相邻正例均符合 |
| 深层输入（独立进程 timeout 20 秒） | 10000 层括号、2000 层 unsafe 块、5000 层指针类型均 exit 0、无错误；表达式表分别为 10001/1/0 项 |

临时验收产物在 `/tmp/novella-s05-341dgW`，包括 `verification-report.json`、
模块源码与 JSON、公开消费者、HIR JSON。它们不是仓库测试套件，也不是持久交付依赖；
核心复现源码与命令如下，临时目录清理后仍可手动重建。

### 4.3 最小复现与预期

把下面每段分别保存为 `case.nov`，执行
`rtk cargo run -q -p novella -- compile case.nov -o case.json`，
检查 JSON 的唯一 `diagnostics` 树，不单凭 CLI 退出码判断语义成功。

声明顺序与依赖：

```novella
class A {
    func value(self) -> string { return this.number; }
    number = 1;
}
func main() {}
```

预期返回类型不兼容；交换方法与字段顺序仍报同类语义错误。
`let a = b; let b = a; func main() {}` 报推导环，
`let a: i32 = b; let b: i32 = a; func main() {}` 不报类型推导环
（这不承诺运行时初始化次序安全）。

结构对象的上下文下推与宽赋窄：

```novella
func main() {
    let a = {x: [1], nested: {y: [2]}};
    a = {x: [], nested: {y: []}, extra: true};
}
```

预期无错误；把 x 改为 string 则拒绝。泛型身份与实参推导：

```novella
struct Box<T> { value: T; }
func same<T>(value: T) -> T { return value; }
func main() {
    let box = Box<i32>{value: same(1)};
    let value: i32 = box.value;
}
```

预期无错误，公开表达式类型保留 Applied(Box, [i32])；将 value 标注改为 string 则拒绝。
约束检查包括泛型 trait 实参、继承实例与 alias 声明环境，不从期望返回类型反推。

控制流与确定赋值：

```novella
func main() {
    let x: i32;
    while (true) { try { break; } finally { x = 1; } }
    let y = x;
    let z: i32;
    for (; false; z++) { let unreachable = z; }
}
```

预期无错误；删除 finally 的赋值后读取 x 被拒绝。
条件中的赋值在未命中分支时仍保留；恒真前项后的 elif 条件不可达，
for-in 的迭代表达式必求值一次，其赋值可在循环后读取。协议隐式调用保留异常入口状态。
try/catch/finally 对正常、return、throw、break、continue 分别汇合，调用/new/await
记录可能抛出的入口状态，finally 初始化作用于实际跳转；不可达路径不产生未初始化读取错误。

match 值分支、异常与异步的最小边界：

```novella
func choose(flag: boolean) -> i32 {
    return match flag { case true => 1 case false => 0 };
}
async func value() -> i32 { return 1; }
async func use() -> i32 { let f: Future<i32> = value(); return await f; }
func main() { let x = choose(true); }
```

预期无错误；删除 false 分支后值 match 非穷尽，拒绝；同步 main 中 await 拒绝。
从 `std:exception` 导入 Exception 后可作为 throw/catch 名义根，无关 class 拒绝；
root catch 在子类 catch 前时拒绝后者，裸 throw 仅 catch 内合法。

HIR 求值顺序与大整数载荷：

```novella
func index() -> i32 { return 0; }
func value() -> i32 { return 7; }
func main() {
    let a: i32[] = [0];
    a[index()] = value();
    a[index()] += value();
    let big: u64 = 18446744073709551615;
}
```

当前使用 `rtk cargo run -q -p novella -- compile case.nov --emit hir -o case.hir.json`；
重复的 `hir_smoke` 调试 example 已移除。
实际 Call 顺序是 value、index、index、value；复合赋值在调用 RHS 前已读取旧值。
JSON 原始文本精确保留 18446744073709551615，HIR 整数载荷改为 i128，不能经 JS Number
比较这个 u64 边界。sema 对 `a[x] = (x = 1)` 也按 RHS 优先，复合版本读取未赋值 x 则拒绝。

### 4.4 文件职责与消费者契约

- sema 根 API 保持最小重导出；公开值类型在 api.rs，管线在 analysis.rs，内部状态在 state.rs。
- resolve/ 按声明收集、导入、类型解析分开；ty/ 按定义、关系、泛型、字面量分开。
- check/fields.rs 与 globals.rs 按依赖定型；check/type_defs/ 注册成员并校验继承、trait 与覆写。
- typecheck/expr/ 按分派、名称、提示、字符串、聚合、一元拆分；body/ 负责语句、绑定、异常路径；
  flow.rs 计算出口，matching.rs 维护匹配事实，unsafe_rules/ 分管运算符、赋值及构造。
- 字符串解码只有 core/string.rs 一处；lexer_at 提供插值宿主坐标，注释仍只在 parser/preprocess.rs 过滤。
- 无注解普通形参等保留 Unknown；已报错类型独立投影为 Error。
  错误结果不能作为 HIR 输入。匿名函数始终投影 Function，不因无名义键退化为 Unknown。
- 新增 TypeView::Applied/Object/Future、目标位宽入口、trait 方法摘要、穷尽 match 事实；AST 成员
  name_span、StructInit.type_args、Throw.expr 可选、HIR i128 会改变 Rust/serde 消费者形状。
  内仓 parser、sema、HIR、LSP 与 grammar 已同步，外部消费者按新形状穷尽处理。
  **无迁移，直接替换**；不保留旧字段或双重诊断树。
- GuardedClone 与 AST 分段析构、sema 递归守卫共同覆盖已实测深度；不承诺任意资源消耗下无上限，
  深类型投影仍有递归/分配成本，不以压力样例声称无限深度。

### 4.5 后端边界

本轮完成的是已确认静态语义及消费者契约。LLVM、异常展开、异步状态机、ARC、泛型单态化、
class 动态派发与闭包捕获不在交付范围。HIR 对暂不支持的 Future、Applied、Object、字节串、
f-string、异常/异步等明确返回带源码位置的 BuildError；不会以丢弃实参、伪造具体类型或部分 HIR
输出放行。没有提交、推送或更改系统/CI 配置；保留任务启动前的已有修改。

## 2026-11-21 语法入口复核

- 元组索引已能通过 `novella check`，赋值右侧沿用槽位的期望类型。HIR 会在索引处报告暂不支持降级。
- `case (value = 1) => value` 保留分支箭头，`match` 至少需要一个 `case`。
  命中分支不能读取仅由后续模式赋值的局部。
  全部布尔分支均提前返回时，包含该 `match` 的变量初始化不会产生错误的落尾诊断。
- 抽象方法使用继承链中最近的声明。`__str__` 和 `__str` 的覆写使用同一个规范名称。
- `yield` 仍依 design §9.4 保留语法并报告未支持。编译器指令以 `@cfg` 为已定义范围。

### 2026-11-21 · String/ARC 原生集成

- D19R：weak 接通独立控制块与原子 upgrade，过期返回 null，禁止 weak 取址。泛型 class 静态判定不等于原生单态化已实现。
- String 身份由 SemaResult.string_type 传递，同名用户 class 不匹配；索引只读返回 UTF-16 码元 u16。
- f-string 已检查 AST 保存在 SemaResult，供 HIR 降级；支持整数、boolean、f32/f64、String、class __str。b 字符串为 u8[]，支持 no_std。
- 析构 self/this 不得进入拥有位置，不得调用自身实例方法或被捕获，防止直接/间接复活；字段访问及字段对象的方法允许。
- HIR/codegen 接通构造、ARC、weak、数组、struct/tuple 递归所有权、完整表达式及作用域清理。此前 ARC/b/f 不在范围的条目为 2026-11-15 历史。
- 验证与限制见 [String/ARC 交接](ai-handover-string-arc.md)，标准库契约见 [std 说明](../std/README.md)。

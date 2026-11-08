# Parser 无法下推给 sema 的问题清单

> 状态：**已全部修复**（2026-11-08）
> 基准：工作区 commit `cce23e9` + 未提交的 parser 改动
> 范围：`novella-parser` → `novella-sema` 的职责边界
> 结论：parser 可以“接受超集、把语义判断下沉给 sema”，但必须保证 **AST 无损**；
> 下列问题要么 sema 看不到（信息已丢）、要么看到错误结构、要么根本收不到（parser
> 拒了合法输入），因此不能交给 sema 兜底。
>
> 修复结果：A / B / C / D / E 全部条目已在 parser（含 AST 补字段）修复，
> 回归用例见 [`crates/novella-parser/tests/boundary_regression.rs`](../crates/novella-parser/tests/boundary_regression.rs)
> （按条目命名，23 个用例）；规范拍板（E 类）已同步 `design.md` 与
> `grammar/NovellaParser.g4`。逐条修复方式见 §11。

---

## 1. 判据

以下四条是本文的收录标准：

1. **信息已丢**：parser 在构造 AST 时丢弃了 token，sema 无法从 AST 恢复。
2. **结构已错**：parser 把原文解析成了另一种 AST 结构，sema 看到的是另一段程序。
3. **输入被拒**：parser 对文法/设计允许的输入报错，sema 没有机会运行。
4. **解析崩溃**：parser 在到达 sema 之前栈溢出，进程 abort。

不满足以上条件的“语义约束”（可见性规则、类型规则、`try`/`match` 的结构校验、
`weak` 使用范围等）可以下沉给 sema，见 §6。

> 重要前提：sema 只读 AST。若 parser 丢弃 `@`、`pub`、类型实参、`async` 标志等
> 信息，sema 无法通过“更严格的检查”补回来；若 parser 把 `with` 绑定解析成 cast，
> sema 也无法知道用户原本想写什么。

---

## 2. A 类：解析崩溃（sema 不会运行）—— ✅ 已修复

### A-01 ~ A-04 递归链绕过嵌套上限

`MAX_NESTING = 128` 只在 [`parse_expression`](../crates/novella-parser/src/expression.rs)
和 `parse_block` 入口检查；以下递归链没有 `nesting_exceeded()` 检查，或用 `?`
提前返回导致 `leave_nesting` 被跳过：

| ID | 触发输入（5 万层） | 递归入口 | 修复前实测 |
|---|---|---|---|
| A-01 | `!` × 50000 + `true` | `parse_unary` | `thread 'main' has overflowed its stack`，exit -6 |
| A-02 | `1 ? ` × 50000 + `1` + ` : 2` × 50000 | `parse_ternary` | 同上 |
| A-03 | `a = ` × 50000 + `1` | `parse_assignment` | 同上 |
| A-04 | 类型中的 `(` × 50000 + `int` + `)` × 50000 | `parse_type_expr` | 同上 |

根因是 `enter_nesting` **只报一次诊断、不阻断递归**；真正阻断依赖各函数主动
调用 `nesting_exceeded()`。

### A-05 深度泄漏

`parse_unary`、`parse_primary`、`parse_type_expr` 等函数的失败路径使用 `?`
直接返回，没有执行 `leave_nesting(depth)`，抬高后续输入的 `depth`，让合法输入
被误判为“嵌套过深”。

> 修复方式（落地）：四个递归入口拆成「守卫包装 + 实体」两层——包装负责
> `enter_nesting` / `nesting_exceeded` 检查 / 保证 `leave_nesting`（覆盖所有
> `?` 提前返回路径），实体只做递归。同时把 `checkpoint()/rollback()` 扩展为
> 三要素（pos / 诊断数 / `depth_reported`）：推测分支回滚时恢复
> `depth_reported`，避免“推测路径消耗唯一一次深度诊断后，真正路径的防护静默
> 截断”的回归。

---

## 3. B 类：合法输入被拒（sema 收不到 AST）—— ✅ 已修复

以下输入在文法或 `design.md` 中是合法的，但 parser 曾直接报错/返回 `None`：

| ID | 输入 | 修复方式 |
|---|---|---|
| B-01 | `with ctx { }` | `parse_with` 改为文法的无括号表头（`With expression (As binding)? block`）；旧括号形态继续解析但提示「with 表头无需括号」 |
| B-02 | `(A \| B)*`、`(i32, i32)[]`、`(A)*` | 抽出共用 `parse_type_suffixes`；`(` 分支解析后统一走后缀循环，分组 `(T)` 返回内层 |
| B-03 | `() => 1` | `parse_paren_or_lambda` 重写：先整体试探 lambda（`try_parse_paren_lambda`），失败回滚再按分组/元组解析，不再用 `params.is_empty()` 误拒 |
| B-04 | `(x, y: int) => x` | 同上，删除只查三 token 的 `looks_like_typed_lambda` |
| B-05 | `x: int => x` | 标识符后遇 `:` 时 checkpoint 试探 `parse_return_type` + `=>`，成立则升格 lambda（与三元冒号歧义用 rollback 解决） |
| B-06 | `for (a[0] = 1; x; x++) { }` | `looks_like_bare_declaration` 扫描集补 `[` `.` `{` → 判表达式 |
| B-07 | `g<func (a: int) -> int>(1)` | `is_type_token` 补 `Func` / `Colon` |
| B-08 | `new (Foo)()` | 见 C-04：`new` 后解析整条后缀表达式，目标不再限于标识符点链 |
| B-09 | `{ x: 1 };` 作为表达式语句 | **规范拍板：不合法**（E-04）——语句位 `{` 一律是 block，见 §6 |

---

## 4. C 类：AST 结构误判（sema 看到错误程序）—— ✅ 已修复

| ID | 输入 | 修复方式 |
|---|---|---|
| C-01 | `with (e as (a, b)) { }` | `looks_like_with_binding` 重写：`as` 后 `(` + 标识符 + `,`/`)` 判元组绑定；配合 E-02 的绑定优先规则，`parse_cast` 在 with 表头让位 |
| C-02 | `while (x) ;`、`for (;;) ;` | `Statement` 新增 `Empty` 变体；语句位孤 `;` 产节点（顶层 `;` 仍跳过） |
| C-03 | `for (x = g(() => { for (a in b) { } }); x < 3; x++) { }` | `looks_like_for_in` 的 `In` 命中要求 `depth == 0` |
| C-04 | `new getFoo()()` | New 分支重写：`new` 后解析整条后缀表达式；顶层是 `Call` 则提升（`callee`→`target`、`type_args`/`args`→`New`），否则整条即 `target`、实参为空 |
| C-05 | `match y { default => 0 default => 1 }` | 重复 `default` 报「match 中只允许一个 default 分支」并丢弃后者；`default` 后再出现 `case` 报「case 分支不能出现在 default 之后」（仍保留，供 sema 使用） |
| C-06 | `let a, b;` | `Statement` 新增 `VariableList { declarations }` 变体，多绑定不再包 `Block` |

---

## 5. D 类：AST 信息丢失（sema 无法恢复）—— ✅ 已修复

| ID | 写法 | 修复方式 |
|---|---|---|
| D-01 | `new Foo<int>(1)` | `Expression::New` 增加 `type_args: Vec<TypeExpr>`；与 C-04 的提升一并落地 |
| D-02 | `async x => x` | `parse_single_param_lambda` 带 `(token, start, is_async)` 签名，`async` 快道传真值；`: T` 返回注解也并入 |
| D-03 | `class C { @a x: int; }` | 字段装饰器保留进 `FieldMember.attributes`（E-01 拍板允许） |
| D-04 | `pub trait T {}` | `TraitItem` 增加 `scope: Option<Scope>` |
| D-05 | `@dec extern func f();` | `parse_extern_function` 接收并写入 `attributes` |
| D-06 | `struct S(A) {}` | `StructItem` 增加 `heritage: Vec<TypeExpr>`；继承合法性由 sema 判定 |
| D-07 | 函数声明 span | `parse_item` 统一取 `item_start = peek().span.start` 传入各 `parse_xxx`，span 起点恒为首个修饰 token |
| D-08 | `from "std:exception" import ...` | `parse_import` 按首个 `:` 切出 `ImportItem.prefix` |
| D-09 | `pub class C {}` | 同 D-07 |
| D-10 | `type F = <T>(T) => T;` | **规范拍板：类型位 `=>` 不支持**（E-03），`preprocess::rule_out_arrow_type` 整体删除，见 §6 |

---

## 6. E 类：规范拍板结果（2026-11-08 逐项确认）

| ID | 冲突点 | 拍板结论 | 落地 |
|---|---|---|---|
| E-01 | 字段装饰器 | **允许** | parser 保留 `FieldMember.attributes`；`design.md` §4.5 补充字段为可修饰对象 |
| E-02 | `with` 头部 `as` | **绑定优先**（含 tuple 绑定）；写 cast 需双层括号 `with ((e as T))` | `with_shield` 屏蔽嵌套括号/实参内的 `as`；`parse_cast` 只在 `with_shield == 0` 时让位 |
| E-03 | 类型位的 `=>` | **不支持**：类型箭头唯一为 `->` | 删除 `preprocess::rule_out_arrow_type`；类型位遇 `=>` 由 `types.rs` 就近报「类型位不支持 `=>`，类型箭头应为 `->`」并按 `->` 恢复；例外：lambda 返回注解与 match 的 `: T` 模式等内嵌类型位经 `allow_arrow_after_type` 宽容 |
| E-04 | 裸对象字面量语句 | **不合法** | 语句位 `{` 一律按 block 解析（现状即此），B-09 随之关闭；design.md 明确 |
| E-05 | `constructor(...)` / 裸 `name(...)` 省略 `func` | **都不保留**，一律要求 `func` | parser 对二者报「方法声明须以 `func` 开头」（为恢复仍按方法解析）；构造语义由特殊方法 `__init` 承载 |
| E-06 | 方法 `weak` | **不允许** | parser 报「`weak` 不能修饰方法（weak 仅用于字段 / 变量）」 |
| E-07 | 元组 `()` / `(a,)` | **语法错误**（表达式位与类型位都拒） | `parse_paren_or_tuple` / `parse_type_arrow_or_primary` 按 arity 报诊断（仍产出 Tuple 节点供恢复） |

---

## 7. 可以下沉给 sema 的问题（不列入上面的硬清单）

下面这些只要 AST 保留信息，就可以由 sema 承担；它们不属于本文的“无法兜底”：

- `try {}`：AST 已有空 `catches` + `None` finally，sema 可按“catch+ 或 finally”拒绝。
- `for` 初始化中的显式 `let/const`：AST 已保留 `VariableType`，sema/规范层可决定。
- class 多父类：AST `parent` 是 `Vec`，sema 可按“只允许一个”拒绝。
- `pub trait` / `@dec extern` / `struct` 继承：AST 字段已补齐
  （`TraitItem.scope` / `FunctionItem.attributes` / `StructItem.heritage`），
  合法性校验归 sema。

（原清单中的 `weak` 方法、省略 `func`、元组 arity 三项已由 E-05 / E-06 / E-07
拍板为 **parser 层直接拒绝**，不再下沉。）

---

## 8. 修复顺序建议（已按此执行）

1. **A 类（崩溃/深度泄漏）**：parser。优先做，任何不可信输入都能让编译器 abort。
2. **B 类（合法输入被拒）**：parser。修 `with`、复合类型后缀、lambda 形态、
   for-init 探测、泛型调用 token 集、New 目标。
3. **C 类（结构误判）**：parser。修 with tuple 绑定、空语句、for-in 的 `In`
   深度、New 的 Call 提升、match default、let 多声明的分组语义。
4. **D 类（信息丢失）**：先补 AST 字段（New.type_args、Trait.scope、
   Struct.heritage 等），再决定校验放 parser 还是 sema；函数 span 直接修 parser。
5. **E 类（规范）**：先拍板，再同步 parser / 文法 / design。
6. 为以上每条补最小回归用例 →
   [`crates/novella-parser/tests/boundary_regression.rs`](../crates/novella-parser/tests/boundary_regression.rs)。

---

## 9. 复现方式

```bash
cargo test --workspace          # 回归套件（含 boundary_regression）
cargo check --workspace --all-targets
target/debug/novella compile <case.nov> -o /tmp/out.json
```

深度防护用例（A 类）由 `boundary_regression.rs` 的
`a_nesting_guard_blocks_pathological_input` 覆盖（输入规模在测试内可控，
断言报「语法嵌套层级过深」而非栈溢出），不再需要子进程探针。

---

## 10. 维护说明

- 本文只记录 **parser 不能交给 sema** 的问题；语义规则、类型检查、可见性规则
  的完整清单应在 sema 文档中维护。
- 修掉一项后，应从本文删除对应条目，或移到“已下沉给 sema”的章节。
- 若给 AST 增加字段，必须同步 `design.md` 的 AST 描述与 `novella-ast` 的
  serde/消费者，避免再次出现“字段存在但 parser 不填”的静默丢失。

---

## 11. 修复记录（2026-11-08）

涉及面（相对 `cce23e9`）：

- **AST**：`Expression::New.type_args`、`TraitItem.scope`、`StructItem.heritage`
  新增字段；`Statement` 新增 `Empty` / `VariableList` 变体。
- **parser**：`expression.rs`（递归守卫包装、New 提升、lambda 试探重写、
  match default 诊断、with 绑定判定、单参 lambda 注解）、`types.rs`
  （守卫包装、共用后缀循环、元组/分组 arity、`=>` 拒绝与宽容开关）、
  `stmt.rs`（无括号 with、`Empty`、`VariableList`、for-in/for-init 探测修正）、
  `lib.rs`（`item_start` 统一 span 起点、import prefix、trait/extern/struct
  字段补齐、`weak`/省略 `func` 诊断、`error_at_span`、checkpoint 三要素）、
  `preprocess.rs`（删除 `=>` 改判，只保留 from/import 降级）。
- **文法**：`NovellaParser.g4` 同步（trait 可见性、struct heritage、
  extern 装饰器、空语句/多绑定/New/lambda 注释）。
- **设计**：`design.md` §4.2 / §4.5 / §4.6 / §4.7 / §4.8 / §4.9 / §4.10 / §5.1
  同步。
- **测试**：`crates/novella-parser/tests/boundary_regression.rs`（23 个用例，
  按条目命名）；`novella-parser` 新增 `dev-dependencies: novella-lexer`。
- **示例**：`examples/novella-syntax-demo.nov` 的 `with` 表头迁移为无括号形态。

行为变化（面向使用者的迁移点）：

- `with (expr) { }` → 改写 `with expr { }`（旧形态仍可解析但有迁移诊断）；
- 类型位 `=>` → 改写 `->`（如 `type F = <T>(T) => T;` → `(T) -> T`）；
- 类方法 `constructor(...) { }` / `name(...) { }` → 改写 `func name(...) { }`；
- `weak func m()` → 删除 `weak`；
- `()` / `(a,)` 元组 → 删除（不再合法）。

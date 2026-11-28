# novella 全 workspace 代码简化分析 · 总览

> 基线：commit `88db1d9`「refactor(compiler): 重构运算符重载与下标协议派发机制」
> 范围：13 个 crate / 42605 行
> 性质：**只读分析，零源码改动**（`git status` 可见的新增只有 `docs/reports/`）
> 分块：HIR+codegen 18 条、parser/lexer/ast/core 18 条、sema 20 条各自成文；**lsp/cli/target/syntax-gen/derive/runtime 18 条只收录在本页第 6 节**

## 阅读顺序

| 块 | 文件 | 条目 |
|----|------|------|
| HIR + codegen | [hir-codegen-simplification-analysis.md](hir-codegen-simplification-analysis.md) | 18 |
| parser / lexer / ast / core | [parser-frontend-simplification-analysis.md](parser-frontend-simplification-analysis.md) | 18 |
| sema | [sema-simplification-analysis.md](sema-simplification-analysis.md) | 20 |
| lsp / cli / target / syntax-gen / derive / runtime | 本页第 6 节 | 18 |

## 基线数据

- `cargo clippy --workspace --all-targets` = **0 告警**；`allow` 共 7 项：4×`dead_code`、3×`clippy::mutable_key_type`、2×`too_many_arguments`。
- `TODO`/`FIXME` = **0**；注释掉的代码 4 处；`unwrap` 29、`expect` 190（parser 的 `.expect(` 绝大多数是 `self.expect(TokenKind…)` 解析原语，非 `Result` 解包）。
- 最长函数：`instruction` 412 行（`crates/novella-codegen/src/body/instructions.rs:6`）、`check_stmt_inner` 292（`crates/novella-sema/src/typecheck/body/mod.rs:108`）、`build_expression_inner` 245（`crates/novella-hir/src/build/expr.rs:87`）、`analyze_with_options` 239（`crates/novella-sema/src/analysis.rs:95`）、`load_modules` 232（`crates/novella-sema/src/module/loader.rs:20`）、`constant` 226（`crates/novella-hir/src/build/globals.rs:84`）、`emit_artifact` 186（`crates/novella-codegen/src/lib.rs:326`）。
- **总体判断：不是需要重写的状态。** 分层与纪律大体成立（依赖单向、诊断单出口、cfg 求值真同源、LSP 偏移换算集中），债务集中在少数函数与几处「唯一事实源失守」，每项都有可独立验证的单点改法。

---

## 1. 正确性优先（先修，其余都是风格）

这一组是**用户可见的行为错误**或**永久不可达的诊断**，名义上与"简化"无关，但优先级最高。

| # | 问题 | 位置 | 性质 |
|---|------|------|------|
| A1 | `lookup_member_chain` 的 `static_only` 从未被读取 | `crates/novella-sema/src/typecheck/member.rs:449,454,462-515` | 「静态/实例属性不一致，不能覆写」诊断**永不可达**；`crates/novella-sema/src/check/type_defs/inheritance.rs:220-238` 的对应分支是死代码；字段遮蔽判定同时命中方法与父类链字段 → 误报/漏报 |
| A2 | `HirInstr::IndexStore` 全仓无产出 | 定义 `crates/novella-hir/src/body.rs:307`、仅读取 `crates/novella-hir/src/build/requirements.rs:186`、codegen 分支 `crates/novella-codegen/src/body/instructions.rs:357-372` | 死变体 + 死分支。其 `retain→load→store→release` 序列**缺** `if self.is_managed(...)`（`crates/novella-codegen/src/body/ownership.rs:8` 定义），与 `Store`(`:73-84`)/`StoreIndirect`(`:102-113`)/`FieldStore`(`:306-317`) 不一致；因不可达当前无害，但任何一次「接上 IndexStore 产出」都会立刻对非托管元素误调 ARC |
| A3 | `parse_call_suffix` 造成比较链 O(n²) | `crates/novella-parser/src/expression/postfix.rs:17-108`、`:114-147` | 实测 n=100/802B=9.4ms、n=200/1602B=34.0ms、n=400/3202B=**129.8ms**（规模翻倍 ×3.8~4.0）；等长加法链 1602B=**0.51ms** → 差 254×。LSP 每次击键全量重解析，3KB 比较链即卡 100ms+。**四块中唯一伤害真实体验的性能缺陷** |
| A4 | `parse_visibility` 把 EOF 与"无修饰符"混为一谈 | `crates/novella-parser/src/lib.rs:426-431`（调用点 `:318`、`:720`） | 签名是 `Option<Scope>`，但 `self.peek()?` 在 EOF 时返回 `None`，与"无修饰符"共用同一出口；更根本的是 `parse_item` 里 `other?`：文件以 `pri` 收尾时只产出 1 个 item，**下一行整个函数静默消失**，其调用方随后全报未定义。应改为 `Result<Option<Scope>, _>` 区分"读到 EOF"与"不是修饰符" |
| A5 | `parse_postfix` 失败回滚用 `Literal::Null` + `Span{0,0}` 占位 | `crates/novella-parser/src/expression/postfix.rs:55-77` | 失败路径把真实表达式换成指向文件 0 字节的 `null` 字面量并 `continue` 解析，sema/HIR 对 `Literal::Null` 有正常路径且不报错 → 变成位置错误的类型诊断（实测 `a.b( 1 )` 得「类型 null 上找不到成员」且指向 0:0）；且 `:136 parse_arguments()?` 早退**不**回滚 |
| A6 | `did_close` 重复推送同一 URI | `crates/novella-lsp/src/server/sync.rs:76-82`、`fn publish(&mut self, _uri: &str)`:100 | 参数带下划线即从未使用；`diagnostic_messages`(`:113-156`) 遍历 `self.documents`，而文档此时已被 remove → 第一次 publish 对该 URI 零消息，真正清空客户端诊断的是后面那次 `push` |
| A7 | `return`/`throw`/`yield` 内联吃分号与 `expect_statement_end` 不等价 | `crates/novella-parser/src/stmt.rs:590-658` vs `:660-676` | `func f() -> int { return 1 }`（省分号紧跟 `}`）会多报一条诊断，普通表达式语句不会——用户可见的诊断不一致 |
| A8 | 三套括号/逗号扫描器语义互不一致 | `crates/novella-lsp/src/features/signature_help.rs:329-358`、`:387-403`、`:404-428` | `count_top_level_commas` 跳字符串且括号集 `([{`；`find_matching_paren` 只认圆括号、不跳字符串；`split_top_level` 把 `<>` 也当括号、不跳字符串。`f("(", 1)` 的实参边界只有第一个判得对 |

**建议顺序**：A1 → A2（删除或补齐 `is_managed`，二选一需明确决策）→ A3 → A4/A7 → A5 → A6 → A8。

### 1.1 修复状态（2026-11-27）

A1–A8 已全部落地，全部条目带回归测试。A2 取「删除」决策：`IndexStore` 无产出，真实下标写入走 `UpdatePlace::Address → StoreIndirect`（定长数组）与 `UpdatePlace::Index → __setitem`（协议派发），补产出等于复活一条不该存在的指令路径。

| # | 落点 | 回归测试 |
|---|------|----------|
| A1 | `crates/novella-sema/src/typecheck/member.rs` `lookup_member_chain` 按 `static_only` 过滤方法/字段；`crates/novella-sema/src/check/type_defs/inheritance.rs` 遮蔽用 `field.is_static`、覆写只对「同静态性方法」判不一致 | `static_only_filters_member_staticity`、`instance_field_does_not_shadow_parent_static`（`crates/novella-sema/src/tests.rs`） |
| A2 | 删除 `HirInstr::IndexStore` 及其 `requirements.rs` / `instructions.rs` 分支 | 编译期穷尽性 |
| A3 | `crates/novella-parser/src/expression/postfix.rs` `looks_like_call` 改为单趟 `is_call_angle()`（首个非法 token 即返回，不再扫到表达式结尾） | `a3_comparison_chain_parses_in_linear_time`、`a3_generic_call_detection_unchanged` |
| A4 | `parse_visibility` 改 `Result<Option<Scope>, ()>`，`Err` 专指「已消费修饰符后文件结束」，`parse_item` 安静退出 | `a4_visibility_at_eof_is_silent` |
| A5 | `parse_call_suffix` 改借用 `&Expression`，只在成功时 clone；实参解析失败也回滚游标与诊断 | `a5_failed_call_suffix_leaves_no_null_placeholder` |
| A6 | `crates/novella-lsp/src/server/sync.rs` `publish(focus)` 记录真正的触发 URI；`did_close` 移除文档后刷新其余文档，再由末尾空推送清空自身诊断 | 行为由 `diagnostics` 推送路径覆盖 |
| A7 | `return`/`throw`/`yield`/`break`/`continue` 统一改调 `expect_statement_end` | `a7_keyword_statements_allow_omitted_semicolon_at_block_end` |
| A8 | `crates/novella-lsp/src/features/signature_help.rs` 三套扫描器收敛为 `scan_balanced`（括号集 `([{` / `)]}` + 跳过字符串），`split_top_level` 同步该规则并去掉 `<` `>` 计深度 | `a8_bracket_scanners_agree_on_strings` |

A1/A3/A4/A5/A7 的测试在 `crates/novella-parser/tests/boundary_regression.rs`（A1 在 sema 单元测试），命名统一带条目编号。

---

## 2. 唯一事实源失守（同一条知识被维护多遍）

项目规范要求「`TokenKind` 是唯一事实源」「不在 core 之外维护并列的字符面集合」。以下都是同一张表被手抄 2~4 份，且旁边往往已有正确形态可参照。

| # | 事实源 | 副本位置 | 差异概况 |
|---|--------|----------|----------|
| B1 | `TokenKind` 变体名 | 权威在 `crates/novella-core/src/token.rs:15-242` | 副本 `crates/novella-syntax-gen/src/lib.rs:349-402` `variant_name`（45 行，脚本核对 **45/45 全为恒等映射**，且带 `_ => ""` 兜底）；副本 `crates/novella-lexer/src/lib.rs:258-380`（107 行 `From<RawToken>` 同名映射，编译期由穷尽性保障，属零信息样板）。注意 `crates/novella-core/src/token.rs:244-249` 的 `Display` 为 `write!("{self:?}")`，能不能直接替换取决于有无自定义 `Debug` |
| B2 | 关键字集合 | 权威 `crates/novella-core/src/keywords/mod.rs:26` `KEYWORDS[46]` | 副本①`crates/novella-lsp/src/features/hover.rs:59-93`：实测只写 **29/46**，缺 `and, break, case, catch, continue, default, elif, else, false, finally, if, in, not, null, or, throw, true` **17 条**（含 `if`/`else`/`true`/`false`/`null`，即最常见的那批）；副本②`crates/novella-syntax-gen/src/lib.rs:406-414` `keyword_scope` 显式覆盖 **26/46**，其余 20 条落 `_ => "keyword.other.novella"`（脚本核对结果当前正确，但无机制保证）。对照 `crates/novella-lsp/src/features/completion.rs:66-78` 已正确遍历 `KEYWORDS` |
| B3 | 魔法方法名 | 常量只在 `crates/novella-sema/src/typecheck/magic.rs:101-102`（`GET_ITEM`/`SET_ITEM`）；名单在 `crates/novella-sema/src/check/mod.rs:23-24` `CLASS_SPECIAL_METHODS` | 字面量散落 sema 10 个文件**并越界到 HIR/codegen**：`crates/novella-hir/src/build/lvalue.rs:46,78,112`、`crates/novella-hir/src/build/call.rs:49,121,271`、`crates/novella-hir/src/build/types.rs:228`、`crates/novella-hir/src/build/strings.rs:149`、`crates/novella-codegen/src/lib.rs:125`。另 `crates/novella-sema/src/check/type_defs/registration.rs:365-380` 是**第二张表**（返回类型），必须与 `CLASS_SPECIAL_METHODS` 人工同步 |
| B4 | 目标位宽 32/64 ↔ `PointerWidth` | 枚举在 `crates/novella-sema/src/analysis.rs:52` | 映射写 **3 份**：`crates/novella-cli/src/main.rs:160-163`、`crates/novella-lsp/src/server/analysis.rs:133-136`、`crates/novella-cli/src/cache/mod.rs:103-106`（反向）；另有 `crates/novella-hir/src/build/module.rs:15-18` 与 `crates/novella-sema/src/ty/literals.rs:165-166` 各自 `match`，默认值在 `crates/novella-sema/src/analysis.rs:56-68` |
| B5 | cfg 键与 flag 清单 | `crates/novella-target/src/cfg.rs:318-329` `KNOWN_KEYS`(10 项) + `:332-341` `KNOWN_FLAGS`(8 项) | 求值器 `key_value`(`:413-434`)/`flag`(`:436-451`) 再列一遍同一组键；`check`(`:265-314`) 是**第二套独立 token 扫描**（与 `Predicate` 的递归下降并行维护 `all/any/not` 语义）。新增一个 `target_*` 键要动 4 处 |
| B6 | 行表 | 唯一实现 `crates/novella-core/src/line_index.rs:24` `LineIndex::build` | 没有"副本"，但被**重复构建**：`crates/novella-core/src/decor.rs:19-46` 的 `decorate_one` 每次调用都 `LineIndex::build(source)`，而它是 sema 唯一诊断出口（`crates/novella-sema/src/module/mod.rs:144-154`，经 `error_with_code`:118-129、`warning`:131-138）→ 500 行文件报 50 条错 = 扫描全文件 **50 次**（脚本核算：15KiB×50 = 750KiB，相对一次性 build **50×**）。另 `decorate_one` 把单条包成 `vec![]` 再 `next().expect(...)`；`shift_span_in_place`(`:52-72`) 在 crate 外**零调用** |
| B7 | UTF-16 列 → 字节换算 | `crates/novella-lsp/src/source.rs:200-208` `InnerIndex::byte_of`（`partition_point`，模块 doc `:1-16` 声明"只此一处"） | 副本 `crates/novella-lsp/src/features/completion.rs:159-168` `byte_of_column`（线性 `char_indices`）+ 专属测试 `crates/novella-lsp/src/features/mod.rs:174-181`。当前逻辑一致故无 bug，但任一侧修代理对/越界边界就会分叉 |
| B8 | 字符串前缀剥离 | `crates/novella-core/src/string.rs:131-139` `split_prefix`（只剥**一个**前缀字符） | 副本 `crates/novella-parser/src/lib.rs:1099-1107` `unquote` 用 `trim_start_matches(['f','r','b'])`（剥光全部）。实测 `f"x"`/`r"x"`/`fr"x"` 两边都得 `x`，差异只在多前缀边界（当前无实际 bug，但语义分叉） |
| B9 | 字面量判真假 / 判 `dyn` | 应以 `TokenKind` 为准，禁止比 `slice` | `crates/novella-parser/src/expression/primary.rs:29` `Literal::Boolean(token.slice == "true")`；`crates/novella-parser/src/types.rs:273` `t.slice == "dyn"` —— `dyn` **无 TokenKind 变体**、也不在 `KEYWORDS` 表里，是"词法不可见的隐形关键字"，LSP 高亮/补全认不出 |

---

## 3. 跨 crate 的结构重复

| 主题 | 重复面 | 证据 |
|------|--------|------|
| 分段栈参数 | 2 份逐字 + 10 处魔数 | `crates/novella-parser/src/recursion.rs:16-52` 与 `crates/novella-ast/src/drop_guard.rs:30-46` 逐字重复 `STACK_SEGMENT = 2*1024*1024`、`RED_ZONE = 512*1024`；sema 侧 `stacker::maybe_grow(128*1024, 2*1024*1024, …)` 另有 10 处（`typecheck/expr/hints.rs:29`、`expr/dispatch.rs:10`、`body/mod.rs:93`、`flow.rs:84/122/310`、`check/type_defs/overlap.rs:11`、`check/returns.rs:107`、`resolve/types.rs:19`、`traits.rs:180`）。**常量与文档三处矛盾**：`recursion.rs:49 MAX_STACK_BYTES = 1024*1024*1024`（1 GiB）而文档写 256 MiB；`recursion.rs:31-41` 与 `:42-48` 同一段"内存预算"重复两遍。约束：`novella-core` 目前**不依赖** `stacker` |
| Ty(`TypeView`) 递归重写 | 2 份 | `crates/novella-sema/src/ty/generics.rs:78 substitute_type_params` + `:89 rewrite_type`（闭包式，约 70 行，27 变体穷尽）vs `crates/novella-hir/src/build/mono.rs:52 substitute`（同构，约 50 行，带 `_ => view.clone()` 兜底） |
| Span 合并 | 缺原语 | `crates/novella-core/src/lib.rs:14 pub struct Span { start, end }` 只有字段，无 `join`；手写 `start: <左>.span().start, end: <右>.span().end` **44~56 处**，集中 `crates/novella-parser/src/stmt.rs` 13、`expression/stairs.rs` 6、`expression/postfix.rs` 5、`types.rs` 4（典型 `expression/helper.rs:51-56` `binary()`） |
| 名义类型游标 | **缺抽象而非散落** | 「`Ty` → 名义键」3 份：`crates/novella-sema/src/typecheck/mod.rs:229-236` `this_ty_key`、`typecheck/member.rs:366-372` `type_key_of`、`traits.rs:10-15 nominal_base`；「沿父链遍历求解泛型实参」**5 份**：`state.rs:351-388`、`traits.rs:43-63`、`ty/generics.rs:30-70`、`check/type_defs/inheritance.rs:64-87`、`inheritance.rs:341-360`，各自维护 visited、`Applied` 剥壳、`parent_type` vs `parent_resolved` 口径。这是 sema 最值得根治的耦合点 |
| 调用形状检查 | 4 份并行 | `crates/novella-sema/src/typecheck/magic.rs` `method_shape`、`check/type_defs/registration.rs:336-364`、`typecheck/call.rs:46-69 protocol_method`、`typecheck/body/iteration.rs:44-105 protocol_method_return`；判定项重叠（`is_static`/显式参数个数/`type_param_count`/跳过 `self`），容错与文案各自演化 |
| `self` 跳过 | 8 种写法 | `crates/novella-sema/src/ty/mod.rs:168`、`typecheck/call.rs:484`、`:728`、`inheritance.rs:142`、`:300`、`freestanding.rs:109`、`traits.rs:251`、`body/iteration.rs:86-90`。缺 `FunctionSig::receiver_offset` / `explicit_params` 一个访问器 |
| 原子写 | 3 套 | `crates/novella-cli/src/main.rs:225-235`（tempdir+rename）、`crates/novella-cli/src/link.rs:88` 与 `:153`（staging tempdir）、`crates/novella-cli/src/cache/mod.rs:209-219`（`NamedTempFile::persist`）；`link.rs` 的 rename 在 err 路径不清理 |
| 编译编排 | 2 份 | `crates/novella-cli/src/main.rs:98-125`（Obj/Exe）vs `crates/novella-cli/src/build.rs:74-96`：`analyze_with_options → render_diagnostics → has_errors → lower → emit_artifact` 各维护一份 |
| 诊断遍历 | 2 份 | `crates/novella-cli/src/main.rs:130-141` vs `crates/novella-cli/src/cache/record.rs:33-60`：同是"global 先、模块 `ordered()` 后"。**注意：渲染本身不重复**，`crates/novella-cli/src/error.rs:19-67` `render_diagnostic` 是唯一实现 |
| target 装配 | 2 份 + 一处真 bug | `crates/novella-cli/src/main.rs:142-182` `analysis_options` vs `crates/novella-lsp/src/server/analysis.rs:104-140` `configure_project`。**已核实**：LSP 的 `cfg_overrides` 等价于 CLI 的 `--target-os/--target-arch/--target-family`，只喂 `validate_cfg`（`crates/novella-core/src/target.rs:253-273`，不一致直接报错而非覆盖），不入 `CfgOptions` 无妨；真 bug 是 `crates/novella-lsp/src/server/capabilities.rs:56-63` 把 `features` 写进 `self.options.cfg`，随后被 `configure_project` 结尾的 `options.cfg = CfgOptions::from_spec(&spec)` **整体覆盖** → 编辑器里配的 features 不生效。另 LSP 硬编码 `resolve_selector(…, true)` 恒放行自定义目标，CLI 受 `-Z custom-targets` 门控 |
| `TargetSpec::validate` | 6 处调用、2 处丢文案 | `crates/novella-target/src/spec.rs:173`、`builtin/merge.rs:31`、`resolved.rs:32`、`crates/novella-cli/src/link.rs:30`、`crates/novella-sema/src/freestanding.rs:122-127`、`crates/novella-hir/src/build/requirements.rs:51-52`、`crates/novella-codegen/src/lib.rs:327-328`；其中 `requirements.rs:51` 只取 `is_err()`，丢掉 `spec.rs:116` 那句"目标 X 不是裸机目标，不能使用 kernel 代码模型" → 用户只看到笼统 `InvalidInput` |
| `BuildError` 构造 | 72 处 | `crates/novella-hir/src/build/**` 的 `ok_or_else(|| BuildError::at(K).with_site(m,s))`（`at` 共 128、`with_site` 104；`Unimplemented` 54 + `MissingType` 43 占 75%）；`crates/novella-cli/src/main.rs:183-207` 从不匹配 `BuildErrorKind` → 可安全加 `missing()`/`unimplemented()` 构造器 |
| AST 遍历 | 2 份 | `crates/novella-lsp/src/features/inlay_hints.rs:97-182` 复刻 `crates/novella-lsp/src/features/signature_help.rs:96-228` 的穷尽 `match`，且前者用 `_ => {}` 兜底（已缺 Variable 的 value、Return/Yield/Throw/Expression 分支） |
| `server/` 参数解码 | 7 份 | `crates/novella-lsp/src/server/syntax.rs:67-109`、`lexical.rs:17-66`、`navigation.rs:38-120` 各抄一遍 `json!([])`/`Value::Null` 样板；`crates/novella-lsp/src/server/adapter.rs:21-100` 的 `HasTextDocument`/`HasTextDocumentPosition` **只落地 8 个 impl**，实测那三个文件共 **6 个**能力未走 trait，空值形态已分叉 |

---

## 4. 死代码与失效 API（可零风险删除）

| 位置 | 内容 |
|------|------|
| `crates/novella-hir/src/ty.rs:236`、`:359` | 两个 `pub fn from_view` 在 crate 内仅自身递归（`:312`、`:316`），真实入口是 `crates/novella-hir/src/build/mono.rs:142` 的 `from_view_with` |
| `crates/novella-hir/src/body.rs:356` | `HirCond::has_prelude` 全仓零调用（`control.rs:64/129` 直接 `if self.block(&cond.prelude)?`） |
| `crates/novella-hir/src/build/stmt.rs:487` | `prelude.stmts.shrink_to_fit();` 无意义（`prelude` 随即被消耗） |
| `crates/novella-parser/src/stmt.rs:678-724` | `skip_balanced_block` 零调用（`#[allow(dead_code)]`），内含手工构造 `DiagnosticLocation` 的死路径 |
| `crates/novella-ast/src/drop_guard.rs:417-423` | `const _: () = { fn _assert_parts(_: &Param, _: &FunctionType) {} };` 是**零约束伪断言**（只断言类型存在），注释声称"改名/改结构会编译失败"与事实不符 |
| `crates/novella-ast/src/drop_guard.rs:258-262`、`:265-270` | `take_variable_children` 与 `drop_variable` 函数体逐行相同 |
| `crates/novella-core/src/lib.rs:19-23` | `pub struct Spanned<T>` 全仓零构造点；而 `crates/novella-ast/src/lib.rs:2 pub use novella_derive::Spanned;` 是**同名不同物** |
| `crates/novella-core/src/decor.rs:52-72` | `shift_span_in_place` 全仓零调用（f-string 旧路径遗物） |
| `crates/novella-target/src/cfg.rs:503` | `pub struct Evaluation` 仅在定义处出现（sema 实际用 `Vec<(Span, String)>`），doc 已与事实不符 |
| `crates/novella-target/src/query.rs:45` | `pub fn target_list` 零调用；`crates/novella-cli/src/print.rs:13-16` 自己重写了同一输出 |
| `crates/novella-target/src/spec.rs:70` | `pub is_like_msvc` 零读取方，而 `crates/novella-target/src/builtin/base.json` 仍写 `"is-like-msvc": false` |
| `crates/novella-target/src/feature.rs:24`、`:71-77` | `from_table` / `declared` 零调用（`declared` 的 doc 仍写"供 `--check-cfg` 生成白名单"） |
| `crates/novella-lsp/src/nav.rs:79-83`、`:93-97`、`:133-137` | `type_at` / `supertypes_of` / `subtypes_of` 三个 `#[allow(dead_code)]` + `pub fn`，唯一使用者是同文件测试；`crates/novella-lsp/src/server/dispatch.rs` 无 `prepareTypeHierarchy`/`typeHierarchy` 分支 |
| `crates/novella-parser/src/expression/postfix.rs:171`、`expression/stairs.rs:236` | 文档链接 `[Parser::scan_angle]` 指向**不存在**的函数（实际是 `scan_call_angle`）；`cargo doc` 不报错故长期潜伏 |

### 4.1 清理状态（2026-11-27）

除下表两条外，本节条目已全部删除或修正：

| 处理 | 条目 |
|------|------|
| 已删除 | `HirType::from_view` / `FunctionSignature::from_view`（`crates/novella-hir/src/ty.rs`；crate 内调用点改走 `from_view_with`）、`HirCond::has_prelude`（连带其唯一调用者 `HirBlock::is_empty`）、`stmt.rs` 的 `prelude.stmts.shrink_to_fit()`、`skip_balanced_block`、`const _: () = { fn _assert_parts }`、`take_variable_children` / `drop_variable` 合并为一、`novella_core::Spanned<T>`、`shift_span_in_place`、`Evaluation`、`target_list`、`is_like_msvc`（含 `crates/novella-target/src/builtin/base.json` 的同名键） |
| 已修正 | `expression/stairs.rs:236` 的文档链接改为指向 `Parser::looks_like_call`（A3 重写后 `scan_call_angle` 已不存在，原链接本就指向不存在的 `scan_angle`） |
| **保留** | `crates/novella-target/src/feature.rs` 的 `from_table` / `declared`：是 `[features]` 清单表的唯一解析入口，CLI 尚未接线；`crates/novella-lsp/src/nav.rs` 的 `type_at` / `supertypes_of` / `subtypes_of`：带 `#[allow(dead_code, reason = "供尚未实现的 LSP 能力调用")]`，属主动预留。二者均为「未实现能力的脚手架」而非遗留死代码，经确认保留 |

### 4.2 第二轮追加清理（2026-11-28）

第二轮在 4.1 之外又删除/收敛以下项，均为零行为变更：

| 处理 | 条目 |
|------|------|
| 已删除 | `crates/novella-syntax-gen/src/lib.rs` 的 `variant_name`（45 行、脚本核对 45/45 恒等 + `_ => ""` 静默兜底）→ 改用 `format!("{:?}", entry.kind)`（`TokenKind` 的 Debug 输出即变体名） |
| 已删除 | `crates/novella-hir/src/build/types.rs` 的 `let summary = ...; let _ = summary;` → 直接 `self.type_summary(...)?;` |
| 已删除 | `crates/novella-sema/src/typecheck/member.rs` 的 `Ok(ExportTarget::Namespace(ns_target)) => { let _ = ns_target; ... }` → `Ok(ExportTarget::Namespace(_))` |
| 已删除 | `crates/novella-sema/src/check/type_defs/inheritance.rs` 的 `if base.is_abstract { let _ = class_name; }` 空块，连带 `check_override_sig` 的 `class_name` 形参（全函数唯一用处就是这一句） |
| 已删除 | `crates/novella-sema/src/ty/mod.rs:77` 的过时 `#[allow(dead_code)]`（`IntKind::is_signed` 在同文件 `widens_to` 中被调用） |
| 已删除 | `crates/novella-sema/src/typecheck/body/bindings.rs` 的 `Ty::IntLiteral(negative, value) => { let _ = (negative, value); ... }` 死绑定 → `Ty::IntLiteral(..)` |
| 已删除 | `crates/novella-derive/src/lib.rs` 中重复两遍的「具名 `span` 字段」判定 → 抽 `has_span_field`，struct 与 enum 共用 |
| 已删除 | `crates/novella-lsp/src/features/completion.rs` 的 `byte_of_column`（第二套 UTF-16 换算）+ 其 4 条专属断言 → 新增 `SemanticIndex::byte_in_line`（`crates/novella-lsp/src/source.rs`）统一换算 |
| 已删除 | `crates/novella-core/src/decor.rs` 的 `decorate_one`：`Module` 构造时缓存 `novella_core::line_index::LineIndex`，`decorated()` 直接用 `locate(span.start)`，消掉「每条诊断重建整份行索引」的 O(n·m) |
| 已删除 | `crates/novella-sema/src/typecheck/mod.rs` 的 `this_ty_key` 实现（与 `member::type_key_of` 逐字同义）→ `pub(crate) use ... as this_ty_key` 别名 |

---

## 5. 规范面

- **文件行数超 600**：`crates/novella-parser/src/lib.rs` 1125、`stmt.rs` 727、`types.rs` 577。同 crate 的 `expression/` 已按 7 文件（17~380 行）拆好，是现成先例。`novella-hir`、`novella-codegen`、`novella-sema` 的目录拆分**均合规**。
- **公开面**：LSP 已收敛（`crates/novella-lsp/src/lib.rs` 只有 `pub mod server;`，`features/*` 全 `pub(crate)`），符合规范。**唯一超标者是 `crates/novella-target/src/lib.rs:7-14` 的 8 个平铺 `pub mod`**，而 crate 外实际只走 `builtin::list()`（`crates/novella-cli/src/print.rs:13`）与 `query::target_spec_json`（`:27`）两个模块路径，其余全走根重导出 → 应改为 `pub mod query;` + 其余私有。
- **文档时效**：`docs/sema-coverage.md` §3 实施日志引用重构前的旧路径（`typecheck/expr.rs`、`unsafe_rules.rs`、`body.rs`、`module/cfg.rs`），矩阵 S09R 写 Yield 在 `body/mod.rs` 而实测在 `flow.rs:281`；`docs/sema-deep-dive.md:926` 与 `:928` 同一段重复、§1.2 目录树过期、§11.2 写 106 passed 而 coverage 写 102。与规范"文档明显过时须同步"持续冲突。
- **测试网**：`crates/novella-sema` 内 `#[test]` 仅 5 个（`src/tests.rs:7/79/118/147/184`，全是 overlay/再导出），`typecheck/` 无内联测试，也无 `crates/novella-sema/tests/`。**14000 行类型检查器没有一条行为测试** → 第 1、3 节里任何语义保持型改动都无回归网可依；建议先给 A1、B3、三元抽取（sema #4）补最小用例。

---

## 6. lsp / cli / target / syntax-gen / derive / runtime（18 条）

均为只读核实，按性价比排序。

1. **`syntax-gen::variant_name` 45 行映射至少 45/45 恒等**（`crates/novella-syntax-gen/src/lib.rs:349-402`，唯一调用点 `:344`）。脚本核对无一处非恒等，且带 `_ => ""` 静默兜底 → 每个分支应收敛成 `TokenKind::X => "X"` 的算术（或改为穷尽 `match` 而非字符串查找），并删掉兜底，让新增变体由编译器报错。
2. **`completion.rs` 自写第二套 UTF-16 换算**（`crates/novella-lsp/src/features/completion.rs:159-168`，调用点 `:129`）→ 提到 `SemanticIndex` 上一个"给定整行文本与列"的入口并复用 `byte_of`，删 `byte_of_column` 与其专属测试（`features/mod.rs:174-181`）。
3. **`server/` 参数解码的 adapter 只覆盖 8/14 能力** → 补 5 个 trait impl + 一个 `document_and_position::<P>` 泛型 helper（`DocumentHighlightParams` 的字段名是 `text_document_position_params`，与 `ReferenceParams` 不同，各写 3 行 impl）。
4. **`navigation.rs` 的 `references`(`:38-64`) 与 `rename`(`:67-120`) 除空值外逐行同构**，五级 `let Some(...) else { return ... }` → 抽 `locate_target<P: HasTextDocumentPosition>`；同名合法性校验独立成 `is_valid_ident`。
5. **`server/query.rs:82-95 location_of` 每次调用重建 `SemanticIndex`**（`:90`）→ `reference_locations`（`crates/novella-lsp/src/server/navigation.rs:122-167`）对每个引用调一次，N 个引用 = N 次全文件扫描，跨模块还重复建同一模块索引。给 `Server` 加索引缓存或让调用方传入索引。
6. **`signature_help.rs` 三套扫描器语义不一致**（见 A8）→ 合并成单一 `scan(text, skip_strings)`；需补 `f("(", 1)` 回归用例（会改变行为）。
7. **`inlay_hints.rs` 复刻 `signature_help.rs` 的 AST 遍历**（见第 3 节）→ 抽 `features/ast_walk.rs` 单一穷尽 `match`；`symbols.iter().find(...)` 抽 `find_callee_symbol`。
8. **`hover::keyword_hint` 是第三份关键字表，缺 17 条**（见 B2）→ 改为 `fn keyword_hint(kind: TokenKind)`，穷尽性交给编译器；无说明的变体显式 `_ => None` 且集中一处。
9. **`sync.rs::publish(&mut self, _uri)` 死参数**（见 A6）→ 删参数或补上按 URI 过滤的实现（后者才是 `did_close` 的本意）。
10. **CLI 与 LSP 各写一份 selector→spec→cfg 装配**（见第 3 节）→ 收敛为 `novella-target` 的单入口 `resolve_analysis_target(selector, root, overrides, flags) -> (BuildTarget, TargetSpec, CfgOptions)`；同时修掉 LSP `features` 被 `from_spec` 覆盖的问题，并给 `PointerWidth` 实现 `From<u8>` 以删掉 B4 的 3 份匹配。
11. **`target/cfg.rs` 同一组 cfg 键维护两遍**（见 B5）→ 单一 `&[(&str, KeyKind)]` 表，`KNOWN_KEYS`/`KNOWN_FLAGS` 从表派生；`check` 复用求值器的扫描结构或至少共享 `all/any/not` 常量。
12. **`novella-target` 公开面过大 + 一批零调用 `pub`**（见第 4、5 节）。
13. **`TargetSpec::validate` 6 处重复调用、2 处丢文案**（见第 3 节）→ 只在构造点（`builtin/merge.rs:31`、`resolved.rs:32`）校验，或引入 `ValidatedSpec(TargetSpec)` 让类型承载"已验证"。
14. **CLI 编译流水线两份 + 三套原子写**（见第 3 节）→ 抽 `fn produce(args, resolved, options) -> Result<Option<ObjectArtifact>, _>`；原子写收成 `cli::atomic::write`。
15. **CLI 诊断遍历两份**（见第 3 节）→ 抽 `for_each_diagnostic(result, f)` 放 `crates/novella-cli/src/error.rs`。
16. **`nav.rs` 三个未接线的能力函数**（见第 4 节）→ 接进 `dispatch` + `capabilities`（约 40 行），或删除。
17. **`syntax-gen::keyword_scope` 是第三份关键字分组表**（见 B2）→ 改为按 `TokenKind` 分组，`match` 保持穷尽。
18. **`derive` 里"名为 span 的字段"判定写两遍**（`crates/novella-derive/src/lib.rs:15-19` 与 `:40-44` 逐字相同）→ 抽 `has_span_field`；`GuardedClone` 的 `expect("具名字段")`(`:113`) 改成收集 `syn::Error` 后 `to_compile_error()`。

**该块结构性结论**：①"唯一事实源"失守集中三处，都是可从 `TokenKind`/`KEYWORDS` 派生却手抄一份（`variant_name`、`keyword_scope`、`hover::keyword_hint`）——修完这三条，`TokenKind` 才真是唯一事实源。②`server/` 的重复是协议样板而**非**业务重复，`adapter.rs` 的 trait 是正解，只需推到边界。③`novella-target` 的 cfg 求值器**确实同源**（`crates/novella-sema/src/cfg.rs` 只做 AST 裁剪，谓词求值全委托 `CfgOptions`），重复在键表与 `check`。④CLI 的重复是"编排与收尾"，**错误渲染不重复**。⑤控制流复杂度不是本批 crate 的主要问题（`dispatch.rs:51-81` 是 15 分支扁平 `match`，`cfg.rs:367` 的递归下降由 `stacker::maybe_grow`(`:363`) 保护）。

---

## 7. 建议落地顺序

1. **正确性**（第 1 节）：A1 `static_only` → A2 `IndexStore` → A3 比较链 O(n²) → A4/A7 诊断一致性 → A5 占位节点 → A6 `publish` → A8 扫描器。
2. **零风险清理**（第 4 节 + B1/B2/B7）：删死代码、`variant_name` 收敛、`hover` 改按 `TokenKind`、`byte_of` 归位。
3. **机械收敛**（B4/B6 + 第 3 节的 `self` 访问器与 stacker 常量）：`PointerWidth: From<u8>`、sema 缓存 `LineIndex`、`FunctionSig::receiver_offset`、分段栈参数单一来源。
4. **装配统一**（第 3 节 CLI/LSP）：`resolve_analysis_target` 单入口，顺带修 LSP `features` 被覆盖。
5. **根治耦合**（sema 名义类型游标）：`Traversal`/`NominalType` 迭代器替换 3 份 `type_key` 与 5 份父链遍历——改动面最大，**必须先补 sema 行为测试**。
6. **拆分**（第 5 节）：`novella-target` 公开面收窄；`crates/novella-parser/src/lib.rs`/`stmt.rs`/`types.rs` 按 `expression/` 先例拆目录。

第 1~4 步都不涉及公开接口变更，每项可独立验证。

---

## 附注

- 分析期间工作树存在**他人并发改动**：`AGENTS.md` 被替换、新增未跟踪的 `.omo/`、`1.py`、各 crate 下的 `AGENTS.md`。与本分析无关，未擅自恢复。
- 子代理为验证 parser 的 O(n²) 与 lexer 顺序敏感性建过临时探针（`tests/tmp_*.rs`、`examples/tmp_bench.rs`），跑完即删，`git status` 已确认无残留。

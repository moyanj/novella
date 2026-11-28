# novella-parser / lexer / ast / core 只读代码简化分析

> 范围：`crates/novella-parser`（12 文件，4599 行）、`crates/novella-lexer`（582 行）、`crates/novella-ast`（1855 行）、`crates/novella-core`（1920 行）。
> 基线：commit 88db1d9「refactor(compiler): 重构运算符重载与下标协议派发机制」。
> 方法：只读，未修改任何源码；全部行号经 `grep`/`read` 实测。为验证两条结论，临时在 `tests/`、`examples/` 下建过探针测试，跑完即删（`git status` 确认无残留）。按影响从高到低排序，共 18 条。

---

## 1. `parse_call_suffix` 与 `looks_like_call` 在 `parse_postfix` 中造成 O(n²) 重解析，32× 于等长表达式

- **文件**：`crates/novella-parser/src/expression/postfix.rs:17-108`（`parse_postfix`）、`postfix.rs:114-147`（`parse_call_suffix`）、`postfix.rs:151-168`（`looks_like_call`）、`postfix.rs:172-228`（`scan_call_angle`）
- **证据**：`parse_postfix` 的循环对每个 `<` 先调 `looks_like_call()` 做**全量前扫** `scan_call_angle`；判定通过后再调 `parse_call_suffix`，后者在 `!check(OpenParen)` 时回滚：
  ```rust
  // postfix.rs:126-132
  if !self.check(TokenKind::OpenParen) {
      self.pos = saved;
      self.diagnostics.truncate(saved_diagnostics);
      return None;
  }
  ```
  而 `parse_call_suffix` 内部又调 `parse_call_type_args` → `parse_type_expr`（完整类型解析），失败后再整体回滚。由于每一层 `a < ...` 的 lhs 都是下一个 `parse_relational` 的起点，比较链被反复重扫。
- **实测**（临时探针，`Parser::parse` 单次耗时）：
  | 输入 | 长度 | 耗时 |
  |---|---|---|
  | `a < a < … < b > 1 > 1 …;` n=100 | 802 B | 9.4 ms |
  | 同上 n=200 | 1602 B | 34.0 ms |
  | 同上 n=400 | 3202 B | **129.8 ms** |
  | `a + a + … + b;`（等长对照，无 `<`） | 1602 B | **0.51 ms** |
- **影响**：n 翻倍耗时 ×3.8~4.0，是纯二次复杂度；同等长度的普通表达式快 254 倍。LSP 每次击键都要全量重解析，一段 3 KB 的比较链即可让补全卡顿 100 ms 以上；病态输入（几千层 `<`）会直接把编辑器拖死——这是四个 crate 中**唯一会伤害真实用户体验的性能缺陷**。
- **建议**：`scan_call_angle` 已经是线性扫描，让它**同时返回**「闭合 `>` 之后的下一个 token」与「尖括号内的 token 区间」，`parse_call_type_args` 改为消费这份已扫描结果（或在 `looks_like_call` 为真的前提下直接建立 `checkpoint` 并**只尝试一次**、失败后由 `parse_relational` 接管，不再每次重新扫描）；`parse_postfix` 的 `<` 分支只在 `scan_call_angle` 成功时进入，避免「先全扫再全扫再回滚」。补充回归测试：断言 n=400 比较链的耗时与 n=200 同量级。

## 2. `recursion.rs` 与 `drop_guard.rs` 各写一份分段栈参数，且注释与常量值三处互相矛盾

- **文件**：`crates/novella-parser/src/recursion.rs:16-52`、`crates/novella-ast/src/drop_guard.rs:30-46`
- **证据**：两份常量逐字重复（`STACK_SEGMENT = 2 * 1024 * 1024`、`RED_ZONE = 512 * 1024`），靠 `drop_guard.rs:32-33` 的一句注释「与 parser 侧保持同一组参数」人工同步。而 `recursion.rs` 的文档已漂移成三段互相矛盾的描述：
  - `recursion.rs:20-22`：「512 KiB 一段仍可容纳数十层」「取 512 KiB 而非更大的值，是因为…4 MiB 一段时…」——常量实际是 **2 MiB**；
  - `recursion.rs:24`（常量本身）`const STACK_SEGMENT: usize = 2 * 1024 * 1024;`
  - `recursion.rs:31-41` 与 `42-48`：**同一段「内存预算上限」文档连续出现两遍**（两份措辞略有差异的稿子都留下了）；
  - 两份文档都说「256 MiB 预算 = 512 段」「封死在 256 MiB 内」，但 `recursion.rs:49` 实际是 `pub(crate) const MAX_STACK_BYTES: usize = 1024 * 1024 * 1024;`（**1 GiB**）。
- **影响**：`MAX_STACK_BYTES` 是「病态输入的内存兜底」这一安全边界的唯一闸门，注释声称 256 MiB、代码实际 1 GiB（4 倍差）。维护者按注释调参会把真实上限改错方向；两份段参数失去同步则会让「解析期可解析、析构期仍栈溢出」（NOTES 里记录的 2000 层 `unsafe` abort 就是这类不对称）。三份矛盾的注释也说明这块已被反复修改且未收敛。
- **建议**：把 `STACK_SEGMENT` / `RED_ZONE` 提到一处共享定义（`novella-core` 目前不含 `stacker` 依赖，可选 `core::stack` 模块承载常量、`stacker` 调用仍留在两 crate，或新开 `novella-ast` 不依赖的 `novella-stack` 小 crate）；删除重复的文档块，把 `MAX_STACK_BYTES` 与文档统一为同一个数值（1 GiB 或 256 MiB 二选一，并把「段数 = 预算 / 段大小」的推导写进注释而不是手算）。

## 3. `postfix.rs` 用 `Literal::Null` + `Span{0,0}` 占位 + `GuardedClone` 快照来实现回滚，失败路径会留下假节点

- **文件**：`crates/novella-parser/src/expression/postfix.rs:55-77`
- **证据**：
  ```rust
  let saved_expr = expr.clone();                       // 每层调用点整树深拷贝
  let callee = std::mem::replace(&mut expr, Expression::Literal {
      value: Literal::Null,
      span: Span { start: 0, end: 0 },                 // 假位置
  });
  match self.parse_call_suffix(callee) {
      Some(call) => expr = call,
      None => { self.rollback(checkpoint); expr = saved_expr; break; }
  }
  ```
- **影响**：① `expr.clone()` 是 `GuardedClone` 深拷贝，在链式调用（`a.b().c().d()`）上每次后缀都复制整棵已构造子树，与 #1 叠加放大开销；② 占位节点带 `Span{0,0}`，一旦 `parse_call_suffix` 内部提前 `?` 返回（`parse_arguments` 失败时**不会**回滚，见 `postfix.rs:136` 的 `let args = self.parse_arguments()?;`——它直接 `return None` 而 `expr` 已被替换），调用方拿到的是指向文件 0 字节处的 `null` 字面量；下游 sema/HIR 对 `Literal::Null` 有正常路径（`crates/novella-sema/src/typecheck/expr/strings.rs:6`、`crates/novella-hir/src/build/aggregate.rs:300`），伪节点不会当场报错，而是变成一条位置错误的类型诊断。
- **建议**：不要在 `expr` 槽位里放占位值——把 `parse_call_suffix` 改成 `fn parse_call_suffix(&mut self, callee: Expression) -> Result<Expression, Expression>`（返回原 `callee` 以便失败时复原），或用 `Option<Expression>` + `take()/put_back()`；同时把 `Span{0,0}` 替换为 `EMPTY_SPAN` 具名常量（`drop_guard.rs:40` 已有同名常量，但语义不同，需另起名如 `PLACEHOLDER_SPAN`）并在 `Debug`/JSON 输出中显式标注为占位，便于抓出泄漏。

## 4. 三个「守卫包装 + `_inner`」接口的命名与职责不一致，`parse_postfix` 是唯一自递归却无守卫的入口

- **文件**：`crates/novella-parser/src/expression/postfix.rs:17`（`parse_postfix`）、`postfix.rs:248-258`（`parse_arguments` / `parse_arguments_inner`）、`crates/novella-parser/src/types.rs:503-513`（`parse_typed_params` / `parse_typed_params_inner`）
- **证据**：NOTES 要求「递归入口一律守卫包装 + `_inner` 实体，包装层调用 `self.guarded(...)`」。现状分三种写法：
  - **真守卫**：`stairs.rs:23-35`（`parse_expression`）、`40-41`、`67-68`、`330-331`、`primary.rs:9-10`、`match_expr.rs:18-19`、`types.rs:34-35`、`stmt.rs:25-33/58-59/187-192` —— 共 10 处，全部合规；
  - **假包装**（有 `_inner` 名字但包装层只做 `with_shield` 计数，不调 `guarded`）：`postfix.rs:248-258` 与 `types.rs:503-513`，两处的注释都写着「实体（with 屏蔽由外层包装负责）」——用词与真守卫的「分段栈扩展由外层包装负责」几乎相同，**极易被误读成已加守卫**；
  - **裸单层**：`postfix.rs:17 parse_postfix` 不在 NOTES 的守卫列表里，本身不自递归（`parse_primary → parse_primary_atom → parse_postfix` 这条链上的守卫由 `parse_primary` 提供），但它是「后缀累积」的唯一入口，未来若在这里加入任何回退到表达式层的后缀（调用实参里的 lambda、索引表达式等）就会绕过红区保护。
- **影响**：命名把「分段栈守卫」与「with 上下文屏蔽」两种完全不同的包装混成一个 `_inner` 后缀，读代码与做审计时无法从签名判断哪层是栈保护；`parse_postfix` 的缺失是「当前安全、结构上不安全」的隐患。
- **建议**：把两种包装的命名区分开——分段栈用 `guarded` + `_inner`（统一 `fn foo(&mut self) -> Option<T> { self.guarded(|p| p.foo_inner())? }`），上下文屏蔽用 `with_paren_shield(f)` 这类具名辅助函数而不是 `_inner` 方法；`parse_postfix` 外加一层 `guarded` 包装（成本是每层多一次 `remaining_stack()` 读取，可接受），或在 `postfix.rs` 顶部注释明确「本层不自递归，栈保护由 `parse_primary` 提供」。

## 5. `Parser` 的 6 个歧义上下文标志散落在一个 struct 里，且 3 个依赖手工 `+= 1 / -= 1` 配平

- **文件**：`crates/novella-parser/src/lib.rs:65-76`（字段声明）、`lib.rs:88-93`（构造）、使用点散在 `stairs.rs`（4 处）、`match_expr.rs`（10 处）、`primary.rs`（9 处）、`types.rs`（8 处）、`stmt.rs`（2 处）、`lib.rs`（12 处）
- **证据**：
  ```rust
  pub(crate) with_stack: u32,                  // with 表头层数
  pub(crate) with_shield: u32,                 // 括号屏蔽深度
  pub(crate) allow_arrow_after_type: bool,     // 类型位宽容开关
  pub(crate) inside_match: u32,                // match 分支内层数
  pub(crate) in_match_subject: bool,           // 正在解析 match 主体
  pub(crate) match_pattern_boundary: Option<usize>, // case 模式边界字节位
  ```
  其中 `with_stack`/`with_shield`/`inside_match` 三处手工配平，例如 `stmt.rs:536-538` 与 `types.rs:506-509`、`primary.rs:232-235` 都是同一个模式：
  ```rust
  self.with_shield += 1;
  let result = self.parse_xxx_inner();
  self.with_shield -= 1;
  result
  ```
  而 `allow_arrow_after_type`/`in_match_subject` 用的是 `std::mem::replace` + 事后复原（`types.rs:568-575`、`match_expr.rs:27-30`、`match_expr.rs:117-119`）。
- **影响**：同一类「进入上下文 → 解析 → 退出上下文」的语义存在两套写法，且三处 `+= / -=` 都不是 RAII：任何中途 `return None`（`?` 提前返回）都会漏减，造成后续整棵树在错误的上下文中解析——这类 bug 表现为「某个不相关位置突然多出/少了 cast 或 StructInit」，极难定位。`Parser` 结构体也因此承载「游标 + 诊断 + 栈预算 + 五种语法歧义状态」，是 `lib.rs` 膨胀到 1125 行的直接原因之一。
- **建议**：把这些标志收进一个 `struct ParseContext { with_stack: u32, with_shield: u32, allow_arrow_after_type: bool, inside_match: u32, in_match_subject: bool, match_pattern_boundary: Option<usize> }` 并提供 `fn with_shield<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T`（内部 `+= 1` / 守卫对象 `Drop` 复原）与 `fn with_flag(...)`；`Parser` 只保留一个 `ctx: ParseContext` 字段。这不改变任何解析结果，但把「配平」从 8 处调用点收进 1 处。

## 6. `parse_call_suffix` 两处手写位置快照，绕过 `checkpoint()/rollback()` 且漏恢复栈预算

- **文件**：`crates/novella-parser/src/expression/postfix.rs:121-123`、`postfix.rs:128-130`、`lib.rs:169-184`（`checkpoint`/`rollback` 定义）
- **证据**：`lib.rs:164-184` 提供了带注释的正式快照：
  ```rust
  /// 记录游标、诊断长度与递归预算的快照，供试探解析回滚（见 [`Parser::rollback`]）。
  fn checkpoint(&self) -> (usize, usize, usize, bool) { ... }
  ```
  但 `parse_call_suffix` 两处用的是手工局部变量：
  ```rust
  let saved = self.pos;
  let saved_diagnostics = self.diagnostics.len();
  ...
  self.pos = saved;
  self.diagnostics.truncate(saved_diagnostics);
  ```
  **不恢复 `stack_segments` / `stack_exhausted`**，正是 `lib.rs:164` 注释要求快照必须覆盖的两项；同一函数的另一处（`postfix.rs:57`）却用的是 `self.checkpoint()` + `self.rollback(checkpoint)`。
- **影响**：同一个函数里并存两套快照机制。手工那套漏掉栈预算，会让一次失败的泛型实参试探**永久消耗**递归预算（`stack_segments` 只增不减直到下一个成功守卫），在「大量失败调用试探 + 深层嵌套」的组合下提前触发「语法嵌套层级超出工程上限」的误报。`checkpoint` 返回的 4 元组本身也是位置参数，容易调错顺序（见 #10 对 `error_at_span` 的同类批评）。
- **建议**：删除 `saved` / `saved_diagnostics`，统一用 `let snapshot = self.checkpoint();` + `self.rollback(snapshot);`；`checkpoint` 的返回值改为命名结构体（`struct Snapshot { pos, diagnostics_len, stack_segments, stack_exhausted }`），杜绝位置参数错序。

## 7. 13 层二元运算符阶梯是同一段 5 行样板，唯一差异是「token → `BinaryOp`」映射

- **文件**：`crates/novella-parser/src/expression/stairs.rs:126-325`
- **证据**：`parse_coalesce`(126-134)、`parse_logical_or`(137-148)、`parse_logical_and`(151-162)、`parse_bit_or`(165-173)、`parse_bit_xor`(176-184)、`parse_bit_and`(187-195)、`parse_equality`(198-213)、`parse_relational`(216-231)、`parse_shift`(237-250)、`parse_additive`(277-290)、`parse_multiplicative`(293-307)、`parse_is`(310-325) 共 12 处同构：
  ```rust
  let mut left = self.parse_next_level()?;
  while self.check(TokenKind::X) {           // 或 loop + match 取 op
      self.advance();
      let right = self.parse_next_level()?;
      left = binary(BinaryOp::Y, left, right);
  }
  Some(left)
  ```
  `helper.rs:51-62` 已有 `binary(op, left, right)` 负责拼 span，说明「构造」这一步已抽象、只剩「遍历 + 映射」没抽。`parse_equality`/`parse_relational`/`parse_shift` 三个是 `loop { match peek { k => BinaryOp::V, _ => break } }` 变体。
- **影响**：200 行代码承载 12 个几乎相同的循环，占了 `stairs.rs`(371 行) 的 54%。新增一个运算符要在 13 个函数里找到正确层级、复制 5 行、再改 1 个字面量；层级顺序本身只体现在「谁调谁」的调用链上，无法静态校验（没有 `const PRECEDENCE: &[(TokenKind, BinaryOp, u8)]` 之类表格）。
- **建议**：把优先级表抽成数据：`const BINARY_LEVELS: &[&[(TokenKind, BinaryOp)]]`（外层到内层），再用一个「按层号解析」的通用循环 `fn parse_binary_level(&mut self, level: usize) -> Option<Expression>`；`??`/`or`/`and`/`is`/`as` 这些带特殊语义的层保留手写函数并在表中标注为 `Special`。`parse_cast`(253-274) 与 `parse_unary`(330-370) 因带 `with`/`as` 消歧与一元前缀，继续单独存在。

## 8. 「逗号分隔列表 + 允许尾逗号 + expect 闭合」样板重复 4 份，其中 2 份连返回值约定都不同

- **文件**：`crates/novella-parser/src/expression/primary.rs:271-291`（`parse_comma_separated_exprs`）、`primary.rs:293-317`（`parse_array_literal`）、`primary.rs:326-378`（`parse_object_body`）、`postfix.rs:258-276`（`parse_arguments_inner`）
- **证据**：四处主体都是：
  ```rust
  let mut items = vec![];
  if !self.check(TokenKind::CloseX) {
      loop {
          items.push(self.parse_expression()?);
          if self.eat(TokenKind::Comma).is_none() { break; }
          if self.check(TokenKind::CloseX) { break; }
      }
  }
  self.expect(TokenKind::CloseX)?;
  ```
  但返回约定互相不同：`parse_comma_separated_exprs` 返回 `(Vec<Expression>, bool /* 尾逗号 */, usize /* 闭合结束位 */)`，`parse_array_literal` 返回 `Expression`，`parse_arguments_inner` 返回 `Vec<Expression>`，`parse_object_body` 返回 `(Vec<PropertyAssignment>, Span)`。另外 `primary.rs:335-343` 的属性名 match 与 `postfix.rs` 的同类判定各自列举字符串字面量种类。
- **影响**：尾逗号规则（`(a,)` 要报错、`[a,]` 合法、`f(a,)` 合法）是三个独立的语法决定，却分散在四份循环里；任一处漏了 `if self.check(CloseX) { break; }` 就会产生「尾逗号后 parse_expression 报错」的不一致诊断。`parse_comma_separated_exprs` 的三元组返回也让调用方 `primary.rs:237-238` 需要用位置解构。
- **建议**：提取 `fn parse_delimited<T>(&mut self, close: TokenKind, mut item: impl FnMut(&mut Self) -> Option<T>) -> Option<(Vec<T>, bool, usize)>`，统一返回「元素 + 是否尾逗号 + 闭合结束位」，四个调用点各自把闭合种类与元素解析闭包传进去；尾逗号是否合法由调用点在拿到 `bool` 后决定并报告。

## 9. `stmt.rs` 的 `return` / `throw` / `yield` / `jump` 四段「表达式 + 吃分号」逻辑与 `expect_statement_end` 重复

- **文件**：`crates/novella-parser/src/stmt.rs:590-608`（`parse_return`）、`610-620`（`parse_jump`）、`622-640`（`parse_throw`）、`642-658`（`parse_yield`）、`660-676`（`expect_statement_end`）
- **证据**：`parse_return`：
  ```rust
  let value = if self.check(TokenKind::SemiColon) { None } else { Some(self.parse_expression()?) };
  if self.eat(TokenKind::SemiColon).is_none() { self.error_here("语句末尾应为 `;`"); }
  ```
  `parse_yield`/`parse_throw` 逐字重复同一段；`parse_jump` 通过 `make: impl FnOnce(Span) -> Statement` 参数化掉了节点构造，**未参数化**分号处理。而 `expect_statement_end`(660-676) 已经实现了「容忍 `CloseBrace`/`Eof`、否则 `error_here("语句末尾应为 \`;\`")`」的更完整版本——语义比内联版更宽容（内联版在 `}` 前也会报错）。
- **影响**：同一个「语句结束」判定存在两套不等价的实现，`return`/`throw`/`yield` 走的是**较严格**的那套：`func f() -> int { return 1 }`（省略分号、紧跟 `}`）会多报一条 `语句末尾应为 \`;\``，而同样写法的普通表达式语句不会。这是用户可见的诊断不一致。
- **建议**：`parse_return`/`parse_throw`/`parse_yield` 统一改为「`parse_jump` 风格 + 解析完可选表达式后调一次 `self.expect_statement_end()`」，删除三处内联分号判定；把 `parse_jump` 的 `make` 闭包签名扩成 `FnOnce(Span, Option<Box<Expression>>) -> Statement` 后四者共用一个实现。

## 10. `error_at_span` 越界线性扫描 token 流取行列；`stmt.rs` 的死代码绕过它直接构造 `Diagnostic`

- **文件**：`crates/novella-parser/src/lib.rs:230-247`（`error_at_span`）、`crates/novella-parser/src/stmt.rs:678-724`（`skip_balanced_block`，`#[allow(dead_code)]`）
- **证据**：
  ```rust
  // lib.rs:232-237：整条 token 流做线性 find，失配静默退化为 (0,0)
  let (line, column) = self.tokens.iter()
      .find(|t| t.span.start == span.start)
      .map(|t| (t.line, t.column))
      .unwrap_or((0, 0));
  ```
  而 `stmt.rs:697-705` 的死函数里又手工构造了一遍：
  ```rust
  let mut diag = Diagnostic::error("此处缺少与之配对的 `}`").with_code(DiagnosticCode::SyntaxError);
  diag = diag.with_location(DiagnosticLocation { file: None, line: open.line, column: open.column, span: Some(open.span) });
  self.diagnostics.push(diag);
  ```
  `grep` 确认 `skip_balanced_block` **零调用点**、`error_at_span` 仅 `match_expr.rs:43/64/78` 三处调用。
- **影响**：`error_at_span` 是 O(n) 扫描且**失配即静默 `(0,0)`**——`match_expr.rs` 的三处传的是 `close.span` / 分支 span，若将来有任何 span 不是 token 起点（例如合成 span、拆半后的第二段——见 #11 的 `split_token_pair` 产生的第二个 token 其 `span.start` 确实是 token 起点，所以现在侥幸成立），诊断会指到文件首行首列且无任何提示。死函数则是一条「已写好但从未接入」的恢复路径，`#[allow(dead_code)]` 掩盖了它可以被删掉或应当被接上的事实。
- **建议**：`error_at_span` 改为用 `LineIndex`（`novella_core::line_index`，全项目唯一行列换算实现，CONVENTIONS 明确要求）在 `&self.source` 上 `locate(span.start)`——但 `Parser` 当前不持有源码，需在 `ParserInput` 增加 `source: String`（或 `Arc<str>`）并让 `error_here`/`error_at_span` 共用一次 `LineIndex::build`；同时把 `unwrap_or((0,0))` 改成显式的 `debug_assert!` 或「回退到最近的前一个 token 行列」。`skip_balanced_block` 要么接进 `recover_to_statement_boundary`（用它跳过 `{...}` 而不是逐 token 前进），要么直接删除。

## 11. `split_token_pair` / `unquote` / `describe_token` 三个字符串级工具放在 `lib.rs` 尾部，职责不属于「顶层 item 解析」

- **文件**：`crates/novella-parser/src/lib.rs:1062-1094`（`split_token_pair`）、`1099-1111`（`unquote`）、`1114-1117`（`namespace_alias`）、`1119-1125`（`describe_token`）
- **证据**：`lib.rs` 从 1062 行到文件尾（1125 行）全是**与词法/文本处理有关**的自由函数：`split_token_pair` 把 `<<`/`>>` 按字符切半并补偿 `column`（`column: token.column + mid - token.span.start`）、`unquote` 用 `trim_start_matches(['f', 'r', 'b'])` 去前缀再 `trim_matches('"')`、`namespace_alias` 用 `rsplit(['/', '\\', '.'])` 取末段、`describe_token` 做 `TokenKind` → 中文描述。而 `lib.rs` 的另一半是 `Parser` 结构体 + 顶层 item 分发。`preprocess.rs`(46 行) 才是 token 重写的归属地，而 `unquote` 的语义已与 `novella_core::string::split_prefix`（`crates/novella-core/src/string.rs:131-139`，只剥**一个**前缀字符）重复。
- **影响**：`lib.rs` 1125 行里约 60 行是这些工具，但真正的膨胀问题是「`Parser` 的固有方法 + 顶层 item 解析 + 文本工具」三种职责挤在一个文件；`unquote` 对 `fr"..."` 这种组合前缀的处理（`trim_start_matches` 会剥光所有 `f`/`r`/`b`）与 `core::string::split_prefix`（只剥一个字符）**语义不一致**，两处都「解字符串前缀」却给出不同结果。
- **建议**：新建 `crates/novella-parser/src/token_text.rs`（或并入 `preprocess.rs`），把 `split_token_pair`/`unquote`/`namespace_alias`/`describe_token` 整体搬过去；`unquote` 与 `novella_core::string` 的前缀剥离统一为同一份实现（core 提供 `fn strip_literal_prefix(raw) -> (&str, Prefix)`，parser 复用，符合「值语义延后但字面量形态归一化在 core」的分层）。

## 12. `lib.rs` / `stmt.rs` / `types.rs` 均超 600 行规范，且拆分边界与既有 `expression/` 目录的组织原则冲突

- **文件**：`crates/novella-parser/src/lib.rs`（1125 行）、`crates/novella-parser/src/stmt.rs`（727 行）、`crates/novella-parser/src/types.rs`（577 行）
- **证据**：CONVENTIONS 规定「单文件超约 600 行即拆成 `foo/mod.rs` + 若干子模块；`mod.rs` 只放类型定义、模块声明与重导出」。当前 `expression/` 已经按此拆成 7 个文件（17~380 行），而**同级的** `stmt.rs`/`types.rs`/`lib.rs` 却是单文件。`lib.rs` 的构成：`Parser` 结构体与游标原语（44-210）+ 诊断（212-247）+ `parse()`/恢复（249-294）+ 顶层 item 分发与各 item 解析（296-1040）+ 文本工具（1062-1125）。
- **影响**：`expression/` 的拆分先例说明团队认可「按语法产生式分文件」，但顶层 item 与语句、类型没有跟进，导致同一 crate 内两套粒度。`lib.rs` 1125 行让「找 `parse_class` 在哪」变成翻 700 行以上；也直接造成 #11 那类职责混装（文本工具被顺手塞进尾部）。
- **建议**：`lib.rs` → `lib.rs`（`ParseResult`/`ParserInput`/`Parser` 字段/游标/诊断原语，约 250 行）+ `item/mod.rs`（`parse_item` 分发 + `parse_directives`/`parse_visibility`）+ `item/import.rs`（`parse_import` 一族）+ `item/decl.rs`（`parse_class`/`parse_struct`/`parse_trait`/`parse_function`/`parse_extern_function`/`parse_member_block`）；`stmt.rs` → `stmt/mod.rs`（`parse_statement`/`parse_block`/`parse_statement_inner` 分发）+ `stmt/variable.rs` + `stmt/control.rs`（if/while/for/try/with/unsafe）+ `stmt/jump.rs`（return/break/continue/throw/yield/match 语句）+ `stmt/recover.rs`；`types.rs` → `types/mod.rs` + `types/expr.rs` + `types/reference.rs`（`parse_reference_path`/`parse_dynamic_type`/`parse_associated_type_member`）+ `types/params.rs`（类型参数与形参）。

## 13. `RawToken → TokenKind` 是 107 行同名恒等映射，且 `RawToken` 变体名与 `TokenKind` 完全一致

- **文件**：`crates/novella-lexer/src/lib.rs:258-380`（`impl From<RawToken> for TokenKind`）
- **证据**：107 个变体逐一写成 `RawToken::PlusPlus => PlusPlus,` / `RawToken::LeftShiftArithmetic => LeftShiftArithmetic,` ……**每一行左右两侧名字相同**。CONVENTIONS 把这条双枚举定义为「靠 `From` 的 match 穷尽性保证新增变体不漏转」的机制，代价是 107 行纯样板 + logos 属性里再抄一遍 107 个字面量（`lib.rs:13-252`）。同时 `crates/novella-core/src/token.rs:15-242` 的 `TokenKind` 是第三份枚举。
- **影响**：新增一个 token 要改三处（`RawToken` 声明与其 `#[token]`、`From` 映射、`TokenKind`），其中 `From` 那处是零信息增量。虽然穷尽 match 确实能挡住漏转，但「同名枚举 + 同名映射」意味着它挡的只是「忘记加一行」这种本可由 `#[derive]` 或宏消除的错误。`Drop`-style 的机械样板也会稀释真正需要审阅的差异（如 `BooleanLiteral` 同时对应 `true`/`false`）。
- **建议**：用宏一次性生成两侧：`macro_rules! define_tokens { ($( $name:ident : $lit:literal ),* ) => { /* 生成 RawToken 变体 + #[token] + TokenKind 变体 + From */ } }`，把字面量作为唯一数据源；或在保持双枚举不变的前提下，至少把 `From` 改为 `#[derive]` 可生成的形式（如 `novella-derive` 增一个 `#[derive(MirrorToken)]`）。这一步能同时消掉 #16 的「字面量集合散落」压力。

## 14. lexer 声明顺序依赖 logos 的平局裁决，四种字符串各写两条 regex，且「最长匹配」掩盖了关键字前缀问题

- **文件**：`crates/novella-lexer/src/lib.rs:19-24`（`<<`/`>>` 声明在 `<<=`/`>>=` 之前）、`lib.rs:225-242`（四种字符串 × 双/单引号 = 8 条 regex）、`lib.rs:196-210`（关键字与 `BooleanLiteral`）
- **证据**：`LeftShiftArithmetic`(`<<`) 声明在第 19 行，`LeftShiftArithmeticAssign`(`<<=`) 在第 23 行——**长匹配在后**。实测（临时探针）`a<<=b` 正确产出 `LeftShiftArithmeticAssign`，说明 logos 用的是「最长匹配优先，同长取声明在前者」（与 `grammar/NovellaLexer.g4:14-15` 的说明一致），所以当前**没有 bug**；但源码顶部的注释「多字符运算符（优先于单字符）」暗示作者以为是「声明顺序优先」，而 `grammar/NovellaLexer.g4:12-13` 写的是「必须先于同前缀的单字符」——两处注释与真实裁决规则表述不一致。字符串部分 4 个种类 × 2 种引号 = 8 条几乎相同的 regex（仅前缀 `[fF]`/`[rR]`/`[bB]` 与引号字符不同）。
- **影响**：① 顺序敏感性没有任何测试保护（仅有 `keyword_seed_table_matches_lexer` 覆盖关键字，`<<=`/`===`/`->` 这类运算符没有），一旦有人「按字母序整理」运算符表就会静默改变词法；② 8 条 regex 的字符串体部分 `([^"\\\r\n]|\\.)*` 重复 8 次，改转义规则要改 8 处；③ 实测 `ra"x"` 被切成 `Identifier("ra") + StringLiteral`（而不是 `RawStringLiteral("ra...")` 之类更长的复合形态），`1.5e` 被切成 `DecimalLiteral("1.5") + Identifier("e")`——这类「无法匹配处切段」的行为同样是顺序/最长匹配的隐式结果，没有测试锁定。
- **建议**：加一条表驱动词法测试：对 `["<<=", ">>=", "===", "!==", "<=", "->", "=>", "??", "f\"x\"", "r\"x\"", "b\"x\"", "ra\"x\""]` 断言首 token 种类与长度（我已用临时探针确认这些都能跑，直接搬成固定用例即可）；字符串 regex 用 `#[regex]` 的重复声明改为宏生成或提取公共字符类；把 `lib.rs` 顶部注释与 `.g4` 的裁决表述统一为「最长匹配优先，同长取声明在前者」。

## 15. `core::decor::decorate_one` 每次调用都重建整份 `LineIndex`，是 sema 唯一诊断入口

- **文件**：`crates/novella-core/src/decor.rs:19-46`；调用点 `crates/novella-sema/src/module/mod.rs:144-154`
- **证据**：
  ```rust
  // decor.rs:20-22：每次 build 一次行索引
  let index = LineIndex::build(source);
  // decor.rs:41-46：单条版本把一条诊断包成 Vec 再取回
  pub fn decorate_one(diagnostic: Diagnostic, file: Option<&str>, source: &str) -> Diagnostic {
      decorate(vec![diagnostic], file, source).into_iter().next()
          .expect("decorate 保持入参条数，单条入参必有单条输出")
  }
  ```
  `module/mod.rs:146-153` 的 `Module::decorated` 是 `error`/`warning`/`error_with_code` 的共同出口，而 `grep` 显示整个 workspace 里 `decorate`/`decorate_one` **只有这一个消费者**（sema）；`decorate` 的批量版本和 `shift_span_in_place`（`decor.rs:52-72`）**零外部调用**。
- **影响**：`decorate_one` 在 `Module::decorated` 里被每条诊断调用一次，**每次重建整份 `LineIndex`**（`line_index.rs:44` 的 `partition_point` 本身是 O(log n)，但 `LineIndex::build` 是 O(n) 扫描全部换行）。一个 500 行的文件报 50 条语义错误 = 50 次全文件扫描；批量 `decorate` 明明存在却没人用。`shift_span_in_place` 是 f-string 内联片段的旧路径（`crates/novella-sema/src/typecheck/expr/strings.rs:221-223` 的注释明确写着「旧实现的 `adjust_diagnostics`…不需要任何事后平移」），说明这段代码已完成历史使命但未删除。
- **建议**：`Module` 在构造时缓存一份 `LineIndex`（它已持有 `source`），`decorated` 改为直接 `self.line_index.locate(span.start)` 并填 `DiagnosticLocation`，不再走 `decorate_one`；删除 `decorate_one` 与 `shift_span_in_place`（连带 `decorate` 若也无消费者），或把 `decorate` 改成接受 `&LineIndex` 的批量版本供未来批量场景使用。这样 `novella-core` 同时少 30 行且消掉一个 O(n·m) 热点。

## 16. `expression/primary.rs` 的字面量分支用 `token.slice` 字符串比较判真假/判 `dyn`，绕过 `TokenKind`

- **文件**：`crates/novella-parser/src/expression/primary.rs:29`、`crates/novella-parser/src/types.rs:273`
- **证据**：
  ```rust
  // primary.rs:26-30
  Some(TokenKind::BooleanLiteral) => Expression::Literal {
      value: Literal::Boolean(token.slice == "true"),
      span: token.span,
  },
  // types.rs:271-275
  Some(TokenKind::Identifier)
      if self.peek().is_some_and(|token| token.slice == "dyn")
          && self.check_n(TokenKind::Identifier, 1) =>
  ```
  `true`/`false` 在 lexer 里是**同一个** `BooleanLiteral` 变体（`lib.rs:208-210`），只能靠 `slice` 区分；而 `dyn` 在 `RawToken`/`TokenKind` 里**根本没有变体**（`grep` 确认 lexer 与 `token.rs` 均无 `dyn`），只能靠字符串。ANTI-PATTERNS 规定「在 `token.rs` 之外维护并列的字面量/关键字集合——`TokenKind` 是唯一事实源」。
- **影响**：`"true"` 这个字面量在 parser 里出现一次、在 `keyword/mod.rs:205-209` 的 `KEYWORDS` 表里出现一次、在 `lexer/src/lib.rs:578` 的测试里出现一次；`"dyn"` 在 `types.rs:273` 出现，却不在 `novella_core::keywords::KEYWORDS`（46 条）里——也就是说 `dyn` 是完全词法不可见的「隐形关键字」，LSP 的补全/高亮（`keywords/mod.rs` 是它们的唯一事实源）不会认得它。`BooleanLiteral` 合一也让 `TokenKind` 无法区分 `true`/`false`，下游任何需要判断布尔值常量种类的地方都必须再看 `slice`。
- **建议**：给 `TokenKind` 增加 `TrueLiteral`/`FalseLiteral`（或 `BooleanLiteral(bool)`）拆分，parser 用 `match token.kind` 直接映射 `Literal::Boolean`，删掉 `slice == "true"`；`dyn` 要么在 `RawToken` 中升格为独立 token（`DynamicType` 之类）并进 `KEYWORDS` 表，要么至少在 `keywords/mod.rs` 增加 `pub const CONTEXTUAL_KEYWORDS: [KeywordEntry; 1] = [KeywordEntry { kind: TokenKind::Identifier, literal: "dyn" }]` 让补全/高亮生成器能看见它，parser 改用 `novella_core::keywords::kind_of(token.slice)` 判定而非硬编码字符串。

## 17. 死代码与过时文档：`skip_balanced_block`、`_assert_parts` 空断言、`Spanned<T>` 泛型包装、`[`Parser::scan_angle`]` 悬空文档链接

- **文件**：`crates/novella-parser/src/stmt.rs:678-724`（`#[allow(dead_code)] fn skip_balanced_block`）、`crates/novella-ast/src/drop_guard.rs:417-423`（`fn _assert_parts(_: &Param, _: &FunctionType) {}`）、`crates/novella-core/src/lib.rs:19-23`（`pub struct Spanned<T>`）、`crates/novella-parser/src/expression/postfix.rs:171` 与 `stairs.rs:236`（`[`Parser::scan_angle`]` 链接）
- **证据**：
  - `stmt.rs:678` 的函数 `grep` 零调用；`stmt.rs:697-705` 还手工绕过了 #10 的 `error_at_span`。
  - `drop_guard.rs:417-423` 的 `const _: () = { fn _assert_parts(_: &Param, _: &FunctionType) {} };` ——注释称「静态断言：`Param` 仍是函数类型的唯一参数载体（若改名/改结构此处编译失败）」，但一个只接收两个空引用的空函数**在 `Param` 改结构时不会编译失败**，它只断言两个类型存在。这是零约束的伪断言。
  - `core/src/lib.rs:19-23` 的 `pub struct Spanned<T> { node: T, span: Span }` 全 workspace **零构造点**（`grep "Spanned {"` 只命中 `core/lib.rs:20` 的定义自身）；`novella-ast/src/lib.rs:2` 的 `pub use novella_derive::Spanned;` 是**同名不同物**（derive 宏），AGENTS.md 已注明「`Spanned` 暂保留备用」。
  - `postfix.rs:171` 与 `stairs.rs:236` 的文档链接指向 `Parser::scan_angle`，该函数**全仓不存在**（`grep "fn scan_angle"` 零命中；实际实现是 `scan_call_angle`）。
- **影响**：四条都是「读代码的人会误以为存在某种保障/结构」的噪声：伪断言让人以为 `Param` 的承载关系被编译器保护（实际没有，`drop_guard.rs:417` 的注释也给错了保证）；`Spanned<T>` 是公开 API 里的死类型（等于对外兼容承诺了一个无人使用的泛型）；两处悬空链接 `cargo doc` 不报错（因为 `Parser` 是私有模块内的 `pub` 结构，rustdoc 静默跳过），会让读者按错误的名字去找函数。
- **建议**：删除 `skip_balanced_block`（或按 #10 接入恢复路径）、删除 `_assert_parts` 整块（若确需断言 `Param` 的承载关系，改为对 `FunctionType.params` 字段类型做 `const _: fn(&FunctionType) -> &Vec<Param> = |f| &f.params;` 这类真正会随结构变化而失败的断言）、从 `core` 删除 `Spanned<T>`（AGENTS.md 同步删「`Spanned` 暂保留备用」一句）、把两处 `[`Parser::scan_angle`]` 改为 `[`Parser::scan_call_angle`]`。

## 18. `parse_visibility` 的 `?` 早退吃掉无可见性的 item，外加一组私有类型名与文法不同步

- **文件**：`crates/novella-parser/src/lib.rs:417-430`（`parse_visibility` 及 `parse_item` 调用点）
- **证据**：
  ```rust
  // lib.rs:426-430
  fn parse_visibility(&mut self) -> Option<Scope> {
      let scope = Scope::from_token(&self.peek()?.kind)?;   // 无修饰符 → None（不消费）
      self.advance();
      ...
  }
  ```
  调用点 `lib.rs:317` 是 `let scope = self.parse_visibility();`——把 `Option<Scope>` 里的 `None` 与「流已结束」的 `None` 混为一谈。实测（临时探针）四种输入：

  | 输入 | 产出 items | 诊断 |
  |---|---|---|
  | `pri func foo() {}` | 1 | 无 |
  | `pri let x = 1;` | 1 | 无 |
  | `pri pub func foo() {}` | 1 | `\`pub\` / \`pri\` / \`pkg\` 后应为 class / struct / trait / func / extern / let / const 声明或导入` |
  | `pri ;\nfunc foo() {}` | **1** | 同上（第二行的 `func foo() {}` 被整条丢弃） |

  即：一个孤立的 `pri` 修饰符会让紧随其后的行首 item 静默消失——`parse` 的 `while !self.at_eof()` 循环继续，但 `parse_item` 早退前没有消费 `func`，`recover_after_item` 只跳到行尾，于是 `func foo() {}` 整行被跳过且只有一条含糊的可见性诊断。
- **影响**：这是「打错一个可见性修饰符 → 整个函数消失 → 后续调用全部报未定义」的错误放大链，诊断还不指向真正的问题行。属用户可见的恢复质量缺陷，但触发条件是非法输入，故排在性能与规范问题之后。
- **建议**：`parse_visibility` 返回 `Option<Scope>` 但内部不要用 `?`：`let kind = self.peek()?.kind; let scope = Scope::from_token(&kind)?;` 就已经分离了「流空」与「无修饰符」——当前写法 `Scope::from_token(&self.peek()?.kind)?` 里第一个 `?` 是流空、第二个 `?` 是无修饰符，功能上正确但**调用点**把 `Option` 当「无可见性」用就已经丢掉了「流空」信息；应改为返回 `VisibilityParse { scope: Option<Scope>, eof: bool }` 或 `Result<Option<Scope>, ()>`，让 `parse_item` 在 `eof` 时直接 `return None` 而不进分发，并让「可见性后非法 item」的诊断指向修饰符自身而非下一个 token。

---

## 结构性结论

**parser（`crates/novella-parser`，4599 行）**：真正需要先处理的是**一个性能缺陷 + 一套快照机制分裂**，而不是文件长度。#1 的 O(n²)（3 KB 比较链 130 ms、32× 于等长表达式）是唯一会伤害 LSP 真实体验的问题，根因是 `looks_like_call → scan_call_angle` 全扫、`parse_call_suffix` 再全解析、再整体回滚这三层叠加；它与 #3（`expr.clone()` 深拷贝 + `Literal::Null` 占位）一起构成 `postfix.rs` 的热点与正确性风险。次一级是 #6：同一个 `parse_call_suffix` 里 `checkpoint()/rollback()` 与手写 `saved`/`saved_diagnostics` 并存，后者漏恢复栈预算，会在「大量失败泛型试探 + 深嵌套」下提前触发误报。

规范层面，#4 与 #12 是同一件事的两面：NOTES 的「守卫包装 + `_inner`」在 10 个真入口上执行得很好，但 `postfix.rs:248`/`types.rs:503` 用同样的 `_inner` 命名做了完全不同的 `with_shield` 包装，`parse_postfix` 则是唯一自递归而无守卫的入口——命名把两种保护混为一谈。文件超限（`lib.rs` 1125 / `stmt.rs` 727 / `types.rs` 577）应按 `expression/` 已有的先例拆分，其中 `lib.rs` 尾部的 60 行文本工具（#11）应先进 `preprocess.rs` 或新建 `token_text.rs`，因为那里藏着与 `core::string` 语义不一致的第二份字面量前缀剥离实现。

重复代码的分布很有规律：**二元阶梯（#7，13 层 × 5 行）与逗号列表（#8，4 份）是「结构相同、参数不同」的样板，应当数据化/泛化**；而 #9 的 `return/throw/yield` 分号处理是「两份实现、语义不等价」的重复（导致 `return 1 }` 多报一条诊断），应当合并到 `expect_statement_end` 单点。#5 的 6 个上下文标志与 #10 的 `error_at_span` 线性扫描则属于「`Parser` 结构体承载过多职责」的外溢。

**lexer（582 行）**：功能上干净（实测 `<<=`/`===`/`->`/`f"..."` 全部正确切分，最长匹配优先），但 `RawToken → TokenKind` 的 107 行同名恒等映射（#13）与 8 条重复字符串 regex（#14）是纯样板；更值得注意的是**顺序敏感性无测试保护**，而源码注释与 `.g4` 对裁决规则的表述互相矛盾。`BooleanLiteral` 合一导致的 `slice == "true"`（#16）和下不存在的 `"dyn"` 才是双枚举真正该修的建模缺陷——它们各自在两个 crate 里留了字符串副本，正面违反「`TokenKind` 是唯一事实源」的 ANTI-PATTERNS。

**ast（1855 行）**：`drop_guard.rs`(423 行) 的穷尽 match 设计是好的（漏改编译失败），但 `take_variable_children`(`:258-262`) 与 `drop_variable`(`:265-270`) 函数体逐行相同（#17 相关），`_assert_parts` 是零约束伪断言；`GuardedClone` 由 `novella-derive/src/lib.rs:98-128` 生成，只在最外层包一次 `with_stack`，与 NOTES「每层一次分段切换」的描述并不完全对应——机制说明与实现之间的这处落差值得在文档里澄清，否则后续维护者会按 NOTES 的理解去改 `drop_guard`。

**core（1920 行）**：`line_index`/`path`/`string`/`target` 的单一实现纪律保持得很好（parser 的 `error_at_span` 是唯一「自算行列表」的例外，见 #10/#15）。问题集中在 `decor.rs`：`decorate_one` 每次重建 `LineIndex`（sema 每条诊断一次）、`shift_span_in_place` 已是 f-string 旧路径的遗物；`Spanned<T>` 泛型包装零使用（#17）。`core` 不依赖 `stacker` 使得 #2 的共享常量无法顺手下沉——这是选共享方案时要先解决的前置约束。

**优先级建议**：#1（消除 O(n²) 重解析）与 #3（去掉占位节点与整树 clone）优先，两者都在 `postfix.rs` 一个文件内，可独立验证（加一条 n=400 耗时断言 + 一条「失败调用后 AST 无 `Span{0,0}` 节点」断言）；其次 #2（栈参数单一来源 + 修正 1 GiB/256 MiB 矛盾）与 #6（快照机制收敛）；#9、#16 是「小改动、消除用户可见不一致」的高性价比项；#12/#13/#17 属清理与拆分，风险最低可随时插入。

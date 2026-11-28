# novella-hir / novella-codegen 只读代码简化分析

> 范围：`crates/novella-hir`（25 文件，约 6871 行）、`crates/novella-codegen`（14 文件，约 3005 行）。
> 基线：commit 88db1d9「refactor(compiler): 重构运算符重载与下标协议派发机制」。
> 方法：只读，未修改任何源码；全部行号经 `grep`/`read` 实测。按影响从高到低排序，共 18 条。

---

## 1. `build_address_for_use` 与 `aggregate_address` 是同一套「按表达式形态选寻址方式」的两次实现

- **文件**：`crates/novella-hir/src/build/lvalue.rs:194-223`（`aggregate_address`）、`crates/novella-hir/src/build/lvalue.rs:278-378`（`build_address_for_use`）
- **证据**：
  ```rust
  // aggregate_address:194-223
  Expression::Identifier { .. } | Expression::MemberAccess { .. } | Expression::This { .. }
  | Expression::Unary { op: UnaryOp::DerefPointer, .. } => self.build_address(expr, block),
  Expression::IndexAccess { .. } if self.overloaded_index_receiver(expr)?.is_none() => self.build_address(expr, block),
  Expression::Paren { expr, .. } => self.aggregate_address(expr, block),
  ```
  而 `build_address_for_use:283-376` 对 `Paren/Identifier/This/MemberAccess/IndexAccess/Unary(Deref)` 六个变体各写一遍，并各自再判 `HirType::Struct(..)` 决定 `local_address` / `aggregate_address` / `build_expression`。`build_address`（270-276）只是 `build_address_for_use(expr, false, block)` 的转发。
- **影响**：两处变体集合必须同步演进；`This` 分支逻辑在 `build_address_for_use:317-329`、`aggregate_address:204` 与 `expr.rs:316-329` 出现三次。新增左值形态（如属性访问糖）时极易只改一处，导致「取址可编译、读值不可编译」的不对称缺陷。
- **建议**：把 `aggregate_address` 收敛为 `build_address_for_use` 的一个模式参数（`enum AddressMode { Value, AggregateStorage }`），或在 `build_address_for_use` 内部对 `HirType::Struct(..)` 目标统一走 `aggregate_address` 的单点判定；删除 194-223 中与 283-376 重叠的变体分发。

## 2. `emit_c_for_scoped` 里 `{ let this = self; … }` 包裹块与无意义 `shrink_to_fit`

- **文件**：`crates/novella-hir/src/build/stmt.rs:450-494`（块）、`crates/novella-hir/src/build/stmt.rs:487`（`prelude.stmts.shrink_to_fit()`）
- **证据**：
  ```rust
  {
      let this = self;
      let init_start = this.next_local;
      ...
      prelude.stmts.shrink_to_fit();
      block.push(HirStmt::Loop { ... });
      Ok(())
  }
  ```
- **影响**：58 行的函数体被人工别名 `this` 包裹，唯一作用是绕开某个借用检查（当前已无必要——函数内无 `self` 与他用并发借用）；`shrink_to_fit` 在 AST→HIR 阶段无任何收益，只是噪声。该函数是 4 个 `for` 变体之一，可读性问题会被后续维护者照抄。
- **建议**：移除包裹块与 `shrink_to_fit`，直接使用 `self`；若确有借用冲突，用局部 `let mut block_tail = HirBlock::new()` 之类具名临时替代 `this` 别名。

## 3. `BuildError` 的 `ok_or_else(|| BuildError::at(...).with_site(...))` 样板 72 处，占 128 处 `at` 调用的 56%

- **文件**：`crates/novella-hir/src/build/error.rs:18-35`；样板分布见 `aggregate.rs:36-43`、`operators.rs:157-162`、`172-177`、`call.rs:112-128`、`strings.rs:29-43`、`types.rs:281/326`
- **证据**：
  ```rust
  .ok_or_else(|| BuildError::at(BuildErrorKind::MissingType { what: "class 实例定义" })
      .with_site(self.module, span))?
  ```
  统计：`BuildError::at` 128 处、`with_site` 104 处、`ok_or_else(|| BuildError::at(` 同式 72 处；`map_err(|` 仅 15 处。`BuildErrorKind` 中 `Unimplemented` 54 次、`MissingType` 43 次，合计占 75%。`crates/novella-cli/src/main.rs:183-207` 只使用 `to_string()/module/span`，从未匹配 `BuildErrorKind`。
- **影响**：每次新增失败点都要重复 4 行模板；`Unimplemented{node: "字符串常量"}` 与 `MissingType{what: "…"}` 的 `node/what` 文案是唯一实际信息载体，却被埋在重复结构中。
- **建议**：在 `error.rs` 增加 `pub fn missing(what: &'static str) -> Self` / `pub fn unimplemented(node: &'static str) -> Self`（或 `BuildErrorKind` 上的 `at_missing(what)`），并给 `BodyBuilder` 加 `fn fail(&self, kind: BuildErrorKind, span: Span) -> BuildError`（内部固定 `with_site(self.module, span)`）。可消除 ≥70 处的重复尾部；`build/call/target.rs:45-49`、`module.rs:317-321` 已用局部 `let missing = || …` 闭包，证明该形状可接受。

## 4. `build_binary` 105 行三条返回路径，`binary_op_of`、`expr_type` 各取 2~3 次

- **文件**：`crates/novella-hir/src/build/operators.rs:100-244`
- **证据**：
  - 109-111 短路、112-114 coalesce、115-139 `operator_calls`；
  - 157-162 与 172-177 两段逐字相同的
    ```rust
    binary_op_of(op).ok_or_else(|| BuildError::at(BuildErrorKind::Unimplemented {
        node: binary_node_name(op),
    }).with_site(self.module, span))?
    ```
  - 140-141 取 `expr_type(left.span()).ok()/right`；180-181 再取一次；188-191 第三次取（仅供比较止错）。
- **影响**：同一运算符的能力检查（是否可降级）在两个返回路径各做一次；`expr_type` 三次查询让「比较结果不可反推操作数类型」这条关键约束散落在三处。函数 105 行也逼近读取上限。
- **建议**：函数开头一次性解析 `let hir_op = binary_op_of(op).ok_or_else(...)?;`（在 float 分支之前），两处共用；把 140-141/180-181/188-191 的三次 `expr_type` 合并为 `let (left_ty, right_ty) = (…ok(), …ok());` 一次求值，比较分支用 `left_ty.clone().or(right_ty.clone()).ok_or(…)?` 表达；将 210-232 的指针算术 / identity 检查抽成 `fn pointer_arithmetic_types(...) -> Result<(HirType, HirType, HirBinaryOp), BuildError>`。

## 5. `UpdatePlace` 引入后没有留下旧路径死代码，但下标寻址仍在 `expr.rs` 与 `lvalue.rs` 各实现一份

- **文件**：`crates/novella-hir/src/build/expr.rs:228-255`、`crates/novella-hir/src/build/lvalue.rs:345-367`
- **证据**：两侧都是 `check_index_target` → `Array` 走 `aggregate_address` → `build_typed_expression(index, Int(Usize))` → `fresh_local(Pointer(element_ty))`，仅末条指令不同（`IndexLoad` vs `IndexAddress`）。`grep` 确认 `IndexPlace`/`read_index_place`/`write_index_place`/`build_index_place` 均为 **0 处**——88db1d9 已把真正的旧路径删干净，**不存在**「UpdatePlace 遗留死代码」。
- **影响**：真正的重复是「读元素」与「求元素地址」的定位前奏；`check_index_target` 被 expr.rs:237 与 lvalue.rs:171/350 三处调用。两处若对 `tuple` 拒绝、对 `usize` 索引定型的处理出现分歧，会产生「`a[i]` 可读不可写」类缺陷。
- **建议**：抽 `fn element_address(&mut self, target, index, span, block) -> Result<HirOperand, BuildError>`（返回 `Pointer(element_ty)` 操作数 + `element_ty`），`IndexLoad` 分支与 `build_address_for_use` 的 `IndexAccess` 分支都调用它；`IndexLoad` 只需在返回的地址上 `Deref`。

## 6. codegen `instruction()` 单函数 400 行、30+ 分支，且「旧值释放」序列重复 4 份

- **文件**：`crates/novella-codegen/src/body/instructions.rs:6-417`
- **证据**：`Store`(73-84)、`StoreIndirect`(102-113)、`FieldStore`(306-317)、`IndexStore`(365-372) 四处几乎逐行相同：
  ```rust
  self.reference_value("novella_rt_retain", new_value, &ty)?;
  let old = self.builder.build_load(value_type(...)?, pointer, "previous_local")?;
  self.builder.build_store(pointer, new_value)?;
  self.reference_value("novella_rt_release", old, &ty)?;
  ```
  仅标签字符串（`previous_local`/`previous_indirect`/`previous_field_value`/`previous_element`）与指针来源不同。`IndexStore`(365) 更是**无条件** retain/release，与另三处的 `is_managed` 判定不一致。
- **影响**：单函数 400 行违反「单文件/单函数可读」直觉；4 份 ARC 序列任何一份漏改都会造成引用计数泄漏或过早释放（内存安全级缺陷），而 `IndexStore` 与其余三处的 `is_managed` 分支差异是当前已存在的可疑不一致点。
- **建议**：提取 `fn store_with_ownership(&mut self, pointer, ty: &HirType, new_value: BasicValueEnum, label: &str) -> Result<()>`，统一 `is_managed` 判定与 retain→load→store→release 顺序；`IndexStore` 改为调用它（顺带修正无条件 retain 的差异）。再按指令族把 `instruction()` 拆成 `fn store_instr / fn field_instr / fn index_instr / fn call_instr`。
  
## 7. `emit_artifact` 185 行混杂 8 类职责，函数发射循环内联 12 个校验分支

- **文件**：`crates/novella-codegen/src/lib.rs:326-511`
- **证据**：326-345 目标校验；346-351 入口选择；352-369 target machine；370-469 函数声明循环（含 `contextualize` 闭包与 384-462 的 10 余处 `return Err(contextualize(CodegenError(…)))`）；470 globals 初始化；471-479 函数体发射；480 析构；481-494 合成 `main`；495-497 bare 属性；498-511 verify/optimize/audit。
- **影响**：185 行内既做语义一致性校验（extern/链接名/保留命名空间/符号冲突）又做 LLVM 构造与产物写出，任一处校验文案改动都要在长函数中定位；「HIR 外部声明与链接名不一致」等错误无法单测，因为全被包在 `emit_artifact` 里。
- **建议**：拆出 `fn declare_functions(...) -> Result<(Functions, HashSet<String>)>`（含全部符号校验）、`fn synthesize_main(...)`、`fn finish_artifact(...)`；`emit_artifact` 只保留参数校验 + 顺序编排。

## 8. `Types::build` 把同一个结构构造两次，末尾再把字段 move 回去

- **文件**：`crates/novella-codegen/src/lib.rs:105-151`
- **证据**：
  ```rust
  let types = Self { bare_metal: program.spec.is_like_bare, aggregates, fields: HashMap::new(), destructors: HashMap::new() };   // 108-113 构造 #1
  ...
  Ok(Self { bare_metal: types.bare_metal, aggregates: types.aggregates, fields: fields_by_type, destructors })                  // 146-151 构造 #2
  ```
  中间为了借用 `types.aggregates` 而让 `fields`/`destructors` 先置空。
- **影响**：`Types` 有两个「半初始化」状态，任何在 108-146 之间访问 `types.fields` 的代码都会拿到空表且编译通过——是静默错误温床。
- **建议**：改为两阶段局部变量：先 `let mut aggregates = …` 与 `let mut fields_by_type = …`，全部完成后再一次性 `Ok(Self { … })`；或把声明与布局填入拆成 `fn declare_aggregates()` / `fn fill_layouts(&mut self)`。

## 9. `implicit_member` 双分支重复组装 `FieldLoad`，且用魔数 `Span { start: 0, end: 0 }` 掩盖真实位置

- **文件**：`crates/novella-hir/src/build/operators.rs:428-469`（两分支 433-446、449-467）、`crates/novella-hir/src/build/expr.rs:63-85`（第三份 `emit_field_read`）
- **证据**：`operators.rs:435` 与 `operators.rs:454` 均用 `self.field_slot(&ty, name, Span { start: 0, end: 0 })`；`FieldLoad` 组装在两分支各 12 行，仅 `target`/`target_ty` 来源不同。全仓 `Span { start: 0, end: 0 }` 共 12 处（`types.rs:202/281/326`、`requirements.rs:20/80/105/128/141`、`mono.rs:412`、`module.rs:31`、`operators.rs:435/454`）。
- **影响**：字段缺失诊断会指向 `0..0`（文件首行首列），用户看不到真实出错位置；三份 `FieldLoad` 组装的 `borrow` 语义（`implicit_member` 恒 `false`，`emit_field_read` 可传 `true`）容易漂移。
- **建议**：`implicit_member` 接收调用方的真实 `span`（`expr.rs` 的 `Identifier` 分支本就有 `*span`），并把两分支收敛为 `self.emit_field_load(target, target_ty, name, span, false, block)`；与 `emit_field_read` 共用同一 helper。

## 10. `context.rs` 的 `expr_type` 与 `expr_type_pref` 逐行重复

- **文件**：`crates/novella-hir/src/build/context.rs:114-124`、`crates/novella-hir/src/build/context.rs:132-146`、`crates/novella-hir/src/build/context.rs:148-154`
- **证据**：两者除 `concrete_type(view, None)` 与 `concrete_type(view, preferred)` 外完全相同（含相同的 `map_err(|error| error.with_site(self.module, span))`）；`concrete_type`(148-154) 只是 `mono::concrete_type` 的转发。
- **影响**：调用量很大（`expr_type` 34 处、`expr_type_pref` 17 处），任何取类型逻辑变化（例如未来加入「标记为 Error 时打印）」都要改两份；转发函数让调用链多一跳。
- **建议**：`pub(super) fn expr_type(&mut self, span) -> … { self.expr_type_pref(span, None) }`，删除 `concrete_type` 转发，直接调 `mono::concrete_type`。

## 11. `ty.rs` 存在两个无调用者的 `pub fn from_view` 转发壳

- **文件**：`crates/novella-hir/src/ty.rs:236-241`（`HirType::from_view`）、`crates/novella-hir/src/ty.rs:359-364`（`FunctionSignature::from_view`）
- **证据**：`HirType::from_view` 仅 `Self::from_view_with(view, function_id, None)`；`grep` 全仓 `HirType::from_view(` 命中仅在 `ty.rs` 自身 312/316 —— 那是 `from_view_with` 内 Dynamic 分支的递归调用（不传 preferred），对外 **0 调用**。`FunctionSignature::from_view(` 同样 **0 调用**（`ty.rs:293` 调的是私有 `from_view_with`）。真实入口只有 `build/mono.rs:142` 的 `HirType::from_view_with(&view, &mut function_id_of, preferred)`。
- **影响**：`pub` 面即兼容承诺（AGENTS.md「crate 公开面最小化」）。两个壳的存在会让调用者误以为「不带 preferred 的转换是合法入口」，而它恰恰对未定型字面量一律 `UntypedLiteral` 失败。
- **建议**：删除两个 `pub fn from_view`（Dynamic 分支递归改用 `from_view_with(…, None)`），把 `from_view_with` 提升为唯一入口；`ty.rs:223-235` 叠加的两段 doc（「从语义投影降级为 HIR 类型」与「投影为 HIR 类型」）合并为一段。

## 12. 类型转换升降（`from_view`/`coerce`）模板与「未定型字面量落地」在 `ty.rs` 与 `stmt.rs` 各有一套

- **文件**：`crates/novella-hir/src/ty.rs:259-267`（`IntTy::from_name`/`FloatTy::from_name` + `UnknownTypeName`）、`crates/novella-hir/src/ty.rs:305-341`（`IntLiteral`/`FloatLiteral`/`Null` + `preferred`）、`crates/novella-hir/src/build/stmt.rs:306-330`（`coerce` 的 `UntypedInt`/`UntypedFloat` 落地 + 范围检查）
- **证据**：
  ```rust
  TypeView::IntLiteral => match preferred {
      Some(ty @ HirType::Int(_)) => Ok(ty.clone()),
      _ => Err(BuildError::at(BuildErrorKind::UntypedLiteral { what: "int literal" })),
  }
  ```
  与 `stmt.rs` 的 `(HirOperand::UntypedInt { value, .. }, HirType::Int(_)) => Ok(HirOperand::IntConst(*value))` 属于同一「字面量未定型 → 由上下文定型」规则的两处实现；`Float→Int` 的范围收敛在 `stmt.rs:318-330` 手写 `powi` 边界，而 `ty.rs` 里 `bit_width/is_signed` 已有同样语义的谓词。
- **影响**：`UntypedLiteral` 的 `what` 文案在 `ty.rs` 内重复 3 次（`"null"`/`"int literal"`/`"float literal"`），`stmt.rs` 的 `HirOperand::*` 匹配又独立判一次；新增字面量种类（如 `char`）要同时改两处匹配。
- **建议**：在 `HirOperand` 上提供 `fn settle(&self, to: &HirType) -> Option<HirOperand>`（`UntypedInt/UntypedFloat` 收敛于此），`coerce` 首段直接应用；`ty.rs` 的三段 `UntypedLiteral{what}` 用 `fn untyped(what: &'static str)` 小构造器收敛。

## 13. `append_default_arguments` / `expand_default_arguments` 双接口，8 参数函数被 `#[allow(too_many_arguments)]` 掩盖

- **文件**：`crates/novella-hir/src/build/call.rs:449-492`（6 参数，转发）、`crates/novella-hir/src/build/call.rs:496-548`（8 参数，`#[allow(clippy::too_many_arguments)]`）、`crates/novella-hir/src/build/call.rs:158-167`（`build_new` 直接调 `expand`）
- **证据**：`append` 在 489-491 调 `self.expand_default_arguments(…)`；`expand` 的参数表为 `(self, signature, method, module, type_name, provided, span, block)`。
- **影响**：同一职责两个入口，`build_new` 绕开 `append` 直接调 `expand` 说明两者边界本身不清晰；`allow(too_many_arguments)` 是「结构未收敛」的显式标记，而非无害豁免。
- **建议**：只保留一个函数，把 `(module, type_name, method)` 聚成 `struct MethodSite { module: ModuleId, type_name: &str, method: &str }`（或复用已有的 `FunctionSignature` + 一个 `DefaultOwner` 小结构），从而降到 5~6 参数并去掉 `allow`。

## 14. `build_call` 接收者解析块重复查 `self` 并提前 clone `receiver`

- **文件**：`crates/novella-hir/src/build/call.rs:326-374`
- **证据**：338-349 先 `self.lookup_local("self").map(|id| self.local_type(id))` 得到 `target_ty`；349 又 `HirOperand::Local(self.lookup_local("self").expect("接收者已检查"))` —— 第二次查找同一键；367 `dyn_receiver = Some((receiver.clone(), target_ty))` 随后 `operands.push(receiver)`，唯一消费点是 399 的 `dyn_receiver.expect(...)`。
- **影响**：`expect("接收者已检查")` 与 338-349 的 `ok_or_else` 形成「检查一次、再断言一次」的双重保险，若中间逻辑变化而检查被移动，`expect` 会 panic 而非返回 `BuildError`（违反「不可恢复错误 fail-fast 但不得 panic」的边界感）；`clone` 与 `Option` 包装可省。
- **建议**：`let Some(self_id) = self.lookup_local("self") else { … };` 一次取到 `(self_id, self_ty)`，后续两处复用；`dyn_receiver` 改为 `Option<HirType>`（receiver 操作数直接 `operands[0]`），去掉 clone 与 `expect`。

## 15. `same_string_class` 定义在运算符文件里、`protocol_receiver_value` 与 `build_protocol_index_place` 组装的临时槽重复

- **文件**：`crates/novella-hir/src/build/operators.rs:472-476`（`same_string_class`）、`crates/novella-hir/src/build/operators.rs:371-387`（`protocol_receiver_value`）、`crates/novella-hir/src/build/strings.rs:148-167`（`__str` 分支持有同样的 `Alloc` + `LocalAddress` 序列）、`crates/novella-hir/src/build/call.rs:134-155`（`build_protocol_index_place`）
- **证据**：`protocol_receiver_value` 的 `Alloc{init}` + `local_address` 两步在 `strings.rs:149-166` 被手抄一遍；`same_string_class` 只有 `strings.rs:102` 一个调用者，却放在 `operators.rs`。
- **影响**：职责错位（字符串语义在运算符模块）会诱导后续在 `operators.rs` 继续堆字符串特判；「struct 值 → 指针接收者」的临时槽模式写两遍，`borrow`/所有权语义易分叉。
- **建议**：`same_string_class` 移入 `strings.rs` 或 `helpers.rs`；`protocol_receiver_value` 提升为 `pub(super)` 并让 `strings.rs` 的 `__str` 分支复用（它需要的正是「struct 值物化成接收者指针」）。

## 16. `contextualize` 与路径前缀格式化在 `emit_artifact` 内重复

- **文件**：`crates/novella-codegen/src/lib.rs:384-386`、`crates/novella-codegen/src/lib.rs:471-479`
- **证据**：
  ```rust
  let contextualize = |error: CodegenError| CodegenError(format!("{}：{}：{}", source.path, function.id, error.0));   // 384-386
  ...
  body::emit(…).map_err(|error| CodegenError(format!("{}：{}：{}", source.path, function.id, error.0)))?;              // 473-476
  ```
  同一前缀格式串写两遍且**不是** `CodegenError` 的方法。
- **影响**：几乎全部 codegen 诊断都走这个前缀；格式从 `：` 改为 `: ` 之类的统一调整要改两处，且无法在 `CodegenError` 层单测。
- **建议**：`impl CodegenError { fn at(self, path: &str, id: &HirFunctionId) -> Self }`，两处共用。

## 17. 运行时符号名与「保留名」判定散落在多处字符串前缀比较

- **文件**：`crates/novella-codegen/src/globals.rs:13-15`（`format!("m{module}::{name}")`）、`crates/novella-codegen/src/lib.rs:56`（`entry()` 用 `format!("m{}::main", module.id)`）、`crates/novella-codegen/src/lib.rs:421`（`name.starts_with("__novella_drop_") || name.starts_with("llvm.")`）、`crates/novella-codegen/src/ownership.rs:15`（`format!("__novella_drop_m{module}_{name}")`）
- **证据**：HIR 侧已有 `HirFunctionId` 这一稳定键（`ty.rs:159` `HirFunctionId::anonymous(module)`），但 codegen 仍在两处各自拼 `m{module}::{name}`，并以字符串前缀判定后端保留命名空间。
- **影响**：符号命名规则有两份实现（`globals::name` 与 `entry()` 内联），新增符号类别时容易只改一处；前缀判定分散使得「保留命名空间」清单不唯一。
- **建议**：把 `format!("m{module}::{name}")` 收敛到 `Identity/Name` 单点（例如 `crate::identity::symbol(module, name)`），`entry()` 直接复用；把 `RESERVED_PREFIXES: &[&str]` 常量化供 `lib.rs` 与 `ownership.rs` 共用。

## 18. `body.rs` 的 `HirCond::has_prelude` 与 `HirBlock::is_empty` 无消费者；`item.rs` 已承载「程序/模块/类型定义」三类职责

- **文件**：`crates/novella-hir/src/body.rs:91-93`（`HirBlock::is_empty`）、`crates/novella-hir/src/body.rs:356-358`（`HirCond::has_prelude`）、`crates/novella-hir/src/item.rs:17-162`
- **证据**：`grep` `has_prelude` 仅命中定义处（0 调用）；`HirBlock::is_empty` 仅被 `has_prelude` 调用。`item.rs` 同时定义 `HirProgram`(17)、`HirModule`、`HirItem`(51)、`HirTypeDef`/`HirField`(153) 等多个层级，`body.rs` 390 行则混装指令集与表达式类型。
- **影响**：`has_prelude` 注释写着「codegen 可据此跳过空块的额外 basic block」，但 `body/control.rs:64/129` 实际是直接 `if self.block(&cond.prelude)?`，说明该优化由 codegen 自行处理——HIR 侧保留的「给 codegen 的提示」是失效 API。`item.rs` 若继续增长会逼近 600 行拆分阈值。
- **建议**：删除 `HirCond::has_prelude` 与 `HirBlock::is_empty`（或让 `control.rs` 真正使用它，二者取一）；`item.rs` 按 `program.rs`（`HirProgram`/`HirModule`/`HirItem`）+ `types.rs`（`HirTypeDef`/`HirField`/`HirMethod`）拆分，`item.rs` 只留重导出。

---

## 结构性结论

**HIR 侧（`crates/novella-hir`）**：88db1d9 的重构本身是干净的——`UpdatePlace` 取代的 `IndexPlace`/`read_index_place`/`write_index_place`/`build_index_place` 已全部删除（grep 0 命中），题材中担心的「旧路径死代码」并不存在。真正的技术债在另一层：`build/` 目录下「按表达式形态分派寻址」这件事被 `aggregate_address`、`build_address_for_use`、`expr.rs::IndexAccess` 三处各写一遍，`self` 接收者判定散落四处（`lvalue.rs:288/317-329`、`expr.rs:97-102/316-329`、`call.rs:338-349`）。`UpdatePlace::read/write` 已经把"位置定位一次、读写两次"抽象出来，但这层抽象没有继续向`定位`本身延伸——建议定义单一 `fn element_address / fn field_address_of` 族函数，把 `expr.rs`/`lvalue.rs` 的重叠命令式代码收进 2~3 个原语，而不是继续在 match 分支里加 `if matches!`。另外，出错位置一类的元数据债务集中：`Span { start: 0, end: 0 }` 12 处（`types.rs` 3、`requirements.rs` 5、`operators.rs` 2、`mono.rs` 1、`module.rs` 1），与 `implicit_member` 的 `field_slot` 用假位置直接绑定，应当把 span 一路从调用点传到 `BuildError`。

**codegen 侧（`crates/novella-codegen`）**：所有文件均 < 600 行、目录拆分符合规范，且严格只读 HIR（`emit_object` 无反向依赖）——架构纪律没问题。问题全部落在**粒度**：`instructions.rs::instruction()` 单函数 400 行、`emit_artifact` 185 行、`build_fstring` 约 90 行，这三处都是「一个函数是一个子系统」。更关键的是 ARC 旧值释放序列（retain → load previous → store → release）在 4 条指令分支各写一次，其中 `IndexStore` 与其余三条的 `is_managed` 判定已经不一致——这是当前代码里**唯一可能影响运行期正确性**的重复，建议优先于其他项处理。

**优先级建议**：先做 #1（寻址统一）与 #6（ARC store 统一，含 `IndexStore` 的 `is_managed` 差异核查），这两项同时是「减少重复」与「消除潜在缺陷」；再做 #3/#9（`BuildError` 构造器 + 真实 span，一次重构覆盖 70+ 处）；#11/#18（删除 `from_view` 与 `has_prelude` 等失效 API）风险最低，可随时插入。整体上 HIR 与 codegen 都不是"需要重写"的状态，而是集中在 3~4 个 `fn` 上的局部债——每一项都有明确的、可独立验证的单点修改路径。

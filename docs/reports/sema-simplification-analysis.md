# novella-sema 只读代码简化分析

> 范围：`crates/novella-sema/src`，58 个文件 / 14029 行
> 基线：commit 88db1d9「refactor(compiler): 重构运算符重载与下标协议派发机制」
> 方法：只读，未修改任何源码；全部行号经 grep/read 实测。按影响从高到低排序，共 20 条。

---

## 1. 成员查找的 `static_only` 是死参数，覆写静态性冲突诊断与字段遮蔽检查整体失效

- **文件**：`crates/novella-sema/src/typecheck/member.rs:449`、`member.rs:454`、`member.rs:462-515`、`crates/novella-sema/src/check/type_defs/inheritance.rs:220-238`
- **证据**：

```rust
// member.rs:449-455
/// 沿 class 继承链查找成员（struct 只看自身；static_only 时只看静态方法）。
pub(crate) fn lookup_member_chain(
    sema: &Sema,
    key: &(ModuleId, String),
    name: &str,
    _static_only: bool,
) -> Option<MemberHit> {
```

```rust
// inheritance.rs:220-238
let Some(hit) = lookup_member_chain(sema, &parent, &method.name, method.is_static) else {
    if let Some(other) = lookup_member_chain(sema, &parent, &method.name, !method.is_static)
        && let MemberInfo::Method(base) = other.info
    {
        … "静态 / 实例属性不一致，不能覆写" …
        let _ = base;
    }
    continue;
};
```

函数体（member.rs:462-515）只按 `TypeDef` 种类查 `fields` / `methods`，`_static_only` 从未被读取。13 个调用点（`member.rs:178/244/439/444`、`call.rs:55`、`expr/unary.rs:160`、`unsafe_rules/assignment.rs:158/283`、`body/iteration.rs:55`、`body/mod.rs:358`、`inheritance.rs:190/209/220/224`）传入的 `true` 与 `false` 语义完全一致。

影响链有两个：
1. `inheritance.rs:220` 的 `let Some(hit) = … else { … }` 因查找恒返回同名成员而**永不进入 else 分支** → 「覆写处静态/实例属性不一致」诊断是死代码（`let _ = base;` 是残留）。
2. `inheritance.rs:190` 用 `lookup_member_chain(sema, &parent, &field.name, false)` 判定「父类是否有同名字段」以报字段遮蔽，而该查找会同时命中**方法**与**父类链上的字段**，因此「父类方法被字段遮蔽」「祖父类字段被本类字段遮蔽」都会被误报/漏报；静态性过滤实际只由事后 `check_static_access`（member.rs:514-528）与 `check_static_member`（member.rs:196-205）承担。

- **影响**：一段用户可见的诊断（静态性冲突）永久不可达；字段遮蔽与静态成员访问的判定口径不一致，误报风险集中在继承场景。注释描述的契约与实现相反，后续维护者会据此写错新调用点。
- **建议**：二选一——(a) 让 `lookup_member_chain` 真正按 `MemberInfo::Field(f).is_static` / `MemberInfo::Method(m).is_static == static_only` 过滤并跳过不匹配项继续上溯，同时修正 `inheritance.rs:220` 的两段式查找；(b) 若确认静态性检查已由 `check_static_access` 全权负责，则删除该参数、修正 `member.rs:449` 注释、删除 `inheritance.rs:220-238` 整段死分支（并让 `inheritance.rs:190` 改用「只查字段」的专用查询）。

## 2. `stacker::maybe_grow` 的 128 KiB / 2 MiB 魔数在 10 处重复，无共享常量

- **文件**：`crates/novella-sema/src/typecheck/expr/hints.rs:29`、`typecheck/expr/dispatch.rs:10`、`typecheck/body/mod.rs:93`、`typecheck/flow.rs:84`、`flow.rs:122`、`flow.rs:310`、`check/type_defs/overlap.rs:11`、`check/returns.rs:107`、`resolve/types.rs:19`、`traits.rs:180`
- **证据**：

```rust
// 10 处完全一致的写法（以 flow.rs:84 为例）
stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || constant_bool_inner(expr))
```

- **影响**：递归防护阈值是本 crate 唯一的栈安全策略，却散落 10 处。任何一次调参（例如代码生成器加深递归链后需要更大段）必须同步 10 个文件，漏一处就会在深层嵌套输入上 abort；数值本身没有注释说明依据。
- **建议**：在 `crates/novella-sema/src/lib.rs` 或新建 `typecheck/recursion.rs` 定义 `const RED_ZONE: usize = 128 * 1024; const STACK_SEGMENT: usize = 2 * 1024 * 1024;`（或直接复用 parser 侧已有常量命名），提供 `fn grown<T>(f: impl FnOnce() -> T) -> T` 包装，10 处改为调用它；阈值来源写进 doc 注释。

## 3. `static_only`/`self` 之外的接收者口径重复：`self` 跳过逻辑 8 种写法、`this_ty_key` 与 `type_key_of` 两份实现

- **文件**：`crates/novella-sema/src/ty/mod.rs:168`、`typecheck/call.rs:484`、`call.rs:728`、`check/type_defs/inheritance.rs:142`、`inheritance.rs:300`、`freestanding.rs:109`、`traits.rs:251`、`typecheck/mod.rs:229-236`、`typecheck/member.rs:366-372`、`traits.rs:10-15`
- **证据**：

```rust
// ty/mod.rs:168  —— 投影时跳过 self
if self.is_method && param.name == "self" && index == 0 { continue; }
// call.rs:484
let offset = usize::from(sig.is_method && sig.params.first().is_some_and(|p| p.name == "self"));
// call.rs:728
let skip_self = sig.is_method && sig.params.first().is_some_and(|p| p.name == "self");
// inheritance.rs:142
let params = |sig: &FunctionSig| usize::from(sig.is_method && sig.params.first().is_some_and(|p| p.name == "self"));
// freestanding.rs:109
!(sig.is_method && index == 0 && param.name == "self" && matches!(param.ty, Ty::Struct(..)))
// traits.rs:251
(index != 0 || param.name != "self")
// body/iteration.rs:87-91
let has_explicit_params = match params.as_slice() { [] => false, [receiver] => receiver.name != "self", _ => true };
```

```rust
// typecheck/mod.rs:229-236
pub(crate) fn this_ty_key(ty: &Ty) -> (ModuleId, String) {
    match ty {
        Ty::Applied(base, _) => this_ty_key(base),
        Ty::Struct(m, n) | Ty::Class(m, n) | Ty::Trait(m, n) => (*m, n.clone()),
        _ => unreachable!("this_ty 恒为 Struct/Class"),
    }
}
// member.rs:366-372 —— 逐字相同的第二份，只有 unreachable 文案不同
pub(crate) fn type_key_of(ty: &Ty) -> (ModuleId, String) { … unreachable!("仅 Struct/Class 调用") }
// traits.rs:10-15 nominal_base —— 同一逻辑第三份（返回 &Ty 而非 key）
```

- **影响**：「实例方法是否跳过 `self`」是签名形状的核心判据，8 处各自实现，对 `self` 位置、`is_method` 标志的假设并不统一（`traits.rs:251` 只看 `index`/`name` 不看 `is_method`；`freestanding.rs:109` 还额外要求 `Ty::Struct`）。`this_ty_key`/`type_key_of`/`nominal_base` 三份等价实现意味着三处 `unreachable!` 边界各自维护（且两处文案都与实际分支矛盾——分支含 `Ty::Trait` 却写「恒为 Struct/Class」）。
- **建议**：在 `ty/mod.rs` 的 `FunctionSig` 上加两个方法：`fn receiver_offset(&self) -> usize`、`fn explicit_params(&self) -> &[ParamSig]`，所有调用点（含 `view()`）改用它们；把 `this_ty_key` 与 `type_key_of` 合一（保留 `type_key_of`，`this_ty_key` 作为 `pub(crate) use` 别名或直接删除），`nominal_base` 用 `type_key_of` 的结果重建或反之，并把 `unreachable!` 文案改成「仅 Struct / Class / Trait」。

## 4. 三元表达式分支逻辑在 `check_expr_inner` 与 `check_expr_with_hint_inner` 中逐行重复约 30 行

- **文件**：`crates/novella-sema/src/typecheck/expr/dispatch.rs:109-139`、`typecheck/expr/hints.rs:94-132`
- **证据**：

```rust
// dispatch.rs:109-139 与 hints.rs:94-132 的共同骨架
let condition = constant_bool(cond);
let before = ctx.uninitialized.clone();
let reachable = ctx.reachable;
ctx.reachable &= condition != Some(false);
let then_ty = check_expr(sema, ctx, then_expr);
let after_then = ctx.uninitialized.clone();
ctx.reachable = reachable && condition != Some(true);
let else_ty = check_expr(sema, ctx, else_expr);
ctx.uninitialized = match condition {
    Some(true) => after_then, Some(false) => before, None => { … extend … }
};
unify_types(&then_ty, &else_ty).unwrap_or_else(|| { … "三元表达式分支类型 … 不一致" … Ty::Error })
```

两份只在「失败回退」上不同：`hints.rs:118-131` 多了 `is_assignable(hint, &then_ty) && is_assignable(hint, &else_ty)` 双检后返回 `hint.clone()`。

- **影响**：可达性（`reachable`）与未初始化集合（`uninitialized`）的快照/恢复顺序是确定赋值分析的正确性关键，两处手工同步；任何对三元语义的修正（例如把 `Some(false)` 时 `after_then` 也回填）必须两处同时改，且没有测试能发现只改一处。
- **建议**：抽 `fn check_ternary<S: TernaryFallback>(sema, ctx, cond, then_expr, else_expr, fallback: S) -> Ty`，把「结果类型不一致时的回退策略」作为唯一差异点（`None` 策略 / `hint` 双检策略），两个臂各调一次。

## 5. 隐式 / 显式转换判定分散在四处，`assign_mismatch_reason` 只覆盖两族类型

- **文件**：`crates/novella-sema/src/ty/relations.rs:53-119`、`state.rs:394-421`、`typecheck/call.rs:480-482`、`call.rs:506-525`、`typecheck/unsafe_rules/operators.rs:425-472`、`typecheck/unsafe_rules/assignment.rs:86-96`
- **证据**：

```rust
// relations.rs:53-119 assignable() —— 唯一判定；但调用方普遍写成
if !sema.is_assignable(&param.ty, arg_ty)
    && !sema.record_dyn_conversion(ctx.module, arg.span(), &param.ty, arg_ty)
{ … assign_mismatch_reason(...).unwrap_or_else(|| 通用文案) }   // call.rs:518-524
// assignment.rs:86-96 —— 同一三连组合，只是文案尾巴不同
if !sema.is_assignable(&target_ty, &value_ty) { … assign_mismatch_reason(...).unwrap_or_else(...) }
// operators.rs:425-472 check_cast —— 自己重写了一遍「null→可空 / 数值互转 / 指针 unsafe / 上下转型」判据
// state.rs:394-421
if let (ty::Ty::Float(kind), ty::Ty::FloatLiteral(value)) = (target, source) { … }
if let (ty::Ty::Int(kind), ty::Ty::IntLiteral(negative, value)) = (target, source) { … }
None   // 其余情形一律退回通用文案
```

- **影响**：「能否赋值 + 是否记录动态转换 + 如何解释失败」是同一件事的三步，在 `call.rs`、`assignment.rs`、`var/field` 初始化、`check_cast` 中各自拼装顺序，漏掉 `record_dyn_conversion` 就会让 `Dynamic` 转换静默丢失（HIR 依赖它做动态派发）。`assign_mismatch_reason` 只在字面量越界时报可读原因，`Tuple/Array/Object/Union` 的逐元素失败仍退化到「不能把 A 赋给 B」，用户在深层结构上得到的定位信息为零。
- **建议**：在 `Sema` 上提供单一入口 `fn explain_assignment(&mut self, module, span, target, source, site: ValuePosition) -> Option<String>`，内部依次做 `is_assignable` → `record_dyn_conversion` → `assign_mismatch_reason` → 递归定位首个不匹配子类型（Tuple/Array/Object/Union 各一条），全部调用点改为它；`check_cast` 的判据表改为复用 `assignable` + 一条「指针互转需 unsafe」的补充规则。

## 6. 五份「沿父类链 / 线性化遍历」实现各自维护 visited 与泛型代入

- **文件**：`crates/novella-sema/src/state.rs:351-388`（`is_subclass`）、`traits.rs:43-63`（`nominal_lineage`）、`ty/generics.rs:30-70`（`member_substitution`）、`check/type_defs/inheritance.rs:64-87`（环检测）、`inheritance.rs:341-360`（`is_abstract_class`）
- **证据**：

```rust
// state.rs:365-388
let base = match &current { ty::Ty::Applied(base, _) => &**base, ty => ty };
let ty::Ty::Class(module, name) = base else { return false; };
if matches!(ancestor,ty::Ty::Class(am,an) if am == module && an == name) { return true; }  // ← 格式化后缺空格
let key = (*module, name.clone());
if !visited.insert(key.clone()) { return false; }
let Some(TypeDef::Class(def)) = self.type_defs.get(&key) else { return false; };
let Some(parent) = &def.parent_type else { return false; };
current = ty::substitute_type_params(parent, &ty::instance_substitution(self, &current));
```

```rust
// inheritance.rs:341-360 is_abstract_class —— 同样的 loop + visited + parent_resolved 上溯
```

- **影响**：五份遍历对「`Applied` 是否剥壳」「用 `parent_type` 还是 `parent_resolved`」「visited 记 key 还是记 Ty」的假设互不相同，继承环、泛型父类、trait 默认实现三条路径的行为无法从一处推断。`state.rs:372` 的 `matches!` 缺空格说明该行长期没被格式化工具覆盖（`cargo fmt` 不会重排宏内 `ty::Ty::Class(am,an)`）。
- **建议**：在 `ty/generics.rs` 或 `traits.rs` 提供 `fn inheritance_chain(sema: &Sema, ty: &Ty) -> impl Iterator<Item = (ModuleId, String, Ty)>`（自带 visited、自带 `Applied` 剥壳与 `instance_substitution` 代入），`is_subclass` / `is_abstract_class` / 环检测 / `member_substitution` 全部改写成对它的 `any`/`find`/`collect`。

## 7. `check_call` 与 `check_new` 的「泛型实参 arity + 逐参 hint 下推 + 调用核对」几乎逐段重复

- **文件**：`crates/novella-sema/src/typecheck/call.rs:288-422`、`call.rs:550-722`、`call.rs:458-526`
- **证据**：

```rust
// call.rs:350-357 与 call.rs:625-632 —— 完全相同的实参 hint 下推
let arg_tys: Vec<Ty> = args.iter().enumerate().map(|(index, arg)| {
    match param_tys.as_ref().and_then(|tys| tys.get(index)) {
        Some(param_ty) => check_expr_with_hint(sema, ctx, arg, param_ty),
        None => check_expr(sema, ctx, arg),
    }
}).collect();

// call.rs:468-479 与 call.rs:643-656 —— 同一段「泛型实参个数不匹配」诊断
if !type_arg_tys.is_empty() && type_arg_tys.len() != sig.type_param_count { … }
// call.rs:594-604 与 ty/generics.rs:14-27 —— 手工收集 class_subst，等价于 instance_substitution
let class_subst: HashMap<String, Ty> = type_key.as_ref().and_then(|key| sema.type_defs.get(key))
    .map(|def| def.type_params().iter().cloned().zip(type_arg_tys.iter().cloned()).collect())
    .unwrap_or_default();
```

`check_new` 内部还两次 `def.methods.get("__init")`（`call.rs:615`、`call.rs:668`），把同一份 `def` 克隆两次以绕过借用检查。

- **影响**：`new` 与普通调用的实参检查是两条并行维护的路径——`check_call` 修了某个 hint 规则（如空数组下推）后需要人工判断是否同步到 `check_new`；`check_call_against_sig` 的 `sig.params[offset..]` 参数窗在 `check_new` 里又通过 `instance_param_tys` 预处理了一遍，两处对「`self` 是否算显式参数」的理解不一致（前者切片后校验、后者先剥再校验）。
- **建议**：抽出 `fn check_arguments_against(sema, ctx, sig: &FunctionSig, args, type_args, span) -> Vec<Ty>`，把「泛型 arity → 参数窗 → 逐参 hint 下推 → 逐参可赋值」一次做完并返回 `arg_tys`；`check_call` 与 `check_new` 各自只保留「取 sig / 组装返回类型」。`class_subst` 改用 `ty::instance_substitution`。

## 8. 类型名 / 特殊方法名硬编码分散在 10 个文件，`CLASS_SPECIAL_METHODS` 之外还有两张影子表

- **文件**：`crates/novella-sema/src/check/mod.rs:23-24`、`check/type_defs/registration.rs:338`、`registration.rs:366-370`、`registration.rs:375`、`registration.rs:386`、`typecheck/call.rs:618`、`call.rs:668`、`check/type_defs/inheritance.rs:205`、`typecheck/body/mod.rs:356`、`typecheck/mod.rs:200`、`typecheck/expr/strings.rs:253`
- **证据**：

```rust
// check/mod.rs:23-24 —— 唯一集中表
pub(crate) const CLASS_SPECIAL_METHODS: &[&str] = &["__init", "__destroy", "__str", "__enter", "__exit"];
// registration.rs:366-370 —— 第二张表（返回类型），与上面逐项对应但需人工同步
let special_ret = match method_name {
    "__init" | "__destroy" | "__enter" | "__exit" => Some(Ty::Void),
    "__str" => Some(sema.string_type().unwrap_or(Ty::Error)),
    _ => None,
};
// 其余散落字面量
if method_name == crate::typecheck::magic::SET_ITEM { … }        // registration.rs:338
if method.name.as_str() != "__init" && count != 0 { … }           // registration.rs:386
for name in ["__enter", "__exit"] { … }                           // body/mod.rs:356
if method.name == "__destroy" && matches!(this_ty, Ty::Class(..)) // typecheck/mod.rs:200
def.methods.get("__init")                                          // call.rs:618, call.rs:668
if method.name.as_str() == "__init" { … }                          // inheritance.rs:205
lookup_typed_member_chain(sema, …, "__str", false)                 // expr/strings.rs:253
```

- **影响**：`registration.rs:366-370` 的返回类型表与 `CLASS_SPECIAL_METHODS` 名单必须一一对应（多一个 `__foo` 走 `_ => None` 分支换来的是「不校验返回类型」而不是「报错」），已构成「同一概念两处维护」。`__str`/`__init` 等字符串还被 `typecheck/` 层直接引用，破坏了 magic 协议集中在 `magic.rs` 的既有设计（`GET_ITEM`/`SET_ITEM` 已提升为常量，`__str` 却退回字面量）。
- **建议**：把特殊方法升级为 `magic.rs` 里的单一表 `const SPECIAL_METHODS: &[(&str, SpecialKind)]`，每项携带 `returns: SpecialReturn`（`Void` / `String` / `None`）；`registration.rs`、`body/mod.rs`、`typecheck/mod.rs`、`inheritance.rs`、`expr/strings.rs` 全部改引用该表常量。任务第 4 点要求的「operators.rs / magic.rs 是否还有 String 硬编码」结论是：**这两处已清理干净**（运算符文案走 `binary_op_text`/`unary_op_text`，协议名全在 `magic.rs:23-102` 的常量表），残留集中在 `registration.rs` 与 `typecheck/` 的其它文件。

## 9. 整数范围表四份并行，`int_negated_literal_fits` 用裸魔数

- **文件**：`crates/novella-sema/src/ty/literals.rs:90-101`、`literals.rs:106-115`、`literals.rs:118-134`、`ty/mod.rs:89-96`、`ty/mod.rs:103-105`、`literals.rs:166-172`
- **证据**：

```rust
// literals.rs:106-115 —— I8/I16 裸魔数，I32/I64 用表达式，风格不一
IntKind::I8 => value <= 128,
IntKind::I16 => value <= 32768,
IntKind::I32 => value <= i32::MAX as u128 + 1,
IntKind::I64 | IntKind::Isize => value <= i64::MAX as u128 + 1,
// ty/mod.rs:89-96 —— 第四份位宽表
IntKind::Usize | IntKind::Isize => 64,   // 注释称「按 64 位判定」
// literals.rs:166-172 —— 与上面口径冲突
PointerWidth::Bits32 => IntKind::U32 / I32,   // usize/isize 在 32 位目标下归一
```

- **影响**：`assignable` 的数值放行（`relations.rs:74-87`）走 `widens_to` → `bit_width`（恒 64），而诊断文案 `assign_mismatch_reason` 走 `target_int_kind`（随 `PointerWidth`）——同一份代码在 32 位目标下可能「判定通过但文案说越界」（或反之）。`128` / `32768` 两个裸数字脱离 `i8::MIN`/`i16::MIN` 语义，改动时极易写错。
- **建议**：把 10 个 `IntKind` 的范围集中成 `const INT_RANGE: &[(IntKind, i128, i128)]`（或 `IntKind::range(self, width) -> (i128, i128)`），`int_literal_fits_non_negative` / `int_negated_literal_fits` / `int_range_text` / `widens_to` 全部派生自它；`bit_width` 接受 `PointerWidth` 参数以消除第 9 条与 `target_int_kind` 的口径分歧。

## 10. `check_binary_with_hint` 承担 7 类职责、两次协议派发、嵌套 4-5 层

- **文件**：`crates/novella-sema/src/typecheck/unsafe_rules/operators.rs:31-111`
- **证据**：

```rust
if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) { … 短路 + uninitialized 快照 … }
let lhs = check_expr(sema, ctx, left);
let equality = matches!(op, BinaryOp::Equals | BinaryOp::NotEquals);
let dispatch = !(equality && (matches!(lhs, Ty::Null) || magic::is_null_literal(right)));
if dispatch && let Some(selected) = magic::binary(sema, op, &lhs, false) { … return … }   // 第一次派发
let right_hint: Option<Ty> = match op { BinaryOp::NullCoalesce => { … }, _ => None };
let rhs = match &right_hint { Some(hint) => check_expr_with_hint(…), None => check_expr(…) };
if dispatch && !(equality && matches!(rhs, Ty::Null)) {
    let selected = magic::binary(sema, op, &rhs, true).or_else(|| {
        (op == BinaryOp::NotEquals)
            .then(|| magic::inverted_equality(sema, &lhs, false)
                .or_else(|| magic::inverted_equality(sema, &rhs, true)))
            .flatten()
    });
    …
}
check_binary_operands(sema, ctx, op, left, right, span, lhs, rhs)
```

- **影响**：「左侧单目派发 → 右侧 hint → 右侧反向派发 → 相等协议反相」是一个状态机，却写成一个含 6 个早退、2 次 `magic::binary`、1 个三层闭包链的线性函数。`dispatch` 布尔与两处 `equality && … null` 判断是三处耦合的守卫；新增一个「某运算符不做协议派发」的规则需要同时理解三处。
- **建议**：抽出 `fn dispatch_left(...) -> Option<Ty>` / `fn dispatch_right(..., reversed: bool) -> Option<Ty>` 两个纯函数，主函数变成「逻辑运算分支 → 左派发 → 计算 rhs → 右派发 → 矩阵」五步顺序表；「禁止派发条件」集中为 `fn allows_dispatch(op, lhs, rhs) -> bool` 单点判断。

## 11. `BodyCtx` 18 个字段、`fork_scope` 手写逐字段克隆，`check_lambda` 再手写 10 行重置

- **文件**：`crates/novella-sema/src/typecheck/body/mod.rs:28-57`、`body/mod.rs:401-423`、`typecheck/lambda.rs:17-31`
- **证据**：

```rust
// body/mod.rs:401-423 —— 20 行逐字段克隆，唯一差异是 borrowed_receiver / fill_existing 归零
pub(crate) fn fork_scope(ctx: &BodyCtx, scope: ScopeId) -> BodyCtx {
    BodyCtx {
        inferred_returns: ctx.inferred_returns.clone(),
        … 18 个字段 …
        borrowed_receiver: false,
        fill_existing: false,
    }
}
// lambda.rs:17-31 —— fork 之后再逐字段覆盖 10 个
lambda_ctx.destructor_capture = false;
lambda_ctx.in_async = false;
… 10 行 …
```

- **影响**：`fork_scope` 是「哪些状态随作用域继承」的唯一声明点，但它靠手抄，`lambda.rs:17-31` 又抄了第二份反向清空表——两张表必须互补，靠人工保证。`BodyCtx` 已有 12 个 bool/flag 语义字段（`in_unsafe`/`in_async`/`in_loop`/`in_catch`/`reachable`/`destructor_capture`/`borrowed_receiver`/`fill_existing`…），继续增长会让「同一层里的语义」越来越难推理。
- **建议**：把可继承的上下文拆成 `#[derive(Clone)] struct Env { module, scope, this_ty, type_env, expected_return, … }`，标志位拆成 `#[derive(Clone, Copy, Default)] struct Flags { in_unsafe, in_async, in_loop, in_catch, … }`，`fork_scope` 变成 `BodyCtx { env: Env { scope, ..ctx.env.clone() }, flags: ctx.flags, ..Default }`；lambda 的清空改成 `Flags { in_async: false, ..Default::default() }` 一表达式。

## 12. `check_field_initializer` 同一 `TypeDef::Struct | Class` 解构写两遍，写回分支再嵌两层 match

- **文件**：`crates/novella-sema/src/typecheck/mod.rs:239-337`
- **证据**：

```rust
// 读（:274-280）
let existing_ty = match sema.type_defs.get(&key) {
    Some(TypeDef::Struct(StructDef { fields, .. }))
    | Some(TypeDef::Class(ClassDef { fields, .. })) => fields.iter().find(|f| f.name == field.name).map(|f| f.ty.clone()),
    _ => None,
};
// 写回（:303-316）
if let Some(def) = sema.type_defs.get_mut(&key) {
    let fields = match def {
        TypeDef::Struct(StructDef { fields, .. }) | TypeDef::Class(ClassDef { fields, .. }) => fields,
        _ => return,
    };
    if let Some(field_def) = fields.iter_mut().find(|f| f.name == field.name) { field_def.ty = inferred; }
}
```

`Struct`/`Class` 双变体解构在本 crate 中另有 `typecheck/mod.rs:222-228`、`registration.rs`、`associated.rs:76-80`、`state.rs` 等多处。

- **影响**：`StructDef` 与 `ClassDef` 共享 `fields`/`methods` 结构但类型上是两个独立 struct，导致「按名字找字段」这一操作被复制到每个消费点；`_ => return` 在写回分支里静默跳过（不属于该类型就什么都不做），而读分支用 `_ => None` 提前 `return`——同一函数两种失败语义。
- **建议**：给 `TypeDef` 加 `fn field(&self, name: &str) -> Option<&FieldDef>` / `fn field_mut(&mut self, name: &str) -> Option<&mut FieldDef>` / `fn methods(&self) -> Option<&HashMap<..>>`，消费点不再写 `match (Struct, Class)`；或者把 `StructDef`/`ClassDef` 的公共部分抽成 `CompositeDef { fields, methods, type_params }` 由两者内嵌。

## 13. 方法体 / 函数体检查两份近同构，返回类型推导启动条件重复定义

- **文件**：`crates/novella-sema/src/typecheck/mod.rs:124-185`、`typecheck/mod.rs:186-221`、`check/returns.rs:59-98`
- **证据**：

```rust
// typecheck/mod.rs:206-210
let infer_return = method.return_type.is_none()
    && !crate::check::CLASS_SPECIAL_METHODS.contains(&method.name.as_str())
    && magic::method_shape(&method.name).is_none();
// check/returns.rs:70-73 —— 同一判据取反后作为早退条件
if method.return_type.is_some() || method.block.is_none()
    || CLASS_SPECIAL_METHODS.contains(&method.name.as_str())
    || magic::method_shape(&method.name).is_some() { return; }
```

`check_function_body`（`typecheck/mod.rs:124-185`）与 `check_method_body`（`typecheck/mod.rs:186-221`）的流程完全平行：`new_scope(ScopeKind::Function)` → `BodyCtx::base` → `inferred_returns = infer_return.then(Default::default)` → `bind_type_parameters` → 逐形参 `define_local` → `check_block` / `check_stmt` → `infer_block_return` 或 `flow::check_block`。

- **影响**：「哪些声明参与返回类型推导」由两处独立表达式定义，`magic::method_shape` 与 `CLASS_SPECIAL_METHODS` 任一变化都必须双改；两处稍有偏差就会出现「函数体按无标注推导、收集器按有标注跳过」从而推导结果永不写回。两条 body 入口的平行也意味着 `freestanding::check_body_signature`、`ctx.expected_return` 等新步骤需要加两遍。
- **建议**：把 `fn collects_return_type(name: Option<&str>, annotated: bool, has_block: bool) -> bool` 放进 `check/returns.rs` 供两处共用；`check_function_body` / `check_method_body` 合并为 `check_body(sema, ctx_spec: BodySpec)`，`BodySpec` 描述「属主 / 形参列表 / 块 / 是否方法」的差异。

## 14. 返回路径分析：`exits.normal || returns || throws || breaks || continues` 手工重算，穷尽性闭包重复构造 5 次

- **文件**：`crates/novella-sema/src/typecheck/flow.rs:9-40`、`flow.rs:50`、`flow.rs:62`、`flow.rs:129`、`flow.rs:136`、`flow.rs:143`、`flow.rs:237-260`
- **证据**：

```rust
// flow.rs:245-249 —— Try 的 finally 分支手工重算「是否有任何出口」
let reachable = exits.normal || exits.returns || exits.throws || exits.breaks || exits.continues;
// 同一表达式在 5 处构造（闭包体完全一致）
statement(body, &|span| sema.exhaustive_matches.contains(&(module, span)))
```

`Exits` 已实现 `union`（`:25-33`）与 `then`（`:34-40`），但没有 `is_reachable()`；`Exits` 的 5 个字段在 `union`、`loop_exits`（`:110-118`）、`finish_for_header`（`:279-287`）、Try 分支各被逐字段处理一次。

- **影响**：`Exits` 的字段集合是「返回路径 + 异常 + 循环跳转」的联合建模，任何新增出口种类（例如未来的 `yield`/`suspend`）都要改 `union`、`then`、`loop_exits`、`finish_for_header`、Try 的 `reachable` 五处；漏改的表现是「该出口被当作正常到达」，即误报「存在正常到达末尾而未返回值的路径」。
- **建议**：`Exits` 增加 `fn is_reachable(&self) -> bool`、`fn with(&self, kind: ExitKind) -> Self`，并改为 `#[derive(Default)] struct Exits { mask: u8 }`（或小整数位集合）使 `union = |`、`is_reachable = mask != 0`，字段增减只改 `ExitKind` 常量；穷尽性闭包提为 `fn exhaustive(sema: &Sema, module: ModuleId) -> impl Fn(Span) -> bool` 返回一次。

## 15. `check_assignment` 内部 7 段顺序状态 + `check_arithmetic_compound` 与 `magic::compound` 两份 AssignOp→BinaryOp 映射

- **文件**：`crates/novella-sema/src/typecheck/unsafe_rules/assignment.rs:10-116`、`assignment.rs:207-235`、`typecheck/magic.rs:219-225`
- **证据**：

```rust
// assignment.rs:207-235 —— 10 臂直译
AssignOp::PlusAssign => check_arithmetic(sema, ctx, BinaryOp::Plus, lhs, rhs, span),
… 9 臂同形 …
AssignOp::Assign => unreachable!("Assign 已在调用点处理"),
// magic.rs:219-225 —— 同一映射已存在一份（含协议方法名）
pub(super) fn compound(op: AssignOp) -> Option<(BinaryOp, &'static str)>
```

`check_assignment` 的 107 行里依次做：RHS 早算（`:23-31`）→ 协议下标分支（`:32-44`）→ 左值校验或占位记录（`:46-63`）→ 复合协议查找（`:64-77`）→ 动态转换（`:78-83`）→ uninitialized 回写（`:84-85`）→ 赋值 / 复合分派（`:86-114`）。

- **影响**：`AssignOp → BinaryOp` 现在是两张表：`magic.rs` 的 BINARY 表（附带 `__iadd` 等方法名）和 `assignment.rs` 的手写 match。新增复合运算符时若只改一处，会在「有协议方法」与「无协议方法」两条路径上得到不同语义。`check_assignment` 的 7 段顺序没有显式阶段名，`early_value` / `protocol_receiver` / `selected` 三个 `Option` 的交互只能靠通读理解。
- **建议**：`check_arithmetic_compound` 改为 `magic::compound(op).map(|(binary, _)| binary).unwrap_or_else(unreachable)` 驱动（去掉 10 臂 match）；`check_assignment` 抽出三个私有函数 `prepare_value`（RHS 早算 + 协议下标）、`resolve_place`（左值/占位 + uninitialized）、`apply_op`（分派），主函数变成三段调用。

## 16. `check/globals.rs` 用裸 `u8` 复刻 `check/returns.rs` 的三态状态机

- **文件**：`crates/novella-sema/src/check/globals.rs:10-14`、`globals.rs:69-107`、`check/returns.rs:28-32`、`returns.rs:112-152`
- **证据**：

```rust
// globals.rs:10-14
pub(crate) struct GlobalTask { pub declaration: VariableStmt, state: u8 }   // 0/1/2 魔法数
// globals.rs:69-107
match task.state {
    2 => return,
    1 => { … "顶层变量的类型推导存在循环依赖" … 符号 ty 置 Error … }
    _ => {}
}
sema.global_tasks.get_mut(&key).expect("已收集变量").state = 1;
…
sema.global_tasks.get_mut(&key).expect("已收集变量").state = 2;
// returns.rs:28-32 —— 同名状态机
enum State { Pending, Resolving, Done }
```

`returns.rs:123/134/146` 另有三次 `sema.return_tasks.get_mut(&key).expect("已收集函数").state = …`。

- **影响**：两个「按需延迟推导 + 环检测」任务（返回类型、全局变量类型）用两套表示（枚举 vs 裸 `u8`）实现同一算法；`2`/`1`/`_` 分支不具名，新增中间状态无法扩展；`expect` 在 5 处假设「收集阶段一定登记过」，一旦收集阶段漏登记就是 panic 而不是诊断。
- **建议**：把三态提为 `check/mod.rs` 里的共享 `pub(crate) enum TaskState { Pending, Resolving, Done }`，`GlobalTask.state` 改用它；更进一步抽出 `fn resolve_on_demand<K, T>(tasks: &mut HashMap<K, T>, key, on_cycle: impl FnOnce() -> …) -> …` 的通用驱动，两处只提供「如何推导」与「环上如何报错」。

## 17. `resolve/types.rs` 之外仍有两处手工回推 span（`span.end - name.len()`、`span.start + type_name.len()`）

- **文件**：`crates/novella-sema/src/typecheck/member.rs:76-86`、`member.rs:128-149`、`member.rs:392-395`
- **证据**：

```rust
// member.rs:76-86 —— 用「成员名长度」从复合 span 反推名字区间
sema.record_reference(ctx.module, Span { start: span.end.saturating_sub(name.len()), end: span.end }, &symbol);
// member.rs:392-395 —— 同一算法第二份（record）
let reference = Span { start: span.end.saturating_sub(name.len()), end: span.end };
// member.rs:128-149 —— 用类型名长度反推，再重新构造 TypeExpr 交给 resolve_type
Span { start: span.start, end: span.start + type_name.len() }
```

- **影响**：`span.end - name.len()` 只在「成员访问 span 恰好以名字结尾且名字无空白」时成立；`span.start + type_name.len()` 更脆弱（`static_receiver` 为解析别名而重建 `TypeExpr`，`qualifier: vec![]` 丢弃了别名可能带的限定路径）。项目规范要求「位置换算只在 core」，而这里是按标识符长度做字节算术，与 `novella_core::line_index`/`decor` 的定位体系并列存在第三套做法。
- **建议**：`check_member_access` / `check_static_member` 的调用方（`check_expr_inner` 的 `MemberAccess` 臂）本就持有成员名 token 的 span，直接作为参数传入（或在 `Expression::MemberAccess` 上补 `name_span` 字段，AST 已有此类先例：`Symbol.name_span`）；`static_receiver` 改为接收 `&TypeExpr`（别名分支直接复用原节点的 `span`/`qualifier`），不再重建语法节点。

## 18. 文档与实现不一致：`sema-coverage.md` 的实施日志仍在引用重构前的旧路径，`deep-dive.md` 有重复段与测试数冲突

- **文件**：`docs/sema-coverage.md:257+`（§3 逐日实施日志）、`docs/sema-deep-dive.md:926`、`docs/sema-deep-dive.md:928`、`docs/sema-deep-dive.md` §1.2 目录树、§11.2
- **证据**：

```
sema-coverage.md §3 日志中的路径：crates/novella-sema/src/typecheck/expr.rs（现 typecheck/expr/ 目录）
                              typecheck/unsafe_rules.rs（现 unsafe_rules/ 目录）
                              typecheck/body.rs（现 body/ 目录）、module/cfg.rs（现 src/cfg.rs）
sema-deep-dive.md:926 与 :928 两行完全相同的「> **可改进点（非缺陷结论）**：resolve_trait_impls …」
sema-deep-dive.md §1.2 目录树仍写 module/cfg.rs；§11.2 写 106 passed / 24 suites，coverage 基线写 102 passed
```

另：`sema-coverage.md` 矩阵中 S09R 标注 `body/mod.rs` 处理 `Yield`，实测 `Yield` 在 `typecheck/flow.rs:281` 的 `statement_inner` 中处理。

- **影响**：`sema-coverage.md` 被 AGENTS.md 定义为「唯一权威矩阵」，但 §3 的日志体量与矩阵相当且承载历史状态，读者无法区分「当前能力」与「某日改了什么」；旧路径会让按文档检索源码的人直接找不到文件。`deep-dive.md` 的重复段落属于明显的复制粘贴残留。
- **建议**：§3 实施日志整体迁到 `docs/reports/`（按日期一文件）或直接删除（git 历史已保存）；矩阵表格中每条规则补「当前位置」（文件:行）而非过程描述；`deep-dive.md:926-928` 删除重复的一行，§1.2 目录树与 §11.2 计数改为从 `just test` 实测更新。

## 19. 五处 `let _ =` 静默丢弃（含一处 prelude 定义失败被吞）

- **文件**：`crates/novella-sema/src/typecheck/body/bindings.rs:160`、`typecheck/member.rs:63`、`check/type_defs/inheritance.rs:235`、`inheritance.rs:326`、`resolve/imports.rs:256`
- **证据**：

```rust
let _ = (negative, value);                                    // bindings.rs:160
let _ = ns_target;                                            // member.rs:63（Ok(Namespace) 分支，值不必用）
if base.is_abstract { let _ = class_name; }                   // inheritance.rs:326（空操作块）
let _ = sema.scopes.define(prelude, symbol);                   // imports.rs:256（定义失败被吞）
let _ = base;                                                 // inheritance.rs:235（死分支内，见第 1 条）
```

- **影响**：`imports.rs:256` 是实质问题——prelude 符号定义失败（重名）时没有任何诊断，后续解析会以「未定义标识符」的形式在别处报错，偏离「不得静默吞错」。其余四处是占位：`member.rs:63` 可以改为 `Ok(ExportTarget::Namespace(_))`，`inheritance.rs:326` 的空 if 整块删除，`bindings.rs:160` 与第 1 条的死分支一并处理。
- **建议**：`imports.rs:256` 改为检查返回值并 `sema.warning/error`（或断言不变量并注释理由）；其余四处按上述直接删除或改模式匹配。

## 20. `trait_method_hit` / `trait_member_type` 用 `Option<Result<T, ()>>` 表达三态，调用方两处各解一层

- **文件**：`crates/novella-sema/src/typecheck/member.rs:296-320`、`member.rs:322-364`、`typecheck/body/iteration.rs:55-70`
- **证据**：

```rust
// member.rs:322 —— 返回 Option<Result<(Ty, MemberHit), ()>>：None=无候选 / Err=已报错 / Ok=命中
pub(crate) fn trait_method_hit(…) -> Option<Result<(Ty, MemberHit), ()>> { … }
// member.rs:306-319 —— 立刻被压平成 Option<Ty>
.map(|result| { let (trait_ref, hit) = result?; … Ok(…) })
.map(|result: Result<Ty, ()>| result.unwrap_or(Ty::Error))
// iteration.rs:61-68 —— 第二个调用方解同一结构
match trait_method_hit(…) {
    Some(Ok((trait_ref, found))) => { applied_trait = Some(trait_ref); hit = Some(found); }
    Some(Err(())) => return Ty::Error,
    None => {}
}
```

- **影响**：`()` 作为错误载荷丢掉了「为什么失败」（多重命中 vs 不可动态派发），两个调用方只能统一翻译成 `Ty::Error`；嵌套 `Option<Result<_, ()>>` 需要「`?` 在 Option 里、`Ok` 包在内层」的双重理解才能读对，是这个模块可读性最差的签名之一。
- **建议**：改为 `enum TraitLookup { Hit { trait_ref: Ty, hit: MemberHit }, NotFound, Failed }`，或至少 `Result<Option<(Ty, MemberHit)>, ()>`（`Ok(None)` = 无候选，语义方向更直白）；错误载荷换成 `Diagnostic` 或让函数自己报错后返回 `Option<MemberHit>`。

---

## 测试覆盖（任务第 5 点核对结果）

任务假设「`tests.rs` 与 `typecheck` 内联测试重复」**不成立**：`crates/novella-sema` 内 `#[test]` 仅出现在 `src/tests.rs:7/79/118/147/184` 共 5 个（`references_and_type_summaries_are_collected`、`expression_types_are_exported_as_type_views`、`reexport_chain_resolves_through_public_imports`、`private_import_is_not_reexported`、`reexport_cycle_is_reported`），`typecheck/` 目录无任何内联测试，也没有 `crates/novella-sema/tests/` 目录。真正的问题是反方向的：**整个类型检查器（14000 行、20 条问题中的 19 条所在区域）没有一条行为测试**，5 个测试全部围绕 overlay / 再导出。上述任一重构（尤其是第 1、3、4、11 条的语义保持型改动）都无回归网可依。

---

## 结构性结论

**存在架构层面的重复，且集中在「名义类型解析」这一横切关注点。**

1. **「一个 Ty → 一个名义键」被实现了三次**（`typecheck/mod.rs:229-236`、`typecheck/member.rs:366-372`、`traits.rs:10-15`），而「沿父链遍历求解泛型实参」被实现了五次（`state.rs:351-388`、`traits.rs:43-63`、`ty/generics.rs:30-70`、`inheritance.rs:64-87`、`inheritance.rs:341-360`）。这两个原语是所有成员访问、继承校验、trait 解析、子类型判定的底座，它们各自维护 `Applied` 剥壳、visited 环检测、`parent_type` vs `parent_resolved` 的选择，导致同一语义问题在五个地方有五种边界行为。这是本 crate 最值得根治的耦合点——它不属于「重复代码」，而属于**缺失的原语**：没有 `TypeCursor`/`NominalType` 这样的抽象，于是每个消费者都重写一遍游标。

2. **「一次调用的形状检查」有四个并行实现**：`magic::method_shape`（协议方法形状表）、`registration.rs:337-364`（魔法方法注册校验）、`call.rs:46-69 protocol_method`（协议派发取签名）、`body/iteration.rs:44-105 protocol_method_return`（迭代协议）。四者都在回答「这个方法是否满足 X 形状」，判定项大量重叠（`is_static` / 显式参数个数 / `type_param_count` / `self` 跳过），但错误文案与容错策略各自演化。

3. **`typecheck/` 与 `check/` 的分层边界是「按时间」而非「按职责」**：`check/returns.rs` 做返回类型推导（本属类型检查），`typecheck/mod.rs` 做方法体编排（本属流程控制），`check/globals.rs` 与 `check/returns.rs` 实现同一状态机，`typecheck/mod.rs:206-210` 与 `check/returns.rs:70-73` 定义同一判据。更具体地，`for module in 0..sema.modules.len()` 这一「全模块扫描」样板在 7 个文件中各写一遍并各自持有阶段语义：`check/globals.rs:15`、`check/type_defs/associated.rs:61`、`check/fields.rs:31`、`check/returns.rs:33`、`resolve/imports.rs:173`、`resolve/imports.rs:259`、`freestanding.rs:152`（其中 `freestanding.rs:152` 连 `syntax_failed` 跳过都未写）。管线本身（`analysis.rs` → `module/loader.rs` → `check/decl.rs` → `typecheck/`）没有显式的阶段清单，阶段顺序由 `analysis.rs` 的调用序列隐式表达，新增一个「全模块后置检查」必须自己再抄一遍扫描循环 + `syntax_failed` 跳过条件。

4. **文档与代码的耦合已失效**：`sema-coverage.md` 声称是唯一权威矩阵，却把历史实施日志与当前能力混在一起并引用旧路径；`deep-dive.md` 自称「实现快照」但目录树、测试计数均已过期。项目规范「文档明显过时须同步」在此处是持续违反状态，而 §11.3 那 10 条自认缺陷中有 3 条与本报告第 1、9、17 条重合，说明问题已被识别但未收敛。

**优先级建议**：先做 1（`static_only` 死参数 → 恢复或被删除的死诊断，属于正确性 bug 而非风格问题）→ 3 + 2（`FunctionSig::receiver_offset`/`explicit_params` 与 `stacker` 常量，两者都是低风险高收益的机械收敛）→ 6（父链遍历原语，改动面大但消除最大耦合）→ 4 + 14（三元与 `Exits`，需先补行为测试）→ 12 + 16（`TypeDef` 访问器与共享状态机）→ 18 + 19（文档与静默丢弃）→ 其余。第 8、9 条（硬编码与范围表）可与任何一步并行，风险最低。

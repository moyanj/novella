# novella-sema 深度技术文档

> 生成时间：2026-11-20 · 基于当前工作树源码逐文件精读
> 适用对象：需要完整理解语义分析层工作流程、代码架构、设计决策的维护者
> 阅读前提：已了解 novella 的 TokenKind / AST / Parser 基础（见根目录 `AGENTS.md`、`docs/design.md`）

---

## 导读：这份文档怎么读

`novella-sema` 是 novella 编译器的**静态语义分析层**。它接收词法/语法产物，完成从"源码文本"到"带类型、带绑定、带诊断的只读结果"的全部工作。本组织按以下层次展开：

| 章节 | 内容 | 对应源码 |
|------|------|----------|
| 第 1 章 | 模块定位、目录结构、依赖、设计哲学 | 全局 |
| 第 2 章 | 公开契约（下游唯一可见面） | `lib.rs`、`api.rs` |
| 第 3 章 | 编排中枢与内部可变状态 | `analysis.rs`、`state.rs` |
| 第 4 章 | Phase 0：模块加载、路径解析、cfg | `module/*` |
| 第 5 章 | 符号系统与名称解析 | `symbol.rs`、`resolve/*`、`declarations.rs` |
| 第 6 章 | 类型系统 `ty/` | `ty/*` |
| 第 7 章 | `check/`：类型定义与声明检查 | `check/*` |
| 第 8 章 | `typecheck/`：表达式与 unsafe 规则 | `typecheck/expr/*`、`typecheck/call.rs`、`typecheck/member.rs` |
| 第 9 章 | `typecheck/`：语句、控制流、绑定、match、lambda | `typecheck/body/*`、`typecheck/matching.rs`、`typecheck/lambda.rs`、`typecheck/flow.rs` |
| 第 10 章 | 横切机制：诊断、级联抑制、取消、错误恢复、引用 | 全局 |
| 第 11 章 | 设计不变量、约定、可改进点 | 全局 |

贯穿全篇的四条主线，抓住它们就抓住了这个模块：

1. **诊断只有一份**。所有阶段的诊断统一存进 `DiagnosticReport.modules`，靠 `syntax_diagnostic_count` 切分阶段，靠 `ModuleDiagnostics::ordered()` 按 span 排序。禁止并列字段。
2. **词法/语法只跑一次**。`analyze` 内部完成 lex+parse，产物（`tokens`/`program`）随 `SemaResult.modules` 交还下游；CLI/codegen/LSP 不得重跑。
3. **签名占位与回填分离**。Phase 1 只登记名字与种类（函数签名用 `name: None` 占位），Phase 2b 才解析真签名回填；这是循环导入与前向引用正确的根本。
4. **`Unknown` / `Error` 双档占位 + `is_opaque()` 级联抑制**。前面已报错的地方用占位类型继续走完分析，检查时一律放行，避免一个错误炸出一片次生错误。

---

## 第 1 章 模块定位与整体架构

### 1.1 定位

`novella-sema` 位于依赖链 `core ← ast ← parser ← sema ← hir ← codegen ← cli` 的中段，是 AST 之后、HIR 之前的唯一语义权威。它负责：

- **模块系统**：文件即模块，`@:` / `super:` / `std:` 别名，`nov.toml` 定位 crate 根，循环导入安全；
- **名称解析**：作用域树、可见性（`pub`/`pri`/`pkg`）、具名与命名空间导入、跨模块符号查找；
- **类型系统**：名义类型（struct/class/trait）、泛型（带声明身份的参数）、类型别名、字面量未定型与范围判定、可赋值性；
- **检查**：类型定义注册、继承/trait 实现、覆写校验、字段与全局变量的按需定型、入口 `main` 约束、extern/unsafe 规则、表达式与语句类型检查、返回路径与确定赋值分析、match 穷尽性。

它**不做**代码生成（那是 `novella-hir` + `novella-codegen` 的事），也不重复词法/语法。它的输出 `SemaResult` 是只读数据契约，供 CLI（JSON 输出）、HIR（定型指令流）、LSP（跳转/补全）共同消费。

### 1.2 目录结构

```
crates/novella-sema/src/
├── lib.rs              # crate 根：6 阶段管线说明、公开导出
├── api.rs              # 公开只读数据契约（SemaResult / TypeView / FunctionView / ...）
├── analysis.rs         # 阶段编排中枢（analyze 系列入口 + AnalysisOptions）
├── state.rs            # 内部可变状态 Sema（诊断、类型表、引用、继承判定...）
├── symbol.rs           # ScopeTree / Symbol / SymbolKind / NsTarget
├── declarations.rs     # DeclarationIndex（按名字/位置定位声明）
├── module/
│   ├── mod.rs          # Module / ModuleId / ParsedSource / FileLoader / OverlayLoader
│   ├── loader.rs       # Phase 0：worklist 加载 + lex + parse + cfg + 扫导入
│   ├── resolve_path.rs # 导入路径解析（别名、crate 根、std 根）
│   └── cfg.rs          # cfg 条件求值（all/any/not、target_os/feature...）
├── resolve/
│   ├── mod.rs          # Phase 1 collect_module + Phase 2a bind_imports + 类型解析入口
│   ├── collect.rs      # 声明收集（骨架 + 符号登记）
│   ├── imports.rs      # 导入绑定
│   └── types.rs        # TypeExpr → Ty（含别名展开、泛型约束登记与满足）
├── ty/
│   ├── mod.rs          # Ty / TypeDef / FunctionSig / TypeView 投影 / Display
│   ├── relations.rs    # ty_eq / sig_eq / assignable
│   ├── generics.rs     # param_key / substitution / infer_type_param / 内建异常根
│   └── literals.rs     # 数字字面量分类、数值范围判定、预定义名映射
├── check/
│   ├── mod.rs          # 阶段入口汇总、特殊方法名规整
│   ├── decl.rs         # Phase 2d：模块级声明检查、main 入口、默认值/函数体
│   ├── type_defs/
│   │   ├── mod.rs      # build_type_defs 分派
│   │   ├── registration.rs # 字段/方法/函数签名注册、特殊方法、extern 规则
│   │   └── inheritance.rs  # 继承/trait 分类、环检测、覆写、trait 实现、抽象类
│   ├── fields.rs       # 字段初值按需定型（三态环检测）
│   └── globals.rs      # 顶层变量按需定型（三态环检测）
└── typecheck/
    ├── mod.rs          # 分派入口（函数体/方法体/字段初值/顶层语句/默认值）
    ├── body/
    │   ├── mod.rs      # BodyCtx + 语句检查
    │   ├── try_flow.rs # try/catch/finally 确定赋值
    │   └── bindings.rs # 局部声明、绑定模式、字面量归一化
    ├── expr/
    │   ├── dispatch.rs # check_expr 唯一登记出口 + 表达式分派
    │   ├── hints.rs    # check_expr_with_hint 双向检查下推
    │   ├── names.rs    # 标识符值/类型分类
    │   ├── aggregates.rs # 数组/对象/StructInit
    │   ├── strings.rs  # 字面量与 f-string
    │   ├── unary.rs    # 一元运算、取址/解引用
    │   ├── operators.rs # 二元运算、cast、unsafe 闸口
    │   └── assignment.rs # 赋值顺序、左值判定
    ├── flow.rs         # 返回落尾分析（Exits）
    ├── matching.rs     # match 模式覆盖与穷尽性
    ├── lambda.rs       # lambda 检查与块体返回推导
    ├── member.rs       # 成员访问、可见性、继承链查找
    └── call.rs         # 调用/索引/new/泛型推导
```

依赖（`Cargo.toml`）：仅 `novella-core`、`novella-ast`、`novella-lexer`、`novella-parser`（均 workspace 内）加 `stacker`（递归分段栈），无 features。

### 1.3 设计哲学

- **可恢复收集**：遇到错误不立即中断，用 `Ty::Error`（已报明确诊断）/`Ty::Unknown`（无法推导）占位继续，最后一起产出诊断。只有 CLI 入口在存在诊断时 fail-fast 不进 codegen。
- **分母唯一**：所有诊断、所有类型表、所有引用表都只有一份存储，阶段之间传递的是可变状态 `Sema`，最终一次性投影成只读的 `SemaResult`。
- **位置即身份**：泛型参数 `T` 的身份是 `T@m3:123`（模块 + 声明起点字节偏移）；局部变量确定赋值用"定义处 Span"作键；这些设计让同名遮蔽、跨模块同名泛型都正确。
- **跨阶段解耦**：`is_subclass` 需要继承链，但 `assignable` 在 `ty/` 里，于是用闭包注入 `&mut dyn FnMut(&Ty,&Ty)->bool`（`relations.rs:48-49`），`ty/` 不依赖 `check/`。
- **防御性递归**：所有递归入口套 `stacker::maybe_grow(128*1024, 2*1024*1024)`，合法深嵌套不爆栈；各继承/查找路径用 `HashSet` 做环检测。

### 1.4 六阶段管线总览

入口 `analyze` → `analyze_with`（用 `FsLoader`）→ `analyze_with_target`（带指针宽度）→ `analyze_with_options`（完全配置）。核心编排在 `analysis.rs:68-217`：

```
Phase 0  load_modules（loader.rs）
         读文件 → lex → parse → cfg 裁剪 → 建模块全局 Scope → 扫 import 扩展 worklist
         ↓
Phase 1  collect_module × N（resolve/collect.rs）
         声明骨架入 type_defs、符号入 ScopeTree（函数签名 name:None 占位）
         ↓
Phase 2a bind_imports × N（resolve/imports.rs）
         具名/命名空间绑定、可见性检查、std:exception 特例
         ↓
Phase 2b build_type_defs × N（check/type_defs/registration.rs）
         解析字段/方法/函数真签名，回填 Phase 1 占位
         ↓
Phase 2c resolve_class_parents（全局一次，inheritance.rs）
         + globals::collect + fields::check_fields + globals::check_all
         ↓
Phase 2d check_module × N（check/decl.rs）
         函数体/方法体/顶层语句检查、main 入口、返回落尾
         ↓
         泛型实参约束统一校验（analysis.rs:125-139）
         ↓
         装配 SemaResult（analysis.rs:150-216）
```

每个阶段之间都检查 `loader.is_cancelled()`；中途取消则加一条全局诊断"分析已取消，产物不可使用"（`:143-148`）。

---

## 第 2 章 公开契约：`lib.rs` 与 `api.rs`

### 2.1 `lib.rs`（1-53 行）

crate 根注释定义范围与 6 阶段管线。公开导出：

- 入口函数：`analyze` / `analyze_with` / `analyze_with_target` / `analyze_with_options`；
- 数据契约：`api::*` 全部重导出；
- 加载器：`FileLoader` / `FsLoader` / `ModuleId` / `OverlayLoader` / `ParsedSource`；
- 路径工具：`module_key`。

内部状态 `Sema` 仅 `pub(crate)`，对下游不可见。核心契约写死在文档注释里：**词法/语法在 `analyze` 内只跑一次，下游不得重跑 lexer/parser**。

### 2.2 `SemaResult`（api.rs:245-263）

```rust
pub struct SemaResult {
    pub declarations: DeclarationIndex,
    pub exhaustive_matches: HashSet<(ModuleId, Span)>,
    pub diagnostics: DiagnosticReport,
    pub bindings: BindingTable,
    pub types: TypeHierarchy,
    pub modules: ModuleOutput,
    pub expression_types: ExpressionTypes,
    pub annotation_types: ExpressionTypes,
    pub declaration_types: ExpressionTypes,
}
```

各表含义：

- `declarations`：声明索引，按名字/位置定位函数、类型及其成员；
- `exhaustive_matches`：被判定为穷尽的 match 的 `(ModuleId, Span)` 集合，返回路径分析、HIR 共用；
- `diagnostics`：唯一诊断树 `{ global, modules: [{ id, path, diagnostics }] }`；
- `bindings`：引用表与定义表（`Definition` / `DefinitionKind`），供 LSP 跳转；
- `types`：类型摘要（`TypeSummary` / `FieldSummary` / `MethodSummary` / `TypeSummaryKind(Struct/Class/Trait/Alias)`），供 outline；
- `modules`：每模块的 AST + tokens + 源文本；
- `expression_types` / `annotation_types` / `declaration_types`：表达式/类型标注/声明的类型投影，键均为 `(ModuleId, Span)`。

### 2.3 `ModuleDiagnostics` 与诊断切片（api.rs）

`ModuleDiagnostics` 保留真实 `syntax_diagnostic_count`，据此把合并后的诊断流切成两阶段视图：

- `syntax_diagnostics()`：前 `syntax_diagnostic_count` 条；
- `semantic_diagnostics()`：其余；
- `ordered()`：按 `location.span.start` 统一排序。

这是"诊断只有一份"的实现：底层一个 `Vec<Diagnostic>` 里词法/语法/语义按 span 合并排序，视图层按计数切分，而不是存三份。

`AnalyzedModule`（api.rs:103-119）：`id` / `path` / `source` / `tokens` / `program` / `syntax_failed` / `load_failed`。`ModuleOutput::get` / `entry` / `find_by_path` / `find_entry`——路径两侧都经 `module_key` 规范化后再比较。

### 2.4 `TypeView`（api.rs:159-191）

内部 `Ty` 因持有 `Rc<FunctionSig>` 与内部定义类型无法直接导出，`Ty::view()` 投影为公开的 `TypeView`：

```
Applied / Future / Object / Void / Boolean / String /
Int(&'static str) / Float(&'static str) / IntLiteral / FloatLiteral /
Null / Pointer / Array / Tuple / Union / Function / Struct / Class /
Trait / TypeParam / Unknown / Error
```

要点：`Unknown` 与 `Error` 分别投影为 `TypeView::Unknown` / `TypeView::Error`（下游据此拒绝生成代码）；数值变体取 `kind.name()`；字面量擦除载荷；`Function` 投影为 `TypeView::Function(Box<FunctionView>)`，携带完整结构，不因 `name: None` 退化。

### 2.5 `FunctionView`（api.rs:202-224）

```rust
pub struct FunctionView {
    pub module: ModuleId,
    pub name: Option<String>,
    pub params: Vec<...>,
    pub ret: ...,
    pub type_param_count: usize,
    pub is_async: bool,
    pub is_extern: bool,
    pub receiver: bool,
}
```

`name: None` 用于 lambda / 占位签名；`self` 不占 `params` 位，而由 `receiver: true` 表达。这是 HIR 建符号键与函数类型时的依据（`module`+`name` 为名义键；lambda 实体另用模块+源码位置作唯一键）。

### 2.6 定义与引用

`Definition` / `DefinitionKind`（`Function` / `Type` / `Variable` / `Parameter` / `Import`）。`definition_index` 显式跳过所有 Import 符号，只登记真实定义与字段/方法——所以跳转直达源头而非 import 行。

### 2.6 定义与引用

`Definition` / `DefinitionKind`（`Function` / `Type` / `Variable` / `Parameter` / `Import`）。`definition_index` 显式跳过所有 Import 符号，只登记真实定义与字段/方法——所以跳转直达源头而非 import 行。

---

## 第 3 章 编排中枢 `analysis.rs` 与内部状态 `state.rs`

### 3.1 入口与配置（`analysis.rs:1-66`）

四个入口层层收窄配置：

```rust
analyze(entry: &Path) -> SemaResult                                  // FsLoader + 默认配置
analyze_with(loader, entry) -> SemaResult
analyze_with_target(loader, entry, target: AnalysisTarget) -> SemaResult
analyze_with_options(loader, entry, options: AnalysisOptions) -> SemaResult
```

- `PointerWidth { Bits32, Bits64 }`：`Default` 按宿主 `usize::BITS` 选（`:26-32`）；
- `AnalysisTarget { pointer_width }`（`:34-49`）：注入 `Sema.target`，影响 `usize/isize` 的范围判定与指针宽度；
- `AnalysisOptions { target, cfg, std_root, module_only }`（`:7-14`）：
  - `cfg: CfgOptions`：目标 OS/arch/family/features；
  - `std_root: Option<PathBuf>`：标准库根，`std:` 导入必须指向真实声明，不能跳过类型检查；
  - `module_only: bool`：为 true 时不检查 `main` 入口（LSP 单模块场景）。

### 3.2 `analyze_with_options` 全流程（:68-217）

1. **建状态**：`Sema::new` 设 `module_only` / `target` / `std_root`。

2. **Phase 0**（`:78-82`）：`module::load_modules(loader, entry, &mut sema.scopes, &options)`，把 `modules` / `global_diagnostics` / `module_by_path` 装进 `sema`。

3. **闭包 `complete`（:84-142）** 顺序执行后续阶段，每阶段开头 `if loader.is_cancelled() { return false; }`：
   - Phase 1：`for id in 0..modules.len() { resolve::collect_module(&mut sema, id) }`
   - Phase 2a：`for id { resolve::bind_imports(&mut sema, id) }`
   - Phase 2b：`for id { check::build_type_defs(&mut sema, id) }`
   - Phase 2c：`check::resolve_class_parents(&mut sema)`（全局一次）+ `check::globals::collect` + `check::fields::check_fields` + `check::globals::check_all`
   - Phase 2d：`for id { check::check_module(&mut sema, id) }`
   - 泛型约束统一校验（`:125-139`）：遍历 `sema.generic_uses.clone()`，`subst = ty::instance_substitution(&sema, &ty)`，逐 `(param, actual)` 若 `type_param_bounds` 有 bound（`substitute_type_params` 后）且 `!satisfies_bound` → `type_error("类型实参 `{actual}` 不满足 `{bound}` 约束")`。
   - 末尾 `!is_cancelled()`。

4. **取消处理**（`:143-148`）：若 `complete()` 返回 false，加全局诊断"分析已取消，产物不可使用"。

5. **装配 `SemaResult`（:150-216）**：
   - `types = sema.type_summaries()`、`definitions = sema.definition_index()`；
   - 遍历 `mem::take(&mut sema.modules)`，按 `syntax_diagnostic_count` 切诊断，构造 `AnalyzedModule` + `ModuleDiagnostics`；
   - 类型表统一投影：`types.into_iter().map(|(k, ty)| (k, ty.view()))`；
   - 最后 `result.declarations = DeclarationIndex::build(&result.modules, &result.bindings)`（`:215`，依赖已组装的 modules 与 bindings）。

### 3.3 内部状态 `Sema`（state.rs:21-47）

```rust
pub(crate) struct Sema {
    pub(crate) std_root: Option<PathBuf>,
    pub(crate) module_only: bool,
    pub(crate) exception_imports: HashSet<(ModuleId, String)>,
    pub(crate) generic_uses: HashMap<(ModuleId, Span), Ty>,
    pub(crate) type_param_bounds: HashMap<String, Ty>,
    pub(crate) target: AnalysisTarget,
    pub(crate) exhaustive_matches: HashSet<(ModuleId, Span)>,
    pub(crate) global_tasks: HashMap<(ModuleId, Span), GlobalTask>,
    pub(crate) global_tasks_by_name: HashMap<(ModuleId, String), Span>,
    pub(crate) field_tasks: HashMap<FieldKey, FieldTask>,
    pub(crate) modules: Vec<Module>,
    pub(crate) scopes: ScopeTree,
    pub(crate) type_defs: HashMap<(ModuleId, String), TypeDef>,
    pub(crate) resolving_aliases: HashSet<(ModuleId, String)>,
    pub(crate) module_by_path: HashMap<PathBuf, ModuleId>,
    pub(crate) global_diagnostics: Vec<Diagnostic>,
    pub(crate) references: HashMap<(ModuleId, Span), Definition>,
    pub(crate) expression_types: HashMap<(ModuleId, Span), Ty>,
    pub(crate) annotation_types: HashMap<(ModuleId, Span), Ty>,
    pub(crate) declaration_types: HashMap<(ModuleId, Span), Ty>,
}
```

关键子表：

- `type_defs`：类型定义注册表，键 `(ModuleId, 类型名)`，值为 `TypeDef`（Struct/Class/Trait/Alias）；
- `type_param_bounds`：泛型参数约束，键为带身份的 `param_key`；
- `generic_uses`：所有泛型实例使用点，供最终阶段统一校验 bound；
- `exception_imports`：`std:exception` 具名/命名空间导入登记（内建异常根特例）；
- `field_tasks` / `global_tasks` / `global_tasks_by_name`：字段与顶层变量的按需定型三态表（见 7.4）；
- `references`：引用 → 定义映射，供 LSP 跳转；
- 三张类型表：表达式、标注、声明，键都是 `(ModuleId, Span)`。

### 3.4 诊断方法（state.rs）

- `error(NameError)` / `error_with_code`：产生 NVL3001 名称诊断；
- `type_error`：NVL3002；`decl_error`：NVL3003；`unsafe_error`：NVL3004；`warning`：NVL3009；
- `record_reference`（:123-138）：跳过零长 span；具名导入 `NamedImport{target:Some(t), imported}` 追到目标模块的原始定义，追不到退回本地；写 `references[(module, span)] = Definition::from_symbol`（取 `name_span`）。

### 3.5 继承/可赋值判定（state.rs:288-364）

- `is_subclass(ancestor, derived)`（:288-325）：`ancestor` 为 `Applied` 取 base；若 base 是 `Trait` 转交 `satisfies_bound`；否则从 `derived` 沿 `parent_type` 上溯，`Applied` 时取 base，`HashSet` 环检测；每步用 `substitute_type_params(parent, &instance_substitution(self, &current))` 代入实参——这是泛型继承 `class B<T>: A<T>` 实例 `B<i32>` 正确上溯为 `A<i32>` 的关键。
- `is_assignable` / `assign_mismatch_reason`（:331-364）：前者闭包注入 `is_subclass` 与指针宽度调 `ty::assignable`；后者在不可赋值时给出可说清的精确原因（数值范围、浮点非有限等），其余返回 `None` 由调用方用通用文案。
- `type_summaries` / `definition_index`：投影 outline 与定义表。

- `type_summaries` / `definition_index`：投影 outline 与定义表。

---

## 第 4 章 Phase 0：模块加载、路径解析与 cfg

### 4.1 `module/mod.rs`：核心数据结构

- `ModuleId = usize`（:22）：模块在 `sema.modules` 中的下标，**入口恒为 0**。
- `ParsedSource { source, tokens, program, diagnostics, syntax_failed }`（:26-32）：LSP 未保存缓冲/语法快照的载体，供 `OverlayLoader` 复用——**避免同一份源码 lex+parse 两次**。
- `FileLoader trait`（:35-48）：
  - `is_cancelled() -> bool { false }`：协作式取消钩子；
  - `parsed_source(&mut self, path) -> Option<ParsedSource> { None }`：可选的语法快照复用；
  - `read_to_string(&mut self, path) -> io::Result<String>`；
  - `exists(&mut self, path) -> bool`。
- `FsLoader`（:51-61）：基于 `std::fs` 的默认实现。
- `Module`（:64-93）字段：
  - `key`：经 `module_key` 规范化的路径，用于去重 / overlay / crate 根查找 / 相对导入基准；
  - `path`：调用方原始路径，用于诊断回显（CLI 输出里看到的是命令行写法，如 `test.nov`）；
  - `source` / `tokens`（未过滤注释的完整流）/ `program`；
  - `global_scope`：模块根作用域（`scopes.new_scope(None, ScopeKind::Module)`）；
  - `diagnostics` / `syntax_diagnostic_count` / `syntax_failed` / `load_failed` / `crate_root`。
- `Module::error` / `error_with_code` / `warning`（:106-132）经 `decorated()`（:138-150）统一走 `novella_core::decor::decorate_one` 补行列与文件路径。

**双路径设计**：`key` 与 `path` 分离是贯穿 sema 的约定——比较/去重一律用 `key`，展示一律用 `path`。

### 4.2 `module/loader.rs`：worklist 加载（`load_modules`，:20-173）

```rust
load_modules(
    loader: &mut dyn FileLoader,
    entry: &Path,
    scopes: &mut ScopeTree,
    options: &AnalysisOptions,
) -> (Vec<Module>, Vec<Diagnostic>, HashMap<PathBuf, ModuleId>)
```

算法：

1. `worklist: Vec<(PathBuf, PathBuf)>`（元素 = `(调用方路径, module_key 规范化键)`），初始 `vec![(entry, module_key(entry))]`（:31）。

2. 循环取队首（:33-168）：
   - `is_cancelled()` → break；
   - `by_path` 已含 `key` → continue。**循环导入安全**：路径→id 在入队时（处理该模块前）就登记，第二次遇到同 key 直接复用 id，不会无限入队；
   - `id = modules.len()` 后立即 `by_path.insert(key, id)`，故入口恒 id 0。

3. **读文件**（:44-69）：`read_to_string` 失败 → 一条 global 诊断"无法读取模块文件 '{}': {e}" + 占位模块（`syntax_failed=true, load_failed=true`，空 program）保证 `modules` 下标与其他表一致。具名导入仍保留目标模块身份；后续具名/命名空间的类型、值及构造访问传播 `Error`，抑制读取失败引起的符号不存在与可见性级联诊断。可读模块中的真实缺失符号与其他独立错误仍正常报告。

4. **词法/语法**（:74-100）：
   - 先查 `loader.parsed_source(&path).filter(|s| s.source == source)`，命中则复用快照（LSP 场景，源码未变时不再 lex/parse）；
   - 否则 `novella_lexer::lexer(&source)` 得 token 流 + 词法错误（`errors.to_diagnostic()`），`Parser::new(ParserInput { file, tokens }).parse()` 得 `ParseResult { program, diagnostics, aborted }`；
   - `failed = !diagnostics.is_empty()`。

5. **诊断装饰**（:101-110）：所有诊断的 `location.file` 统一为本次 `file`（原始路径），经 `decor` 补行列。

6. **cfg 裁剪**（:111-114）：`syntax_diagnostic_count = diagnostics.len()`（此时含词法/语法），随后 `cfg::configure(&mut program, &options.cfg)`——**在声明收集与导入扫描之前**。

7. **建模块**（:115-127）：`Module { key, path, source, tokens, program, global_scope: scopes.new_scope(None, ScopeKind::Module), ... }`。

8. **cfg 错误**（:129-131）：`cfg_errors` 逐条 `module.error`。

9. **扫导入扩展 worklist**（:132-166）：遍历 `program.items` 的 `Item::Import`：
   - `import.prefix == "@"` 时懒加载 `find_crate_root(loader, &module.key)`（只在真有 `@:` 导入时才找 `nov.toml`）；
   - `importer_dir = module.key.parent()`；
   - `resolve_import(import, &importer_dir, crate_root, std_root)`：
     - `ResolvedImport::File(target)` → `worklist.push((target.clone(), module_key(&target)))`；
     - `ExceptionBuiltin` → 跳过（内建根，不加载文件）；
     - `Err(msg)` → `module.error(import.span, msg)`。

### 4.3 `module/resolve_path.rs`：导入路径解析

- `find_crate_root(loader, file)`（:14-22）：从 `file.parent()` 逐级向上找 `nov.toml`，返回其所在目录。
- `ResolvedImport { File(PathBuf), ExceptionBuiltin }`（:25-30）、`NsResolution { Module(ModuleId), ExceptionBuiltin }`（:33-36）。
- `resolve_import_path(sema, module, import)`（:41-69）：以 `importer.key.parent()` 为基准 dir，调 `resolve_import`；若返回 `File`，再以 `module_key` 查 `sema.module_by_path` 得 `ModuleId`。
- `resolve_import(import, importer_dir, crate_root, std_root)`（:74-120）规则：
  - path 含 `:` → `"路径段分隔符只支持 `/`（`:` 仅作别名分隔符，最多一个）"`——`:` 是别名分隔符，不是路径分隔符；
  - `split('/')` 遇空段 → `"非法模块路径 \"{}\"（含空段）"`；
  - 按 `prefix` 定 base：
    - `None` → `importer_dir`（相对当前模块）；
    - `"@"` → `crate_root`（无 → `"未找到 nov.toml（crate 根），无法解析 `@:` 导入"`）；
    - `"super"` → `importer_dir.parent()`（无 → `"super:` 导入已超出文件系统根"`）；
    - `"std"` 且 `path == "exception"` → `ExceptionBuiltin`；
    - `"std"` 含 `.` / `..` → `"std 模块路径不允许 . 或 .. 路径段"`，否则 base = `std_root`（无 → `"标准库模块 `{}` 缺少声明：请配置 std_root，不能跳过类型检查"`）；
    - 其他 → `"未知路径别名 `{}`"`；
  - 逐段 `push` 后 `set_extension("nov")`。

### 4.4 `module/cfg.rs`：条件编译（185 行全文）

- `CfgOptions { target_os, target_arch, target_family: String, features: BTreeSet<String> }`，`Default` 取 `std::env::consts::OS / ARCH / FAMILY`（:14-30）。

- `configure(program: &mut Program, options) -> Vec<(Span, String)>`（:32-66）：
  - `program.items.retain_mut`；
  - `DirectiveBlock { name == "cfg" }` 单独 `evaluate`，enabled 且有 block → 降级成 `Item::Statement(Block)`；
  - 其余 Function/Class/Struct/Trait 用 `enabled(attributes)`：false 则删整个 item，true 则对 members 同样 `retain_mut`。

- `enabled`（:68-83）：所有 cfg 都 `evaluate`（`result &=`，**不短路**，防止后面的 cfg 错误被吞），并移除 cfg 编译器指令。

- `evaluate`（:85-105）：解析 `Predicate` + 检查无多余 token（多余 → `"cfg 条件后有多余 token"`），错误收集进 `Vec<(Span,String)>` 并保守返回 false。

- `Predicate`（:107-184）递归下降（parse 用 stacker）：
  - 组合 `all` / `any` / `not`（not 恰 1 条件，空列表允许 → `"cfg not 必须恰有一个条件"`）；
  - 键值 `target_os` / `target_arch` / `target_family` / `feature`（值须 StringLiteral → `"cfg 条件值必须是普通字符串"`，用 `decode_string_literal` 解码）；
  - 裸 `true` / `false` / `unix` / `windows`；
  - 未知 → `"未知 cfg 条件 `{}`"` / `"未知 cfg 条件键 `{}`"` / `"未知 cfg 组合 `{}`"`。

**关键时机**：cfg 在词法/语法之后、声明收集与 import 扫描之前应用——语法错误照常报，但关闭的 item 不进符号表、其中的 import 不会被加载。编辑器侧的原始 AST 仍完整保留在 `ParsedSource.program`（未经 cfg 裁剪）。

**关键时机**：cfg 在词法/语法之后、声明收集与 import 扫描之前应用——语法错误照常报，但关闭的 item 不进符号表、其中的 import 不会被加载。编辑器侧的原始 AST 仍完整保留在 `ParsedSource.program`（未经 cfg 裁剪）。

---

## 第 5 章 符号系统与名称解析

### 5.1 `symbol.rs`：作用域 arena

- `ScopeId = usize`（:16）：`ScopeTree.scopes: Vec<Scope>` 的 arena 下标。
- `ScopeKind { Module, Block, Function }`（:19-27）：仅调试用途（`#[allow(dead_code)]`），不参与查找逻辑。
- `Scope { parent: Option<ScopeId>, kind, symbols: HashMap<String, Symbol> }`（:86-93）、`ScopeTree { scopes: Vec<Scope> }`（:96-99）。**全分析共用一棵 arena**（`Sema.scopes`），模块间经 `NsTarget::Module` 互查。

- 操作：
  - `new_scope(parent, kind)`（:103-110）→ push 返回下标；模块根 scope 在 loader 建（`parent=None`），函数/块/lambda 子 scope 到 Phase 2d 由 `typecheck/*` 建；
  - `define(name, symbol)`（:113-120）只查当前 scope，重名返回 `Box<已有 Symbol>` 供诊断；
  - `lookup_local`（:123-125）只查当前（`Module::global_scope_symbol` 是薄封装）；
  - `lookup`（:128-139）沿 parent 逐级向上；
  - `lookup_mut`（:142-154）先不可变定位再一次 `get_mut`——唯一用户是 Phase 2b 的函数签名回填。

- `NsTarget { Module(ModuleId), ExceptionBuiltin }`（:29-36）。

- `SymbolKind`（:38-58）：
  - `Function { sig }`（`is_extern` 区分）；
  - `Type`（实体在 `Sema.type_defs`，符号只存引用键）；
  - `Variable { is_const, ty: Option<Ty> }`（检查后回填）；
  - `Param { ty }`；
  - `NamespaceImport { target }`；
  - `NamedImport { target: Option<ModuleId>, imported }`（`target: None` 仅内建 Exception 根）。

- `Symbol`（:60-83）：`name` / `kind` / `visibility: Option<Scope>`（None = 模块私有）/ `span`（整条声明）/ `name_span`（标识符，跳转/高亮/重命名用）/ `module`。`is_importable()`（:80-82）= `visibility == Some(Public)`，**严格只认显式 pub**。

**`span` vs `name_span`**：对外定位一律取 `name_span`（`Definition::from_symbol`、`type_summaries`）。但 AST 中只有 `FunctionItem` / `FieldMember` / `MethodMember` 有真实 `name_span`，`TypeAlias/Struct/Class/Trait` 没有——`collect.rs:146` 的 `define_type` 用整段声明 span 顶替。`DeclarationIndex::build` 会从 `bindings.definitions` 中按 `(kind==Type && module && name && 落在 item.span 内)` 找回更精确值（declarations.rs:36-83），补救这处不对称。

**可见性**：`Public / Private / Package`（token 侧 `Pub / Pri / Pkg`，转换只在 `Scope::from_token`）；字段/方法另有 `is_public_member`（registration.rs:283-285），也只认 Public——**pkg 与 pri 归同一类**（语言无包边界）。

### 5.2 Phase 1 `collect_module`（resolve/collect.rs:9-130）

流程：`syntax_failed` 跳过 → 取 global_scope + `items.clone()` → 逐 item：

- `Import`：不收集（Phase 2a 处理）。
- `TypeAlias`：建 `AliasDef { type_params: map(param_key), target: alias.alias }` 插入 `type_defs`；`define_type(..., None, span)`——别名不支持 pub，不可跨模块导入（:18-31）。
- `Trait`：`TraitDef { type_params, type_param_count, methods 空, name }`，`define_type` 用 trait.scope；方法签名留 Phase 2b（:32-54）。
- `Class`：`ClassDef { parent_type: None, trait_types: vec![], type_params, fields 空, methods 空, type_param_count, parent_resolved: None, name, span, trait_impls 空, ... }`；D04：括号列表先记 `trait_impls` 候选，Phase 2c 按符号表分类为父类/trait 实现（:55-78）。
- `Struct`：`StructDef { type_params, fields 空, methods 空, type_param_count }`（:79-100）。
- `Function`：**只放占位签名** `FunctionSig { module, name: None, params 空, ret: Unknown, type_param_count: 0, ..., is_async, is_extern, is_method: false }` + `Symbol { name, kind: Function, sig, visibility: function.scope, span, name_span }`（:101-124）。
- `Item::Statement`：收顶层变量；`CompilerDirective` / `DirectiveBlock`：无符号产出。

**泛型参数身份**：`param_key(module, p) = format!("{}@m{}:{}", p.name, module, p.span.start)`（ty/generics.rs:4-6），源码位置参与身份，防不同声明的同名 `T` 串用。bound 在 Phase 2b `register_bounds` 注册。

**顶层变量收集**（:169-191）：只认 `Statement::Variable` / `VariableList`；`is_const = matches!(decl.r#type, VariableType::Const)`；`collect_pattern_names`（:194-203）递归展开元组解构 `BindingPattern::Ident/Tuple`；每名 `Symbol { Variable { is_const, ty: None }, visibility: decl.scope, span = name_span = 标识符 span }`。

**重定义**（:153-166）：`scopes.define` 返回 Err(existing) → `sema.error(module, 新 span, "重复定义 `{name}`（首次定义见本模块 {} 字节偏移处）", existing.span.start)`；新符号不插入，保留首次定义。

### 5.3 Phase 2a `bind_imports`（resolve/imports.rs:6-144）

流程：

1. `syntax_failed` 跳过；收集全部 `ImportItem`。
2. `resolve_import_path`（Err → continue，路径错误 loader 已报）。
3. `NsResolution::Module(target)`：若 `target.syntax_failed && !load_failed`，在本模块 `import.span` 报 warning"所导入的模块存在语法错误，已跳过其语义检查"；`ExceptionBuiltin` 直达。

**命名空间导入**（:44-57）：`prefix == "std" && path == "exception"` → `exception_imports.insert((module, local_alias))`；`Symbol { NamespaceImport { target }, visibility: None, span = name_span = import.span }`；无存在性/可见性检查。

**具名导入**（:58-128）：

- `ExceptionBuiltin` 且 `imported != "Exception"` → error"std:exception 中尚未声明符号 `{}`，不能跳过类型检查" + continue；
- `local = specifier.local.unwrap_or(imported)`；
- Exception 具名同样登记 `exception_imports((module, local))`；
- 仅 `NsTarget::Module` 时用 `global_scope_symbol` 查目标全局符号：
  - None → "模块 '…' 中不存在符号"；
  - `!is_importable()` → "符号 `…` 不是 `pub`，无法导入"；均 error + continue；
- `target_module = Some / None` 决定 `NamedImport.target`。

**冲突** `define_import`（:134-144）：error"导入绑定名 `{name}` 与本模块符号冲突"，不插入。

**循环导入**：loader 的 worklist + by_path 先入队即登记（loader.rs:31-41），导入延后绑定；Phase 1 全模块收集完才进 Phase 2a，环中符号已就绪。

### 5.4 `resolve/types.rs`：TypeExpr → Ty

- `resolve_type`（:11-24）：stacker 包裹 `resolve_type_inner`；**唯一写入 `annotation_types[(module, ty.span())]` 的出口**。

- `resolve_type_inner`（:27-78）：
  - `Predefined` → `predefined_from_enum`（int→I32、float→F32）；
  - `Reference` → `resolve_type_reference`；
  - `Array` / `Pointer` / `Tuple` 递归；
  - `Function` → `resolve_function_type`（产出 `Rc<FunctionSig>`）；
  - `Union` len < 2 报 error 但仍构造 `Ty::Union`。

- `resolve_type_reference`（:82-196）：
  - 先用 tokens 筛 span 内 `Identifier` 取第 `nth(qualifier.len())` 个精修类型名 span（:92-103）；
  - 实参先独立解析；
  - 无 qualifier 优先级：`Future` 特例（1 实参 → `Ty::Future`）→ `type_env` 替换 → `predefined_from_name` → `scopes.lookup` + `record_reference` + `type_from_symbol`；
  - 限定名：>1 段报"暂不支持多级限定名"；首段须为 `NamespaceImport`；
  - `ExceptionBuiltin` 仅认 `Exception` 且 `exception_imports` 登记过别名 → `exception_type() = Ty::Class(usize::MAX, "Exception")`（不占用户模块号）；
  - `Module` 分支取目标全局符号、`record_reference`、非 pub 报错、`type_from_symbol`。**`record_reference` 在 pub 检查之前调用**。

- `type_from_symbol`（:199-241）：`NamedImport { target: Some }` → 取 `type_defs[(target, imported)]`；内建 Exception 具名导入（`target: None, imported == "Exception"`，`exception_imports` 含 `(symbol.module, symbol.name)`）→ `exception_type()`；其余须 `SymbolKind::Type`。

- `type_from_def`（:243-282）：arity 检查（expected = `type_param_count`，不符 → error + `Ty::Error`）；Struct/Class/Trait → 名义 Ty；Alias → `expand_alias`；有实参 → `Ty::Applied` 并登记 `generic_uses[(use_module, span)]`（供最终 bound 校验）。

- `expand_alias`（:284-321）：`resolving_aliases: HashSet` 循环检测（进入 insert，成功路径 remove；错误提前 return 未 remove，属期望的错误抑制）；env 用 `param.split('@').next()` 为 key → `Ty::TypeParam(完整 key)`，subst 完整 key → 调用方实参；**在别名定义模块作用域 `def_scope = modules[key.0].global_scope` 解析 target**，再 `substitute_type_params` 代入实参；`generic_uses` 登记别名 Applied。

- `resolve_function_type`（:323-369）：`type_env` 叠加自身 `type_parameters` 恒等映射 → `register_bounds` → params（None 标注 → `Ty::Unknown`）→ ret → `FunctionSig { name: None, ... }`（函数名 Phase 2b 回填）。

- `register_bounds`（:373-394）：bound 解析后剥 Applied 取 base，非 Trait 且非 opaque → `type_error("泛型约束必须是 trait 类型")`，写 `type_param_bounds[param_key] = ty`。

- `satisfies_bound`（:396-441）：opaque → true；`TypeParam` 查 bounds；否则沿 Class 继承链代入检查 `trait_types`，`HashSet` 防环。

### 5.5 `declarations.rs`：声明索引

- `DeclarationId { module: ModuleId, span: Span }`（:11-15）：本次分析内有效，与定义表同身份；
- `DeclarationIndex { named: HashMap<(ModuleId, String), DeclarationId>, locations: HashMap<DeclarationId, Location> }`，`Location { item: usize, member: Option<usize> }` 私有；
- `build`（:30-103）：遍历 `module.items`，只处理 Function/Struct/Class/Trait；类型 span 先从 `bindings.definitions` 找回更精确值，退回 `value.span`；成员以 `"name::member"` 为键 `insert(member: Some(index))`；
- 查询：`named`（:116-118）、`parameters`（:120-135，由 Location 反查 AST 取 Param slice）、`members`（:137-147，保源码顺序）；
- 在 `analysis.rs:215` 最后 build，依赖已组装的 `modules` + `bindings`。

### 5.6 `std:exception` 特例贯穿三处

`NsTarget::ExceptionBuiltin`（符号种类）、`exception_imports` 集合（导入登记）、`exception_type() = Ty::Class(ModuleId::MAX, "Exception")`（类型表示）。它不加载 `.nov`、不进 `type_defs`、不进定义表；值位置明确拒绝（names.rs:54-61）；类型位置须 `exception_imports` 登记过别名——实现"不跳过类型检查、不伪造 std 成员"的严格策略。

### 5.6 `std:exception` 特例贯穿三处

`NsTarget::ExceptionBuiltin`（符号种类）、`exception_imports` 集合（导入登记）、`exception_type() = Ty::Class(ModuleId::MAX, "Exception")`（类型表示）。它不加载 `.nov`、不进 `type_defs`、不进定义表；值位置明确拒绝（names.rs:54-61）；类型位置须 `exception_imports` 登记过别名——实现"不跳过类型检查、不伪造 std 成员"的严格策略。

---

## 第 6 章 类型系统 `ty/`

### 6.0 模块组织与设计基调

`ty/mod.rs` 是门面，声明三个私有子模块并重导出：

```rust
mod generics; mod literals; mod relations;
pub(crate) use generics::*;
pub(crate) use literals::target_int_kind;   // 与下一行 glob 重复，属无害冗余
pub(crate) use literals::*;
pub(crate) use relations::*;
```

整个 `ty` 模块对 crate 外不可见（无 `pub mod`）。对外公开面是 `api.rs` 的 `TypeView` / `FunctionView`：内部 `Ty` 因持有 `Rc<FunctionSig>` 与内部定义类型无法直接导出，故 `Ty::view()` / `FunctionSig::view()` 负责投影（api.rs:155 注释明确此设计）。

**设计基调**（mod.rs 顶部 doc）：`int ≡ i32`、`float ≡ f32` 解析时即归一化；数值字面量先记为未定型 `IntLiteral` / `FloatLiteral`；**不做隐式数值转换**；隐式转换仅三类——`null → 任意指针/class`、`void* ↔ 任意 T*`、子类 → 父类。

### 6.1 类型枚举 `Ty`（mod.rs:176-222）

```rust
#[derive(Debug, Clone)]
pub(crate) enum Ty {
    Unknown, Void, Null, Boolean, String,
    Int(IntKind), Float(FloatKind),
    IntLiteral(bool, u128), FloatLiteral(f64),
    Pointer(Box<Ty>), Array(Box<Ty>), Tuple(Vec<Ty>),
    Function(Rc<FunctionSig>), Union(Vec<Ty>),
    Applied(Box<Ty>, Vec<Ty>), Future(Box<Ty>),
    Object(std::collections::BTreeMap<String, Ty>),
    Struct(ModuleId, String), Class(ModuleId, String), Trait(ModuleId, String),
    TypeParam(String), Error,
}
```

逐变体语义：

- **基础**：`Unknown`（前置诊断已报，抑制级联）、`Void`、`Null`（null 字面量专用，可隐式赋给任意指针/class）、`Boolean`、`String`（已移出 `PredefinedType`，按名归类）、`Int(IntKind)`、`Float(FloatKind)`。
- `IntKind`（:28-84）：`U8/U16/U32/U64/I8/I16/I32/I64/Usize/Isize`；`name()` 返回静态串；`from_name()` 把 `"i32"|"int"` 都归一到 `I32`；`is_signed()` 带 `#[allow(dead_code)]`（供 codegen/移位诊断后续使用）。
- `FloatKind`（:87-115）：`F8/F16/F32/F64`；`"f32"|"float"` 归一到 `F32`。
- **字面量**：`IntLiteral(bool, u128)` 载荷是 `(是否取负, 绝对值)`——`42→(false,42)`、`-128→(true,128)`；数值只参与目标位宽范围判定，公开投影恒为 `TypeView::IntLiteral`。`FloatLiteral(f64)` 按 `f64` 解析，只用于有限性/范围判定。
- **复合**：`Pointer` / `Array` / `Tuple` / `Union`（成员无序语义，由 `ty_eq` 的 `any` 判定体现）。
- **函数**：`Function(Rc<FunctionSig>)`——唯一使用 `Rc` 的位置。
- **泛型/异步/对象**：`Applied(Box<Ty>, Vec<Ty>)`（具名泛型实例保留实参，**不与未实例化定义混同**）、`Future`（async 返回包装）、`Object(BTreeMap<String, Ty>)`（按名排序字段集）。
- **具名类型**：`Struct/Class/Trait(ModuleId, String)`——按"模块+名"名义判等；`Trait` 仅 `is`/`as`/约束位置有意义。
- **占位**：`TypeParam(String)`（浅泛型不透明，字符串是带身份 `param_key`）、`Error`（产生处已报明确诊断，向下传播不再级联）。

**为何只有函数用 `Rc`**：`FunctionSig` 含 `Vec<ParamSig>` / `Ty` 递归结构且体积大，函数类型在表达式表/声明表/签名替换中大量克隆；`Rc` 让克隆变成引用计数递增，避免深拷贝。`ty_eq` 对 `Function` 比较**签名内容**（`sig_eq`）而非 `Rc` 指针身份，故共享不破坏值语义。其余递归用 `Box`（固定尺寸、无共享需求）。

**辅助谓词**（:262-284）：

- `is_numeric()` = `Int | Float | IntLiteral | FloatLiteral`；
- `is_integer()` = `Int | IntLiteral`；
- `is_nullable()` = `Pointer | Class | Null` **或** `Applied(base, _)` 且 base 为 Class（泛型类实例也可判空）；
- `is_opaque()` = `Unknown | Error`——`Error` 比 `Unknown` 高一档（来源处已带明确诊断），检查时一律放行以抑制级联。

**`Ty::view()`**（:228-260）：投影到 `TypeView`——`Unknown/Error` 原样保留（下游拒绝生成代码）；数值取 `kind.name()`；字面量擦除载荷；`Function(sig)` 投影为 `TypeView::Function(Box<FunctionView>)`，**携带完整参数/返回结构，不因 `name: None` 退化**。

### 6.2 类型注册表 `TypeDef`（mod.rs:286-399）

注册表值 `TypeDef`，键 `(ModuleId, String)`（存于 `Sema.type_defs`）：

```rust
pub(crate) enum TypeDef { Struct(StructDef), Class(ClassDef), Trait(TraitDef), Alias(AliasDef) }
```

- `FieldDef`（:289-304）：`name_span`；`name`；`ty`；`has_default`（有默认值则 StructInit 可省略）；`is_weak`（weak 修饰，仅 class 字段有意义）；`is_static`（不进实例布局而是全局）；`is_public`（D02 缺省 pri）；`module`。
- `MethodSig`（:307-320）：`name_span`（只覆盖成员名，供跳转/重命名）；`sig: FunctionSig`；`is_static`；`is_public`；`name`（**不含 `类型::` 前缀**，成员查找/覆写校验用）；`is_abstract`（只有声明无函数体）；`span`（整条声明，诊断/覆写定位）。注释明确「`this` 不占参数位」。
- `StructDef`（:323-329）：`type_params`、`fields`、`methods: HashMap<String, MethodSig>`、`type_param_count`。
- `ClassDef`（:332-351）：继承/泛型/成员字段同上，另加 `parent_type: Option<Ty>`、`trait_types: Vec<Ty>`、`parent_resolved: Option<(ModuleId,String)>`（跨模块收集完成后解析）、`name`、`span`、`trait_impls: Vec<TypeExpr>`、`trait_impls_resolved: Vec<(ModuleId,String)>`。
- `TraitDef`（:354-362）：`type_params`、`type_param_count`、`methods`（要求实现的方法签名，D04 供一致性检查）、`name`。
- `AliasDef`（:365-370）：`type_params`、`target: TypeExpr`（按需展开，循环在使用点检测）。

**统一访问器**：`type_params() -> &[String]`（四变体各取）；`type_param_count() -> usize`（前三者取字段，**Alias 特殊地返回 `d.type_params.len()`**）。注意 `type_params` 里存的是 `param_key` 生成的**带身份字符串**，非裸名。

### 6.3 函数签名 `FunctionSig` / `ParamSig`（mod.rs:117-174）

```rust
pub(crate) struct FunctionSig {
    pub module: ModuleId,
    pub name: Option<String>,
    pub params: Vec<ParamSig>,
    pub ret: Ty,
    pub type_param_count: usize,      // 调用点按 arity 校验显式实参
    pub type_params: Vec<String>,     // 按声明顺序建立替换表
    pub is_async: bool, pub is_extern: bool,
    pub is_method: bool,              // 隐式接收 this
}
pub(crate) struct ParamSig {
    pub(crate) name: String,
    pub ty: Ty,                       // 未标注为 Ty::Unknown（调用点跳过）
    pub has_default: bool,
}
```

- **名义键 `(module, name)`**：顶层函数用原名；方法在 `build_method_sig` 里用 `format!("{name}::{}", method.name)`（即 `类型::方法`），trait 方法带 `Trait::` 前缀，避免与顶层同名混淆；`build_function_sig` 在解析完签名后回填 `sig.module` / `sig.name = Some(...)`。
- **`name: None`**：lambda 与占位签名。占位签名见 `registration.rs:430-441` 的 None 防御分支（注释「parser 恒产出签名，None 仅见于异常恢复路径」）。**`name` 只用于合成 HIR 符号键，不决定类型投影**。
- **`self` 处理**：`build_method_sig` 把实例方法首个无注解 `self` 定型为 `owner.clone()`；有标注的 `self` 必须 `ty_eq` 于 `declared_owner`（泛型属主构造为 `Ty::Applied(owner, owner_args)`）；静态/非首位 `self` 报错；顶层函数声明 `self` 报"顶层函数不能声明 self 形参"。`FunctionSig::view()` 在 `is_method && param.name=="self" && index==0` 时跳过该参并置 `receiver = true`。
- **泛型参数**：`type_param_count = 方法 type_parameters.len()`，`type_params` 由 `param_key` 生成；方法自身泛型形参叠加进 `env`。特殊方法 `__init/__destroy/__enter/__exit` 强制返回 Void、`__str` 强制 String，且不能声明方法类型参数。

### 6.4 可赋值性 `assignable`（relations.rs:50-108）

```rust
pub(crate) fn assignable(
    target: &Ty, source: &Ty,
    is_subclass: &mut dyn FnMut(&Ty, &Ty) -> bool,
    width: crate::PointerWidth,
) -> bool
```

`is_subclass` 以**闭包注入**解耦继承链（`relations.rs:48-49` 注释：sema 需继承链，class 父子判定由调用方注入），`width` 决定 `usize/isize` 位宽。注入点在 `state.rs:361-364`。

完整判定顺序（短路，前一条命中即返回）：

1. **不透明放行**：`target.is_opaque() || source.is_opaque()` → true；
2. **结构相等**：`ty_eq(target, source)` → true；
3. **字面量归一化**（跨数值类型的唯一通道）：
   - `(Int(kind), IntLiteral(false, value))` → `int_literal_fits_non_negative(target_int_kind(*kind, width), *value)`；
   - `(Int(kind), IntLiteral(true, value))` → `int_negated_literal_fits(...)`；
   - `(Float(kind), FloatLiteral(value))` → `float_literal_fits(*kind, *value)`；
   - **已定型值跨数值种类无隐式转换**（`i32 ← i8` 为 false），字面量之间也互不赋值；
4. **null**：`(Pointer(_) | Class(..), Null)` → true；`Applied(base,_) ← Null` 且 base 为 Class → true；
5. **void\***：`(Pointer(t), Pointer(s))` 且 t/s 任一为 Void → true（双向）；
6. **子类→父类**：`(Class, Class)` / `(Trait, _)` / `(Applied, Applied|Class)` / `(Class, Applied)` → `is_subclass(...)`；
7. **联合目标**：`(Union(members), _)` → `members.iter().any(...)`（source 可赋给任一成员即可）；
8. **联合源**：`(_, Union(members))` → `members.iter().all(...)`（全部成员都能赋给目标才可赋，保守）；
9. **元组**：同长后逐元素 `all(assignable)`；
10. **数组**：逐元素 `assignable(inner_t, inner_s)`；
11. **对象**：target 每个 `(name, ty)` 都能在 source 找到同名且 `assignable(ty, source_ty)`（source 可有额外字段）；
12. 兜底 false。

`Sema::is_subclass`（state.rs:288-325）见 3.5 节；其沿 `parent_type` 每步用 `substitute_type_params(parent, &instance_substitution(...))` 代入实参，是泛型继承正确上溯的关键。

### 6.5 泛型系统（generics.rs + resolve/types.rs）

- **`param_key(module, &TypeParameter) -> String`**（generics.rs:4-6）：`format!("{}@m{}:{}", param.name, module, param.span.start)`——**源码声明身份（模块+span 起点）参与判等**。
- **`exception_type()`**（:9-11）：`Ty::Class(ModuleId::MAX, "Exception")`，内建异常根，不占用户模块号。
- **`instance_substitution(sema, ty)`**（:13-29）：仅当 `ty` 为 `Applied(base,args)` 时，用 `typecheck::this_ty_key(base)` 取 `(ModuleId,String)`，从 `type_defs` 取 `type_params()` 与 args zip 收集；非 Applied 或定义缺失返回空表。
- **`member_substitution`（:32-53）**：沿实际实例父链代入，返回"成员定义环境"替换表——循环取 `this_ty_key(current)`、`subst = instance_substitution(...)`，`key == owner` 或环检测命中则返回 subst，否则取 `ClassDef.parent_type` 以 `substitute_type_params(parent, &subst)` 上溯。
- **`substitute_type_params(ty, &subst) -> Ty`**（:60-118）：浅泛型替换——空表 clone；`TypeParam(name)` 查表命中则换否则原样（调用方据此判"推导不出"）；递归 `Applied`（仅替换 args，base 不动）/ `Future` / `Object` / `Array` / `Pointer` / `Tuple` / `Union` / `Function`（重建 `Rc<FunctionSig>`，逐参替换 `ParamSig.ty` 与 ret，保留 module/name/type_param_count/type_params/is_async/is_extern/is_method/has_default）。**关键限制**（:115 注释）：`Struct/Class/Trait` 变体本身不带泛型实参位（实参挂在 `Applied` 上），故这些位置无可代换形参。
- **`infer_type_param`（:124-222）**：从"形参类型 ← 实参类型"推绑定，`Result<(), String>`，冲突报"泛型实参推导冲突：形参 `{name}` 先推出 `{existing}`，后又推出 `{actual}`"。
  - `TypeParam(name)`：不透明实参或 `Null`/`IntLiteral`/`FloatLiteral` **不作为推导依据**（直接 Ok）——字面量未定型；已绑定须 `ty_eq`，否则冲突；
  - `Applied` / `Future` / `Array` / `Pointer` / `Tuple` / `Function`：仅形状匹配（base `ty_eq`、arity/长度相等）才逐对递归，否则宽松 Ok；
  - `Union`（:193-219）：把实参归一为 slice，跳过与某成员 `ty_eq` 的具体分支；对每个剩余实参遍历成员试推，只有相对 subst 有新绑定的才算候选；**仅当候选恰好 1 个才提交**，避免依赖成员顺序、避免歧义误推。

**约束登记与满足**（resolve/types.rs）：

- `register_bounds`（:380-393）：解析 bound，要求 base 为 Trait（否则"泛型约束必须是 trait 类型"），写 `type_param_bounds[param_key] = bound`；
- `satisfies_bound`（:396-441）：opaque → true；`TypeParam` 查 bounds 要 `ty_eq`；否则沿父链（仅 Class，Applied 取 base）带 `HashSet` 环检测上溯，每步用 `instance_substitution` 代入 `def.trait_types` 并 `ty_eq(bound, substituted)` 命中即 true。

**完整代入/约束流程**：

1. 解析期 `type_from_def`：按 `type_param_count()` 校验 arity（不符报"类型 `{}` 需要 {expected} 个泛型实参，实给 {}"并返回 `Ty::Error`）；具名定义有实参构造 `Ty::Applied` 并登记 `generic_uses`；别名走 `expand_alias`。
2. 调用期 `typecheck/call.rs:130-180`：`build_substitution`（:193-209）**显式实参优先**，否则逐形参 `infer_type_param`；对每个 `sig.type_params`：未出现在 subst 报"无法推导泛型参数 `{param}`"；有 bound 则代入后 `satisfies_bound`，失败报"泛型实参 `{actual}` 不满足约束 `{bound}`"；最后 `substitute_type_params(&Ty::Function(sig), &subst)` 得实例化签名并据此检查实参、算返回（async 再包 Future）。
3. 最终阶段 `analysis.rs:125-139`：遍历 `generic_uses`，对每个 `(param, actual)` 代入 bound 并 `satisfies_bound`，失败报"类型实参 `{actual}` 不满足 `{bound}` 约束"。

### 6.6 字面量解析（literals.rs）

- **`classify_number_literal(raw) -> Option<Ty>`**（:8-39）：滤 `_`；空串 None；按小写判 `0x/0o/0b` 前缀；非前缀且含 `.` 或 `e` 判浮点 `parse::<f64>().ok().map(Ty::FloatLiteral)`（`1e999 → inf` 仍保留，交给范围判定拒绝）；整数按 radix 校验每位 `is_digit`，`u128::from_str_radix(...).ok()?`——**超 u128 返回 None**（不静默截断）；成功返回 `Ty::IntLiteral(false, value)`。调用点 strings.rs:28-29 报"非法数字字面量 `{raw}`"。
- **`predefined_from_name`**（:45-59）：按名 `IntKind::from_name` / `FloatKind::from_name` 优先，再 `string/boolean/void/null`；None 表示非预定义名。
- **`predefined_from_enum(PredefinedType) -> Ty`**（:62-84）：穷尽各变体，Int→I32、Float→F32。
- **整数范围**（:86-134）：
  - `int_literal_fits_non_negative(kind, value)`：正字面量按有符号上限判定——`U8..U64/Usize` 用各自无符号 MAX（`U64|Usize → u64::MAX`），`I8/I16/I32` 用 signed MAX，`I64|Isize → i64::MAX`。`let x:i8=128` 非法、127 合法。
  - `int_negated_literal_fits(kind, value)`：仅有符号可表示负数，按 `|MIN|` 判定——`I8→128`、`I16→32768`、`I32→i32::MAX+1`、`I64|Isize→i64::MAX+1`；无符号全 false。i8 允许 -128。
  - `int_range_text(kind)`：`(min,max): (i128,i128)`，格式 `"{min}..={max}"`。
  - 负号由 `UnaryOp::Minus` 在 unary.rs:34-46 处理：`IntLiteral(negative,value) → IntLiteral(!negative, value)`——**保留未定型态**（提前 concrete 成 i32 会让 `let x:i8=-128` 误报，L04）。
- **浮点范围**（:136-167）：
  - `float_literal_fits(kind, value)`：先 `is_finite()`；F64 → true；F32 → `(value as f32).is_finite()`（判据是"舍入后仍有限"而非 `abs ≤ f32::MAX`——`3.4028235e38` 比 `f32::MAX` 略大但舍入后正好落在 f32::MAX，合法）；F16 → `value.abs() < 65520.0`（IEEE binary16 最近偶数舍入溢出阈值，最大有限 65504）；F8 → `value.abs() <= 464.0`（E4M3FN 最大有限 448、下一编码 NaN、中点 464）。
  - `float_range_text(kind)`：F64≈±1.7976931348623157e308、F32≈±3.4028235e38、F16 ±65504、F8 ±448（短格式避免默认 Display 展开上百位）。
- **`target_int_kind(kind, width)`**（:170-176）：仅范围判定时把 `Usize→U32`、`Isize→I32`（Bits32），其余原样；**不改变名义类型相等性**。

**位宽/边界已用脚本复核**：u8 255、u16 65535、u32 4294967295、u64 18446744073709551615；i8 -128..127（|MIN|=128）、i16 -32768..32767、i32 -2147483648..2147483647、i64 全范围；f16 溢出中点 65520、f8 NaN 中点 464——与代码常量一致。

**`concrete` 归一化**（bindings.rs:155-168）：`IntLiteral → Int(I32)`（注释称"取回符号"但代码 `let _ = (negative, value)` **丢弃载荷，一律落 i32**）、`FloatLiteral → Float(F32)`、递归 Tuple/Array、其余 clone。用于无标注变量/字段/数组元素/二元运算的默认定型。`let x:i8=-128` 靠赋值上下文的 `assignable` 正确通过；无标注 `let x=-128` 落 i32，符合默认定型设计。

### 6.7 类型相等与显示

**`ty_eq(a,b)`**（relations.rs:5-36）——结构化判等，`Function` 比较签名内容而非 Rc 指针：

- 同类原子 true：`Unknown/Unknown`、`Void`、`Null`、`Boolean`、`String`、**同为 `IntLiteral(..)` 即相等**（数值差异交给赋值点范围判定）、**同为 `FloatLiteral(_)` 即相等**；
- `Int(x)/Int(y)`、`Float/Float`：`x == y`；
- `Applied`：`ty_eq(base)` + arity 相等 + zip 全等；`Future`/`Pointer`/`Array` 递归；`Tuple` 同长逐元素；
- `Object`：长度相等 + 每个 `(n,t)` 在对方存在且 `ty_eq`；
- `Function` → `sig_eq`；`Union` → 同长 + 每成员在对方 `any(ty_eq)`（无序成员集语义）；
- `Struct/Class/Trait`：`m1==m2 && n1==n2`（**名义判等，跨变体不相等**）；`TypeParam`：字符串身份；`Error/Error → true`。

**`sig_eq(a,b)`**（relations.rs:39-46）：`params.len()` 相等 + 逐参 `ty_eq` + `ty_eq(ret)`；**明确忽略 `module`/`name`/`type_param_count`/`type_params`/`is_async`/`is_extern`/`is_method`/`has_default`**——只比参数与返回类型（注释"忽略 async/extern 标记"），是覆写/trait 实现校验的结构性口径。

**`Display for Ty`**（mod.rs:400-459），诊断文案直接用到：`Unknown→"<未知>"`、`Error→"<错误类型>"`、`Function→"<函数>"`（不展开签名）；基础类型输出种名；`IntLiteral→"{}{value}"`（前缀 - 或无）、`FloatLiteral→"{value:?}"`；`Pointer→"{t}*"`、`Array→"{t}[]"`、`Tuple→"(t1, t2)"`、`Union→"t1 | t2"`、`Future→"Future<{inner}>"`、`Object→"{n: t}"`（BTreeMap 有序）、`Applied→"{base}<a1, a2>"`；`Struct/Class/Trait`/`TypeParam` 只打印名字（模块身份不显示）。

### 6.8 诊断文案生成（state.rs:331-358）

`Sema::assign_mismatch_reason(target, source) -> Option<String>`：先 `is_assignable` 为 true 返回 None；否则对两类可说清的情形给精确原因：

- `Float ← FloatLiteral`：非有限 → "浮点字面量 `{value:?}` 不是有限值，超出 `{kind.name()}` 的可表示范围 {float_range_text}"；否则"超出 … 可表示范围 …"；
- `Int ← IntLiteral`：一律"整数字面量 `{}{value}` 超出 `{kind.name()}` 的取值范围 {int_range_text(target_int_kind(kind,width))}"。

通用兜底由调用方给（如 bindings.rs:25"初始化类型 `{value_ty}` 与标注 `{annotated}` 不兼容"、call.rs:271 实参不匹配、assignment.rs:47 赋值不匹配）。所有文案经 `Sema::type_error/decl_error` 落到 `Diagnostic`，由 CLI rustc 风格统一渲染；`ty/` 本身不产诊断，只提供判定与数值/文案工具。

> **值得注意（非缺陷断言）**：`sig_eq` 忽略 async/extern/module/name 使函数类型相等是纯结构口径；`ty_eq` 把 `Unknown/Unknown`、`Error/Error`、`IntLiteral/IntLiteral` 都视为相等，与"避免级联"目标略有张力；`substitute_type_params` 不递归进 `Struct/Class/Trait`（实参在 Applied 上）；`assignable` 的 `Union ← Union` 先走目标联合的 `any`、逐成员再走源联合的 `all`。

> **值得注意（非缺陷断言）**：`sig_eq` 忽略 async/extern/module/name 使函数类型相等是纯结构口径；`ty_eq` 把 `Unknown/Unknown`、`Error/Error`、`IntLiteral/IntLiteral` 都视为相等，与"避免级联"目标略有张力；`substitute_type_params` 不递归进 `Struct/Class/Trait`（实参在 Applied 上）；`assignable` 的 `Union ← Union` 先走目标联合的 `any`、逐成员再走源联合的 `all`。

---

## 第 7 章 `check/`：类型定义注册与声明检查

### 7.0 阶段总览与关键不变量

`check/` 承担 Phase 2b / 2c / 2d 的绝大部分工作：

```
Phase 1  collect_module*N
         → TypeDef 骨架（空 fields/methods）插入 Sema.type_defs: HashMap<(ModuleId,String),TypeDef>
Phase 2a bind_imports*N
Phase 2b build_type_defs*N（registration.rs）
         → 解析真实字段/方法/函数签名，回填骨架
Phase 2c resolve_class_parents（全局一次，inheritance.rs）
         + globals::collect + fields::check_fields + globals::check_all
Phase 2d check_module*N（decl.rs）
         → 函数体/方法体/顶层语句检查
analysis.rs:125-139 → generic_uses 校验 type_param_bounds
```

**关键不变量**：TypeDef 骨架在 Phase 1 建、Phase 2b 才填签名——保证跨模块符号在解析签名时都已可见（循环导入、前向引用正确）。

`check/mod.rs`：

- `CLASS_SPECIAL_METHODS = ["__init", "__destroy", "__str", "__enter", "__exit"]`（:18）；
- `canonical_method_name`（:21-26）：`strip_suffix("__")` 命中白名单则返回短名——`__init` 与 `__init__` 视为同一特殊方法，全链路经此归一化。

诊断码：`decl_error = NVL3003`（state.rs:246）、`type_error = NVL3002`（:236）。`Ty::Unknown/Error` 的 `is_opaque()` 是级联抑制核心。

### 7.1 Phase 2b `build_type_defs`（check/type_defs/registration.rs:5-181）

`syntax_failed` 模块早退（:6-8）。

**`identity_env(module, params)`**（:184-197）→ 把每个类型参数映射为 `Ty::TypeParam(param_key)`，`param_key = format!("{}@m{}:{}", name, module, span.start)`（ty/generics.rs:4-6），声明位置参与身份。`register_bounds`（resolve/types.rs:373-394）要求 bound base 为 Trait 或 opaque，否则"泛型约束必须是 trait 类型"，写 `type_param_bounds[param_key]`。

分派处理四种 TypeDef：

- **TypeAlias**（:13-18）：`resolve_type(alias.alias)`；循环检测在 `expand_alias` 用 `resolving_aliases: HashSet`，报"类型别名 `{}` 循环定义"；注释"未使用的循环/非法类型不能躲过诊断"。
- **Trait**（:20-76）：字段 → "trait `{}` 不能声明字段 `{}`"；带 block 方法 → "trait `{}` 的方法 `{}` 暂不能带函数体（默认方法未实现）"；`check_special_method_name(true)`；`build_method_sig_trait`；回填 `def.methods`；编译器指令 → "成员编译器指令展开尚未实现"。
- **Class**（:77-129）：字段 `build_field`；方法 `check_member_collision(canonical)` + `check_special_method_name(true)` + `build_method_sig(owner = Ty::Class)`；`def.trait_impls = class.parent.clone()`（混合候选原样保存待 2c 分类）+ `def.span = class.span`；填 `def.fields` / `def.methods`。
- **Struct**（:130-177）：`check_special_method_name(false)`；heritage 非空 → "struct 不支持继承"。

**成员名规整**：

- `check_member_collision`（:200-210）："重复的成员 `{name}`"；
- `check_special_method_name`（:213-241）：class 下非白名单 `__` 前缀 → "未知特殊方法 `{}`（已登记：__init / __destroy / __str / __enter / __exit）"；struct 下 `__` 前缀一律 → "struct 不识别 `__` 前缀特殊方法（`{}`）"。

**`build_field`**（:245-276）：无标注无初值 → "字段 `{}` 需要类型标注或默认值" → Unknown；产 `FieldDef { name_span, name, ty, has_default, is_weak, is_static, is_public: is_public_member(scope), module }`；`is_public_member`（:283-285）仅 `Some(Public)` → true（D02：pkg ≈ pri）。

**`build_method_sig`**（:288-411）：

- `env` = 类型级 clone + 方法级 param_key；`check_duplicate_params`（:488-500）"重复的形参 `{}`"；
- 首个无注解 `self` 且非 static → `owner.clone()`，其余 None → Unknown；ret None → Void；
- `self` 非首位/static → "self 只能是实例方法的第一个形参"；显式标注 ≠ declared_owner（含 Applied 泛型实参）→ "self 的类型标注必须是方法所属类型"；
- 特殊方法：`__init/__destroy/__enter/__exit` → Void、`__str` → String；有方法泛型 → "特殊方法不能声明方法类型参数"；返回不符 → "特殊方法 `{}` 必须返回 `{expected}`"；static → "特殊方法不能是 static"；除 `__init` 外显式实参 ≠ 0 → "该特殊方法不能接受显式实参"；
- 产 `MethodSig { sig: FunctionSig { module, name: Some(format!("{name}::{}", method.name)), params, ret, type_param_count, type_params, is_async: false, is_extern: false, is_method: !is_static }, is_static, is_public, name, is_abstract: block.is_none(), span }`；trait 方法强制 `is_abstract`（:503-517）。

**`build_function_sig`**（:414-485）：`resolve_function_type`（产 `name: None` 签名）后回填 name/is_async/is_extern/module；`declaration_types.insert((module, function.span), Function(sig))`；extern E2–E5（:453-480）：

- E2 参数无标注 → "extern 函数参数 `{}` 必须显式标注类型"（返回标注检查因 AST 无法区分显式 void 暂不落地）；
- E3 有体 → "extern 函数不能有函数体"；
- E4 泛型 → "extern 函数暂不支持泛型参数"；
- E5 main → "`main` 不能声明为 extern"。

`lookup_mut(global_scope, name)` 回填真签名（重名时先定义者接收后定义者签名）。顶层重复由 Phase 1 define 报"重复定义 `{}`（首次定义见本模块 {} 字节偏移处）"。

### 7.2 Phase 2c `resolve_class_parents`（inheritance.rs:5-81，全局一次）

**继承/trait 分类**（:6-52）：遍历 `def.trait_impls` 用 `resolve_type`，`base = if Applied(b, _) { &**b } else { &ty }`：

- `Class` → 父类（第二个 → "class 只能有一个父类"，保留完整 Applied 含泛型实参）；
- `Trait` → 实现（`ty_eq` 重复 → "重复实现同一 trait"）；
- opaque → 静默放行；
- 其他 → "继承列表只允许 class 或 trait"。

写回两对字段：

- `parent_resolved = parent.map(this_ty_key)`（名义键 `(ModuleId,String)`，供快速沿链/环检测/lookup_member_chain/find_implementation/is_abstract_class）；
- `parent_type = parent`（完整 Ty，供 `is_subclass` / `member_substitution` / `satisfies_bound` 代入实参）；
- `trait_impls_resolved` / `trait_types` 同理。

**二者成对维护，否则泛型继承退化。**

**继承环检测**（:53-78）：每个 class key 独立 visited，重复 → "class `{}` 的继承链存在循环"，span = `Span{0,0}`（退化到文件头；虽 1.2 已写 `def.span = class.span`，属定位精度问题）；环上 N 类报 N 条。

**`check_overrides`（:247-334，D02）**：

- 字段遮蔽 → "字段 `{}` 不能遮蔽继承成员"；
- method `canonical == "__init"` 跳过；
- 父类同名 Field → "方法 `{}` 不能遮蔽继承字段"；
- static 性不一致 → "方法 `{}` 与父类 `{}::{}` 的静态 / 实例属性不一致，不能覆写"；
- `subst = member_substitution + 父类 type_params → 子类重命名`。

`check_override_sig`（:337-403）：

- static/async/type_params.len 不一致 → "覆写方法的 static、async 与泛型参数个数必须与父类一致"；
- 形参个数 → "方法 `{name}` 覆写父类同名方法时形参个数必须一致（子类 {} 个，父类 `{}::{name}` {} 个）"；
- 逐位 `ty_eq`（index 0 self 跳过）→ "方法 `{name}` 覆写父类同名方法时第 {index} 个形参类型须为 `{}`，实为 `{}`"；
- 返回协变失败 → "方法 `{name}` 覆写父类 `{}::{name}` 时返回类型 `{}` 不兼容父类的 `{}`"。**参数全等，不允许形似重载。**

**`resolve_trait_impls` / `check_trait_impl`（:83-165，D04）**：`find_implementation` 沿继承链找最近非抽象且 static 匹配的实现（canonical 归一化）；缺 → "class `{}` 声明实现 trait `{}`，但缺少方法 `{name}` 的实现"；签名不符 → "方法 `{name}` 的实现与 trait `{}` 声明的签名不一致"。`trait_signature_compatible`（:206-238）：type_params.len/is_async/params.len 相同、逐位 ty_eq（self 跳过）、返回协变。

**`is_abstract_class`（:410-455）**：沿 `parent_resolved` 对每层抽象方法调 `is_implemented`（从原始 key 找最近非抽象实现），有未实现 → true；环 visited → false。`is_implemented` 用 `def.methods.get(name)` 未走 canonical（与其他路径不一致，`__str` / `__str__` 判定可能不一致）。

### 7.3 字段与全局变量的按需定型

**fields.rs**：

- `FieldKey = (ModuleId, String, String) = (module, owner_name, field_name)`；
- `FieldTask { owner: Ty, field: FieldMember, env, state: State{Pending,Resolving,Done} }` 存 `Sema.field_tasks`；
- `check_fields`（:28-70）：跳过 syntax_failed，对每个有 initializer 的字段 `or_insert` 任务；
- `field_type`（:73-90）：仅当 `task.field.ty.is_none()` 时触发 `resolve_field`（显式标注断环）；
- `resolve_field`（:92-123）三态：Done 返回；Resolving 回边 → "字段 `{}.{}` 的类型推导存在循环依赖，请添加类型标注" + `set_field_type(Error)`；Pending → Resolving → `typecheck::check_field_initializer`（typecheck/mod.rs:234-294）→ Done。
- 有标注走 `check_expr_with_hint` + `is_assignable`（失败"字段 `{}` 的初始值类型 `{}` 与标注 `{}` 不兼容"）；无标注走 `check_expr` + `concrete` 回填，越界"字段初始值 `{}` 超出默认类型 `{}` 的范围"。字段初值引用另一字段经 `field_type` 按需 DFS。

**globals.rs**：

- `GlobalTask { declaration: VariableStmt, state: u8 }`（0/1/2）；`global_tasks: HashMap<(ModuleId,Span), GlobalTask>`、`global_tasks_by_name: HashMap<(ModuleId,String), Span>`；
- `collect`（:14-55）：支持 Variable/VariableList，取名 insert by_name，有显式注解的先 `register_global_annotation`（typecheck/mod.rs:296-317：resolve_type + bind_pattern_typed，`fill_existing = true`），注释"有注解的环不属于类型推导环"；
- `check_all`（:57-63）：按 `(module, span.start)` 排序后 resolve，诊断顺序稳定；
- `resolve_name`（:65-73）：由 `expr/names.rs::value_type_of_symbol`（:29-41）遇 `ty = None` 全局符号时按需触发；
- `resolve`（:75-107）：state 1 → "顶层变量的类型推导存在循环依赖" + 所有符号 `ty = Some(Error)` 固化；state 0 → `check_toplevel_statement` → state 2。`check_module` 的 Statement 分支跳过 Variable/VariableList 避免重复。

### 7.4 Phase 2d `check_module`（check/decl.rs:15-75）

`syntax_failed` 早退。

**main 入口**（仅 `module == 0 && !module_only`，:22-24）：`check_main_entry`（:98-136）：

- find 首个 main，缺 → "缺少程序入口：顶层 `func main`"（`Span{0,0}`）；
- `is_extern` → return（E5 已报）；
- 参数 → "`main` 不能接受参数"；
- 泛型 → "`main` 不能带泛型参数"；
- 返回 → "`main` 的返回类型必须是 `void` 或 `int`，实为 `{ret}`"；
- async → "`main` 不能声明为 async"。

`is_main_return_type`（:142-149）仅 Void / Int(I32)，Unknown 放行，其他宽度拒绝（退出码语义只定义 32 位有符号整数）。

**编译器指令**：item attributes 逐条"编译器指令展开尚未实现"；DirectiveBlock → "指令块展开尚未实现，不能完成语义检查"；整块跳过检查靠诊断阻断下游。

**函数/类/struct body**：

- `Function`（:38-43）：`check_function_defaults`（typecheck/mod.rs:85-95，失败"默认值需要 `{}`，实为 `{}`"）+ 有体则 `check_function_body`（:116-160，Function scope，形参 zip(AST, sig) 容错，check_stmt + flow::check_statement）；
- `Class/Struct`（:44-55）：逐 member `check_member_body`（:78-95），Field 跳过（字段阶段已检查），Method → `check_method_defaults` + `check_method_body`（:162-211，this_ty 绑定 + 两侧泛型入 type_env + check_block + flow::check_block）；
- `Statement` 非变量 → `check_toplevel_statement(fill_existing = true)`。

### 7.5 横切错误恢复（check/ 内体现）

1. **语法失败模块整体跳过**：多阶段 `syntax_failed` 早退，`ModuleOutput.syntax_failed = true` 交下游拒绝生成代码；
2. **签名缺失容错**：None 防御签名，lookup 不到 `Function{sig}` 直接 return，形参 zip 而非索引硬取防 panic；
3. **is_opaque() 静默跳过**：继承分类 / satisfies_bound / 泛型推导 / 成员查找多处；
4. **环检测就地固化 Error**：字段 `State::Resolving`、全局 state 1；
5. **重复定义只报一次**：顶层 Phase 1、成员/形参独立、main extern E5 后 `check_main_entry` return；
6. **visited 保护**：lookup_member_chain / find_implementation / is_implemented / is_subclass / satisfies_bound；
7. **借用安全克隆模式**：先收集 keys / 克隆 def 再写回；
8. **每阶段查 is_cancelled**，取消加 global 诊断。

> **可改进点（非缺陷结论）**：`resolve_trait_impls` 文档注释"首项是父类"与按符号类型分类的实现不一致（注释过时）；继承环诊断落 `Span{0,0}` 而 `ClassDef.span` 已写入；`is_implemented` 未走 canonical；`build_function_sig` 重名时先定义者接收后定义者签名。

> **可改进点（非缺陷结论）**：`resolve_trait_impls` 文档注释"首项是父类"与按符号类型分类的实现不一致（注释过时）；继承环诊断落 `Span{0,0}` 而 `ClassDef.span` 已写入；`is_implemented` 未走 canonical；`build_function_sig` 重名时先定义者接收后定义者签名。

---

## 第 8 章 `typecheck/` 表达式与 unsafe 规则

> **术语纠正**：代码中**没有函数级 unsafe 修饰符**——只有 `Statement::Unsafe`，`FunctionSig` 只有 `is_extern`；AST 中也**没有 `Expression::Super`**（super 表达式未实现）。

### 8.1 三个检查出口与类型登记

- `check_expr`（expr/dispatch.rs:9-13）：`maybe_grow` 包 `check_expr_recorded`；
- `check_expr_recorded`（:15-26）：**唯一登记点**——`check_expr_inner` 后，若表达式是 `Call/New/Await` 调 `ctx.record_possible_exception()`，再 `sema.expression_types.insert((module, expr.span()), ty)`；`Paren` 只登记内层不登记括号自身；
- `check_expr_with_hint`（expr/hints.rs:23-34）：**第二出口**（双向检查下推），各分支须手动 `record_expression_type`（hints.rs:37-39）补登记。

`BodyCtx`（body/mod.rs:28-79）表达式相关字段：`module` / `scope` / `uninitialized`（按定义 span，读取报错、`&x` 临时摘除、赋值后移除）/ `reachable` / `in_unsafe` / `in_async` / `this_ty` / `type_env` / `exception_states`；`lambda.rs:28` 是 `in_unsafe` 的唯一重置点（lambda 不继承外层 unsafe 上下文）。

### 8.2 表达式分派全变体（dispatch.rs:30-145）

`check_expr_inner`：

```
Literal          → check_literal
Identifier       → check_identifier
Paren            → 内层（只登记内层）
Tuple            → Ty::Tuple
ArrayLiteral     → check_array_literal
ObjectLiteral    → check_object_literal
This             → ctx.this_ty 或报错
Unary            → check_unary
Await            → 非 async 报错但继续；Future(inner) → *inner
Binary           → check_binary
Is               → 只验证 operand + resolve_type，恒 Boolean
Cast             → check_cast
StructInit       → check_struct_init
Ternary          → expect_boolean + constant_bool 剪枝 + unify_types
Assignment       → check_assignment
MemberAccess     → check_member_access
IndexAccess      → check_index_access
Call             → check_call
New              → check_new
Match            → check_match(None)
Lambda           → check_lambda
```

### 8.3 标识符与值/类型分类（names.rs）

- `check_identifier`（:5-19）：`scopes.lookup` → 未定义"未定义的标识符 `{name}`"→ Error；`record_reference`；`uninitialized` 检查"局部变量 `{name}` 可能尚未初始化"。
- `value_type_of_symbol`（:21-76）：
  - `Variable` `ty=None` → 触发 `globals::resolve_name`（按需定型）；
  - `Function` → `Ty::Function`；
  - `NamedImport { target: Some }` → 追目标模块（**不支持再导出链**）；`target: None` → "标准库内建类型不能作为普通值使用"；
  - `NamespaceImport` → "命名空间 `{name}` 不能作为值使用，请用 `{name}.成员`"；
  - `Type` → "类型 `{name}` 不能作为值使用"。
- `type_key_of_name`（:78-99）、`classify_identifier`（:101-131）返回 `Value / Type / Namespace(NsTarget)`。

### 8.4 聚合字面量（aggregates.rs + unsafe_rules/aggregate.rs）

- `check_array_literal`（:4-46）：空数组报"空数组字面量无法推断元素类型，请显式写出元素或改用类型标注"（**只有 hint 能合法**）；非空以首元素 seed + `unify_types` + `concrete` + `is_assignable` 范围校验；
- `check_array_literal_with_hint`（:53-78）：空数组 → `Array(hint)`，逐元素下推，只报首个不匹配；
- 对象字面量（:80-120）：重复键"对象字面量的键 `{}` 重复"；简写 `{b}` → `check_identifier`；BTreeMap 有序；
- `check_struct_init`（unsafe_rules/aggregate.rs:5-138）：`resolve_type` + `instance_substitution` + `substitute_type_params`，字段用 `check_expr_with_hint` 下推，未知字段"struct `{name}` 没有字段 `{}`"、缺字段"字段 `{}` 未初始化"；Class 用 `{}` → "class 不能用 `{...}` 初始化，请用 `new`"。

### 8.5 字面量与 f-string（strings.rs）

- `check_literal`（:4-33）：`b` → `decode_byte_literal` 恒 `u8[]`（`Ty::Array(Int(U8))`）；`f` → `check_fstring` 后 String；普通/`r` → `decode_string_literal` 校验后都调 `check_fstring`；Number → `classify_number_literal` 失败"非法数字字面量"。
- `check_fstring`（:38-110）：手写扫描器支持嵌套 `{}` 与引号、`{{/}}` 转义；缺 `}` → "f-string 插值缺少配对的 `}"、落单 `}` → "f-string 中的字面 `}` 必须写成 `}}`"。
- `check_fstring_interpolation`（:117-175）：**关键宿主坐标**——`base = span.start + 1 + leading`，用 `novella_lexer::lexer_at(text, base)` 让插值内 token 带父文件真实坐标（修 E07：旧 `adjust_diagnostics` 只平移诊断、漏了类型表/绑定表）；解析须得到单个 Expression 语句。

### 8.6 一元运算（unary.rs:4-145）

- `AddressOf`：临时摘除 `local_slot`（取址不读值——不触发"未初始化"误报）；
- `Plus/Minus`：字面量保持未定型（负号翻符号位，`IntLiteral(false,v) → IntLiteral(true,v)`），使 `let x:i8 = -128` 可行（L04）；
- `Not`：仅 Boolean，恒返回 Boolean；`BitNot`：整数；
- `++/--`：要左值；指针需 unsafe；`void*` 报错；
- `AddressOf` 安全（只要左值）→ Pointer；`DerefPointer` 需 unsafe（S2）、`void*` / null 解引用报错。

### 8.7 二元运算、cast 与 unsafe 闸口（operators.rs）

- `require_unsafe`（:5-13）：**唯一闸口**——`!ctx.in_unsafe` → `sema.unsafe_error("{what}是不安全操作，需要 `unsafe {{ }}` 块")`（NVL3004）。
- `check_binary_with_hint`（:30-84）：对 `&&/||`（Boolean hint + constant_bool 短路）、`??`（rhs hint = lhs 或调用方 hint）下推。
- `check_binary_operands`（:88-257）矩阵：
  - opaque 早期返回（比较/逻辑 → Boolean，算术 → Unknown）；
  - `&&/||` Boolean；`??` 左须 `is_nullable`，返回 lhs 不隐式解引用；
  - `==/!=` 等：struct 禁比"struct 类型不支持 `==`/`!=` 比较"；
  - 关系：数值或 String；`+`：字符串拼接或算术；`%`：整数；
  - 位运算：同宽整数"整数操作数位宽不一致"；移位返回 `concrete(lhs)`；
- `check_arithmetic`（:259-328）：`void*` → "void* 不支持指针算术" Error；指针 ± 整数需 unsafe、整数 + 指针、指针 - 指针 → isize；`unify_numeric`（:330-344）不隐式数值转换；
- `comparable`（:346-354）/ `unify_types`（:356-376）；
- `check_cast`（:378-422）：`ty_eq` / numeric 间（含 int→float 安全）/ Null → nullable 均 ok；指针 ↔ 整数或指针 `as` 需 unsafe（"指针显式转换"，S5）；引用类型向上转型 ok、向下转型需 unsafe（"向下转型"）；否则"无法把 `{}` 转换为 `{}`"；恒返回 target。

**unsafe S1–S10 映射**（design.md:597-606）：

| 规则 | 内容 | 实现位置 |
|------|------|----------|
| S1 | `&x` 安全（只要左值） | unary.rs:116-123 |
| S2 | `*p` 需 unsafe | unary.rs:124-143 |
| S3 | `p[i]` 需 unsafe | call.rs:18-55 |
| S4 | 指针算术需 unsafe | operators.rs:279-301 |
| S5 | 指针 `as` 需 unsafe | operators.rs:396-401 |
| S6 | `p.m` 需 unsafe | member.rs:236-237 |
| S7 | 指针比较安全 | operators.rs:152-170 |
| S8 | `void*` 隐式赋值 + 解引用/算术报错 | 多处 |
| S9 | `null → T*` 合法 + null 解引用报错 | 多处 |
| S10 | 指针不隐式转换 | 多处 |

（注：completion-plan 里的 S10 = 继承覆写，是另一套编号体系。）上下文传播：唯一入口 `Statement::Unsafe`（body/mod.rs:378-384），`fork_scope` 继承，lambda 是唯一重置点。

### 8.8 赋值（assignment.rs）

- `is_lvalue`（:175-235）：合法目标 = Paren / Identifier（仅 Variable 非 const 与 Param）/ MemberAccess（静态字段须 is_static）/ IndexAccess / This（作基座）/ DerefPointer；
- `check_lvalue_assignment`（:237-276）：const → "不能给 const 变量赋值"；NamedImport → "导入的 `{}` 是只读的"；Function → "不能给函数赋值"；this → "不能给 this 赋值"；兜底"赋值目标不是可写的左值"。
- `check_assignment`（:5-65）**顺序敏感**：
  - 普通赋值：`lvalue_hint` 只读查声明类型 → `check_expr_with_hint(RHS)` → 左值诊断 → 临时摘除 `uninitialized` 读当前值 → 放回 → `is_assignable`（"不能把 `{}` 赋给 `{}`"，优先 `assign_mismatch_reason`）；
  - 复合赋值：`check_arithmetic_compound`（:139-172）复用 BinaryOp 矩阵 → "复合赋值结果 `{}` 不能赋回 `{}`"；
  - 最后移除被赋值符号的 `uninitialized`。
- `lvalue_hint`（:68-129）覆盖 Identifier/This/Paren/MemberAccess/IndexAccess/DerefPointer/Call（async 包 Future）。

### 8.9 调用、索引、new（call.rs，464 行）

- `check_index_access`（:18-55）：索引须整数（否则"索引必须是整数"）；Array → 元素类型；Pointer → S4 `require_unsafe("指针下标访问")` 后 inner；Tuple → "元组索引需要常量下标，本期暂不支持（请用解构）" → Unknown；opaque → Unknown。
- `check_call`（:57-186）：
  - callee 是 Identifier 且 classify 为 Type → "类型 `X` 不能直接调用：class 请用 `new X(...)`，struct 请用 `X{ ... }`" → Unknown；Namespace → "命名空间 `X` 不可调用"；
  - `callee_ty = check_expr`；`type_arg_tys` resolve；`explicit = sig.type_params zip type_arg_tys`；`param_tys = instance_param_tys(sig)` 代入 explicit；`arg_tys` 逐参 `check_expr_with_hint(param_ty)` 下推（如 `takes([])` 空数组靠形参定型）；
  - callee_ty 为 `Function` → `build_substitution`（D05：显式优先，缺失则 `infer_type_param` 从实参推导，冲突报错），逐 type_param 检查 subst 存在（"无法推导泛型参数 `X`，请显式提供类型实参"）与 `satisfies_bound`（"泛型实参 `X` 不满足约束 `Y`"），代入后 `check_call_against_sig`，ret = instantiated.ret，sig.is_async 时包 `Ty::Future(ret)`；opaque → Unknown；其余"X 不可调用"。
- `check_call_against_sig`（:218-278）：泛型 arity 校验；`sig.is_extern` → `require_unsafe("调用 extern 函数")`（E7，:239-242）；`offset = sig.is_method && 首参 self`（接收者不算显式实参）；required = 非默认形参数，范围 `[required, params.len()]` 校验否则"实参个数不匹配：需要 R–P 个，给了 A 个"；逐参 `is_assignable` 校验。
- `check_new`（:280-452）：`path()` 递归收集类型路径（Identifier / MemberAccess）；非路径 → "new 的目标必须是类的类型路径"；`resolve_type` 拆 `Applied(base, type_arg_tys)`；`class_subst = type_params zip type_arg_tys`；`init_params = Class 的 __init 形参` 代入；arg_tys 用 `check_expr_with_hint` 下推；Class → 泛型 arity 校验、`is_abstract_class` → "class `X` 是抽象类（含未实现的抽象方法），不能 `new`"、有 `__init` 则 `check_call_against_sig`、无 `__init` 要求零实参；返回 `Ty::Class` 或 `Ty::Applied` 并登记 `generic_uses`；Struct → "struct `X` 不能用 `new` 构造，请用 `X{ ... }`"；Exception 根 → 要求零实参返回 resolved；其余 Error。
- `instance_param_tys`（:457-464）：`is_method && 首参 self` 时跳过（与 `FunctionSig::view` 的 receiver 规则一致）。

### 8.10 类型下推表（hints.rs）

`check_expr_with_hint` 的分支（:4-22, :48-169）：ObjectLiteral + Object；Match(Some(hint))；Paren 穿透；ArrayLiteral + Array（空数组合法）；Tuple + Tuple（位数不符报错不伪造）；Ternary 两分支共享 hint，合并失败但可赋 hint 则取 hint；`??` / `&&` / `||`；`Literal::Null` + nullable → hint；其余走 check_expr。`unify_tuple`（:172-189）不符 → "元组类型 `{}` 与期望 `{}` 不兼容"，返回 hint。每个手动分支都要补 `record_expression_type`。

### 8.11 级联抑制与诊断码

`is_opaque() = Unknown | Error`（ty/mod.rs:279-284）：Unknown = 推导不出、Error = 已报明确诊断。`check_binary_operands` 一侧 opaque 早返回；`check_unary` 各分支 opaque 放行；出错仍把 Error/Unknown 登记进 `expression_types`，HIR 投影得 `TypeView::Error/Unknown` 不崩溃；CLI 入口有错则 fail-fast 不进 codegen。

诊断码总表：`NVL3001` 名称、`NVL3002` 类型、`NVL3003` 声明、`NVL3004` unsafe、`NVL3009` warning（语义）；词法 `NVL1001/1002`、语法 `NVL2001`、I/O `NVL9001`。

诊断码总表：`NVL3001` 名称、`NVL3002` 类型、`NVL3003` 声明、`NVL3004` unsafe、`NVL3009` warning（语义）；词法 `NVL1001/1002`、语法 `NVL2001`、I/O `NVL9001`。

---

## 第 9 章 `typecheck/`：语句、控制流、绑定、match、lambda

### 9.1 `BodyCtx` 完整字段（body/mod.rs:28-79）

```rust
pub(crate) struct BodyCtx {
    pub(crate) inferred_returns: Option<Rc<RefCell<Vec<(Span, Ty)>>>>,  // 仅无返回注解的块体 lambda
    pub(crate) return_states: Option<Rc<RefCell<Vec<HashSet<Span>>>>>,   // 确定赋值状态快照
    pub(crate) exception_states: Option<Rc<RefCell<Vec<HashSet<Span>>>>>,
    pub(crate) reachable: bool,
    pub(crate) loop_assignments: Option<Rc<RefCell<LoopAssignments>>>,   // breaks/continues: Vec<HashSet<Span>>
    pub(crate) uninitialized: HashSet<Span>,      // 按定义位置追踪未赋值局部（避免同名遮蔽污染）
    pub(crate) module: ModuleId,
    pub(crate) scope: ScopeId,
    pub(crate) in_unsafe: bool, pub(crate) in_async: bool, pub(crate) in_loop: bool, pub(crate) in_catch: bool,
    pub(crate) this_ty: Option<Ty>,
    pub(crate) expected_return: Option<Ty>,       // 顶层 None（return 即报错）
    pub(crate) type_env: HashMap<String, Ty>,     // 泛型形参 T → TypeParam("T")
    pub(crate) fill_existing: bool,               // 顶层变量回填声明收集阶段符号
}
```

`BodyCtx::base`（:53-72）全 false/None、`reachable = true`。`record_possible_exception`（:73-79）把当前 `uninitialized` 快照压入 `exception_states`。

### 9.2 `check_stmt` 与语句分派（body/mod.rs:83-389）

- `check_stmt`（:83-97）：`stacker::maybe_grow` 包 `check_stmt_inner`，之后若 `reachable` 且是 Return 压 `return_states`、是 Throw 调 `record_possible_exception`。
- `check_stmt_inner`（:99-389）分派：

| 语句 | 处理要点 |
|------|----------|
| `Variable/VariableList` | `check_variable_decl` |
| `Return` | `expected_return` None → "`return` 只能出现在函数内"；Some → `check_expr_with_hint` 下推期望类型，收集 `inferred_returns`，`is_assignable` 校验 |
| `Break/Continue` | 压 `loop_assignments` 的 breaks/continues 快照；`!in_loop` 报错 |
| `Throw{expr:None}` | `!in_catch` → "throw; 仅能在 catch 内重抛"；Some → check_expr 后要求 `is_subclass(exception_type(), ty)` |
| `Yield` | 明确报 "`yield` 语义尚未支持（语法保留，检查跳过）"但仍检查子表达式 |
| `Block` | `check_block` |
| `If` | 逐分支 `fork_scope` + `constant_bool` 剪枝 + `falls_through` 合并 `uninitialized`，无 else 时保留条件里的赋值 |
| `While` | 布尔条件 + fork_scope + in_loop/loop_assignments，`constant_bool == true` 时 uninitialized 取 breaks 汇总 |
| `For` | `ForIn`：check_expr 右侧须 `Ty::Array`，`bind_pattern` 左侧元素类型；迭代表达式必执行一次、循环体可零次，`uninitialized = entry`。`ForLoop`：init 支持 Declaration/Expression，condition 布尔，update 只由正常尾部或 continue 到达，return/break 不经过；always = 无 condition 或常量 true |
| `Try` | `check_try`（见 9.5） |
| `With` | 资源类型 check_expr，对 `__enter`/`__exit` 查 `lookup_member_chain` 并检查可见性；binding 标注与 resource_ty 用 `is_assignable` 校验后 `bind_pattern`（D08：绑定原资源） |
| `Unsafe` | fork_scope 置 `in_unsafe = true` |
| `Expression` | check_expr |

- `fork_scope`（:391-410）：克隆所有共享 Rc 状态与标志、scope 换新、`fill_existing = false`。
- `check_block`（:411-419）：新 Block scope + fork_scope，逐句 check_stmt 后 `reachable &= flow::falls_through`，回写 uninitialized。
- `expect_boolean`（:460-469）：check_expr 后要求 `Ty::Boolean` 或 opaque。

### 9.3 返回落尾分析（flow.rs，265 行）

`flow.rs` 与 `body/mod.rs` 的确定赋值分析**并行**：前者管"函数是否有正常落尾未返回"，后者管"局部变量是否所有路径已赋值"。

- `Exits { normal, returns, throws, breaks, continues: bool }`（:9-41）：
  - `union`（或）、`then(self, next)`（self.normal 为真时清 normal 并 union next，实现顺序组合）。
- 入口 `check_statement` / `check_block`（:43-65）→ build Exits → `check_exit`（:67-75）：非 Void 且非 opaque 且 `exits.normal` → "返回类型为 `{ret}` 的函数存在正常到达末尾而未返回值的路径"。
- `constant_bool`（:83-96）：仅字面量 Boolean 与 Paren（递归），stacker 保护。
- `loop_exits`（:98-113）：condition == false → normal；否则 `inner = statement(body)`，`normal = condition != true || inner.breaks`。
- `statement_inner`（:115-227）：
  - Return→returns；Throw→throws；Break→breaks；Continue→continues；Block/Unsafe→block；
  - If→逐分支 union（constant_bool false 剪枝、true 后 remaining = false），最后 `exits.normal |= remaining`；
  - While/For→loop_exits（ForLoop 无 condition 视 `Some(true)`，ForIn 用 None）；
  - Try→`block(block)` union 各 catch block，finally 的 cleanup 若非 normal 则替换出口并 union（保守计入任一执行中表达式可能抛出）；
  - With→block；Expression→expression；Variable/VariableList/Empty/Yield→normal。
- `expression_inner`（:236-265）：Match → 若 `sema.exhaustive_matches` 含 `(module, span)` 则 `Exits::default()`（无 normal）否则 normal，再 union 各 case body（Expr→expression，Block→block）；其他→normal。
- `falls_through` / `block_falls_through`（:121-133）：`statement/block(...).normal`。

### 9.4 局部声明、绑定模式与字面量归一化（body/bindings.rs）

- `check_variable_decl`（:4-75）：`is_const = VariableType::Const`；有标注先 `resolve_type`，用 `check_expr_with_hint` 下推；`bound_ty` 四情形：
  - `(Some, Some)`：`is_assignable` 校验（失败取 `assign_mismatch_reason` 或"初始化类型 X 与标注 Y 不兼容"），取 annotated；
  - `(Some, None)`：const 未初始化 → 报错；
  - `(None, Some)`：Null → "无法用 `null` 推断变量类型，请添加类型标注" → Unknown；否则 `concrete(value_ty)` 并校验范围；
  - `(None, None)`："变量声明需要类型标注或初始值（无法推断类型）" → Unknown。
  - 登记 `declaration_types[(module, decl.span)] = bound_ty`，`bind_pattern_typed`；未初始化且非 fill_existing 时 `collect_pattern_names` 把名字 span 塞进 `ctx.uninitialized`。
- `bind_pattern` / `bind_pattern_typed` / `bind_pattern_impl`（:77-131）：Ident → `SymbolKind::Variable { is_const, ty }`，fill_existing 时 `lookup_mut` 回填符号类型否则 `define_local`；Tuple → 要求 `Ty::Tuple` 且 arity 相等（否则"元组解构的元素个数不匹配" / "不能用 X 做元组解构"，opaque 容错给 Unknown），逐元素递归。
- `define_local`（:133-154）：构造 `Symbol`（visibility None，`name_span = Span{start, start+name.len()}`），`scopes.define` 失败报"重复定义 `{name}`"。
- `concrete`（:155-168）：见 6.6 节。

### 9.5 try/catch/finally 确定赋值（body/try_flow.rs，157 行）

`check_try`（:7-157）：

- 无 catch 且无 finally → "try 必须有 catch 或 finally"；
- 为 return / exception 各建 `Rc<RefCell<States>>`，记 loop_start 的 breaks/continues 长度；
- body fork_scope 注入 return_states / exception_states 后 `check_block`；normal 只在 `block_falls_through` 时收 `body.uninitialized`；
- `thrown = exceptions.take()`，`catch_entry = thrown` 展平；逐 catch：
  - catch 类型须 Exception 子类（无标注默认 `exception_type()`）；
  - 已被先前 catch 类型 `is_assignable` 覆盖 → "该 catch 已被先前的异常类型覆盖"；
  - `catches_all |= ty_eq(ty, Exception)`；
  - catch 名字的 `name_span` 从 tokens 精确回查（span 在 `catch.span..catch.block.span` 间、slice == catch.name）；
  - `define_local` 为 const 变量，`in_catch = true`，`check_block` 后若落尾收 uninitialized；
- `!catches_all` 时把 thrown 放回 exceptions；
- finally 入口 `incoming = normal ∪ return_states ∪ exception_states ∪ 新产生的 breaks/continues`，`check_block` 后 `initialized = incoming − ctx.uninitialized`，`resumes = block_falls_through`；若 `!resumes` 且 lambda 无注解则 drain 掉 finally 区间内 `inferred_returns`（finally 覆盖原 return）；
- 循环跳转：resumes 时对 `[bs..be]` / `[cs..ce]` 逐个 `retain` 掉 initialized，否则 drain 掉旧跳转保留 finally 新增；
- `ctx.uninitialized = normal − initialized`；resumes 时把 returns / exceptions 状态 `retain initialized` 后交还父级。

### 9.6 match 模式覆盖与穷尽性（matching.rs，181 行）

- `constant_literal`（:15-26）：Literal 与 Paren 递归、Unary +/- 且 Number 时取常量身份；调用等动态模式不参与重复判定。
- `check_match`（:28-181）：

  - subject = check_expr；
  - 逐 case：
    - Expr 模式 → check_expr + comparable 校验（失败"case 值 X 与 match 目标 Y 不可比较"），常量 key（i/f/s/b/null）判重复 → "重复的 case 常量"；类型模式被先前 `is_assignable` 覆盖 → "case 已被先前类型模式覆盖"；
    - Type 模式 → `resolve_type` + 覆盖检查 + possible 判定（Union 成员双向 `is_assignable`）失败 → 警告 `NVL3009` "类型模式不可能匹配目标类型"；其他覆盖、重复与穷尽性错误仍按原规则报告；
  - `exhaustive` = 有 default || Boolean 且 true/false 都出现 || Union 各成员被某类型模式 assignable || 非 Union 时 `ty_eq`；exhaustive 时 `sema.exhaustive_matches.insert((module, node.span))`；
  - 逐分支：Expr body 用 `check_expr(_with_hint)`，Block body 用 `check_block` 且类型 Void；`has_value` / `has_block` 标志；`reaches_join`（Expr 恒真，Block 用 `block_falls_through`）时合并 uninitialized；
  - 结果分支用 `unify_types` 合一，失败时若 hint 同时 assignable 两边则取 hint，否则"match 分支类型不一致"→ Error；
  - 混用值块 → "match 不能混合值分支与 void 块分支"；有值且非穷尽 → "值 match 必须穷尽目标类型或提供 default"；非穷尽时合并 before；
  - 返回 `result.unwrap_or(Void)`。

### 9.7 lambda 检查与返回推导（lambda.rs，177 行）

- `check_lambda`（:14-146）：新 Function scope + fork_scope，重置 in_async = lambda.is_async、in_loop = false、loop_assignments / return_states / exception_states = None、reachable = true、in_catch / in_unsafe = false、`inferred_returns = None`（继承词法 this）。
  - Single 参数名 self → "lambda 不能声明实例接收者 self"，类型 Unknown；
  - List 参数逐项查 self / 重名、`resolve_type` 标注（无则 Unknown）、默认值 `check_expr_with_hint` + `is_assignable`，`define_local` 并收集 `ParamSig { name, ty, has_default }`；
  - `annotated_ret = return_type resolve`；
  - Expr body → `check_expr(_with_hint)`，标注时校验兼容取标注，否则 `concrete(body_ty)`；
  - Block body → `expected_return = annotated 或 Some(Unknown)`，无标注时 `inferred_returns = Some(default)`，`check_block`，有标注时 `flow::check_block`，返回 annotated 或 `infer_block_return`；
  - 产出 `Ty::Function(Rc<FunctionSig { module, name: None, params, ret, type_param_count: 0, type_params: vec![], is_async, is_extern: false, is_method: false }>)`。
- `infer_block_return`：普通函数、方法与块体 lambda 共用。收集自身可达的 `inferred_returns`，可达落尾追加 `(block.span, Void)`，空则 `Void`；先检查组合类型中是否残留 `Unknown` / `Error`，再对原始返回类型逐项 `unify_types`，最后 `concrete` 并逐项校验字面量范围。不兼容返回报错；**不从调用点反向猜测**。

普通函数与方法的推导任务在 `check/returns.rs` 中管理：

- 声明注册时，无返回注解的普通有体函数和方法先以 `Unknown` 登记；`extern`、无体声明与特殊方法保留既定规则。
- 在字段与顶层变量检查前收集任务，以 `(ModuleId, 函数名或类型::方法名)` 为键；函数名称解析与实际方法/协议查询按需触发推导，支持前向及跨模块依赖。
- 任务状态为 `Pending → Resolving → Done`；遇到 `Resolving` 回边报返回推导循环并固定为 `Error`，要求显式注解打断循环；已经完成的函数体不重复检查。
- 推导结果回填全局函数或类型方法签名及 `declaration_types`，供 `main` 校验、调用点与 HIR 使用；全部任务定型后再校验继承覆写和 trait 实现签名。
- 推导模式的 `return` 直接检查表达式，显式返回注解才向表达式下推期望类型；嵌套 lambda 重置返回收集器，保持独立。

### 9.8 成员访问、可见性与继承链（member.rs，419 行）

- `check_member_access`（:21-83）：特殊路径 `ns.member`——
  - `classify_identifier` 为 `Namespace(ExceptionBuiltin)` → "std:exception 没有可用的值成员"；
  - `Namespace(Module)` → `global_scope_symbol` 查找（无 → "模块 X 中没有 Y"；`!is_importable` → "Y 在模块 X 中不是 pub"），`record_reference` 后 `value_type_of_symbol`；
  - `IdentTarget::Type` → `check_static_member`；否则 `check_expr(target)` 后 `member_type_of`。
- `static_receiver`（:85-112）：`type_key_of_name` 还原类型键，Alias → resolve_type，Struct/Class/Trait → 对应 Ty。
- `check_static_member`（:114-180）：仅静态成员（含继承链），`lookup_member_chain(static_only = true)`，`subst = member_substitution`，Field 要求 `is_static` 否则"字段不是静态成员（struct/class 字段随实例）"，Method 要求 `is_static` 否则"方法不是静态方法，不能通过类型名访问"，`check_visible` 后 `substitute_type_params`。
- `member_type_of`（:182-255）：TypeParam → 查 `type_param_bounds` 递归；Object(fields) → 字段查找；Applied/Trait/Struct/Class → `lookup_member_chain(static_only = false)`，record + check_visible + subst，Field/Method 都经 `check_static_access`（禁止静态成员经实例访问），`substitute_type_params`；Pointer(inner) → S3 `require_unsafe("通过指针访问成员")` 后递归 inner（须 Struct/Class/Applied）；opaque 容错；其余"类型 X 没有成员 Y"。
- `MemberInfo { Field(FieldDef), Method(MethodSig) }`、`MemberHit { info, owner: (ModuleId, String) }`（:263-273）；`MemberHit::record`（:277-295）：引用 span 只覆盖末尾成员名（`span.end - name.len()..span.end`），definition 取 `field.name_span` / `method.name_span`，kind Variable/Function。
- `check_visible`（:300-320，D02）：pub 或同模块可见；否则 `ctx.this_ty` 是 owner 的子类（`is_subclass(owner_ty, this_ty)`）则可见（类内部含子类方法体）；否则"成员 X 不是 `pub`，无法从类型外部访问"。
- `lookup_member_chain`（:323-382）：`canonical_method_name` 规整，visited 防继承环，Trait → 只查 methods，Struct → 字段+方法后 return None（无继承），Class → 字段+方法，未命中取 `parent_resolved` 继续向父类；`static_only` 参数当前未用（`_static_only`）。
- `check_static_access`（:388-402，D02）：`is_static` → "X 是静态成员，不能通过实例访问，请改用类型名"。

### 9.9 关键架构结论

- 表达式/语句类型登记统一走 `check_expr`（body 内 `check_variable_decl` 写 `declaration_types[(module, decl.span)]`）；
- **确定赋值用"按定义位置 Span 为键的 HashSet"而非名字**，正确支持遮蔽；
- **"落尾"与"确定赋值"是两套并行分析**：flow.rs 的 `Exits` 组合 vs body/try_flow.rs 的 `uninitialized` 状态机；
- match 穷尽性作为 sema 事实存 `exhaustive_matches`，供两者与 HIR 共用；
- 所有递归入口套 `stacker::maybe_grow(128*1024, 2*1024*1024)`。

- 所有递归入口套 `stacker::maybe_grow(128*1024, 2*1024*1024)`。

---

## 第 10 章 横切机制：诊断、级联抑制、取消与错误恢复

### 10.1 诊断唯一存储

全模块所有诊断只存于 `DiagnosticReport.modules`，每模块 `ModuleDiagnostics` 持有一个合并排序的 `Vec<Diagnostic>` + 真实 `syntax_diagnostic_count`。阶段视图（`syntax_diagnostics()` / `semantic_diagnostics()` / `ordered()`）只是切片与排序，不复制。全局诊断（I/O 失败、分析取消）单独存 `global_diagnostics`。

诊断结构（novella-core/src/diagnostic.rs）：`Diagnostic { severity, code, message, location: DiagnosticLocation{ file, span, line, column } }`，serde 派生，CLI 统一按 rustc 风格渲染（`error: <msg>` + `--> 文件:行:列` + 源码行 + caret）。列号 1-based，caret 按字符渲染（多字节不错位）。

错误码总表：

| 码 | 来源 |
|----|------|
| `NVL1001/1002` | 词法 |
| `NVL2001` | 语法 |
| `NVL3001` | 名称/引用（Sema::error、Module::error，多数语义错误） |
| `NVL3002` | 类型（type_error） |
| `NVL3003` | 声明（decl_error） |
| `NVL3004` | unsafe（unsafe_error） |
| `NVL3009` | warning（语义告警也用此码） |
| `NVL9001` | I/O |

存在细节不一致：`resolve/types.rs:68/121` 用 `self.error`（NVL3001）即使语义偏类型错误；`register_bounds` 用 `type_error`；`registration.rs` 的 extern/self/成员冲突用 `decl_error`。

### 10.2 级联抑制：`Unknown` / `Error` 双档

- `Unknown`：前置已报诊断或合法未推导状态（如未标注形参 `ParamSig.ty = Unknown`，调用点跳过该参检查）；
- `Error`：产生处已报**明确**诊断（非法数字、泛型 arity 不符、别名循环、`yield` 未实现等）；
- `is_opaque() = Unknown | Error`：检查时一律放行，避免一个错误炸出一片次生错误。短路点：`assignable` 第 1 条、`infer_type_param` 的 TypeParam 分支、`satisfies_bound`、`is_subclass`、继承分类、成员查找、`check_binary_operands`、`check_unary` 各分支。
- 出错仍把 `Error/Unknown` 登记进 `expression_types` / `declaration_types`，HIR 投影得 `TypeView::Error/Unknown` 不崩溃；CLI 入口有错则 fail-fast 不进 codegen。

### 10.3 取消协作

`FileLoader::is_cancelled()` 是唯一钩子。`analysis.rs` 的每个阶段之间都检查；中途取消则 `complete()` 返回 false，加全局诊断"分析已取消，产物不可使用"。LSP 可用此在重新分析时中断旧任务。

### 10.4 错误恢复手法汇总

| 手法 | 说明 |
|------|------|
| syntax_failed 早退 | 语法失败模块整体跳过所有后续阶段，`ModuleOutput.syntax_failed = true` 交下游拒绝生成代码 |
| 占位模块 | 文件读失败也构造 `load_failed = true` 的占位模块，保证 `modules` 下标稳定 |
| None 防御签名 | lookup 不到 `Function{sig}` 直接 return；形参用 zip(AST, sig) 而非索引硬取，防 panic |
| 环检测固化 Error | 字段 `State::Resolving` / 全局 state 1 回边时把相关符号类型固化为 Error，避免无限递归 |
| 重复定义只报一次 | 顶层 Phase 1、成员/形参各自独立、main extern E5 后 `check_main_entry` return |
| visited 保护 | lookup_member_chain / find_implementation / is_implemented / is_subclass / satisfies_bound 全部带环检测 |
| 借用安全克隆 | 先收集 keys / 克隆 def 再写回，规避 Rust 借用冲突 |
| 保守剔除 | cfg 条件解析错误 → evaluate 返回 false（不放行），同时报错 |
| warning 而非 error | 导入语法失败模块报 warning 而非 error，允许部分检查继续 |

### 10.5 引用记录与跳转

`record_reference((module, span), Definition)`：跳过零长 span（避免箭头/占位误命中）；具名导入追到目标模块原始定义；`definition_index` 显式跳过 Import 符号，只登记真实定义 + 字段/方法。因此：

- LSP 跳转直接到定义源头，而非 import 行；
- `Definition.span` 与类型摘要定位一律取 `name_span`（只覆盖标识符），否则编辑器会选中 `() -> int { ... }` 整段；
- 成员引用的 span 只覆盖末尾成员名（`span.end - name.len()..span.end`）。

---

## 第 11 章 设计不变量、项目约定与可改进点

### 11.1 必须遵守的设计不变量（改动时不可破坏）

1. **诊断只有一份**：禁止新增 `sema_diagnostics` 之类的并列字段；阶段切分靠 `syntax_diagnostic_count`。
2. **词法/语法只跑一次**：下游（CLI/codegen）不得重跑 lexer/parser；语法产物经 `SemaResult.modules` 交还。
3. **注释只过滤一次**：`LineComment` 是正式 token，`novella-parser/src/preprocess.rs` 的 `retain` 是唯一过滤点；`Module.tokens` 保留完整流（含注释）。
4. **双路径 key/path**：`module_key` 规范化只用于比较/去重/查找；原始 `path` 只用于诊断回显。
5. **位置换算只在 core**：行列用 `novella_core::line_index::LineIndex`，路径用 `novella_core::path::module_key`，诊断填空用 `novella_core::decor`；LSP 的 UTF-16 列在其上叠加。
6. **表达式类型登记唯一出口**：`check_expr`（`expr/dispatch.rs`）是 `expression_types` 的唯一登记点；`check_expr_with_hint` 的每个分支必须手动补 `record_expression_type`。
7. **函数签名名义键**：`(module, name)`；顶层用原名，方法用 `类型::方法`，lambda / 占位 `name: None`；`name` 只用于合成 HIR 符号键，不决定类型投影。
8. **实例方法 self 不占 params 位**：由 `FunctionView::receiver` 表达；HIR 成员访问统一查表达式类型表，不用局部类型兜底。
9. **签名占位与回填分离**：Phase 1 建骨架、Phase 2b 回填真签名——循环导入与前向引用正确的根本。
10. **泛型参数带声明身份**：`param_key = format!("{}@m{}:{}", name, module, span.start)`。
11. **确定赋值按定义 Span 为键**，不按名字——正确支持遮蔽。
12. **已知未实现特性一律 error，不 warning 放行**：`@cfg` 已实现；其他未定义指令、`yield` 明确报错；std 除 Exception 根外必须加载真实声明。
13. **公开面最小化**：`Sema` 仅 `pub(crate)`；新增 `pub` 前确认有 crate 外调用方。
14. **递归防护**：所有递归入口套 stacker；AST 析构另有自定义 Drop 分段栈（见根 AGENTS.md）。

### 11.2 与项目覆盖矩阵的对应

规则覆盖以 `docs/sema-coverage.md` 为唯一权威矩阵（每条 design 规则 / 每个 AST 变体一行，标注 已实现/部分/缺失/待决策）：M01-M09 模块与导入、T01-T15 类型系统、L01-L11 字面量、D01R-D18R 声明、E01R-E29R 表达式、S01R-S19R 语句与控制流、A01R-A04R 异常/异步。补全计划 S00–S14 已全部完成并验收（106 passed / 24 suites，fmt/check/clippy -D warnings/doc/diff 均通过，44 正例 + 37 反例符合预期；记录见覆盖矩阵执行记录）。关键决策 D01–D08：

- **D01** 普通赋值 RHS→定位 LHS→写回；复合赋值 定位→读旧值→RHS→写回；
- **D02** 成员默认 pri、pkg≈pri、静态不得经实例、覆写形参全等/返回协变、抽象类不可 new；
- **D03** 局部读取须所有可达路径已赋值、无注解形参容错；
- **D04** trait 用括号列表显式实现；
- **D05** 完整显式泛型代入 + 实参递归推导；
- **D06** 值 match 必须穷尽；
- **D07** 匿名对象结构类型、b 字符串 u8[]、f16 IEEE binary16、f8 E4M3FN、usize/isize 按目标位宽；
- **D08** 编译器提供 `std:exception` Exception 名义根、throw; 仅 catch 内、内建可写 `Future<T>`、with as 绑定原资源、特殊方法签名。

### 11.3 可改进点（精读中发现，非缺陷结论）

以下均为实现细节层面的观察，不构成"需要立即修复"的断言：

1. **继承环诊断定位**：`resolve_class_parents` 的环诊断落 `Span{0,0}`（文件头），而 `ClassDef.span = class.span` 已写入——可改用真实声明位置。
2. **`is_implemented` 未走 canonical**：用 `def.methods.get(name)` 直接查，`__str` / `__str__` 判定可能与 `lookup_member_chain` 等其他路径不一致。
3. **`resolve_trait_impls` 文档注释过时**：注释称"首项是父类"，与按符号类型分类的实际实现不一致。
4. **`concrete` 丢弃字面量载荷**：注释称"取回符号使 -128 可落 i8"，代码实际 `let _ = (negative, value)` 一律落 i32；`let x:i8=-128` 靠赋值上下文的 `assignable` 正确通过，但注释与代码略有出入。
5. **`sig_eq` 纯结构口径**：忽略 `is_async/is_extern/module/name/type_param_count`，函数类型相等可能与名义期望有张力。
6. **`ty_eq` 的宽松相等**：`Unknown/Unknown`、`Error/Error`、`IntLiteral/IntLiteral` 均视为相等，与"避免级联"目标略有张力（两个不同来源的 Error 相等）。
7. **`substitute_type_params` 不递归进 `Struct/Class/Trait`**：泛型实参挂在 `Applied` 上，这些变体本身无可代换形参（S05 注释自述未闭环）。
8. **`float_literal_fits` 注释与常量出入**：注释写"标准未定、按不窄于 f32 放行"，实际已给出具体阈值（F8 464 / F16 65520），`float_range_text` 给的是最大有限值（448 / 65504），文案与判据口径不同——以代码常量为准。
9. **`build_function_sig` 重名**：重名时先定义者接收后定义者签名（Phase 1 已报重复定义，属恢复行为）。
10. **`lookup_member_chain` 的 `static_only` 参数当前未用**（`_static_only`），静态过滤实际在各调用点（check_static_member / check_static_access）完成。
11. **`register_reference` 对具名导入不支持再导出链**：`NamedImport{target: Some}` 只追一层目标模块符号，不递归追导入。

### 11.4 扩展阅读路径

按以下顺序读源码能最快建立完整心智模型：

1. `lib.rs` + `api.rs`：知道对外承诺什么；
2. `analysis.rs`：知道阶段怎么串；
3. `state.rs` + `symbol.rs`：知道内部状态长什么样；
4. `module/loader.rs` + `module/resolve_path.rs`：知道模块怎么进来；
5. `resolve/collect.rs` + `resolve/imports.rs` + `resolve/types.rs`：知道名字与类型怎么解析；
6. `ty/mod.rs` + `ty/relations.rs` + `ty/generics.rs` + `ty/literals.rs`：知道类型系统全貌；
7. `check/type_defs/*` + `check/decl.rs`：知道声明检查；
8. `typecheck/expr/*` + `typecheck/body/*` + `typecheck/{flow,matching,lambda,member,call}.rs`：知道最终怎么检查表达式与语句。

---

> 本文档基于逐文件精读写成，覆盖 `novella-sema` 全部源码文件。规则覆盖的权威状态以 `docs/sema-coverage.md` 为准；本组织描述的是"实现如何工作"，而非"规则应当如何"——两者如有出入，以代码与覆盖矩阵共同核对。


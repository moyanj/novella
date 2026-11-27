# 目标描述与条件编译

novella 的目标体系对齐 rustc 的形状：目标由一份 JSON 描述文件定义，内置目标就是随
编译器发布的描述文件，自定义目标走同一条加载路径。条件编译谓词全部由目标描述派生，
不再由各阶段自行猜测。

## 目标描述（TargetSpec）

`crates/novella-target` 提供 `TargetSpec`，字段与 rustc 的 target spec 同名
（kebab-case）。关键字段：

| 字段 | 含义 | 内置 Linux | 内置裸机 |
| --- | --- | --- | --- |
| `triple` | 目标三元组，`arch-vendor-os[-env]` | `x86_64-unknown-linux-gnu` | `x86_64-unknown-none-elf` |
| `arch` / `os` / `vendor` / `env` / `abi` | 参与 `target_*` 谓词 | `x86_64` / `linux` / `unknown` / `gnu` / 空 | `x86_64` / `none` / `unknown` / 空 / 空 |
| `data-layout` | LLVM 数据布局；首字符必须与 `endian` 一致 | `e-m:e-p270:32:32-…-S128` | 同左 |
| `pointer-width` | 指针位宽，32 或 64 | 64 | 64 |
| `endian` | `little` / `big` | `little` | `little` |
| `target-family` | 家族列表（可多值） | `["unix"]` | `["none"]` |
| `llvm-target` / `cpu` | 交给 LLVM 的目标名与 CPU | 同 triple / `generic` | 同 triple / `generic` |
| `target-features` | LLVM 特性串（`+`/`-` 前缀） | 空 | 禁用 MMX/SSE/AVX 等 17 项 |
| `reloc-mode` | `pic` / `static` | `pic` | `static` |
| `code-models` / `default-code-model` | 允许与默认的代码模型 | `["small"]` / `small` | `["small","kernel"]` / `kernel` |
| `opt-level` | 默认优化级别（`0`/`1`/`2`/`3`/`s`） | 缺省（等同 `0`） | 缺省（等同 `0`） |
| `runtime` | `managed` / `none` | `managed` | `none` |
| `has-float` | 是否允许浮点 | `true` | `false` |
| `requires-no-std` | 是否强制 `@no_std;`（或更强的 `@no_alloc;`）+ `@no_main;` | `false` | `true` |
| `emit` | 发射后端；缺省表示只作描述、不能生成代码 | `x86_64-linux` | `x86_64-bare` |
| `linker` / `linker-flavor` / `linker-args` | 链接器命令、风格与固定参数 | 缺省 / `clang` / `[]` | `ld.lld` / `lld` / `-m elf_x86_64 --static …` |
| `is-like-linux` / `is-like-bare` / `is-like-msvc` | 链接行为类比 | linux | bare |

`TargetSpec::validate()` 校验下列不变量，任一不满足即报错而不是静默降级：

- `data-layout` 的字节序前缀与 `endian` 一致；
- `pointer-width` 必须是 32 或 64；
- `code-models` 若存在，必须包含 `default-code-model`；
- 默认代码模型为 `kernel` 时必须有 `is-like-bare`；
- `emit` 缺省时不得声明 `linker` / `linker-args`；
- `x86_64-linux` 后端要求 `has-float` 且 `requires-no-std` 为 false；
- `x86_64-bare` 后端要求 `has-float` 为 false。

内置描述存放在 `crates/novella-target/src/builtin/*.json`，以 `include_str!` 编入二进制。
加载时先把覆盖层逐键合并到 `base.json` 模板（覆盖层写 `null` 表示保留模板值），再做
`serde_json` 反序列化与 `validate()`。因此自定义目标可以只写差异字段：

```json
{
  "triple": "x86_64-unknown-linux-gnu",
  "os": "linux",
  "target-family": ["unix"],
  "pointer-width": 64,
  "has-float": true,
  "emit": "x86_64-linux",
  "linker-flavor": "clang",
  "linker-args": [],
  "is-like-linux": true
}
```

## 选择与查询

目标选择的优先级：CLI `--target` > 清单 `build.target` > 默认内置目标。`--target`
既接受内置 triple 名，也接受 `.json` 路径（含 `/` 或 `\` 的值按路径处理）。
自定义描述必须开启 `-Z custom-targets`，否则报
`自定义目标 '<path>' 需要 -Z custom-targets`。

```sh
novella check --print target-list
novella check --print target-spec-json --target x86_64-unknown-none-elf
novella compile -Z custom-targets --target ./targets/my-linux.json main.nov -o a.out
```

`--print target-spec-json` 的输出可被 `--target <file>` 原样回读（往返一致的单元测试见
`crates/novella-target/src/spec.rs`）。LSP 不接收 `-Z` 开关，自定义目标在编辑器内一律
放行，可读性由文件本身决定。

## 条件编译

### 谓词集合

`@cfg(...)` 求值器位于 `crates/novella-target/src/cfg.rs`，sema 的
`crates/novella-sema/src/cfg.rs` 只负责 AST 裁剪。可用谓词：

| 类别 | 谓词 |
| --- | --- |
| 裸 flag | `unix`、`windows`、`target_has_float`、`debug_assertions`、`test`、`true`、`false` |
| 目标键值 | `target_os`、`target_arch`、`target_family`、`target_vendor`、`target_env`、`target_abi`、`target_endian`、`target_pointer_width`、`target_feature` |
| feature | `feature = "name"` |
| 组合 | `all(...)`、`any(...)`、`not(...)`（not 恰一个参数；all/any 可为空） |

`unix` / `windows` 由 `target-family` 派生，与 rustc 一致而不是比较字符串。
`target_env` 缺省为空串，因此 `@cfg(target_env = "")` 对内置目标成立。
`target_feature` 只暴露启用的特性（去掉 `+` 前缀，跳过 `-` 项）。
`panic` 在本语言恒为 abort，`@cfg(panic = "abort")` 不成立。

### 命令行

```sh
novella check --cfg 'build="dev"' --cfg ci main.nov
novella check --check-cfg 'cfg(build)' --check-cfg 'values(feature,"extra")' main.nov
```

`--cfg` 的注入值与目标派生值取并集：键值进 `extra.values`，裸名进 `extra.flags`。
因此自定义键（`--cfg 'build="dev"'`）可以在源码里写 `@cfg(build = "dev")`。

`--check-cfg` 默认关闭以保持历史行为；指定后按白名单校验。`values(key,"v",…)` 同时
声明键合法；裸 flag 用 `cfg(name,…)` 声明。越界报 `未声明的 cfg 键 `x`` 或
`未声明的 cfg 值 `v`（键 k）`。求值器内建键与 flag 始终合法。

feature 名不允许负形式（`-foo`）与 `/`、`:`，因为当前没有依赖图，`dep:` 形式会进入
`FeatureSet::cross_crate` 而不参与本地展开。`FeatureSet::resolve` 按 `implies` 做 BFS
传递展开且幂等。

### AST 裁剪

`@cfg` 可修饰函数、class/struct/trait、字段、方法及顶层指令块。多个 cfg 按 and 合并；
为 false 的节点在导入扫描、声明收集之前剔除，不参与名称与类型检查，但词法/语法错误仍
报告。所有 cfg 参数都会被解析，不因前一个条件为 false 而掩盖后续错误。已处理的 cfg
不继续交给 HIR。独立 `@cfg(...);` 无修饰目标，报错。

目标默认值不再是宿主平台：`CfgOptions::default()` 仍等于宿主（供未解析目标的调用方
使用），CLI 与 LSP 一律用 `CfgOptions::from_spec` 从实际目标派生。

## 优化级别

优化级别是目标配置的一部分（`BuildTarget.opt_level`，取值 `OptLevel::None` / `Less` /
`Default` / `Aggressive` / `Size`），优先级 **CLI `-O` > 清单 `build.opt-level` > 目标
描述 `opt-level` > `0`**：

```sh
novella compile main.nov -O 2        # 等同 --opt-level 2
novella compile main.nov -Os         # 体积优化
```

```toml
[build]
opt-level = "3"
```

接受的写法是 `0`、`1`、`2`、`3`、`s`（对应 `-O0`/`-O1`/`-O2`/`-O3`/`-Os`），非法值报
`不支持优化级别 <x>；可选 0、1、2、3 或 s（即 -O0/-O1/-O2/-O3/-Os）`。

后端（`crates/novella-codegen/src/lib.rs`）把级别同时用于两处：`create_target_machine` 的
`OptimizationLevel`（`0`→`None`、`1`→`Less`、`2`/`s`→`Default`、`3`→`Aggressive`），
以及在 `module.verify()` 之后、`write_to_memory_buffer` 之前跑一遍 LTO-free 的模块管线
（`run_passes`）：`0` 不跑管线，`1`/`2`/`3` 用 `default<O1>`/`default<O2>`/`default<O3>`，
`s` 用 `default<O2>` 并关闭循环展开、循环向量化、SLP 向量化与循环交错。管线执行失败报到
`CodegenError`，不做静默降级。

优化级别随 `BuildTarget` 进入项目缓存摘要，切换级别不会命中旧产物。

### 各档实际执行的管道

`-O0` 不进入管道；其余四档调用 `run_passes("default<Ox>")`，`-Os` 额外关掉四个
`PassBuilderOptions` 开关。`default<Ox>` 是 LLVM 的版本化别名，下面的清单是
LLVM 23.1.4（与 `inkwell` 的 `llvm23-1` 特性一致）的快照，升级 LLVM 后需重新核对：

| 档位 | 目标机器 `OptimizationLevel` | 模块管道 | 关闭的变换 |
|------|------------------------------|----------|------------|
| `-O0` | `None` | 不跑 | — |
| `-O1` | `Less` | `default<O1>` | — |
| `-O2` | `Default` | `default<O2>` | — |
| `-O3` | `Aggressive` | `default<O3>` | — |
| `-Os` | `Default` | `default<O2>` | 循环展开、循环向量化、SLP 向量化、循环交错 |

**`-O0`**：后端走快速指令选择，不跑任何函数/模块级 pass。注意 `default<O0>` 里那几条
强制 pass（`always-inline`、`coro-*`）同样不执行，因此依赖强制内联才能验证的 HIR 需要
更高档位。这一档保持 HIR 到机器码的逐指令忠实映射，便于对照反汇编调试。

**`-O1`**：保守的标量优化。过程间有 `ipsccp` / `called-value-propagation` / `globalopt` /
`globaldce` / `deadargelim` / `function-attrs`；内联走 `cgscc(devirt, inline)` 的 O1 阈值；
函数内是 `sroa` / `early-cse` / `instcombine` / `simplifycfg` / `reassociate` / `sccp` /
`bdce` / `adce` / `memcpyopt`。循环只有 `licm` / `loop-rotate` / `loop-distribute` /
`loop-load-elim` / `loop-sink`；`loop-unroll<O1>` 只展开带 pragma/metadata 的循环，
`loop-vectorize` 处于 `vectorize-forced-only`，等于不自动向量化。

**`-O2`**：在 `-O1` 之上新增 `gvn`、`dse`、`aggressive-instcombine`、
`correlated-propagation`、`constraint-elimination`、`jump-threading`、`dfa-jump-threading`、
`jump-table-to-switch`、`mldst-motion`、`extra-simple-loop-unswitch-passes`、
`slp-vectorizer`、`speculative-execution`、`openmp-opt-cgscc`、`move-auto-init`；同时解除
`vectorize-forced-only` 限制开始真正的循环向量化，`loop-unroll` 升到 `O2` 阈值，
`simplifycfg` 打开 `hoist-common-insts` / `sink-common-insts` / `switch-to-arithmetic`。

**`-O3`**：相对 `-O2` 只多三样——`argpromotion`（只被读取的参数改按值传递）、
`callsite-splitting`、`chr`（LLVM 23 起在 O3 加入的控制高度削减 pass）；`loop-unroll` 升到
`O3`，`simple-loop-unswitch` 允许对 `nontrivial` 条件做循环外提。

**`-Os`**：管道仍取 `default<O2>`，标量优化与 `-O2` 完全相同，但不进 `loop-unroll`、
`loop-unroll-full`、`loop-vectorize`、`slp-vectorizer` 与循环交错，机器级仍按 `Default`
建。体积收益来自「不做以空间换时间的变换」，而不是换一条更弱的管道。

管道内容可以直接用 LLVM 复现（与 novella 无关）：

```sh
opt -passes='default<O2>' -print-pipeline-passes -disable-output /dev/null
```

实测一份 `f64` 多项式累加循环（`--emit obj`）：`-O0` 1696 字节 / 114 行反汇编，
`-O1`~`-O3` 均 960 字节 / 51 行，`-Os` 896 字节 / 36 行；五档程序输出一致。

## 缓存

`target/.novella-cache/v2` 的请求摘要包含目标描述的规范化 JSON、指针位宽，以及条件
编译的完整视图（键值、flag、feature、`--cfg` 注入值、`--check-cfg` 白名单）。自定义
目标的任何字段变化或任意一条 `--cfg` 改动都会使缓存失效。

## 分层约束

`novella-target` 只依赖 `novella-core` 与 `novella-ast`，位于 sema 之上游、codegen
之下游：

```
core ← novella-target ← sema ← hir ← codegen ← cli
```

`novella-core` 的 `target/` 只保留与目标描述无关的值类型（`TargetTriple`、`Endian`、
`RelocMode`、`LinkerFlavor`、`EmitBackend`）与旧的 `BuildTarget` 枚举，避免与
`novella-target` 形成依赖环。`HirProgram` 同时携带 `spec: TargetSpec` 与
`target: BuildTarget`，后者只保留运行时与代码模型的合并结果供链接阶段使用。

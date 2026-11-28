# novella-cli 知识

## OVERVIEW
`novella` 二进制：项目发现、目标解析、分析调度、缓存复用、原生链接与 `--print` 查询。

## STRUCTURE
```
src/
├── cli.rs            # clap 定义：new / build / check / compile
├── main.rs           # run() 分发，render_diagnostics，emit 分派
├── build.rs          # 项目构建：缓存 → 分析 → HIR → 目标文件 → 链接
├── config.rs         # CLI 覆盖项与 nov.toml 合并；-Z custom-targets
├── link.rs           # 链接驱动校验与可执行文件写出
├── print.rs          # --print 纯查询，不要求入口文件
├── project.rs        # 项目清单/入口/输出路径
├── error.rs
└── cache/            # mod.rs(身份+lookup) / inputs.rs / record.rs
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 子命令与 emit 分派 | `src/cli.rs`, `src/main.rs` | ast 在 HIR 前处理；obj/exe 走 codegen |
| 目标选择优先级 | `src/config.rs` | `--target` > 清单 `build.target` > 内置默认；自定义描述需 `-Z custom-targets` |
| 缓存身份 | `src/cache/mod.rs` | 编译器/LLVM 二进制、目标描述、`--cfg`、输入内容 |
| 缓存失效与校验 | `src/cache/lookup()` | 版本/身份/校验和/输入摘要逐项校验 |
| 链接规则 | `src/link.rs` | 裸机必须指定 entry 与 linker-script，使用 ELF 链接器 |
| 项目约定 | `src/project.rs` | `nov.toml` + `src/main.nov` + `target/<包名>` |

## CONVENTIONS
- 缓存不可用只告警并退化为正常编译，不阻断构建。
- 目标派生值与 `--cfg` 注入值都进身份摘要；改 `--cfg` 不得复用旧缓存。
- 清单存在但不可读或解析失败时不回退上层项目。
- 额外链接输入必须是同架构可重定位 ELF64；禁止共享库、归档与可执行文件混用。

## ANTI-PATTERNS (THIS PROJECT)
- 不可发射目标不得声明 linker 参数，链接层不得猜测其平台。
- 裸机输出必须是静态 ELF 可执行文件，不能用编译器驱动代替 ELF 链接器。
- 不得用 `--print` 触发编译或要求入口文件。
- 不得静默吞掉缓存校验失败；命中路径必须重放缓存的诊断。

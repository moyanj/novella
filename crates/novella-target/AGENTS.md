# novella-target 知识

## OVERVIEW
目标描述、条件编译 cfg 与 feature 体系：内置/自定义目标走同一解析路径，`TargetSpec::validate` 是唯一校验源。

## STRUCTURE
```
src/
├── spec.rs       # TargetSpec 字段与 validate / platform / build_target
├── cfg.rs        # CfgOptions / CfgValues；from_spec 派生 target_* 谓词
├── feature.rs    # feature 解析：负号与分隔符名字在解析期拒绝
├── loader.rs     # 自定义目标描述加载
├── selector.rs   # 内置/清单/文件路径选择器
├── resolved.rs   # 清单 build 配置与目标合并
├── query.rs      # --print target-list / target-spec-json
└── builtin/      # base.json + 两个内置目标 JSON + merge.rs / mod.rs
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 描述字段与校验 | `src/spec.rs` | data-layout 字节序、pointer-width、linker 与 emit 一致性 |
| cfg 谓词派生 | `src/cfg.rs::from_spec` | `target_env` 缺省为空串，保留 rustc 语义 |
| 内置目标清单 | `src/builtin/*.json`, `builtin/mod.rs` | 枚举与内置清单必须一一对应 |
| 模板合并 | `src/builtin/merge.rs` | null 覆盖层表示「不改变模板值」 |
| 目标回读 | `src/query.rs`, `spec.rs` | `--print target-spec-json` 必须可被 `--target <file>` 原样回读 |
| 自定义目标准入 | `src/selector.rs` | CLI 需 `-Z custom-targets` |

## CONVENTIONS
- 下游（sema/HIR/codegen/link）只调用 `TargetSpec::validate`，不各自重写校验。
- 不可发射目标返回 `emit = None`，`platform()` 报错而非猜测。
- 内置目标与自定义目标共用一条解析路径；`null` 只表示模板值不变，不表示清空。

## ANTI-PATTERNS (THIS PROJECT)
- 自定义目标若省略 `emit`，`platform()` 必须报错，不得回退到内置平台。
- 不得让内置平台枚举与 JSON 清单失配；测试断言两者一一对应。
- 不得在 feature 解析期静默忽略带分隔符或负号的名字。
- 不得在下游复制目标平台判断逻辑。

# novella-sema 知识

## OVERVIEW
语义分析 crate：加载所有可达模块、定型、产出只读 `SemaResult`；公开面仅入口与结果类型族。

## STRUCTURE
```
src/
├── analysis.rs       # analyze / analyze_with_target / analyze_with_options，阶段编排
├── api.rs            # SemaResult / DiagnosticReport / BindingTable 等公开结果
├── state.rs, symbol.rs, declarations.rs, traits.rs, instances.rs, dependencies.rs, freestanding.rs, cfg.rs
├── module/           # loader / resolve_path / std_manifest：模块加载与 std 分层
├── resolve/          # collect / imports / reexport / types：顶层声明与导入绑定
├── check/            # type_defs 等：字段/方法签名与声明级检查
├── typecheck/        # body / expr / unsafe_rules 等：函数体类型检查
├── ty/               # 类型关系、泛型、字面量定型
└── tests.rs          # 单元测试（tests/ 目录为空）
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 分析入口与阶段顺序 | `src/analysis.rs`, `src/lib.rs` | Phase 0 → 2d，取消检查贯穿各轮 |
| 模块加载与循环导入 | `src/module/loader.rs` | worklist；`std_manifest.rs` 读分层清单 |
| 导入绑定与重导出 | `src/resolve/imports.rs`, `reexport.rs` | 中间轮次跳过未就绪目标，不立即报错 |
| 类型注册表 | `src/check/type_defs/` | 字段与方法签名先于函数体 |
| unsafe 规则 | `src/typecheck/unsafe_rules/` | NVL3004 |
| 公开结果视图 | `src/api.rs` | `DiagnosticReport` 分 modules/global |
| 标准库分层准入 | `src/module/std_manifest.rs`, `src/analysis.rs` | `@no_std` / `@no_alloc` 指令从入口读取 |

## CONVENTIONS
- 阶段顺序固定：Phase 0 `load_modules` → 1 `collect_module` → 2a `bind_imports` → 2b `build_type_defs` → 2c `resolve_class_parents` → 2d `check_module`。
- 加载器 `is_cancelled()` 在每轮、每模块检查；收敛后统一复核跳过的导入。
- 入口指令决定分析环境：`@no_std` 不注入 site，`@no_alloc` 隐含 `@no_std` 且仅允许 core 层，`@no_main` 关闭入口要求；`module_only` 继承裸机禁止标准库的语义。
- 除入口与结果类型族外一律 `pub(crate)`；新增公开符号前确认是否需要长期兼容。
- 词法/语法只在 `analyze` 内执行一次；`SemaResult.modules` 携带源码、token 流、AST。

## ANTI-PATTERNS (THIS PROJECT)
- 不得在 sema 内再调用 lexer/parser，或从 token 重建 AST。
- 不得让未解析符号、容错投影或联合类型流入 HIR。
- 不得把中间轮次「尚未就绪」当成最终诊断上报。
- 诊断断言不得使用消息文本；用 `DiagnosticCode`（NVL3xxx 语义段）。

# novella-hir 知识

## OVERVIEW
HIR 是 sema 之后、codegen 之前的定型中间表示；构造期 fail-fast，禁止携带任何未定型信息。

## STRUCTURE
```
src/
├── ty.rs             # 后端可见的 HirType 与 TypeView → HirType 穷尽转换
├── body.rs           # HirBlock / HirStmt / HirOperand 两级结构
├── item.rs           # HirProgram / HirModule / HirFunction
└── build/            # 唯一入口 build_hir；19 个降级文件，call/ 子目录
    ├── module.rs, types.rs, expr.rs, stmt.rs, lvalue.rs
    ├── operators.rs, call.rs, loops.rs, coalesce.rs, dynamic.rs
    ├── aggregates.rs, strings.rs, globals.rs, mono.rs, layout.rs
    ├── requirements.rs, context.rs, helpers.rs, error.rs
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| sema → HIR 入口 | `src/build/`（`build_hir`） | 消费 `SemaResult`，产出 `HirProgram` |
| 类型投影 | `src/ty.rs` | 穷尽匹配；未知类型直接构造失败 |
| 语句/指令降级 | `src/build/stmt.rs`, `expr.rs`, `lvalue.rs` | 解析期标记在此全部消除 |
| 方法体与声明 | `src/build/module.rs` | `finish_types` 只处理声明，方法体需单独降级 |
| 泛型单态化 | `src/build/mono.rs` | 多态递归可能产生新实例，队列必须有上界 |
| 运行时需求 | `src/build/requirements.rs` | 由真实指令、参数槽位与全局数据计算 |
| 目标契约 | `src/item.rs` | `spec` / `target` / `no_main` / `requirements` 随程序携带 |

## CONVENTIONS
- 只依赖 core + ast + sema；不出现 inkwell 或 LLVM 类型，LLVM 版本绑定只在 codegen。
- 类型一律为已解析 `HirType`；函数统一获得稳定 `HirFunctionId`（顶层用名字，方法/lambda 用属主+源码位置）。
- 入口 `main` 查找、运行时对象和 weak 字段的降级都在这里决定，不在 codegen 补救。
- 构造期遇到仍带语法上下文或未定型值，立即产出 `BuildError`，不猜测。

## ANTI-PATTERNS (THIS PROJECT)
- weak 字段禁止取字段地址或按裸载荷读写，必须走字段指令保留元数据。
- 不得让下游重建 `with_stack` / `in_match_subject` / `as` 消歧等解析状态。
- 不得让单态化队列无限增长。

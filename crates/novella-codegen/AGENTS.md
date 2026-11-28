# novella-codegen 知识

## OVERVIEW
唯一接触 LLVM/inkwell 的 crate：校验 HIR 与目标契约，发射 x86-64 Linux 或裸机 ELF 对象；链接由 CLI 负责。

## STRUCTURE
```
src/
├── lib.rs            # emit_artifact：目标校验、入口查找、符号与链接契约
├── identity.rs       # llvm_library_path；缓存身份依赖这里
├── abi.rs            # x86-64 C ABI 窄整数扩展
├── target.rs         # 后端实际引入的 runtime 调用 → 链接契约
├── globals.rs, operators.rs, ownership.rs
└── body/             # arrays / calls / control / dynamic / instructions / ownership
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 发射总入口 | `src/lib.rs::emit_artifact` | 先 `target.validate()` 与 `spec.validate()`，再比平台一致性 |
| 入口 main 规则 | `src/lib.rs::entry` | 入口模块必须无参 `main`，返回 int 或 void |
| 符号/链接名冲突 | `src/lib.rs` 函数声明循环 | extern 不能带导出名；禁止 `__novella_drop_` / `llvm.` 保留前缀 |
| ABI 一致性 | `src/abi.rs` | 声明与调用点必须一致 |
| 对象与析构所有权 | `src/body/ownership.rs` | class 析构接收者不得复活自身强引用 |
| runtime 需求 | `src/target.rs` | HIR 需求之外，后端引入的调用也要纳入链接契约 |
| LLVM 动态库路径 | `src/identity.rs` | 编译器缓存身份依赖该路径的哈希 |

## CONVENTIONS
- `TARGET_TRIPLE` 只代表当前原生目标；缓存身份、链接驱动与发射必须一致。
- 外部声明与 HIR `link_name` / `is_extern` / `export_name` 三者必须自洽，冲突立即报错。
- 多模块可声明同一个 C 符号；同一符号的非外部定义只能有一份。
- 裸机发射使用 spec 自带的 LLVM 特性串，不在此另造禁用向量寄存器的开关。

## ANTI-PATTERNS (THIS PROJECT)
- 不得让 LLVM/inkwell 类型出现在前端或 HIR。
- 不得在 `emit = None` 的目标上发射代码或猜平台。
- 不得把 extern 与 export_name 叠加，也不得使用保留符号前缀。
- 不得声称已支持指针 → 整数转换；当前 `compile` 会明确报「暂不支持」。

# novella-core 知识

## OVERVIEW
跨阶段通用类型：Span/Token/Diagnostic、行表、模块路径、项目清单、目标平台；各阶段只依赖本 crate。

## STRUCTURE
```
src/
├── diagnostic.rs     # Severity / DiagnosticCode / Diagnostic 与 rustc 风格渲染数据
├── token.rs          # TokenKind / Token（kind + span + slice + line/column）
├── path.rs           # 模块键归一化
├── line_index.rs     # 字节偏移 ↔ 行列（LSP 复用）
├── project_config.rs # nov.toml 中的 build 配置与解析结果
├── target.rs         # TargetPlatform / BuildTarget / RuntimeMode / CodeModel
├── keywords/         # 预定义类型与关键字种子表
├── decor.rs, string.rs
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 诊断错误码分段 | `src/diagnostic.rs` | NVL1xxx 词法 / 2xxx 语法 / 3xxx 语义 / 9xxx 工具链 |
| 模块唯一键 | `src/path.rs` | `super:` 含 `..`，必须先归一化再比较 |
| 行列换算 | `src/line_index.rs` | LSP 的 UTF-16 列在外层 `novella-lsp/src/source.rs` |
| 平台与默认目标 | `src/target.rs` | 仅 Linux x86-64 与裸机 x86-64；默认值由平台决定 |
| 关键字种子表 | `src/keywords/mod.rs` | 新增预定义类型必须同步 |
| 目标 cfg 校验 | `src/target.rs::validate_cfg` | 条件编译不得伪装成另一发射目标 |

## CONVENTIONS
- 错误码稳定、消息文本可变；文档跳转、quickfix 与测试断言只用 `DiagnosticCode`。
- 任何「这个文件属于哪个模块」的判断都经 `module_key`，不直接比较原始路径字符串。
- `BuildTarget` 是已解析的唯一目标配置，直接传给 HIR、发射与缓存；`validate()` 只校验 kernel 代码模型与平台匹配。

## ANTI-PATTERNS (THIS PROJECT)
- 不得按诊断消息文本做匹配或断言。
- 不得把未归一化的 `..` 路径当模块身份。
- 新增预定义类型不得绕过关键字表，否则词法无法识别。
- 未实现的 triple 必须显式拒绝，不得静默回退宿主平台。

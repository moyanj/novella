# novella-parser 知识

## OVERVIEW
手写递归下降解析器：只消费 `Token`/`TokenKind`，语法错误就近记录并继续；输出 `ParseResult`。

## STRUCTURE
```
src/
├── lib.rs            # Parser / ParseResult / ParserInput，游标与诊断入口
├── preprocess.rs     # token 流一次性重写（from/import 降级、>> 拆分）
├── recursion.rs      # 分段栈与 MAX_STACK_SEGMENTS
├── expression/       # primary / postfix / lambda / match_expr 等
└── stmt.rs, types.rs
tests/
└── boundary_regression.rs         # E-01…E-05 边界回归
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 游标与前瞻 | `src/lib.rs` | `peek` / `peek_n` / `advance` / `expect` |
| 歧义 token 重写 | `src/preprocess.rs`, `src/lib.rs::split_token_pair` | 只改 `kind`，`slice` / `span` / 行列保持原样 |
| 栈安全 | `src/recursion.rs` | `guarded` 成对增减；分段大小须显著大于红区 |
| 表达式/语句边界 | `src/expression/`, `src/stmt.rs` | 状态字段：`with_stack` / `with_shield` / `inside_match` / `in_match_subject` |
| 语法边界回归 | `tests/boundary_regression.rs` | 按 E 编号组织，不按消息文本 |
| 文法同步 | `../../grammar/*.g4` | TokenKind / AST 变更后同步 |

## CONVENTIONS
- 所有语法错误进 `diagnostics`，不中断解析；风格与 lexer 的「可恢复错误只进 errors」一致。
- 递归深度不是闸门：栈安全由分段栈负责，`MAX_STACK_SEGMENTS` 兜底病态输入。
- `as` 消歧依赖上下文状态：with 表头、括号屏蔽、类型位容许箭头各自独立。
- `match` 主体后的 `{` 留给 match 块；`default` 必须在末尾。

## ANTI-PATTERNS (THIS PROJECT)
- 预处理 token 时不得改动 `slice` / `span` / 行列；JSON 输出必须反映真实输入。
- 不得用递归深度阈值拒绝合法深嵌套输入。
- 不得只在 `grammar/*.g4` 改语法而不改代码：文法是投影，不是事实源。
- 不得把解析期状态字段带到 sema 或 HIR。

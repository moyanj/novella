# novella-lsp 知识

## OVERVIEW
stdio 语言服务器：词法级能力复用 lexer，语法/语义级能力复用 parser 与 sema；公开面只有 `server::run`。

## STRUCTURE
```
src/
├── main.rs           # 可执行入口；stdout 锁定为协议通道，日志走 stderr
├── protocol.rs       # Content-Length 报文编解码
├── source.rs         # 字节偏移 ↔ UTF-16 行列的唯一换算层
├── document.rs       # 单文档缓存：文本、词法、语法、结构、偏移索引
├── semantic.rs, highlight.rs, diagnostics.rs, nav.rs
├── outline/          # AST → 符号树与折叠区间
├── features/         # 内部结构 → lsp-types，按能力一文件
└── server/           # dispatch / capabilities / lexical / syntax / navigation / sync / query / adapter / run
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 三线程状态机与版本门控 | `src/server/run.rs` | 接收线程读帧、状态线程管输出、工作线程跑分析 |
| 能力声明 | `src/server/capabilities.rs` | 未实现方法按 JSON-RPC 返回 -32601 |
| 诊断推送与清空 | `src/server/sync.rs`, `diagnostics.rs` | 关闭文档必须清空旧诊断 |
| 语义高亮 | `src/semantic.rs` | `TokenKind` → semantic token，与 lexer 同源 |
| 偏移换算 | `src/source.rs` | 中文与 emoji 按 UTF-16 码元计列 |
| 跳转/引用/重命名 | `src/nav.rs` | 数据来自 `SemaResult` 绑定表与类型摘要 |
| 悬浮/补全等转换 | `src/features/` | 一能力一文件，不改内部结构 |

## CONVENTIONS
- 日志统一用 `eprintln!`；无参数启动等待 stdio 属正常。
- 新编辑协作取消旧分析：仅状态线程接受的 revision 使旧结果过期，倒序版本不饿死有效结果。
- 导航在文档版本尚未定型时返回空结果，不使用旧 span。
- 语法错误不阻断快照构建：边打字边可用。
- 扩展侧只需启动本服务器；词法规则不在 VS Code 插件里维护。

## ANTI-PATTERNS (THIS PROJECT)
- 不得把日志、panic 输出或调试文本写到 stdout。
- 不得用旧 revision 的 span 回答当前版本请求。
- 不得在 LSP 内实现第二套词法/关键字规则。
- 不得将内部模块提升为公开 API：逐文件拆细的结构不是稳定契约。

# PROJECT KNOWLEDGE BASE

**Generated:** 2026-11-27

## OVERVIEW
Novella 是一门系统编程语言：TypeScript 风格语法、Rust 风格内存模型、直接发射原生机器码。本仓库是 Rust workspace（edition 2024，13 个 crate），前端手写 lexer/parser，sema 定型后经 HIR 交给 LLVM 23.1（inkwell 动态加载）发射；原生目标仅 x86_64-unknown-linux-gnu 与 x86_64-unknown-none-elf。

## STRUCTURE
```
./
├── crates/          # 编译器各阶段；依赖方向 core → ast/lexer → parser → sema → hir → codegen
├── std/             # Novella 编写的标准库；分层见 std/modules.toml
├── examples/        # 可编译的 .nov 示例；string/arc/runtime 主题为主
├── grammar/         # ANTLR4 文法：以代码为基准对齐，非事实源
├── editors/vscode/  # 扩展（pnpm + esbuild）；词法高亮由生成的 TextMate 文件提供
├── docs/            # 设计文档与深度交接文档；reports/ 为分析快照
├── tools/           # hir_vm.py
└── Justfile         # 全部常用命令入口
```

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| 编译器总流程 | `docs/program-deep-dive.md` | 源码到 ELF 的完整旅程 |
| 语言设计（权威） | `docs/design.md` | 与代码冲突时以代码为准 |
| 已实现/未实现边界 | `docs/sema-coverage.md`, `ROADMAP.md` | check / HIR / 原生代码是三层能力 |
| CLI 子命令与构建 | `crates/novella-cli/src/{cli,main,build}.rs` | `new`/`build`/`check`/`compile` |
| 语义规则 | `crates/novella-sema/` | 分阶段管线，公开面只有入口+结果 |
| 目标描述与 cfg | `crates/novella-target/` | 单一校验源 |
| LSP 与编辑器 | `crates/novella-lsp/`, `editors/vscode/` | stdout 是协议通道 |
| 标准库分层 | `std/modules.toml`, `std/README.md` | core/alloc/std 三层 |
| 语法高亮生成 | `crates/novella-syntax-gen/` | 事实源是 lexer 声明，不是 TokenKind |

## CODE MAP
| Symbol | Type | Location | Refs | Role |
|--------|------|----------|------|------|
| `Cli` / `run` | struct / fn | `crates/novella-cli/src/cli.rs`, `main.rs:37` | 1 | CLI 分发总入口 |
| `Cache::open` | fn | `crates/novella-cli/src/cache/mod.rs:72` | 2 | 缓存身份与复用判定 |
| `analyze_with_options` | fn | `crates/novella-sema/src/analysis.rs:95` | 多阶段内部调用 | 词法+语法+语义唯一入口 |
| `DiagnosticReport` | struct | `crates/novella-sema/src/api.rs:53` | 1 | 诊断归组（modules/global） |
| `HirProgram` | struct | `crates/novella-hir/src/item.rs:17` | codegen 消费 | 携带 spec/target/requirements |
| `build_hir` | fn | `crates/novella-hir/src/build/` | CLI | sema → HIR 唯一入口 |
| `emit_artifact` | fn | `crates/novella-codegen/src/lib.rs:326` | 1 | LLVM 校验并发射 ELF 对象 |
| `server::run` | fn | `crates/novella-lsp/src/server/run.rs:29` | 1 | LSP 三线程状态机 |
| `TargetSpec` | struct | `crates/novella-target/src/spec.rs:26` | 22 | 目标描述与 `validate` 唯一校验源 |
| `DiagnosticCode` | enum | `crates/novella-core/src/diagnostic.rs:39` | 跨阶段 | 稳定错误码 NVL1xxx–NVL9xxx |
| `RawToken` | enum | `crates/novella-lexer/src/lib.rs:13` | 2 | 所有词法规则的唯一事实源 |

## CONVENTIONS
- `*.nov` 注释、Rust doc 与提交信息统一中文；面向人类的文字逐词压缩，不写溢美之词。
- 事实源单向：lexer 声明 → TextMate 语法；TokenKind/AST 变更 → 同步 `grammar/*.g4`；新增预定义类型 → 同步关键字表。
- 每条规则有唯一校验源：目标描述在 `TargetSpec::validate`，字节偏移换算在 `novella-lsp/src/source.rs`，模块键在 `novella-core/src/path.rs`。
- 诊断用稳定 `NVLxxxx` 错误码做断言、跳转和 quickfix；消息文本只面向人类。
- 未实现能力以代码为准：`docs/design.md` 与 `*deep-dive.md` 含早期草案，不能当作已落地承诺。

## ANTI-PATTERNS (THIS PROJECT)
- 下游不得重跑 lexer/parser：`SemaResult.modules` 已带源码、token 流与 AST。
- 前端/HIR 不出现 inkwell 或 LLVM 类型；LLVM 版本绑定只在 codegen。
- LSP stdout 只写协议帧，任何日志走 stderr。
- 不得把 `Unknown` / `External` / `Error` / 未定型字面量 / 联合类型 / 泛型形参带进 HIR。
- 不得按诊断消息文本做测试断言、编辑器跳转或 quickfix 匹配。
- 不可发射目标（`emit = None`）不得声明 linker 参数，也不得由下游猜测其平台。

## UNIQUE STYLES
- 语义分层：`class` 自动 ARC、`struct` 值拷贝；穿指针访问必须写 `unsafe { }`。
- 标准库分层：`@no_alloc` 仅 core，`@no_std` 允许 alloc（需 managed runtime），std 需宿主 libc。
- 构建缓存以编译器/LLVM 二进制、目标描述、`--cfg` 与全部输入内容做身份，目标是 `target/.novella-cache/v2`。
- 词法高亮双轨：冷启动用生成的 TextMate 文件，常驻由 LSP 语义 token 接管。

## COMMANDS
```bash
just check && just lint && just fmt-check   # 提交前必须通过
just test                                   # cargo test --workspace
just build                                  # cargo build --workspace
cargo build --release -p novella             # target/release/novella
just gen-syntax-check                       # 校验生成的 TextMate 语法未过期
just gen-syntax                             # 从 lexer 声明重新生成
just lsp-build && just lsp-pack             # 语言服务器与 .vsix
novella check  examples/native-main.nov
novella compile examples/native-main.nov -o hello --emit exe
```

## NOTES
- 原生构建要求 LLVM 23.1 动态库（含开发头文件）、`clang`；裸机链接另需 `ld.lld`。
- LSP stdout 为 `Content-Length` 报文通道；无参数启动等待 stdio 属正常。
- 仓库没有 `.github/workflows`；CI 检查靠 `Justfile` 手工/外部调用。
- `docs/` 下没有 `SUMMARY.md`；`just book`（mdbook）需先补该索引。
- 本知识库不采集提交历史；需要历史信息时直接询问维护者。

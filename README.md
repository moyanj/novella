# novella

novella 语言编译器。Rust workspace（edition 2024，Apache-2.0）。

`grammar/` 下为 ANTLR4 文法，**以代码为基准对齐**：词法面与 `TokenKind` / `novella-lexer`
逐条对应，语法面与 `novella-ast` 的节点定义对应；改代码后须同步文法
（对齐约定与校验方式见 [grammar/README.md](grammar/README.md)）。

## 当前状态

| 阶段 | Crate | 状态 |
|------|-------|------|
| 词法分析 | `novella-lexer` | ✅ 可用：产出 token 流与结构化词法错误 |
| AST | `novella-ast` | 🚧 部分落地 |
| 语法分析 | `novella-parser` | 🚧 骨架，逐条语句实现中 |
| 语义分析 | `novella-sema` | ⬜ 占位 |
| 代码生成 | `novella-codegen` | ⬜ 占位 |

## 目录结构

```
crates/
├── novella-core/     # 共享基础：Span / TokenKind / Token / Diagnostic
├── novella-derive/   # proc-macro：#[derive(Spanned)]
├── novella-ast/      # AST 节点定义
├── novella-lexer/    # 词法器（logos）
├── novella-parser/   # 手写递归下降解析器
├── novella-sema/     # 语义分析（占位）
├── novella-codegen/  # 代码生成（占位）
├── novella-cli/      # 命令行入口（二进制名 `novella`）
└── novella-lsp/      # 语言服务器（薄 LSP，供编辑器语法高亮）
editors/vscode/       # VS Code 扩展
grammar/              # ANTLR4 文法参考
```

依赖链自上而下单向：`core ← ast ← parser ← sema ← codegen ← cli`，
`core ← lexer ← parser`。

## 常用命令

```bash
just check       # cargo check --workspace --all-targets
just build       # cargo build --workspace
just test        # cargo test --workspace
just lint        # clippy --workspace --all-targets -D warnings
just fmt         # cargo fmt --all

# 编译源文件（当前产出 tokens / AST / 诊断 的 JSON）
cargo run -p novella -- compile <file> -o out.json
```

## 编辑器支持

[`editors/vscode/`](editors/vscode/) 提供 VS Code 扩展。语法高亮由**薄 LSP 服务器**
`novella-lsp` 通过 semantic tokens 提供，它直接复用编译器自身的词法器——
因此染色结果与编译器对源码的切分永远一致，不存在两套词法规则漂移的问题。

```bash
just lsp-build   # 构建语言服务器（release）
just lsp-run     # 本地运行（stdio 通道）
just lsp-pack    # 打包 VS Code 扩展（生成 vsix）
```

安装与配置详见 [editors/vscode/README.md](editors/vscode/README.md)。

当前仅实现词法级高亮（注释 / 关键字 / 数字 / 字符串 / 运算符）；
标识符的语义分类需要 parser 与语义分析落地后再扩展。

## 文档

- [AGENTS.md](AGENTS.md)：项目结构、编码约定与防错清单

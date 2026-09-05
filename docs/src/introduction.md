# Novella

一个从零开始的手写 Rust 编译器项目(工作名 Novella),以 Cargo workspace 组织,采用传统的分层编译管线。

## Workspace 结构

```
novella/
├── Cargo.toml
├── Justfile
├── docs/                  # 本书(mdbook)
└── crates/
    ├── novella-core/      # 共享类型:Token、Span、AST、Diagnostic
    ├── novella-lexer/     # 词法分析:源码 → Token 流
    ├── novella-parser/    # 语法分析:Token 流 → AST
    ├── novella-sema/      # 语义分析:类型检查、作用域解析
    ├── novella-codegen/   # 代码生成:AST → 目标代码
    └── novella-cli/       # 命令行入口,串联整个编译管线
```

依赖方向:crates 自下而上单向依赖,`novella-cli` 依赖其余全部五个 crate。

## 常用命令

```bash
just check       # cargo check --workspace --all-targets
just build       # cargo build --workspace
just test        # cargo test --workspace
just lint        # cargo clippy --workspace --all-targets -- -D warnings
just fmt         # cargo fmt --all
just doc         # cargo doc(生成 rustdoc)
just book        # 构建本书(mdbook)
just book-serve  # 本地预览本书:http://localhost:3000
```

## 文档规划

- 本目录(本书)记录设计文档、架构决策与开发进度
- 各 crate 的 API 文档由 rustdoc(`just doc`)生成
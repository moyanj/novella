# novella

novella 语言编译器。Rust workspace（edition 2024，Apache-2.0）。

`grammar/` 下为 ANTLR4 文法，**以代码为基准对齐**：词法面与 `TokenKind` / `novella-lexer`
逐条对应，语法面与 `novella-ast` 的节点定义对应；改代码后须同步文法
（对齐约定与校验方式见 [grammar/README.md](grammar/README.md)）。

## 当前状态

| 阶段 | Crate | 状态 |
|------|-------|------|
| 词法分析 | `novella-lexer` | ✅ 可用：产出 token 流与结构化词法错误 |
| AST | `novella-ast` | ✅ 可用：文法全部产生式均能产出节点 |
| 语法分析 | `novella-parser` | ✅ 可用；完整语法演示含尚未实现的语义 |
| 语义分析 | `novella-sema` | ✅ 第一期：导入系统 / 名称解析 / 基本类型检查 / unsafe 规则 |
| HIR | `novella-hir` | ✅ 将已通过静态检查的程序降级为定型指令 |
| 代码生成 | `novella-codegen` | Linux x86-64 原生程序与无 libc 的 x86-64 裸机/内核目标；见 [裸机支持](docs/freestanding.md) |

## 目录结构

```
crates/
├── novella-core/     # 共享基础：Span / TokenKind / Token / Diagnostic
│                     #   + line_index / path / decor（行列、模块路径、诊断装饰）
├── novella-derive/   # proc-macro：#[derive(Spanned)]
├── novella-ast/      # AST 节点定义（expr/ 目录 / stmt / types / program）
├── novella-lexer/    # 词法器（logos）
├── novella-parser/   # 手写递归下降解析器（expression/ 目录）
├── novella-sema/     # 语义分析（第一期；module/ check/ typecheck/）
├── novella-hir/      # 定型中间表示
├── novella-codegen/  # 代码生成（LLVM）
├── novella-cli/      # 命令行入口（二进制名 `novella`）
├── novella-lsp/      # 语言服务器（outline/ features/ server/）
└── novella-syntax-gen/ # TextMate 语法生成器（TokenKind → vscode 扩展）
editors/vscode/       # VS Code 扩展
grammar/              # ANTLR4 文法参考
```

依赖链自上而下单向：`core ← ast ← parser ← sema ← hir ← codegen ← cli`，
`core ← lexer ← parser`。

## 常用命令

```bash
just check       # cargo check --workspace --all-targets
just build       # cargo build --workspace
just test        # cargo test --workspace
just lint        # clippy --workspace --all-targets -D warnings
just fmt         # cargo fmt --all

# 不需要标准库；运行 ./a.out，退出码为 42
cargo run -p novella -- compile examples/native-main.nov
cargo run -p novella -- check examples/native-main.nov
cargo run -p novella -- compile examples/native-main.nov --emit hir -o out.hir.json
cargo run -p novella -- compile examples/native-main.nov --emit obj -o out.o
```

构建 `novella` 需 LLVM 23.1（`llvm-config` 可见，或通过 `LLVM_SYS_231_PREFIX`
指向安装前缀）；Linux 可执行文件需要 `clang` 与 GNU 链接工具链。裸机目标直接使用
ELF 链接器，显式提供入口、链接脚本与平台对象，不添加 CRT 或 libc；无堆及 managed
两个内核和 QEMU 启动脚本见 [freestanding 示例](examples/freestanding/README.md)。
旧 JSON 消费者改用 `compile <file> --emit ast -o out.json`，成功时仍是
`tokens`、`program`、`diagnostics` 三个字段；失败时退出非零并保留已有输出。
`check` 不要求 `main`，通过检查不代表能生成 HIR 或原生代码。

## 创建与构建项目

```bash
novella new hello
cd hello
novella build
./target/hello
```

`new` 在不存在的目标目录中生成 `nov.toml`、`src/main.nov` 和 `.gitignore`，不覆盖现有目录。
目标目录名用作包名：须以 ASCII 字母或数字开头，其余字符可为 ASCII 字母、数字、`-` 或 `_`。
`build` 从当前目录向上查找最近的 `nov.toml`，读取其中的 `package.name`，编译该项目的
`src/main.nov`，并输出 Linux x86-64 可执行文件到项目根的 `target/<包名>`。
可用 `-o` 指定其他输出路径（相对路径以当前目录为基准），也可传入 `--std-root` 和 `--feature`。
单文件编译继续使用 `novella compile <file>`，不需要项目清单。

### 编译缓存

`novella build` 默认把成功生成的 LLVM 目标文件缓存到项目根的
`target/.novella-cache/v2/`。输入未变化时跳过词法、语法、语义分析、HIR 降级和 LLVM 发射，
重放编译诊断，并重新链接到指定的输出路径。链接失败仍保留已有可执行文件。

```bash
novella build --verbose    # 查看命中、失效原因及校验、编译、写入、链接耗时
novella build --no-cache   # 同时禁止读取和写入缓存，用于对照与排障
```

缓存按内容校验源码和传递依赖，同时追踪 crate 根清单的存在性与模块路径解析结果。
编译器、实际加载的 LLVM 文件、目标配置、features 或项目清单变化都会使对应记录失效。
不同输出路径可以复用同一目标文件；缓存损坏或不可用时会报告并继续正常编译。
缓存仅在本地使用，不承诺跨机器或项目路径共享；校验工具链文件本身有开销，
极小程序使用缓存不一定更快。`compile`、`check` 和中间产物导出不使用此缓存。
实现边界见 [编译缓存](docs/compile-cache.md)。

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

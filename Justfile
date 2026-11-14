# novella —— cargo workspace 常用命令入口
set shell := ["bash", "-uc"]

# 可用命令列表
default:
    @just --list

# 检查所有 crate（含测试代码）
check:
    cargo check --workspace --all-targets

# 构建整个 workspace
build:
    cargo build --workspace

# 运行测试
test:
    cargo test --workspace

# 运行 clippy（严格模式）
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# 格式化
fmt:
    cargo fmt --all

# 校验代码格式（CI 用）
fmt-check:
    cargo fmt --all --check

# 生成 rustdoc 文档
doc:
    cargo doc --workspace --no-deps --open

# 构建本书（mdbook）
book:
    mdbook build docs

# 本地预览本书（http://localhost:3000）
book-serve:
    mdbook serve docs

# 构建语言服务器（release，供编辑器连接）
lsp-build:
    cargo build --release -p novella-lsp

# 本地运行语言服务器（stdio 通道，启动后等待编辑器连接）
lsp-run:
    cargo run -p novella-lsp

# 由 novella-lexer 的 logos 声明生成 TextMate 语法（编辑器冷启动着色）
gen-syntax:
    cargo run -p novella-syntax-gen

# 校验生成的语法文件是否最新（CI 用；过期则提示重新生成）
gen-syntax-check:
    cargo run -p novella-syntax-gen -- --check

# 打包 VS Code 扩展（esbuild 打包 + vsce 产出 vsix）
lsp-pack:
    cd editors/vscode && pnpm install && pnpm run package
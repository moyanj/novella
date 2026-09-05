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
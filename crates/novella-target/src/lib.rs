//! 目标描述（TargetSpec）、条件编译配置（CfgOptions）与 feature 体系。
//!
//! 分层：本 crate 依赖 `novella-core` 与 `novella-ast`，不依赖 lexer/parser/sema。
//! 内置目标与自定义目标描述走同一条解析路径，保证 `--print target-spec-json`
//! 的输出可被 `--target <file.json>` 原样回读。

pub mod builtin;
pub mod cfg;
pub mod feature;
pub mod loader;
pub mod query;
pub mod resolved;
pub mod selector;
pub mod spec;

pub use cfg::{CfgExtra, CfgOptions, CheckCfg};
pub use feature::FeatureSet;
pub use query::PrintKind;
pub use resolved::resolve_build_config;
pub use selector::{ResolveError, resolve_builtin, resolve_selector};
pub use spec::{ArchiveKey, TargetSpec};

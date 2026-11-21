//! 缓存清单只保存成功产物与结构化诊断，不序列化编译器内部状态。

use novella_core::diagnostic::Diagnostic;
use novella_sema::SemaResult;
use serde::{Deserialize, Serialize};

use super::inputs::Input;

#[derive(Serialize, Deserialize)]
pub(super) struct Envelope {
    pub checksum: String,
    pub record: Record,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Record {
    pub version: u32,
    pub request: String,
    pub compilation: String,
    pub inputs: Vec<Input>,
    pub object: String,
    pub diagnostics: Vec<CachedDiagnostic>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct CachedDiagnostic {
    diagnostic: Diagnostic,
    source: Option<String>,
}

impl CachedDiagnostic {
    pub(super) fn collect(result: &SemaResult) -> Vec<Self> {
        let mut diagnostics: Vec<_> = result
            .diagnostics
            .global
            .iter()
            .map(|diagnostic| Self {
                diagnostic: diagnostic.clone(),
                source: None,
            })
            .collect();
        for (id, module) in result.diagnostics.modules.iter().enumerate() {
            for diagnostic in module.ordered() {
                diagnostics.push(Self {
                    diagnostic: diagnostic.clone(),
                    source: result.modules.get(id).map(|module| module.source.clone()),
                });
            }
        }
        diagnostics
    }

    pub(super) fn render(&self) {
        eprintln!(
            "{}",
            crate::error::render_diagnostic(&self.diagnostic, self.source.as_deref())
        );
    }
}

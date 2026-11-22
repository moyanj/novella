//! 不可变分析任务：状态线程构造输入，工作线程运行，版本一致后原子提交结果。
use super::Server;
use novella_sema::{AnalysisOptions, FileLoader, OverlayLoader, ParsedSource, SemaResult};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct AnalysisJob {
    pub generation: u64,
    pub cancelled: Arc<AtomicBool>,
    overlay: HashMap<PathBuf, String>,
    parsed: HashMap<PathBuf, ParsedSource>,
    entries: Vec<PathBuf>,
    options: AnalysisOptions,
    cfg_overrides: [Option<String>; 3],
}

impl Server {
    pub(super) fn analysis_job(&mut self, cancelled: Arc<AtomicBool>) -> AnalysisJob {
        self.analysis_requested = false;
        let overlay = self
            .documents
            .values()
            .filter_map(|doc| {
                doc.path
                    .as_ref()
                    .map(|path| (PathBuf::from(path), doc.text.clone()))
            })
            .collect();
        let parsed = self
            .documents
            .values()
            .filter_map(|doc| {
                let path = PathBuf::from(doc.path.as_ref()?);
                let mut diagnostics = doc.lex_diagnostics();
                diagnostics.extend(doc.parsed.diagnostics.clone());
                Some((
                    path,
                    ParsedSource {
                        source: doc.text.clone(),
                        tokens: doc.lexed.tokens.clone(),
                        program: doc.parsed.program.clone(),
                        syntax_failed: !diagnostics.is_empty(),
                        diagnostics,
                    },
                ))
            })
            .collect();
        AnalysisJob {
            generation: self.generation,
            cancelled,
            overlay,
            parsed,
            entries: self.entries.clone(),
            options: self.options.clone(),
            cfg_overrides: self.cfg_overrides.clone(),
        }
    }
}

impl AnalysisJob {
    pub fn execute(self) -> Vec<SemaResult> {
        let mut roots = self.entries.clone();
        roots.extend(self.overlay.keys().cloned());
        roots.sort_by_key(|root| novella_sema::module_key(root));
        roots.dedup_by_key(|root| novella_sema::module_key(root));
        let mut results = Vec::new();
        for root in roots {
            if self.cancelled.load(Ordering::Relaxed) {
                break;
            }
            let mut options = self.options.clone();
            options.module_only = !self
                .entries
                .iter()
                .any(|entry| novella_sema::module_key(entry) == novella_sema::module_key(&root));
            let mut loader = CancellableLoader {
                inner: OverlayLoader::new(self.overlay.clone()).with_parsed(self.parsed.clone()),
                cancelled: Arc::clone(&self.cancelled),
            };
            let config_error = configure_project(&root, &mut options, &self.cfg_overrides).err();
            let mut analysis = novella_sema::analyze_with_options(&mut loader, &root, options);
            if let Some(error) = config_error {
                analysis
                    .diagnostics
                    .global
                    .push(novella_core::diagnostic::Diagnostic::error(error));
            }
            if self.cancelled.load(Ordering::Relaxed) {
                break;
            }
            results.push(analysis);
        }
        results
    }
}

/// 每个分析根使用自己的清单，避免多项目工作区互相覆盖目标配置。
fn configure_project(
    root: &Path,
    options: &mut AnalysisOptions,
    cfg: &[Option<String>; 3],
) -> Result<(), String> {
    use novella_core::project_config::{BuildConfig, load_project_config};
    let target = if let Some(project) = load_project_config(root)? {
        project
            .build
            .resolve(&BuildConfig::default(), &project.root)?
            .target
    } else {
        Default::default()
    };
    target.validate_cfg(cfg[0].as_deref(), cfg[1].as_deref(), cfg[2].as_deref())?;
    options.build_target = target;
    options.target.pointer_width = novella_sema::PointerWidth::Bits64;
    options.cfg.target_os = target.os().into();
    options.cfg.target_arch = target.arch().into();
    options.cfg.target_family = target.family().into();
    Ok(())
}

struct CancellableLoader {
    inner: OverlayLoader,
    cancelled: Arc<AtomicBool>,
}
impl FileLoader for CancellableLoader {
    fn read_to_string(&mut self, path: &Path) -> std::io::Result<String> {
        self.inner.read_to_string(path)
    }
    fn exists(&mut self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn parsed_source(&mut self, path: &Path) -> Option<ParsedSource> {
        self.inner.parsed_source(path)
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

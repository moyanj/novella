//! 项目本地 LLVM 目标文件缓存：按输入内容校验，始终重新链接。

mod inputs;
mod record;

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use novella_sema::{AnalysisOptions, PointerWidth, SemaResult};
use record::{CachedDiagnostic, Envelope, Record};
use serde::Serialize;

pub(crate) use inputs::TrackingLoader;

const VERSION: u32 = 1;

#[derive(Serialize)]
struct Request<'a> {
    version: u32,
    compiler: String,
    llvm: String,
    host: [&'a str; 3],
    cwd: PathBuf,
    entry: &'a Path,
    target: &'a str,
    pointer_width: u8,
    cfg: (&'a str, &'a str, &'a str, &'a BTreeSet<String>),
    std_root: Option<&'a Path>,
    module_only: bool,
}

pub(crate) struct Cache {
    root: PathBuf,
    request: String,
}

pub(crate) struct Hit {
    pub object: Vec<u8>,
    diagnostics: Vec<CachedDiagnostic>,
}

pub(crate) enum Lookup {
    Hit(Hit),
    Miss(String),
}

impl Hit {
    pub(crate) fn render_diagnostics(&self) {
        for diagnostic in &self.diagnostics {
            diagnostic.render();
        }
    }
}

impl Cache {
    pub(crate) fn open(
        root: &Path,
        entry: &Path,
        options: &AnalysisOptions,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let compiler = std::env::current_exe()?;
        let llvm = novella_codegen::llvm_library_path()?;
        // 编译器摘要也覆盖当前固定的 generic CPU、PIC、优化等级等后端选项。
        // 所有可配置分析选项取自实际传入 sema 的配置，不在缓存内重算默认值。
        let identity = Request {
            version: VERSION,
            compiler: hash_file(&compiler)?,
            llvm: hash_file(&llvm)?,
            host: [
                std::env::consts::OS,
                std::env::consts::ARCH,
                std::env::consts::FAMILY,
            ],
            cwd: std::env::current_dir()?,
            entry,
            target: novella_codegen::TARGET_TRIPLE,
            pointer_width: match options.target.pointer_width {
                PointerWidth::Bits32 => 32,
                PointerWidth::Bits64 => 64,
            },
            cfg: (
                &options.cfg.target_os,
                &options.cfg.target_arch,
                &options.cfg.target_family,
                &options.cfg.features,
            ),
            std_root: options.std_root.as_deref(),
            module_only: options.module_only,
        };
        Ok(Self {
            root: root.join("target/.novella-cache/v1"),
            request: hash_bytes(&serde_json::to_vec(&identity)?),
        })
    }

    fn record_path(&self) -> PathBuf {
        self.root
            .join("records")
            .join(format!("{}.json", self.request))
    }

    /// 校验元数据、所有输入和目标文件；任何不一致都不能复用。
    pub(crate) fn lookup(&self) -> Result<Lookup, String> {
        let bytes = match fs::read(self.record_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Lookup::Miss(
                    "没有匹配当前编译器、目标和配置的构建记录".into(),
                ));
            }
            Err(error) => return Err(format!("无法读取构建记录：{error}")),
        };
        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|error| format!("构建记录损坏：{error}"))?;
        let record = envelope.record;
        let payload = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
        if record.version != VERSION
            || record.request != self.request
            || hash_bytes(&payload) != envelope.checksum
        {
            return Err("构建记录版本、身份或校验和不匹配".into());
        }
        let compilation = serde_json::to_vec(&(&self.request, &record.inputs))
            .map_err(|error| error.to_string())?;
        if hash_bytes(&compilation) != record.compilation {
            return Err("编译输入摘要不匹配".into());
        }
        for input in &record.inputs {
            if let Err(reason) = input.validate() {
                return Ok(Lookup::Miss(reason));
            }
        }
        // 先解析摘要，避免损坏的清单把对象路径指向缓存目录之外。
        let digest = blake3::Hash::from_hex(&record.object)
            .map_err(|error| format!("目标文件摘要损坏：{error}"))?;
        let object = fs::read(self.root.join("objects").join(digest.to_hex().as_str()))
            .map_err(|error| format!("无法读取缓存目标文件：{error}"))?;
        if blake3::hash(&object) != digest {
            return Err("缓存目标文件校验失败".into());
        }
        Ok(Lookup::Hit(Hit {
            object,
            diagnostics: record.diagnostics,
        }))
    }

    pub(crate) fn store(
        &self,
        loader: TrackingLoader,
        result: &SemaResult,
        object: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 编译期间输入发生变化时不发布混合快照；不以重新读取的内容替换实际编译输入。
        for input in &loader.inputs {
            input.validate().map_err(io::Error::other)?;
        }
        let compilation = hash_bytes(&serde_json::to_vec(&(&self.request, &loader.inputs))?);
        let record = Record {
            version: VERSION,
            request: self.request.clone(),
            compilation,
            inputs: loader.inputs,
            object: hash_bytes(object),
            diagnostics: CachedDiagnostic::collect(result),
        };
        let envelope = Envelope {
            checksum: hash_bytes(&serde_json::to_vec(&record)?),
            record,
        };
        atomic_write(
            &self.root.join("objects").join(&envelope.record.object),
            object,
        )?;
        // 目标文件先发布，构建记录最后发布；并发构建允许重复计算，不暴露半成品。
        atomic_write(&self.record_path(), &serde_json::to_vec(&envelope)?)?;
        Ok(())
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("缓存路径没有父目录"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        hasher.update(&buffer[..length]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

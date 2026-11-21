//! 记录编译实际观察的文件内容、存在性和模块身份，不重复解析导入。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use novella_core::path::module_key;
use novella_sema::FileLoader;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(super) enum Input {
    Source { path: PathBuf, digest: String },
    Exists { path: PathBuf, exists: bool },
    ModuleKey { path: PathBuf, key: PathBuf },
}

impl Input {
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Source { path, digest } => {
                let actual = super::hash_file(path)
                    .map_err(|error| format!("无法校验输入 '{}'：{error}", path.display()))?;
                if actual != *digest {
                    return Err(format!("文件内容变化：{}", path.display()));
                }
            }
            Self::Exists { path, exists } => {
                if path.exists() != *exists {
                    return Err(format!("路径存在性变化：{}", path.display()));
                }
            }
            Self::ModuleKey { path, key } => {
                if module_key(path) != *key {
                    return Err(format!("模块路径解析变化：{}", path.display()));
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct TrackingLoader {
    pub(super) inputs: Vec<Input>,
}

impl FileLoader for TrackingLoader {
    fn read_to_string(&mut self, path: &Path) -> io::Result<String> {
        let source = fs::read_to_string(path)?;
        self.inputs.push(Input::Source {
            path: path.to_path_buf(),
            digest: super::hash_bytes(source.as_bytes()),
        });
        Ok(source)
    }

    fn exists(&mut self, path: &Path) -> bool {
        let exists = path.exists();
        self.inputs.push(Input::Exists {
            path: path.to_path_buf(),
            exists,
        });
        exists
    }

    fn module_key(&mut self, path: &Path) -> PathBuf {
        let key = module_key(path);
        self.inputs.push(Input::ModuleKey {
            path: path.to_path_buf(),
            key: key.clone(),
        });
        key
    }
}

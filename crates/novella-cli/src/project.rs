//! 单可执行项目的清单发现、目录布局与初始化。

use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MANIFEST: &str = "nov.toml";
const SOURCE_DIR: &str = "src";
const ENTRY_FILE: &str = "main.nov";
const OUTPUT_DIR: &str = "target";

pub(crate) struct Project {
    root: PathBuf,
    name: String,
}

impl Project {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 使用最近的清单；清单存在但无法读取或解析时，不回退到上层项目。
    pub(crate) fn discover(start: &Path) -> Result<Self, Box<dyn Error>> {
        for root in start.ancestors() {
            let manifest = root.join(MANIFEST);
            match fs::symlink_metadata(&manifest) {
                Ok(_) => return Self::from_manifest(root, &manifest),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(path_error("无法检查项目清单", &manifest, error).into()),
            }
        }
        Err(format!("从 '{}' 向上未找到 {MANIFEST}", start.display()).into())
    }

    fn from_manifest(root: &Path, manifest: &Path) -> Result<Self, Box<dyn Error>> {
        let source = fs::read_to_string(manifest)
            .map_err(|error| path_error("无法读取项目清单", manifest, error))?;
        let table: toml::Table = toml::from_str(&source)
            .map_err(|error| format!("无法解析项目清单 '{}'：{error}", manifest.display()))?;
        let name = table
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("项目清单 '{}' 缺少字符串 package.name", manifest.display()))?;
        validate_name(name)
            .map_err(|error| format!("项目清单 '{}'：{error}", manifest.display()))?;
        Ok(Self {
            root: root.to_path_buf(),
            name: name.to_owned(),
        })
    }

    pub(crate) fn entry(&self) -> PathBuf {
        self.root.join(SOURCE_DIR).join(ENTRY_FILE)
    }

    pub(crate) fn default_output(&self) -> io::Result<PathBuf> {
        let directory = self.root.join(OUTPUT_DIR);
        fs::create_dir_all(&directory)
            .map_err(|error| path_error("无法创建构建目录", &directory, error))?;
        Ok(directory.join(&self.name))
    }
}

/// 仅在目标目录不存在时创建项目，不覆盖已有文件。
pub(crate) fn create(root: &Path) -> Result<(), Box<dyn Error>> {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("项目路径必须以有效的包名结尾")?;
    validate_name(name)?;
    fs::create_dir(root).map_err(|error| path_error("无法创建项目目录", root, error))?;
    let source_dir = root.join(SOURCE_DIR);
    fs::create_dir(&source_dir)
        .map_err(|error| path_error("无法创建源码目录", &source_dir, error))?;
    let manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
    write_new(&root.join(MANIFEST), &manifest)?;
    write_new(
        &source_dir.join(ENTRY_FILE),
        "func main() -> int {\n    return 0;\n}\n",
    )?;
    write_new(&root.join(".gitignore"), "/target/\n")?;
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    if bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(format!(
            "无效包名 '{name}'：须以 ASCII 字母或数字开头，且仅含 ASCII 字母、数字、连字符或下划线"
        ))
    }
}

fn write_new(path: &Path, contents: &str) -> io::Result<()> {
    File::create_new(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .map_err(|error| path_error("无法写入项目文件", path, error))
}

fn path_error(action: &str, path: &Path, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{action} '{}'：{error}", path.display()),
    )
}

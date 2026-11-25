use anyhow::{Result, anyhow, bail, ensure};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct LocalMemoryPolicy {
    pub namespace: String,
    pub distribution: String,
    pub python_executable: String,
    pub model_directory: PathBuf,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Disabled,
    BgeCpu,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default = "version")]
    protocol_version: u64,
    #[serde(default)]
    mode: Mode,
    namespace: Option<String>,
    distribution: Option<String>,
    python_executable: Option<String>,
    model_directory: Option<PathBuf>,
}
fn version() -> u64 { 1 }

impl LocalMemoryPolicy {
    pub fn load(root: &Path) -> Result<Option<Self>> {
        let path = root.join("runtime/local-memory-policy.json");
        if !path.exists() { return Ok(None); }
        ensure!(std::fs::metadata(&path)?.len() <= 4096, "本地记忆策略过大");
        let file: PolicyFile = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| anyhow!("本地记忆策略格式无效"))?;
        ensure!(file.protocol_version == 1, "不支持的本地记忆策略版本");
        if file.mode == Mode::Disabled { return Ok(None); }
        let mut policy = Self {
            namespace: file.namespace.ok_or_else(|| anyhow!("本地记忆缺少数据空间"))?,
            distribution: file.distribution.ok_or_else(|| anyhow!("本地记忆缺少 WSL 发行版"))?,
            python_executable: file.python_executable.ok_or_else(|| anyhow!("本地记忆缺少 Linux Python 路径"))?,
            model_directory: file.model_directory.ok_or_else(|| anyhow!("本地记忆缺少模型目录"))?,
        };
        policy.validate()?;
        policy.model_directory = std::fs::canonicalize(&policy.model_directory)?;
        Ok(Some(policy))
    }

    pub(super) fn validate(&self) -> Result<()> {
        ensure!(matches!(self.namespace.as_str(), "real" | "simulator"), "本地记忆数据空间无效");
        ensure!(!self.distribution.is_empty() && self.distribution.len() <= 80
            && !self.distribution.starts_with('-')
            && self.distribution.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "本地记忆 WSL 发行版无效");
        ensure!(linux_absolute(&self.python_executable), "本地记忆 Python 必须为 Linux 绝对路径");
        windows_path_argument(&self.model_directory)?;
        ensure!(self.model_directory.is_dir(), "本地记忆模型目录不存在");
        Ok(())
    }
}

pub(super) fn linux_absolute(value: &str) -> bool {
    value.starts_with('/') && !value.starts_with("//") && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && value.split('/').all(|part| part != "..")
}

/// wslpath does not interpret canonicalize's verbatim Windows prefix correctly.
pub(super) fn windows_path_argument(path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        ensure!(path.is_absolute(), "本地记忆需要本机 Windows 绝对路径");
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else { bail!("本地记忆路径缺少本机盘符") };
        match prefix.kind() {
            Prefix::Disk(_) => Ok(path.to_owned()),
            Prefix::VerbatimDisk(drive) => {
                let mut result = PathBuf::from(format!("{}:\\", char::from(drive.to_ascii_uppercase())));
                for component in components {
                    if component != Component::RootDir { result.push(component.as_os_str()); }
                }
                Ok(result)
            }
            _ => bail!("本地记忆不支持 UNC 或设备路径"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        bail!("本地记忆 WSL 拥有者需要 Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_and_disabled_policies_do_not_prepare_any_model() -> Result<()> {
        let root = tempfile::tempdir()?;
        assert!(LocalMemoryPolicy::load(root.path())?.is_none());
        std::fs::create_dir(root.path().join("runtime"))?;
        for value in ["{}", r#"{"protocol_version":1,"mode":"disabled"}"#] {
            std::fs::write(root.path().join("runtime/local-memory-policy.json"), value)?;
            assert!(LocalMemoryPolicy::load(root.path())?.is_none());
        }
        for value in [r#"{"mode":"bge_cpu"}"#, r#"{"mode":"gpu"}"#,
            r#"{"mode":"disabled","download":true}"#, r#"{"protocol_version":2}"#] {
            std::fs::write(root.path().join("runtime/local-memory-policy.json"), value)?;
            assert!(LocalMemoryPolicy::load(root.path()).is_err());
        }
        Ok(())
    }
    #[test]
    fn linux_paths_cannot_be_shell_commands_or_relative_paths() {
        assert!(linux_absolute("/mnt/c/AI Tutor/runtime/python/bin/python"));
        for value in ["python", "../python", "//server/python", "/tmp/../python", "/bin/python\n--other"] {
            assert!(!linux_absolute(value));
        }
    }
    #[cfg(windows)]
    #[test]
    fn wsl_paths_accept_local_drives_and_remove_only_verbatim_prefix() -> Result<()> {
        assert_eq!(windows_path_argument(Path::new(r"\\?\C:\AI Tutor\models"))?, PathBuf::from(r"C:\AI Tutor\models"));
        for value in [r"models\bge", r"\\server\share\bge", r"\\.\pipe\bge"] {
            assert!(windows_path_argument(Path::new(value)).is_err());
        }
        Ok(())
    }
    #[cfg(windows)]
    #[test]
    fn enabling_requires_explicit_namespace_and_local_model_directory() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("runtime"))?;
        let model = root.path().join("model"); std::fs::create_dir(&model)?;
        let value = serde_json::json!({"protocol_version":1,"mode":"bge_cpu","namespace":"simulator",
            "distribution":"Ubuntu","python_executable":"/opt/embedding/bin/python","model_directory":model});
        let path = root.path().join("runtime/local-memory-policy.json");
        std::fs::write(&path,value.to_string())?;
        assert_eq!(LocalMemoryPolicy::load(root.path())?.unwrap().namespace,"simulator");
        for (field, invalid) in [("namespace","all"),("distribution","--exec"),("python_executable","python"),("model_directory","relative")] {
            let mut wrong = value.clone(); wrong[field] = serde_json::json!(invalid);
            std::fs::write(&path,wrong.to_string())?;
            assert!(LocalMemoryPolicy::load(root.path()).is_err(),"{field}");
        }
        Ok(())
    }
}

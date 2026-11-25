//! Explicit local ASR preparation. No devices, descriptors or automatic retries.
//! Runtime installation is a future-capture client swap, not a capture restart.
mod owned;
use anyhow::{Result, anyhow, ensure};
pub(crate) use owned::OwnedAsrService;
use reqwest::{Client, RequestBuilder, Url};
use serde::Deserialize;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub(crate) const MODEL_DIRECTORY: &str =
    "models/speech/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AsrMode {
    #[default]
    Disabled,
    Managed,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalAsrPolicy {
    pub protocol_version: u64,
    #[serde(default)]
    pub mode: AsrMode,
    pub python_executable: Option<PathBuf>,
}
impl Default for LocalAsrPolicy {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            mode: AsrMode::Disabled,
            python_executable: None,
        }
    }
}
impl LocalAsrPolicy {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("runtime/local-asr-policy.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        ensure!(std::fs::metadata(&path)?.len() <= 4096, "本地识别策略过大");
        let mut policy: Self = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| anyhow!("本地识别策略格式无效"))?;
        ensure!(policy.protocol_version == 1, "不支持的本地识别策略版本");
        if policy.mode == AsrMode::Managed {
            let python = policy
                .python_executable
                .as_ref()
                .ok_or_else(|| anyhow!("请指定已安装的本地识别 Python 环境"))?;
            ensure!(
                python.is_absolute() && python.is_file(),
                "本地识别 Python 路径必须是现有绝对路径"
            );
            policy.python_executable = Some(std::fs::canonicalize(python)?);
        }
        Ok(policy)
    }
}

/// Immutable for one recording. Never serialize/debug the private credential.
pub(crate) struct AsrClient {
    pub instance_id: String,
    base_url: String,
    token: String,
    http: Client,
    failed: AtomicBool,
}
impl AsrClient {
    pub fn new(instance_id: String, base_url: String, token: String) -> Result<Self> {
        ensure!(
            instance_id.len() == 32 && instance_id.bytes().all(|b| b.is_ascii_hexdigit()),
            "本地识别实例无效"
        );
        let url = Url::parse(&base_url).map_err(|_| anyhow!("本地识别地址无效"))?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some_and(|v| v > 0)
                && url.path() == "/"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "识别服务只允许本机回环地址"
        );
        ensure!(
            (24..=256).contains(&token.len())
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "本地识别凭证无效"
        );
        Ok(Self {
            instance_id,
            base_url: base_url.trim_end_matches('/').into(),
            token,
            failed: AtomicBool::new(false),
            http: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_millis(500))
                .timeout(Duration::from_secs(2))
                .build()?,
        })
    }
    pub fn post(&self, path: &str) -> RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .header("X-ASR-Instance", &self.instance_id)
    }
    pub fn mark_unavailable(&self) {
        self.failed.store(true, Ordering::SeqCst);
    }
    pub fn unavailable(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }
    pub fn check_instance(&self, value: &Value) -> Result<()> {
        ensure!(
            value["protocol_version"] == 1 && value["instance_id"] == self.instance_id,
            "本地识别服务实例已变化"
        );
        Ok(())
    }
    pub async fn check_warm(&self) -> Result<()> {
        let mut response = self
            .http
            .get(format!("{}/health", self.base_url))
            .bearer_auth(&self.token)
            .header("X-ASR-Instance", &self.instance_id)
            .send()
            .await?;
        ensure!(response.status().is_success(), "本地识别服务未就绪");
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(bytes.len() + chunk.len() <= 4096, "本地识别状态过大");
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        self.check_instance(&value)?;
        ensure!(
            value["ready"] == true && value["warmup_completed"] == true,
            "本地识别模型尚未完成预热"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_policy_is_disabled_even_with_external_descriptor() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join("runtime"))?;
        std::fs::write(
            dir.path().join("runtime/speech-service.json"),
            b"external fixture",
        )?;
        assert_eq!(LocalAsrPolicy::load(dir.path())?.mode, AsrMode::Disabled);
        Ok(())
    }
    #[test]
    fn managed_policy_requires_explicit_absolute_interpreter() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join("runtime"))?;
        for value in [
            r#"{"mode":"managed"}"#,
            r#"{"protocol_version":1,"mode":"managed"}"#,
            r#"{"protocol_version":1,"mode":"managed","python_executable":"python.exe"}"#,
            r#"{"protocol_version":1,"mode":"external"}"#,
            r#"{"protocol_version":1,"mode":"managed","command":"arbitrary"}"#,
            r#"{"protocol_version":2,"mode":"disabled"}"#,
        ] {
            std::fs::write(dir.path().join("runtime/local-asr-policy.json"), value)?;
            assert!(LocalAsrPolicy::load(dir.path()).is_err());
        }
        std::fs::write(
            dir.path().join("runtime/local-asr-policy.json"),
            r#"{"protocol_version":1,"mode":"disabled"}"#,
        )?;
        assert_eq!(LocalAsrPolicy::load(dir.path())?.mode, AsrMode::Disabled);
        Ok(())
    }
    #[test]
    fn client_rejects_foreign_addresses_and_instance_receipts() -> Result<()> {
        let instance = "a".repeat(32);
        let token = "b".repeat(32);
        for address in [
            "http://localhost:18867",
            "https://127.0.0.1:18867",
            "http://127.0.0.1:18867/path",
            "http://127.0.0.1:18867/?token=bad",
            "http://user@127.0.0.1:18867",
        ] {
            assert!(AsrClient::new(instance.clone(), address.into(), token.clone()).is_err());
        }
        let client = AsrClient::new(instance.clone(), "http://127.0.0.1:49152".into(), token)?;
        assert!(
            client
                .check_instance(&serde_json::json!({"protocol_version":1,"instance_id":"old"}))
                .is_err()
        );
        client.check_instance(&serde_json::json!({"protocol_version":1,"instance_id":instance}))?;
        Ok(())
    }
}

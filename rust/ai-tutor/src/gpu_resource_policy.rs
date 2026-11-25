//! Explicit, versioned admission profile; it never starts a model.
use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GpuResourceMode {
    Exclusive,
    #[serde(rename = "qwen_pair_12gb_v1")]
    QwenPair12gbV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    protocol_version: u64,
    mode: GpuResourceMode,
}

pub(crate) fn load(root: &Path) -> Result<GpuResourceMode> {
    let path = root.join("runtime/gpu-resource-policy.json");
    if !path.exists() {
        return Ok(GpuResourceMode::Exclusive);
    }
    ensure!(
        std::fs::metadata(&path)?.len() <= 4096,
        "GPU resource policy exceeds its bound"
    );
    let policy: Policy = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| anyhow!("Invalid GPU resource policy"))?;
    ensure!(
        policy.protocol_version == 1,
        "Unsupported GPU resource policy version"
    );
    if policy.mode == GpuResourceMode::QwenPair12gbV1 {
        // These locks live inside one WSL distribution and its default user.
        // Separately configured speech/ASR distributions cannot share them.
        let read_peer = |name: &str| -> Result<Option<serde_json::Value>> {
            let path = root.join("runtime").join(name);
            if !path.exists() {
                return Ok(None);
            }
            ensure!(
                std::fs::metadata(&path)?.len() <= 4096,
                "Speech policy exceeds its bound"
            );
            Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
        };
        if let (Some(voice), Some(asr)) = (
            read_peer("local-speech-policy.json")?,
            read_peer("local-final-asr-policy.json")?,
        ) {
            if voice["mode"] == "managed" && asr["enabled"] == true {
                let left = voice["managed"]["distribution"].as_str().unwrap_or("");
                let right = asr["managed"]["distribution"].as_str().unwrap_or("");
                ensure!(
                    !left.is_empty() && left.eq_ignore_ascii_case(right),
                    "The shared GPU profile requires one WSL distribution for both models"
                );
            }
        }
    }
    Ok(policy.mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gpu_sharing_requires_the_named_profile() -> Result<()> {
        let root = tempfile::tempdir()?;
        assert_eq!(load(root.path())?, GpuResourceMode::Exclusive);
        std::fs::create_dir(root.path().join("runtime"))?;
        let path = root.path().join("runtime/gpu-resource-policy.json");
        for value in [
            r#"{"protocol_version":1,"mode":"shared"}"#,
            r#"{"protocol_version":2,"mode":"qwen_pair_12gb_v1"}"#,
            r#"{"protocol_version":1,"mode":"qwen_pair_12gb_v1","allow_any_model":true}"#,
        ] {
            std::fs::write(&path, value)?;
            assert!(load(root.path()).is_err());
        }
        std::fs::write(path, r#"{"protocol_version":1,"mode":"qwen_pair_12gb_v1"}"#)?;
        assert_eq!(load(root.path())?, GpuResourceMode::QwenPair12gbV1);
        std::fs::write(
            root.path().join("runtime/local-speech-policy.json"),
            r#"{"mode":"managed","managed":{"distribution":"Ubuntu"}}"#,
        )?;
        std::fs::write(
            root.path().join("runtime/local-final-asr-policy.json"),
            r#"{"enabled":true,"managed":{"distribution":"devbox"}}"#,
        )?;
        assert!(load(root.path()).is_err());
        Ok(())
    }
}

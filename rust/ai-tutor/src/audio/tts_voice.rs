//! Model-specific request fields. Transport and audio device ownership stay separate.
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    wav_path: String,
    sha256: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct References {
    zh: Reference,
    en: Reference,
}
pub(super) enum Voice {
    Qwen,
    Cosy { zh: Value, en: Value },
}

fn prepare(reference: Reference, directory: &Path) -> Result<Value> {
    ensure!(
        !reference.text.trim().is_empty() && reference.text.chars().count() <= 2000,
        "Local TTS reference needs a bounded exact transcript"
    );
    let mut bytes = Vec::new();
    File::open(directory.join(reference.wav_path))?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 2 * 1024 * 1024
            && bytes.starts_with(b"RIFF")
            && bytes.get(8..12) == Some(b"WAVE"),
        "Local TTS reference must be a WAV within 2 MiB"
    );
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == reference.sha256.to_ascii_lowercase(),
        "Local TTS reference hash mismatch"
    );
    Ok(
        json!({"ref_audio":format!("data:audio/wav;base64,{}",STANDARD.encode(bytes)),"ref_text":reference.text}),
    )
}

impl Voice {
    pub(super) fn load(
        model: &str,
        references: Option<References>,
        directory: &Path,
    ) -> Result<Self> {
        match model {
            "tutor-qwen-tts" => {
                ensure!(
                    references.is_none(),
                    "Qwen custom voices do not use reference audio"
                );
                Ok(Self::Qwen)
            }
            "tutor-cosy-tts" => {
                let references = references.ok_or_else(|| {
                    anyhow::anyhow!("CosyVoice needs explicit Chinese and English reference audio")
                })?;
                Ok(Self::Cosy {
                    zh: prepare(references.zh, directory)?,
                    en: prepare(references.en, directory)?,
                })
            }
            _ => anyhow::bail!("Unsupported local TTS model"),
        }
    }
    pub(super) fn fields(&self, language: &str) -> Value {
        match self {
            Self::Qwen => {
                json!({"voice":if language.starts_with("zh") {"vivian"} else {"ryan"},"language":"Auto"})
            }
            Self::Cosy { zh, en } => {
                if language.starts_with("zh") {
                    zh.clone()
                } else {
                    en.clone()
                }
            }
        }
    }
    pub(super) fn engine_kind(&self) -> &'static str {
        match self {
            Self::Qwen => "qwen_pcm",
            Self::Cosy { .. } => "cosy_pcm",
        }
    }
    pub(super) fn provider_name(&self) -> &'static str {
        match self {
            Self::Qwen => "Qwen3-TTS local PCM",
            Self::Cosy { .. } => "CosyVoice3 local PCM",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn references(directory: &Path) -> Result<Value> {
        // A small complete silent PCM WAV, never played or sent to a model.
        let mut wave = b"RIFF".to_vec();
        wave.extend(38u32.to_le_bytes());
        wave.extend(b"WAVEfmt ");
        wave.extend(16u32.to_le_bytes());
        wave.extend(1u16.to_le_bytes());
        wave.extend(1u16.to_le_bytes());
        wave.extend(16000u32.to_le_bytes());
        wave.extend(32000u32.to_le_bytes());
        wave.extend(2u16.to_le_bytes());
        wave.extend(16u16.to_le_bytes());
        wave.extend(b"data");
        wave.extend(2u32.to_le_bytes());
        wave.extend([0, 0]);
        std::fs::write(directory.join("reference.wav"), &wave)?;
        let item = json!({"wav_path":"reference.wav","sha256":format!("{:x}",Sha256::digest(&wave)),"text":"Chinese reference transcript"});
        let mut english = item.clone();
        english["text"] = json!("English reference transcript");
        Ok(json!({"zh":item,"en":english}))
    }
    #[test]
    fn bilingual_reference_selection_is_explicit_and_content_bound() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let data = references(directory.path())?;
        let voice = Voice::load(
            "tutor-cosy-tts",
            Some(serde_json::from_value(data.clone())?),
            directory.path(),
        )?;
        ensure!(voice.fields("zh-CN")["ref_text"] == data["zh"]["text"]);
        ensure!(voice.fields("en-US")["ref_text"] == data["en"]["text"]);
        ensure!(voice.fields("en")["voice"].is_null());
        ensure!(voice.engine_kind() == "cosy_pcm");
        let mut changed = std::fs::read(directory.path().join("reference.wav"))?;
        *changed.last_mut().unwrap() = 1;
        std::fs::write(directory.path().join("reference.wav"), changed)?;
        ensure!(
            Voice::load(
                "tutor-cosy-tts",
                Some(serde_json::from_value(data)?),
                directory.path()
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn a_model_cannot_silently_ignore_or_invent_reference_audio() -> Result<()> {
        let directory = tempfile::tempdir()?;
        ensure!(Voice::load("tutor-cosy-tts", None, directory.path()).is_err());
        ensure!(
            Voice::load(
                "tutor-qwen-tts",
                Some(serde_json::from_value(references(directory.path())?)?),
                directory.path()
            )
            .is_err()
        );
        let qwen = Voice::load("tutor-qwen-tts", None, directory.path())?;
        ensure!(qwen.fields("zh")["voice"] == "vivian" && qwen.fields("en")["voice"] == "ryan");
        ensure!(qwen.fields("en")["ref_audio"].is_null());
        Ok(())
    }
}

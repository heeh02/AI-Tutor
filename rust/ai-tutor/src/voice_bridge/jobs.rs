use super::{Command, Request, Work, backend::VoiceBackend, media};
use crate::audio::SpeechEnd;
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};

async fn canceled(cancel: &mut watch::Receiver<bool>) {
    if *cancel.borrow() {
        return;
    }
    let _ = cancel.changed().await;
}

pub(super) fn create(
    backend: &Arc<VoiceBackend>,
    request: &Request,
    mut cancel: watch::Receiver<bool>,
    starts: mpsc::Sender<(u64, Value)>,
) -> Result<Work> {
    let backend = backend.clone();
    let sequence = request.sequence;
    match &request.command {
        Command::Prepare => Ok(Box::pin(async move {
            ensure!(!*cancel.borrow(), "voice_operation_canceled");
            backend.prepare().await
        })),
        Command::Transcribe { wav_base64, .. } => {
            let pcm = media::decode_wav_base64(wav_base64)?;
            // Generate the service request ID internally; caller IDs are correlation only.
            let id = uuid::Uuid::new_v4().to_string();
            Ok(Box::pin(async move {
                ensure!(!*cancel.borrow(), "voice_operation_canceled");
                let work = backend.transcribe(&id, sequence, pcm);
                tokio::pin!(work);
                tokio::select! { biased;
                    _ = canceled(&mut cancel) => {
                        backend.cancel_asr(&id, sequence).await?;
                        Err(anyhow!("voice_operation_canceled"))
                    }
                    result = &mut work => result,
                }
            }))
        }
        Command::Speak { text, language, .. } => {
            let chunks = media::split_speech(text)?;
            ensure!(
                ["zh", "zh-CN", "en", "en-US"].contains(&language.as_str()),
                "voice_language_unsupported"
            );
            let language = language.clone();
            Ok(Box::pin(async move {
                let mut receipts = Vec::new();
                for (index, chunk) in chunks.into_iter().enumerate() {
                    ensure!(!*cancel.borrow(), "voice_operation_canceled");
                    if chunk.trim().is_empty() {
                        continue;
                    }
                    let (started, mut received) = oneshot::channel();
                    let speech = backend.speak(chunk, language.clone(), "normal", started);
                    tokio::pin!(speech);
                    let mut start_seen = false;
                    let outcome = loop {
                        tokio::select! { biased;
                            _ = canceled(&mut cancel) => {
                                backend.stop_speech().await?;
                                return Err(anyhow!("voice_operation_canceled"));
                            }
                            start = &mut received, if !start_seen => {
                                start_seen = true;
                                if let Ok(receipt) = start {
                                    starts.send((sequence, json!({"chunk_index":index,"receipt":receipt}))).await
                                        .map_err(|_| anyhow!("voice_output_unavailable"))?;
                                }
                            }
                            outcome = &mut speech => break outcome?,
                        }
                    };
                    ensure!(
                        outcome.state == SpeechEnd::Completed,
                        "voice_playback_interrupted"
                    );
                    receipts.push(outcome);
                }
                Ok(json!({"chunks":receipts,"speaker_output_measured":false}))
            }))
        }
        _ => Err(anyhow!("voice_invalid_operation")),
    }
}

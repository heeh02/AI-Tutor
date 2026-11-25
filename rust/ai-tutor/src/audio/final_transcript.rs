//! Optional precise final, using the client captured when recording began.
use super::*;
use crate::final_asr::FinalAsrClient;

#[derive(Default)]
pub(super) struct FinalAsrSlot {
    installed: Option<Arc<FinalAsrClient>>,
    recording: Option<(u64, Option<Arc<FinalAsrClient>>)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client(letter: char) -> Arc<FinalAsrClient> {
        Arc::new(
            FinalAsrClient::new(
                letter.to_string().repeat(32),
                "http://127.0.0.1:49152".into(),
                "b".repeat(32),
            )
            .unwrap(),
        )
    }
    #[test]
    fn final_client_is_frozen_at_recording_start_including_none() {
        let (audio, _) = AudioService::configuration_test_channel(0);
        audio.record_generation.store(1, Ordering::SeqCst);
        audio.reserve_final_asr(1);
        let first = client('a');
        audio.install_final_asr(first.clone());
        assert_eq!(audio.status()["asr_ready"], true);
        assert!(audio.take_final_asr(1).is_none());
        audio.record_generation.store(2, Ordering::SeqCst);
        audio.reserve_final_asr(2);
        audio.install_final_asr(client('c'));
        assert!(Arc::ptr_eq(&audio.take_final_asr(2).unwrap(), &first));
        assert!(audio.take_final_asr(2).is_none());
    }
    #[test]
    fn stale_start_or_stop_cannot_consume_new_final_snapshot() {
        let (audio, _) = AudioService::configuration_test_channel(0);
        let expected = client('a');
        audio.install_final_asr(expected.clone());
        audio.record_generation.store(2, Ordering::SeqCst);
        audio.reserve_final_asr(2);
        audio.reserve_final_asr(1);
        assert!(audio.take_final_asr(1).is_none());
        assert!(!audio.clear_final_asr("foreign"));
        assert!(Arc::ptr_eq(&audio.take_final_asr(2).unwrap(), &expected));
    }
    #[test]
    fn stopping_owner_revokes_its_captured_client_only() {
        let (audio, _) = AudioService::configuration_test_channel(0);
        let first = client('a');
        audio.install_final_asr(first.clone());
        audio.reserve_final_asr(0);
        assert!(audio.clear_final_asr(&first.instance_id));
        assert_eq!(audio.status()["asr_ready"], false);
        assert!(first.unavailable());
        assert!(audio.take_final_asr(0).is_none());
        let next = client('c');
        audio.install_final_asr(next.clone());
        assert!(!audio.clear_final_asr(&first.instance_id));
        assert!(!next.unavailable());
    }
}

struct CancelOnDrop {
    client: Arc<FinalAsrClient>,
    request: String,
    generation: u64,
    armed: bool,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let client = self.client.clone();
            let request = self.request.clone();
            let generation = self.generation;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = client.cancel(&request, generation).await;
                });
            }
        }
    }
}

impl AudioService {
    pub(super) fn precise_asr_ready(&self) -> bool {
        self.final_asr
            .lock()
            .unwrap()
            .installed
            .as_ref()
            .is_some_and(|c| !c.unavailable())
    }
    pub(crate) fn install_final_asr(&self, client: Arc<FinalAsrClient>) {
        self.final_asr.lock().unwrap().installed = Some(client);
    }
    pub(crate) fn clear_final_asr(&self, instance: &str) -> bool {
        let mut slot = self.final_asr.lock().unwrap();
        if slot
            .installed
            .as_ref()
            .is_some_and(|c| c.instance_id == instance)
        {
            slot.installed.take().unwrap().mark_unavailable();
            true
        } else {
            false
        }
    }
    pub(super) fn reserve_final_asr(&self, generation: u64) {
        let mut slot = self.final_asr.lock().unwrap();
        if self.record_generation.load(Ordering::SeqCst) == generation {
            let client = slot
                .installed
                .as_ref()
                .filter(|c| !c.unavailable())
                .cloned();
            slot.recording = Some((generation, client));
        }
    }
    fn take_final_asr(&self, generation: u64) -> Option<Arc<FinalAsrClient>> {
        let mut slot = self.final_asr.lock().unwrap();
        if slot
            .recording
            .as_ref()
            .is_some_and(|(g, _)| *g == generation)
        {
            slot.recording
                .take()
                .and_then(|(_, client)| client)
                .filter(|c| !c.unavailable())
        } else {
            None
        }
    }
    pub(super) async fn try_precise_final(
        &self,
        generation: u64,
        binding: Option<&InputBinding>,
        samples: &[f32],
        rate: u32,
    ) -> Result<Option<crate::final_asr::FinalTranscript>> {
        let Some(client) = self.take_final_asr(generation) else {
            return Ok(None);
        };
        self.transcript_context_current(generation, binding)?;
        if rate == 0 || samples.len() > rate as usize * 20 || samples.iter().any(|s| !s.is_finite())
        {
            bail!("录音数据无效");
        }
        let mut waveform = resample(samples, rate, 16000);
        let pcm = waveform
            .iter()
            .flat_map(|v| {
                // Same PCM16 quantization as the existing live partial stream.
                ((v.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()
            })
            .collect::<Vec<_>>();
        waveform.fill(0.0);
        let request = uuid::Uuid::new_v4().to_string();
        let mut guard = CancelOnDrop {
            client: client.clone(),
            request: request.clone(),
            generation,
            armed: true,
        };
        patch(&self.info, "state", json!("transcribing"));
        let inference = client.transcribe(&request, generation, pcm);
        tokio::pin!(inference);
        let mut check = tokio::time::interval(Duration::from_millis(20));
        let result = loop {
            tokio::select! {
                answer = &mut inference => break answer,
                _ = check.tick() => {
                    self.transcript_context_current(generation, binding)?;
                    if self.closing.load(Ordering::SeqCst) || client.unavailable() {
                        break Err(anyhow!("精确识别服务已停止"));
                    }
                }
            }
        };
        self.transcript_context_current(generation, binding)?;
        match result {
            Ok(answer) => {
                guard.armed = false; // A completed final has already retired its GPU job.
                if answer.text.trim().is_empty() {
                    patch(
                        &self.info,
                        "asr_fallback_reason",
                        json!("精确识别未获得文字，使用现有本地识别结果"),
                    );
                    Ok(None)
                } else {
                    Ok(Some(answer))
                }
            }
            Err(_) => {
                patch(
                    &self.info,
                    "asr_fallback_reason",
                    json!("精确识别暂不可用，使用现有本地识别结果"),
                );
                Ok(None)
            }
        }
    }
}

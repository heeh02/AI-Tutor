//! Owned local voice capabilities for a parent process. No Workstation or store.
use crate::{
    audio::{AudioService, SpeechOutcome, SpeechStarted},
    final_asr::{self, FinalAsrClient, OwnedFinalAsrService},
    gpu_resource_policy::{self, GpuResourceMode},
    local_speech::{
        LocalSpeech, LocalSpeechMode, WarmupPolicy, managed::OwnedManagedTts,
        managed_prepare::prepare_owned_managed,
    },
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot, watch};

pub const MAX_ASR_PCM_BYTES: usize = 640_000;
pub const MAX_TTS_CHARACTERS: usize = 320;

#[derive(Default)]
struct Owners {
    tts: Option<OwnedManagedTts>,
    asr: Option<OwnedFinalAsrService>,
    // Captured once under the lifecycle lock; never replaced while either owner lives.
    distribution: Option<String>,
    cleanup_failed: bool,
}

pub struct VoiceBackend {
    root: PathBuf,
    owners: AsyncMutex<Owners>,
    lifecycle_epoch: AtomicU64,
    closing: AtomicBool,
    local: Mutex<Option<Arc<LocalSpeech>>>,
    audio: Mutex<Option<Arc<AudioService>>>,
    final_client: Mutex<Option<Arc<FinalAsrClient>>>,
    final_cancel: Mutex<Option<watch::Sender<bool>>>,
    view: Mutex<Value>,
}

impl VoiceBackend {
    /// Construction reads no voice policies and starts no model or audio owner.
    pub fn new(root: PathBuf) -> Result<Arc<Self>> {
        let root = root
            .canonicalize()
            .map_err(|_| anyhow!("voice_root_unavailable"))?;
        ensure!(root.is_dir(), "voice_root_unavailable");
        Ok(Arc::new(Self {
            root,
            owners: AsyncMutex::new(Owners::default()),
            lifecycle_epoch: AtomicU64::new(0),
            closing: AtomicBool::new(false),
            local: Mutex::new(None),
            audio: Mutex::new(None),
            final_client: Mutex::new(None),
            final_cancel: Mutex::new(None),
            view: Mutex::new(json!({"phase":"idle","ready":false,
                "tts_ready":false,"asr_ready":false,"microphone_owner":"caller",
                "playback_owner":"bridge","raw_audio_saved":false})),
        }))
    }

    fn patch(&self, fields: Value) {
        let mut view = self.view.lock().unwrap();
        for (key, value) in fields.as_object().into_iter().flatten() {
            view[key] = value.clone();
        }
    }

    /// Only an allowlisted capability view is exposed, never service descriptors/logs.
    pub fn status(&self) -> Value {
        self.view.lock().unwrap().clone()
    }

    /// Diagnostic counters only; reference PCM never leaves the audio owner.
    pub fn audio_processing_status(&self) -> Value {
        let audio = self.audio.lock().unwrap().clone();
        audio.map_or(Value::Null, |audio| {
            let status = audio.status();
            json!({"capture":status["capture_processing"],
                "render_reference":status["render_reference"]})
        })
    }

    /// Available before prepare; this does not read configuration or create audio owners.
    pub fn capability_info(&self) -> Value {
        json!({
            "tts":{"provider":"owned_qwen3_tts","max_text_characters":MAX_TTS_CHARACTERS,
                "languages":["zh","zh-CN","en","en-US"],"speeds":["normal"],
                "pcm_sample_rate":24000,"channels":1,"encoding":"pcm_s16le",
                "playback_owner":"bridge","fallback":"sapi_before_first_output",
                "actual_engine_in_receipts":true},
            "asr":{"provider":"owned_qwen3_asr_1_7b","max_pcm_bytes":MAX_ASR_PCM_BYTES,
                "max_duration_seconds":20,"sample_rate":16000,"channels":1,
                "encoding":"pcm_s16le","language":"auto","capture_owner":"caller"},
            "resources":{"root":self.root,
                "tts_bundle":"bin/local-speech","asr_bundle":"bin/final-asr",
                "tts_policy":"runtime/local-speech-policy.json",
                "asr_policy":"runtime/local-final-asr-policy.json",
                "gpu_policy":"runtime/gpu-resource-policy.json"},
            "automatic_prepare":false,"raw_audio_saved":false
        })
    }

    fn current(&self, epoch: u64) -> bool {
        !self.closing.load(Ordering::SeqCst) && self.lifecycle_epoch.load(Ordering::SeqCst) == epoch
    }

    /// Uses existing explicit managed-Qwen policies and installed bundles.
    /// Nothing is installed, downloaded, adopted or automatically enabled.
    pub async fn prepare(self: &Arc<Self>) -> Result<Value> {
        let mut owners = self
            .owners
            .try_lock()
            .map_err(|_| anyhow!("voice_lifecycle_busy"))?;
        ensure!(!self.closing.load(Ordering::SeqCst), "voice_closed");
        ensure!(!owners.cleanup_failed, "voice_cleanup_unconfirmed");
        ensure!(
            owners.tts.is_none() && owners.asr.is_none(),
            "voice_already_prepared"
        );
        let epoch = self
            .lifecycle_epoch
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        self.patch(json!({"phase":"preparing","ready":false,"error":null,
            "tts_ready":false,"asr_ready":false,"cleanup":null}));
        let result = self.prepare_locked(&mut owners, epoch).await;
        if let Err(error) = result {
            let canceled = !self.current(epoch);
            let failure_stage = self.status()["phase"].clone();
            let reason = match error.to_string().as_str() {
                "WSL project path conversion failed" => "wsl_path_conversion_failed",
                "Invalid WSL project path" => "wsl_path_invalid",
                "Managed bootstrap failed; owned bridge cleanup attempted" => {
                    "tts_bootstrap_failed"
                }
                "Installed local speech bundle is missing; run the project installer" => {
                    "tts_bundle_missing"
                }
                _ => "preparation_failed",
            };
            let os_error = error
                .downcast_ref::<std::io::Error>()
                .and_then(|error| error.raw_os_error());
            self.cleanup_locked(&mut owners, if canceled { "stopped" } else { "failed" })
                .await;
            let code = if canceled {
                "voice_preparation_canceled"
            } else {
                "voice_preparation_failed"
            };
            self.patch(json!({"error":code,"failure_stage":failure_stage,"failure_reason":reason,"os_error":os_error}));
            return Err(anyhow!(code));
        }
        drop(owners);
        self.start_monitor(epoch);
        Ok(self.status())
    }

    async fn prepare_locked(&self, owners: &mut Owners, epoch: u64) -> Result<()> {
        let local = Arc::new(LocalSpeech::new(&self.root)?);
        let policy = local.policy();
        ensure!(
            policy.mode == LocalSpeechMode::Managed
                && policy.warmup == WarmupPolicy::SyntheticBilingual,
            "voice_managed_bilingual_policy_required"
        );
        let tts = policy
            .managed
            .as_ref()
            .ok_or_else(|| anyhow!("voice_qwen_required"))?;
        ensure!(tts.engine == "qwen", "voice_qwen_required");
        let asr = final_asr::load_policy(&self.root)?
            .ok_or_else(|| anyhow!("voice_final_asr_policy_required"))?;
        asr.validate()?;
        ensure!(
            gpu_resource_policy::load(&self.root)? == GpuResourceMode::QwenPair12gbV1,
            "voice_shared_qwen_profile_required"
        );
        ensure!(
            tts.distribution.eq_ignore_ascii_case(&asr.distribution),
            "voice_distribution_mismatch"
        );
        ensure!(self.current(epoch), "voice_preparation_canceled");
        owners.distribution = Some(tts.distribution.clone());
        *self.local.lock().unwrap() = Some(local.clone());
        self.patch(json!({"phase":"preparing_tts"}));
        let prepared = prepare_owned_managed(
            &self.root,
            &local,
            &mut owners.tts,
            &self.lifecycle_epoch,
            epoch,
            || {
                // Do not forward raw owner events, credentials, paths or model logs.
                let state = local.status();
                let phase = match state["phase"].as_str() {
                    Some("waiting_gpu") => "waiting_gpu",
                    _ => "preparing_tts",
                };
                self.patch(json!({"phase":phase}));
            },
        )
        .await?;
        ensure!(self.current(epoch), "voice_preparation_canceled");
        self.patch(json!({"phase":"preparing_asr","tts_ready":true}));
        let (cancel, cancelled) = watch::channel(false);
        *self.final_cancel.lock().unwrap() = Some(cancel);
        // Recheck after publishing the cancellation sender, closing its creation race.
        ensure!(self.current(epoch), "voice_preparation_canceled");
        let (send, mut events) = mpsc::channel(32);
        let preparing = OwnedFinalAsrService::prepare(&self.root, &asr, cancelled, send);
        tokio::pin!(preparing);
        let result = loop {
            tokio::select! {
                result = &mut preparing => break result,
                Some(event) = events.recv() => {
                    let phase = match event["state"].as_str() {
                        Some("waiting_gpu") => "waiting_gpu",
                        Some("warming") => "warming_asr",
                        _ => "preparing_asr",
                    };
                    self.patch(json!({"phase":phase}));
                }
            }
        };
        owners.asr = result.owner;
        let client = result.ready?;
        ensure!(self.current(epoch), "voice_preparation_canceled");
        // Capture commands are never exposed. AudioService opens no microphone on construction.
        let audio = Arc::new(AudioService::new_for_workstation(self.root.clone()));
        *self.audio.lock().unwrap() = Some(audio.clone());
        let receipt = local
            .install_prepared(&audio, &prepared, audio.speech_epoch())
            .await?;
        ensure!(receipt.applied, "voice_tts_install_failed");
        ensure!(self.current(epoch), "voice_preparation_canceled");
        *self.final_client.lock().unwrap() = Some(client);
        self.patch(json!({"phase":"ready","ready":true,"tts_ready":true,
            "asr_ready":true,"tts_engine":"qwen","asr_engine":"qwen3_asr_1_7b"}));
        Ok(())
    }

    fn start_monitor(self: &Arc<Self>, epoch: u64) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let Some(backend) = weak.upgrade() else {
                    return;
                };
                if !backend.current(epoch) {
                    return;
                }
                let mut owners = backend.owners.lock().await;
                if !backend.current(epoch) {
                    return;
                }
                let tts_alive = owners
                    .tts
                    .as_mut()
                    .is_some_and(|owner| owner.poll_running().is_ok_and(|running| running));
                let asr_alive = owners
                    .asr
                    .as_mut()
                    .is_some_and(|owner| owner.is_running().is_ok_and(|running| running));
                let asr_available = backend
                    .final_client
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|client| !client.unavailable());
                if !tts_alive || !asr_alive || !asr_available {
                    backend.lifecycle_epoch.fetch_add(1, Ordering::SeqCst);
                    backend.patch(json!({"error":"voice_owned_service_stopped"}));
                    backend.cleanup_locked(&mut owners, "failed").await;
                    return;
                }
            }
        });
    }

    fn audio_ready(&self) -> Result<Arc<AudioService>> {
        ensure!(
            !self.closing.load(Ordering::SeqCst) && self.status()["ready"] == true,
            "voice_not_ready"
        );
        self.audio
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("voice_not_ready"))
    }

    fn asr_ready(&self) -> Result<Arc<FinalAsrClient>> {
        ensure!(
            !self.closing.load(Ordering::SeqCst) && self.status()["ready"] == true,
            "voice_not_ready"
        );
        self.final_client
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("voice_not_ready"))
    }

    /// Raw PCM16LE, mono, 16 kHz. WAV parsing and microphone capture belong to caller.
    /// Existing Qwen final ASR auto-detects language; it has no language-hint input.
    pub async fn transcribe(
        &self,
        request_id: &str,
        generation: u64,
        pcm: Vec<u8>,
    ) -> Result<Value> {
        let client = self.asr_ready()?;
        let epoch = self.lifecycle_epoch.load(Ordering::SeqCst);
        ensure!(
            !pcm.is_empty() && pcm.len() <= MAX_ASR_PCM_BYTES && pcm.len() % 2 == 0,
            "voice_pcm_invalid"
        );
        let mut retirement = CancelAsrOnDrop {
            client: client.clone(),
            request_id: request_id.to_owned(),
            generation,
            armed: true,
        };
        let result = client
            .transcribe(request_id, generation, pcm)
            .await
            .map_err(safe_asr_error)?;
        retirement.armed = false;
        ensure!(self.current(epoch), "voice_operation_canceled");
        Ok(
            json!({"text":result.text,"detected_language":result.detected_language,
            "elapsed_ms":result.elapsed_ms,"provider":"qwen3_asr_1_7b","final":true}),
        )
    }

    pub async fn cancel_asr(&self, request_id: &str, generation: u64) -> Result<Value> {
        let client = self
            .final_client
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("voice_not_ready"))?;
        let receipt = client
            .cancel(request_id, generation)
            .await
            .map_err(safe_asr_error)?;
        Ok(
            json!({"canceled":receipt["canceled"],"worker_retired":receipt["worker_retired"],
            "gpu_busy":receipt["gpu_busy"]}),
        )
    }

    pub async fn speak(
        &self,
        text: String,
        language: String,
        speed: &str,
        started: oneshot::Sender<SpeechStarted>,
    ) -> Result<SpeechOutcome> {
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= MAX_TTS_CHARACTERS,
            "voice_text_limit"
        );
        ensure!(
            ["zh", "zh-CN", "en", "en-US"].contains(&language.as_str()),
            "voice_language_unsupported"
        );
        ensure!(speed == "normal", "voice_speed_unsupported");
        let audio = self.audio_ready()?;
        let epoch = audio.reserve_speech_epoch();
        audio
            .speak_with_receipts(text, language, speed, epoch, started)
            .await
            .map_err(|_| anyhow!("voice_playback_failed"))
    }

    pub async fn stop_speech(&self) -> Result<()> {
        let audio = self.audio.lock().unwrap().clone();
        if let Some(audio) = audio {
            audio
                .stop_speech()
                .await
                .map_err(|_| anyhow!("voice_playback_stop_failed"))?;
        }
        Ok(())
    }

    /// Revoke playback immediately; the job then awaits the owner's stop receipt.
    pub fn request_stop_speech(&self) {
        if let Some(audio) = self.audio.lock().unwrap().as_ref() {
            audio.reserve_speech_epoch();
        }
    }

    /// Cancels only model preparation/lifecycle. Operation cancellation uses the methods above.
    pub fn request_stop_preparation(&self) {
        self.lifecycle_epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(local) = self.local.lock().unwrap().as_ref() {
            local.cancel_preparation();
        }
        if let Some(cancel) = self.final_cancel.lock().unwrap().as_ref() {
            let _ = cancel.send(true);
        }
    }

    /// Signal first on EOF/shutdown; async shutdown then awaits owned cleanup receipts.
    pub fn request_shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.request_stop_preparation();
        if let Some(audio) = self.audio.lock().unwrap().as_ref() {
            audio.reserve_speech_epoch();
        }
    }

    pub async fn shutdown(&self) -> Value {
        self.request_shutdown();
        // Revoke playback promptly even while preparation owns the lifecycle mutex.
        let _ = self.stop_speech().await;
        let mut owners = self.owners.lock().await;
        // A canceled preparation may already have awaited both stop receipts.
        // Preserve that evidence on EOF/repeated shutdown instead of replacing it
        // with a synthetic "no process started" result.
        if owners.tts.is_none()
            && owners.asr.is_none()
            && self.audio.lock().unwrap().is_none()
            && self.status()["cleanup"].is_object()
        {
            if !owners.cleanup_failed {
                self.patch(json!({"phase":"stopped"}));
            }
            return self.status();
        }
        self.cleanup_locked(&mut owners, "stopped").await;
        self.status()
    }

    async fn cleanup_locked(&self, owners: &mut Owners, final_phase: &str) {
        self.patch(json!({"phase":"stopping","ready":false,"tts_ready":false,"asr_ready":false}));
        let client = self.final_client.lock().unwrap().take();
        if let Some(client) = client {
            client.mark_unavailable();
        }
        let audio = self.audio.lock().unwrap().take();
        if let Some(audio) = audio {
            audio.close().await;
        }
        let mut tts = owners.tts.take();
        let mut asr = owners.asr.take();
        let tts_result = match tts.as_mut() {
            Some(owner) => Some(owner.stop().await),
            None => None,
        };
        let asr_result = match asr.as_mut() {
            Some(owner) => Some(owner.stop().await),
            None => None,
        };
        let tts_cleanup = cleanup_view(tts_result);
        let asr_cleanup = cleanup_view(asr_result);
        owners.cleanup_failed |=
            tts_cleanup["cleanup_confirmed"] != true || asr_cleanup["cleanup_confirmed"] != true;
        if !owners.cleanup_failed {
            owners.distribution = None;
        }
        self.local.lock().unwrap().take();
        self.final_cancel.lock().unwrap().take();
        self.patch(
            json!({"phase":if owners.cleanup_failed { "cleanup_failed" } else { final_phase },
            "cleanup_confirmed":!owners.cleanup_failed,
            "cleanup":{"tts":tts_cleanup,"asr":asr_cleanup}}),
        );
    }
}

// An aborted caller future must still cancel exactly its own GPU request.
struct CancelAsrOnDrop {
    client: Arc<FinalAsrClient>,
    request_id: String,
    generation: u64,
    armed: bool,
}
impl Drop for CancelAsrOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let client = self.client.clone();
        let request_id = self.request_id.clone();
        let generation = self.generation;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = client.cancel(&request_id, generation).await;
            });
        }
    }
}

fn safe_asr_error(error: anyhow::Error) -> anyhow::Error {
    let code = error.to_string();
    if code.starts_with("final_asr_")
        && code.len() <= 80
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        anyhow!(code)
    } else {
        anyhow!("voice_asr_failed")
    }
}

fn cleanup_view(result: Option<Result<Value>>) -> Value {
    match result {
        None => json!({"process_started":false,"cleanup_confirmed":true}),
        Some(Ok(receipt)) => {
            // Owned stop() has already validated Linux retirement before returning Ok.
            let process_started = receipt["receipt"]["process_started"]
                .as_bool()
                .unwrap_or(true);
            let mut result = json!({"process_started":process_started,"cleanup_confirmed":true,
                "linux_cleanup_confirmed":true});
            for key in ["bridge_exit_code", "normal_owner_stop", "protocol_failed"] {
                if let Some(value) = receipt.get(key) {
                    result[key] = value.clone();
                }
            }
            result
        }
        Some(Err(_)) => json!({"process_started":true,"cleanup_confirmed":false,
            "linux_cleanup_confirmed":false,"error":"voice_owned_cleanup_unconfirmed"}),
    }
}

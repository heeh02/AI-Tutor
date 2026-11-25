//! Rust-owned WASAPI capture, optional local streaming ASR, whisper.cpp fallback and SAPI.
//! Model services never own devices; no cloud speech, saved recordings or unbounded queues.
mod barge_capture;
mod barge_in;
#[cfg(test)]
mod barge_diagnostic;
mod capture_endpoint;
#[cfg(test)]
mod capture_endpoint_tests;
mod capture_processing;
#[cfg(test)]
mod capture_processing_tests;
mod echo_cancel;
mod final_transcript;
mod input_devices;
mod render_reference;
#[cfg(all(test, windows))]
mod render_reference_diagnostic;
use final_transcript::FinalAsrSlot;
mod pcm_output;
#[cfg(test)]
pub(crate) mod recording_test;
mod speech_output;
mod streaming_asr;
#[cfg(all(test, windows))]
pub(crate) mod voice_loop_source;
#[cfg(test)]
pub(crate) mod voice_loop_test;
#[cfg(all(test, windows))]
pub(crate) use streaming_asr::voice_model_chain_diagnostic;
pub mod streaming_tts;
pub mod voice_endpoint;
use crate::local_asr::AsrClient;
use anyhow::{Result, anyhow, bail};
pub(crate) use capture_endpoint::EndpointMode;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
pub(crate) use input_devices::{InputBinding, InputSelection};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
type Response<T> = oneshot::Sender<std::result::Result<T, String>>;
const AUDIO_QUEUE_CAPACITY: usize = 8;
struct BusyReset(Arc<AtomicBool>);
impl Drop for BusyReset {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
enum CaptureCommand {
    Start(
        InputSelection,
        EndpointMode,
        u64,
        Option<streaming_asr::CaptureStream>,
        Response<Value>,
    ),
    Monitor(InputSelection, u64, u64, Response<Value>),
    RenewMonitor(u64),
    PromoteMonitor(
        u64,
        u64,
        Option<streaming_asr::CaptureStream>,
        Response<Value>,
    ),
    StopMonitor(u64, Response<Value>),
    Devices(SyncSender<std::result::Result<Value, String>>),
    Stop(u64, Response<Captured>),
    Discard(u64),
    Quit,
}
struct Captured {
    samples: Vec<f32>,
    rate: u32,
    streaming: Option<streaming_asr::FinalWait>,
    input_binding: Option<InputBinding>,
    endpoint_receipt: Option<capture_endpoint::EndpointReceipt>,
}
fn speech_rate(speed: &str) -> Result<i32> {
    match speed {
        "slow" => Ok(-2),
        "normal" => Ok(0),
        "fast" => Ok(2),
        _ => Err(anyhow!("无效语速")),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SpeechStarted {
    pub playback_id: String,
    pub epoch: u64,
    pub engine_kind: &'static str,
    pub milestone: &'static str,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechEnd {
    Completed,
    Interrupted,
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct SpeechOutcome {
    pub playback_id: String,
    pub epoch: u64,
    pub state: SpeechEnd,
    pub engine_kind: &'static str,
    pub callback_submitted_frames: u64,
    pub predicted_output_drained: bool,
    pub speaker_output_measured: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct TtsConfigurationReceipt {
    pub applied: bool,
    pub reason: &'static str,
    pub epoch: u64,
    pub engine_kind: Option<&'static str>,
}
struct TtsConfigurationRequest {
    client: Option<Arc<streaming_tts::TtsClient>>,
    expected: u64,
    preparation: Option<TtsPreparationToken>,
    response: Response<TtsConfigurationReceipt>,
}
pub(crate) struct TtsPreparationToken {
    pub epoch: Arc<AtomicU64>,
    pub expected: u64,
    // Some(epoch) records an installed client, not a queued intention.
    pub committed: Arc<Mutex<Option<u64>>>,
}
struct SpeechRequest {
    playback_id: String,
    text: String,
    language: String,
    rate: i32,
    expected: u64,
    started: Option<oneshot::Sender<SpeechStarted>>,
    response: Response<SpeechOutcome>,
}

enum SpeechCommand {
    Speak(SpeechRequest),
    Stop(u64, Response<()>),
    ConfigureTts(TtsConfigurationRequest),
    Quit,
}

pub struct AudioService {
    root: PathBuf,
    capture: SyncSender<CaptureCommand>,
    speech: SyncSender<SpeechCommand>,
    info: Arc<Mutex<Value>>,
    record_generation: Arc<AtomicU64>,
    speech_generation: Arc<AtomicU64>,
    asr_busy: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    model: Arc<Mutex<Option<WhisperContext>>>,
    streaming_asr_client: Mutex<Option<Arc<AsrClient>>>,
    final_asr: Mutex<FinalAsrSlot>,
    legacy_asr_descriptor: bool,
    input_devices: Arc<input_devices::InputDevices>,
    render_reference: Arc<render_reference::RenderReference>,
    barge_in_enabled: bool,
}

fn patch(info: &Arc<Mutex<Value>>, key: &str, value: Value) {
    info.lock().unwrap()[key] = value;
}

impl AudioService {
    pub fn new(root: PathBuf) -> Self {
        Self::with_initial_services(root, false, true)
    }
    /// Future managed startup can keep SAPI available while a separate module
    /// prepares a streaming voice. This constructor does not start any service.
    pub fn new_with_deferred_tts(root: PathBuf) -> Self {
        Self::with_initial_services(root, true, true)
    }
    /// Workstation services require explicit lifecycle policies. Diagnostic
    /// constructors retain the legacy descriptor behavior for manual probes.
    pub fn new_for_workstation(root: PathBuf) -> Self {
        Self::with_initial_services(root, true, false)
    }
    fn with_initial_services(
        root: PathBuf,
        defer_initial_tts: bool,
        legacy_asr_descriptor: bool,
    ) -> Self {
        // No logging backend is enabled: suppress native token/text diagnostics.
        // Editable ASR text must not leak into server logs before confirmation.
        whisper_rs::install_logging_hooks();
        let processing_policy = capture_processing::ProcessingPolicy::load(&root);
        let render_reference = render_reference::RenderReference::new(processing_policy.enabled());
        let info = Arc::new(Mutex::new(
            json!({"state":"idle","recording":false,"asr_ready":root.join("models/ggml-base.bin").exists(),"asr_provider":"whisper-rs / whisper.cpp / multilingual base / CPU","asr_local":true,"tts_provider":"Windows SAPI / Rust COM","tts_local":true,"voices":[],"error":null,"asr_ms":null,"speech_ms":null,"max_record_seconds":20,"raw_audio_saved":false}),
        ));
        {
            let mut state = info.lock().unwrap();
            state["asr_partial"] = Value::Null;
            state["asr_streaming_state"] = json!("disabled");
            state["asr_fallback_reason"] = Value::Null;
            state["recording_endpoint"] = Value::Null;
            state["capture_pending_result"] = json!(false);
            state["endpoint_mode"] = json!(EndpointMode::Manual);
            state["capture_processing"] = processing_policy.status();
        }
        let (capture, receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let capture_info = info.clone();
        let record_generation = Arc::new(AtomicU64::new(0));
        let record_epoch = record_generation.clone();
        let speech_generation = Arc::new(AtomicU64::new(0));
        let monitor_speech_epoch = speech_generation.clone();
        let asr_busy = Arc::new(AtomicBool::new(false));
        let capture_asr = asr_busy.clone();
        let closing = Arc::new(AtomicBool::new(false));
        let capture_closing = closing.clone();
        let input_devices = Arc::new(input_devices::InputDevices::new());
        let capture_devices = input_devices.clone();
        let capture_reference = processing_policy
            .enabled()
            .then(|| render_reference.clone());
        std::thread::Builder::new()
            .name("wasapi-owner".into())
            .spawn(move || {
                capture_loop(
                    receive,
                    capture_info,
                    record_epoch,
                    capture_asr,
                    capture_closing,
                    capture_devices,
                    capture_reference,
                    monitor_speech_epoch,
                )
            })
            .expect("audio owner thread");
        let (speech, receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let speech_info = info.clone();
        let speech_epoch = speech_generation.clone();
        let speech_closing = closing.clone();
        let speech_root = root.clone();
        let speech_runtime = tokio::runtime::Handle::try_current().ok();
        let speech_reference = processing_policy
            .enabled()
            .then(|| render_reference.clone());
        std::thread::Builder::new()
            .name("speech-owner".into())
            .spawn(move || {
                if let Err(error) = speech_output::run(
                    speech_root,
                    speech_runtime,
                    receive,
                    speech_info.clone(),
                    speech_epoch,
                    speech_closing,
                    defer_initial_tts,
                    speech_reference,
                ) {
                    patch(&speech_info, "error", json!(error.to_string()));
                }
            })
            .expect("speech owner thread");
        Self {
            root,
            capture,
            speech,
            info,
            record_generation,
            speech_generation,
            asr_busy,
            closing,
            model: Arc::new(Mutex::new(None)),
            streaming_asr_client: Mutex::new(None),
            final_asr: Mutex::new(FinalAsrSlot::default()),
            legacy_asr_descriptor,
            input_devices,
            render_reference,
            barge_in_enabled: processing_policy.barge_in_enabled(),
        }
    }

    pub fn status(&self) -> Value {
        let mut status = self.info.lock().unwrap().clone();
        status["recording_generation"] = json!(self.record_generation.load(Ordering::SeqCst));
        if status["barge_in"]["recording_generation"]
            .as_u64()
            .is_some_and(|token| token != self.record_generation.load(Ordering::SeqCst))
        {
            status["barge_in"]["monitoring"] = json!(false);
            status["barge_in"]["phase"] = json!("failed");
            status["barge_in"]["reason"] = json!("capture_generation_changed");
        }
        if status["recording_endpoint"]["recording_generation"].as_u64()
            != status["recording_generation"].as_u64()
        {
            status["recording_endpoint"] = Value::Null;
            status["capture_pending_result"] = json!(false);
        }
        if status["capture_pending_result"].is_null() {
            status["capture_pending_result"] = json!(false);
        }
        self.input_devices.stamp_status(&mut status);
        status["asr_precise_ready"] = json!(self.precise_asr_ready());
        status["render_reference"] = self.render_reference.status();
        status["asr_ready"] = json!(
            self.root.join("models/ggml-base.bin").exists()
                || status["asr_precise_ready"] == true
                || self
                    .streaming_asr_client
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|c| !c.unavailable())
                || (self.legacy_asr_descriptor
                    && self.root.join("runtime/speech-service.json").exists())
        );
        let raw_state = status["state"].as_str().unwrap_or("idle").to_owned();
        status["state"] = json!(if self.asr_busy.load(Ordering::SeqCst) {
            "transcribing"
        } else if status["recording"] == true {
            "recording"
        } else if raw_state == "recording" || raw_state == "transcribing" {
            "idle"
        } else {
            &raw_state
        });
        status
    }
    pub fn speech_epoch(&self) -> u64 {
        self.speech_generation.load(Ordering::SeqCst)
    }
    /// Reserve a distinct playback generation without waiting on a device.
    /// The owner and PCM callback already reject any prior generation.
    pub fn reserve_speech_epoch(&self) -> u64 {
        self.speech_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }
    /// A delayed, bound close can only revoke its own stream generation.
    pub fn invalidate_speech_epoch(&self, expected: u64) -> bool {
        self.speech_generation
            .compare_exchange(
                expected,
                expected.wrapping_add(1),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }
    /// Changes only the next playback's client, never starts or retries speech.
    /// The owner rejects changes while output is active or this epoch is stale.
    pub async fn configure_tts(
        &self,
        client: Option<Arc<streaming_tts::TtsClient>>,
        expected: u64,
    ) -> Result<TtsConfigurationReceipt> {
        self.configure_tts_request(client, expected, None).await
    }
    pub(crate) async fn configure_prepared_tts(
        &self,
        client: Arc<streaming_tts::TtsClient>,
        expected: u64,
        preparation: TtsPreparationToken,
    ) -> Result<TtsConfigurationReceipt> {
        self.configure_tts_request(Some(client), expected, Some(preparation))
            .await
    }
    async fn configure_tts_request(
        &self,
        client: Option<Arc<streaming_tts::TtsClient>>,
        expected: u64,
        preparation: Option<TtsPreparationToken>,
    ) -> Result<TtsConfigurationReceipt> {
        let (response, receive) = oneshot::channel();
        self.speech
            .try_send(SpeechCommand::ConfigureTts(TtsConfigurationRequest {
                client,
                expected,
                preparation,
                response,
            }))
            .map_err(|_| anyhow!("播报配置队列繁忙或已关闭"))?;
        tokio::time::timeout(Duration::from_secs(2), receive)
            .await
            .map_err(|_| anyhow!("播报配置确认超时"))?
            .map_err(|_| anyhow!("本地播报线程已关闭"))?
            .map_err(anyhow::Error::msg)
    }
    pub fn devices(&self) -> Result<Value> {
        if self.closing.load(Ordering::SeqCst) {
            bail!("音频服务已关闭");
        }
        let (send, receive) = mpsc::sync_channel(1);
        self.capture
            .try_send(CaptureCommand::Devices(send))
            .map_err(|_| anyhow!("音频操作繁忙"))?;
        receive
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| anyhow!("麦克风设备列表暂不可用"))?
            .map_err(anyhow::Error::msg)
    }
    pub(crate) fn validate_input_selection(&self, selection: &InputSelection) -> Result<()> {
        self.input_devices.validate(selection)
    }
    pub(crate) fn validate_endpoint_selection(
        &self,
        selection: &InputSelection,
        mode: EndpointMode,
    ) -> Result<()> {
        if mode == EndpointMode::VoiceEndpointV1 && selection.binding().is_none() {
            bail!("自动语句结束检测需要重新选择稳定麦克风绑定");
        }
        self.validate_input_selection(selection)
    }
    fn transcript_context_current(&self, token: u64, binding: Option<&InputBinding>) -> Result<()> {
        if token != self.record_generation.load(Ordering::SeqCst) {
            bail!("转写已取消");
        }
        if !self.input_devices.binding_current(binding) {
            patch(&self.info, "asr_partial", Value::Null);
            patch(&self.info, "error", json!(input_devices::CHANGED));
            patch(
                &self.info,
                "recording_end_reason",
                json!("input_topology_changed"),
            );
            bail!(input_devices::CHANGED);
        }
        Ok(())
    }

    pub async fn start_recording(&self, device: Option<usize>) -> Result<Value> {
        self.start_recording_selected(InputSelection::Legacy(device))
            .await
    }
    pub(crate) async fn start_recording_selected(
        &self,
        selection: InputSelection,
    ) -> Result<Value> {
        self.start_recording_with_endpoint(selection, EndpointMode::Manual)
            .await
    }
    pub(crate) async fn start_recording_with_endpoint(
        &self,
        selection: InputSelection,
        mode: EndpointMode,
    ) -> Result<Value> {
        self.start_recording_with_endpoint_reserved(selection, mode, |_| true)
            .await
    }
    pub(crate) async fn start_recording_with_endpoint_reserved<F>(
        &self,
        selection: InputSelection,
        mode: EndpointMode,
        on_reserved: F,
    ) -> Result<Value>
    where
        F: FnOnce(u64) -> bool + Send,
    {
        self.validate_endpoint_selection(&selection, mode)?;
        if self.closing.load(Ordering::SeqCst) {
            bail!("音频服务已关闭");
        }
        if self.status()["recording"] == true {
            bail!("已有录音，请先松开或停止");
        }
        if self.status()["capture_pending_result"] == true {
            bail!("录音已结束，请先收取或丢弃结果");
        }
        if self.asr_busy.load(Ordering::SeqCst) {
            bail!("本地转写尚未结束，请稍候");
        }
        self.stop_speech().await?;
        if self.asr_busy.load(Ordering::SeqCst) {
            bail!("本地转写尚未结束，请稍候");
        }
        let token = self.record_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.clear_partial_for_generation(token);
        if !on_reserved(token) || token != self.record_generation.load(Ordering::SeqCst) {
            if self
                .record_generation
                .compare_exchange(
                    token,
                    token.wrapping_add(1),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                self.clear_partial_for_generation(token.wrapping_add(1));
            }
            bail!("录音请求已取消");
        }
        let client = self.streaming_asr_snapshot();
        self.reserve_final_asr(token);
        let streaming = streaming_asr::begin(
            self.legacy_asr_descriptor.then_some(self.root.as_path()),
            client,
            self.info.clone(),
            self.record_generation.clone(),
            token,
        );
        let (send, receive) = oneshot::channel();
        self.capture
            .try_send(CaptureCommand::Start(
                selection, mode, token, streaming, send,
            ))
            .map_err(|_| anyhow!("音频操作繁忙"))?;
        self.await_capture_start(token, receive, Duration::from_secs(8))
            .await
    }
    async fn await_capture_start(
        &self,
        token: u64,
        receive: oneshot::Receiver<std::result::Result<Value, String>>,
        timeout: Duration,
    ) -> Result<Value> {
        let result = tokio::time::timeout(timeout, receive).await;
        if result.is_err()
            && self
                .record_generation
                .compare_exchange(
                    token,
                    token.wrapping_add(1),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
        {
            self.clear_partial_for_generation(token.wrapping_add(1));
        }
        result
            .map_err(|_| anyhow!("麦克风启动超时，请重试"))?
            .map_err(|_| anyhow!("麦克风工作线程已关闭"))?
            .map_err(anyhow::Error::msg)
    }
    pub(crate) fn install_streaming_asr(&self, client: Arc<AsrClient>) {
        *self.streaming_asr_client.lock().unwrap() = Some(client);
    }
    pub(crate) fn clear_streaming_asr(&self, instance: &str) -> bool {
        let mut slot = self.streaming_asr_client.lock().unwrap();
        if slot
            .as_ref()
            .is_some_and(|client| client.instance_id == instance)
        {
            *slot = None;
            true
        } else {
            false
        }
    }
    pub(crate) fn streaming_asr_snapshot(&self) -> Option<Arc<AsrClient>> {
        self.streaming_asr_client
            .lock()
            .unwrap()
            .as_ref()
            .filter(|client| !client.unavailable())
            .cloned()
    }
    pub fn discard_recording(&self) {
        let token = self.record_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.clear_partial_for_generation(token);
        let _ = self.capture.try_send(CaptureCommand::Discard(token));
    }
    pub fn discard_recording_expected(&self, token: u64) -> Result<()> {
        let next = token.wrapping_add(1);
        self.record_generation
            .compare_exchange(token, next, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| anyhow!("录音请求已过期，请重新录音"))?;
        self.clear_partial_for_generation(next);
        // Even if the queue is full, the owner's generation check will release
        // this capture. A late start response cannot discard a newer one.
        let _ = self.capture.try_send(CaptureCommand::Discard(next));
        Ok(())
    }
    fn clear_partial_for_generation(&self, generation: u64) {
        let mut info = self.info.lock().unwrap();
        if self.record_generation.load(Ordering::SeqCst) == generation {
            info["asr_partial"] = Value::Null;
            info["asr_streaming_state"] = json!("idle");
            info["recording_endpoint"] = Value::Null;
            info["capture_pending_result"] = json!(false);
        }
    }
    pub async fn stop_recording(&self, language: &str) -> Result<Value> {
        self.stop_recording_expected(language, self.record_generation.load(Ordering::SeqCst))
            .await
    }
    pub async fn stop_recording_expected(&self, language: &str, token: u64) -> Result<Value> {
        // Reject an old browser/CLI stop before it can consume the newer capture
        // or occupy its ASR slot. The owner checks this token again after queuing.
        if token != self.record_generation.load(Ordering::SeqCst) {
            bail!("录音请求已过期，请重新录音");
        }
        if self.asr_busy.swap(true, Ordering::SeqCst) {
            bail!("已有本地转写任务");
        }
        let busy_reset = BusyReset(self.asr_busy.clone());
        let asr_started = Instant::now();
        let (send, receive) = oneshot::channel();
        if self
            .capture
            .try_send(CaptureCommand::Stop(token, send))
            .is_err()
        {
            bail!("音频操作繁忙");
        }
        let received = tokio::time::timeout(Duration::from_secs(8), receive).await;
        if received.is_err() {
            self.record_generation.fetch_add(1, Ordering::SeqCst);
        }
        let data = received
            .map_err(|_| anyhow!("麦克风停止超时，请重试"))?
            .map_err(|_| anyhow!("麦克风工作线程已关闭"))?;
        let Captured {
            mut samples,
            rate,
            streaming,
            input_binding,
            endpoint_receipt,
        } = match data {
            Ok(v) => v,
            Err(e) => {
                bail!(e)
            }
        };
        let endpoint_json = json!(endpoint_receipt);
        if let Err(error) = self.transcript_context_current(token, input_binding.as_ref()) {
            samples.fill(0.0);
            return Err(error);
        }
        let sample_count = samples.len();
        let recorded_ms = sample_count as u64 * 1000 / u64::from(rate.max(1));
        patch(&self.info, "recorded_samples", json!(sample_count));
        patch(&self.info, "recorded_ms", json!(recorded_ms));
        let rms = if samples.is_empty() {
            0.0
        } else {
            (samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32).sqrt()
        };
        patch(&self.info, "rms", json!(rms));
        if samples.len() < rate as usize * 45 / 100 || rms < 0.002 {
            samples.fill(0.0);
            patch(&self.info, "asr_partial", Value::Null);
            return Ok(
                json!({"text":"","error":"录音太短或声音过轻，请重录","rms":rms,"sample_count":sample_count,"sample_rate":rate,"recorded_ms":recorded_ms,"recording_endpoint":endpoint_json}),
            );
        }
        let precise = self
            .try_precise_final(token, input_binding.as_ref(), &samples, rate)
            .await;
        match precise {
            Ok(Some(answer)) => {
                samples.fill(0.0);
                patch(&self.info, "state", json!("idle"));
                patch(&self.info, "asr_partial", Value::Null);
                patch(&self.info, "asr_backend_ms", json!(answer.elapsed_ms));
                patch(
                    &self.info,
                    "asr_ms",
                    json!(asr_started.elapsed().as_millis()),
                );
                patch(
                    &self.info,
                    "asr_provider",
                    json!("Qwen3-ASR-1.7B / local GPU final"),
                );
                return Ok(
                    json!({"text":answer.text,"rms":rms,"sample_count":sample_count,
                    "sample_rate":rate,"recorded_ms":recorded_ms,"requires_confirmation":true,
                    "error":null,"provider":"Qwen3-ASR-1.7B / local GPU final","fallback_reason":null,
                    "detected_language":answer.detected_language,"recording_endpoint":endpoint_json}),
                );
            }
            Err(error) => {
                samples.fill(0.0);
                return Err(error);
            }
            Ok(None) => {}
        }
        if let Some(streaming) = streaming {
            let final_started = Instant::now();
            let streamed = streaming.wait().await;
            if let Err(error) = self.transcript_context_current(token, input_binding.as_ref()) {
                samples.fill(0.0);
                return Err(error);
            }
            if let Ok(final_text) = &streamed {
                if !final_text.text.trim().is_empty() {
                    samples.fill(0.0);
                    patch(&self.info, "state", json!("idle"));
                    patch(&self.info, "asr_partial", Value::Null);
                    patch(
                        &self.info,
                        "asr_ms",
                        json!(asr_started.elapsed().as_millis()),
                    );
                    patch(
                        &self.info,
                        "asr_backend_ms",
                        json!(final_started.elapsed().as_millis()),
                    );
                    patch(&self.info, "asr_provider", json!(streaming_asr::PROVIDER));
                    return Ok(
                        json!({"text":final_text.text,"rms":rms,"sample_count":sample_count,"sample_rate":rate,"recorded_ms":recorded_ms,"requires_confirmation":true,"error":null,"provider":streaming_asr::PROVIDER,"fallback_reason":self.info.lock().unwrap()["asr_fallback_reason"].clone(),"stream_total_ms":final_text.elapsed_ms,"recording_endpoint":endpoint_json}),
                    );
                }
            }
            let reason = streamed
                .err()
                .unwrap_or_else(|| "流式识别未获得文字，改用完整音频离线转写".into());
            patch(&self.info, "asr_fallback_reason", json!(reason));
            patch(&self.info, "asr_streaming_state", json!("fallback"));
        }
        patch(&self.info, "asr_partial", Value::Null);
        patch(
            &self.info,
            "asr_provider",
            json!("whisper-rs / whisper.cpp / multilingual base / CPU"),
        );
        let fallback_reason = self.info.lock().unwrap()["asr_fallback_reason"].clone();
        let generation = self.record_generation.clone();
        let model = self.model.clone();
        let path = self.root.join("models/ggml-base.bin");
        let language = language.to_owned();
        let info = self.info.clone();
        let input_devices = self.input_devices.clone();
        let inference_binding = input_binding.clone();
        patch(&info, "state", json!("transcribing"));
        let job = tokio::task::spawn_blocking(move || {
            let _busy_reset = busy_reset;
            let start = Instant::now();
            let result = (|| -> Result<String> {
                if !path.exists() {
                    bail!("本地语音模型尚未安装");
                }
                let mut holder = model.lock().map_err(|_| anyhow!("转写模型锁异常"))?;
                if holder.is_none() {
                    let mut options = WhisperContextParameters::default();
                    options.use_gpu(false);
                    *holder = Some(WhisperContext::new_with_params(
                        path.to_str().ok_or_else(|| anyhow!("模型路径无效"))?,
                        options,
                    )?);
                }
                let mut pcm = resample(&samples, rate, 16000);
                samples.fill(0.0);
                let mut state = holder.as_ref().unwrap().create_state()?;
                let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                params.set_n_threads(4);
                params.set_language(Some(if language.starts_with("zh") {
                    "zh"
                } else {
                    "en"
                }));
                params.set_translate(false);
                params.set_no_context(true);
                params.set_print_progress(false);
                params.set_print_realtime(false);
                params.set_print_timestamps(false);
                params.set_print_special(false);
                params.set_initial_prompt(
                    "主控、电机、测距、超声波、停车距离、绿灯。control, motor, distance, light.",
                );
                let abort_gen = generation.clone();
                let abort_devices = input_devices.clone();
                let abort_binding = inference_binding.clone();
                params.set_abort_callback_safe::<_, Box<dyn FnMut() -> bool>>(Some(Box::new(
                    move || {
                        abort_gen.load(Ordering::SeqCst) != token
                            || !abort_devices.binding_current(abort_binding.as_ref())
                            || start.elapsed() > Duration::from_secs(45)
                    },
                )
                    as Box<dyn FnMut() -> bool>));
                let inferred = state.full(params, &pcm);
                pcm.fill(0.0);
                if generation.load(Ordering::SeqCst) != token
                    || !input_devices.binding_current(inference_binding.as_ref())
                {
                    bail!("转写已取消");
                }
                if start.elapsed() > Duration::from_secs(45) {
                    bail!("本地转写超时，请重录");
                }
                inferred?;
                let mut text = String::new();
                for segment in state.as_iter() {
                    if segment.no_speech_probability() < 0.6 {
                        text.push_str(segment.to_str()?);
                    }
                }
                Ok(text.trim().to_owned())
            })();
            samples.fill(0.0);
            patch(&info, "asr_backend_ms", json!(start.elapsed().as_millis()));
            patch(&info, "asr_ms", json!(asr_started.elapsed().as_millis()));
            patch(&info, "state", json!("idle"));
            result.map(|text|json!({"text":text,"rms":rms,"sample_count":sample_count,"sample_rate":rate,"recorded_ms":recorded_ms,"requires_confirmation":true,"provider":"whisper-rs / whisper.cpp / multilingual base / CPU","fallback_reason":fallback_reason,"error":if text.is_empty(){Some("未获得可靠转写，请重录或输入文字")}else{None}}))
        });
        let result = job.await.map_err(|_| anyhow!("本地转写工作线程异常"))?;
        self.transcript_context_current(token, input_binding.as_ref())?;
        result.map(|mut reply| {
            reply["recording_endpoint"] = endpoint_json;
            reply
        })
    }

    pub async fn stop_speech(&self) -> Result<()> {
        let stopped_epoch = self.reserve_speech_epoch();
        let (send, receive) = oneshot::channel();
        self.speech
            .try_send(SpeechCommand::Stop(stopped_epoch, send))
            .map_err(|_| anyhow!("播报操作繁忙"))?;
        tokio::time::timeout(Duration::from_secs(4), receive)
            .await
            .map_err(|_| anyhow!("本地播报停止超时"))?
            .map_err(|_| anyhow!("本地播报线程已关闭"))?
            .map_err(anyhow::Error::msg)
    }
    pub async fn speak(&self, text: String, language: String, expected: u64) -> Result<bool> {
        self.speak_with_speed(text, language, "normal", expected)
            .await
    }
    pub async fn speak_with_speed(
        &self,
        text: String,
        language: String,
        speed: &str,
        expected: u64,
    ) -> Result<bool> {
        Ok(self
            .speak_request(text, language, speed, expected, None)
            .await?
            .state
            == SpeechEnd::Completed)
    }
    /// A one-shot receipt reports the actual engine start milestone, not an
    /// enqueue or network receipt. CPAL submission is not measured speaker sound.
    pub async fn speak_with_receipts(
        &self,
        text: String,
        language: String,
        speed: &str,
        expected: u64,
        started: oneshot::Sender<SpeechStarted>,
    ) -> Result<SpeechOutcome> {
        self.speak_request(text, language, speed, expected, Some(started))
            .await
    }
    async fn speak_request(
        &self,
        text: String,
        language: String,
        speed: &str,
        expected: u64,
        started: Option<oneshot::Sender<SpeechStarted>>,
    ) -> Result<SpeechOutcome> {
        let rate = speech_rate(speed)?;
        let playback_id = uuid::Uuid::new_v4().to_string();
        let status = self.status();
        let monitor_conflict = status["barge_in"]["monitoring"] == true
            && status["barge_in"]["expected_audio_epoch"].as_u64() != Some(expected);
        if expected != self.speech_epoch()
            || self.closing.load(Ordering::SeqCst)
            || self.asr_busy.load(Ordering::SeqCst)
            || status["recording"] == true
            || monitor_conflict
        {
            return Ok(SpeechOutcome {
                playback_id,
                epoch: expected,
                state: SpeechEnd::Interrupted,
                engine_kind: "none",
                callback_submitted_frames: 0,
                predicted_output_drained: false,
                speaker_output_measured: false,
                reason: Some("cancelled_before_enqueue".into()),
            });
        }
        let (send, receive) = oneshot::channel();
        self.speech
            .try_send(SpeechCommand::Speak(SpeechRequest {
                playback_id,
                text,
                language,
                rate,
                expected,
                started,
                response: send,
            }))
            .map_err(|_| anyhow!("播报操作繁忙"))?;
        receive
            .await
            .map_err(|_| anyhow!("本地播报线程已关闭"))?
            .map_err(anyhow::Error::msg)
    }
    pub async fn close(&self) {
        self.discard_recording();
        let _ = self.stop_speech().await;
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.capture.try_send(CaptureCommand::Quit);
        let _ = self.speech.try_send(SpeechCommand::Quit);
    }
}

// Test-only queue access lets lifecycle tests pause the owner precisely at the
// commit boundary without constructing an AudioService/device owner thread.
#[cfg(test)]
pub(crate) struct TestTtsQueue(mpsc::Receiver<SpeechCommand>);
#[cfg(test)]
pub(crate) struct TestTtsRequest(TtsConfigurationRequest);
#[cfg(test)]
impl TestTtsQueue {
    pub(crate) fn take(&self) -> Result<TestTtsRequest> {
        match self.0.try_recv()? {
            SpeechCommand::ConfigureTts(request) => Ok(TestTtsRequest(request)),
            _ => bail!("test expected a configuration command, never playback"),
        }
    }
}
#[cfg(test)]
impl TestTtsRequest {
    pub(crate) fn commit(
        self,
        client: &mut Option<Arc<streaming_tts::TtsClient>>,
        current_epoch: u64,
    ) -> TtsConfigurationReceipt {
        speech_output::configure_tts_slot(client, self.0, current_epoch, false, false)
            .expect("the installer must still own its receipt")
    }
}
#[cfg(test)]
impl AudioService {
    pub(crate) fn configuration_test_channel(epoch: u64) -> (Self, TestTtsQueue) {
        let (speech, receive) = mpsc::sync_channel(4);
        let (capture, _capture) = mpsc::sync_channel(1);
        (
            Self {
                root: PathBuf::new(),
                capture,
                speech,
                info: Arc::new(Mutex::new(json!({}))),
                record_generation: Arc::new(AtomicU64::new(0)),
                speech_generation: Arc::new(AtomicU64::new(epoch)),
                asr_busy: Arc::new(AtomicBool::new(false)),
                closing: Arc::new(AtomicBool::new(false)),
                model: Arc::new(Mutex::new(None)),
                streaming_asr_client: Mutex::new(None),
                final_asr: Mutex::new(FinalAsrSlot::default()),
                legacy_asr_descriptor: false,
                input_devices: Arc::new(input_devices::InputDevices::new()),
                render_reference: render_reference::RenderReference::new(false),
                barge_in_enabled: false,
            },
            TestTtsQueue(receive),
        )
    }
}

enum InputStream {
    Native {
        _stream: cpal::Stream,
    },
    #[cfg(all(test, windows))]
    DiagnosticWav {
        _source: voice_loop_source::Opened,
    },
    #[cfg(test)]
    Fake {
        _close: TestInputClose,
    },
}
#[cfg(test)]
struct TestInputClose(Option<Box<dyn FnOnce() + Send>>);
#[cfg(test)]
impl Drop for TestInputClose {
    fn drop(&mut self) {
        if let Some(close) = self.0.take() {
            close();
        }
    }
}
struct Recording {
    stream: Option<InputStream>,
    data: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    started: Instant,
    generation: u64,
    failed: Arc<AtomicBool>,
    streaming: Option<streaming_asr::CaptureStream>,
    input_binding: Option<InputBinding>,
    endpoint: Option<capture_endpoint::CaptureEndpoint>,
    processing: Option<capture_processing::CaptureProcessing>,
    barge: Option<barge_capture::Monitor>,
}
const INPUT_DISCONNECTED: &str = "麦克风已断开，请重新连接后重新录音";
fn discard_input(recording: &mut Option<Recording>, info: &Arc<Mutex<Value>>) {
    if let Some(mut current) = recording.take() {
        if let Some(monitor) = &current.barge {
            let mut state = monitor.status(current.generation, current.processing.as_ref());
            state["phase"] = json!("failed");
            state["monitoring"] = json!(false);
            state["reason"] = json!("capture_discarded");
            patch(info, "barge_in", state);
        }
        current.stream.take();
        current.data.lock().unwrap().fill(0.0);
        if let Some(processing) = current.processing.as_mut() {
            processing.wipe();
        }
        let mut info = info.lock().unwrap();
        if info["asr_partial"]["recording_generation"].as_u64() == Some(current.generation) {
            info["asr_partial"] = Value::Null;
        }
    }
    patch(info, "recording", json!(false));
    patch(info, "recording_endpoint", Value::Null);
    patch(info, "capture_pending_result", json!(false));
}
fn discard_stale_input(
    recording: &mut Option<Recording>,
    info: &Arc<Mutex<Value>>,
    generation: &AtomicU64,
) {
    if recording
        .as_ref()
        .is_some_and(|r| r.generation != generation.load(Ordering::SeqCst))
    {
        discard_input(recording, info);
    }
}
fn discard_failed_input(recording: &mut Option<Recording>, info: &Arc<Mutex<Value>>) {
    if recording
        .as_ref()
        .is_some_and(|r| r.endpoint.is_none() && r.failed.load(Ordering::SeqCst))
    {
        // Dropping a WASAPI stream belongs to its owner, never its callback.
        discard_input(recording, info);
        patch(info, "state", json!("idle"));
        patch(info, "error", json!(INPUT_DISCONNECTED));
        patch(info, "recording_end_reason", json!("device_error"));
    }
}
fn discard_changed_input(
    recording: &mut Option<Recording>,
    info: &Arc<Mutex<Value>>,
    generation: &AtomicU64,
    devices: &input_devices::InputDevices,
) {
    if let Some(current) = recording
        .as_ref()
        .filter(|r| r.endpoint.is_none() && !devices.binding_current(r.input_binding.as_ref()))
    {
        // A late topology check may discard only this recording, never advance
        // a newer start's generation. Callbacks never drop streams or lock here.
        let _ = generation.compare_exchange(
            current.generation,
            current.generation.wrapping_add(1),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        discard_input(recording, info);
        patch(info, "state", json!("idle"));
        patch(info, "error", json!(input_devices::CHANGED));
        patch(
            info,
            "recording_end_reason",
            json!("input_topology_changed"),
        );
    }
}
fn finish_input(
    recording: &mut Option<Recording>,
    info: &Arc<Mutex<Value>>,
) -> std::result::Result<Captured, String> {
    let result = recording
        .take()
        .ok_or_else(|| "没有进行中的录音".to_owned())
        .and_then(|mut r| {
            r.stream.take();
            let mut data = r.data.lock().unwrap();
            if barge_capture::unadopted(&r) {
                if let Some(monitor) = &r.barge {
                    let mut state = monitor.status(r.generation, r.processing.as_ref());
                    state["phase"] = json!("failed");
                    state["monitoring"] = json!(false);
                    state["reason"] = json!("monitor_not_adopted");
                    patch(info, "barge_in", state);
                }
                data.fill(0.0);
                if let Some(processing) = &mut r.processing {
                    processing.wipe();
                }
                return Err("监测音频尚未接管为录音".into());
            }
            if r.failed.load(Ordering::SeqCst) {
                data.fill(0.0);
                if let Some(processing) = r.processing.as_mut() {
                    processing.wipe();
                }
                patch(info, "error", json!(INPUT_DISCONNECTED));
                return Err(INPUT_DISCONNECTED.to_owned());
            }
            let (samples, rate) = if let Some(processing) = r.processing.as_mut() {
                processing.pump(&data, true);
                patch(info, "capture_processing", processing.status());
                let samples = processing.samples().to_vec();
                processing.wipe();
                data.fill(0.0);
                (samples, 16_000)
            } else {
                (std::mem::take(&mut *data), r.rate)
            };
            let streaming = r
                .streaming
                .take()
                .map(|stream| stream.finish(&samples, rate));
            Ok(Captured {
                samples,
                rate,
                streaming,
                input_binding: r.input_binding,
                endpoint_receipt: r
                    .endpoint
                    .as_ref()
                    .and_then(|endpoint| endpoint.receipt().cloned()),
            })
        });
    patch(info, "recording", json!(false));
    patch(info, "state", json!("idle"));
    patch(info, "capture_pending_result", json!(false));
    result
}

fn endpoint_candidate(
    recording: &mut Recording,
    devices: &input_devices::InputDevices,
) -> Option<(voice_endpoint::EndpointCandidate, Option<&'static str>)> {
    let endpoint = recording.endpoint.as_mut()?;
    if endpoint.receipt().is_some() {
        return None;
    }
    // This is an owner pump, never a WASAPI callback. Limit each turn to 40ms
    // analysis so Stop/Discard is reconsidered even after a metadata delay.
    for _ in 0..4 {
        let fault = if recording.failed.load(Ordering::SeqCst) {
            Some("stream_error")
        } else if !devices.binding_current(recording.input_binding.as_ref()) {
            Some("input_topology_changed")
        } else if recording.rate == 0 {
            Some("invalid_sample_rate")
        } else {
            None
        };
        if let Some(cause) = fault {
            return endpoint
                .error(recording.generation)
                .map(|end| (end, Some(cause)));
        }
        let (samples, sample_rate) = if recording.barge.is_some() {
            (
                recording
                    .processing
                    .as_ref()
                    .map_or(0, |p| p.samples().len()),
                16_000,
            )
        } else {
            (recording.data.lock().unwrap().len(), recording.rate)
        };
        if capture_endpoint::duration_reached(recording.started.elapsed(), samples, sample_rate) {
            return endpoint.limit(recording.generation).map(|end| (end, None));
        }
        let frame = if let Some(processing) = recording.processing.as_ref() {
            endpoint.next_frame(processing.samples(), 16_000)
        } else {
            let data = recording.data.lock().unwrap();
            endpoint.next_frame(&data, recording.rate)
        }?;
        if let Some(end) = endpoint.process(recording.generation, &frame) {
            let cause = (end.reason == voice_endpoint::EndpointReason::DeviceError)
                .then_some("invalid_audio_samples");
            return Some((end, cause));
        }
    }
    None
}

fn close_endpoint_input(
    recording: &mut Recording,
    mut candidate: voice_endpoint::EndpointCandidate,
    mut cause: Option<&'static str>,
    info: &Arc<Mutex<Value>>,
    generation: &AtomicU64,
    devices: &input_devices::InputDevices,
) {
    // Drop joins the native capture worker. No status receipt may precede it.
    recording.stream.take();
    let closed_elapsed = recording.started.elapsed();
    if let Some(processing) = recording.processing.as_mut() {
        processing.pump(&recording.data.lock().unwrap(), true);
        patch(info, "capture_processing", processing.status());
    }
    let (samples, sample_rate) = barge_capture::sample_clock(recording);
    if recording.failed.load(Ordering::SeqCst) {
        candidate.reason = voice_endpoint::EndpointReason::DeviceError;
        cause = Some("stream_error");
    } else if !devices.binding_current(recording.input_binding.as_ref()) {
        candidate.reason = voice_endpoint::EndpointReason::DeviceError;
        cause = Some("input_topology_changed");
    } else if candidate.reason != voice_endpoint::EndpointReason::DeviceError
        && capture_endpoint::duration_reached(closed_elapsed, samples, sample_rate)
    {
        candidate.reason = voice_endpoint::EndpointReason::DurationLimit;
        cause = None;
    }
    if candidate.reason == voice_endpoint::EndpointReason::DeviceError {
        // CaptureStream::Drop takes info too; cancel before locking it below.
        // Full local PCM stays available until Stop/Discard.
        recording.streaming.take();
    }
    let mut state = info.lock().unwrap();
    if generation.load(Ordering::SeqCst) != recording.generation {
        return;
    }
    let Some(receipt) = recording.endpoint.as_mut().and_then(|endpoint| {
        endpoint.record_closed(
            candidate,
            cause,
            samples,
            sample_rate,
            closed_elapsed.as_millis() as u64,
        )
    }) else {
        return;
    };
    state["recording"] = json!(false);
    state["state"] = json!("idle");
    state["capture_pending_result"] = json!(true);
    state["recording_endpoint"] = json!(receipt);
    state["recording_end_reason"] = cause
        .map(|cause| json!(cause))
        .unwrap_or_else(|| json!(candidate.reason));
    if candidate.reason == voice_endpoint::EndpointReason::DeviceError {
        state["error"] = json!(if cause == Some("input_topology_changed") {
            input_devices::CHANGED
        } else {
            INPUT_DISCONNECTED
        });
    }
}

fn pump_capture_endpoint(
    recording: &mut Option<Recording>,
    info: &Arc<Mutex<Value>>,
    generation: &AtomicU64,
    devices: &input_devices::InputDevices,
) {
    discard_stale_input(recording, info, generation);
    let Some(current) = recording.as_mut() else {
        return;
    };
    // Once published, a terminal is never rewritten. A later device change
    // instead revokes that capture token, as it does for a cancelled recording.
    let invalidate_closed = current
        .endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.receipt())
        .is_some_and(|receipt| {
            receipt.candidate.reason != voice_endpoint::EndpointReason::DeviceError
                && (current.failed.load(Ordering::SeqCst)
                    || !devices.binding_current(current.input_binding.as_ref()))
        });
    if invalidate_closed {
        let _ = generation.compare_exchange(
            current.generation,
            current.generation.wrapping_add(1),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        discard_input(recording, info);
        patch(info, "recording_end_reason", json!("device_error"));
        return;
    }
    if let Some((candidate, cause)) = endpoint_candidate(current, devices) {
        close_endpoint_input(current, candidate, cause, info, generation, devices);
    }
    discard_stale_input(recording, info, generation);
}

fn manual_capture_limit(recording: &mut Recording) {
    if recording.barge.is_none()
        && recording.endpoint.is_none()
        && recording.started.elapsed() > Duration::from_secs(20)
    {
        recording.stream.take();
    }
}

#[allow(clippy::too_many_arguments)]
fn open_capture(
    recording: &mut Option<Recording>,
    selection: InputSelection,
    mode: EndpointMode,
    expected: u64,
    streaming: Option<streaming_asr::CaptureStream>,
    info: &Arc<Mutex<Value>>,
    generation: &Arc<AtomicU64>,
    input_devices: &input_devices::InputDevices,
    reference: &Option<Arc<render_reference::RenderReference>>,
    mut barge: Option<barge_capture::Monitor>,
) -> Result<Value> {
    if mode == EndpointMode::VoiceEndpointV1 && selection.binding().is_none() {
        bail!("自动语句结束检测需要稳定麦克风绑定");
    }
    #[cfg(all(test, windows))]
    if barge.is_none()
        && let Some(source) = input_devices.voice_loop_source(&selection)?
    {
        let (opened, receipt) = source.open(
            mode,
            expected,
            streaming,
            &info,
            &generation,
            &input_devices,
        )?;
        *recording = Some(opened);
        return Ok(receipt);
    }
    let (chosen, device) = match &selection {
        InputSelection::Bound(binding) => input_devices.resolve(binding)?,
        InputSelection::Legacy(id) => {
            let devices = cpal::default_host().input_devices()?.collect::<Vec<_>>();
            let chosen = (*id)
                .or_else(|| {
                    devices.iter().position(|d| {
                        d.name()
                            .unwrap_or_default()
                            .to_uppercase()
                            .contains("OBSBOT")
                    })
                })
                .unwrap_or(0);
            let device = devices
                .into_iter()
                .nth(chosen)
                .ok_or_else(|| anyhow!("未发现所选麦克风"))?;
            (chosen, device)
        }
    };
    let supported = device.default_input_config()?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.clone().into();
    // Metadata work can race a notification. Recheck immediately
    // before opening, then again before publishing success.
    if !input_devices.binding_current(selection.binding()) {
        bail!(input_devices::CHANGED);
    }
    if let Some(monitor) = &mut barge {
        monitor.configure_rate(rate);
    }
    let window = barge.as_ref().map(|monitor| monitor.window.clone());
    let samples = Arc::new(Mutex::new(Vec::with_capacity(
        rate as usize * if barge.is_some() { 2 } else { 20 },
    )));
    let failed = Arc::new(AtomicBool::new(false));
    let failures = failed.clone();
    let error = move |_| {
        // Per-recording flag: late callbacks from a released
        // device must not poison the next recording.
        failures.store(true, Ordering::SeqCst);
    };
    let data = samples.clone();
    let timing = reference
        .as_ref()
        .map(|_| capture_processing::CaptureTiming::new());
    let callback_timing = timing.clone();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |input: &[f32], info: &cpal::InputCallbackInfo| {
                push_timed_samples(
                    &data,
                    input.iter().step_by(channels).copied(),
                    rate,
                    callback_timing.as_deref(),
                    window.as_deref(),
                    info,
                )
            },
            error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |input: &[i16], info: &cpal::InputCallbackInfo| {
                push_timed_samples(
                    &data,
                    input.iter().step_by(channels).map(|v| *v as f32 / 32768.0),
                    rate,
                    callback_timing.as_deref(),
                    window.as_deref(),
                    info,
                )
            },
            error,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |input: &[u16], info: &cpal::InputCallbackInfo| {
                push_timed_samples(
                    &data,
                    input
                        .iter()
                        .step_by(channels)
                        .map(|v| (*v as f32 - 32768.0) / 32768.0),
                    rate,
                    callback_timing.as_deref(),
                    window.as_deref(),
                    info,
                )
            },
            error,
            None,
        )?,
        _ => bail!("麦克风采样格式暂不支持"),
    };
    stream.play()?;
    if failed.load(Ordering::SeqCst) {
        drop(stream);
        samples.lock().unwrap().fill(0.0);
        patch(&info, "error", json!(INPUT_DISCONNECTED));
        bail!(INPUT_DISCONNECTED);
    }
    if expected != generation.load(Ordering::SeqCst) {
        drop(stream);
        samples.lock().unwrap().fill(0.0);
        bail!("录音请求已取消");
    }
    if !input_devices.binding_current(selection.binding()) {
        drop(stream);
        samples.lock().unwrap().fill(0.0);
        bail!(input_devices::CHANGED);
    }
    let mut processing = reference.as_ref().zip(timing).map(|(reference, timing)| {
        capture_processing::CaptureProcessing::new(reference.clone(), timing, rate)
    });
    if let Some(monitor) = &mut barge {
        monitor.opened_now();
    }
    if let (Some(monitor), Some(processing)) = (&barge, &mut processing) {
        processing.enable_monitor(monitor.expected_audio_epoch, expected);
    }
    let is_monitor = barge.is_some();
    *recording = Some(Recording {
        stream: Some(InputStream::Native { _stream: stream }),
        data: samples,
        rate,
        started: Instant::now(),
        generation: expected,
        failed,
        streaming,
        input_binding: selection.binding().cloned(),
        endpoint: (!is_monitor && mode == EndpointMode::VoiceEndpointV1)
            .then(|| capture_endpoint::CaptureEndpoint::new(expected)),
        processing,
        barge,
    });
    patch(&info, "recording", json!(!is_monitor));
    if !is_monitor {
        patch(&info, "state", json!("recording"));
    }
    patch(&info, "device_id", json!(chosen));
    patch(&info, "error", Value::Null);
    patch(&info, "recording_end_reason", Value::Null);
    patch(&info, "recording_endpoint", Value::Null);
    patch(&info, "capture_pending_result", json!(false));
    patch(&info, "endpoint_mode", json!(mode));
    let mut receipt = json!({"device_id":chosen,"sample_rate":rate,"recording_generation":expected,"endpoint_mode":mode});
    if let Some(binding) = selection.binding() {
        receipt["device_key"] = json!(binding.device_key);
        receipt["input_topology_revision"] = json!(binding.topology_revision);
    }
    if let Some(current) = recording.as_ref() {
        if let Some(monitor) = &current.barge {
            patch(
                info,
                "barge_in",
                monitor.status(expected, current.processing.as_ref()),
            );
            receipt["barge_in_monitor"] = json!(true);
            receipt["expected_audio_epoch"] = json!(monitor.expected_audio_epoch);
        }
    }
    Ok(receipt)
}

fn capture_loop(
    receiver: mpsc::Receiver<CaptureCommand>,
    info: Arc<Mutex<Value>>,
    generation: Arc<AtomicU64>,
    asr_busy: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    input_devices: Arc<input_devices::InputDevices>,
    reference: Option<Arc<render_reference::RenderReference>>,
    speech_generation: Arc<AtomicU64>,
) {
    let mut recording: Option<Recording> = None;
    // Created only by an explicit device-list request; no startup enumeration.
    let mut monitor: Option<input_devices::Monitor> = None;
    loop {
        if closing.load(Ordering::SeqCst) {
            break;
        }
        discard_stale_input(&mut recording, &info, &generation);
        discard_failed_input(&mut recording, &info);
        discard_changed_input(&mut recording, &info, &generation, &input_devices);
        barge_capture::pump(&mut recording, &info, &speech_generation);
        if let Some(current) = recording.as_mut().filter(|r| r.barge.is_none()) {
            if let Some(processing) = current.processing.as_mut() {
                processing.pump_shared(&current.data, false);
                patch(&info, "capture_processing", processing.status());
            }
        }
        pump_capture_endpoint(&mut recording, &info, &generation, &input_devices);
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(CaptureCommand::Devices(response)) => {
                if monitor.is_none() {
                    // Legacy list remains usable when notification registration
                    // is unavailable, but never advertises a stable binding.
                    monitor = input_devices::Monitor::new(input_devices.topology.clone()).ok();
                }
                let _ = response.send(input_devices.enumerate().map_err(|e| e.to_string()));
            }
            Ok(CaptureCommand::Start(selection, mode, expected, streaming, response)) => {
                if expected != generation.load(Ordering::SeqCst) || asr_busy.load(Ordering::SeqCst)
                {
                    let _ = response.send(Err("录音请求已取消或转写尚未结束".into()));
                    continue;
                }
                discard_stale_input(&mut recording, &info, &generation);
                if recording.is_some() {
                    let _ = response.send(Err("已有录音，请先松开或停止".into()));
                    continue;
                }
                let result = open_capture(
                    &mut recording,
                    selection,
                    mode,
                    expected,
                    streaming,
                    &info,
                    &generation,
                    &input_devices,
                    &reference,
                    None,
                )
                .map_err(|error| error.to_string());
                let _ = response.send(result);
            }
            Ok(CaptureCommand::Monitor(selection, token, audio_epoch, response)) => {
                let result = if asr_busy.load(Ordering::SeqCst) {
                    Err(anyhow!("本地转写尚未结束"))
                } else {
                    barge_capture::open(
                        &mut recording,
                        selection,
                        token,
                        audio_epoch,
                        &info,
                        &generation,
                        &speech_generation,
                        &input_devices,
                        &reference,
                    )
                };
                let _ = response.send(result.map_err(|error| error.to_string()));
            }
            Ok(CaptureCommand::RenewMonitor(token)) => {
                if token == generation.load(Ordering::SeqCst) {
                    barge_capture::renew(&mut recording, token);
                }
            }
            Ok(CaptureCommand::PromoteMonitor(token, audio_epoch, streaming, response)) => {
                let result = barge_capture::promote(
                    &mut recording,
                    token,
                    audio_epoch,
                    streaming,
                    &info,
                    &generation,
                    &speech_generation,
                    &input_devices,
                    &reference,
                );
                let _ = response.send(result.map_err(|error| error.to_string()));
            }
            Ok(CaptureCommand::StopMonitor(token, response)) => {
                let result = if token == generation.load(Ordering::SeqCst) {
                    barge_capture::stop(&mut recording, token, &info)
                } else {
                    Ok(
                        json!({"stopped":false,"stale":true,"onset_pending":false,"recording_generation":token}),
                    )
                };
                let _ = response.send(result.map_err(|error| error.to_string()));
            }
            Ok(CaptureCommand::Stop(expected, response)) => {
                discard_changed_input(&mut recording, &info, &generation, &input_devices);
                if expected != generation.load(Ordering::SeqCst)
                    || recording.as_ref().is_some_and(|r| r.generation != expected)
                {
                    let _ = response.send(Err("录音结果已取消".into()));
                    continue;
                }
                let result = finish_input(&mut recording, &info);
                let _ = response.send(result);
            }
            Ok(CaptureCommand::Discard(expected)) => {
                if expected == generation.load(Ordering::SeqCst) {
                    discard_input(&mut recording, &info);
                }
            }
            Ok(CaptureCommand::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(r) = recording.as_mut() {
            pump_streaming(r);
            manual_capture_limit(r);
        }
    }
    discard_input(&mut recording, &info);
    input_devices.clear();
    drop(monitor);
}
fn pump_streaming(recording: &mut Recording) {
    if let Some(streaming) = recording.streaming.as_mut() {
        if let Some(processing) = recording.processing.as_ref() {
            streaming.pump(processing.samples(), 16_000, false);
        } else {
            streaming.pump(&recording.data.lock().unwrap(), recording.rate, false);
        }
    }
}
fn push_samples(data: &Arc<Mutex<Vec<f32>>>, samples: impl Iterator<Item = f32>, rate: u32) {
    let mut buffer = data.lock().unwrap();
    let remaining = (rate as usize * 20).saturating_sub(buffer.len());
    buffer.extend(samples.take(remaining));
}

fn push_timed_samples(
    data: &Arc<Mutex<Vec<f32>>>,
    samples: impl Iterator<Item = f32>,
    rate: u32,
    timing: Option<&capture_processing::CaptureTiming>,
    window: Option<&barge_capture::Window>,
    info: &cpal::InputCallbackInfo,
) {
    let Some(timing) = timing else {
        push_samples(data, samples, rate);
        return;
    };
    let now = Instant::now();
    let timestamp = info.timestamp();
    let captured_at = timestamp
        .callback
        .duration_since(&timestamp.capture)
        .filter(|delay| *delay <= Duration::from_millis(500))
        .and_then(|delay| now.checked_sub(delay));
    let mut buffer = data.lock().unwrap();
    let start = buffer.len();
    let limit = window.map_or(rate as usize * 20, |window| {
        window.limit.load(Ordering::Relaxed) as usize
    });
    let remaining = limit.saturating_sub(start);
    let mut samples = samples;
    buffer.extend(samples.by_ref().take(remaining));
    if let Some(window) = window {
        if samples.next().is_some() {
            window.overflowed.store(true, Ordering::SeqCst);
        }
    }
    let base = window.map_or(0, |window| window.base.load(Ordering::Relaxed) as usize);
    timing.record(base + start, buffer.len() - start, rate, captured_at);
}

pub fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return samples.to_vec();
    }
    if samples.is_empty() {
        return vec![];
    }
    let length = samples.len() as u64 * to as u64 / from as u64;
    (0..length)
        .map(|i| {
            let pos = i as f64 * from as f64 / to as f64;
            let left = pos as usize;
            let right = (left + 1).min(samples.len() - 1);
            let frac = (pos - left as f64) as f32;
            samples[left] * (1.0 - frac) + samples[right] * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resampling_bounded() {
        assert_eq!(resample(&vec![0.1; 48000], 48000, 16000).len(), 16000);
        assert!(resample(&[], 48000, 16000).is_empty());
    }
    #[test]
    fn twenty_seconds_is_hard_bound() {
        let data = Arc::new(Mutex::new(Vec::new()));
        push_samples(&data, std::iter::repeat_n(0.2, 600000), 16000);
        assert_eq!(data.lock().unwrap().len(), 320000);
    }

    fn simulated_recording(failed: Arc<AtomicBool>) -> Recording {
        Recording {
            stream: None,
            data: Arc::new(Mutex::new(vec![0.5; 16000])),
            rate: 16000,
            started: Instant::now(),
            generation: 1,
            failed,
            streaming: None,
            input_binding: None,
            endpoint: None,
            processing: None,
            barge: None,
        }
    }

    #[test]
    fn topology_change_discards_bound_audio_and_never_advances_a_newer_generation() {
        for current_generation in [1, 7] {
            let devices = input_devices::InputDevices::new();
            let binding = devices.test_binding();
            let mut captured = simulated_recording(Arc::new(AtomicBool::new(false)));
            captured.input_binding = Some(binding);
            let samples = captured.data.clone();
            let mut recording = Some(captured);
            let info = Arc::new(Mutex::new(json!({"recording":true})));
            let generation = AtomicU64::new(current_generation);
            devices.topology.changed();
            discard_changed_input(&mut recording, &info, &generation, &devices);
            assert!(recording.is_none());
            assert!(samples.lock().unwrap().iter().all(|v| *v == 0.0));
            assert_eq!(
                generation.load(Ordering::SeqCst),
                if current_generation == 1 { 2 } else { 7 }
            );
            assert_eq!(
                info.lock().unwrap()["recording_end_reason"],
                "input_topology_changed"
            );
            assert!(finish_input(&mut recording, &info).is_err());
        }
    }

    #[test]
    fn topology_change_preserves_legacy_manual_capture_and_current_bound_capture() {
        let devices = input_devices::InputDevices::new();
        let mut recording = Some(simulated_recording(Arc::new(AtomicBool::new(false))));
        let info = Arc::new(Mutex::new(json!({"recording":true})));
        let generation = AtomicU64::new(1);
        devices.topology.changed();
        discard_changed_input(&mut recording, &info, &generation, &devices);
        assert!(recording.is_some());
        recording.as_mut().unwrap().input_binding = Some(devices.test_binding());
        discard_changed_input(&mut recording, &info, &generation, &devices);
        assert!(recording.is_some());
        assert_eq!(generation.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn topology_change_after_capture_invalidates_final_without_poisoning_new_capture() {
        let (service, _queue) = AudioService::configuration_test_channel(0);
        let binding = service.input_devices.test_binding();
        assert!(
            service
                .transcript_context_current(0, Some(&binding))
                .is_ok()
        );
        service.input_devices.topology.changed();
        assert!(
            service
                .transcript_context_current(0, Some(&binding))
                .is_err()
        );
        assert_eq!(
            service.status()["recording_end_reason"],
            "input_topology_changed"
        );
        service.record_generation.store(1, Ordering::SeqCst);
        patch(&service.info, "error", Value::Null);
        patch(&service.info, "recording_end_reason", Value::Null);
        assert!(
            service
                .transcript_context_current(0, Some(&binding))
                .is_err()
        );
        assert!(service.status()["error"].is_null());
        assert!(service.status()["recording_end_reason"].is_null());
    }

    #[tokio::test]
    async fn unknown_bound_start_is_rejected_by_owner_without_enumerating_devices() {
        let (send, receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let devices = Arc::new(input_devices::InputDevices::new());
        let binding = devices.test_binding(); // Available notification fixture, no device handle.
        let info = Arc::new(Mutex::new(json!({"recording":false})));
        let observed = info.clone();
        let owner = std::thread::spawn(move || {
            capture_loop(
                receive,
                info,
                Arc::new(AtomicU64::new(1)),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                devices,
                None,
                Arc::new(AtomicU64::new(0)),
            )
        });
        let (reply, result) = oneshot::channel();
        send.send(CaptureCommand::Start(
            InputSelection::Bound(binding),
            EndpointMode::Manual,
            1,
            None,
            reply,
        ))
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), result)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(observed.lock().unwrap()["recording"], false);
        send.send(CaptureCommand::Quit).unwrap();
        owner.join().unwrap();
    }

    #[test]
    fn disconnected_input_is_discarded_and_releases_recording_state() {
        let info = Arc::new(Mutex::new(json!({"state":"recording","recording":true})));
        let failed = Arc::new(AtomicBool::new(true));
        let captured = simulated_recording(failed);
        let samples = captured.data.clone();
        let mut recording = Some(captured);
        discard_failed_input(&mut recording, &info);
        assert!(recording.is_none());
        assert!(samples.lock().unwrap().iter().all(|v| *v == 0.0));
        let state = info.lock().unwrap();
        assert_eq!(state["state"], "idle");
        assert_eq!(state["recording"], false);
        assert_eq!(state["error"], INPUT_DISCONNECTED);
    }

    #[test]
    fn disconnect_racing_with_stop_never_returns_partial_audio() {
        let info = Arc::new(Mutex::new(json!({"recording":true})));
        let failed = Arc::new(AtomicBool::new(false));
        let captured = simulated_recording(failed.clone());
        let samples = captured.data.clone();
        let mut recording = Some(captured);
        // The callback arrives after the owner's periodic fault check.
        discard_failed_input(&mut recording, &info);
        failed.store(true, Ordering::SeqCst);
        assert!(finish_input(&mut recording, &info).is_err());
        assert!(recording.is_none());
        assert!(samples.lock().unwrap().iter().all(|v| *v == 0.0));
        assert_eq!(info.lock().unwrap()["recording"], false);
    }

    #[test]
    fn released_device_callback_cannot_discard_a_new_recording() {
        let info = Arc::new(Mutex::new(json!({"recording":true,"error":null})));
        let old_failure = Arc::new(AtomicBool::new(false));
        let mut recording = Some(simulated_recording(old_failure.clone()));
        discard_input(&mut recording, &info);
        recording = Some(simulated_recording(Arc::new(AtomicBool::new(false))));
        patch(&info, "recording", json!(true));
        old_failure.store(true, Ordering::SeqCst);
        discard_failed_input(&mut recording, &info);
        assert!(recording.is_some());
        let Captured { samples, rate, .. } = finish_input(&mut recording, &info).unwrap();
        assert_eq!(rate, 16000);
        assert_eq!(samples, vec![0.5; 16000]);
        assert!(info.lock().unwrap()["error"].is_null());
    }

    #[tokio::test]
    async fn stale_audio_controls_cannot_stop_or_discard_a_new_recording() {
        let (capture, commands) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let (speech, _) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let service = AudioService {
            root: PathBuf::from("missing"),
            capture,
            speech,
            info: Arc::new(Mutex::new(json!({"state":"recording","recording":true}))),
            record_generation: Arc::new(AtomicU64::new(2)),
            speech_generation: Arc::new(AtomicU64::new(0)),
            asr_busy: Arc::new(AtomicBool::new(false)),
            closing: Arc::new(AtomicBool::new(false)),
            model: Arc::new(Mutex::new(None)),
            streaming_asr_client: Mutex::new(None),
            final_asr: Mutex::new(FinalAsrSlot::default()),
            legacy_asr_descriptor: false,
            input_devices: Arc::new(input_devices::InputDevices::new()),
            render_reference: render_reference::RenderReference::new(false),
            barge_in_enabled: false,
        };
        // This fixture never follows an external descriptor.
        assert!(service.stop_recording_expected("zh", 1).await.is_err());
        assert!(service.discard_recording_expected(1).is_err());
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!service.asr_busy.load(Ordering::SeqCst));
        assert_eq!(service.status()["recording"], true);
        assert_eq!(service.status()["recording_generation"], 2);
        // The matching token invalidates only that capture, exactly once.
        service.discard_recording_expected(2).unwrap();
        assert_eq!(service.status()["recording_generation"], 3);
        assert!(matches!(
            commands.try_recv(),
            Ok(CaptureCommand::Discard(3))
        ));
        assert!(service.discard_recording_expected(2).is_err());
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn cancellation_releases_buffer_even_when_control_queue_is_full() {
        let (send, _receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        for _ in 0..AUDIO_QUEUE_CAPACITY {
            send.try_send(CaptureCommand::Discard(1)).unwrap();
        }
        assert!(matches!(
            send.try_send(CaptureCommand::Discard(2)),
            Err(mpsc::TrySendError::Full(_))
        ));
        let info = Arc::new(Mutex::new(json!({"recording":true})));
        let samples = Arc::new(Mutex::new(vec![0.5; 16000]));
        let mut recording = Some(Recording {
            stream: None,
            data: samples.clone(),
            rate: 16000,
            started: Instant::now(),
            generation: 1,
            failed: Arc::new(AtomicBool::new(false)),
            streaming: None,
            input_binding: None,
            endpoint: None,
            processing: None,
            barge: None,
        });
        let epoch = AtomicU64::new(2);
        discard_stale_input(&mut recording, &info, &epoch);
        assert!(recording.is_none());
        assert!(samples.lock().unwrap().iter().all(|v| *v == 0.0));
        assert_eq!(info.lock().unwrap()["recording"], false);
    }

    #[tokio::test]
    async fn stale_queued_start_is_rejected_before_opening_any_device() {
        let (send, receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let info = Arc::new(Mutex::new(json!({"recording":false})));
        let current = Arc::new(AtomicU64::new(2));
        let busy = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let owner = std::thread::spawn(move || {
            capture_loop(
                receive,
                info,
                current,
                busy,
                closed,
                Arc::new(input_devices::InputDevices::new()),
                None,
                Arc::new(AtomicU64::new(0)),
            )
        });
        let (reply, result) = oneshot::channel();
        send.send(CaptureCommand::Start(
            InputSelection::Legacy(None),
            EndpointMode::Manual,
            1,
            None,
            reply,
        ))
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), result)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        send.send(CaptureCommand::Quit).unwrap();
        owner.join().unwrap();
    }

    #[tokio::test]
    async fn aborting_waiter_does_not_release_a_running_asr_slot() {
        let busy = Arc::new(AtomicBool::new(true));
        let reset = BusyReset(busy.clone());
        let (started, ready) = oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _reset = reset;
            let _ = started.send(());
            waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        ready.await.unwrap();
        worker.abort();
        assert!(busy.load(Ordering::SeqCst));
        release.send(()).unwrap();
        let _ = worker.await;
        assert!(!busy.load(Ordering::SeqCst));
    }

    #[test]
    fn status_prioritizes_live_capture_and_asr_over_old_speech_updates() {
        let (capture, _) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let (speech, _) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        let service = AudioService {
            root: PathBuf::from("missing"),
            capture,
            speech,
            info: Arc::new(Mutex::new(json!({"state":"idle","recording":true}))),
            record_generation: Arc::new(AtomicU64::new(0)),
            speech_generation: Arc::new(AtomicU64::new(0)),
            asr_busy: Arc::new(AtomicBool::new(true)),
            closing: Arc::new(AtomicBool::new(false)),
            model: Arc::new(Mutex::new(None)),
            streaming_asr_client: Mutex::new(None),
            final_asr: Mutex::new(FinalAsrSlot::default()),
            legacy_asr_descriptor: false,
            input_devices: Arc::new(input_devices::InputDevices::new()),
            render_reference: render_reference::RenderReference::new(false),
            barge_in_enabled: false,
        };
        assert_eq!(service.status()["state"], "transcribing");
        service.asr_busy.store(false, Ordering::SeqCst);
        assert_eq!(service.status()["state"], "recording");
        patch(&service.info, "recording", json!(false));
        patch(&service.info, "state", json!("recording"));
        assert_eq!(service.status()["state"], "idle");
    }
}

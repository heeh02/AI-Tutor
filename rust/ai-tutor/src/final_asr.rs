//! Authenticated, whole-waveform final ASR. No discovery, retries or GPU ownership.
#[cfg(all(test, windows))]
mod cancel_diagnostic;
#[cfg(all(test, windows))]
mod diagnostic;
mod owned;
use anyhow::{Result, anyhow, ensure};
pub(crate) use owned::{FinalAsrConfig, OwnedFinalAsrService};
use reqwest::{Client, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_PCM_BYTES: usize = 640_000;
const MAX_REPLY_BYTES: usize = 65_536;
const FINAL_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const CANCEL_TIMEOUT: Duration = Duration::from_millis(300);
const MODEL_ID: &str = "Qwen/Qwen3-ASR-1.7B";
const MODEL_REVISION: &str = "7278e1e70fe206f11671096ffdd38061171dd6e5";
const SDK_COMMIT: &str = "7c6daf77a2421100f5fb066495372c00129d39ff";
const PROVIDER: &str = "Qwen3-ASR-1.7B / PyTorch BF16 / local offline final";

#[derive(Debug)]
pub(crate) struct FinalTranscript {
    pub text: String,
    pub detected_language: String,
    /// Complete HTTP final call, not microphone release or subtitle latency.
    pub elapsed_ms: u64,
}

/// The credential is deliberately neither Debug nor serializable.
pub(crate) struct FinalAsrClient {
    pub instance_id: String,
    base_url: String,
    token: String,
    http: Client,
    failed: AtomicBool,
    ready: AtomicBool,
    state: Mutex<RequestState>,
}

#[derive(Default)]
struct RequestState {
    last_generation: Option<u64>,
    active: Option<ActiveRequest>,
}
struct ActiveRequest {
    id: String,
    generation: u64,
    canceled: bool,
}

struct Failure {
    code: &'static str,
    permanent: bool,
}
impl Failure {
    fn protocol(code: &'static str) -> Self {
        Self {
            code,
            permanent: true,
        }
    }
    fn temporary(code: &'static str) -> Self {
        Self {
            code,
            permanent: false,
        }
    }
    fn transport(error: reqwest::Error) -> Self {
        // Never attach the reqwest error: it may contain request URLs/details.
        Self::temporary(if error.is_timeout() {
            "final_asr_timeout"
        } else {
            "final_asr_transport"
        })
    }
}
type WireResult<T> = std::result::Result<T, Failure>;

impl FinalAsrClient {
    pub fn new(instance_id: String, base_url: String, token: String) -> Result<Self> {
        ensure!(
            instance_id.len() == 32
                && instance_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "final_asr_invalid_instance"
        );
        let url = Url::parse(&base_url).map_err(|_| anyhow!("final_asr_invalid_address"))?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some_and(|port| port > 0)
                && url.path() == "/"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "final_asr_loopback_only"
        );
        ensure!(
            (24..=256).contains(&token.len())
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "final_asr_invalid_credential"
        );
        let http = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .http1_only()
            .pool_max_idle_per_host(0)
            .connect_timeout(Duration::from_millis(500))
            .timeout(FINAL_TIMEOUT)
            .build()
            .map_err(|_| anyhow!("final_asr_client_unavailable"))?;
        Ok(Self {
            instance_id,
            base_url: url.as_str().trim_end_matches('/').into(),
            token,
            http,
            failed: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            state: Mutex::new(RequestState::default()),
        })
    }

    pub fn unavailable(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }
    pub fn mark_unavailable(&self) {
        self.failed.store(true, Ordering::SeqCst);
        self.ready.store(false, Ordering::SeqCst);
    }

    fn authenticated(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .bearer_auth(&self.token)
            .header("X-ASR-Instance", &self.instance_id)
            .header("Connection", "close")
    }
    fn finish_result<T>(&self, result: WireResult<T>) -> Result<T> {
        result.map_err(|failure| {
            if failure.permanent {
                self.mark_unavailable();
            }
            anyhow!(failure.code)
        })
    }
    fn check_identity(&self, protocol: u64, instance: &str) -> WireResult<()> {
        if protocol != 1 || instance != self.instance_id {
            return Err(Failure::protocol("final_asr_instance_changed"));
        }
        Ok(())
    }

    async fn exchange(&self, request: RequestBuilder, budget: Duration) -> WireResult<Vec<u8>> {
        let operation = async {
            let mut response = self
                .authenticated(request)
                .timeout(budget)
                .send()
                .await
                .map_err(Failure::transport)?;
            let status = response.status();
            if response
                .content_length()
                .is_some_and(|size| size > MAX_REPLY_BYTES as u64)
            {
                return Err(Failure::protocol("final_asr_response_limit"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(Failure::transport)? {
                if chunk.len() > MAX_REPLY_BYTES - bytes.len() {
                    return Err(Failure::protocol("final_asr_response_limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            if status != StatusCode::OK {
                let error: ErrorReceipt = parse(&bytes)?;
                self.check_identity(error.protocol_version, &error.instance_id)?;
                return Err(match (status.as_u16(), error.error.as_str()) {
                    (409, "gpu_worker_busy") => Failure::temporary("final_asr_busy"),
                    (409, "request_canceled") => Failure::temporary("final_asr_canceled"),
                    (409, "stale_or_replayed_generation") => {
                        Failure::temporary("final_asr_stale_generation")
                    }
                    (503, "model_not_warm_or_closing") => Failure::temporary("final_asr_not_ready"),
                    (504, "final_deadline_exceeded") => Failure::temporary("final_asr_timeout"),
                    (502, "decoder_truncated") => Failure::temporary("final_asr_truncated"),
                    (502, "local_gpu_inference_failed") => {
                        Failure::protocol("final_asr_engine_failed")
                    }
                    _ => Failure::protocol("final_asr_http_error"),
                });
            }
            Ok(bytes)
        };
        tokio::time::timeout(budget, operation)
            .await
            .unwrap_or_else(|_| Err(Failure::temporary("final_asr_timeout")))
    }

    /// Call after the owned service's warm_ready lifecycle event; cache success.
    pub async fn check_warm(&self) -> Result<()> {
        ensure!(!self.unavailable(), "final_asr_unavailable");
        if self.ready.load(Ordering::SeqCst) {
            return Ok(());
        }
        let result = async {
            let bytes = self
                .exchange(
                    self.http.get(format!("{}/health", self.base_url)),
                    HEALTH_TIMEOUT,
                )
                .await?;
            let health: HealthReceipt = parse(&bytes)?;
            self.check_identity(health.protocol_version, &health.instance_id)?;
            if health.provider != PROVIDER || health.inference_mode != "offline_final" {
                return Err(Failure::protocol("final_asr_health_contract"));
            }
            if health.phase != "warm_ready"
                || !health.model_ready
                || !health.ready
                || !health.warmup_completed
                || health.busy
                || health.closing
            {
                return Err(Failure::temporary("final_asr_not_ready"));
            }
            if self.unavailable() {
                return Err(Failure::protocol("final_asr_unavailable"));
            }
            self.ready.store(true, Ordering::SeqCst);
            Ok(())
        }
        .await;
        self.finish_result(result)
    }

    pub async fn transcribe(
        &self,
        request_id: &str,
        generation: u64,
        pcm: Vec<u8>,
    ) -> Result<FinalTranscript> {
        valid_request_id(request_id)?;
        ensure!(
            !pcm.is_empty() && pcm.len() <= MAX_PCM_BYTES && pcm.len() % 2 == 0,
            "final_asr_invalid_pcm"
        );
        let began = Instant::now();
        let mut in_flight = self.admit(request_id, generation)?;
        let request = self
            .http
            .post(format!("{}/v1/asr/transcribe", self.base_url))
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", pcm.len())
            .header("X-ASR-Request-Id", request_id)
            .header("X-Recording-Generation", generation)
            .header("X-Audio-Sample-Rate", "16000")
            .header("X-Audio-Channels", "1")
            .header("X-Audio-Encoding", "pcm_s16le")
            .header("X-Audio-Sample-Count", pcm.len() / 2)
            .body(pcm);
        let result = async {
            let bytes = self.exchange(request, FINAL_TIMEOUT).await?;
            let receipt: FinalReceipt = parse(&bytes)?;
            self.validate_final(&receipt, request_id, generation)?;
            // Cancellation invalidates delivery, not the resident GPU service.
            in_flight.complete()?;
            Ok(FinalTranscript {
                text: receipt.text,
                detected_language: receipt.detected_language,
                elapsed_ms: began.elapsed().as_millis() as u64,
            })
        }
        .await;
        self.finish_result(result)
    }

    /// Independent of an aborted final request. A cancel receipt is not GPU retirement.
    pub async fn cancel(&self, request_id: &str, generation: u64) -> Result<Value> {
        valid_request_id(request_id)?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("final_asr_state_unavailable"))?;
            if let Some(active) = state.active.as_mut()
                && active.id == request_id
                && active.generation == generation
            {
                active.canceled = true;
            }
        }
        let request = self
            .http
            .post(format!("{}/v1/asr/cancel", self.base_url))
            .header("Content-Length", "0")
            .header("X-ASR-Request-Id", request_id)
            .header("X-Recording-Generation", generation)
            .body(Vec::<u8>::new());
        let result = async {
            let bytes = self.exchange(request, CANCEL_TIMEOUT).await?;
            let receipt: CancelReceipt = parse(&bytes)?;
            self.check_identity(receipt.protocol_version, &receipt.instance_id)?;
            if receipt.request_id != request_id || receipt.recording_generation != generation {
                return Err(Failure::protocol("final_asr_cancel_identity"));
            }
            if (receipt.canceled && !receipt.worker_retired.is_boolean())
                || (!receipt.canceled && !receipt.worker_retired.is_null())
                || (receipt.canceled && receipt.worker_retired == false && !receipt.gpu_busy)
            {
                return Err(Failure::protocol("final_asr_cancel_contract"));
            }
            serde_json::to_value(receipt)
                .map_err(|_| Failure::protocol("final_asr_cancel_contract"))
        }
        .await;
        self.finish_result(result)
    }

    fn admit(&self, request_id: &str, generation: u64) -> Result<InFlight<'_>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("final_asr_state_unavailable"))?;
        ensure!(!self.unavailable(), "final_asr_unavailable");
        ensure!(self.ready.load(Ordering::SeqCst), "final_asr_not_ready");
        ensure!(state.active.is_none(), "final_asr_busy");
        ensure!(
            state.last_generation.is_none_or(|last| generation > last),
            "final_asr_stale_generation"
        );
        // Consume even a timed-out/busy generation; no replay on this instance.
        state.last_generation = Some(generation);
        state.active = Some(ActiveRequest {
            id: request_id.into(),
            generation,
            canceled: false,
        });
        Ok(InFlight {
            client: self,
            id: request_id.into(),
            generation,
        })
    }

    fn validate_final(
        &self,
        value: &FinalReceipt,
        request_id: &str,
        generation: u64,
    ) -> WireResult<()> {
        self.check_identity(value.protocol_version, &value.instance_id)?;
        if value.request_id != request_id
            || value.recording_generation != generation
            || !value.final_received
        {
            return Err(Failure::protocol("final_asr_final_identity"));
        }
        if value.model_id != MODEL_ID
            || value.model_revision != MODEL_REVISION
            || value.sdk_commit != SDK_COMMIT
            || value.provider != PROVIDER
            || value.backend != "transformers"
            || value.precision != "bfloat16"
            || value.attention != "sdpa"
            || value.max_new_tokens != 512
        {
            return Err(Failure::protocol("final_asr_model_contract"));
        }
        if value.possible_truncation || value.generation_calls.len() != 1 {
            return Err(Failure::protocol("final_asr_invalid_generation"));
        }
        let call = &value.generation_calls[0];
        // EOS exactly at the ceiling is a complete final under this service contract.
        if !(1..=512).contains(&call.new_tokens)
            || call.hit_token_limit != (call.new_tokens == 512)
            || !call.ended_with_eos
            || call.raw_decoder_text.is_empty()
            || value.text.chars().count() > 8000
        {
            return Err(Failure::protocol("final_asr_invalid_generation"));
        }
        Ok(())
    }
}

struct InFlight<'a> {
    client: &'a FinalAsrClient,
    id: String,
    generation: u64,
}
impl InFlight<'_> {
    fn complete(&mut self) -> WireResult<()> {
        let mut state = self
            .client
            .state
            .lock()
            .map_err(|_| Failure::protocol("final_asr_state_unavailable"))?;
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| Failure::temporary("final_asr_canceled"))?;
        if active.id != self.id || active.generation != self.generation || active.canceled {
            return Err(Failure::temporary("final_asr_canceled"));
        }
        if self.client.unavailable() {
            return Err(Failure::protocol("final_asr_unavailable"));
        }
        state.active = None;
        Ok(())
    }
}
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.client.state.lock()
            && state
                .active
                .as_ref()
                .is_some_and(|active| active.id == self.id && active.generation == self.generation)
        {
            state.active = None;
        }
        // Caller sends exact cancel separately. The service alone owns real GPU retirement.
    }
}

fn valid_request_id(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value),
        "final_asr_invalid_request_id"
    );
    Ok(())
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> WireResult<T> {
    serde_json::from_slice(bytes).map_err(|_| Failure::protocol("final_asr_invalid_reply"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthReceipt {
    protocol_version: u64,
    instance_id: String,
    phase: String,
    model_ready: bool,
    ready: bool,
    warmup_completed: bool,
    busy: bool,
    closing: bool,
    provider: String,
    inference_mode: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalReceipt {
    protocol_version: u64,
    instance_id: String,
    request_id: String,
    recording_generation: u64,
    #[serde(rename = "final")]
    final_received: bool,
    provider: String,
    text: String,
    detected_language: String,
    model_id: String,
    model_revision: String,
    sdk_commit: String,
    backend: String,
    precision: String,
    attention: String,
    max_new_tokens: u64,
    generation_calls: Vec<GenerationReceipt>,
    possible_truncation: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationReceipt {
    new_tokens: u64,
    hit_token_limit: bool,
    ended_with_eos: bool,
    raw_decoder_text: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CancelReceipt {
    protocol_version: u64,
    instance_id: String,
    request_id: String,
    recording_generation: u64,
    canceled: bool,
    worker_retired: Value,
    gpu_busy: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorReceipt {
    protocol_version: u64,
    instance_id: String,
    error: String,
    #[serde(default, rename = "diagnostic")]
    _diagnostic: Option<Value>,
}

#[cfg(test)]
#[path = "final_asr/client_tests.rs"]
mod client_tests;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    protocol_version: u64,
    #[serde(default)]
    enabled: bool,
    managed: Option<FinalAsrConfig>,
}

pub(crate) fn load_policy(root: &std::path::Path) -> Result<Option<FinalAsrConfig>> {
    let path = root.join("runtime/local-final-asr-policy.json");
    if !path.exists() {
        return Ok(None);
    }
    ensure!(std::fs::metadata(&path)?.len() <= 4096, "精确识别配置过大");
    let value: Policy =
        serde_json::from_slice(&std::fs::read(path)?).map_err(|_| anyhow!("精确识别配置无效"))?;
    ensure!(value.protocol_version == 1, "不支持的精确识别配置版本");
    if !value.enabled {
        return Ok(None);
    }
    crate::gpu_resource_policy::load(root)?;
    Ok(Some(
        value
            .managed
            .ok_or_else(|| anyhow!("缺少精确识别运行环境"))?,
    ))
}

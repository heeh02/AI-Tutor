//! Optional local Zipformer inference. Capture and full fallback audio stay in Rust.
use crate::local_asr::AsrClient;
use anyhow::{Result, anyhow, ensure};
use reqwest::{Client, RequestBuilder, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

const CHUNK_SAMPLES: usize = 320; // 20 ms at 16 kHz, matching the capture pump.
// Preserve the existing 800 ms audio backlog budget with smaller wire chunks.
const QUEUE_CHUNKS: usize = 16000 * 800 / 1000 / CHUNK_SAMPLES;
const MAX_RESPONSE: usize = 64 * 1024;
pub(super) const PROVIDER: &str = "sherpa-onnx / bilingual Zipformer / local streaming";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    protocol_version: u64,
    base_url: String,
    token: String,
}
impl Config {
    fn load(root: &Path) -> Result<Option<Self>> {
        let path = root.join("runtime/speech-service.json");
        if !path.exists() {
            return Ok(None);
        }
        ensure!(
            std::fs::metadata(&path)?.len() <= 4096,
            "本地流式语音配置过大"
        );
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| anyhow!("本地流式语音配置无效"))?;
        config.validate()?;
        Ok(Some(config))
    }
    fn validate(&self) -> Result<()> {
        let url = Url::parse(&self.base_url).map_err(|_| anyhow!("本地流式语音地址无效"))?;
        ensure!(
            self.protocol_version == 1
                && url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some_and(|port| port > 0)
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "流式识别只允许指定本机回环端口"
        );
        ensure!(
            (24..=256).contains(&self.token.len())
                && self
                    .token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
            "本地流式语音凭证无效"
        );
        Ok(())
    }
}

struct Control {
    cancelled: AtomicBool,
    failure: Mutex<Option<String>>,
    generation: Arc<AtomicU64>,
    token: u64,
    info: Arc<Mutex<Value>>,
}
impl Control {
    fn live(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
            && self.generation.load(Ordering::SeqCst) == self.token
    }
    fn update(&self, update: impl FnOnce(&mut Value)) {
        let mut info = self.info.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) == self.token {
            update(&mut info);
        }
    }
    fn update_live(&self, update: impl FnOnce(&mut Value)) {
        let mut info = self.info.lock().unwrap();
        if self.live() {
            update(&mut info);
        }
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let failed = self.failure.lock().unwrap().is_some();
        if !failed {
            self.update(|info| {
                info["asr_partial"] = Value::Null;
                info["asr_streaming_state"] = json!("idle");
            });
        }
    }
    fn fail(&self, reason: &str) {
        self.cancelled.store(true, Ordering::SeqCst);
        let mut failure = self.failure.lock().unwrap();
        if failure.is_none() {
            *failure = Some(reason.chars().take(240).collect());
        }
        let reason = failure.clone().unwrap();
        drop(failure);
        self.update(|info| {
            info["asr_partial"] = Value::Null;
            info["asr_streaming_state"] = json!("fallback");
            info["asr_fallback_reason"] = json!(reason);
            info["asr_provider"] = json!("whisper-rs / whisper.cpp / multilingual base / CPU");
        });
    }
}
enum Input {
    Chunk(u64, Vec<u8>),
    Finish(u64),
}

pub(super) struct CaptureStream {
    tx: mpsc::Sender<Input>,
    control: Arc<Control>,
    result: Option<oneshot::Receiver<Result<FinalText, String>>>,
    next_sample: u64,
    pcm: Vec<u8>,
    sequence: u64,
    handed_off: bool,
}
pub(super) struct FinalText {
    pub text: String,
    pub elapsed_ms: u64,
}
pub(super) struct FinalWait {
    result: oneshot::Receiver<Result<FinalText, String>>,
    control: Arc<Control>,
}
impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.pcm.fill(0);
        if !self.handed_off {
            self.control.cancel();
        }
    }
}
impl Drop for FinalWait {
    fn drop(&mut self) {
        self.control.cancel();
    }
}
impl CaptureStream {
    pub(super) fn pump(&mut self, samples: &[f32], rate: u32, flush: bool) {
        if !self.control.live() {
            return;
        }
        if rate == 0 {
            self.control.fail("麦克风采样率无效，改用完整音频离线转写");
            return;
        }
        let count = samples.len() as u64 * 16000 / rate as u64;
        while self.next_sample < count {
            let position = self.next_sample as f64 * rate as f64 / 16000.0;
            let left = position as usize;
            if left + 1 >= samples.len() && !flush {
                break;
            }
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            let sample = samples[left] * (1.0 - fraction) + samples[right] * fraction;
            let pcm = if sample.is_finite() {
                (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
            } else {
                0
            };
            self.pcm.extend_from_slice(&pcm.to_le_bytes());
            self.next_sample += 1;
            if self.pcm.len() == CHUNK_SAMPLES * 2 && !self.send_chunk() {
                return;
            }
        }
        if flush && !self.pcm.is_empty() {
            self.send_chunk();
        }
    }
    fn send_chunk(&mut self) -> bool {
        let bytes = std::mem::replace(&mut self.pcm, Vec::with_capacity(CHUNK_SAMPLES * 2));
        if let Err(error) = self.tx.try_send(Input::Chunk(self.sequence, bytes)) {
            if let Input::Chunk(_, mut bytes) = error.into_inner() {
                bytes.fill(0);
            }
            self.control
                .fail("流式识别来不及处理音频，改用完整音频离线转写");
            return false;
        }
        self.sequence += 1;
        true
    }
    pub(super) fn finish(mut self, samples: &[f32], rate: u32) -> FinalWait {
        self.pump(samples, rate, true);
        if self.control.live() {
            self.control
                .update(|info| info["asr_streaming_state"] = json!("finalizing"));
            if self.tx.try_send(Input::Finish(self.sequence)).is_err() {
                self.control
                    .fail("流式识别收尾队列已满，改用完整音频离线转写");
            }
        }
        self.handed_off = true;
        FinalWait {
            result: self.result.take().unwrap(),
            control: self.control.clone(),
        }
    }
}
impl FinalWait {
    pub(super) async fn wait(mut self) -> Result<FinalText, String> {
        let result = tokio::time::timeout(Duration::from_secs(7), &mut self.result).await;
        match result {
            Ok(Ok(result)) => result,
            _ => {
                self.control
                    .fail("流式识别收尾超时或线程退出，改用完整音频离线转写");
                Err("流式识别收尾未完成".into())
            }
        }
    }
}

pub(super) fn begin(
    legacy_root: Option<&Path>,
    managed: Option<Arc<AsrClient>>,
    info: Arc<Mutex<Value>>,
    generation: Arc<AtomicU64>,
    token: u64,
) -> Option<CaptureStream> {
    {
        let mut info = info.lock().unwrap();
        if generation.load(Ordering::SeqCst) != token {
            return None;
        }
        info["asr_partial"] = Value::Null;
        info["asr_fallback_reason"] = Value::Null;
        info["asr_streaming_state"] = json!("disabled");
        info["asr_provider"] = json!("whisper-rs / whisper.cpp / multilingual base / CPU");
    }
    if let Some(client) = managed {
        return Some(start(
            ConnectionConfig::Managed(client),
            info,
            generation,
            token,
        ));
    }
    let root = legacy_root?;
    match Config::load(root) {
        Ok(Some(config)) => Some(start(config, info, generation, token)),
        Ok(None) => None,
        Err(error) => {
            let mut info = info.lock().unwrap();
            if generation.load(Ordering::SeqCst) == token {
                info["asr_fallback_reason"] = json!(error.to_string());
                info["asr_streaming_state"] = json!("fallback");
            }
            None
        }
    }
}
fn start(
    config: impl Into<ConnectionConfig>,
    info: Arc<Mutex<Value>>,
    generation: Arc<AtomicU64>,
    token: u64,
) -> CaptureStream {
    let config = config.into();
    let control = Arc::new(Control {
        cancelled: AtomicBool::new(false),
        failure: Mutex::new(None),
        generation,
        token,
        info,
    });
    control.update(|info| {
        info["asr_streaming_state"] = json!("starting");
    });
    let (tx, rx) = mpsc::channel(QUEUE_CHUNKS);
    let (done, result) = oneshot::channel();
    let state = control.clone();
    tokio::spawn(async move {
        let result = worker(config, rx, state.clone()).await;
        if let Err(error) = &result {
            if state.live() {
                state.fail(error);
            }
        }
        let result = result.map_err(|error| state.failure.lock().unwrap().clone().unwrap_or(error));
        let _ = done.send(result);
    });
    CaptureStream {
        tx,
        control,
        result: Some(result),
        next_sample: 0,
        pcm: Vec::with_capacity(CHUNK_SAMPLES * 2),
        sequence: 0,
        handed_off: false,
    }
}

enum ConnectionConfig {
    Descriptor(Config),
    Managed(Arc<AsrClient>),
}
impl From<Config> for ConnectionConfig {
    fn from(config: Config) -> Self {
        Self::Descriptor(config)
    }
}
enum Connection {
    Descriptor(Config, Client),
    Managed(Arc<AsrClient>),
}
impl Connection {
    fn new(source: ConnectionConfig) -> Result<Self, String> {
        match source {
            ConnectionConfig::Managed(client) => Ok(Self::Managed(client)),
            ConnectionConfig::Descriptor(config) => Ok(Self::Descriptor(
                config,
                Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_millis(500))
                    .timeout(Duration::from_secs(2))
                    .build()
                    .map_err(|_| "无法建立本地流式识别连接".to_owned())?,
            )),
        }
    }
    fn post(&self, path: &str) -> RequestBuilder {
        match self {
            Self::Descriptor(config, http) => http
                .post(format!("{}{path}", config.base_url.trim_end_matches('/')))
                .bearer_auth(&config.token),
            Self::Managed(client) => client.post(path),
        }
    }
    fn managed(&self) -> Option<&AsrClient> {
        match self {
            Self::Managed(client) => Some(client),
            _ => None,
        }
    }
    fn validate(
        &self,
        value: &Value,
        session: &str,
        token: u64,
        sequence: Option<u64>,
        final_text: bool,
        revision: &mut u64,
        text: &mut String,
        control: &Control,
    ) -> Result<(), String> {
        let result = validate(value, session, token, sequence, final_text, revision, text);
        if result.is_err() && control.live() {
            if let Some(client) = self.managed() {
                client.mark_unavailable();
            }
        }
        result
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum ConnectionPolicy {
    Close,
    KeepAlive,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
struct ObservedTransport {
    connection_id: Option<u64>,
    request_id: Option<u64>,
    connection_policy: Option<ConnectionPolicy>,
    missing_header_fields: u8,
    invalid_header_fields: u8,
}

fn observed_transport(headers: &reqwest::header::HeaderMap) -> ObservedTransport {
    let mut result = ObservedTransport {
        connection_id: None,
        request_id: None,
        connection_policy: None,
        missing_header_fields: 0,
        invalid_header_fields: 0,
    };
    for (index, name) in ["x-asr-connection-id", "x-asr-request-id", "connection"]
        .iter()
        .enumerate()
    {
        let all = headers.get_all(*name);
        let mut values = all.iter();
        let Some(value) = values.next() else {
            result.missing_header_fields += 1;
            continue;
        };
        if values.next().is_some() {
            result.invalid_header_fields += 1;
            continue;
        }
        let Ok(value) = value.to_str() else {
            result.invalid_header_fields += 1;
            continue;
        };
        let valid = if index == 2 {
            result.connection_policy = if value.eq_ignore_ascii_case("close") {
                Some(ConnectionPolicy::Close)
            } else if value.eq_ignore_ascii_case("keep-alive") {
                Some(ConnectionPolicy::KeepAlive)
            } else {
                None
            };
            result.connection_policy.is_some()
        } else {
            let number = (!value.is_empty()
                && value.len() <= 20
                && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten()
            .filter(|number| *number > 0);
            if index == 0 {
                result.connection_id = number;
            } else {
                result.request_id = number;
            }
            number.is_some()
        };
        if !valid {
            result.invalid_header_fields += 1;
        }
    }
    result
}

#[derive(Debug)]
struct ObservedResponse {
    value: Value,
    #[cfg_attr(not(test), allow(dead_code))]
    // Only explicit diagnostics consume transport receipts.
    transport: ObservedTransport,
}

async fn response(
    request: RequestBuilder,
    control: &Control,
    managed: Option<&AsrClient>,
) -> Result<Value, String> {
    response_observed(request, control, managed, None)
        .await
        .map(|observed| observed.value)
}

// Keep URLs, credentials and raw error text out of observations. The optional
// diagnostic sink changes neither retries nor the production failure policy.
fn send_error_metadata(error: &reqwest::Error) -> Value {
    let mut io_errors = Vec::new();
    let mut cause = std::error::Error::source(error);
    let mut depth = 0;
    while let Some(current) = cause {
        if depth == 8 {
            break;
        }
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            io_errors
                .push(json!({"kind":format!("{:?}",io.kind()),"raw_os_error":io.raw_os_error()}));
        }
        depth += 1;
        cause = current.source();
    }
    json!({"is_timeout":error.is_timeout(),"is_connect":error.is_connect(),
        "is_request":error.is_request(),"is_body":error.is_body(),
        "is_decode":error.is_decode(),"is_builder":error.is_builder(),
        "http_status":error.status().map(|s|s.as_u16()),
        "source_chain_depth":depth,"source_chain_truncated":cause.is_some(),"io_errors":io_errors})
}

async fn response_observed(
    request: RequestBuilder,
    control: &Control,
    managed: Option<&AsrClient>,
    mut send_failure: Option<&mut Option<Value>>,
) -> Result<ObservedResponse, String> {
    let unavailable = || {
        if control.live() {
            if let Some(client) = managed {
                client.mark_unavailable();
            }
        }
    };
    let operation = async {
        let mut response = request.send().await.map_err(|error| {
            if let Some(sink) = send_failure.as_deref_mut() {
                *sink = Some(send_error_metadata(&error));
            }
            unavailable();
            "本地流式识别服务未响应".to_owned()
        })?;
        if !response.status().is_success() {
            if response.status().is_server_error() {
                unavailable();
            }
            return Err(format!(
                "本地流式识别返回状态 {}",
                response.status().as_u16()
            ));
        }
        let transport = observed_transport(response.headers());
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            unavailable();
            "本地流式识别响应中断".to_owned()
        })? {
            if bytes.len() + chunk.len() > MAX_RESPONSE {
                unavailable();
                return Err("本地流式识别响应超限".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| {
            unavailable();
            "本地流式识别响应格式无效".to_owned()
        })?;
        if let Some(client) = managed {
            client.check_instance(&value).map_err(|_| {
                unavailable();
                "本地流式识别实例不匹配".to_owned()
            })?;
        }
        Ok(ObservedResponse { value, transport })
    };
    tokio::pin!(operation);
    loop {
        if !control.live() {
            return Err("流式识别已取消".into());
        }
        tokio::select! {
            result = &mut operation => return result,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}
fn validate(
    value: &Value,
    session: &str,
    token: u64,
    sequence: Option<u64>,
    final_text: bool,
    revision: &mut u64,
    text: &mut String,
) -> Result<(), String> {
    if value["protocol_version"] != 1
        || value["session_id"] != session
        || value["recording_generation"].as_u64() != Some(token)
        || value["final"].as_bool() != Some(final_text)
        || sequence.is_some_and(|sequence| value["sequence"].as_u64() != Some(sequence))
    {
        return Err("流式识别会话或音频序号不匹配".into());
    }
    let next = value["revision"].as_u64().ok_or("流式识别缺少文本修订号")?;
    let transcript = value["text"].as_str().ok_or("流式识别缺少文本")?;
    if transcript.chars().count() > 8000
        || next < *revision
        || (transcript != text && next <= *revision)
        || (transcript == text && next != *revision)
    {
        return Err("流式识别文本修订无效或超限".into());
    }
    *revision = next;
    *text = transcript.to_owned();
    Ok(())
}
async fn worker(
    config: ConnectionConfig,
    mut rx: mpsc::Receiver<Input>,
    control: Arc<Control>,
) -> Result<FinalText, String> {
    let connection = Connection::new(config)?;
    let session = uuid::Uuid::new_v4().to_string();
    let path = format!("/v1/asr/{session}");
    let start = Instant::now();
    let run = async {
        let value = response(connection.post("/v1/asr/start")
            .json(&json!({"protocol_version":1,"session_id":session,"recording_generation":control.token,"sample_rate":16000,"channels":1,"encoding":"pcm_s16le","language":"auto","max_duration_ms":20000})), &control, connection.managed()).await?;
        let mut revision = 0;
        let mut text = String::new();
        connection.validate(&value, &session, control.token, None, false, &mut revision, &mut text, &control)?;
        control.update_live(|info| { info["asr_provider"] = json!(PROVIDER); info["asr_streaming_state"] = json!("streaming"); });
        let mut expected = 0;
        loop {
            if !control.live() { return Err("流式识别已取消".into()); }
            let input = tokio::select! {
                input = rx.recv() => input.ok_or("流式识别音频输入已关闭")?,
                _ = tokio::time::sleep(Duration::from_millis(20)) => continue,
            };
            match input {
                Input::Chunk(sequence, mut bytes) => {
                    if sequence != expected { bytes.fill(0); return Err("流式识别音频块不连续".into()); }
                    let value = response(connection.post(&format!("{path}/chunk?sequence={sequence}"))
                        .header("X-Recording-Generation", control.token).header("content-type", "application/octet-stream")
                        .body(bytes), &control, connection.managed()).await?;
                    connection.validate(&value, &session, control.token, Some(sequence), false, &mut revision, &mut text, &control)?;
                    expected += 1;
                    control.update_live(|info| info["asr_partial"] = json!({"recording_generation":control.token,"revision":revision,"text":text}));
                }
                Input::Finish(sequence) => {
                    if sequence != expected { return Err("流式识别收尾序号不匹配".into()); }
                    let value = response(connection.post(&format!("{path}/final"))
                        .timeout(Duration::from_secs(5)).json(&json!({"recording_generation":control.token,"sequence":sequence})), &control, connection.managed()).await?;
                    connection.validate(&value, &session, control.token, Some(sequence), true, &mut revision, &mut text, &control)?;
                    if !control.live() { return Err("流式识别已取消".into()); }
                    control.update_live(|info| { info["asr_partial"] = Value::Null; info["asr_streaming_state"] = json!("idle"); });
                    return Ok(FinalText { text, elapsed_ms: start.elapsed().as_millis() as u64 });
                }
            }
        }
    }.await;
    if run.is_err() {
        // UUID+generation cancellation can never release a later session. Its
        // own deadline is independent of the just-cancelled inference request.
        let _ = connection
            .post(&format!("{path}/cancel"))
            .timeout(Duration::from_millis(300))
            .json(&json!({"recording_generation":control.token}))
            .send()
            .await;
    }
    while let Ok(input) = rx.try_recv() {
        if let Input::Chunk(_, mut bytes) = input {
            bytes.fill(0);
        }
    }
    run
}

#[cfg(all(test, windows))]
#[path = "ascend_asr_diagnostic.rs"]
mod ascend_asr_diagnostic;

#[cfg(all(test, windows))]
#[path = "asr_connection_diagnostic.rs"]
mod asr_connection_diagnostic;

#[cfg(test)]
#[path = "streaming_asr_tests.rs"]
pub(crate) mod tests;

#[cfg(all(test, windows))]
#[path = "streaming_asr_managed_diagnostic.rs"]
mod managed_diagnostic;

#[cfg(all(test, windows))]
#[path = "voice_model_chain_diagnostic.rs"]
pub(crate) mod voice_model_chain_diagnostic;

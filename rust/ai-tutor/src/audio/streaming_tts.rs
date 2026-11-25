//! Local PCM transport. The Rust audio owner, never the model service, owns output.
//! Receiving PCM is not a playback receipt. Callers must not replay via fallback
//! after any samples have actually been presented to the output device.
use anyhow::{Result, anyhow, ensure};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
#[path = "tts_voice.rs"]
mod tts_voice;

pub const SAMPLE_RATE: u32 = 24_000;
pub const PACKET_SAMPLES: usize = 480;
pub const QUEUE_CAPACITY: usize = 24;
const MAX_CHUNK_BYTES: usize = 256 * 1024;
const MAX_AUDIO_BYTES: usize = SAMPLE_RATE as usize * 2 * 90;
pub const WARM_FIXTURE_VERSION: &str = "local-speech-warm-v1";
const WARM_ZH: &str = "这是一次本地语音准备测试。";
const WARM_EN: &str = "This is a local voice preparation test.";
#[derive(Clone, Copy)]
struct RequestLimits {
    total: Duration,
    response: Duration,
    chunk: Duration,
}
const FOREGROUND_LIMITS: RequestLimits = RequestLimits {
    total: Duration::from_secs(90),
    response: Duration::from_secs(15),
    chunk: Duration::from_secs(15),
};
const WARM_LIMITS: RequestLimits = RequestLimits {
    total: Duration::from_secs(120),
    response: Duration::from_secs(120),
    chunk: Duration::from_secs(120),
};

pub(crate) async fn wait_cancelled(epoch: &AtomicU64, expected: u64) {
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tick.tick().await;
        if epoch.load(Ordering::SeqCst) != expected {
            return;
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    protocol_version: u32,
    endpoint: String,
    bearer_token: String,
    model: String,
    #[serde(default)]
    references: Option<tts_voice::References>,
}

pub struct TtsClient {
    client: reqwest::Client,
    descriptor: Descriptor,
    voice: tts_voice::Voice,
    configuration_sha256: String,
}

#[derive(Debug)]
pub enum SynthesisOutcome {
    Completed(Value),
    Cancelled,
}

impl TtsClient {
    pub fn from_descriptor(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_configuration(&bytes, path.parent().unwrap_or(Path::new(".")))
    }

    /// Managed credentials remain in memory; both entry points share the same
    /// endpoint, model and content-bound reference validation.
    pub(crate) fn from_configuration(bytes: &[u8], directory: &Path) -> Result<Self> {
        let configuration_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let mut descriptor: Descriptor = serde_json::from_slice(&bytes)?;
        ensure!(
            descriptor.protocol_version == 1,
            "Unsupported local TTS protocol"
        );
        let url = reqwest::Url::parse(&descriptor.endpoint)?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some()
                && url.path() == "/v1/audio/speech"
                && url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "Local TTS must use its loopback speech endpoint"
        );
        ensure!(
            (16..=256).contains(&descriptor.bearer_token.len())
                && descriptor
                    .bearer_token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid local TTS credential"
        );
        let voice =
            tts_voice::Voice::load(&descriptor.model, descriptor.references.take(), directory)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .build()?;
        Ok(Self {
            client,
            descriptor,
            voice,
            configuration_sha256,
        })
    }

    pub fn engine_kind(&self) -> &'static str {
        self.voice.engine_kind()
    }
    pub fn provider_name(&self) -> &'static str {
        self.voice.provider_name()
    }
    pub fn model_name(&self) -> &str {
        &self.descriptor.model
    }
    pub fn configuration_sha256(&self) -> &str {
        &self.configuration_sha256
    }

    /// Protocol readiness is separate from running any synthetic inference.
    /// No process-instance identity is inferred from a successful HTTP response.
    pub async fn check_ready(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut url = reqwest::Url::parse(&self.descriptor.endpoint)?;
            url.set_path("/health");
            let health = self
                .client
                .get(url.clone())
                .bearer_auth(&self.descriptor.bearer_token)
                .send()
                .await?;
            ensure!(
                health.status().is_success(),
                "Local TTS health check failed"
            );
            url.set_path("/v1/models");
            let mut response = self
                .client
                .get(url)
                .bearer_auth(&self.descriptor.bearer_token)
                .send()
                .await?;
            ensure!(
                response.status().is_success(),
                "Local TTS model check failed"
            );
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    bytes.len() + chunk.len() <= 64 * 1024,
                    "Local TTS model list exceeds its bound"
                );
                bytes.extend_from_slice(&chunk);
            }
            let models: Value = serde_json::from_slice(&bytes)?;
            ensure!(
                models["data"].as_array().is_some_and(|items| items
                    .iter()
                    .any(|item| item["id"] == self.descriptor.model)),
                "Configured local TTS model is unavailable"
            );
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow!("Local TTS readiness check timed out"))?
    }

    /// Cold-service preparation only. The fixed fixture cannot contain learner
    /// text, and a caller must drain this bounded channel without playing it.
    pub async fn warm_synthetic(
        &self,
        language: &str,
        epoch: Arc<AtomicU64>,
        expected: u64,
        packets: mpsc::Sender<Vec<i16>>,
    ) -> Result<SynthesisOutcome> {
        let text = match language {
            "zh" => WARM_ZH,
            "en" => WARM_EN,
            _ => anyhow::bail!("Warmup language must be zh or en"),
        };
        ensure!(
            packets.max_capacity() <= QUEUE_CAPACITY,
            "Warmup PCM queue exceeds its bound"
        );
        self.request(text, language, epoch, expected, packets, WARM_LIMITS)
            .await
    }

    /// Output packets contain mono signed PCM16 at 24 kHz, at most 20 ms each.
    /// The consumer must use a bounded queue and recheck its playback epoch.
    pub async fn synthesize(
        &self,
        text: &str,
        language: &str,
        speed: &str,
        epoch: Arc<AtomicU64>,
        expected: u64,
        packets: mpsc::Sender<Vec<i16>>,
    ) -> Result<SynthesisOutcome> {
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 320,
            "TTS expects one bounded teaching segment"
        );
        ensure!(
            ["zh", "zh-CN", "en", "en-US"].contains(&language),
            "Unsupported teaching language"
        );
        // This backend rejects non-native speed adjustment while streaming.
        // Preserve the setting through the existing voice path until real DSP is integrated.
        ensure!(
            speed == "normal",
            "Local streaming voice does not yet support this speed"
        );
        ensure!(
            packets.max_capacity() <= QUEUE_CAPACITY,
            "PCM queue exceeds the 480ms transport bound"
        );
        self.request(text, language, epoch, expected, packets, FOREGROUND_LIMITS)
            .await
    }

    async fn request(
        &self,
        text: &str,
        language: &str,
        epoch: Arc<AtomicU64>,
        expected: u64,
        packets: mpsc::Sender<Vec<i16>>,
        limits: RequestLimits,
    ) -> Result<SynthesisOutcome> {
        if epoch.load(Ordering::SeqCst) != expected {
            return Ok(SynthesisOutcome::Cancelled);
        }
        let run = self.run(text, language, packets, limits);
        tokio::select! {
            biased;
            _ = wait_cancelled(&epoch, expected) => Ok(SynthesisOutcome::Cancelled),
            result = tokio::time::timeout(limits.total, run) => {
                let metrics = result.map_err(|_| anyhow!("Local TTS request timed out"))??;
                if epoch.load(Ordering::SeqCst) != expected { Ok(SynthesisOutcome::Cancelled) }
                else { Ok(SynthesisOutcome::Completed(metrics)) }
            }
        }
    }

    async fn run(
        &self,
        text: &str,
        language: &str,
        packets: mpsc::Sender<Vec<i16>>,
        limits: RequestLimits,
    ) -> Result<Value> {
        let start = Instant::now();
        let mut request = self.voice.fields(language);
        request.as_object_mut().unwrap().extend(
            json!({"model":self.descriptor.model,"input":text,
            "stream":true,"stream_format":"audio","response_format":"pcm","speed":1.0})
            .as_object()
            .unwrap()
            .clone(),
        );
        let response = tokio::time::timeout(
            limits.response,
            self.client
                .post(&self.descriptor.endpoint)
                .bearer_auth(&self.descriptor.bearer_token)
                .json(&request)
                .send(),
        )
        .await
        .map_err(|_| anyhow!("Local TTS response timed out"))??;
        ensure!(
            response.status().is_success(),
            "Local TTS returned HTTP {}",
            response.status().as_u16()
        );
        let media = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim();
        validate_pcm_media(media)?;
        let mut stream = response.bytes_stream();
        let mut decoder = PcmDecoder::default();
        let (mut received, mut chunks, mut emitted, mut first_pcm_ms) =
            (0usize, 0usize, 0usize, None);
        loop {
            let chunk = tokio::time::timeout(limits.chunk, stream.next())
                .await
                .map_err(|_| anyhow!("Local TTS PCM stream stalled"))?;
            let Some(chunk) = chunk else {
                break;
            };
            let chunk = chunk?;
            ensure!(
                chunk.len() <= MAX_CHUNK_BYTES,
                "Local TTS chunk exceeds its bound"
            );
            received += chunk.len();
            ensure!(
                received <= MAX_AUDIO_BYTES,
                "Local TTS audio exceeds 90 seconds"
            );
            chunks += 1;
            if received >= 2 {
                first_pcm_ms.get_or_insert(start.elapsed().as_secs_f64() * 1000.0);
            }
            // One bounded raw chunk stays resident. Decode at most one packet
            // before each backpressure point, rather than the entire chunk.
            for bytes in chunk.chunks(PACKET_SAMPLES * 2) {
                for packet in decoder.push(bytes) {
                    emitted += packet.len();
                    packets
                        .send(packet)
                        .await
                        .map_err(|_| anyhow!("PCM consumer closed"))?;
                }
            }
        }
        if let Some(packet) = decoder.finish()? {
            emitted += packet.len();
            packets
                .send(packet)
                .await
                .map_err(|_| anyhow!("PCM consumer closed"))?;
        }
        ensure!(emitted > 0, "Local TTS returned no PCM");
        Ok(
            json!({"provider":self.provider_name(),"sample_rate":SAMPLE_RATE,"samples":emitted,
            "network_chunks":chunks,"first_pcm_available_ms":first_pcm_ms,
            "transport_completed_ms":start.elapsed().as_secs_f64()*1000.0,"speaker_output_measured":false,
            "transport_queue_capacity_packets":packets.max_capacity(),"max_raw_chunk_bytes":MAX_CHUNK_BYTES,
            "max_decoder_pending_samples":PACKET_SAMPLES-1,"packet_max_samples":PACKET_SAMPLES}),
        )
    }
}

fn validate_pcm_media(header: &str) -> Result<()> {
    let mut parts = header.split(';');
    let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    ensure!(
        ["audio/pcm", "application/octet-stream"].contains(&media.as_str()),
        "Local TTS did not return agreed PCM16LE"
    );
    for parameter in parts {
        if let Some((key, value)) = parameter.trim().split_once('=') {
            let value = value.trim().trim_matches('"');
            match key.trim().to_ascii_lowercase().as_str() {
                "rate" | "sample_rate" => ensure!(
                    value == "24000",
                    "Local TTS sample rate conflicts with 24kHz contract"
                ),
                "channels" => ensure!(value == "1", "Local TTS must return mono PCM"),
                _ => {}
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct PcmDecoder {
    low: Option<u8>,
    pending: Vec<i16>,
}
impl PcmDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<i16>> {
        let mut packets = Vec::new();
        for byte in bytes {
            if let Some(low) = self.low.take() {
                self.pending.push(i16::from_le_bytes([low, *byte]));
                if self.pending.len() == PACKET_SAMPLES {
                    packets.push(std::mem::replace(
                        &mut self.pending,
                        Vec::with_capacity(PACKET_SAMPLES),
                    ));
                }
            } else {
                self.low = Some(*byte);
            }
        }
        packets
    }
    fn finish(self) -> Result<Option<Vec<i16>>> {
        ensure!(self.low.is_none(), "Local TTS ended mid-sample");
        Ok((!self.pending.is_empty()).then_some(self.pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Response, routing::post};

    #[tokio::test]
    async fn cold_warmup_has_separate_header_and_chunk_limits_without_relaxing_foreground()
    -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        assert_eq!(FOREGROUND_LIMITS.response, Duration::from_secs(15));
        assert_eq!(FOREGROUND_LIMITS.chunk, Duration::from_secs(15));
        assert_eq!(FOREGROUND_LIMITS.total, Duration::from_secs(90));
        assert_eq!(WARM_LIMITS.total, Duration::from_secs(120));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let server = tokio::spawn(async move {
            let mut children = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                children.push(tokio::spawn(async move {
                    let mut request=[0u8;4096]; let _=stream.read(&mut request).await;
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/pcm\r\nContent-Length: 960\r\nConnection: close\r\n\r\n").await.is_err() {return;}
                    let _=stream.flush().await;
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    let _=stream.write_all(&[0u8;960]).await;
                }));
            }
            for child in children {
                let _ = child.await;
            }
        });
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("tts.json");
        std::fs::write(&path,json!({"protocol_version":1,"endpoint":format!("http://127.0.0.1:{port}/v1/audio/speech"),
            "bearer_token":"synthetic-delay-credential","model":"tutor-qwen-tts"}).to_string())?;
        let client = TtsClient::from_descriptor(&path)?;
        let epoch = Arc::new(AtomicU64::new(1));
        for (response, chunk, succeeds) in [(20, 400, false), (400, 400, true), (400, 20, false)] {
            let (send, mut receive) = mpsc::channel(1);
            let limits = RequestLimits {
                total: Duration::from_secs(2),
                response: Duration::from_millis(response),
                chunk: Duration::from_millis(chunk),
            };
            let (result, samples) = tokio::join!(
                client.request(WARM_EN, "en", epoch.clone(), 1, send, limits),
                async {
                    let mut samples = 0;
                    while let Some(packet) = receive.recv().await {
                        samples += packet.len();
                    }
                    samples
                }
            );
            assert_eq!(result.is_ok(), succeeds);
            assert_eq!(samples, if succeeds { 480 } else { 0 });
        }
        server.await?;
        Ok(())
    }

    #[test]
    fn pcm_contract_rejects_l16_and_conflicting_rate_or_channels() -> Result<()> {
        validate_pcm_media("audio/pcm; rate=24000; channels=1")?;
        validate_pcm_media("application/octet-stream")?;
        assert!(validate_pcm_media("audio/L16; rate=24000; channels=1").is_err());
        assert!(validate_pcm_media("audio/pcm; rate=48000").is_err());
        assert!(validate_pcm_media("audio/pcm; channels=2").is_err());
        Ok(())
    }
    #[test]
    fn largest_raw_chunk_is_decoded_one_packet_at_each_backpressure_point() -> Result<()> {
        let bytes = vec![0u8; MAX_CHUNK_BYTES];
        let mut decoder = PcmDecoder::default();
        let mut samples = 0;
        for chunk in bytes.chunks(PACKET_SAMPLES * 2) {
            let packets = decoder.push(chunk);
            assert!(packets.len() <= 1);
            samples += packets.iter().map(Vec::len).sum::<usize>();
            assert!(decoder.pending.len() < PACKET_SAMPLES);
        }
        samples += decoder.finish()?.map_or(0, |packet| packet.len());
        assert_eq!(samples, MAX_CHUNK_BYTES / 2);
        Ok(())
    }

    #[test]
    fn pcm_network_splits_keep_every_signed_sample_and_reject_truncation() -> Result<()> {
        let expected: Vec<i16> = (0..1091).map(|i| (i * 53) as i16).collect();
        let bytes: Vec<u8> = expected.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut decoder = PcmDecoder::default();
        let mut packets = vec![];
        for chunk in bytes.chunks(67) {
            packets.extend(decoder.push(chunk));
        }
        packets.extend(decoder.finish()?);
        assert!(packets.iter().all(|p| p.len() <= PACKET_SAMPLES));
        assert_eq!(packets.into_iter().flatten().collect::<Vec<_>>(), expected);
        let mut odd = PcmDecoder::default();
        odd.push(&[1]);
        assert!(odd.finish().is_err());
        Ok(())
    }

    async fn fixture() -> Result<(tempfile::TempDir, TtsClient, tokio::task::JoinHandle<()>)> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let router = Router::new().route(
            "/v1/audio/speech",
            post(|| async {
                Response::builder()
                    .header("content-type", "audio/pcm")
                    .body(Body::from(vec![0u8; SAMPLE_RATE as usize * 2]))
                    .unwrap()
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("tts.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"protocol_version":1,
            "endpoint":format!("http://127.0.0.1:{port}/v1/audio/speech"),
            "bearer_token":"synthetic-credential","model":"tutor-qwen-tts"}))?,
        )?;
        Ok((dir, TtsClient::from_descriptor(&path)?, task))
    }

    #[tokio::test]
    async fn cancellation_interrupts_full_pcm_queue_without_waiting_for_consumer() -> Result<()> {
        let (_dir, client, server) = fixture().await?;
        let epoch = Arc::new(AtomicU64::new(1));
        let worker_epoch = epoch.clone();
        let (send, mut receive) = mpsc::channel(1);
        let job = tokio::spawn(async move {
            client
                .synthesize("Synthetic fixture.", "en", "normal", worker_epoch, 1, send)
                .await
        });
        assert_eq!(receive.recv().await.unwrap().len(), PACKET_SAMPLES);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(receive.len(), 1);
        epoch.store(2, Ordering::SeqCst);
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(250), job).await???,
            SynthesisOutcome::Cancelled
        ));
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn normal_stream_is_complete_and_non_normal_speed_is_never_ignored() -> Result<()> {
        let (_dir, client, server) = fixture().await?;
        let epoch = Arc::new(AtomicU64::new(1));
        let (send, mut receive) = mpsc::channel(QUEUE_CAPACITY);
        assert!(
            client
                .synthesize("Synthetic.", "en", "slow", epoch.clone(), 1, send.clone())
                .await
                .is_err()
        );
        let job = tokio::spawn(async move {
            client
                .synthesize("Synthetic.", "en", "normal", epoch, 1, send)
                .await
        });
        let mut samples = 0;
        while let Some(packet) = receive.recv().await {
            samples += packet.len();
        }
        assert_eq!(samples, SAMPLE_RATE as usize);
        assert!(matches!(job.await??, SynthesisOutcome::Completed(_)));
        server.abort();
        Ok(())
    }
}

use super::*;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::post,
};

const TOKEN: &str = "synthetic-local-test-token-123456";
#[derive(Default)]
struct Service {
    session: String,
    generation: u64,
    sequence: u64,
    revision: u64,
    active: bool,
    text: String,
    bytes: Vec<u8>,
    cancels: Vec<(String, u64)>,
    fail_chunk: bool,
    managed_instance: Option<String>,
    forced_status: Option<StatusCode>,
    wrong_instance: bool,
    starts: usize,
    finals: usize,
}
impl Service {
    fn response(&self, final_text: bool) -> Value {
        json!({"protocol_version":1,"session_id":self.session,"recording_generation":self.generation,"revision":self.revision,"text":self.text,"final":final_text})
    }
}
async fn endpoint(
    State(state): State<Arc<Mutex<Service>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    assert_eq!(
        headers.get("authorization").unwrap().to_str().unwrap(),
        format!("Bearer {TOKEN}")
    );
    assert!(headers.get("origin").is_none());
    let mut state = state.lock().unwrap();
    if let Some(instance) = &state.managed_instance {
        assert_eq!(
            headers.get("x-asr-instance").unwrap().to_str().unwrap(),
            instance
        );
    }
    if let Some(status) = state.forced_status {
        return status.into_response();
    }
    let path: Vec<_> = uri.path().trim_matches('/').split('/').collect();
    let mut value = if path == ["v1", "asr", "start"] {
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["sample_rate"], 16000);
        assert_eq!(request["encoding"], "pcm_s16le");
        assert_eq!(request["max_duration_ms"], 20000);
        if state.active {
            return StatusCode::CONFLICT.into_response();
        }
        state.session = request["session_id"].as_str().unwrap().into();
        state.generation = request["recording_generation"].as_u64().unwrap();
        state.sequence = 0;
        state.revision = 0;
        state.text.clear();
        state.bytes.clear();
        state.active = true;
        state.starts += 1;
        state.response(false)
    } else if path.len() == 4 && path[3] == "cancel" {
        let request: Value = serde_json::from_slice(&body).unwrap();
        let token = request["recording_generation"].as_u64().unwrap();
        if state.session == path[2] && state.generation == token {
            state.active = false;
        }
        state.cancels.push((path[2].into(), token));
        json!({"protocol_version":1,"session_id":path[2],"recording_generation":token,"canceled":true})
    } else if path.len() == 4 && path[3] == "chunk" {
        if state.fail_chunk {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        assert_eq!(path[2], state.session);
        assert_eq!(
            headers["x-recording-generation"]
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            state.generation
        );
        let sequence: u64 = uri
            .query()
            .unwrap()
            .strip_prefix("sequence=")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(sequence, state.sequence);
        assert!(body.len() <= 3200 && body.len() % 2 == 0 && !body.is_empty());
        state.sequence += 1;
        state.revision += 1;
        state.text = format!("片段{}", state.sequence);
        state.bytes.extend_from_slice(&body);
        let mut value = state.response(false);
        value["sequence"] = json!(sequence);
        value
    } else if path.len() == 4 && path[3] == "final" {
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(path[2], state.session);
        assert_eq!(request["recording_generation"], state.generation);
        assert_eq!(request["sequence"], state.sequence);
        state.text = "最终 editable text".into();
        state.revision += 1;
        state.active = false;
        state.finals += 1;
        let mut value = state.response(true);
        value["sequence"] = json!(state.sequence);
        value
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    value["test_service"] = json!(true);
    if let Some(instance) = &state.managed_instance {
        value["instance_id"] = json!(if state.wrong_instance {
            "old-instance"
        } else {
            instance
        });
    }
    axum::Json(value).into_response()
}
pub(crate) struct Fixture {
    config: Config,
    state: Arc<Mutex<Service>>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    pub(crate) fn received_pcm(&self) -> Vec<u8> {
        self.state.lock().unwrap().bytes.clone()
    }

    pub(crate) fn managed_client(&self) -> Arc<AsrClient> {
        let instance = "a".repeat(32);
        self.state.lock().unwrap().managed_instance = Some(instance.clone());
        Arc::new(AsrClient::new(instance, self.config.base_url.clone(), TOKEN.into()).unwrap())
    }
    pub(crate) fn request_counts(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.starts, state.finals)
    }
}

#[tokio::test]
async fn managed_service_conflict_falls_back_without_destroying_instance() {
    let fixture = Fixture::new().await;
    let client = fixture.managed_client();
    fixture.state.lock().unwrap().forced_status = Some(StatusCode::CONFLICT);
    let info = info();
    let samples = vec![0.2; 8000];
    let stream = start(
        ConnectionConfig::Managed(client.clone()),
        info.clone(),
        Arc::new(AtomicU64::new(1)),
        1,
    );
    until(|| info.lock().unwrap()["asr_streaming_state"] == "fallback").await;
    assert!(stream.finish(&samples, 16000).wait().await.is_err());
    assert!(!client.unavailable());
    assert_eq!(samples, vec![0.2; 8000]); // Full recording remains available for Whisper.
}

#[tokio::test]
async fn managed_service_failure_and_wrong_instance_invalidate_only_that_client() {
    for wrong_instance in [false, true] {
        let fixture = Fixture::new().await;
        let client = fixture.managed_client();
        if wrong_instance {
            fixture.state.lock().unwrap().wrong_instance = true;
        } else {
            fixture.state.lock().unwrap().forced_status = Some(StatusCode::SERVICE_UNAVAILABLE);
        }
        let info = info();
        let samples = vec![0.2; 8000];
        let stream = start(
            ConnectionConfig::Managed(client.clone()),
            info.clone(),
            Arc::new(AtomicU64::new(1)),
            1,
        );
        until(|| info.lock().unwrap()["asr_streaming_state"] == "fallback").await;
        assert!(client.unavailable());
        assert!(stream.finish(&samples, 16000).wait().await.is_err());
        assert_eq!(samples.len(), 8000);
    }
}

#[tokio::test]
async fn managed_user_cancel_preserves_service_for_next_recording() {
    let fixture = Fixture::new().await;
    let client = fixture.managed_client();
    let info = info();
    let generation = Arc::new(AtomicU64::new(1));
    let stream = start(
        ConnectionConfig::Managed(client.clone()),
        info.clone(),
        generation.clone(),
        1,
    );
    until(|| info.lock().unwrap()["asr_streaming_state"] == "streaming").await;
    generation.store(2, Ordering::SeqCst);
    drop(stream);
    until(|| !fixture.state.lock().unwrap().cancels.is_empty()).await;
    assert!(!client.unavailable());
}
impl Fixture {
    pub(crate) async fn new() -> Self {
        let state = Arc::new(Mutex::new(Service::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = Config {
            protocol_version: 1,
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            token: TOKEN.into(),
        };
        let app = Router::new()
            .route("/{*path}", post(endpoint))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            config,
            state,
            task,
        }
    }
    fn begin(
        &self,
        info: Arc<Mutex<Value>>,
        generation: Arc<AtomicU64>,
        token: u64,
    ) -> CaptureStream {
        start(
            Config {
                protocol_version: 1,
                base_url: self.config.base_url.clone(),
                token: TOKEN.into(),
            },
            info,
            generation,
            token,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn info() -> Arc<Mutex<Value>> {
    Arc::new(Mutex::new(json!({})))
}
fn detached_stream() -> (CaptureStream, mpsc::Receiver<Input>) {
    let control = Arc::new(Control {
        cancelled: AtomicBool::new(false),
        failure: Mutex::new(None),
        generation: Arc::new(AtomicU64::new(1)),
        token: 1,
        info: info(),
    });
    let (tx, rx) = mpsc::channel(QUEUE_CHUNKS);
    let (_, result) = oneshot::channel();
    (
        CaptureStream {
            tx,
            control,
            result: Some(result),
            next_sample: 0,
            pcm: Vec::new(),
            sequence: 0,
            handed_off: false,
        },
        rx,
    )
}

#[test]
fn incremental_resampling_preserves_full_audio_across_capture_and_wire_boundaries() {
    let rate = 44100;
    let samples: Vec<f32> = (0..44541)
        .map(|i| ((i as f32) * 0.017).sin() * 0.5)
        .collect();
    let (mut stream, mut rx) = detached_stream();
    let mut pcm = Vec::new();
    let mut sequences = Vec::new();
    for count in (882..=samples.len()).step_by(882) {
        stream.pump(&samples[..count], rate, false);
        while let Ok(Input::Chunk(sequence, bytes)) = rx.try_recv() {
            sequences.push(sequence);
            pcm.extend(bytes);
        }
    }
    let _wait = stream.finish(&samples, rate);
    while let Ok(input) = rx.try_recv() {
        match input {
            Input::Chunk(sequence, bytes) => {
                sequences.push(sequence);
                pcm.extend(bytes);
            }
            Input::Finish(sequence) => assert_eq!(sequence, 51),
        }
    }
    let expected: Vec<u8> = crate::audio::resample(&samples, rate, 16000)
        .into_iter()
        .flat_map(|v| ((v * 32767.0).round() as i16).to_le_bytes())
        .collect();
    assert_eq!(pcm, expected);
    assert_eq!(sequences, (0..51).collect::<Vec<_>>());
    assert_eq!(pcm.len(), 16160 * 2); // Includes the final half-chunk.
}

#[test]
fn partial_audio_is_forwarded_before_the_previous_100ms_batch_boundary() {
    let (mut stream, mut rx) = detached_stream();
    // Include the interpolation look-ahead sample, as the capture buffer does.
    stream.pump(&vec![0.25; 321], 16000, false);
    let Input::Chunk(sequence, bytes) = rx.try_recv().expect("first 20ms chunk") else {
        panic!("expected audio")
    };
    assert_eq!(sequence, 0);
    assert_eq!(bytes.len(), 640);
    assert!(rx.try_recv().is_err());
    assert!(stream.control.live());
}

#[test]
fn full_stream_queue_reports_fallback_without_mutating_retained_recording() {
    let (mut stream, _rx) = detached_stream();
    let samples = vec![0.25; 16000];
    stream.pump(&samples, 16000, false);
    assert!(!stream.control.live());
    assert!(stream.control.failure.lock().unwrap().is_some());
    assert_eq!(
        stream.control.info.lock().unwrap()["asr_streaming_state"],
        "fallback"
    );
    assert_eq!(samples, vec![0.25; 16000]);
    assert!(stream.sequence <= QUEUE_CHUNKS as u64);
}

#[tokio::test]
async fn loopback_partial_and_final_are_bound_to_generation_and_complete_pcm() {
    let fixture = Fixture::new().await;
    let info = info();
    let generation = Arc::new(AtomicU64::new(7));
    let mut stream = fixture.begin(info.clone(), generation, 7);
    let samples = vec![0.25; 4800];
    stream.pump(&samples, 16000, false);
    until(|| {
        info.lock().unwrap()["asr_partial"]["revision"]
            .as_u64()
            .is_some_and(|v| v >= 2)
    })
    .await;
    assert_eq!(
        info.lock().unwrap()["asr_partial"]["recording_generation"],
        7
    );
    let result = stream.finish(&samples, 16000).wait().await.unwrap();
    assert_eq!(result.text, "最终 editable text");
    assert_eq!(fixture.state.lock().unwrap().bytes.len(), samples.len() * 2);
    assert!(!fixture.state.lock().unwrap().active);
    assert!(info.lock().unwrap()["asr_partial"].is_null());
}

#[tokio::test]
async fn service_error_cancels_only_its_session_and_returns_explicit_fallback() {
    let fixture = Fixture::new().await;
    fixture.state.lock().unwrap().fail_chunk = true;
    let info = info();
    let samples = vec![0.5; 3200];
    let mut stream = fixture.begin(info.clone(), Arc::new(AtomicU64::new(1)), 1);
    stream.pump(&samples, 16000, false);
    until(|| !fixture.state.lock().unwrap().cancels.is_empty()).await;
    let result = stream.finish(&samples, 16000).wait().await;
    assert!(result.is_err());
    assert!(
        info.lock().unwrap()["asr_fallback_reason"]
            .as_str()
            .unwrap()
            .contains("502")
    );
    assert_eq!(info.lock().unwrap()["asr_streaming_state"], "fallback");
    assert_eq!(samples, vec![0.5; 3200]);
    assert!(!fixture.state.lock().unwrap().active);
}

#[tokio::test]
async fn discarded_old_capture_cannot_clear_new_partial_or_cancel_new_session() {
    let fixture = Fixture::new().await;
    let info = info();
    let generation = Arc::new(AtomicU64::new(1));
    let stream = fixture.begin(info.clone(), generation.clone(), 1);
    until(|| fixture.state.lock().unwrap().active).await;
    generation.store(2, Ordering::SeqCst);
    {
        let mut current = fixture.state.lock().unwrap();
        current.session = "synthetic-new-session".into();
        current.generation = 2;
    }
    info.lock().unwrap()["asr_partial"] =
        json!({"recording_generation":2,"revision":1,"text":"new"});
    drop(stream);
    until(|| !fixture.state.lock().unwrap().cancels.is_empty()).await;
    assert_eq!(fixture.state.lock().unwrap().cancels[0].1, 1);
    assert!(fixture.state.lock().unwrap().active);
    assert_eq!(info.lock().unwrap()["asr_partial"]["text"], "new");
}

#[test]
fn protocol_rejects_remote_endpoints_wrong_identity_revisions_and_excess_text() {
    for base_url in [
        "https://127.0.0.1:1234",
        "http://localhost:1234",
        "http://example.com:1234",
        "http://127.0.0.1:1234/path",
        "http://user@127.0.0.1:1234",
    ] {
        assert!(
            Config {
                protocol_version: 1,
                base_url: base_url.into(),
                token: TOKEN.into()
            }
            .validate()
            .is_err()
        );
    }
    let base = json!({"protocol_version":1,"session_id":"session","recording_generation":1,"sequence":0,"revision":1,"text":"hello","final":false});
    assert!(
        validate(
            &base,
            "session",
            1,
            Some(0),
            false,
            &mut 0,
            &mut String::new()
        )
        .is_ok()
    );
    for (field, value) in [
        ("recording_generation", json!(2)),
        ("sequence", json!(1)),
        ("revision", json!(0)),
        ("text", json!("x".repeat(8001))),
    ] {
        let mut invalid = base.clone();
        invalid[field] = value;
        assert!(
            validate(
                &invalid,
                "session",
                1,
                Some(0),
                false,
                &mut 0,
                &mut String::new()
            )
            .is_err()
        );
    }
    assert!(
        validate(
            &base,
            "session",
            1,
            Some(0),
            false,
            &mut 1,
            &mut "different".into()
        )
        .is_err()
    );
}

fn fixed_pcm16(path: &std::path::Path) -> Vec<f32> {
    let mut wav = std::fs::read(path).unwrap();
    assert!(wav.len() <= 1_000_000 && wav.len() >= 12);
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    let mut offset = 12;
    let mut format = None;
    let mut data = None;
    while offset + 8 <= wav.len() {
        let length = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start + length;
        assert!(end <= wav.len());
        if &wav[offset..offset + 4] == b"fmt " && length >= 16 {
            format = Some((
                u16::from_le_bytes(wav[start..start + 2].try_into().unwrap()),
                u16::from_le_bytes(wav[start + 2..start + 4].try_into().unwrap()),
                u32::from_le_bytes(wav[start + 4..start + 8].try_into().unwrap()),
                u16::from_le_bytes(wav[start + 14..start + 16].try_into().unwrap()),
            ));
        } else if &wav[offset..offset + 4] == b"data" {
            data = Some((start, end));
        }
        offset = end + length % 2;
    }
    assert_eq!(format, Some((1, 1, 16000, 16)));
    let (start, end) = data.unwrap();
    assert!((end - start) % 2 == 0 && end - start <= 640000);
    let samples = wav[start..end]
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
        .collect();
    wav.fill(0);
    samples
}

#[tokio::test]
#[ignore = "Explicit local Zipformer service and existing fixed SAPI corpus only; no devices"]
async fn real_local_service_fixed_synthetic_wav_pipeline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let corpus = root.join("runtime/goal-baselines/20261103/sapi-corpus");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(corpus.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["audible_output"], false);
    let mut results = Vec::new();
    for row in manifest["samples"].as_array().unwrap() {
        assert_eq!(row["source_type"], "synthetic_sapi_adult_voice");
        let id = row["id"].as_str().unwrap();
        assert!(id.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'_'));
        let source = fixed_pcm16(&corpus.join(format!("{id}.wav")));
        // Exercise the actual streaming resampler as well as HTTP, rather than
        // bypassing it with pre-built request chunks. No WAV is written.
        let rate = 44100;
        let mut samples = crate::audio::resample(&source, 16000, rate);
        let info = info();
        let mut stream = begin(
            Some(&root),
            None,
            info.clone(),
            Arc::new(AtomicU64::new(1)),
            1,
        )
        .expect("ready service descriptor required");
        let started = Instant::now();
        let mut first_partial = None;
        let mut partial_revisions = 0;
        let mut last_revision = 0;
        for count in (882..samples.len())
            .step_by(882)
            .chain(std::iter::once(samples.len()))
        {
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                started + Duration::from_secs_f64(count as f64 / rate as f64),
            ))
            .await;
            stream.pump(&samples[..count], rate, false);
            let status = info.lock().unwrap().clone();
            assert!(
                status["asr_fallback_reason"].is_null(),
                "synthetic stream must not silently fall back"
            );
            if let Some(revision) = status["asr_partial"]["revision"].as_u64() {
                if revision > last_revision {
                    first_partial.get_or_insert(started.elapsed().as_secs_f64() * 1000.0);
                    partial_revisions += 1;
                    last_revision = revision;
                }
            }
        }
        let final_started = Instant::now();
        let result = stream.finish(&samples, rate).wait().await.unwrap();
        samples.fill(0.0);
        assert!(!result.text.is_empty());
        results.push(json!({"id":id,"source":"fixed_synthetic_sapi_adult_voice","input_rate":rate,"wire_rate":16000,"first_partial_ms":first_partial,"partial_revisions":partial_revisions,"final_request_ms":final_started.elapsed().as_secs_f64()*1000.0,"transcript":result.text}));
    }
    println!(
        "ASR_RUST_PIPELINE {}",
        json!({"microphone":false,"speaker_output":false,"ui_latency_measured":false,"results":results})
    );
}

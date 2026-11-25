use super::*;
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Response},
    routing::{get, post},
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize},
};
use tokio::{sync::Notify, task::JoinHandle};

const INSTANCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN: &str = "synthetic-final-asr-private-credential";
const REQUEST: &str = "cfe6d19e-1c83-4e6d-8b27-c947cf310aaa";
const OTHER_REQUEST: &str = "cf90ecaf-47e2-41bf-96dc-b98ebda35ae0";

fn health() -> Value {
    json!({"protocol_version":1,"instance_id":INSTANCE,"phase":"warm_ready",
        "model_ready":true,"ready":true,"warmup_completed":true,"busy":false,"closing":false,
        "provider":PROVIDER,"inference_mode":"offline_final"})
}
fn final_reply(id: &str, generation: u64) -> Value {
    json!({"protocol_version":1,"instance_id":INSTANCE,"request_id":id,
        "recording_generation":generation,"final":true,"provider":PROVIDER,
        "text":"No.","detected_language":"English","model_id":MODEL_ID,
        "model_revision":MODEL_REVISION,"sdk_commit":SDK_COMMIT,"backend":"transformers",
        "precision":"bfloat16","attention":"sdpa","max_new_tokens":512,
        "generation_calls":[{"new_tokens":6,"hit_token_limit":false,"ended_with_eos":true,
            "raw_decoder_text":"language English<asr_text>No.<|im_end|>"}],"possible_truncation":false})
}
fn service_error(code: &str) -> Value {
    json!({"protocol_version":1,"instance_id":INSTANCE,"error":code})
}
fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
#[derive(Clone)]
enum Reply {
    Healthy,
    Json(StatusCode, Value),
    Raw(Vec<u8>),
    ChunkedLarge,
    Redirect(String),
}
struct SeenRequest {
    headers: HeaderMap,
    body: Vec<u8>,
}
struct Fake {
    health: Mutex<Value>,
    reply: Mutex<Reply>,
    cancel_reply: Mutex<Option<Value>>,
    health_calls: AtomicUsize,
    final_calls: AtomicUsize,
    redirected_calls: AtomicUsize,
    finals: Mutex<Vec<SeenRequest>>,
    cancels: Mutex<Vec<SeenRequest>>,
    blocked: AtomicBool,
    cancel_delay_ms: AtomicU64,
    started: Notify,
    release: Notify,
}
async fn get_health(State(fake): State<Arc<Fake>>) -> Response<Body> {
    fake.health_calls.fetch_add(1, Ordering::SeqCst);
    let value = fake.health.lock().unwrap().clone();
    json_response(StatusCode::OK, value)
}
async fn transcribe(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    fake.final_calls.fetch_add(1, Ordering::SeqCst);
    let id = headers["x-asr-request-id"].to_str().unwrap().to_owned();
    let generation: u64 = headers["x-recording-generation"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    fake.finals.lock().unwrap().push(SeenRequest {
        headers,
        body: body.to_vec(),
    });
    let reply = fake.reply.lock().unwrap().clone();
    fake.started.notify_one();
    if fake.blocked.load(Ordering::SeqCst) {
        fake.release.notified().await;
    }
    match reply {
        Reply::Healthy => json_response(StatusCode::OK, final_reply(&id, generation)),
        Reply::Json(status, value) => json_response(status, value),
        Reply::Raw(bytes) => Response::builder().body(Body::from(bytes)).unwrap(),
        Reply::ChunkedLarge => {
            let stream = futures_util::stream::iter(vec![
                Ok::<_, std::io::Error>(Bytes::from(vec![b' '; MAX_REPLY_BYTES])),
                Ok(Bytes::from_static(b"x")),
            ]);
            Response::builder().body(Body::from_stream(stream)).unwrap()
        }
        Reply::Redirect(location) => Response::builder()
            .status(StatusCode::FOUND)
            .header("location", location)
            .body(Body::from(service_error("redirecting").to_string()))
            .unwrap(),
    }
}
async fn cancel(State(fake): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response<Body> {
    let id = headers["x-asr-request-id"].to_str().unwrap().to_owned();
    let generation: u64 = headers["x-recording-generation"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    fake.cancels.lock().unwrap().push(SeenRequest {
        headers,
        body: body.to_vec(),
    });
    let value = fake
        .cancel_reply
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| {
            json!({
        "protocol_version":1,"instance_id":INSTANCE,"request_id":id,
        "recording_generation":generation,"canceled":false,"worker_retired":null,"gpu_busy":false})
        });
    tokio::time::sleep(Duration::from_millis(
        fake.cancel_delay_ms.load(Ordering::SeqCst),
    ))
    .await;
    json_response(StatusCode::OK, value)
}
async fn redirected(State(fake): State<Arc<Fake>>) -> Response<Body> {
    fake.redirected_calls.fetch_add(1, Ordering::SeqCst);
    json_response(StatusCode::OK, final_reply(REQUEST, 1))
}
struct Fixture {
    url: String,
    fake: Arc<Fake>,
    task: JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let fake = Arc::new(Fake {
            health: Mutex::new(health()),
            reply: Mutex::new(Reply::Healthy),
            cancel_reply: Mutex::new(None),
            health_calls: AtomicUsize::new(0),
            final_calls: AtomicUsize::new(0),
            redirected_calls: AtomicUsize::new(0),
            finals: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            blocked: AtomicBool::new(false),
            cancel_delay_ms: AtomicU64::new(0),
            started: Notify::new(),
            release: Notify::new(),
        });
        let router = Router::new()
            .route("/health", get(get_health))
            .route("/v1/asr/transcribe", post(transcribe))
            .route("/v1/asr/cancel", post(cancel))
            .route("/redirected", get(redirected))
            .with_state(fake.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Ok(Self { url, fake, task })
    }
    fn client(&self) -> FinalAsrClient {
        FinalAsrClient::new(INSTANCE.into(), self.url.clone(), TOKEN.into()).unwrap()
    }
    async fn warm_client(&self) -> Result<Arc<FinalAsrClient>> {
        let client = Arc::new(self.client());
        client.check_warm().await?;
        Ok(client)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[test]
fn constructor_rejects_foreign_endpoints_credentials_and_noncanonical_instances() {
    for url in [
        "http://localhost:12345",
        "https://127.0.0.1:12345",
        "http://127.0.0.1:12345/path",
        "http://127.0.0.1:12345/?key=private",
        "http://user@127.0.0.1:12345",
        "http://[::1]:12345",
        "http://127.0.0.1:0",
        "http://127.0.0.1",
    ] {
        assert!(FinalAsrClient::new(INSTANCE.into(), url.into(), TOKEN.into()).is_err());
    }
    assert!(
        FinalAsrClient::new(
            INSTANCE.to_uppercase(),
            "http://127.0.0.1:12345".into(),
            TOKEN.into()
        )
        .is_err()
    );
    assert!(
        FinalAsrClient::new(
            INSTANCE.into(),
            "http://127.0.0.1:12345".into(),
            "bad\r\nheader".into()
        )
        .is_err()
    );
}

#[tokio::test]
async fn requires_warm_health_caches_success_and_sends_original_pcm_with_strict_generations()
-> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.client();
    assert!(client.transcribe(REQUEST, 0, vec![0, 1]).await.is_err());
    assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), 0);
    fixture.fake.health.lock().unwrap()["warmup_completed"] = json!(false);
    assert!(client.check_warm().await.is_err());
    assert!(!client.unavailable());
    assert!(client.transcribe(REQUEST, 0, vec![0, 1]).await.is_err());
    *fixture.fake.health.lock().unwrap() = health();
    client.check_warm().await?;
    client.check_warm().await?;
    assert_eq!(fixture.fake.health_calls.load(Ordering::SeqCst), 2);
    let pcm = vec![0, 128, 0, 0, 255, 127];
    let transcript = client.transcribe(REQUEST, 0, pcm.clone()).await?;
    assert_eq!(transcript.text, "No.");
    assert_eq!(transcript.detected_language, "English");
    assert!(transcript.elapsed_ms < 5000);
    assert!(
        client
            .transcribe(OTHER_REQUEST, 0, vec![0, 0])
            .await
            .is_err()
    );
    client.transcribe(OTHER_REQUEST, 2, vec![0, 0]).await?;
    assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), 2);
    let seen = fixture.fake.finals.lock().unwrap();
    assert_eq!(seen[0].body, pcm);
    for (name, expected) in [
        ("authorization", format!("Bearer {TOKEN}")),
        ("x-asr-instance", INSTANCE.into()),
        ("x-asr-request-id", REQUEST.into()),
        ("x-recording-generation", "0".into()),
        ("x-audio-sample-count", "3".into()),
        ("x-audio-sample-rate", "16000".into()),
        ("x-audio-channels", "1".into()),
        ("x-audio-encoding", "pcm_s16le".into()),
        ("content-type", "application/octet-stream".into()),
        ("content-length", "6".into()),
    ] {
        assert_eq!(seen[0].headers[name].to_str()?, expected);
    }
    Ok(())
}

#[tokio::test]
async fn pcm_limit_and_uuid_validation_happen_before_network_without_consuming_generation()
-> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    for pcm in [Vec::new(), vec![0], vec![0; MAX_PCM_BYTES + 2]] {
        assert!(client.transcribe(REQUEST, 1, pcm).await.is_err());
    }
    assert!(
        client
            .transcribe("not-a-uuid", 1, vec![0, 0])
            .await
            .is_err()
    );
    assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), 0);
    client
        .transcribe(REQUEST, 1, vec![0; MAX_PCM_BYTES])
        .await?;
    assert_eq!(
        fixture.fake.finals.lock().unwrap()[0].body.len(),
        MAX_PCM_BYTES
    );
    assert!(!client.unavailable());
    Ok(())
}

#[tokio::test]
async fn empty_final_and_eos_at_exact_token_ceiling_remain_valid() -> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    let mut value = final_reply(REQUEST, 1);
    value["text"] = json!("");
    value["detected_language"] = json!("");
    value["generation_calls"][0]["new_tokens"] = json!(512);
    value["generation_calls"][0]["hit_token_limit"] = json!(true);
    *fixture.fake.reply.lock().unwrap() = Reply::Json(StatusCode::OK, value);
    assert_eq!(client.transcribe(REQUEST, 1, vec![0, 0]).await?.text, "");
    assert!(!client.unavailable());
    Ok(())
}

#[tokio::test]
async fn rejects_identity_and_frozen_model_contract_changes_and_latches_unavailable() -> Result<()>
{
    let fixture = Fixture::new().await?;
    let mutations = [
        ("instance_id", json!("b".repeat(32))),
        ("request_id", json!(OTHER_REQUEST)),
        ("recording_generation", json!(2)),
        ("protocol_version", json!(1.0)),
        ("final", json!(false)),
        ("model_id", json!("different")),
        ("model_revision", json!("different")),
        ("sdk_commit", json!("different")),
        ("backend", json!("vllm")),
        ("precision", json!("float16")),
        ("attention", json!("other")),
        ("max_new_tokens", json!(511)),
    ];
    for (field, change) in mutations {
        let client = fixture.warm_client().await?;
        let mut value = final_reply(REQUEST, 1);
        value[field] = change;
        *fixture.fake.reply.lock().unwrap() = Reply::Json(StatusCode::OK, value);
        assert!(
            client.transcribe(REQUEST, 1, vec![0, 0]).await.is_err(),
            "{field}"
        );
        assert!(client.unavailable(), "{field}");
        let calls = fixture.fake.final_calls.load(Ordering::SeqCst);
        assert!(
            client
                .transcribe(OTHER_REQUEST, 2, vec![0, 0])
                .await
                .is_err()
        );
        assert!(client.check_warm().await.is_err());
        assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), calls);
    }
    Ok(())
}

#[tokio::test]
async fn refuses_truncation_missing_or_contradictory_generation_metadata() -> Result<()> {
    let fixture = Fixture::new().await?;
    let base = final_reply(REQUEST, 1);
    let mut cases = Vec::new();
    let mut value = base.clone();
    value["possible_truncation"] = json!(true);
    cases.push(value);
    let mut value = base.clone();
    value["generation_calls"] = json!([]);
    cases.push(value);
    let mut value = base.clone();
    value["generation_calls"][0]["ended_with_eos"] = json!(false);
    cases.push(value);
    let mut value = base.clone();
    value["generation_calls"][0]["hit_token_limit"] = json!(true);
    cases.push(value);
    let mut value = base.clone();
    value["generation_calls"][0]["new_tokens"] = json!(0);
    cases.push(value);
    let mut value = base.clone();
    value.as_object_mut().unwrap().remove("possible_truncation");
    cases.push(value);
    let mut value = base;
    value["text"] = json!("字".repeat(8001));
    cases.push(value);
    for value in cases {
        let client = fixture.warm_client().await?;
        *fixture.fake.reply.lock().unwrap() = Reply::Json(StatusCode::OK, value);
        assert!(client.transcribe(REQUEST, 1, vec![0, 0]).await.is_err());
        assert!(client.unavailable());
    }
    Ok(())
}

#[tokio::test]
async fn bounds_declared_and_streamed_bodies_and_never_returns_raw_error_details() -> Result<()> {
    let fixture = Fixture::new().await?;
    for reply in [
        Reply::Raw(vec![b' '; MAX_REPLY_BYTES + 1]),
        Reply::ChunkedLarge,
        Reply::Raw(format!("invalid private response {TOKEN}").into_bytes()),
        Reply::Json(StatusCode::INTERNAL_SERVER_ERROR, service_error(TOKEN)),
    ] {
        let client = fixture.warm_client().await?;
        *fixture.fake.reply.lock().unwrap() = reply;
        let error = client
            .transcribe(REQUEST, 1, vec![0, 0])
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains(TOKEN));
        assert!(!error.contains("private response"));
        assert!(client.unavailable());
    }
    // Typed deserialization also rejects duplicate identity fields.
    let client = fixture.warm_client().await?;
    let raw = final_reply(REQUEST, 1)
        .to_string()
        .replacen('{', "{\"instance_id\":\"other\",", 1);
    *fixture.fake.reply.lock().unwrap() = Reply::Raw(raw.into_bytes());
    assert!(client.transcribe(REQUEST, 1, vec![0, 0]).await.is_err());
    assert!(client.unavailable());
    Ok(())
}

#[tokio::test]
async fn does_not_follow_redirects() -> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    *fixture.fake.reply.lock().unwrap() = Reply::Redirect(format!("{}/redirected", fixture.url));
    assert!(client.transcribe(REQUEST, 1, vec![0, 0]).await.is_err());
    assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.fake.redirected_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn expected_service_errors_do_not_poison_or_retry_and_generations_are_not_replayed()
-> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    for (generation, status, code) in [
        (0, 409, "gpu_worker_busy"),
        (1, 409, "request_canceled"),
        (2, 502, "decoder_truncated"),
        (3, 504, "final_deadline_exceeded"),
        (4, 503, "model_not_warm_or_closing"),
    ] {
        *fixture.fake.reply.lock().unwrap() =
            Reply::Json(StatusCode::from_u16(status)?, service_error(code));
        let before = fixture.fake.final_calls.load(Ordering::SeqCst);
        assert!(
            client
                .transcribe(REQUEST, generation, vec![0, 0])
                .await
                .is_err()
        );
        assert!(!client.unavailable());
        assert!(
            client
                .transcribe(REQUEST, generation, vec![0, 0])
                .await
                .is_err()
        );
        assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), before + 1);
    }
    *fixture.fake.reply.lock().unwrap() = Reply::Healthy;
    client.transcribe(OTHER_REQUEST, 5, vec![0, 0]).await?;
    Ok(())
}

#[tokio::test]
async fn cancel_invalidates_late_final_without_poisoning_and_uses_empty_exact_identity_request()
-> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    fixture.fake.blocked.store(true, Ordering::SeqCst);
    let worker = {
        let client = client.clone();
        tokio::spawn(async move { client.transcribe(REQUEST, 1, vec![0, 0]).await })
    };
    fixture.fake.started.notified().await;
    assert!(
        client
            .transcribe(OTHER_REQUEST, 2, vec![0, 0])
            .await
            .is_err()
    );
    *fixture.fake.cancel_reply.lock().unwrap() = Some(
        json!({"protocol_version":1,"instance_id":INSTANCE,
        "request_id":REQUEST,"recording_generation":1,"canceled":true,"worker_retired":false,"gpu_busy":true}),
    );
    let receipt = client.cancel(REQUEST, 1).await?;
    assert_eq!(receipt["worker_retired"], false);
    fixture.fake.blocked.store(false, Ordering::SeqCst);
    fixture.fake.release.notify_one();
    assert!(worker.await?.is_err());
    assert!(!client.unavailable());
    client.transcribe(OTHER_REQUEST, 2, vec![0, 0]).await?;
    let requests = fixture.fake.cancels.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].body.is_empty());
    assert_eq!(requests[0].headers["content-length"], "0");
    assert_eq!(requests[0].headers["x-asr-request-id"], REQUEST);
    assert_eq!(requests[0].headers["x-recording-generation"], "1");
    Ok(())
}

#[tokio::test]
async fn dropped_future_releases_only_local_slot_and_busy_next_request_can_fall_back() -> Result<()>
{
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    fixture.fake.blocked.store(true, Ordering::SeqCst);
    let worker = {
        let client = client.clone();
        tokio::spawn(async move { client.transcribe(REQUEST, 1, vec![0, 0]).await })
    };
    fixture.fake.started.notified().await;
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert!(!client.unavailable());
    fixture.fake.blocked.store(false, Ordering::SeqCst);
    *fixture.fake.reply.lock().unwrap() =
        Reply::Json(StatusCode::CONFLICT, service_error("gpu_worker_busy"));
    assert!(
        client
            .transcribe(OTHER_REQUEST, 2, vec![0, 0])
            .await
            .is_err()
    );
    assert!(!client.unavailable());
    fixture.fake.release.notify_one();
    *fixture.fake.reply.lock().unwrap() = Reply::Healthy;
    client.transcribe(OTHER_REQUEST, 3, vec![0, 0]).await?;
    assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn stale_cancel_does_not_invalidate_new_active_generation() -> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    fixture.fake.blocked.store(true, Ordering::SeqCst);
    let worker = {
        let client = client.clone();
        tokio::spawn(async move { client.transcribe(OTHER_REQUEST, 2, vec![0, 0]).await })
    };
    fixture.fake.started.notified().await;
    assert_eq!(client.cancel(REQUEST, 1).await?["canceled"], false);
    fixture.fake.blocked.store(false, Ordering::SeqCst);
    fixture.fake.release.notify_one();
    assert_eq!(worker.await??.text, "No.");
    assert!(!client.unavailable());
    Ok(())
}

#[tokio::test]
async fn final_deadline_is_five_seconds_without_retry_or_permanent_failure() -> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    fixture.fake.blocked.store(true, Ordering::SeqCst);
    let began = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(8),
        client.transcribe(REQUEST, 1, vec![0, 0]),
    )
    .await?;
    assert_eq!(outcome.unwrap_err().to_string(), "final_asr_timeout");
    assert!(began.elapsed() >= Duration::from_secs(4));
    assert_eq!(fixture.fake.final_calls.load(Ordering::SeqCst), 1);
    assert!(!client.unavailable());
    assert_eq!(client.cancel(REQUEST, 1).await?["canceled"], false);
    fixture.fake.release.notify_one();
    Ok(())
}

#[tokio::test]
async fn cancel_has_independent_300ms_budget_and_remains_available_after_permanent_error()
-> Result<()> {
    let fixture = Fixture::new().await?;
    let client = fixture.warm_client().await?;
    fixture.fake.cancel_delay_ms.store(1000, Ordering::SeqCst);
    let result =
        tokio::time::timeout(Duration::from_millis(900), client.cancel(REQUEST, 1)).await?;
    assert_eq!(result.unwrap_err().to_string(), "final_asr_timeout");
    assert!(!client.unavailable());
    fixture.fake.cancel_delay_ms.store(0, Ordering::SeqCst);
    client.mark_unavailable();
    assert_eq!(client.cancel(REQUEST, 1).await?["canceled"], false);
    assert!(client.unavailable());
    Ok(())
}

#[tokio::test]
async fn cancel_rejects_wrong_identity_or_missing_retirement_receipt() -> Result<()> {
    let fixture = Fixture::new().await?;
    for receipt in [
        json!({"protocol_version":1,"instance_id":INSTANCE,"request_id":OTHER_REQUEST,
            "recording_generation":1,"canceled":false,"worker_retired":null,"gpu_busy":false}),
        json!({"protocol_version":1,"instance_id":INSTANCE,"request_id":REQUEST,
            "recording_generation":1,"canceled":true,"worker_retired":null,"gpu_busy":true}),
    ] {
        let client = fixture.warm_client().await?;
        *fixture.fake.cancel_reply.lock().unwrap() = Some(receipt);
        assert!(client.cancel(REQUEST, 1).await.is_err());
        assert!(client.unavailable());
    }
    Ok(())
}

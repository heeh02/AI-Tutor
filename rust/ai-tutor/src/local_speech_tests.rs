use super::*;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Response},
    routing::{get, post},
};
use std::sync::atomic::AtomicUsize;

#[test]
fn shared_gpu_profile_requires_managed_qwen_and_real_bilingual_warmup() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("runtime"))?;
    std::fs::write(
        dir.path().join("runtime/gpu-resource-policy.json"),
        r#"{"protocol_version":1,"mode":"qwen_pair_12gb_v1"}"#,
    )?;
    let path = dir.path().join("runtime/local-speech-policy.json");
    for value in [
        json!({"protocol_version":1,"mode":"external","warmup":"synthetic_bilingual"}),
        json!({"protocol_version":1,"mode":"managed","warmup":"disabled",
            "managed":{"engine":"qwen","distribution":"Ubuntu"}}),
        json!({"protocol_version":1,"mode":"managed","warmup":"synthetic_bilingual",
            "managed":{"engine":"cosy","distribution":"Ubuntu","references_file":"references.json"}}),
    ] {
        std::fs::write(&path, serde_json::to_vec(&value)?)?;
        assert!(LocalSpeechPolicy::load(dir.path()).is_err());
    }
    std::fs::write(
        path,
        serde_json::to_vec(&json!({"protocol_version":1,"mode":"managed",
        "warmup":"synthetic_bilingual","managed":{"engine":"qwen","distribution":"Ubuntu"}}))?,
    )?;
    assert_eq!(
        LocalSpeechPolicy::load(dir.path())?.mode,
        LocalSpeechMode::Managed
    );
    Ok(())
}

struct FakeService {
    posts: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    delay: Duration,
    wrong_model: bool,
    malformed_second: bool,
}
async fn models(State(state): State<Arc<FakeService>>) -> Json<Value> {
    Json(json!({"data":[{"id":if state.wrong_model {"other-model"} else {"tutor-qwen-tts"}}]}))
}
async fn speech(
    State(state): State<Arc<FakeService>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response<Body> {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer synthetic-warm-credential"
    );
    let sequence = state.posts.fetch_add(1, Ordering::SeqCst);
    state.requests.lock().unwrap().push(body);
    tokio::time::sleep(state.delay).await;
    let bytes = if state.malformed_second && sequence == 1 {
        vec![1]
    } else {
        vec![0; 2400]
    };
    Response::builder()
        .header("content-type", "audio/pcm")
        .body(Body::from(bytes))
        .unwrap()
}
struct Fixture {
    _dir: tempfile::TempDir,
    local: LocalSpeech,
    service: Arc<FakeService>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(
        enabled: bool,
        delay: Duration,
        wrong_model: bool,
        malformed_second: bool,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let service = Arc::new(FakeService {
            posts: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
            delay,
            wrong_model,
            malformed_second,
        });
        let router = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/v1/models", get(models))
            .route("/v1/audio/speech", post(speech))
            .with_state(service.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join("runtime"))?;
        std::fs::write(dir.path().join("runtime/tts-service.json"),json!({"protocol_version":1,
            "endpoint":format!("http://127.0.0.1:{port}/v1/audio/speech"),"bearer_token":"synthetic-warm-credential","model":"tutor-qwen-tts"}).to_string())?;
        if enabled {
            std::fs::write(
                dir.path().join("runtime/local-speech-policy.json"),
                json!({"protocol_version":1,"mode":"external","warmup":"synthetic_bilingual"})
                    .to_string(),
            )?;
        }
        let local = LocalSpeech::new(dir.path())?;
        Ok(Self {
            _dir: dir,
            local,
            service,
            task,
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn absent_policy_never_contacts_an_available_external_voice() -> Result<()> {
    let f = Fixture::new(false, Duration::ZERO, false, false).await?;
    assert!(f.local.prepare_external_tts().await.is_err());
    assert_eq!(f.local.status()["phase"], "disabled");
    assert_eq!(f.service.posts.load(Ordering::SeqCst), 0);
    Ok(())
}
#[test]
fn local_policy_requires_explicit_bounded_supported_warmup() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("runtime"))?;
    let path = dir.path().join("runtime/local-speech-policy.json");
    for bad in [
        json!({"protocol_version":2,"mode":"external"}).to_string(),
        json!({"protocol_version":1,"mode":"disabled","warmup":"synthetic_bilingual"}).to_string(),
        json!({"protocol_version":1,"mode":"external","warmup":"learner_text"}).to_string(),
        json!({"protocol_version":1,"mode":"external","command":"arbitrary"}).to_string(),
        "x".repeat(4097),
    ] {
        std::fs::write(&path, bad)?;
        assert!(LocalSpeechPolicy::load(dir.path()).is_err());
    }
    Ok(())
}
#[tokio::test]
async fn ready_model_gets_exactly_two_synthetic_probes_and_waits_for_explicit_installation()
-> Result<()> {
    let f = Fixture::new(true, Duration::ZERO, false, false).await?;
    let prepared = f.local.prepare_external_tts().await?;
    assert_eq!(f.service.posts.load(Ordering::SeqCst), 2);
    let requests = f.service.requests.lock().unwrap();
    assert_eq!(requests[0]["voice"], "vivian");
    assert_eq!(requests[1]["voice"], "ryan");
    assert_eq!(requests[0]["input"], "这是一次本地语音准备测试。");
    assert_eq!(
        requests[1]["input"],
        "This is a local voice preparation test."
    );
    assert_eq!(f.local.status()["phase"], "warm_ready_pending_install");
    assert_eq!(prepared.receipt()["languages"][0]["samples"], 1200);
    assert_eq!(prepared.receipt()["languages"][1]["samples"], 1200);
    assert_eq!(prepared.receipt()["speaker_output_measured"], false);
    assert_eq!(prepared.receipt()["external_instance_verified"], false);
    assert_eq!(prepared.receipt()["total_deadline_ms"], 120000);
    assert_eq!(prepared.client().configuration_sha256().len(), 64);
    Ok(())
}
#[tokio::test]
async fn readiness_and_a_partial_warmup_cannot_claim_bilingual_warm_success() -> Result<()> {
    let wrong = Fixture::new(true, Duration::ZERO, true, false).await?;
    assert!(wrong.local.prepare_external_tts().await.is_err());
    assert_eq!(wrong.service.posts.load(Ordering::SeqCst), 0);
    let invalid = Fixture::new(true, Duration::ZERO, false, true).await?;
    assert!(invalid.local.prepare_external_tts().await.is_err());
    assert_eq!(invalid.local.status()["ready"], true);
    assert_eq!(invalid.local.status()["warmup_completed"], false);
    assert_eq!(invalid.local.status()["phase"], "degraded");
    Ok(())
}
#[tokio::test]
async fn both_languages_share_one_total_deadline() -> Result<()> {
    let f = Fixture::new(true, Duration::from_millis(70), false, false).await?;
    assert!(
        f.local
            .prepare_with_deadline(Duration::from_millis(110))
            .await
            .is_err()
    );
    assert_eq!(f.service.posts.load(Ordering::SeqCst), 2);
    assert_eq!(f.local.status()["phase"], "degraded");
    assert_eq!(f.local.status()["warmup_completed"], false);
    Ok(())
}
#[tokio::test]
async fn cancellation_interrupts_cold_headers_and_blocks_overlapping_preparation() -> Result<()> {
    let f = Fixture::new(true, Duration::from_secs(2), false, false).await?;
    let preparation = f.local.prepare_external_tts();
    tokio::pin!(preparation);
    tokio::select! {
        result=&mut preparation=>panic!("cold fixture finished unexpectedly: {}",result.is_ok()),
        _=async {while f.service.posts.load(Ordering::SeqCst)==0 {tokio::task::yield_now().await;}}=>{}
    }
    assert!(f.local.prepare_external_tts().await.is_err());
    f.local.cancel_preparation();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), preparation)
            .await?
            .is_err()
    );
    assert_eq!(f.local.status()["phase"], "cancelled");
    assert_eq!(f.service.posts.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn cancellation_returning_before_owner_commit_rejects_the_queued_prepared_client()
-> Result<()> {
    let f = Fixture::new(true, Duration::ZERO, false, false).await?;
    let prepared = f.local.prepare_external_tts().await?;
    let original = Arc::new(TtsClient::from_descriptor(
        &f._dir.path().join("runtime/tts-service.json"),
    )?);
    let mut slot = Some(original.clone());
    let (audio, owner) = AudioService::configuration_test_channel(7);
    let installation = f.local.install_prepared(&audio, &prepared, 7);
    tokio::pin!(installation);
    assert!(futures_util::poll!(installation.as_mut()).is_pending());
    let pending = owner.take()?;
    f.local.cancel_preparation();
    // Owner is deliberately paused until cancellation has returned; the live
    // receipt has not been dropped and no 10ms cancellation poll is involved.
    let owner_receipt = pending.commit(&mut slot, 7);
    assert!(!owner_receipt.applied);
    assert_eq!(owner_receipt.reason, "preparation_cancelled");
    assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &original));
    assert_eq!(installation.await?, owner_receipt);
    assert_eq!(f.local.status()["phase"], "cancelled");
    Ok(())
}

#[tokio::test]
async fn owner_commit_before_cancellation_is_kept_even_before_the_installer_polls_its_receipt()
-> Result<()> {
    let f = Fixture::new(true, Duration::ZERO, false, false).await?;
    let prepared = f.local.prepare_external_tts().await?;
    let (audio, owner) = AudioService::configuration_test_channel(7);
    let installation = f.local.install_prepared(&audio, &prepared, 7);
    tokio::pin!(installation);
    assert!(futures_util::poll!(installation.as_mut()).is_pending());
    let mut slot = None;
    let owner_receipt = owner.take()?.commit(&mut slot, 7);
    assert!(owner_receipt.applied);
    f.local.cancel_preparation();
    assert_eq!(
        f.local.status()["phase"],
        "active",
        "an already committed install is not revoked"
    );
    assert_eq!(
        installation.await?,
        owner_receipt,
        "the committed receipt takes precedence"
    );
    assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &prepared.client()));
    assert_eq!(f.local.status()["phase"], "active");
    assert_eq!(f.local.status()["cancellation_after_install"], true);
    Ok(())
}

#[test]
fn managed_policy_requires_explicit_distribution_engine_and_cosy_references() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("runtime"))?;
    let path = dir.path().join("runtime/local-speech-policy.json");
    let mut policy = json!({"protocol_version":1,"mode":"managed","warmup":"synthetic_bilingual",
        "managed":{"engine":"cosy","distribution":"Ubuntu","references_file":"runtime/tts-references.json"}});
    std::fs::write(&path, serde_json::to_vec(&policy)?)?;
    assert_eq!(
        LocalSpeech::new(dir.path())?.policy().mode,
        LocalSpeechMode::Managed
    );
    policy["managed"]["references_file"] = Value::Null;
    std::fs::write(&path, serde_json::to_vec(&policy)?)?;
    assert!(LocalSpeech::new(dir.path()).is_err());
    policy["managed"]["engine"] = json!("qwen");
    std::fs::write(&path, serde_json::to_vec(&policy)?)?;
    assert!(LocalSpeech::new(dir.path()).is_ok());
    policy["managed"]["distribution"] = json!("Ubuntu; unsafe");
    std::fs::write(&path, serde_json::to_vec(&policy)?)?;
    assert!(LocalSpeech::new(dir.path()).is_err());
    Ok(())
}

#[test]
fn descriptor_without_opt_in_does_not_enable_local_speech() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("runtime"))?;
    std::fs::write(
        dir.path().join("runtime/tts-service.json"),
        b"invalid and never opened",
    )?;
    let local = LocalSpeech::new(dir.path())?;
    assert_eq!(local.policy().mode, LocalSpeechMode::Disabled);
    assert_eq!(local.status()["phase"], "disabled");
    Ok(())
}

//! Worker tests use only fake providers and temporary, synthetic databases.
use ai_tutor::{
    kimi::memory_contract::{self, PROMPT_VERSION, SCHEMA_VERSION},
    memory_worker::{
        CancelFuture, MemoryConsolidator, MemoryWorker, MemoryWorkerMode, MemoryWorkerPolicy,
        ProviderFuture,
    },
    store::{Store, s},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, watch};

struct Fixture {
    _dir: tempfile::TempDir,
    stores: Mutex<HashMap<String, Store>>,
    lid: String,
    job: String,
}
impl Fixture {
    fn new(synthetic: bool) -> Result<Self> {
        Self::with_prompt_version(synthetic, PROMPT_VERSION)
    }
    fn with_prompt_version(synthetic: bool, prompt_version: &str) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("synthetic.sqlite3"), "simulator")?;
        let learner = store.create_learner("synthetic fixture", "en", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        let sid = s(&session, "session_id");
        let event = store.record_education_event(
            &lid,
            sid,
            "simulated",
            "observation",
            &json!({"text":"The fixture light turned on.","synthetic_fixture":synthetic}),
        )?;
        let job = store.enqueue_memory_job(
            &lid,
            sid,
            &[s(&event, "event_id").to_owned()],
            prompt_version,
            &Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )?;
        let job = s(&job, "job_id").to_owned();
        Ok(Self {
            _dir: dir,
            stores: Mutex::new(HashMap::from([("simulator".into(), store)])),
            lid,
            job,
        })
    }
    fn status(&self) -> Result<Value> {
        self.stores.lock().unwrap()["simulator"].memory_job_status(&self.lid, &self.job)
    }
    fn policy() -> MemoryWorkerPolicy {
        MemoryWorkerPolicy {
            mode: MemoryWorkerMode::SyntheticOnly,
            ..Default::default()
        }
    }
}

struct FakeProvider {
    wait: bool,
    fail: bool,
    malformed: bool,
    report_started: bool,
    entered: Notify,
    release: watch::Sender<bool>,
    calls: AtomicUsize,
    cancels: AtomicUsize,
    active: AtomicUsize,
}
impl FakeProvider {
    fn new(wait: bool) -> Self {
        let (release, _) = watch::channel(false);
        Self {
            wait,
            fail: false,
            malformed: false,
            report_started: true,
            entered: Notify::new(),
            release,
            calls: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
        }
    }
    fn reply(request: &Value) -> Value {
        let event = &request["context"]["sources"][0];
        let eid = s(event, "event_id");
        let mut candidate = json!({"kind":"trial_observation","claim_type":"reported_observation","content":"The learner reported the fixture light turned on.","scope":"current_activity",
            "source_event_ids":[eid],"source_event_versions":{},"source_quotes":[{"event_id":eid,"quote":event["content"]["text"]}],"support_level":"unknown",
            "importance":{"goal_relevance":1,"continuity":1,"novel_change":1,"explicit_request":0}});
        candidate["source_event_versions"][eid] = event["event_version"].clone();
        json!({"schema_version":SCHEMA_VERSION,"candidates":[candidate]})
    }
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl MemoryConsolidator for FakeProvider {
    fn consolidate<'a>(
        &'a self,
        request: &'a Value,
        started: watch::Sender<bool>,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(&self.active);
            if self.report_started {
                let _ = started.send(true);
            }
            let mut release = self.release.subscribe();
            self.entered.notify_one();
            if self.wait && !*release.borrow() {
                release.changed().await?;
            }
            if self.fail {
                return Err(anyhow!("synthetic provider failure"));
            }
            let mut result = Self::reply(request);
            if self.malformed {
                result["candidates"][0]["source_event_versions"] = json!({"not-the-source":99});
            }
            Ok(result)
        })
    }
    fn cancel<'a>(&'a self, _request_id: &'a str) -> CancelFuture<'a> {
        Box::pin(async move {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            // Deliberately returns an otherwise valid late response, to test
            // that cancellation wins even over a cooperative finished result.
            self.release.send_replace(true);
            Ok(())
        })
    }
}

#[tokio::test]
async fn default_worker_never_claims_or_calls_and_synthetic_mode_rejects_unmarked_events()
-> Result<()> {
    let f = Fixture::new(false)?;
    let worker = MemoryWorker::new();
    let provider = FakeProvider::new(false);
    ensure!(
        worker
            .run_once(&f.stores, &provider, "simulator", &Default::default())
            .await?["status"]
            == "disabled"
    );
    ensure!(f.status()?["attempts"] == 0 && provider.calls.load(Ordering::SeqCst) == 0);
    ensure!(
        worker
            .run_once(&f.stores, &provider, "real", &Fixture::policy())
            .await
            .is_err()
    );
    ensure!(
        worker
            .run_once(&f.stores, &provider, "simulator", &Fixture::policy())
            .await?["status"]
            == "failed"
    );
    ensure!(provider.calls.load(Ordering::SeqCst) == 0);
    ensure!(f.status()?["attempt_receipts"][0]["started_at_ms"].is_null());
    Ok(())
}

#[tokio::test]
async fn fake_result_stages_shadow_and_never_applies_memory() -> Result<()> {
    let f = Fixture::new(true)?;
    let worker = MemoryWorker::new();
    let provider = FakeProvider::new(false);
    let result = worker
        .run_once(&f.stores, &provider, "simulator", &Fixture::policy())
        .await?;
    ensure!(
        result["status"] == "shadow_staged"
            && result["candidate_count"] == 1
            && result["applied"] == false
    );
    ensure!(f.status()?["candidates"][0]["status"] == "shadow");
    ensure!(
        f.stores.lock().unwrap()["simulator"]
            .memories(&f.lid)?
            .is_empty()
    );
    ensure!(f.status()?["attempt_receipts"][0]["started_at_ms"].is_number());
    ensure!(!result.to_string().contains("fixture light"));
    Ok(())
}

#[tokio::test]
async fn foreground_preempts_late_result_and_new_receipt_can_resume() -> Result<()> {
    let f = Fixture::new(true)?;
    let worker = MemoryWorker::new();
    let provider = FakeProvider::new(true);
    let policy = Fixture::policy();
    let (result, guard) = tokio::join!(
        worker.run_once(&f.stores, &provider, "simulator", &policy),
        async {
            provider.entered.notified().await;
            // An in-flight provider must never hold the Store lock.
            assert!(f.stores.try_lock().is_ok());
            worker.foreground()
        }
    );
    ensure!(result?["status"] == "preempted");
    ensure!(
        provider.cancels.load(Ordering::SeqCst) == 1 && provider.active.load(Ordering::SeqCst) == 0
    );
    ensure!(f.status()?["status"] == "pending" && f.status()?["candidates"] == json!([]));
    let attempts = f.status()?["attempts"].clone();
    ensure!(
        worker
            .run_once(&f.stores, &provider, "simulator", &policy)
            .await?["status"]
            == "foreground_or_worker_busy"
    );
    ensure!(f.status()?["attempts"] == attempts);
    drop(guard);
    ensure!(
        worker
            .run_once(&f.stores, &FakeProvider::new(false), "simulator", &policy)
            .await?["status"]
            == "shadow_staged"
    );
    ensure!(f.status()?["attempts"] == 2);
    Ok(())
}

#[tokio::test]
async fn deletion_during_provider_wait_rejects_late_result_and_clears_sources() -> Result<()> {
    let f = Fixture::new(true)?;
    let worker = MemoryWorker::new();
    let provider = FakeProvider::new(true);
    let policy = Fixture::policy();
    let (result, deleted) = tokio::join!(
        worker.run_once(&f.stores, &provider, "simulator", &policy),
        async {
            provider.entered.notified().await;
            f.stores.lock().unwrap()["simulator"].forget(&f.lid, None)?;
            provider.release.send_replace(true);
            Ok::<_, anyhow::Error>(())
        }
    );
    deleted?;
    ensure!(result?["status"] == "invalidated");
    let status = f.status()?;
    ensure!(status["source_snapshot"] == json!([]) && status["candidates"] == json!([]));
    ensure!(
        f.stores.lock().unwrap()["simulator"]
            .memories(&f.lid)?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn provider_failure_and_wrong_source_versions_never_commit_candidates() -> Result<()> {
    for malformed in [false, true] {
        let f = Fixture::new(true)?;
        let worker = MemoryWorker::new();
        let mut provider = FakeProvider::new(false);
        provider.fail = !malformed;
        provider.malformed = malformed;
        provider.report_started = malformed;
        ensure!(
            worker
                .run_once(&f.stores, &provider, "simulator", &Fixture::policy())
                .await?["status"]
                == "failed"
        );
        let status = f.status()?;
        ensure!(status["status"] == "pending" && status["candidates"] == json!([]));
        ensure!(status["attempt_receipts"][0]["started_at_ms"].is_number() == malformed);
    }
    Ok(())
}

#[test]
fn memory_contract_is_bounded_and_prompt_prohibits_plan_completion_and_ability_labels() -> Result<()>
{
    ensure!(
        memory_contract::validate_response(
            &json!({"schema_version":SCHEMA_VERSION,"candidates":[]})
        )?
        .is_empty()
    );
    ensure!(
        memory_contract::validate_response(&json!({"schema_version":"unknown","candidates":[]}))
            .is_err()
    );
    ensure!(
        memory_contract::validate_response(
            &json!({"schema_version":SCHEMA_VERSION,"candidates":[],"reply_text":"extra"})
        )
        .is_err()
    );
    ensure!(
        memory_contract::validate_response(
            &json!({"schema_version":SCHEMA_VERSION,"candidates":vec![json!({});6]})
        )
        .is_err()
    );
    ensure!(memory_contract::schema()["properties"]["candidates"]["maxItems"] == 5);
    Ok(())
}

#[test]
fn prompt_and_expanded_type_schema_have_distinct_versions() -> Result<()> {
    ensure!(PROMPT_VERSION != SCHEMA_VERSION);
    ensure!(PROMPT_VERSION == "educational-memory-prompt-v4");
    ensure!(SCHEMA_VERSION == "educational-memory-v2");
    ensure!(memory_contract::schema()["properties"]["schema_version"]["const"] == SCHEMA_VERSION);
    ensure!(
        memory_contract::validate_response(&json!({
            "schema_version": SCHEMA_VERSION, "candidates": []
        }))?
        .is_empty()
    );
    ensure!(
        memory_contract::validate_response(&json!({
            "schema_version": PROMPT_VERSION, "candidates": []
        }))
        .is_err()
    );
    ensure!(
        memory_contract::validate_response(&json!({
            "schema_version": "educational-memory-v1", "candidates": []
        }))
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn obsolete_prompt_job_never_calls_provider_or_creates_candidates() -> Result<()> {
    let f = Fixture::with_prompt_version(true, "educational-memory-v1")?;
    let worker = MemoryWorker::new();
    let provider = FakeProvider::new(false);
    let result = worker
        .run_once(&f.stores, &provider, "simulator", &Fixture::policy())
        .await?;
    ensure!(result["status"] == "failed" && result["provider_started"] == false);
    ensure!(provider.calls.load(Ordering::SeqCst) == 0);
    let status = f.status()?;
    ensure!(status["candidates"] == json!([]));
    ensure!(status["attempt_receipts"][0]["started_at_ms"].is_null());
    Ok(())
}

#[tokio::test]
async fn explicit_synthetic_probe_uses_only_fixture_data_and_returns_no_raw_content() -> Result<()>
{
    let provider = FakeProvider::new(false);
    for language in ["zh", "en"] {
        let result = ai_tutor::memory_worker::run_synthetic_probe(&provider, language).await?;
        ensure!(
            result["result"]["status"] == "shadow_staged"
                && result["real_learner_data"] == false
                && result["applied"] == false
        );
        ensure!(
            !result.to_string().contains("指示灯") && !result.to_string().contains("green light")
        );
    }
    ensure!(provider.calls.load(Ordering::SeqCst) == 2);
    ensure!(
        ai_tutor::memory_worker::run_synthetic_probe(&provider, "unknown")
            .await
            .is_err()
    );
    ensure!(provider.calls.load(Ordering::SeqCst) == 2);
    Ok(())
}

#[tokio::test]
async fn preemption_before_first_provider_poll_does_not_start_a_cancelled_call() -> Result<()> {
    struct PreemptBeforePoll<'a> {
        worker: &'a MemoryWorker,
        calls: AtomicUsize,
    }
    impl MemoryConsolidator for PreemptBeforePoll<'_> {
        fn consolidate<'a>(
            &'a self,
            _request: &'a Value,
            _started: watch::Sender<bool>,
        ) -> ProviderFuture<'a> {
            // A foreground event lands after the worker's last cancellation
            // check but before the provider future gets its first poll.
            self.worker.preempt();
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"schema_version":SCHEMA_VERSION,"candidates":[]}))
            })
        }
        fn cancel<'a>(&'a self, _request_id: &'a str) -> CancelFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }
    let f = Fixture::new(true)?;
    let worker = MemoryWorker::new();
    let provider = PreemptBeforePoll {
        worker: &worker,
        calls: AtomicUsize::new(0),
    };
    let result = worker
        .run_once(&f.stores, &provider, "simulator", &Fixture::policy())
        .await?;
    ensure!(result["status"] == "preempted" && provider.calls.load(Ordering::SeqCst) == 0);
    ensure!(f.status()?["attempt_receipts"][0]["started_at_ms"].is_null());
    ensure!(f.status()?["candidates"] == json!([]));
    Ok(())
}

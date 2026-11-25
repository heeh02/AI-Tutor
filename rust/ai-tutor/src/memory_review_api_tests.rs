//! Explicit review through Workstation APIs. Temporary simulator evidence and a
//! controlled provider only: no model, credentials, browser or device is opened.
use super::*;
use crate::{
    kimi::memory_contract,
    memory_worker::{CancelFuture, MemoryConsolidator, ProviderFuture},
    store::{MemoryJobLimits, s},
};
use anyhow::{Result, anyhow, ensure};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    sync::{Notify, watch},
    time::{Duration, timeout},
};
use tower::ServiceExt;

struct Fixture {
    _directory: tempfile::TempDir,
    app: Arc<Workstation>,
    lid: String,
    sid: String,
    source_ids: Vec<String>,
    other_lid: String,
    other_sid: String,
    other_job: Option<String>,
}

fn fixture(queue_other: bool) -> Result<Fixture> {
    let directory = tempfile::tempdir()?;
    let app = Workstation::new(directory.path().to_owned(), "synthetic-review-token".into())?;
    let (lid, sid, source_ids, other_lid, other_sid, other_job) = {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let other = store.create_learner("Synthetic other review", "en", "guided")?;
        let other_lid = s(&other, "learner_id").to_owned();
        let other_session = store.create_session(&other_lid)?;
        let other_sid = s(&other_session, "session_id").to_owned();
        let foreign = store.record_education_event(
            &other_lid,
            &other_sid,
            "simulated",
            "observation",
            &json!({"text":"Synthetic foreign learner report: the blue lamp flashed."}),
        )?;
        let other_job = if queue_other {
            let job = store.enqueue_memory_job(
                &other_lid,
                &other_sid,
                &[s(&foreign, "event_id").to_owned()],
                memory_contract::PROMPT_VERSION,
                &MemoryJobLimits::default(),
                chrono::Utc::now().timestamp_millis() - 1_000,
            )?;
            Some(s(&job, "job_id").to_owned())
        } else {
            None
        };
        let learner = store.create_learner("Synthetic target review", "zh", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        let sid = s(&session, "session_id").to_owned();
        let mut source_ids = vec![];
        for text in [
            "Synthetic target report: the green lamp lit after pressing the switch.",
            "Synthetic target report: the wheel stopped after releasing the switch.",
        ] {
            // Manual OptInSimulator admission must not require a special lab
            // synthetic_fixture marker. The namespace and source remain honest.
            let event = store.record_education_event(
                &lid,
                &sid,
                "simulated",
                "observation",
                &json!({"text":text}),
            )?;
            source_ids.push(s(&event, "event_id").to_owned());
        }
        (lid, sid, source_ids, other_lid, other_sid, other_job)
    };
    *app.current.lock().unwrap() = Some(("simulator".into(), sid.clone()));
    Ok(Fixture {
        _directory: directory,
        app,
        lid,
        sid,
        source_ids,
        other_lid,
        other_sid,
        other_job,
    })
}

fn intent(f: &Fixture) -> Result<Value> {
    let stores = f.app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let session = store.session(&f.sid)?;
    Ok(json!({
        "expected_generation":session["generation"],
        "expected_epoch":store.learner(&f.lid)?["epoch"],
        "expected_state_revision":session["state_revision"],
    }))
}

fn counts(f: &Fixture) -> Result<Value> {
    let stores = f.app.stores.lock().unwrap();
    let store = &stores["simulator"];
    Ok(json!({
        "jobs":store.one("SELECT COUNT(*) AS n FROM memory_jobs", [])?.unwrap()["n"],
        "attempts":store.one("SELECT COUNT(*) AS n FROM memory_job_attempts", [])?.unwrap()["n"],
        "candidates":store.one("SELECT COUNT(*) AS n FROM education_memory_candidates", [])?.unwrap()["n"],
        "memories":store.one("SELECT COUNT(*) AS n FROM memories", [])?.unwrap()["n"],
    }))
}

struct Fake {
    calls: AtomicUsize,
    active: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    cancellations: Mutex<Vec<String>>,
    entered: Notify,
    release: watch::Sender<bool>,
}

impl Fake {
    fn new(blocked: bool) -> Arc<Self> {
        let (release, _) = watch::channel(!blocked);
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
            cancellations: Mutex::new(vec![]),
            entered: Notify::new(),
            release,
        })
    }
}

struct ActiveCall<'a>(&'a AtomicUsize);
impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl MemoryConsolidator for Fake {
    fn consolidate<'a>(
        &'a self,
        request: &'a Value,
        started: watch::Sender<bool>,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = ActiveCall(&self.active);
            self.requests.lock().unwrap().push(request.clone());
            let mut release = self.release.subscribe();
            let _ = started.send(true);
            self.entered.notify_one();
            while !*release.borrow() {
                release.changed().await?;
            }
            let sources = request["context"]["sources"]
                .as_array()
                .ok_or_else(|| anyhow!("fake expected source array"))?;
            let candidates: Vec<_> = sources.iter().map(|source| {
                let id = s(source, "event_id");
                let text = s(&source["content"], "text");
                json!({"kind":"trial_observation","claim_type":"reported_observation",
                    "content":text,"scope":"current_activity","source_event_ids":[id],
                    "source_event_versions":{id:source["event_version"]},
                    "source_quotes":[{"event_id":id,"quote":text}],"support_level":"unknown",
                    "importance":{"goal_relevance":2,"continuity":2,"novel_change":1,"explicit_request":0}})
            }).collect();
            // Cancellation deliberately releases a valid late response. The
            // worker must discard it instead of applying/staging stale output.
            Ok(json!({"schema_version":memory_contract::SCHEMA_VERSION,"candidates":candidates}))
        })
    }

    fn cancel<'a>(&'a self, request_id: &'a str) -> CancelFuture<'a> {
        Box::pin(async move {
            self.cancellations
                .lock()
                .unwrap()
                .push(request_id.to_owned());
            self.release.send_replace(true);
            Ok(())
        })
    }
}

async fn settled(f: &Fixture) -> Result<Value> {
    timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = f.app.memory_review_snapshot(&f.lid, &f.sid)?;
            if snapshot["review_running"] == false {
                return Ok(snapshot);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}

async fn assert_no_automatic_retry(
    f: &Fixture,
    provider: &Fake,
    expected_calls: usize,
) -> Result<()> {
    let before = counts(f)?;
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        ensure!(f.app.memory_review_snapshot(&f.lid, &f.sid)?["review_running"] == false);
    }
    ensure!(provider.calls.load(Ordering::SeqCst) == expected_calls);
    ensure!(provider.active.load(Ordering::SeqCst) == 0);
    ensure!(counts(f)? == before);
    Ok(())
}

#[tokio::test]
async fn manual_review_targets_only_selected_sources_and_waits_for_retain_or_dismiss() -> Result<()>
{
    let f = fixture(true)?;
    let provider = Fake::new(true);
    let started = f
        .app
        .start_memory_review_with(&f.lid, &f.sid, &intent(&f)?, provider.clone())
        .await?;
    ensure!(started["review_running"] == true);
    timeout(Duration::from_secs(2), provider.entered.notified()).await?;
    ensure!(
        f.app.stores.try_lock().is_ok(),
        "provider await must not hold Store lock"
    );
    {
        let requests = provider.requests.lock().unwrap();
        ensure!(requests.len() == 1);
        let request = &requests[0];
        ensure!(request["learner_id"] == f.lid && request["session_id"] == f.sid);
        ensure!(request["context"]["namespace"] == "simulator");
        ensure!(request["context"]["output_status"] == "shadow_only");
        let sources = request["context"]["sources"].as_array().unwrap();
        ensure!(sources.len() == f.source_ids.len());
        for source in sources {
            ensure!(f.source_ids.iter().any(|id| source["event_id"] == *id));
            ensure!(source["source_type"] == "simulated" && source["event_kind"] == "observation");
            ensure!(source["content"].get("synthetic_fixture").is_none());
            ensure!(!source.to_string().contains("foreign learner"));
        }
    }
    provider.release.send_replace(true);
    let staged = settled(&f).await?;
    ensure!(staged["review_outcome"] == "shadow_staged");
    let candidates = staged["candidates"].as_array().unwrap();
    ensure!(candidates.len() == 2 && candidates.iter().all(|row| row["can_retain"] == true));
    ensure!(
        candidates
            .iter()
            .all(|row| row["payload"]["source_grade"] == "simulated")
    );
    let retained_id = s(&candidates[0], "candidate_id").to_owned();
    let dismissed_id = s(&candidates[1], "candidate_id").to_owned();
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        ensure!(store.memories(&f.lid)?.is_empty());
        ensure!(
            store
                .recall_for_session(&f.sid, "Synthetic target report", 5)?
                .is_empty()
        );
        let foreign = store.memory_job_status(&f.other_lid, f.other_job.as_deref().unwrap())?;
        ensure!(foreign["status"] == "pending" && foreign["attempts"] == 0);
    }
    let retained = f
        .app
        .decide_memory_review(&f.lid, &f.sid, &retained_id, "retain")
        .await?;
    ensure!(retained["status"] == "applied" && retained["replayed"] == false);
    let mid = s(&retained, "memory_id").to_owned();
    ensure!(!mid.is_empty());
    let replayed = f
        .app
        .decide_memory_review(&f.lid, &f.sid, &retained_id, "retain")
        .await?;
    ensure!(replayed["replayed"] == true && replayed["memory_id"] == mid);
    let dismissed = f
        .app
        .decide_memory_review(&f.lid, &f.sid, &dismissed_id, "dismiss")
        .await?;
    ensure!(dismissed["status"] == "dismissed" && dismissed["replayed"] == false);
    ensure!(
        f.app
            .decide_memory_review(&f.lid, &f.sid, &dismissed_id, "dismiss")
            .await?["replayed"]
            == true
    );
    ensure!(
        f.app
            .decide_memory_review(&f.lid, &f.sid, &dismissed_id, "retain")
            .await
            .is_err()
    );
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let recalled = store.recall_for_session(&f.sid, "Synthetic target report", 5)?;
        ensure!(recalled.len() == 1 && recalled[0]["memory_id"] == mid);
        ensure!(recalled[0]["learner_id"] == f.lid && recalled[0]["source_type"] == "simulated");
        ensure!(
            store.memory_evidence(&f.lid, &mid)?["sources"]
                .as_array()
                .unwrap()
                .len()
                == 1
        );
        ensure!(store.memories(&f.lid)?.len() == 1 && store.memories(&f.other_lid)?.is_empty());
        let foreign = store.memory_job_status(&f.other_lid, f.other_job.as_deref().unwrap())?;
        ensure!(foreign["status"] == "pending" && foreign["attempts"] == 0);
    }
    assert_no_automatic_retry(&f, &provider, 1).await?;
    timeout(Duration::from_secs(3), f.app.close()).await?;
    Ok(())
}

#[tokio::test]
async fn manual_review_rejects_wrong_owner_and_stale_versions_before_enqueuing_or_calling()
-> Result<()> {
    let f = fixture(false)?;
    let provider = Fake::new(false);
    let body = intent(&f)?;
    let before = counts(&f)?;
    ensure!(
        f.app
            .start_memory_review_with(&f.other_lid, &f.sid, &body, provider.clone())
            .await
            .is_err()
    );
    ensure!(
        f.app
            .start_memory_review_with(&f.lid, &f.other_sid, &body, provider.clone())
            .await
            .is_err()
    );
    ensure!(
        f.app
            .start_memory_review_with(&f.other_lid, &f.other_sid, &body, provider.clone())
            .await
            .is_err()
    );
    for field in [
        "expected_generation",
        "expected_epoch",
        "expected_state_revision",
    ] {
        let mut stale = body.clone();
        stale[field] = json!(body[field].as_i64().unwrap() + 1);
        ensure!(
            f.app
                .start_memory_review_with(&f.lid, &f.sid, &stale, provider.clone())
                .await
                .is_err()
        );
    }
    tokio::task::yield_now().await;
    ensure!(counts(&f)? == before && before["jobs"] == 0);
    ensure!(
        provider.calls.load(Ordering::SeqCst) == 0 && provider.requests.lock().unwrap().is_empty()
    );
    ensure!(f.app.memory_review_snapshot(&f.lid, &f.sid)?["review_running"] == false);
    ensure!(f.app.manual_memory_review.lock().unwrap().is_none());
    timeout(Duration::from_secs(3), f.app.close()).await?;
    Ok(())
}

#[tokio::test]
async fn cancelling_manual_review_reaps_provider_and_never_stages_its_late_response() -> Result<()>
{
    let f = fixture(false)?;
    let provider = Fake::new(true);
    f.app
        .start_memory_review_with(&f.lid, &f.sid, &intent(&f)?, provider.clone())
        .await?;
    timeout(Duration::from_secs(2), provider.entered.notified()).await?;
    // A request for another legitimate owner must not preempt this review.
    let _ = f.app.cancel_memory_review(&f.other_lid, &f.other_sid).await;
    ensure!(provider.cancellations.lock().unwrap().is_empty());
    ensure!(f.app.memory_review_snapshot(&f.lid, &f.sid)?["review_running"] == true);
    f.app.cancel_memory_review(&f.lid, &f.sid).await?;
    let snapshot = settled(&f).await?;
    ensure!(snapshot["review_outcome"] == "preempted");
    ensure!(snapshot["candidates"].as_array().unwrap().is_empty());
    ensure!(counts(&f)?["candidates"] == 0 && counts(&f)?["memories"] == 0);
    let request_id = s(&provider.requests.lock().unwrap()[0], "request_id").to_owned();
    ensure!(*provider.cancellations.lock().unwrap() == vec![request_id]);
    assert_no_automatic_retry(&f, &provider, 1).await?;
    ensure!(f.app.memory_worker.is_idle());
    timeout(Duration::from_secs(3), f.app.close()).await?;
    Ok(())
}

#[tokio::test]
async fn foreground_preemption_settles_manual_review_without_automatic_restart() -> Result<()> {
    let f = fixture(false)?;
    let provider = Fake::new(true);
    f.app
        .start_memory_review_with(&f.lid, &f.sid, &intent(&f)?, provider.clone())
        .await?;
    timeout(Duration::from_secs(2), provider.entered.notified()).await?;
    let foreground = f.app.memory_worker.foreground();
    let snapshot = settled(&f).await?;
    ensure!(snapshot["review_outcome"] == "preempted");
    ensure!(snapshot["worker_busy"] == true);
    ensure!(provider.cancellations.lock().unwrap().len() == 1);
    ensure!(provider.active.load(Ordering::SeqCst) == 0);
    ensure!(counts(&f)?["attempts"] == 1 && counts(&f)?["candidates"] == 0);
    drop(foreground);
    assert_no_automatic_retry(&f, &provider, 1).await?;
    ensure!(f.app.memory_worker.is_idle());
    timeout(Duration::from_secs(3), f.app.close()).await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_joins_manual_review_and_leaves_no_provider_future_or_task_owner() -> Result<()> {
    let f = fixture(false)?;
    let provider = Fake::new(true);
    f.app
        .start_memory_review_with(&f.lid, &f.sid, &intent(&f)?, provider.clone())
        .await?;
    timeout(Duration::from_secs(2), provider.entered.notified()).await?;
    timeout(Duration::from_secs(3), f.app.close()).await?;
    ensure!(f.app.shutting_down.load(Ordering::SeqCst));
    ensure!(f.app.manual_memory_review.lock().unwrap().is_none());
    ensure!(f.app.memory_worker.is_idle());
    ensure!(
        provider.calls.load(Ordering::SeqCst) == 1 && provider.active.load(Ordering::SeqCst) == 0
    );
    ensure!(provider.cancellations.lock().unwrap().len() == 1);
    ensure!(counts(&f)?["candidates"] == 0 && counts(&f)?["memories"] == 0);
    {
        let stores = f.app.stores.lock().unwrap();
        ensure!(
            stores["simulator"]
                .rows("SELECT job_id FROM memory_jobs WHERE status='leased'", [])?
                .is_empty()
        );
    }
    ensure!(
        f.app
            .start_memory_review_with(&f.lid, &f.sid, &intent(&f)?, provider.clone())
            .await
            .is_err()
    );
    ensure!(provider.calls.load(Ordering::SeqCst) == 1);
    Ok(())
}

#[tokio::test]
async fn immediate_cancel_before_provider_start_is_not_lost_and_leaves_work_resumable() -> Result<()>
{
    let f = fixture(false)?;
    let provider = Fake::new(true);
    f.app
        .start_memory_review_with(&f.lid, &f.sid, &intent(&f)?, provider.clone())
        .await?;
    // Deliberately do not wait for Fake::entered or yield to the spawned task.
    // Cancellation must survive until its first poll and prevent a late claim.
    f.app.cancel_memory_review(&f.lid, &f.sid).await?;
    let snapshot = settled(&f).await?;
    ensure!(["preempted_before_claim", "preempted"].contains(&s(&snapshot, "review_outcome")));
    ensure!(snapshot["job"]["status"] == "pending");
    ensure!(snapshot["candidates"].as_array().unwrap().is_empty());
    let calls = provider.calls.load(Ordering::SeqCst);
    ensure!(calls <= 1 && provider.active.load(Ordering::SeqCst) == 0);
    ensure!(provider.cancellations.lock().unwrap().len() == calls);
    ensure!(counts(&f)?["attempts"].as_u64().unwrap() <= 1);
    ensure!(counts(&f)?["candidates"] == 0 && counts(&f)?["memories"] == 0);
    assert_no_automatic_retry(&f, &provider, calls).await?;
    ensure!(f.app.memory_worker.is_idle());
    timeout(Duration::from_secs(3), f.app.close()).await?;
    ensure!(f.app.manual_memory_review.lock().unwrap().is_none());
    Ok(())
}

async fn review_http(
    app: Arc<Workstation>,
    method: &str,
    path: &str,
    body: &Value,
    authenticated: bool,
) -> Result<(StatusCode, Value)> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "127.0.0.1:48800")
        .header("Content-Type", "application/json");
    if authenticated {
        request = request.header("Cookie", "ai_tutor_local=synthetic-review-token");
    }
    let response = crate::server::router(app)
        .oneshot(request.body(Body::from(body.to_string()))?)
        .await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

#[tokio::test]
async fn review_router_enforces_auth_owner_provider_readiness_and_decision_payload() -> Result<()> {
    let f = fixture(false)?;
    let path = format!("/api/learners/{}/sessions/{}/memory-review", f.lid, f.sid);
    let body = intent(&f)?;
    let before = counts(&f)?;
    ensure!(
        review_http(f.app.clone(), "GET", &path, &json!({}), false)
            .await?
            .0
            == StatusCode::UNAUTHORIZED
    );
    ensure!(
        review_http(f.app.clone(), "POST", &path, &body, false)
            .await?
            .0
            == StatusCode::UNAUTHORIZED
    );
    let (status, snapshot) = review_http(f.app.clone(), "GET", &path, &json!({}), true).await?;
    ensure!(
        status == StatusCode::OK
            && snapshot["learner_id"] == f.lid
            && snapshot["session_id"] == f.sid
    );
    ensure!(snapshot["provider_available"] == false && snapshot["review_running"] == false);
    ensure!(
        review_http(f.app.clone(), "POST", &path, &body, true)
            .await?
            .0
            == StatusCode::BAD_REQUEST
    );
    let wrong = format!(
        "/api/learners/{}/sessions/{}/memory-review",
        f.other_lid, f.sid
    );
    ensure!(
        review_http(f.app.clone(), "GET", &wrong, &json!({}), true)
            .await?
            .0
            == StatusCode::BAD_REQUEST
    );
    ensure!(counts(&f)? == before && f.app.manual_memory_review.lock().unwrap().is_none());
    ensure!(
        f.app.provider.status()["metrics"]["requests"]
            .as_u64()
            .unwrap_or(0)
            == 0
    );

    // Stage through the injected test provider, then exercise the real router's
    // review action validation without enabling a production provider.
    let provider = Fake::new(false);
    f.app
        .start_memory_review_with(&f.lid, &f.sid, &body, provider.clone())
        .await?;
    let staged = settled(&f).await?;
    let cid = s(&staged["candidates"][0], "candidate_id");
    ensure!(!cid.is_empty());
    let decision_path = format!("{path}/candidates/{cid}");
    let staged_counts = counts(&f)?;
    ensure!(
        review_http(
            f.app.clone(),
            "POST",
            &decision_path,
            &json!({"decision":"retain"}),
            false
        )
        .await?
        .0 == StatusCode::UNAUTHORIZED
    );
    for invalid in [
        json!({}),
        json!({"decision":"apply"}),
        json!({"decision":true}),
        json!({"decision":"retain","apply":true}),
    ] {
        ensure!(
            review_http(f.app.clone(), "POST", &decision_path, &invalid, true)
                .await?
                .0
                == StatusCode::BAD_REQUEST
        );
    }
    ensure!(
        review_http(
            f.app.clone(),
            "POST",
            &format!("{wrong}/candidates/{cid}"),
            &json!({"decision":"retain"}),
            true
        )
        .await?
        .0 == StatusCode::BAD_REQUEST
    );
    ensure!(counts(&f)? == staged_counts && staged_counts["memories"] == 0);
    let (status, dismissed) = review_http(
        f.app.clone(),
        "POST",
        &decision_path,
        &json!({"decision":"dismiss"}),
        true,
    )
    .await?;
    ensure!(status == StatusCode::OK && dismissed["status"] == "dismissed");
    let (_, after) = review_http(f.app.clone(), "GET", &path, &json!({}), true).await?;
    ensure!(after["candidates"].as_array().unwrap().len() == 1);
    ensure!(counts(&f)?["memories"] == 0 && provider.calls.load(Ordering::SeqCst) == 1);
    timeout(Duration::from_secs(3), f.app.close()).await?;
    Ok(())
}

#[tokio::test]
async fn manual_review_recovers_an_expired_lease_without_resetting_its_attempt_history()
-> Result<()> {
    let f = fixture(false)?;
    let provider = Fake::new(false);
    let now = chrono::Utc::now().timestamp_millis();
    let claimed_at = now - 130_000;
    let job_id = {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let limits = MemoryJobLimits::default();
        let prepared = store.prepare_memory_review(&f.lid, &f.sid, &limits, claimed_at)?;
        let job_id = s(&prepared["job"], "job_id").to_owned();
        let claim = store
            .claim_memory_job_for("expired-manual-review", &limits, claimed_at, &job_id)?
            .ok_or_else(|| anyhow!("fixture could not claim the manual review"))?;
        ensure!(claim["receipt"]["lease_until_ms"].as_i64().unwrap() < now);
        let leased = store.memory_job_status(&f.lid, &job_id)?;
        ensure!(leased["status"] == "leased" && leased["attempts"] == 1);
        job_id
    };
    // Simulate reopening after the previous owner disappeared. No SQL update
    // or live ReviewRun is used to manufacture the expired lease.
    ensure!(f.app.manual_memory_review.lock().unwrap().is_none());
    f.app
        .start_memory_review_with(&f.lid, &f.sid, &intent(&f)?, provider.clone())
        .await?;
    let completed = settled(&f).await?;
    ensure!(completed["review_outcome"] == "shadow_staged");
    ensure!(completed["job"]["job_id"] == job_id);
    ensure!(completed["job"]["status"] == "shadow_ready" && completed["job"]["attempts"] == 2);
    ensure!(completed["candidates"].as_array().unwrap().len() == 2);
    ensure!(provider.calls.load(Ordering::SeqCst) == 1);
    {
        let requests = provider.requests.lock().unwrap();
        ensure!(requests.len() == 1 && requests[0]["_memory_job_id"] == job_id);
        ensure!(requests[0]["_memory_attempt"] == 2);
    }
    {
        let stores = f.app.stores.lock().unwrap();
        let job = stores["simulator"].memory_job_status(&f.lid, &job_id)?;
        let receipts = job["attempt_receipts"].as_array().unwrap();
        ensure!(receipts.len() == 2);
        ensure!(receipts[0]["attempt_no"] == 1 && receipts[0]["status"] == "lease_expired");
        ensure!(receipts[1]["attempt_no"] == 2 && receipts[1]["status"] == "response_received");
    }
    let totals = counts(&f)?;
    ensure!(totals["jobs"] == 1 && totals["attempts"] == 2 && totals["memories"] == 0);
    timeout(Duration::from_secs(3), f.app.close()).await?;
    Ok(())
}

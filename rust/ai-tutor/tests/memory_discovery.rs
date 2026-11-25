use ai_tutor::{
    kimi::memory_contract::PROMPT_VERSION,
    store::{MemoryJobLimits, Store, s},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

fn fixture(store: &Store, marked: bool) -> Result<(String, String, String)> {
    let learner = store.create_learner("synthetic discovery", "en", "guided")?;
    let lid = s(&learner, "learner_id").to_owned();
    let session = store.create_session(&lid)?;
    let sid = s(&session, "session_id").to_owned();
    let event = store.record_education_event(
        &lid,
        &sid,
        "simulated",
        "observation",
        &json!({"text":"Synthetic green light report.","synthetic_fixture":marked}),
    )?;
    Ok((lid, sid, s(&event, "event_id").to_owned()))
}
fn discover(store: &Store) -> Result<Value> {
    store.discover_memory_jobs(
        PROMPT_VERSION,
        &Default::default(),
        true,
        chrono::Utc::now().timestamp_millis(),
    )
}
fn count(store: &Store, table: &str) -> Result<i64> {
    Ok(store
        .one(&format!("SELECT COUNT(*) AS count FROM {table}"), [])?
        .unwrap()["count"]
        .as_i64()
        .unwrap())
}

#[test]
fn migration_backfills_typed_sessions_once_and_reopen_does_not_revive_due() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("migration.sqlite3");
    let store = Store::open(&path, "simulator")?;
    fixture(&store, true)?;
    // Recreate the previous released schema, where neither the due table nor
    // its triggers existed. Dropping only the table leaves dangling triggers;
    // that is a corrupted newer DB, not a real pre-migration database.
    store.db.execute_batch(
        "
        DROP TRIGGER memory_schedule_event_insert;
        DROP TRIGGER memory_schedule_event_update;
        DROP TRIGGER memory_schedule_revision;
        DROP TRIGGER memory_schedule_generation;
        DROP TRIGGER memory_schedule_session_delete;
        DROP TRIGGER memory_schedule_learner_delete;
        DROP TABLE memory_schedule_due;
    ",
    )?;
    drop(store);
    let store = Store::open(&path, "simulator")?;
    ensure!(count(&store, "memory_schedule_due")? == 1);
    ensure!(discover(&store)?["enqueued_or_existing"] == 1);
    ensure!(count(&store, "memory_schedule_due")? == 0 && count(&store, "memory_jobs")? == 1);
    drop(store);
    let reopened = Store::open(&path, "simulator")?;
    ensure!(count(&reopened, "memory_schedule_due")? == 0);
    ensure!(discover(&reopened)?["inspected"] == 0 && count(&reopened, "memory_jobs")? == 1);
    Ok(())
}

#[test]
fn finished_session_generation_change_is_rediscovered_without_being_current() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("finished.sqlite3"), "simulator")?;
    let (lid, sid, _) = fixture(&store, true)?;
    store.finish_session(&sid)?;
    discover(&store)?;
    let first = store.one("SELECT job_id FROM memory_jobs", [])?.unwrap();
    fixture(&store, true)?; // Another learner/session is now newer.
    store.bump(&sid, "idle")?; // Select/cancel after finishing preserves finished.
    ensure!(store.session(&sid)?["status"] == "finished");
    ensure!(store.memory_job_status(&lid, s(&first, "job_id"))?["status"] == "invalidated");
    discover(&store)?;
    let replacement = store
        .one(
            "SELECT job_id FROM memory_jobs WHERE session_id=? AND status='pending'",
            [&sid],
        )?
        .unwrap();
    ensure!(replacement["job_id"] != first["job_id"]);
    ensure!(
        store.memory_job_status(&lid, s(&replacement, "job_id"))?["generation"]
            == store.session(&sid)?["generation"]
    );
    Ok(())
}

#[test]
fn discovery_bounds_sessions_and_sources_and_skips_unmarked_or_legacy_evidence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("bounded.sqlite3"), "simulator")?;
    let (lid, sid, _) = fixture(&store, true)?;
    for index in 0..20 {
        store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "observation",
            &json!({"text":format!("Synthetic report {index}"),"synthetic_fixture":true}),
        )?;
    }
    let legacy = store.event(
        &lid,
        &sid,
        "simulated",
        &json!({"text":"Legacy statement","synthetic_fixture":true}),
    )?;
    let unmarked = store.record_education_event(
        &lid,
        &sid,
        "simulated",
        "observation",
        &json!({"text":"Unmarked report"}),
    )?;
    for _ in 0..5 {
        fixture(&store, true)?;
    }
    let result = discover(&store)?;
    // The large session remains dirty, after the two not-yet-inspected sessions.
    ensure!(result["inspected"] == 4 && count(&store, "memory_schedule_due")? == 3);
    let job = store
        .one("SELECT job_id FROM memory_jobs WHERE session_id=?", [&sid])?
        .unwrap();
    let status = store.memory_job_status(&lid, s(&job, "job_id"))?;
    let snapshot = status["source_snapshot"].as_array().unwrap();
    ensure!(snapshot.len() == 12);
    ensure!(
        !snapshot
            .iter()
            .any(|row| row["event_id"] == legacy || row["event_id"] == unmarked["event_id"])
    );
    Ok(())
}

#[test]
fn full_queue_preserves_dirty_sessions_and_invalid_prompt_rolls_back_clear() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("full.sqlite3"), "simulator")?;
    fixture(&store, true)?;
    fixture(&store, true)?;
    ensure!(
        store
            .discover_memory_jobs(
                "",
                &Default::default(),
                true,
                chrono::Utc::now().timestamp_millis()
            )
            .is_err()
    );
    ensure!(count(&store, "memory_schedule_due")? == 2 && count(&store, "memory_jobs")? == 0);
    let limits = MemoryJobLimits {
        max_pending: 1,
        ..Default::default()
    };
    let result = store.discover_memory_jobs(
        PROMPT_VERSION,
        &limits,
        true,
        chrono::Utc::now().timestamp_millis(),
    )?;
    ensure!(result["queue_full"] == true && result["enqueued_or_existing"] == 1);
    ensure!(count(&store, "memory_schedule_due")? == 1 && count(&store, "memory_jobs")? == 1);
    Ok(())
}

#[test]
fn deleted_sessions_learners_and_forgotten_sources_cannot_be_discovered_again() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("deleted.sqlite3"), "simulator")?;
    let (_, deleted_sid, _) = fixture(&store, true)?;
    let (deleted_lid, _, _) = fixture(&store, true)?;
    let (forgotten, _, _) = fixture(&store, true)?;
    store
        .db
        .execute("DELETE FROM sessions WHERE session_id=?", [&deleted_sid])?;
    store
        .db
        .execute("DELETE FROM learners WHERE learner_id=?", [&deleted_lid])?;
    store.forget(&forgotten, None)?;
    ensure!(discover(&store)?["enqueued_or_existing"] == 0);
    ensure!(count(&store, "memory_schedule_due")? == 0 && count(&store, "memory_jobs")? == 0);
    Ok(())
}

#[test]
fn generating_sessions_stay_dirty_until_idle_and_unmarked_sessions_cost_no_call() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("idle.sqlite3"), "simulator")?;
    let (_, sid, _) = fixture(&store, true)?;
    fixture(&store, false)?;
    store.bump(&sid, "generating")?;
    ensure!(discover(&store)?["enqueued_or_existing"] == 0);
    ensure!(count(&store, "memory_schedule_due")? == 1 && count(&store, "memory_jobs")? == 0);
    store.set_status(&sid, "idle")?;
    ensure!(discover(&store)?["enqueued_or_existing"] == 1);
    ensure!(count(&store, "memory_job_attempts")? == 0);
    Ok(())
}

fn burst(store: &Store, size: usize) -> Result<(String, String, Vec<String>)> {
    let (lid, sid, first) = fixture(store, true)?;
    let mut ids = vec![first];
    for index in 1..size {
        let event = store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "observation",
            &json!({"text":format!("Synthetic burst record {index}"),"synthetic_fixture":true}),
        )?;
        ids.push(s(&event, "event_id").to_owned());
    }
    Ok((lid, sid, ids))
}

fn jobs_for(store: &Store, lid: &str, sid: &str) -> Result<Vec<Value>> {
    store
        .rows(
            "SELECT job_id FROM memory_jobs WHERE session_id=? ORDER BY rowid",
            [sid],
        )?
        .iter()
        .map(|row| store.memory_job_status(lid, s(row, "job_id")))
        .collect()
}

fn snapshot_ids(job: &Value) -> HashSet<String> {
    job["source_snapshot"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| s(source, "event_id").to_owned())
        .collect()
}

#[test]
fn twenty_five_source_burst_drains_in_three_disjoint_oldest_first_batches() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("burst.sqlite3");
    let store = Store::open(&path, "simulator")?;
    let (lid, sid, ids) = burst(&store, 25)?;
    for (batch, expected_size) in [12, 12, 1].into_iter().enumerate() {
        ensure!(discover(&store)?["enqueued_or_existing"] == 1);
        let jobs = jobs_for(&store, &lid, &sid)?;
        ensure!(jobs.len() == batch + 1);
        let expected: HashSet<_> = ids[batch * 12..batch * 12 + expected_size]
            .iter()
            .cloned()
            .collect();
        ensure!(snapshot_ids(&jobs[batch]) == expected);
        ensure!(count(&store, "memory_schedule_due")? == if batch < 2 { 1 } else { 0 });
    }
    ensure!(discover(&store)?["inspected"] == 0);
    ensure!(count(&store, "memory_jobs")? == 3);
    ensure!(count(&store, "memory_job_attempts")? == 0);
    ensure!(store.memories(&lid)?.is_empty());
    drop(store);
    let reopened = Store::open(&path, "simulator")?;
    ensure!(discover(&reopened)?["inspected"] == 0);
    ensure!(count(&reopened, "memory_jobs")? == 3);
    Ok(())
}

#[test]
fn coverage_requires_the_actual_source_id_and_version_not_merely_a_session_job() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("coverage.sqlite3"), "simulator")?;
    let (lid, sid, ids) = burst(&store, 13)?;
    store.enqueue_memory_job(
        &lid,
        &sid,
        &ids[12..],
        PROMPT_VERSION,
        &Default::default(),
        chrono::Utc::now().timestamp_millis(),
    )?;
    ensure!(discover(&store)?["enqueued_or_existing"] == 1);
    let jobs = jobs_for(&store, &lid, &sid)?;
    ensure!(jobs.len() == 2);
    ensure!(snapshot_ids(&jobs[0]) == HashSet::from([ids[12].clone()]));
    ensure!(snapshot_ids(&jobs[1]) == ids[..12].iter().cloned().collect());
    ensure!(count(&store, "memory_schedule_due")? == 0);
    // This isolated metadata change exercises the existing version trigger;
    // original event content remains immutable.
    store.db.execute(
        "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
        [&ids[0]],
    )?;
    ensure!(store.memory_job_status(&lid, s(&jobs[1], "job_id"))?["status"] == "invalidated");
    ensure!(discover(&store)?["enqueued_or_existing"] == 1);
    let all = jobs_for(&store, &lid, &sid)?;
    let replacement = all.last().unwrap();
    ensure!(replacement["job_id"] != jobs[1]["job_id"]);
    ensure!(snapshot_ids(replacement) == ids[..12].iter().cloned().collect());
    ensure!(
        replacement["source_snapshot"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["event_id"] == ids[0] && row["event_version"] == 2)
    );
    ensure!(count(&store, "memory_schedule_due")? == 0);

    let state = store.session(&sid)?["state"].clone();
    store.checkpoint(&sid, &state)?;
    let revision = store.session(&sid)?["state_revision"].clone();
    discover(&store)?;
    discover(&store)?;
    let current: Vec<_> = jobs_for(&store, &lid, &sid)?
        .into_iter()
        .filter(|job| job["status"] == "pending")
        .collect();
    ensure!(current.len() == 2 && current.iter().all(|job| job["state_revision"] == revision));
    let current_ids: HashSet<_> = current.iter().flat_map(snapshot_ids).collect();
    ensure!(current_ids == ids.iter().cloned().collect());
    store
        .db
        .execute("DELETE FROM events WHERE event_id=?", [&ids[0]])?;
    discover(&store)?;
    let remaining: HashSet<_> = jobs_for(&store, &lid, &sid)?
        .iter()
        .filter(|job| job["status"] == "pending")
        .flat_map(snapshot_ids)
        .collect();
    ensure!(remaining == ids[1..].iter().cloned().collect());
    ensure!(count(&store, "memory_schedule_due")? == 0);
    ensure!(store.memories(&lid)?.is_empty());
    Ok(())
}

#[test]
fn queue_full_and_preemption_keep_the_tail_without_resetting_exhaustion_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("preempted-tail.sqlite3"), "simulator")?;
    let (lid, sid, ids) = burst(&store, 13)?;
    let limits = MemoryJobLimits {
        max_pending: 1,
        max_attempts: 2,
        ..Default::default()
    };
    let now = chrono::Utc::now().timestamp_millis();
    store.discover_memory_jobs(PROMPT_VERSION, &limits, true, now)?;
    let first = store
        .claim_memory_job("first-owner", &limits, now + 1)?
        .unwrap();
    store.mark_memory_job_started(&first["receipt"], now + 2)?;
    store.preempt_memory_job(&first["receipt"], now + 3)?;
    let blocked = store.discover_memory_jobs(PROMPT_VERSION, &limits, true, now + 4)?;
    ensure!(blocked["queue_full"] == true && blocked["inspected"] == 0);
    ensure!(count(&store, "memory_schedule_due")? == 1 && count(&store, "memory_jobs")? == 1);
    let second = store
        .claim_memory_job("second-owner", &limits, now + 5)?
        .unwrap();
    ensure!(second["job"]["job_id"] == first["job"]["job_id"] && second["job"]["attempts"] == 2);
    ensure!(
        store
            .stage_memory_candidates(&first["receipt"], &[], now + 6)
            .is_err()
    );
    store.fail_memory_job(&second["receipt"], now + 7)?;
    ensure!(store.memory_job_status(&lid, s(&first["job"], "job_id"))?["status"] == "exhausted");
    ensure!(
        store.discover_memory_jobs(PROMPT_VERSION, &limits, true, now + 8)?["enqueued_or_existing"]
            == 1
    );
    let jobs = jobs_for(&store, &lid, &sid)?;
    ensure!(jobs.len() == 2 && snapshot_ids(&jobs[1]) == HashSet::from([ids[12].clone()]));
    ensure!(jobs[0]["attempts"] == 2 && jobs[1]["attempts"] == 0);
    ensure!(jobs[0]["attempt_receipts"][0]["status"] == "preempted");
    ensure!(jobs[0]["attempt_receipts"][1]["status"] == "failed");
    ensure!(count(&store, "memory_schedule_due")? == 0);
    ensure!(store.memories(&lid)?.is_empty());
    Ok(())
}

#[test]
fn a_retained_burst_rotates_behind_other_dirty_sessions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("fairness.sqlite3"), "simulator")?;
    let (_, large_sid, _) = burst(&store, 25)?;
    let mut small = Vec::new();
    for _ in 0..5 {
        small.push(fixture(&store, true)?.1);
    }
    ensure!(discover(&store)?["inspected"] == 4);
    let remaining = store.rows(
        "SELECT session_id FROM memory_schedule_due ORDER BY rowid",
        [],
    )?;
    ensure!(
        remaining
            .iter()
            .map(|row| s(row, "session_id"))
            .collect::<Vec<_>>()
            == vec![small[3].as_str(), small[4].as_str(), large_sid.as_str()]
    );
    ensure!(discover(&store)?["inspected"] == 3);
    ensure!(count(&store, "memory_schedule_due")? == 1);
    Ok(())
}

#[tokio::test]
async fn oversized_whole_record_exhausts_without_a_provider_call_and_later_batch_can_run()
-> Result<()> {
    use ai_tutor::{
        kimi::memory_contract::SCHEMA_VERSION,
        memory_worker::{
            CancelFuture, MemoryConsolidator, MemoryWorker, MemoryWorkerMode, MemoryWorkerPolicy,
            ProviderFuture,
        },
    };
    use std::{collections::HashMap, sync::Mutex};
    use tokio::sync::watch;
    struct Fake {
        seen: Mutex<Vec<String>>,
    }
    impl MemoryConsolidator for Fake {
        fn consolidate<'a>(
            &'a self,
            request: &'a Value,
            started: watch::Sender<bool>,
        ) -> ProviderFuture<'a> {
            Box::pin(async move {
                self.seen.lock().unwrap().extend(
                    request["context"]["sources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| s(row, "event_id").to_owned()),
                );
                let _ = started.send(true);
                Ok(json!({"schema_version":SCHEMA_VERSION,"candidates":[]}))
            })
        }
        fn cancel<'a>(&'a self, _: &'a str) -> CancelFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("oversized.sqlite3"), "simulator")?;
    let learner = store.create_learner("synthetic oversized source", "en", "guided")?;
    let lid = s(&learner, "learner_id").to_owned();
    let session = store.create_session(&lid)?;
    let sid = s(&session, "session_id").to_owned();
    let huge = "x".repeat(30_000);
    let first = store.record_education_event(
        &lid,
        &sid,
        "simulated",
        "observation",
        &json!({"text":huge,"synthetic_fixture":true}),
    )?;
    // Ineligible records precede later eligible ones and must not fill the batch.
    for _ in 0..30 {
        store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "observation",
            &json!({"text":"Unmarked synthetic event"}),
        )?;
    }
    let mut last = String::new();
    for index in 1..13 {
        let event = store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "observation",
            &json!({"text":format!("Later valid event {index}"),"synthetic_fixture":true}),
        )?;
        last = s(&event, "event_id").to_owned();
    }
    let policy = MemoryWorkerPolicy {
        mode: MemoryWorkerMode::SyntheticOnly,
        limits: MemoryJobLimits {
            max_pending: 1,
            max_attempts: 2,
            ..Default::default()
        },
    };
    store.discover_memory_jobs(
        PROMPT_VERSION,
        &policy.limits,
        true,
        chrono::Utc::now().timestamp_millis(),
    )?;
    let stores = Mutex::new(HashMap::from([("simulator".into(), store)]));
    let fake = Fake {
        seen: Mutex::new(Vec::new()),
    };
    let worker = MemoryWorker::new();
    for _ in 0..2 {
        let result = worker
            .run_once(&stores, &fake, "simulator", &policy)
            .await?;
        ensure!(result["status"] == "failed" && result["provider_started"] == false);
        ensure!(fake.seen.lock().unwrap().is_empty());
    }
    {
        let stores = stores.lock().unwrap();
        let store = &stores["simulator"];
        let first_job = jobs_for(store, &lid, &sid)?[0].clone();
        ensure!(first_job["status"] == "exhausted" && first_job["attempts"] == 2);
        ensure!(
            first_job["attempt_receipts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["started_at_ms"].is_null())
        );
        ensure!(
            store.source_events(&lid, &[s(&first, "event_id").to_owned()])?[0]["content"]["text"]
                == huge
        );
        ensure!(
            store.discover_memory_jobs(
                PROMPT_VERSION,
                &policy.limits,
                true,
                chrono::Utc::now().timestamp_millis()
            )?["enqueued_or_existing"]
                == 1
        );
    }
    let result = worker
        .run_once(&stores, &fake, "simulator", &policy)
        .await?;
    ensure!(
        result["status"] == "shadow_staged"
            && result["candidate_count"] == 0
            && result["applied"] == false
    );
    ensure!(*fake.seen.lock().unwrap() == vec![last]);
    let stores = stores.lock().unwrap();
    let store = &stores["simulator"];
    ensure!(count(store, "memory_jobs")? == 2 && count(store, "memory_job_attempts")? == 3);
    ensure!(count(store, "memory_schedule_due")? == 0 && store.memories(&lid)?.is_empty());
    Ok(())
}

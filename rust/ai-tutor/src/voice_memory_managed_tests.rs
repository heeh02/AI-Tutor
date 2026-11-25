use super::*;
use crate::{kimi::memory_contract, store::MemoryJobLimits};

fn prepared(case_index: usize) -> Result<(tempfile::TempDir, Store, Fixture, Value)> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("managed-shadow.sqlite3"), "simulator")?;
    let cases: Value = serde_json::from_str(CASES)?;
    let case = cases["tasks"][case_index].clone();
    let learner = store.create_learner("SYNTHETIC_FIXED_SHADOW", s(&case, "language"), "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let source = &case["events"][0];
    let event = store.record_education_event(
        lid,
        sid,
        "simulated",
        s(source, "kind"),
        &source["content"],
    )?;
    store.finish_session(sid)?;
    let fixture = Fixture {
        learner: lid.into(),
        session: sid.into(),
        event: s(&event, "event_id").into(),
        case,
    };
    let method: Value = serde_json::from_str(METHOD)?;
    let limits: MemoryJobLimits = serde_json::from_value(method["limits"].clone())?;
    let now = chrono::Utc::now().timestamp_millis();
    store.discover_memory_jobs(memory_contract::PROMPT_VERSION, &limits, true, now)?;
    let claim = store
        .claim_memory_job("first-owner", &limits, now + 1)?
        .unwrap();
    store.mark_memory_job_started(&claim["receipt"], now + 2)?;
    store.preempt_memory_job(&claim["receipt"], now + 3)?;
    let mut pending = store.memory_job_status(&fixture.learner, s(&claim["job"], "job_id"))?;
    pending["lease_released_observed"] = json!(
        store
            .one(
                "SELECT lease_token IS NULL AS released FROM memory_jobs WHERE job_id=?",
                [s(&claim["job"], "job_id")]
            )?
            .unwrap()["released"]
            == 1
    );
    assert!(preemption_observed(&pending));
    let second = store
        .claim_memory_job("second-owner", &limits, now + 4)?
        .unwrap();
    assert_ne!(
        second["receipt"]["lease_token"],
        claim["receipt"]["lease_token"]
    );
    store.mark_memory_job_started(&second["receipt"], now + 5)?;
    Ok((directory, store, fixture, second))
}
fn candidate(fixture: &Fixture, claim: &Value) -> Value {
    let expected = &fixture.case["expected"];
    let mut versions = serde_json::Map::new();
    versions.insert(
        fixture.event.clone(),
        claim["sources"][0]["event_version"].clone(),
    );
    json!({"kind":expected["kind"],"claim_type":expected["claim_type"],
        "scope":expected["scope"],"support_level":expected["support_level"],
        "content":fixture.case["events"][0]["content"]["text"],
        "source_event_ids":[fixture.event],"source_event_versions":versions,
        "source_quotes":[{"event_id":fixture.event,"quote":fixture.case["events"][0]["content"]["text"]}],
        "importance":{"goal_relevance":2,"continuity":2,"novel_change":1,"explicit_request":3}})
}

#[test]
fn fixed_educational_sources_survive_real_discovery_preemption_and_shadow_recovery() -> Result<()> {
    for index in 0..2 {
        let (_directory, store, fixture, claim) = prepared(index)?;
        assert!(store.memories(&fixture.learner)?.is_empty());
        store.stage_memory_candidates(
            &claim["receipt"],
            &[candidate(&fixture, &claim)],
            chrono::Utc::now().timestamp_millis() + 20,
        )?;
        let job = store.memory_job_status(&fixture.learner, s(&claim["job"], "job_id"))?;
        assert_eq!(job["status"], "shadow_ready");
        assert_eq!(job["attempts"], 2);
        substantive_shadow(&job, &fixture)?;
        assert!(store.memories(&fixture.learner)?.is_empty());
        let mut wrong = job.clone();
        wrong["candidates"][0]["payload"]["scope"] = json!("current_session");
        assert!(substantive_shadow(&wrong, &fixture).is_err());
        let mut extra = job.clone();
        let mut bad = job["candidates"][0].clone();
        bad["payload"]["kind"] = json!("learning_goal");
        extra["candidates"].as_array_mut().unwrap().push(bad);
        assert!(substantive_shadow(&extra, &fixture).is_err());
    }
    Ok(())
}

#[test]
fn empty_real_shadow_response_is_completed_but_fails_substantive_coverage() -> Result<()> {
    let (_directory, store, fixture, claim) = prepared(1)?;
    store.stage_memory_candidates(
        &claim["receipt"],
        &[],
        chrono::Utc::now().timestamp_millis() + 20,
    )?;
    let job = store.memory_job_status(&fixture.learner, s(&claim["job"], "job_id"))?;
    assert_eq!(job["status"], "completed");
    assert!(substantive_shadow(&job, &fixture).is_err());
    Ok(())
}

#[test]
fn reservation_or_natural_completion_cannot_count_as_observed_preemption() {
    let mut value = json!({"status":"pending","attempts":1,"lease_released_observed":true,"candidates":[],
        "attempt_receipts":[{"status":"preempted","started_at_ms":null}]});
    assert!(!preemption_observed(&value));
    value["attempt_receipts"][0]["started_at_ms"] = json!(123);
    assert!(preemption_observed(&value));
    value["attempt_receipts"][0]["status"] = json!("response_received");
    assert!(!preemption_observed(&value));
    value["attempt_receipts"][0]["status"] = json!("preempted");
    value["candidates"] = json!([{"status":"shadow"}]);
    assert!(!preemption_observed(&value));
    value["candidates"] = json!([]);
    value["attempts"] = json!(2);
    assert!(!preemption_observed(&value));
}

#[test]
fn diagnostic_failure_codes_do_not_copy_unknown_error_details() {
    assert_eq!(
        diagnostic_error_code(&anyhow!(
            "consolidation completed with empty candidates; substantive coverage insufficient"
        )),
        "recovery_empty_candidates"
    );
    assert_eq!(
        diagnostic_error_code(&anyhow!(
            "background naturally completed before observed preemption; branch not covered"
        )),
        "background_finished_before_preemption_not_covered"
    );
    let code = diagnostic_error_code(&anyhow!("PRIVATE_SYNTHETIC_CREDENTIAL_AND_PROVIDER_TEXT"));
    assert_eq!(
        code,
        "dependency_operation_failed_see_stage_and_owner_receipts"
    );
    assert!(!code.contains("PRIVATE"));
}

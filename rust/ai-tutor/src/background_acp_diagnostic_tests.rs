use super::*;
use crate::{kimi::memory_contract, store::MemoryJobLimits};

#[test]
fn prepared_fixture_has_fixed_new_source_two_owners_and_no_queued_call() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let fixed = method()?;
    let fixture = prepare_fixture(temporary.path(), &fixed)?;
    let store = Store::open(
        temporary.path().join("runtime/data/simulator.sqlite3"),
        "simulator",
    )?;
    assert_eq!(
        fixture["source_event"]["content"],
        fixed["fixed_source"]["content"]
    );
    assert_eq!(fixture["source_event"]["source_type"], "simulated");
    assert_eq!(fixture["finished_background_session"]["status"], "finished");
    assert_ne!(
        fixture["background_learner_id"],
        fixture["foreground_learner_id"]
    );
    assert!(store.rows("SELECT job_id FROM memory_jobs", [])?.is_empty());
    assert!(
        store
            .rows("SELECT job_id FROM memory_job_attempts", [])?
            .is_empty()
    );
    let bg = s(&fixture, "background_learner_id");
    let fg = s(&fixture, "foreground_learner_id");
    let sid = s(&fixture, "foreground_session_id");
    let eid = s(&fixture["source_event"], "event_id");
    // A separate offline negative test adds a real background-owner memory;
    // it must still be absent from the foreground owner's production context.
    let epoch = store.learner(bg)?["epoch"].as_i64().unwrap();
    assert!(
        store
            .add_memory(
                bg,
                &json!({"kind":"reported_progress","scope":"current_activity",
        "content":fixed["fixed_source"]["content"]["text"],"source_event_ids":[eid]}),
                epoch
            )?
            .is_some()
    );
    let current = store.record_education_event(
        fg,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":fixed["foreground_question"]}),
    )?;
    let context = build_context(
        &store,
        &store.session(sid)?,
        &store.learner(fg)?,
        s(&fixed, "foreground_question"),
        s(&current, "event_id"),
        &json!({"selected_status":"unknown"}),
    )?;
    assert_eq!(context["slow_memory"], json!([]));
    assert!(!context.to_string().contains(eid));
    assert_eq!(
        read(&temporary.path().join("runtime/tutor-delivery.json"))?,
        json!({"protocol_version":1,"mode":"segments","segment_speech":"disabled"})
    );
    Ok(())
}

#[test]
fn live_generation_requires_exact_binding_thought_and_pending_prompt_not_counts() {
    let fixture = json!({"background_session_id":"session"});
    let job = json!({"job_id":"job","status":"leased","attempts":1,
        "attempt_receipts":[{"status":"started","started_at_ms":42}]});
    let active = json!({"transport":"acp_memory_v1","job_id":"job","session_id":"session",
        "attempt_no":1,"request_id":"request","observation_id":"observation","phase":"prompt_response",
        "prompt_write_completed":true,"nonempty_thought_event_observed":true,
        "first_nonempty_thought_event_ms":120.0,"prompt_pending":true,"nonempty_thought_chunks":0});
    assert!(bound_generation(&job, &active, &fixture));
    for (key, value) in [
        ("job_id", json!("other")),
        ("attempt_no", json!(2)),
        ("session_id", json!("other")),
        ("request_id", json!("")),
        ("observation_id", json!("")),
        ("phase", json!("cleanup")),
        ("prompt_pending", json!(false)),
        ("nonempty_thought_event_observed", json!(false)),
        ("first_nonempty_thought_event_ms", Value::Null),
    ] {
        let mut invalid = active.clone();
        invalid[key] = value;
        invalid["nonempty_thought_chunks"] = json!(999);
        assert!(!bound_generation(&job, &invalid, &fixture), "{key}");
    }
    let mut another = active.clone();
    another["observation_id"] = json!("new-observation");
    assert!(!same_observation(&active, &another));
    let mut pending = job.clone();
    pending["status"] = json!("pending");
    pending["attempt_receipts"][0]["status"] = json!("preempted");
    pending["lease_released_observed"] = json!(true);
    pending["candidates"] = json!([]);
    assert!(preempted(&pending));
    pending["attempt_receipts"][0]["status"] = json!("response_received");
    assert!(!preempted(&pending));
}

#[test]
fn real_store_budgeted_recovery_has_new_lease_and_shadow_provenance() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let fixed = method()?;
    let fixture = prepare_fixture(temporary.path(), &fixed)?;
    let store = Store::open(
        temporary.path().join("runtime/data/simulator.sqlite3"),
        "simulator",
    )?;
    let limits: MemoryJobLimits = serde_json::from_value(fixed["limits"].clone())?;
    let now = chrono::Utc::now().timestamp_millis();
    store.discover_memory_jobs(memory_contract::PROMPT_VERSION, &limits, true, now)?;
    let first = store
        .claim_memory_job("first-test-owner", &limits, now + 1)?
        .unwrap();
    store.mark_memory_job_started(&first["receipt"], now + 2)?;
    store.preempt_memory_job(&first["receipt"], now + 3)?;
    let second = store
        .claim_memory_job("second-test-owner", &limits, now + 4)?
        .unwrap();
    assert_eq!(first["job"]["job_id"], second["job"]["job_id"]);
    assert_ne!(
        first["receipt"]["lease_token"],
        second["receipt"]["lease_token"]
    );
    store.mark_memory_job_started(&second["receipt"], now + 5)?;
    let eid = s(&fixture["source_event"], "event_id");
    let text = &fixture["source_event"]["content"]["text"];
    let mut versions = serde_json::Map::new();
    versions.insert(eid.into(), fixture["source_event"]["event_version"].clone());
    let candidate = json!({"kind":"trial_observation","claim_type":"reported_observation",
        "scope":"current_activity","content":text,"source_event_ids":[eid],"source_event_versions":versions,
        "source_quotes":[{"event_id":eid,"quote":text}],"support_level":"independent",
        "importance":{"goal_relevance":2,"continuity":2,"novel_change":2,"explicit_request":0}});
    store.stage_memory_candidates(&second["receipt"], &[candidate], now + 6)?;
    let value = store.memory_job_status(
        s(&fixture, "background_learner_id"),
        s(&second["job"], "job_id"),
    )?;
    assert_eq!(value["status"], "shadow_ready");
    verify_shadow(&value, &fixture)?;
    assert!(
        store
            .memories(s(&fixture, "background_learner_id"))?
            .is_empty()
    );
    let mut empty = value.clone();
    empty["candidates"] = json!([]);
    empty["status"] = json!("completed");
    assert!(verify_shadow(&empty, &fixture).is_err());
    let mut wrong = value.clone();
    wrong["candidates"][0]["payload"]["source_event_versions"][eid] = json!(99);
    assert!(verify_shadow(&wrong, &fixture).is_err());
    Ok(())
}

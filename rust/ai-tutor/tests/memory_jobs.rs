//! Persistence/policy regressions. Synthetic events and temporary SQLite only.
use ai_tutor::store::{MemoryJobLimits, Store, empty_state, s};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

fn time() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-11-04T23:59:00+08:00")
        .unwrap()
        .timestamp_millis()
}
struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    lid: String,
    sid: String,
    event: Value,
}
impl Fixture {
    fn new(kind: &str, content: Value) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("jobs.sqlite3"), "real")?;
        let learner = store.create_learner("synthetic", "en", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        let sid = s(&session, "session_id").to_owned();
        let event = store.record_education_event(&lid, &sid, "learner_report", kind, &content)?;
        Ok(Self {
            dir,
            store,
            lid,
            sid,
            event,
        })
    }
    fn enqueue(&self, version: &str, limits: &MemoryJobLimits) -> Result<Value> {
        self.store.enqueue_memory_job(
            &self.lid,
            &self.sid,
            &[s(&self.event, "event_id").to_owned()],
            version,
            limits,
            time(),
        )
    }
    fn claim(&self, limits: &MemoryJobLimits, now: i64) -> Result<Value> {
        Ok(self
            .store
            .claim_memory_job("fixture-worker", limits, now)?
            .unwrap())
    }
}
fn candidate(event: &Value, kind: &str, claim: &str, content: &str) -> Value {
    let eid = s(event, "event_id");
    let quote = if !s(&event["content"], "text").is_empty() {
        s(&event["content"], "text")
    } else {
        s(&event["content"], "correction")
    };
    let mut value = json!({"kind":kind,"claim_type":claim,"content":content,"scope":"current_activity",
        "source_event_ids":[eid],"source_event_versions":{},"source_quotes":[{"event_id":eid,"quote":quote}],
        "support_level":"unknown","importance":{"goal_relevance":3,"continuity":3,"novel_change":3,"explicit_request":3}});
    value["source_event_versions"][eid] = event["event_version"].clone();
    value
}
fn observation(f: &Fixture) -> Value {
    candidate(
        &f.event,
        "trial_observation",
        "reported_observation",
        "The learner reported that the green light turned on.",
    )
}

#[test]
fn queue_is_idempotent_bounded_and_preemption_receipts_do_not_revive_old_workers() -> Result<()> {
    let f = Fixture::new("observation", json!({"text":"The green light turned on."}))?;
    let limits = MemoryJobLimits {
        max_pending: 1,
        ..Default::default()
    };
    let first = f.enqueue("prompt-v1", &limits)?;
    ensure!(f.enqueue("prompt-v1", &limits)?["job_id"] == first["job_id"]);
    ensure!(f.enqueue("prompt-v2", &limits).is_err());
    let claim = f.claim(&limits, time())?;
    let receipt = &claim["receipt"];
    let status = f.store.memory_job_status(&f.lid, s(&first, "job_id"))?;
    ensure!(status["attempt_receipts"][0]["status"] == "reserved");
    ensure!(status["attempt_receipts"][0]["started_at_ms"].is_null());
    ensure!(status.get("lease_token").is_none());
    f.store.mark_memory_job_started(receipt, time() + 1)?;
    f.store.preempt_memory_job(receipt, time() + 2)?;
    let second = f.claim(&limits, time() + 3)?;
    ensure!(second["receipt"]["lease_token"] != receipt["lease_token"]);
    ensure!(
        f.store
            .stage_memory_candidates(receipt, &[observation(&f)], time() + 4)
            .is_err()
    );
    f.store.fail_memory_job(&second["receipt"], time() + 5)?;
    let third = f.claim(&limits, time() + 6)?;
    f.store.fail_memory_job(&third["receipt"], time() + 7)?;
    ensure!(
        f.store
            .claim_memory_job("fixture-worker", &limits, time() + 8)?
            .is_none()
    );
    let final_state = f.store.memory_job_status(&f.lid, s(&first, "job_id"))?;
    ensure!(final_state["status"] == "exhausted" && final_state["attempts"] == 3);
    ensure!(final_state["attempt_receipts"][0]["status"] == "preempted");
    ensure!(final_state["attempt_receipts"][0]["started_at_ms"] == time() + 1);
    Ok(())
}

#[test]
fn prompt_revision_has_a_distinct_idempotent_job_without_relabeling_old_work() -> Result<()> {
    let f = Fixture::new(
        "observation",
        json!({"text":"Synthetic comparison evidence."}),
    )?;
    let limits = MemoryJobLimits::default();
    let old_version = "educational-memory-v1";
    let new_version = ai_tutor::kimi::memory_contract::PROMPT_VERSION;
    let old = f.enqueue(old_version, &limits)?;
    let current = f.enqueue(new_version, &limits)?;
    ensure!(old["job_id"] != current["job_id"]);
    ensure!(old["prompt_version"] == old_version && current["prompt_version"] == new_version);
    ensure!(f.enqueue(old_version, &limits)?["job_id"] == old["job_id"]);
    ensure!(f.enqueue(new_version, &limits)?["job_id"] == current["job_id"]);
    let reopened = Store::open(f.dir.path().join("jobs.sqlite3"), "real")?;
    ensure!(
        reopened.memory_job_status(&f.lid, s(&old, "job_id"))?["prompt_version"] == old_version
    );
    ensure!(
        reopened.memory_job_status(&f.lid, s(&current, "job_id"))?["prompt_version"] == new_version
    );
    Ok(())
}

#[test]
fn expired_and_restart_leases_recover_only_through_explicit_receipts() -> Result<()> {
    let f = Fixture::new("observation", json!({"text":"Measured a larger gap."}))?;
    let limits = MemoryJobLimits {
        lease_seconds: 5,
        max_attempts: 3,
        ..Default::default()
    };
    let job = f.enqueue("lease-v1", &limits)?;
    let old = f.claim(&limits, time())?;
    let mut foreign = old["receipt"].clone();
    foreign["lease_owner"] = json!("other-worker");
    ensure!(f.store.preempt_memory_job(&foreign, time() + 1).is_err());
    ensure!(
        f.store
            .stage_memory_candidates(&old["receipt"], &[], time() + 5000)
            .is_err()
    );
    ensure!(f.store.reap_memory_jobs(time() + 5000)? == 1);
    let next = f.claim(&limits, time() + 5001)?;
    let path = f.dir.path().join("jobs.sqlite3");
    let reopened = Store::open(&path, "real")?;
    ensure!(
        reopened.memory_job_status(&f.lid, s(&job, "job_id"))?["status"] == "leased",
        "opening another connection must not steal the lease"
    );
    ensure!(reopened.recover_memory_jobs(time() + 5002)? == 1);
    ensure!(
        reopened
            .stage_memory_candidates(&next["receipt"], &[], time() + 5003)
            .is_err()
    );
    let final_claim = reopened
        .claim_memory_job("restarted-worker", &limits, time() + 5003)?
        .unwrap();
    reopened.stage_memory_candidates(&final_claim["receipt"], &[], time() + 5004)?;
    ensure!(reopened.memory_job_status(&f.lid, s(&job, "job_id"))?["status"] == "completed");
    Ok(())
}

#[test]
fn budgets_use_shanghai_days_count_reservations_and_reject_clock_rollback() -> Result<()> {
    let f = Fixture::new("observation", json!({"text":"A synthetic observation."}))?;
    let limits = MemoryJobLimits {
        daily_reservations: 1,
        session_reservations: 2,
        ..Default::default()
    };
    let job = f.enqueue("budget-v1", &limits)?;
    let first = f.claim(&limits, time())?;
    f.store.preempt_memory_job(&first["receipt"], time() + 1)?;
    ensure!(
        f.store
            .claim_memory_job("worker", &limits, time() + 2)?
            .is_none()
    );
    ensure!(
        f.store
            .claim_memory_job("worker", &limits, time() - 1)
            .is_err()
    );
    let next_day = f
        .store
        .claim_memory_job("worker", &limits, time() + 120_000)?
        .unwrap();
    f.store
        .preempt_memory_job(&next_day["receipt"], time() + 120_001)?;
    ensure!(
        f.store
            .claim_memory_job("worker", &limits, time() + 86_400_000)?
            .is_none(),
        "session budget survives a new day"
    );
    let status = f.store.memory_job_status(&f.lid, s(&job, "job_id"))?;
    ensure!(status["attempt_receipts"][0]["day_key"] == "2026-11-04");
    ensure!(status["attempt_receipts"][1]["day_key"] == "2026-11-05");
    ensure!(
        status["attempt_receipts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["started_at_ms"].is_null())
    );
    Ok(())
}

#[test]
fn finished_session_can_stage_shadow_then_apply_only_an_enabled_memory_kind() -> Result<()> {
    let f = Fixture::new("observation", json!({"text":"The green light turned on."}))?;
    f.store.finish_session(&f.sid)?;
    let limits = MemoryJobLimits::default();
    let job = f.enqueue("finished-v1", &limits)?;
    let claim = f.claim(&limits, time())?;
    f.store
        .mark_memory_job_started(&claim["receipt"], time() + 1)?;
    let mut proposal = observation(&f);
    proposal["source_grade"] = json!("hardware_verified");
    let candidates = f
        .store
        .stage_memory_candidates(&claim["receipt"], &[proposal], time() + 2)?;
    let cid = s(&candidates[0], "candidate_id");
    ensure!(candidates[0]["payload"]["source_grade"] == "learner_report");
    ensure!(f.store.memories(&f.lid)?.is_empty());
    ensure!(
        f.store
            .apply_memory_candidate(&f.lid, cid, &[], time() + 3)
            .is_err()
    );
    let result = f
        .store
        .apply_memory_candidate(&f.lid, cid, &["trial_observation"], time() + 3)?;
    let repeated =
        f.store
            .apply_memory_candidate(&f.lid, cid, &["trial_observation"], time() + 4)?;
    ensure!(result["memory"]["memory_id"] == repeated["memory"]["memory_id"]);
    ensure!(result["education_metadata"]["claim_type"] == "reported_observation");
    ensure!(result["memory"]["source_type"] == "learner_report");
    ensure!(f.store.memories(&f.lid)?.len() == 1);
    ensure!(f.store.memory_job_status(&f.lid, s(&job, "job_id"))?["status"] == "applied");
    Ok(())
}

#[test]
fn source_forgetting_purges_candidate_payload_and_stale_worker_cannot_commit_after_reopen()
-> Result<()> {
    let f = Fixture::new(
        "observation",
        json!({"text":"SYNTHETIC_SECRET_GAP measured today."}),
    )?;
    let limits = MemoryJobLimits::default();
    let job = f.enqueue("delete-v1", &limits)?;
    let claim = f.claim(&limits, time())?;
    let proposals =
        f.store
            .stage_memory_candidates(&claim["receipt"], &[observation(&f)], time() + 1)?;
    let cid = s(&proposals[0], "candidate_id");
    f.store.forget(&f.lid, None)?;
    let status = f.store.memory_job_status(&f.lid, s(&job, "job_id"))?;
    ensure!(status["status"] == "invalidated" && status["source_snapshot"] == json!([]));
    ensure!(status["candidates"][0]["payload"].is_null());
    ensure!(!status.to_string().contains("SYNTHETIC_SECRET_GAP"));
    ensure!(
        f.store
            .apply_memory_candidate(&f.lid, cid, &["trial_observation"], time() + 2)
            .is_err()
    );
    let reopened = Store::open(f.dir.path().join("jobs.sqlite3"), "real")?;
    ensure!(
        reopened.memory_job_status(&f.lid, s(&job, "job_id"))?["candidates"][0]["payload"]
            .is_null()
    );
    ensure!(
        reopened
            .claim_memory_job("worker", &limits, time() + 3)?
            .is_none()
    );
    Ok(())
}

#[test]
fn foreground_revision_invalidates_old_snapshot_and_requires_new_enqueue() -> Result<()> {
    let f = Fixture::new("observation", json!({"text":"The green light turned on."}))?;
    let limits = MemoryJobLimits::default();
    let job = f.enqueue("foreground-v1", &limits)?;
    let claim = f.claim(&limits, time())?;
    f.store.checkpoint(&f.sid, &empty_state())?;
    ensure!(f.store.memory_job_status(&f.lid, s(&job, "job_id"))?["status"] == "invalidated");
    ensure!(
        f.store
            .preempt_memory_job(&claim["receipt"], time() + 1)
            .is_err()
    );
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[observation(&f)], time() + 1)
            .is_err()
    );
    let replacement = f.enqueue("foreground-v1", &limits)?;
    ensure!(replacement["job_id"] != job["job_id"]);
    let other = f.store.create_learner("other", "zh", "guided")?;
    ensure!(
        f.store
            .memory_job_status(s(&other, "learner_id"), s(&job, "job_id"))
            .is_err()
    );
    Ok(())
}

#[test]
fn candidate_validation_keeps_versions_quotes_support_and_credibility_separate() -> Result<()> {
    let f = Fixture::new("observation", json!({"text":"The green light turned on."}))?;
    let limits = MemoryJobLimits::default();
    f.enqueue("validation-v1", &limits)?;
    let claim = f.claim(&limits, time())?;
    let mut wrong = observation(&f);
    wrong["source_event_versions"][s(&f.event, "event_id")] = json!(99);
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[wrong], time() + 1)
            .is_err()
    );
    let mut wrong = observation(&f);
    wrong["source_quotes"][0]["quote"] = json!("The learner mastered all concepts independently.");
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[wrong], time() + 1)
            .is_err()
    );
    let mut wrong = observation(&f);
    wrong["support_level"] = json!("independent");
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[wrong], time() + 1)
            .is_err()
    );
    let mut wrong = observation(&f);
    wrong["source_event_ids"] = json!([f.event["event_id"], f.event["event_id"]]);
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[wrong], time() + 1)
            .is_err()
    );
    f.store.db.execute(
        "UPDATE education_event_meta SET kind='model_summary' WHERE event_id=?",
        [s(&f.event, "event_id")],
    )?;
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[observation(&f)], time() + 1)
            .is_err()
    );
    f.store.db.execute(
        "UPDATE education_event_meta SET kind='observation' WHERE event_id=?",
        [s(&f.event, "event_id")],
    )?;
    let staged =
        f.store
            .stage_memory_candidates(&claim["receipt"], &[observation(&f)], time() + 1)?;
    ensure!(
        staged[0]["payload"]["source_count"] == 1
            && staged[0]["payload"]["source_grade"] == "learner_report"
    );
    Ok(())
}

#[test]
fn plans_and_temporary_preferences_never_become_completed_or_persistent_facts() -> Result<()> {
    let plan = Fixture::new(
        "plan",
        json!({"text":"Next time we will test the wall distance.","accepted":true}),
    )?;
    let limits = MemoryJobLimits::default();
    plan.enqueue("plan-v1", &limits)?;
    let claim = plan.claim(&limits, time())?;
    ensure!(
        plan.store
            .stage_memory_candidates(&claim["receipt"], &[observation(&plan)], time() + 1)
            .is_err()
    );
    let proposal = candidate(
        &plan.event,
        "accepted_pending_experiment",
        "accepted_plan",
        "Test the wall distance next time.",
    );
    let staged = plan
        .store
        .stage_memory_candidates(&claim["receipt"], &[proposal], time() + 1)?;
    let applied = plan.store.apply_memory_candidate(
        &plan.lid,
        s(&staged[0], "candidate_id"),
        &["accepted_pending_experiment"],
        time() + 2,
    )?;
    ensure!(applied["memory"]["kind"] == "pending_experiment");
    ensure!(plan.store.session(&plan.sid)?["state"]["reported_result"] == "");
    let pref = Fixture::new(
        "learner_statement",
        json!({"text":"Only today, please give shorter hints.","explicit_preference":true,"preference_scope":"temporary"}),
    )?;
    pref.enqueue("preference-v1", &limits)?;
    let claim = pref.claim(&limits, time())?;
    let mut proposal = candidate(
        &pref.event,
        "help_preference",
        "explicit_preference",
        "Short hints for this session.",
    );
    proposal["scope"] = json!("current_session");
    let staged = pref
        .store
        .stage_memory_candidates(&claim["receipt"], &[proposal], time() + 1)?;
    ensure!(
        pref.store
            .apply_memory_candidate(
                &pref.lid,
                s(&staged[0], "candidate_id"),
                &["help_preference"],
                time() + 2
            )
            .is_err()
    );
    ensure!(pref.store.memories(&pref.lid)?.is_empty());
    Ok(())
}

#[test]
fn one_persistent_source_cannot_authorize_other_temporary_or_wrong_kind_sources() -> Result<()> {
    let limits = MemoryJobLimits::default();
    for (kind, content, allowed) in [
        (
            "learner_statement",
            json!({"text":"Only today, skip the explanation.","explicit_preference":true,"preference_scope":"temporary"}),
            false,
        ),
        (
            "plan",
            json!({"text":"Next time try the longer bridge.","accepted":false,"explicit_preference":true,"preference_scope":"persistent"}),
            false,
        ),
        (
            "learner_answer",
            json!({"text":"I answered five sixths.","explicit_preference":true,"preference_scope":"persistent"}),
            false,
        ),
        (
            "learner_statement",
            json!({"text":"Could you explain this word?"}),
            false,
        ),
        (
            "learner_statement",
            json!({"text":"For future science activities, give the English term before its Chinese explanation.","explicit_preference":true,"preference_scope":"persistent"}),
            true,
        ),
        (
            "teacher_setting",
            json!({"text":"Use the student's explicitly requested term-first order in future science activities.","explicit_preference":true,"preference_scope":"persistent"}),
            true,
        ),
    ] {
        let f = Fixture::new(
            "learner_statement",
            json!({"text":"Please remember my term-first science vocabulary preference for future activities.","explicit_preference":true,"preference_scope":"persistent"}),
        )?;
        let extra = f.store.record_education_event(
            &f.lid,
            &f.sid,
            if kind == "teacher_setting" {
                "operator_confirmed"
            } else {
                "learner_report"
            },
            kind,
            &content,
        )?;
        f.store.enqueue_memory_job(
            &f.lid,
            &f.sid,
            &[
                s(&f.event, "event_id").to_owned(),
                s(&extra, "event_id").to_owned(),
            ],
            "mixed-preference-v1",
            &limits,
            time(),
        )?;
        let claim = f.claim(&limits, time())?;
        let mut proposal = candidate(
            &f.event,
            "help_preference",
            "explicit_preference",
            &format!(
                "{} {}",
                s(&f.event["content"], "text"),
                s(&extra["content"], "text")
            ),
        );
        proposal["scope"] = json!("cross_session_explicit");
        proposal["source_event_ids"]
            .as_array_mut()
            .unwrap()
            .push(extra["event_id"].clone());
        proposal["source_event_versions"][s(&extra, "event_id")] = extra["event_version"].clone();
        proposal["source_quotes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"event_id":extra["event_id"],"quote":content["text"]}));
        if !allowed {
            ensure!(
                f.store.add_memory(&f.lid, &proposal, 0)?.is_none(),
                "legacy entry must not bypass typed preference scope"
            );
        }
        let staged = f
            .store
            .stage_memory_candidates(&claim["receipt"], &[proposal], time() + 1)?;
        let result = f.store.apply_memory_candidate(
            &f.lid,
            s(&staged[0], "candidate_id"),
            &["help_preference"],
            time() + 2,
        );
        ensure!(
            result.is_ok() == allowed,
            "mixed source authorization mismatch for {kind}"
        );
        ensure!(f.store.memories(&f.lid)?.len() == usize::from(allowed));
        if allowed {
            let evidence = result?;
            ensure!(evidence["sources"].as_array().unwrap().len() == 2);
            ensure!(evidence["memory"]["source_type"] == "learner_report");
        }
    }
    Ok(())
}

#[test]
fn legacy_untyped_preference_requires_each_source_without_changing_confirmed_settings() -> Result<()>
{
    let f = Fixture::new(
        "learner_statement",
        json!({"text":"synthetic unused source"}),
    )?;
    let first = f.store.event(
        &f.lid,
        &f.sid,
        "learner_report",
        &json!({"text":"From now on, give the English term first."}),
    )?;
    let temporary = f.store.event(
        &f.lid,
        &f.sid,
        "learner_report",
        &json!({"text":"Only today, give no explanation.","preference_scope":"temporary"}),
    )?;
    let mut value = json!({"kind":"help_preference","scope":"cross_session_explicit","content":"English term first, without explanations.","source_event_ids":[first,temporary]});
    ensure!(f.store.add_memory(&f.lid, &value, 0)?.is_none());
    let setting = f.store.event(
        &f.lid,
        &f.sid,
        "operator_confirmed",
        &json!({"text":"English term before its Chinese explanation.","set_preference":true}),
    )?;
    value["content"] = json!("English term before its Chinese explanation.");
    value["source_event_ids"] = json!([first, setting]);
    let id = f.store.add_memory(&f.lid, &value, 0)?.unwrap();
    ensure!(
        f.store.memory_evidence(&f.lid, &id)?["sources"]
            .as_array()
            .unwrap()
            .len()
            == 2
    );
    Ok(())
}

#[test]
fn correction_purges_applied_candidate_derivations_and_new_version_can_be_queued() -> Result<()> {
    let f = Fixture::new("observation", json!({"text":"The green light turned on."}))?;
    let limits = MemoryJobLimits::default();
    let old = f.enqueue("correct-v1", &limits)?;
    let claim = f.claim(&limits, time())?;
    let staged =
        f.store
            .stage_memory_candidates(&claim["receipt"], &[observation(&f)], time() + 1)?;
    let applied = f.store.apply_memory_candidate(
        &f.lid,
        s(&staged[0], "candidate_id"),
        &["trial_observation"],
        time() + 2,
    )?;
    let correction = f.store.revise_education_event(
        &f.lid,
        &f.sid,
        s(&f.event, "event_id"),
        "operator_confirmed",
        &json!({"correction":"The light was not on; the earlier report was incorrect."}),
    )?;
    ensure!(
        f.store
            .memory_evidence(&f.lid, s(&applied["memory"], "memory_id"))
            .is_err()
    );
    ensure!(
        f.store.memory_job_status(&f.lid, s(&old, "job_id"))?["candidates"][0]["payload"].is_null()
    );
    f.store.enqueue_memory_job(
        &f.lid,
        &f.sid,
        &[s(&correction, "event_id").to_owned()],
        "correct-v1",
        &limits,
        time() + 3,
    )?;
    let claim = f.claim(&limits, time() + 3)?;
    let proposal = candidate(
        &correction,
        "correction",
        "explicit_correction",
        "Earlier report corrected: the light was not on.",
    );
    let staged = f
        .store
        .stage_memory_candidates(&claim["receipt"], &[proposal], time() + 4)?;
    let result = f.store.apply_memory_candidate(
        &f.lid,
        s(&staged[0], "candidate_id"),
        &["correction"],
        time() + 5,
    )?;
    ensure!(result["memory"]["kind"] == "correction" && result["sources"][0]["event_version"] == 2);
    Ok(())
}

#[test]
fn concept_evidence_requires_a_learner_response_and_explicit_support_level() -> Result<()> {
    let f = Fixture::new(
        "learner_answer",
        json!({"text":"The distance module measures how far the wall is.","support_level":"hinted"}),
    )?;
    let limits = MemoryJobLimits::default();
    f.enqueue("concept-v1", &limits)?;
    let claim = f.claim(&limits, time())?;
    let mut proposal = candidate(
        &f.event,
        "concept_evidence",
        "task_specific_response",
        "Explained the distance module in this task with a hint.",
    );
    proposal["support_level"] = json!("hinted");
    let staged = f
        .store
        .stage_memory_candidates(&claim["receipt"], &[proposal], time() + 1)?;
    let result = f.store.apply_memory_candidate(
        &f.lid,
        s(&staged[0], "candidate_id"),
        &["concept_evidence"],
        time() + 2,
    )?;
    ensure!(result["education_metadata"]["support_level"] == "hinted");
    ensure!(s(&result["memory"], "content").contains("不代表已掌握"));
    Ok(())
}

#[test]
fn queue_rejects_cross_session_and_cross_learner_sources() -> Result<()> {
    let f = Fixture::new(
        "observation",
        json!({"text":"A source in the original session."}),
    )?;
    let limits = MemoryJobLimits::default();
    let another = f.store.create_session(&f.lid)?;
    let ids = [s(&f.event, "event_id").to_owned()];
    ensure!(
        f.store
            .enqueue_memory_job(
                &f.lid,
                s(&another, "session_id"),
                &ids,
                "isolation-v1",
                &limits,
                time()
            )
            .is_err()
    );
    let other = f.store.create_learner("other", "en", "guided")?;
    let other_session = f.store.create_session(s(&other, "learner_id"))?;
    ensure!(
        f.store
            .enqueue_memory_job(
                s(&other, "learner_id"),
                s(&other_session, "session_id"),
                &ids,
                "isolation-v1",
                &limits,
                time()
            )
            .is_err()
    );
    ensure!(Store::open(f.dir.path().join("jobs.sqlite3"), "simulator").is_err());
    Ok(())
}

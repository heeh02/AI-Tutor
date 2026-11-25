//! Synthetic SQLite only: task limits are sourced context, never ability or completion.
use ai_tutor::{
    context::build_context,
    kimi::memory_contract,
    memory_embedding::EmbeddingProfile,
    store::{MemoryEmbeddingSnapshot, MemoryJobLimits, QueryEmbedding, Store, s},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

const KIND: &str = "context_constraint";
const TEXT: &str = "Both programs added two three times, but their initial values were zero and one. Different final values do not establish a faulty loop.";

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

struct Fixture {
    store: Store,
    dir: tempfile::TempDir,
    lid: String,
    sid: String,
    event: Value,
}
impl Fixture {
    fn new(namespace: &str, event_kind: &str, source: &str, content: Value) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("constraints.sqlite3"), namespace)?;
        let learner = store.create_learner("synthetic", "en", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        let sid = s(&session, "session_id").to_owned();
        let event = store.record_education_event(&lid, &sid, source, event_kind, &content)?;
        Ok(Self {
            store,
            dir,
            lid,
            sid,
            event,
        })
    }

    fn statement() -> Result<Self> {
        Self::new(
            "real",
            "learner_statement",
            "learner_report",
            json!({"text": TEXT}),
        )
    }

    fn claim(&self, events: &[Value]) -> Result<Value> {
        claim(&self.store, &self.lid, &self.sid, events)
    }
}

fn claim(store: &Store, lid: &str, sid: &str, events: &[Value]) -> Result<Value> {
    let limits = MemoryJobLimits::default();
    store.enqueue_memory_job(
        lid,
        sid,
        &events
            .iter()
            .map(|e| s(e, "event_id").to_owned())
            .collect::<Vec<_>>(),
        memory_contract::PROMPT_VERSION,
        &limits,
        now_ms(),
    )?;
    Ok(store
        .claim_memory_job("constraint-test", &limits, now_ms())?
        .unwrap()["receipt"]
        .clone())
}

fn candidate(events: &[Value]) -> Value {
    let mut versions = json!({});
    let mut quotes = vec![];
    for event in events {
        let id = s(event, "event_id");
        versions[id] = event["event_version"].clone();
        quotes.push(json!({"event_id":id,"quote":event["content"]["text"]}));
    }
    json!({"kind":KIND,"claim_type":"condition_limit","content":events[0]["content"]["text"],
        "scope":"current_activity","support_level":"unknown",
        "source_event_ids":events.iter().map(|e| s(e,"event_id")).collect::<Vec<_>>(),
        "source_event_versions":versions,"source_quotes":quotes,
        "importance":{"goal_relevance":3,"continuity":3,"novel_change":3,"explicit_request":3},
        "source_grade":"hardware_verified","source_count":999})
}

fn promote(store: &Store, lid: &str, sid: &str, event: &Value) -> Result<Value> {
    let events = [event.clone()];
    let receipt = claim(store, lid, sid, &events)?;
    let staged = store.stage_memory_candidates(&receipt, &[candidate(&events)], now_ms())?;
    store.apply_memory_candidate(lid, s(&staged[0], "candidate_id"), &[KIND], now_ms())
}

// This valid unit vector exercises cache ownership only; no model quality claim.
fn cache_one(store: &Store, lid: &str) -> Result<(MemoryEmbeddingSnapshot, String, Vec<f32>)> {
    let profile = EmbeddingProfile::E5NativeFp32;
    let key = profile.model_key();
    let work = store.embedding_work(lid, &key, 32)?;
    ensure!(work.len() == 1);
    let mut vector = vec![0.0; profile.dimension()];
    vector[0] = 1.0;
    ensure!(store.commit_memory_embedding(&work[0], &key, &vector)?);
    Ok((work[0].clone(), key, vector))
}

#[test]
fn constraint_is_explicitly_enabled_sourced_history_without_state_or_grade_upgrade() -> Result<()> {
    for (namespace, source, text) in [
        ("real", "learner_report", TEXT),
        (
            "real",
            "operator_confirmed",
            "The wet coat is a clue, but this excerpt does not establish why it became wet.",
        ),
        (
            "simulator",
            "simulated",
            "两盘水量不同且温度未测，不能据此断言差异只由遮光造成。",
        ),
    ] {
        let f = Fixture::new(namespace, "learner_statement", source, json!({"text":text}))?;
        let state_before = f.store.session(&f.sid)?["state"].clone();
        let events = [f.event.clone()];
        let receipt = f.claim(&events)?;
        let staged = f
            .store
            .stage_memory_candidates(&receipt, &[candidate(&events)], now_ms())?;
        ensure!(staged[0]["payload"]["source_grade"] == source);
        ensure!(staged[0]["payload"]["source_count"] == 1);
        ensure!(f.store.memories(&f.lid)?.is_empty());
        let cid = s(&staged[0], "candidate_id");
        ensure!(
            f.store
                .apply_memory_candidate(&f.lid, cid, &[], now_ms())
                .is_err()
        );
        let result = f
            .store
            .apply_memory_candidate(&f.lid, cid, &[KIND], now_ms())?;
        ensure!(result["memory"]["kind"] == KIND);
        ensure!(result["memory"]["scope"] == "current_activity");
        ensure!(result["memory"]["source_type"] == source);
        ensure!(result["memory"]["content"] == format!("特定任务的条件与推断边界：{text}"));
        ensure!(result["education_metadata"]["claim_type"] == "condition_limit");
        ensure!(result["education_metadata"]["support_level"] == "unknown");
        ensure!(result["sources"][0]["event_kind"] == "learner_statement");
        ensure!(result["sources"][0]["event_version"] == 1);
        ensure!(f.store.session(&f.sid)?["state"] == state_before);
        let context = build_context(
            &f.store,
            &f.store.session(&f.sid)?,
            &f.store.learner(&f.lid)?,
            text,
            s(&f.event, "event_id"),
            &json!({}),
        )?;
        let memories = context["slow_memory"].as_array().unwrap();
        ensure!(memories.len() == 1 && memories[0]["kind"] == KIND);
        ensure!(memories[0]["historical"] == true);
        ensure!(memories[0]["education_metadata"]["source_grade"] == source);
        ensure!(context["task"]["reported_result"] == "");
        ensure!(context["task"]["attempts"] == json!([]));
        ensure!(
            f.store
                .apply_memory_candidate(&f.lid, cid, &[KIND], now_ms())?["memory"]["memory_id"]
                == result["memory"]["memory_id"]
        );
    }
    Ok(())
}

#[test]
fn constraint_rejects_other_event_types_and_mixed_plan_evidence_at_staging() -> Result<()> {
    for event_kind in [
        "plan",
        "observation",
        "learner_answer",
        "teacher_setting",
        "correction",
    ] {
        let source = if event_kind == "teacher_setting" {
            "operator_confirmed"
        } else {
            "learner_report"
        };
        let f = Fixture::new(
            "real",
            event_kind,
            source,
            json!({"text":TEXT,"accepted":true}),
        )?;
        let events = [f.event.clone()];
        let receipt = f.claim(&events)?;
        ensure!(
            f.store
                .stage_memory_candidates(&receipt, &[candidate(&events)], now_ms())
                .is_err()
        );
        ensure!(f.store.memories(&f.lid)?.is_empty());
    }
    let f = Fixture::statement()?;
    let plan = f.store.record_education_event(
        &f.lid,
        &f.sid,
        "learner_report",
        "plan",
        &json!({"text":"Next time we will use equal initial values.","accepted":false}),
    )?;
    let events = [f.event.clone(), plan];
    let receipt = f.claim(&events)?;
    ensure!(
        f.store
            .stage_memory_candidates(&receipt, &[candidate(&events)], now_ms())
            .is_err()
    );
    Ok(())
}

#[test]
fn constraint_scope_support_claim_quote_and_version_cannot_be_relaxed() -> Result<()> {
    let f = Fixture::new(
        "real",
        "learner_statement",
        "learner_report",
        json!({"text":TEXT,"support_level":"hinted"}),
    )?;
    let events = [f.event.clone()];
    let receipt = f.claim(&events)?;
    for (field, value) in [
        ("scope", "current_session"),
        ("scope", "cross_session_explicit"),
        ("support_level", "hinted"),
        ("support_level", "independent"),
        ("claim_type", "task_specific_response"),
    ] {
        let mut wrong = candidate(&events);
        wrong[field] = json!(value);
        ensure!(
            f.store
                .stage_memory_candidates(&receipt, &[wrong], now_ms())
                .is_err()
        );
    }
    let mut wrong = candidate(&events);
    wrong["source_event_versions"][s(&f.event, "event_id")] = json!(2);
    ensure!(
        f.store
            .stage_memory_candidates(&receipt, &[wrong], now_ms())
            .is_err()
    );
    let mut wrong = candidate(&events);
    wrong["source_quotes"][0]["quote"] = json!("The learner has mastered every loop.");
    ensure!(
        f.store
            .stage_memory_candidates(&receipt, &[wrong], now_ms())
            .is_err()
    );
    // Recording a hint does not make this type a record of hinted performance.
    ensure!(
        f.store
            .stage_memory_candidates(&receipt, &[candidate(&events)], now_ms())?
            .len()
            == 1
    );
    Ok(())
}

#[test]
fn constraint_source_revision_revokes_cache_and_stale_apply_after_reopen() -> Result<()> {
    let f = Fixture::statement()?;
    let events = [f.event.clone()];
    let receipt = f.claim(&events)?;
    let staged = f
        .store
        .stage_memory_candidates(&receipt, &[candidate(&events)], now_ms())?;
    let cid = s(&staged[0], "candidate_id");
    let memory = f
        .store
        .apply_memory_candidate(&f.lid, cid, &[KIND], now_ms())?;
    let (snapshot, key, vector) = cache_one(&f.store, &f.lid)?;
    f.store.revise_education_event(&f.lid, &f.sid, s(&f.event, "event_id"), "learner_report", &json!({"correction":"The initial values were both zero; the earlier statement was incorrect."}))?;
    ensure!(
        f.store
            .memory_evidence(&f.lid, s(&memory["memory"], "memory_id"))
            .is_err()
    );
    ensure!(!f.store.commit_memory_embedding(&snapshot, &key, &vector)?);
    let reopened = Store::open(f.dir.path().join("constraints.sqlite3"), "real")?;
    ensure!(
        reopened
            .apply_memory_candidate(&f.lid, cid, &[KIND], now_ms())
            .is_err()
    );
    ensure!(
        reopened
            .eligible_memory_embeddings(&f.lid, &key)?
            .is_empty()
    );
    ensure!(
        reopened
            .recall_candidates(
                &f.lid,
                TEXT,
                Some(QueryEmbedding {
                    model_key: &key,
                    values: &vector
                }),
                5
            )?
            .is_empty()
    );
    Ok(())
}

#[test]
fn constraint_forgetting_one_nonempty_owner_preserves_another_identical_owner() -> Result<()> {
    let f = Fixture::statement()?;
    let first = promote(&f.store, &f.lid, &f.sid, &f.event)?;
    let (first_snapshot, key, vector) = cache_one(&f.store, &f.lid)?;
    let other = f.store.create_learner("other synthetic", "en", "guided")?;
    let other_id = s(&other, "learner_id");
    let other_session = f.store.create_session(other_id)?;
    let other_sid = s(&other_session, "session_id");
    let other_event = f.store.record_education_event(
        other_id,
        other_sid,
        "learner_report",
        "learner_statement",
        &json!({"text":TEXT}),
    )?;
    let second = promote(&f.store, other_id, other_sid, &other_event)?;
    let (second_snapshot, _, _) = cache_one(&f.store, other_id)?;
    ensure!(first["memory"]["content"] == second["memory"]["content"]);
    ensure!(first["memory"]["memory_id"] != second["memory"]["memory_id"]);
    for owner in [&f.lid[..], other_id] {
        let rows = f.store.recall_candidates(
            owner,
            TEXT,
            Some(QueryEmbedding {
                model_key: &key,
                values: &vector,
            }),
            5,
        )?;
        ensure!(rows.len() == 1 && rows[0].memory["learner_id"] == owner);
    }
    let mut forged = first_snapshot.clone();
    forged.learner_id = other_id.to_owned();
    ensure!(!f.store.commit_memory_embedding(&forged, &key, &vector)?);
    f.store
        .forget(&f.lid, Some(s(&first["memory"], "memory_id")))?;
    let reopened = Store::open(f.dir.path().join("constraints.sqlite3"), "real")?;
    ensure!(!reopened.commit_memory_embedding(&first_snapshot, &key, &vector)?);
    ensure!(
        reopened
            .memory_evidence(&f.lid, &first_snapshot.memory_id)
            .is_err()
    );
    ensure!(
        reopened
            .eligible_memory_embeddings(&f.lid, &key)?
            .is_empty()
    );
    ensure!(reopened.eligible_memory_embeddings(other_id, &key)?.len() == 1);
    ensure!(
        reopened.memory_evidence(other_id, &second_snapshot.memory_id)?["memory"]["status"]
            == "active"
    );
    Ok(())
}

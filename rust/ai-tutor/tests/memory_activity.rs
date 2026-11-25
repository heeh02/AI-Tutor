//! Real temporary Store provenance and synthetic vectors; no model or device.
use ai_tutor::{
    context::build_context,
    kimi::memory_contract,
    memory_embedding::EmbeddingProfile,
    store::{MemoryJobLimits, QueryEmbedding, Store, s, uid},
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

fn origin(store: &Store) -> &'static str {
    if store.namespace == "simulator" {
        "simulated"
    } else {
        "learner_report"
    }
}
fn remember(store: &Store, lid: &str, sid: &str, text: &str) -> Result<(String, String)> {
    let event = store.record_education_event(
        lid,
        sid,
        origin(store),
        "observation",
        &json!({"text":text}),
    )?;
    let id = s(&event, "event_id").to_owned();
    let mid = store.add_memory(lid, &json!({"kind":"reported_progress","scope":"current_activity","content":text,"source_event_ids":[id]}), store.learner(lid)?["epoch"].as_i64().unwrap())?.unwrap();
    Ok((id, mid))
}
fn ids(rows: &[Value]) -> Vec<String> {
    rows.iter().map(|v| s(v, "memory_id").to_owned()).collect()
}
// An old installed writer can create rows without the new side table. This
// intentionally bypasses new admission checks to exercise migration, not UI.
fn legacy_memory(
    store: &Store,
    lid: &str,
    events: &[String],
    kind: &str,
    scope: &str,
) -> Result<String> {
    let mid = uid("legacy_memory");
    store.db.execute(
        "INSERT INTO memories VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            mid,
            lid,
            "modular_rover",
            kind,
            "synthetic legacy record",
            json!(events).to_string(),
            origin(store),
            scope,
            "2000-01-01",
            "2000-01-01",
            "active",
            "[]",
            0
        ],
    )?;
    Ok(mid)
}

#[test]
fn lexical_and_dense_filter_activity_before_top_five_and_context_keeps_identity() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("scope.db"), "real")?;
    let learner = store.create_learner("A", "zh", "guided")?;
    let lid = s(&learner, "learner_id");
    let old = store.create_session(lid)?;
    let query = "电流电压电阻电池";
    for i in 0..6 {
        remember(
            &store,
            lid,
            s(&old, "session_id"),
            &format!("{query}旧试验{i}"),
        )?;
    }
    let current = store.create_session(lid)?;
    let sid = s(&current, "session_id");
    let (event, wanted) = remember(&store, lid, sid, "电流")?;
    ensure!(
        store
            .recall(lid, query, 5)?
            .iter()
            .all(|m| m["memory_id"] != wanted)
    );
    ensure!(ids(&store.recall_for_session(sid, query, 5)?) == vec![wanted.clone()]);
    let profile = EmbeddingProfile::E5NativeFp32;
    let key = profile.model_key();
    let mut question_vector = vec![0.0; profile.dimension()];
    question_vector[0] = 1.0;
    for work in store.embedding_work(lid, &key, 32)? {
        let mut vector = question_vector.clone();
        if work.memory_id == wanted {
            vector[0] = 0.0;
            vector[1] = 1.0;
        }
        ensure!(store.commit_memory_embedding(&work, &key, &vector)?);
    }
    let candidates = store.recall_candidates_for_session(
        sid,
        query,
        Some(QueryEmbedding {
            model_key: &key,
            values: &question_vector,
        }),
        5,
    )?;
    ensure!(candidates.len() == 1 && candidates[0].memory["memory_id"] == wanted);
    ensure!(candidates[0].semantic_rank == Some(1) && candidates[0].cosine == Some(0.0));
    let question = store.event(lid, sid, "learner_report", &json!({"text":query}))?;
    let context = build_context(
        &store,
        &store.session(sid)?,
        &learner,
        query,
        &question,
        &json!({}),
    )?;
    ensure!(context["activity_id"] == current["activity_id"]);
    ensure!(context["slow_memory"][0]["activity_binding"]["activity_id"] == current["activity_id"]);
    for row in context["evidence"].as_array().unwrap() {
        ensure!(row["session_id"] == sid && row["activity_id"] == current["activity_id"]);
    }
    ensure!(store.source_events(lid, &[event])?[0]["activity_id"] == current["activity_id"]);
    let other = store.create_learner("B", "zh", "guided")?;
    let other_session = store.create_session(s(&other, "learner_id"))?;
    let (_, theirs) = remember(
        &store,
        s(&other, "learner_id"),
        s(&other_session, "session_id"),
        query,
    )?;
    ensure!(ids(&store.recall_for_session(sid, query, 5)?) == vec![wanted]);
    ensure!(
        ids(&store.recall_for_session(s(&other_session, "session_id"), query, 5)?) == vec![theirs]
    );
    Ok(())
}

#[test]
fn persistent_preferences_cross_activities_but_mixed_temporary_sources_do_not() -> Result<()> {
    for namespace in ["real", "simulator"] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("preferences.db");
        let store = Store::open(&path, namespace)?;
        let learner = store.create_learner("A", "zh", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let first = store.create_session(&lid)?;
        let event = store.record_education_event(&lid, s(&first,"session_id"), origin(&store), "learner_statement", &json!({"text":"以后每次一次一步","explicit_preference":true,"preference_scope":"persistent"}))?;
        let temporary = store.record_education_event(
            &lid,
            s(&first, "session_id"),
            origin(&store),
            "learner_statement",
            &json!({"text":"这次用图片","explicit_preference":true,"preference_scope":"temporary"}),
        )?;
        let candidate = json!({"kind":"help_preference","scope":"cross_session_explicit","content":"一次一步","source_event_ids":[event["event_id"]]});
        let good = store.add_memory(&lid, &candidate, 0)?.unwrap();
        let mixed = legacy_memory(
            &store,
            &lid,
            &[
                s(&event, "event_id").into(),
                s(&temporary, "event_id").into(),
            ],
            "help_preference",
            "cross_session_explicit",
        )?;
        let mut rejected = candidate.clone();
        rejected["source_event_ids"] = json!([event["event_id"], temporary["event_id"]]);
        ensure!(store.add_memory(&lid, &rejected, 0)?.is_none());
        let current = store.create_session(&lid)?;
        let sid = s(&current, "session_id").to_owned();
        drop(store);
        let store = Store::open(&path, namespace)?;
        ensure!(
            ids(&store.recall_for_session(&sid, "unrelated question", 5)?) == vec![good.clone()]
        );
        ensure!(
            store.memory_evidence(&lid, &mixed).is_ok(),
            "legacy record remains inspectable"
        );
        let edited = store.correct_memory(&lid, &good, "简短分步")?;
        ensure!(
            ids(&store.recall_for_session(&sid, "unrelated question", 5)?) == vec![edited.clone()]
        );
        let evidence = store.memory_evidence(&lid, &edited)?;
        ensure!(evidence["sources"][0]["event_kind"] == "teacher_setting");
        ensure!(
            evidence["sources"][0]["source_type"]
                == if namespace == "simulator" {
                    "simulated"
                } else {
                    "operator_confirmed"
                }
        );
    }
    Ok(())
}

#[test]
fn mixed_new_sources_are_rejected_and_legacy_unresolved_is_not_automatically_recalled() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("mixed.db");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("A", "zh", "guided")?;
    let lid = s(&learner, "learner_id").to_owned();
    let a = store.create_session(&lid)?;
    let (ae, _) = remember(&store, &lid, s(&a, "session_id"), "旧实验")?;
    let b = store.create_session(&lid)?;
    let (be, _) = remember(&store, &lid, s(&b, "session_id"), "新实验")?;
    let source_ids = vec![ae, be];
    let candidate = json!({"kind":"reported_progress","scope":"current_activity","content":"mixed","source_event_ids":source_ids});
    ensure!(store.add_memory(&lid, &candidate, 0)?.is_none());
    let old = legacy_memory(
        &store,
        &lid,
        &source_ids,
        "reported_progress",
        "current_activity",
    )?;
    drop(store);
    let store = Store::open(&path, "real")?;
    ensure!(store.memory_evidence(&lid, &old)?["activity_binding"]["status"] == "unresolved");
    for sid in [s(&a, "session_id"), s(&b, "session_id")] {
        ensure!(
            store
                .recall_for_session(sid, "synthetic legacy", 5)?
                .iter()
                .all(|row| row["memory_id"] != old)
        );
    }
    ensure!(
        store
            .recall(&lid, "synthetic legacy", 5)?
            .iter()
            .any(|row| row["memory_id"] == old)
    );
    Ok(())
}

#[test]
fn manual_correction_preserves_exact_target_despite_mixed_source_closure_and_migration()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("correction.db");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("A", "zh", "guided")?;
    let lid = s(&learner, "learner_id").to_owned();
    let a = store.create_session(&lid)?;
    let (ae, original) = remember(&store, &lid, s(&a, "session_id"), "电流旧记录")?;
    let b = store.create_session(&lid)?;
    let (be, _) = remember(&store, &lid, s(&b, "session_id"), "电流新记录")?;
    legacy_memory(
        &store,
        &lid,
        &[ae.clone(), be],
        "reported_progress",
        "current_activity",
    )?;
    drop(store);
    let store = Store::open(&path, "real")?;
    let corrected = store.correct_memory(&lid, &original, "电流更正记录")?;
    ensure!(store.source_events(&lid, &[ae.clone()])?.is_empty());
    ensure!(
        store.memory_evidence(&lid, &corrected)?["activity_binding"]["activity_id"]
            == a["activity_id"]
    );
    ensure!(
        store
            .recall_for_session(s(&b, "session_id"), "电流更正", 5)?
            .iter()
            .all(|v| v["memory_id"] != corrected)
    );
    // Pre-feature migration can recover the first correction because its live
    // source still names the exact target, even though the old source is gone.
    store
        .db
        .execute("DELETE FROM memory_activity_bindings", [])?;
    drop(store);
    let store = Store::open(&path, "real")?;
    ensure!(
        store.memory_evidence(&lid, &corrected)?["activity_binding"]["activity_id"]
            == a["activity_id"]
    );
    let twice = store.correct_memory(&lid, &corrected, "电流再次更正")?;
    // Normal subsequent corrections retain their persisted identity on restart.
    drop(store);
    let store = Store::open(&path, "real")?;
    ensure!(
        store.memory_evidence(&lid, &twice)?["activity_binding"]["activity_id"] == a["activity_id"]
    );
    ensure!(
        store
            .recall_for_session(s(&a, "session_id"), "电流再次更正", 5)?
            .iter()
            .any(|v| v["memory_id"] == twice)
    );
    ensure!(
        store
            .recall_for_session(s(&b, "session_id"), "电流再次更正", 5)?
            .iter()
            .all(|v| v["memory_id"] != twice)
    );
    // In an older database without these bindings, the intermediate exact
    // corrected_memory_id has now been deleted. Its mixed invalidation closure
    // cannot establish a unique activity: retain inspectable history, not a guess.
    store
        .db
        .execute("DELETE FROM memory_activity_bindings", [])?;
    drop(store);
    let store = Store::open(&path, "real")?;
    ensure!(store.memory_evidence(&lid, &twice)?["activity_binding"]["status"] == "unresolved");
    for sid in [s(&a, "session_id"), s(&b, "session_id")] {
        ensure!(
            store
                .recall_for_session(sid, "电流再次更正", 5)?
                .iter()
                .all(|v| v["memory_id"] != twice)
        );
    }
    ensure!(
        store
            .recall(&lid, "电流再次更正", 5)?
            .iter()
            .any(|v| v["memory_id"] == twice)
    );
    ensure!(store.rows("PRAGMA table_info(memories)", [])?.len() == 13);
    Ok(())
}

fn promote_correction(store: &Store, lid: &str, sid: &str, event: &Value) -> Result<String> {
    let limits = MemoryJobLimits::default();
    let now = chrono::Utc::now().timestamp_millis();
    let id = s(event, "event_id");
    store.enqueue_memory_job(
        lid,
        sid,
        &[id.into()],
        memory_contract::PROMPT_VERSION,
        &limits,
        now,
    )?;
    let receipt = store
        .claim_memory_job("activity-test", &limits, now)?
        .unwrap()["receipt"]
        .clone();
    let mut candidate = json!({"kind":"correction","claim_type":"explicit_correction","content":event["content"]["text"],"scope":"current_activity","support_level":"unknown","source_event_ids":[id],"source_event_versions":{},"source_quotes":[{"event_id":id,"quote":event["content"]["text"]}],"importance":{"goal_relevance":0,"continuity":0,"novel_change":0,"explicit_request":0}});
    candidate["source_event_versions"][id] = event["event_version"].clone();
    let staged = store.stage_memory_candidates(&receipt, &[candidate], now)?;
    let applied =
        store.apply_memory_candidate(lid, s(&staged[0], "candidate_id"), &["correction"], now)?;
    Ok(s(&applied["memory"], "memory_id").to_owned())
}

#[test]
fn typed_revision_follows_deleted_source_identity_and_unlinked_statement_keeps_own_activity()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("typed.db"), "real")?;
    let learner = store.create_learner("A", "zh", "guided")?;
    let lid = s(&learner, "learner_id");
    let a = store.create_session(lid)?;
    let (old, _) = remember(&store, lid, s(&a, "session_id"), "原观察是三次")?;
    let b = store.create_session(lid)?;
    let sid = s(&b, "session_id");
    let revised = store.revise_education_event(
        lid,
        sid,
        &old,
        "learner_report",
        &json!({"text":"更正：那次观察是两次"}),
    )?;
    ensure!(store.source_events(lid, &[old])?.is_empty());
    let corrected = promote_correction(&store, lid, sid, &revised)?;
    ensure!(
        store.memory_evidence(lid, &corrected)?["activity_binding"]["activity_id"]
            == a["activity_id"]
    );
    let statement = store.record_education_event(
        lid,
        sid,
        "learner_report",
        "correction",
        &json!({"text":"更正：本次计数是五次"}),
    )?;
    let local = promote_correction(&store, lid, sid, &statement)?;
    ensure!(
        store.memory_evidence(lid, &local)?["activity_binding"]["activity_id"] == b["activity_id"]
    );
    ensure!(
        store
            .recall_for_session(sid, "更正计数观察", 5)?
            .iter()
            .all(|row| row["memory_id"] != corrected)
    );
    Ok(())
}

#[test]
fn changed_source_binding_stays_unresolved_and_deletion_invalidates_real_vectors() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("invalidate.db");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("A", "zh", "guided")?;
    let lid = s(&learner, "learner_id").to_owned();
    let a = store.create_session(&lid)?;
    let (ae, mid) = remember(&store, &lid, s(&a, "session_id"), "电流证据")?;
    let profile = EmbeddingProfile::E5NativeFp32;
    let key = profile.model_key();
    let work = store.embedding_work(&lid, &key, 32)?;
    let mut vector = vec![0.0; profile.dimension()];
    vector[0] = 1.0;
    ensure!(store.commit_memory_embedding(&work[0], &key, &vector)?);
    // A no-op legacy write preserves identity; an actual provenance rewrite
    // cannot retain its previous trusted binding or cache.
    store.db.execute(
        "UPDATE memories SET source_event_ids=? WHERE memory_id=?",
        params![json!([ae]).to_string(), mid],
    )?;
    ensure!(store.memory_evidence(&lid, &mid)?["activity_binding"]["status"] == "bound");
    let replacement = store.record_education_event(
        &lid,
        s(&a, "session_id"),
        "learner_report",
        "observation",
        &json!({"text":"另一次电流来源"}),
    )?;
    store.db.execute(
        "UPDATE memories SET source_event_ids=? WHERE memory_id=?",
        params![json!([replacement["event_id"]]).to_string(), mid],
    )?;
    drop(store);
    let store = Store::open(&path, "real")?;
    ensure!(
        store
            .one(
                "SELECT activity_id FROM memory_activity_bindings WHERE memory_id=?",
                [&mid]
            )?
            .unwrap()["activity_id"]
            .is_null()
    );
    ensure!(store.memory_evidence(&lid, &mid).is_err());
    ensure!(
        store
            .recall_for_session(s(&a, "session_id"), "电流", 5)?
            .is_empty()
    );
    ensure!(
        store
            .recall_candidates_for_session(
                s(&a, "session_id"),
                "电流",
                Some(QueryEmbedding {
                    model_key: &key,
                    values: &vector
                }),
                5
            )?
            .is_empty()
    );
    let (_, fresh) = remember(&store, &lid, s(&a, "session_id"), "磁场证据")?;
    let fresh_work = store
        .embedding_work(&lid, &key, 32)?
        .into_iter()
        .find(|v| v.memory_id == fresh)
        .unwrap();
    ensure!(store.commit_memory_embedding(&fresh_work, &key, &vector)?);
    store.forget(&lid, Some(&fresh))?;
    ensure!(!store.commit_memory_embedding(&fresh_work, &key, &vector)?);
    ensure!(
        store
            .one(
                "SELECT memory_id FROM memory_activity_bindings WHERE memory_id=?",
                [&fresh]
            )?
            .is_none()
    );
    ensure!(
        store
            .recall_candidates_for_session(
                s(&a, "session_id"),
                "磁场",
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

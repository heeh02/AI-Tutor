//! Synthetic educational memory baseline. Uses temporary SQLite only; no
//! learner data, microphone, camera, model or network requests are involved.
use ai_tutor::{
    context::build_context,
    store::{Store, empty_state, s},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

fn corpus() -> Value {
    serde_json::from_str(include_str!("fixtures/education_memory_v1.json")).unwrap()
}

fn candidate(content: &str, sources: &[String], kind: &str) -> Value {
    json!({"kind":kind,"content":content,"scope":"current_activity","source_event_ids":sources})
}

fn observed(store: &Store, lid: &str, sid: &str, content: &str) -> Result<(String, String)> {
    let event = store.event(lid, sid, "learner_report", &json!({"text":content}))?;
    let epoch = store.learner(lid)?["epoch"].as_i64().unwrap();
    let memory = store
        .add_memory(
            lid,
            &candidate(content, &[event.clone()], "reported_progress"),
            epoch,
        )?
        .unwrap();
    Ok((event, memory))
}

fn fill_history(store: &Store, lid: &str, sid: &str, count: usize) -> Result<()> {
    for index in 0..count {
        observed(
            store,
            lid,
            sid,
            &format!("占位练习记录第{index}次，仅用于合成测试"),
        )?;
    }
    Ok(())
}

#[test]
fn memory_recall_and_deduplication_reach_beyond_the_display_page() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("history.sqlite3");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("synthetic", "zh", "one_step")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let content = "紫色棱镜的观察记录需要保留";
    let (event, oldest) = observed(&store, lid, sid, content)?;
    fill_history(&store, lid, sid, 105)?;
    ensure!(store.memories(lid)?.len() == 100);
    ensure!(
        store
            .memories(lid)?
            .iter()
            .all(|row| row["memory_id"] != oldest)
    );
    ensure!(
        store
            .recall(lid, "紫色棱镜", 5)?
            .iter()
            .any(|row| row["memory_id"] == oldest)
    );
    let duplicate = store.add_memory(lid, &candidate(content, &[event], "reported_progress"), 0)?;
    ensure!(duplicate.as_deref() == Some(oldest.as_str()));
    let count: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM memories WHERE learner_id=? AND status='active'",
        [lid],
        |row| row.get(0),
    )?;
    ensure!(count == 106);
    let question = "紫色棱镜";
    let current_event = store.event(lid, sid, "learner_report", &json!({"text":question}))?;
    let context = build_context(
        &store,
        &store.session(sid)?,
        &learner,
        question,
        &current_event,
        &json!({}),
    )?;
    ensure!(
        context["slow_memory"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["memory_id"] == oldest && row["historical"] == true)
    );
    drop(store);
    let reopened = Store::open(&path, "real")?;
    ensure!(
        reopened
            .recall(lid, "紫色棱镜", 5)?
            .iter()
            .any(|row| row["memory_id"] == oldest)
    );
    Ok(())
}

#[test]
fn recall_budget_skips_oversized_candidates_but_keeps_searching() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("budget.sqlite3"), "real")?;
    let learner = store.create_learner("synthetic", "zh", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let query = "甲乙丙丁戊己庚辛壬癸";
    let mut last = String::new();
    for (index, size) in [800, 800, 700, 800, 800, 50].into_iter().enumerate() {
        let prefix = format!("{query}{index}");
        let content = format!("{prefix}{}", "字".repeat(size - prefix.chars().count()));
        let (_, id) = observed(&store, lid, s(&session, "session_id"), &content)?;
        store.db.execute(
            "UPDATE memories SET updated_at=? WHERE memory_id=?",
            rusqlite::params![format!("2026-01-01T00:00:0{}Z", 6 - index), id],
        )?;
        last = id;
    }
    let recalled = store.recall(lid, query, 5)?;
    ensure!(recalled.len() == 4 && recalled.iter().any(|row| row["memory_id"] == last));
    ensure!(
        recalled
            .iter()
            .map(|row| s(row, "content").chars().count())
            .sum::<usize>()
            == 2350
    );
    Ok(())
}

#[test]
fn memory_pages_are_stable_across_new_inserts_and_keep_legacy_shape() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("pages.sqlite3"), "real")?;
    let learner = store.create_learner("synthetic", "zh", "one_step")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    fill_history(&store, lid, sid, 105)?;
    let (first, cursor) = store.memory_page(lid, None, 100)?;
    ensure!(first == store.memories(lid)? && cursor.is_some());
    let (_, inserted) = observed(&store, lid, sid, "分页读取期间加入的新报告")?;
    let (second, end) = store.memory_page(lid, cursor, 100)?;
    ensure!(first.len() == 100 && second.len() == 5 && end.is_none());
    let all: Vec<_> = first.iter().chain(second.iter()).collect();
    let ids: HashSet<_> = all.iter().map(|row| s(row, "memory_id")).collect();
    ensure!(ids.len() == 105 && !ids.contains(inserted.as_str()));
    ensure!(
        all.iter()
            .all(|row| row.get("page_cursor").is_none() && row["source_event_ids"].is_array())
    );
    ensure!(store.memory_page(lid, None, 0).is_err());
    ensure!(store.memory_page(lid, None, 101).is_err());
    Ok(())
}

#[test]
fn model_correction_invalidates_memory_older_than_one_hundred_records() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("old-correction.sqlite3"), "real")?;
    let learner = store.create_learner("synthetic", "zh", "one_step")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let (old_event, old_memory) = observed(&store, lid, sid, "停车成功")?;
    fill_history(&store, lid, sid, 105)?;
    let correction = store.event(
        lid,
        sid,
        "learner_report",
        &json!({"text":"刚才说错了，停车尚未成功"}),
    )?;
    let updated = store.add_memory(
        lid,
        &candidate(
            "据报告更正：停车尚未成功",
            &[old_event.clone(), correction],
            "correction",
        ),
        0,
    )?;
    ensure!(updated.is_some());
    ensure!(store.source_events(lid, &[old_event.clone()])?.is_empty());
    let recalled = store.recall(lid, "停车", 5)?;
    ensure!(recalled.iter().all(|row| row["memory_id"] != old_memory));
    ensure!(
        recalled
            .iter()
            .any(|row| row["memory_id"] == updated.as_deref().unwrap())
    );
    ensure!(
        store
            .add_memory(
                lid,
                &candidate("停车成功", &[old_event], "reported_progress"),
                0
            )?
            .is_none()
    );
    Ok(())
}

#[test]
fn bilingual_retrieval_corpus_reports_semantic_gaps_without_claiming_model_quality() -> Result<()> {
    let data = corpus();
    ensure!(data["version"] == 1 && data["synthetic_only"] == true);
    let mut rows = vec![];
    for case in data["recall_cases"].as_array().unwrap() {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("recall.sqlite3"), "real")?;
        let learner = store.create_learner("synthetic", "zh", "guided")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let (_, id) = observed(&store, lid, s(&session, "session_id"), s(case, "content"))?;
        let actual = store
            .recall(lid, s(case, "query"), 5)?
            .iter()
            .any(|row| row["memory_id"] == id);
        let expected = case["expected_recall"].as_bool().unwrap();
        let passed = actual == expected;
        if case["required"] == true {
            ensure!(
                passed,
                "required educational recall case failed: {}",
                s(case, "id")
            );
        }
        rows.push(json!({"case":case["id"],"language":case["language"],"expected_recall":expected,"actual_recall":actual,"passed":passed,"required":case["required"]}));
    }
    println!(
        "EDUCATION_MEMORY_BASELINE {}",
        json!({"fixture_version":1,"scope":"retrieval_only","cases":rows})
    );
    Ok(())
}

#[test]
fn bilingual_corrections_revoke_sources_and_deleted_memories_do_not_return() -> Result<()> {
    for case in corpus()["revision_cases"].as_array().unwrap() {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("revision.sqlite3");
        let store = Store::open(&path, "real")?;
        let learner = store.create_learner("synthetic", s(case, "language"), "guided")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let (old_source, old_id) = observed(&store, lid, sid, s(case, "original"))?;
        let new_source = store.event(
            lid,
            sid,
            "learner_report",
            &json!({"text":case["correction"]}),
        )?;
        let new_id = store
            .add_memory(
                lid,
                &candidate(
                    s(case, "stored_correction"),
                    &[old_source.clone(), new_source.clone()],
                    "correction",
                ),
                0,
            )?
            .unwrap();
        ensure!(store.source_events(lid, &[old_source])?.is_empty());
        let memories = store.recall(lid, s(case, "query"), 5)?;
        ensure!(memories.iter().any(|row| row["memory_id"] == new_id));
        ensure!(memories.iter().all(|row| row["memory_id"] != old_id));
        store.forget(lid, Some(&new_id))?;
        let epoch = store.learner(lid)?["epoch"].as_i64().unwrap();
        ensure!(
            store
                .add_memory(
                    lid,
                    &candidate(
                        s(case, "stored_correction"),
                        &[new_source],
                        "reported_progress"
                    ),
                    epoch
                )?
                .is_none()
        );
        drop(store);
        let reopened = Store::open(&path, "real")?;
        ensure!(reopened.recall(lid, s(case, "query"), 5)?.is_empty());
    }
    Ok(())
}

#[test]
fn bilingual_plans_and_displayed_suggestions_are_not_completed_experiments() -> Result<()> {
    for case in corpus()["plan_cases"].as_array().unwrap() {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("plan.sqlite3"), "real")?;
        let learner = store.create_learner("synthetic", s(case, "language"), "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let event = store.event(lid, sid, "learner_report", &json!({"text":case["plan"]}))?;
        let mut state = empty_state();
        state["pending_step"] = case["plan"].clone();
        state["field_evidence"]["pending_step"] = json!({"source_event_ids":[event]});
        store.checkpoint(sid, &state)?;
        let turn = store.add_turn(
            sid,
            "synthetic-request",
            0,
            "assistant",
            s(case, "suggestion"),
            "generated",
            &[],
        )?;
        store.acknowledge(sid, &turn, "shown")?;
        let context = build_context(
            &store,
            &store.session(sid)?,
            &learner,
            s(case, "plan"),
            &event,
            &json!({}),
        )?;
        ensure!(context["task"]["reported_result"] == "");
        ensure!(context["current_hardware_status"] == "unknown");
        store.finish_session(sid)?;
        let memories = store.memories(lid)?;
        ensure!(memories.len() == 1 && memories[0]["kind"] == "pending_experiment");
        ensure!(s(&memories[0], "content").contains(s(case, "plan")));
        ensure!(!s(&memories[0], "content").contains(s(case, "suggestion")));
    }
    Ok(())
}

#[test]
fn full_history_search_still_isolates_learner_namespace_and_activity() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let real = Store::open(dir.path().join("real.sqlite3"), "real")?;
    let sim = Store::open(dir.path().join("simulator.sqlite3"), "simulator")?;
    let a = real.create_learner("synthetic-a", "zh", "one_step")?;
    let b = real.create_learner("synthetic-b", "zh", "one_step")?;
    let lid = s(&a, "learner_id");
    let session = real.create_session(lid)?;
    let (_, secret) = observed(
        &real,
        lid,
        s(&session, "session_id"),
        "水晶棱镜活动的独立记录",
    )?;
    fill_history(&real, lid, s(&session, "session_id"), 105)?;
    ensure!(real.recall(s(&b, "learner_id"), "水晶棱镜", 5)?.is_empty());
    ensure!(sim.recall(lid, "水晶棱镜", 5).is_err());
    real.db.execute(
        "UPDATE memories SET activity_scope='other_activity' WHERE memory_id=?",
        [&secret],
    )?;
    ensure!(real.recall(lid, "水晶棱镜", 5)?.is_empty());
    ensure!(
        real.memory_page(lid, None, 100)?
            .0
            .iter()
            .all(|row| row["learner_id"] == lid)
    );
    Ok(())
}

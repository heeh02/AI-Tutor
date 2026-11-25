//! Additive migration, typed evidence, optimistic updates and deletion contracts.
use ai_tutor::{
    context::build_context,
    store::{Store, empty_state, s},
};
use anyhow::{Result, ensure};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

fn legacy_database(path: &Path, sources: &str) -> Result<()> {
    let db = Connection::open(path)?;
    db.execute_batch("
        CREATE TABLE learners(learner_id TEXT PRIMARY KEY,name TEXT,language TEXT,help_style TEXT,epoch INTEGER DEFAULT 0);
        CREATE TABLE sessions(session_id TEXT PRIMARY KEY,learner_id TEXT,activity_id TEXT,generation INTEGER DEFAULT 0,status TEXT,state TEXT,created_at TEXT);
        CREATE TABLE events(event_id TEXT PRIMARY KEY,learner_id TEXT,session_id TEXT,source_type TEXT,content TEXT,created_at TEXT);
        CREATE TABLE memories(memory_id TEXT PRIMARY KEY,learner_id TEXT,activity_scope TEXT,kind TEXT,content TEXT,source_event_ids TEXT,source_type TEXT,scope TEXT,created_at TEXT,updated_at TEXT,status TEXT,supersedes TEXT,epoch INTEGER);
        INSERT INTO learners VALUES('legacy-learner','synthetic legacy','zh','one_step',0);
    ")?;
    db.execute("INSERT INTO sessions VALUES('legacy-session','legacy-learner','legacy-activity',0,'idle',?,'2026-01-01')", [empty_state().to_string()])?;
    db.execute("INSERT INTO events VALUES('legacy-event','legacy-learner','legacy-session','learner_report',?,'2026-01-01')", [json!({"text":"合成旧记录：紫色棱镜"}).to_string()])?;
    db.execute("INSERT INTO memories VALUES('legacy-memory','legacy-learner','modular_rover','reported_progress','合成旧记录：紫色棱镜',?,'learner_report','current_activity','2026-01-01','2026-01-01','active','[]',0)", [sources])?;
    Ok(())
}

fn candidate(content: &str, event: &Value, kind: &str) -> Value {
    let id = s(event, "event_id");
    let mut value = json!({"content":content,"kind":kind,"scope":"current_activity","source_event_ids":[id],"source_event_versions":{}});
    value["source_event_versions"][id] = event["event_version"].clone();
    value
}

#[test]
fn namespace_is_bound_even_before_the_first_learning_session() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("owner.sqlite3");
    let store = Store::open(&path, "real")?;
    store.create_learner("synthetic", "zh", "guided")?;
    drop(store);
    ensure!(Store::open(&path, "simulator").is_err());
    let reopened = Store::open(&path, "real")?;
    ensure!(reopened.learners()?.len() == 1);
    Ok(())
}

#[test]
fn legacy_migration_preserves_content_and_reopening_does_not_reactivate_links() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("legacy.sqlite3");
    legacy_database(&path, "[\"legacy-event\"]")?;
    let store = Store::open(&path, "real")?;
    let evidence = store.memory_evidence("legacy-learner", "legacy-memory")?;
    ensure!(evidence["sources"][0]["event_kind"] == "legacy_report");
    ensure!(evidence["sources"][0]["event_version"] == 1);
    ensure!(evidence["sources"][0]["content"]["text"] == "合成旧记录：紫色棱镜");
    ensure!(store.state_revision("legacy-session")? == 0);
    ensure!(store.learner("legacy-learner")?["reading_level"] == "standard");
    ensure!(store.learner("legacy-learner")?["speech_speed"] == "normal");
    store.db.execute(
        "UPDATE memory_source_links SET status='invalidated' WHERE memory_id='legacy-memory'",
        [],
    )?;
    drop(store);
    let reopened = Store::open(&path, "real")?;
    ensure!(
        reopened
            .memory_evidence("legacy-learner", "legacy-memory")
            .is_err()
    );
    ensure!(reopened.recall("legacy-learner", "紫色棱镜", 5)?.is_empty());
    let count: i64 =
        reopened
            .db
            .query_row("SELECT COUNT(*) FROM memory_source_links", [], |row| {
                row.get(0)
            })?;
    ensure!(count == 1);
    Ok(())
}

#[test]
fn failed_legacy_migration_is_atomic_and_can_be_retried() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("bad-legacy.sqlite3");
    legacy_database(&path, "not-json")?;
    ensure!(Store::open(&path, "real").is_err());
    let db = Connection::open(&path)?;
    let created: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name='education_event_meta'",
        [],
        |row| row.get(0),
    )?;
    ensure!(created == 0);
    db.execute(
        "UPDATE memories SET source_event_ids='[\"legacy-event\"]'",
        [],
    )?;
    drop(db);
    let fixed = Store::open(&path, "real")?;
    ensure!(
        fixed
            .memory_evidence("legacy-learner", "legacy-memory")
            .is_ok()
    );
    Ok(())
}

#[test]
fn typed_evidence_requires_matching_versions_owners_and_namespaces() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sources.sqlite3");
    let store = Store::open(&path, "real")?;
    let a = store.create_learner("a", "zh", "guided")?;
    let b = store.create_learner("b", "en", "guided")?;
    let lid = s(&a, "learner_id");
    let session = store.create_session(lid)?;
    let event = store.record_education_event(
        lid,
        s(&session, "session_id"),
        "learner_report",
        "observation",
        &json!({"text":"合成观察：绿灯亮起"}),
    )?;
    let mut value = candidate("据报告绿灯亮起", &event, "reported_progress");
    value["source_event_versions"][s(&event, "event_id")] = json!(0);
    ensure!(store.add_memory(lid, &value, 0)?.is_none());
    value["source_event_versions"][s(&event, "event_id")] = json!(1);
    ensure!(store.add_memory(s(&b, "learner_id"), &value, 0)?.is_none());
    let mid = store.add_memory(lid, &value, 0)?.unwrap();
    ensure!(store.memory_evidence(lid, &mid)?["sources"][0]["event_kind"] == "observation");
    ensure!(store.memory_evidence(s(&b, "learner_id"), &mid).is_err());
    store.db.execute(
        "UPDATE memory_source_links SET namespace='simulator' WHERE memory_id=?",
        [&mid],
    )?;
    ensure!(store.memory_evidence(lid, &mid).is_err());
    ensure!(store.recall(lid, "绿灯", 5)?.is_empty());
    drop(store);
    ensure!(
        Store::open(&path, "real").is_err(),
        "namespace corruption must not be silently migrated"
    );
    Ok(())
}

#[test]
fn explicit_event_revision_invalidates_derived_memory_and_fast_state_after_reopen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("revision.sqlite3");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("a", "zh", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let old = store.record_education_event(
        lid,
        sid,
        "learner_report",
        "observation",
        &json!({"text":"停车成功"}),
    )?;
    let old_memory = store
        .add_memory(lid, &candidate("停车成功", &old, "reported_progress"), 0)?
        .unwrap();
    let mut state = empty_state();
    state["reported_result"] = json!("停车成功");
    state["field_evidence"]["reported_result"] = json!({"source_event_ids":[old["event_id"]]});
    ensure!(store.checkpoint_if_current(lid, sid, 0, 0, 0, &state)? == 1);
    let updated = store.revise_education_event(
        lid,
        sid,
        s(&old, "event_id"),
        "operator_confirmed",
        &json!({"correction":"尚未成功，原记录有误"}),
    )?;
    ensure!(updated["event_version"] == 2 && updated["event_kind"] == "correction");
    ensure!(store.memory_evidence(lid, &old_memory).is_err());
    ensure!(store.session(sid)?["state"]["reported_result"] == "");
    let epoch = store.learner(lid)?["epoch"].as_i64().unwrap();
    let mut replacement = candidate("教师更正：尚未成功", &updated, "reported_progress");
    replacement["source_event_versions"][s(&updated, "event_id")] = json!(1);
    ensure!(store.add_memory(lid, &replacement, epoch)?.is_none());
    replacement["source_event_versions"][s(&updated, "event_id")] = json!(2);
    let new_memory = store.add_memory(lid, &replacement, epoch)?.unwrap();
    drop(store);
    let reopened = Store::open(&path, "real")?;
    ensure!(
        reopened
            .source_events(lid, &[s(&old, "event_id").to_owned()])?
            .is_empty()
    );
    ensure!(reopened.memory_evidence(lid, &old_memory).is_err());
    ensure!(reopened.memory_evidence(lid, &new_memory)?["sources"][0]["event_version"] == 2);
    Ok(())
}

#[test]
fn selective_forgetting_preserves_unrelated_typed_sources_and_rejects_queued_updates() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("forget.sqlite3");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("a", "zh", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let old = store.record_education_event(
        lid,
        sid,
        "learner_report",
        "observation",
        &json!({"text":"应删除的合成观察"}),
    )?;
    let deleted = store
        .add_memory(
            lid,
            &candidate("应删除的合成观察", &old, "reported_progress"),
            0,
        )?
        .unwrap();
    let kept = store.record_education_event(
        lid,
        sid,
        "learner_report",
        "plan",
        &json!({"text":"下次测试紫色棱镜","accepted":true}),
    )?;
    let retained = store
        .add_memory(
            lid,
            &candidate("下次测试紫色棱镜", &kept, "pending_experiment"),
            0,
        )?
        .unwrap();
    let before = store.state_revision(sid)?;
    store.forget(lid, Some(&deleted))?;
    ensure!(store.state_revision(sid)? > before);
    ensure!(
        store
            .checkpoint_if_current(lid, sid, 0, before, 0, &empty_state())
            .is_err()
    );
    ensure!(store.memory_evidence(lid, &retained)?["sources"][0]["event_kind"] == "plan");
    drop(store);
    let reopened = Store::open(&path, "real")?;
    ensure!(reopened.memory_evidence(lid, &retained)?["sources"][0]["event_version"] == 1);
    ensure!(reopened.memory_evidence(lid, &deleted).is_err());
    let orphan: i64 = reopened.db.query_row(
        "SELECT COUNT(*) FROM memory_source_links WHERE memory_id=?",
        [deleted],
        |row| row.get(0),
    )?;
    ensure!(orphan == 0);
    Ok(())
}

#[test]
fn fast_state_cas_rejects_older_revision_foreign_sources_and_finished_sessions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cas.sqlite3");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("a", "zh", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let event = store.record_education_event(
        lid,
        sid,
        "learner_report",
        "learner_statement",
        &json!({"text":"想观察绿灯"}),
    )?;
    let mut state = empty_state();
    state["goal"] = json!("观察绿灯");
    state["field_evidence"]["goal"] = json!({"source_event_ids":[event["event_id"]]});
    let other = Store::open(&path, "real")?;
    ensure!(store.checkpoint_if_current(lid, sid, 0, 0, 0, &state)? == 1);
    ensure!(
        other
            .checkpoint_if_current(lid, sid, 0, 0, 0, &empty_state())
            .is_err()
    );
    let b = store.create_learner("b", "zh", "guided")?;
    let bsession = store.create_session(s(&b, "learner_id"))?;
    ensure!(
        store
            .checkpoint_if_current(
                s(&b, "learner_id"),
                s(&bsession, "session_id"),
                0,
                0,
                0,
                &state
            )
            .is_err()
    );
    state["field_evidence"]["goal"] = json!({});
    ensure!(
        store
            .checkpoint_if_current(lid, sid, 0, 1, 0, &state)
            .is_err()
    );
    store.checkpoint(sid, &empty_state())?;
    ensure!(store.state_revision(sid)? == 2);
    store.finish_session(sid)?;
    let final_state = store.session(sid)?;
    ensure!(
        store
            .checkpoint_if_current(
                lid,
                sid,
                final_state["generation"].as_i64().unwrap(),
                store.state_revision(sid)?,
                0,
                &empty_state()
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn typed_plans_cannot_be_promoted_to_progress_or_completed_fast_state() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("plan.sqlite3"), "real")?;
    let learner = store.create_learner("a", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let event = store.record_education_event(
        lid,
        sid,
        "learner_report",
        "plan",
        &json!({"text":"Next time we will test the light","accepted":true}),
    )?;
    ensure!(
        store
            .add_memory(
                lid,
                &candidate("The light test is complete", &event, "reported_progress"),
                0
            )?
            .is_none()
    );
    ensure!(
        store
            .add_memory(
                lid,
                &candidate(
                    "Next time we will test the light",
                    &event,
                    "pending_experiment"
                ),
                0
            )?
            .is_some()
    );
    let mut state = empty_state();
    state["reported_result"] = json!("The light test is complete");
    state["field_evidence"]["reported_result"] = json!({"source_event_ids":[event["event_id"]]});
    ensure!(
        store
            .checkpoint_if_current(lid, sid, 0, 0, 0, &state)
            .is_err()
    );
    let mut state = empty_state();
    state["stage"] = json!("completed");
    state["field_evidence"]["stage"] = json!({"source_event_ids":[event["event_id"]]});
    ensure!(
        store
            .checkpoint_if_current(lid, sid, 0, 0, 0, &state)
            .is_err()
    );
    ensure!(store.state_revision(sid)? == 0);
    Ok(())
}

#[test]
fn learner_preferences_are_partial_persistent_and_do_not_change_teaching_language() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("preferences.sqlite3");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("a", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    ensure!(learner["reading_level"] == "standard" && learner["speech_speed"] == "normal");
    store.update_learner_preferences(lid, Some("plain"), Some("slow"))?;
    store.update_learner_preferences(lid, None, Some("fast"))?;
    ensure!(store.learner(lid)?["reading_level"] == "plain");
    ensure!(store.learner(lid)?["speech_speed"] == "fast");
    ensure!(
        store
            .update_learner_preferences(lid, Some("gifted"), Some("slow"))
            .is_err()
    );
    ensure!(store.learner(lid)?["speech_speed"] == "fast");
    let session = store.create_session(lid)?;
    let event = store.event(
        lid,
        s(&session, "session_id"),
        "learner_report",
        &json!({"text":"What should I test next?"}),
    )?;
    let context = build_context(
        &store,
        &session,
        &store.learner(lid)?,
        "What should I test next?",
        &event,
        &json!({}),
    )?;
    ensure!(context["language"] == "en" && context["reading_level"] == "plain");
    ensure!(context.get("ui_language").is_none());
    ensure!(s(&context, "reading_guidance").contains("不得据此推断"));
    drop(store);
    let reopened = Store::open(&path, "real")?;
    let listed = reopened.learners()?;
    ensure!(
        listed[0]["reading_level"] == "plain"
            && listed[0]["speech_speed"] == "fast"
            && listed[0]["language"] == "en"
    );
    ensure!(
        reopened
            .update_learner_preferences("foreign", Some("plain"), None)
            .is_err()
    );
    Ok(())
}

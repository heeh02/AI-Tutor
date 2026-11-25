//! Explicit activity selection, real temporary SQLite provenance, no model/device.
use ai_tutor::{
    context::build_context,
    memory_embedding::EmbeddingProfile,
    store::{QueryEmbedding, Store, empty_state, s},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

fn origin(store: &Store) -> &'static str {
    if store.namespace == "simulator" {
        "simulated"
    } else {
        "learner_report"
    }
}
fn observe(store: &Store, lid: &str, sid: &str, text: &str) -> Result<String> {
    let event = store.record_education_event(
        lid,
        sid,
        origin(store),
        "observation",
        &json!({"text":text}),
    )?;
    Ok(store.add_memory(lid,&json!({"kind":"reported_progress","scope":"current_activity","content":text,"source_event_ids":[event["event_id"]]}),store.learner(lid)?["epoch"].as_i64().unwrap())?.unwrap())
}
fn continue_from(
    store: &Store,
    lid: &str,
    source: &str,
    current: &str,
    nonce: &str,
) -> Result<Value> {
    let generation = store.session(current)?["generation"].as_i64().unwrap();
    ensure!(
        store
            .continuation_replay(lid, source, nonce, current, generation)?
            .is_none()
    );
    let cancelled = store.bump(current, "idle")?;
    store.continue_finished_session(lid, source, nonce, current, generation, cancelled)
}
fn memory_ids(rows: &[Value]) -> Vec<String> {
    rows.iter().map(|v| s(v, "memory_id").to_owned()).collect()
}

#[test]
fn optional_continuation_never_displaces_near_limit_required_current_text() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("budget.db"), "real")?;
    let learner = store.create_learner("A", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    let source = store.create_session(lid)?;
    let source_sid = s(&source, "session_id");
    let goal = "historical goal ".repeat(50);
    let event = store.record_education_event(
        lid,
        source_sid,
        "learner_report",
        "learner_statement",
        &json!({"text":goal}),
    )?;
    let mut state = empty_state();
    state["goal"] = json!(goal);
    state["field_evidence"]["goal"] = json!({"source_event_ids":[event["event_id"]]});
    store.checkpoint(source_sid, &state)?;
    store.finish_session(source_sid)?;
    let mut low = 1usize;
    let mut high = 17_999usize;
    while low < high {
        let middle = (low + high + 1) / 2;
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let text = "x".repeat(middle);
        let event = store.event(lid, sid, "learner_report", &json!({"text":text}))?;
        if build_context(
            &store,
            &store.session(sid)?,
            &learner,
            &text,
            &event,
            &json!({}),
        )
        .is_ok()
        {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let text = "x".repeat(low - 32); // room for minor RFC3339 fractional-width differences, not a continuation block
    let baseline = store.create_session(lid)?;
    let base_sid = s(&baseline, "session_id");
    let event = store.event(lid, base_sid, "learner_report", &json!({"text":text}))?;
    ensure!(
        build_context(
            &store,
            &store.session(base_sid)?,
            &learner,
            &text,
            &event,
            &json!({})
        )?["user_input"]
            == text
    );
    let continued =
        continue_from(&store, lid, source_sid, base_sid, "budget-selection")?["session"].clone();
    let sid = s(&continued, "session_id");
    let event = store.event(lid, sid, "learner_report", &json!({"text":text}))?;
    let context = build_context(
        &store,
        &store.session(sid)?,
        &learner,
        &text,
        &event,
        &json!({}),
    )?;
    ensure!(
        context["user_input"] == text && context["context_selection"]["omitted_continuation"] == 1
    );
    ensure!(context.get("continuation").is_none());
    ensure!(context.to_string().chars().count() <= 18_000);
    Ok(())
}

#[test]
fn explicit_continuation_keeps_sourced_history_separate_from_new_current_state() -> Result<()> {
    for namespace in ["real", "simulator"] {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("resume.db"), namespace)?;
        let learner = store.create_learner("A", "zh", "guided")?;
        let lid = s(&learner, "learner_id");
        let old = store.create_session(lid)?;
        let old_sid = s(&old, "session_id");
        let goal = "比较同一试验中的测量次数";
        let pending = "下次先用同一把尺测量，再记录结果";
        let goal_source = store.record_education_event(
            lid,
            old_sid,
            origin(&store),
            "learner_statement",
            &json!({"text":goal}),
        )?;
        let plan = store.record_education_event(
            lid,
            old_sid,
            origin(&store),
            "plan",
            &json!({"text":pending,"accepted":true}),
        )?;
        let mut state = empty_state();
        state["goal"] = json!(goal);
        state["pending_step"] = json!(pending);
        state["stage"] = json!("completed");
        state["reported_result"] = json!("仅旧试验结果");
        state["modules"] = json!(["motor"]);
        state["session_preferences"] = json!({"help_style":"one_step"});
        state["field_evidence"]["goal"] = json!({"source_event_ids":[goal_source["event_id"]]});
        state["field_evidence"]["pending_step"] = json!({"source_event_ids":[plan["event_id"]]});
        store.checkpoint(old_sid, &state)?;
        store.finish_session(old_sid)?;
        let before = store.session(old_sid)?["generation"].as_i64().unwrap();
        let response = continue_from(&store, lid, old_sid, old_sid, "continue_1")?;
        let new = &response["session"];
        let sid = s(new, "session_id");
        ensure!(new["session_id"] != old["session_id"] && new["activity_id"] != old["activity_id"]);
        ensure!(new["state"] == empty_state());
        ensure!(
            new["continued_from"]["session_id"] == old_sid && new["continued_from"]["goal"] == goal
        );
        let continuation = &new["continuation"];
        ensure!(continuation["client_submission_id"] == "continue_1");
        ensure!(
            continuation["goal"]["text"] == goal && continuation["pending_step"]["text"] == pending
        );
        ensure!(
            continuation["goal"]["historical"] == true
                && continuation["pending_step"]["hardware_verified"] == false
        );
        let selection = store
            .source_events(lid, &[s(continuation, "selection_event_id").into()])?
            .remove(0);
        ensure!(
            selection["source_type"]
                == if namespace == "simulator" {
                    "simulated"
                } else {
                    "operator_confirmed"
                }
        );
        ensure!(
            selection["content"]["action"] == "continue_finished_activity"
                && selection["content"].get("report").is_none()
        );
        let current = store.record_education_event(
            lid,
            sid,
            origin(&store),
            "learner_statement",
            &json!({"text":"继续"}),
        )?;
        let context = build_context(
            &store,
            &store.session(sid)?,
            &learner,
            "继续",
            s(&current, "event_id"),
            &json!({}),
        )?;
        ensure!(context["continuation"]["goal"]["text"] == goal);
        ensure!(
            context["task"]["goal"] == ""
                && context["task"]["reported_result"] == ""
                && context["task"]["stage"] == "goal_setting"
        );
        ensure!(
            context["task"]["session_preferences"] == json!({})
                && context["task"]["modules"].is_null()
        );
        ensure!(context["current_hardware_status"] == "unknown");
        ensure!(
            context["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["event_id"] == goal_source["event_id"] && e["historical"] == true)
        );
        // A later turn generation is not permission to cancel it on a retry.
        store.bump(sid, "generating")?;
        let generation = store.session(sid)?["generation"].clone();
        let replay = store
            .continuation_replay(lid, old_sid, "continue_1", old_sid, before)?
            .unwrap();
        ensure!(replay["replayed"] == true && replay["session_id"] == sid);
        ensure!(
            store.session(sid)?["generation"] == generation
                && store.session(sid)?["status"] == "generating"
        );
        store.finish_session(sid)?;
        ensure!(
            store
                .continuation_replay(lid, old_sid, "continue_1", old_sid, before)?
                .unwrap()["session"]["status"]
                == "finished"
        );
    }
    Ok(())
}

#[test]
fn ancestors_are_allowed_but_siblings_and_mixed_new_summaries_are_not() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("chain.db"), "real")?;
    let learner = store.create_learner("A", "zh", "guided")?;
    let lid = s(&learner, "learner_id");
    let a = store.create_session(lid)?;
    let asid = s(&a, "session_id");
    let am = observe(&store, lid, asid, "共同实验A观察")?;
    store.finish_session(asid)?;
    let b = continue_from(&store, lid, asid, asid, "B")?["session"].clone();
    let bsid = s(&b, "session_id");
    let bm = observe(&store, lid, bsid, "共同实验B观察")?;
    store.finish_session(bsid)?;
    let c = continue_from(&store, lid, asid, bsid, "C")?["session"].clone();
    let csid = s(&c, "session_id");
    let cm = observe(&store, lid, csid, "共同实验C观察")?;
    let cids = memory_ids(&store.recall_for_session(csid, "共同实验", 5)?);
    ensure!(cids.contains(&am) && cids.contains(&cm) && !cids.contains(&bm));
    let d = continue_from(&store, lid, bsid, csid, "D")?["session"].clone();
    let dsid = s(&d, "session_id");
    let dm = observe(&store, lid, dsid, "共同实验D观察")?;
    let dids = memory_ids(&store.recall_for_session(dsid, "共同实验", 5)?);
    ensure!(dids.contains(&am) && dids.contains(&bm) && dids.contains(&dm) && !dids.contains(&cm));
    ensure!(
        d["continuation"]["ancestor_count"] == 2 && store.session(csid)?["status"] == "finished"
    );
    let sources_a = store.memory_evidence(lid, &am)?["memory"]["source_event_ids"]
        .as_array()
        .unwrap()
        .clone();
    let sources_d = store.memory_evidence(lid, &dm)?["memory"]["source_event_ids"]
        .as_array()
        .unwrap()
        .clone();
    ensure!(store.add_memory(lid,&json!({"kind":"reported_progress","scope":"current_activity","content":"跨代混合摘要不在本片范围","source_event_ids":[sources_a[0],sources_d[0]]}),0)?.is_none());
    let profile = EmbeddingProfile::E5NativeFp32;
    let key = profile.model_key();
    let mut vector = vec![0.0; profile.dimension()];
    vector[0] = 1.0;
    for work in store.embedding_work(lid, &key, 32)? {
        ensure!(store.commit_memory_embedding(&work, &key, &vector)?);
    }
    let dense = store.recall_candidates_for_session(
        dsid,
        "共同实验",
        Some(QueryEmbedding {
            model_key: &key,
            values: &vector,
        }),
        5,
    )?;
    ensure!(dense.iter().all(|r| r.memory["memory_id"] != cm));
    ensure!(
        dense.iter().any(|r| r.memory["memory_id"] == am)
            && dense.iter().any(|r| r.memory["memory_id"] == bm)
    );
    let fresh = store.create_session(lid)?;
    ensure!(
        store
            .recall_for_session(s(&fresh, "session_id"), "共同实验", 5)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn preflight_rejects_invalid_scope_and_payload_without_mutating_and_commit_is_atomic() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("receipt.db"), "real")?;
    let a = store.create_learner("A", "zh", "guided")?;
    let lid = s(&a, "learner_id");
    let source = store.create_session(lid)?;
    let source_sid = s(&source, "session_id");
    store.finish_session(source_sid)?;
    let current = store.create_session(lid)?;
    let sid = s(&current, "session_id");
    let b = store.create_learner("B", "zh", "guided")?;
    let bsession = store.create_session(s(&b, "learner_id"))?;
    store.finish_session(s(&bsession, "session_id"))?;
    let original = store.session(sid)?;
    for (old, nonce, generation) in [
        (s(&bsession, "session_id"), "x", 0),
        (sid, "x", 0),
        (source_sid, "invalid nonce", 0),
        (source_sid, "x", 3),
    ] {
        ensure!(
            store
                .continuation_replay(lid, old, nonce, sid, generation)
                .is_err()
        );
        ensure!(store.session(sid)? == original);
    }
    ensure!(
        store
            .continuation_replay(lid, source_sid, "operation", sid, 0)?
            .is_none()
    );
    let after = store.bump(sid, "idle")?;
    let count = store.rows("SELECT session_id FROM sessions", [])?.len();
    store.db.execute_batch("CREATE TEMP TRIGGER fail_continuation BEFORE INSERT ON session_continuations BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;")?;
    ensure!(
        store
            .continue_finished_session(lid, source_sid, "operation", sid, 0, after)
            .is_err()
    );
    ensure!(
        store.rows("SELECT session_id FROM sessions", [])?.len() == count
            && store.session(sid)?["status"] == "idle"
    );
    ensure!(
        store
            .rows("SELECT event_id FROM events WHERE learner_id=?", [lid])?
            .is_empty()
    );
    store.db.execute_batch("DROP TRIGGER fail_continuation;")?;
    let result = store.continue_finished_session(lid, source_sid, "operation", sid, 0, after)?;
    let new_sid = s(&result, "session_id");
    ensure!(
        store
            .continuation_replay(lid, source_sid, "operation", sid, 1)
            .is_err()
    );
    let state = store.session(new_sid)?;
    ensure!(
        store
            .continuation_replay(lid, source_sid, "operation", sid, 0)?
            .unwrap()["replayed"]
            == true
    );
    ensure!(store.session(new_sid)? == state);
    store.create_session(lid)?;
    ensure!(
        store
            .continuation_replay(lid, source_sid, "operation", sid, 0)
            .is_err()
    );
    Ok(())
}

#[test]
fn selective_forget_keeps_selection_until_its_source_is_explicitly_revised()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("forget.db");
    let store = Store::open(&path, "real")?;
    let learner = store.create_learner("A", "zh", "guided")?;
    let lid = s(&learner, "learner_id").to_owned();
    let a = store.create_session(&lid)?;
    let asid = s(&a, "session_id");
    let forgotten = observe(&store, &lid, asid, "共同实验已忘记")?;
    let retained = observe(&store, &lid, asid, "共同实验旧保留")?;
    store.finish_session(asid)?;
    let original_generation = store.session(asid)?["generation"].as_i64().unwrap();
    let b = continue_from(&store, &lid, asid, asid, "remembered-choice")?["session"].clone();
    let bsid = s(&b, "session_id");
    let selection = s(&b["continuation"], "selection_event_id").to_owned();
    let own = observe(&store, &lid, bsid, "共同实验新保留")?;
    ensure!(
        store
            .recall_for_session(bsid, "共同实验", 5)?
            .iter()
            .any(|m| m["memory_id"] == retained)
    );
    store.forget(&lid, Some(&forgotten))?;
    ensure!(store.session(bsid)?["continuation"]["selection_event_id"] == selection);
    ensure!(store.recall_for_session(bsid, "共同实验", 5)?.iter()
        .any(|memory| memory["memory_id"] == retained));
    // The independent memory deletion is not a withdrawal of this user's
    // explicit continuation. Revoke that separate source to test a broken link.
    store.revise_education_event(&lid, bsid, &selection, "operator_confirmed",
        &json!({"text":"撤销这次续做选择；不再继承旧活动。"}))?;
    drop(store);
    let store = Store::open(&path, "real")?;
    ensure!(store.memory_evidence(&lid, &retained).is_ok());
    ensure!(store.session(bsid)?["continuation"].is_null());
    ensure!(memory_ids(&store.recall_for_session(bsid, "共同实验", 5)?) == vec![own.clone()]);
    ensure!(
        store
            .continuation_replay(&lid, asid, "remembered-choice", asid, original_generation)
            .is_err()
    );
    ensure!(
        store
            .rows("SELECT session_id FROM session_continuations", [])?
            .len()
            == 1,
        "invalid receipt must remain to prevent retry recreation"
    );
    store.finish_session(bsid)?;
    let fresh = continue_from(&store, &lid, bsid, bsid, "new-explicit-choice")?["session"].clone();
    let fresh_ids =
        memory_ids(&store.recall_for_session(s(&fresh, "session_id"), "共同实验", 5)?);
    ensure!(fresh["continuation"]["ancestor_count"] == 1);
    ensure!(
        fresh_ids.contains(&own) && !fresh_ids.contains(&retained),
        "a new explicit choice must not skip B's broken parent link to A"
    );
    Ok(())
}

#[test]
fn catalog_is_owner_scoped_stably_paginated_and_omits_invalid_or_unaccepted_history() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("catalog.db"), "simulator")?;
    let a = store.create_learner("A", "zh", "guided")?;
    let lid = s(&a, "learner_id");
    let first = store.create_session(lid)?;
    let sid = s(&first, "session_id");
    let goal = store.record_education_event(
        lid,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":"已撤销目标"}),
    )?;
    let plan = store.record_education_event(
        lid,
        sid,
        "simulated",
        "plan",
        &json!({"text":"下次测量","accepted":false}),
    )?;
    let mut state = empty_state();
    state["goal"] = json!("已撤销目标");
    state["pending_step"] = json!("下次测量");
    state["field_evidence"]["goal"] = json!({"source_event_ids":[goal["event_id"]]});
    state["field_evidence"]["pending_step"] = json!({"source_event_ids":[plan["event_id"]]});
    store.checkpoint(sid, &state)?;
    store.finish_session(sid)?;
    store.db.execute(
        "DELETE FROM events WHERE event_id=?",
        [s(&goal, "event_id")],
    )?;
    let b = store.create_learner("B", "zh", "guided")?;
    let foreign = store.create_session(s(&b, "learner_id"))?;
    let second = store.create_session(lid)?;
    store.finish_session(s(&second, "session_id"))?;
    let page = store.session_catalog(lid, None, 1)?;
    let cursor = page["next_before"].as_i64().unwrap();
    ensure!(
        page["items"][0]["session_id"] == second["session_id"] && page["items"][0]["goal"] == ""
    );
    store.create_session(lid)?;
    let older = store.session_catalog(lid, Some(cursor), 1)?;
    ensure!(older["items"][0]["session_id"] == sid && older["items"][0]["goal"] == "");
    ensure!(
        store.session_catalog(lid, None, 50)?["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["session_id"] != foreign["session_id"])
    );
    ensure!(
        store.session_catalog(lid, Some(0), 20).is_err()
            && store.session_catalog(lid, None, 0).is_err()
    );
    let current = store.latest_session(lid)?.unwrap();
    let resumed = continue_from(&store, lid, sid, s(&current, "session_id"), "empty-history")?;
    ensure!(
        resumed["session"]["continuation"]["goal"].is_null()
            && resumed["session"]["continuation"]["pending_step"].is_null()
    );
    Ok(())
}

#[test]
fn sourced_completion_or_clear_stops_older_pending_and_broken_middle_link_stops_ancestry()
-> Result<()> {
    for completed in [true, false] {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("pending.db"), "real")?;
        let learner = store.create_learner("A", "zh", "guided")?;
        let lid = s(&learner, "learner_id");
        let a = store.create_session(lid)?;
        let asid = s(&a, "session_id");
        let am = observe(&store, lid, asid, "共同实验A的观察")?;
        let text = "下次测量三次";
        let plan = store.record_education_event(
            lid,
            asid,
            "learner_report",
            "plan",
            &json!({"text":text,"accepted":true}),
        )?;
        let mut state = empty_state();
        state["pending_step"] = json!(text);
        state["field_evidence"]["pending_step"] = json!({"source_event_ids":[plan["event_id"]]});
        store.checkpoint(asid, &state)?;
        store.finish_session(asid)?;
        let b = continue_from(&store, lid, asid, asid, "B-plan")?["session"].clone();
        let bsid = s(&b, "session_id");
        ensure!(b["continuation"]["pending_step"]["completion_status"] == "not_revalidated");
        ensure!(b["continuation"]["pending_step"]["as_of_session_id"] == asid);
        let report = store.record_education_event(
            lid,
            bsid,
            "learner_report",
            "observation",
            &json!({"text":"计划已完成或明确撤回，此处是固定合成fixture"}),
        )?;
        let mut state = empty_state();
        if completed {
            state["stage"] = json!("completed");
            state["field_evidence"]["stage"] = json!({"source_event_ids":[report["event_id"]]});
        } else {
            state["field_evidence"]["pending_step"] =
                json!({"source_event_ids":[report["event_id"]]});
        }
        store.checkpoint(bsid, &state)?;
        store.finish_session(bsid)?;
        let c = continue_from(&store, lid, bsid, bsid, "C-plan")?["session"].clone();
        let csid = s(&c, "session_id");
        ensure!(
            c["continuation"]["pending_step"].is_null(),
            "B's sourced update must not revive A's older pending field"
        );
        let resolution = &c["continuation"]["pending_resolution"];
        ensure!(
            resolution["source_event_ids"] == json!([report["event_id"]])
                && resolution["historical"] == true
        );
        ensure!(
            resolution["kind"]
                == if completed {
                    "completed_activity"
                } else {
                    "cleared_pending_step"
                }
        );
        let question = store.record_education_event(
            lid,
            csid,
            "learner_report",
            "learner_statement",
            &json!({"text":"继续"}),
        )?;
        let context = build_context(
            &store,
            &store.session(csid)?,
            &learner,
            "继续",
            s(&question, "event_id"),
            &json!({}),
        )?;
        ensure!(context["continuation"]["pending_resolution"] == *resolution);
        let completion_source = context["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["event_id"] == report["event_id"])
            .unwrap();
        ensure!(
            completion_source["content"] == report["content"]
                && completion_source["historical"] == true
                && completion_source["original_content_included"] == true
        );
        ensure!(
            store
                .recall_for_session(csid, "共同实验", 5)?
                .iter()
                .any(|v| v["memory_id"] == am)
        );
        store.db.execute(
            "DELETE FROM events WHERE event_id=?",
            [s(&b["continuation"], "selection_event_id")],
        )?;
        ensure!(store.session(csid)?["continuation"]["ancestor_count"] == 1);
        ensure!(
            store
                .recall_for_session(csid, "共同实验", 5)?
                .iter()
                .all(|v| v["memory_id"] != am),
            "broken middle choice cannot be skipped to reach A"
        );
    }
    Ok(())
}

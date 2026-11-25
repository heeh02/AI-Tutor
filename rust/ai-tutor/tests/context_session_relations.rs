//! Real source/session identity, including explicitly selected ancestry.
use ai_tutor::{
    context::{MAX_CONTEXT_CHARS, build_context, quote_windows_arg},
    store::{Store, s, uid},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

fn observation(store: &Store, lid: &str, sid: &str, text: &str) -> Result<(String, String)> {
    let event = store.record_education_event(
        lid,
        sid,
        "simulated",
        "observation",
        &json!({"text":text}),
    )?;
    let mid=store.add_memory(lid,&json!({"kind":"reported_progress","scope":"current_activity","content":text,"source_event_ids":[event["event_id"]]}),store.learner(lid)?["epoch"].as_i64().unwrap())?.unwrap();
    Ok((s(&event, "event_id").into(), mid))
}
fn continued(store: &Store, lid: &str, source: &str, current: &str) -> Result<String> {
    let generation = store.session(current)?["generation"].as_i64().unwrap();
    let nonce = uid("choice");
    ensure!(
        store
            .continuation_replay(lid, source, &nonce, current, generation)?
            .is_none()
    );
    let after = store.bump(current, "idle")?;
    Ok(s(
        &store.continue_finished_session(lid, source, &nonce, current, generation, after)?,
        "session_id",
    )
    .into())
}
fn context(store: &Store, lid: &str, sid: &str, text: &str) -> Result<Value> {
    let event = store.record_education_event(
        lid,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    build_context(
        store,
        &store.session(sid)?,
        &store.learner(lid)?,
        text,
        s(&event, "event_id"),
        &json!({}),
    )
}
fn evidence<'a>(context: &'a Value, id: &str) -> &'a Value {
    context["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| s(e, "event_id") == id)
        .unwrap()
}
fn relation(context: &Value, id: &str, expected: &str, depth: Value) -> Result<()> {
    let row = evidence(context, id);
    ensure!(row["session_relation"] == expected && row["ancestor_depth"] == depth);
    Ok(())
}

#[test]
fn actual_current_parent_grandparent_are_named_without_revealing_sibling() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("relations.db"), "simulator")?;
    let lid = s(&store.create_learner("A", "zh", "guided")?, "learner_id").to_owned();
    let a = s(&store.create_session(&lid)?, "session_id").to_owned();
    let (ae, _) = observation(&store, &lid, &a, "测量记录：原活动读数七厘米。")?;
    store.finish_session(&a)?;
    let b = continued(&store, &lid, &a, &a)?;
    let (be, _) = observation(&store, &lid, &b, "测量记录：第一次续做读数八厘米。")?;
    store.finish_session(&b)?;
    let sibling = continued(&store, &lid, &a, &b)?;
    let (se, sm) = observation(&store, &lid, &sibling, "测量记录：另一条分支读数九厘米。")?;
    let c = continued(&store, &lid, &b, &sibling)?;
    let (ce, cm) = observation(&store, &lid, &c, "测量记录：当前续做读数十厘米。")?;
    let actual = context(&store, &lid, &c, "测量记录有哪些读数？")?;
    relation(&actual, &ae, "selected_ancestor", json!(2))?;
    relation(&actual, &be, "selected_ancestor", json!(1))?;
    relation(&actual, &ce, "current", json!(0))?;
    relation(
        &actual,
        s(&actual, "current_user_event_id"),
        "current",
        json!(0),
    )?;
    // This source entered as a slow-memory summary before recent-evidence pass.
    ensure!(
        actual["slow_memory"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["memory_id"] == cm)
    );
    ensure!(evidence(&actual, &ce)["historical"] == true);
    let serialized = actual.to_string();
    for hidden in [&sibling, &se, &sm] {
        ensure!(!serialized.contains(hidden.as_str()));
    }
    ensure!(
        actual["evidence_scope"]["activity_recall_scope"]
            == "current_and_explicitly_selected_ancestors"
    );
    ensure!(
        actual["evidence_scope"].get("siblings").is_none()
            && actual["evidence_scope"].get("outside_count").is_none()
    );
    Ok(())
}

#[test]
fn authorized_persistent_preference_is_outside_chain_not_an_invented_ancestor() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("preference.db"), "simulator")?;
    let lid = s(&store.create_learner("A", "zh", "guided")?, "learner_id").to_owned();
    let old = s(&store.create_session(&lid)?, "session_id").to_owned();
    let preference=store.record_education_event(&lid,&old,"simulated","learner_statement",&json!({"text":"以后所有活动都一次只讲一步。","explicit_preference":true,"preference_scope":"persistent"}))?;
    store.add_memory(&lid,&json!({"kind":"help_preference","scope":"cross_session_explicit","content":"以后一次一步","source_event_ids":[preference["event_id"]]}),0)?.unwrap();
    let (hidden, mid) = observation(&store, &lid, &old, "测量记录：旧活动读数七厘米。")?;
    let current = s(&store.create_session(&lid)?, "session_id").to_owned();
    let actual = context(&store, &lid, &current, "测量记录怎么整理？")?;
    relation(
        &actual,
        s(&preference, "event_id"),
        "outside_selected_chain",
        Value::Null,
    )?;
    ensure!(
        actual["slow_memory"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["kind"] == "help_preference")
    );
    ensure!(!actual.to_string().contains(&hidden) && !actual.to_string().contains(&mid));
    ensure!(
        actual["evidence_scope"]["cross_session_exception"]
            == "separately_authorized_persistent_preferences_only"
    );
    Ok(())
}

#[test]
fn selective_forget_preserves_ancestry_until_selection_source_is_explicitly_revised() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("forgotten.db"), "simulator")?;
    let lid = s(&store.create_learner("A", "zh", "guided")?, "learner_id").to_owned();
    let a = s(&store.create_session(&lid)?, "session_id").to_owned();
    let (_, forgotten) = observation(&store, &lid, &a, "测量记录：需要忘记的一条。")?;
    let (ae, retained) = observation(&store, &lid, &a, "测量记录：原活动保留的一条。")?;
    store.finish_session(&a)?;
    let b = continued(&store, &lid, &a, &a)?;
    let selection = s(&store.session(&b)?["continuation"], "selection_event_id").to_owned();
    let (be, _) = observation(&store, &lid, &b, "测量记录：本条续做仍保留。")?;
    let before = context(&store, &lid, &b, "测量记录")?;
    relation(&before, &ae, "selected_ancestor", json!(1))?;
    store.forget(&lid, Some(&forgotten))?;
    ensure!(store.memory_evidence(&lid, &retained).is_ok());
    let preserved = context(&store, &lid, &b, "测量记录")?;
    relation(&preserved, &ae, "selected_ancestor", json!(1))?;
    ensure!(store.session(&b)?["continuation"]["selection_event_id"] == selection);
    // Forgetting an independent observation does not withdraw this selection.
    // Only invalidating the selection's own source should break ancestry.
    store.revise_education_event(&lid, &b, &selection, "simulated",
        &json!({"text":"撤销这次续做选择；不再继承旧活动。"}))?;
    let broken = context(&store, &lid, &b, "测量记录")?;
    relation(&broken, &be, "current", json!(0))?;
    ensure!(!broken.to_string().contains(&ae) && !broken.to_string().contains(&a));
    store.finish_session(&b)?;
    let c = continued(&store, &lid, &b, &b)?;
    let new_choice = context(&store, &lid, &c, "测量记录")?;
    relation(&new_choice, &be, "selected_ancestor", json!(1))?;
    ensure!(!new_choice.to_string().contains(&ae) && !new_choice.to_string().contains(&a));
    ensure!(
        new_choice["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["ancestor_depth"].as_u64().is_none_or(|depth| depth <= 1))
    );
    Ok(())
}

#[test]
fn relationship_fields_are_counted_before_whole_evidence_budget_selection() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("budget.db"), "simulator")?;
    let lid = s(&store.create_learner("A", "zh", "guided")?, "learner_id").to_owned();
    let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
    for index in 0..10 {
        store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "observation",
            &json!({"text":format!("第{index}次记录：{}","测量观察".repeat(400))}),
        )?;
    }
    let text = "当前完整问题".repeat(350);
    let actual = context(&store, &lid, &sid, &text)?;
    ensure!(
        actual["user_input"] == text
            && actual["context_selection"]["omitted_evidence"]
                .as_u64()
                .unwrap()
                > 0
    );
    for row in actual["evidence"].as_array().unwrap() {
        ensure!(row["session_relation"] == "current" && row["ancestor_depth"] == 0);
    }
    ensure!(actual.to_string().chars().count() <= MAX_CONTEXT_CHARS - 256);
    ensure!(
        quote_windows_arg(&actual.to_string())
            .encode_utf16()
            .count()
            <= 21_000 - 256
    );
    Ok(())
}

#[test]
fn legacy_required_task_source_is_honestly_outside_recall_chain() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("legacy-required.db"), "simulator")?;
    let lid = s(&store.create_learner("A", "zh", "guided")?, "learner_id").to_owned();
    let old = s(&store.create_session(&lid)?, "session_id").to_owned();
    let text = "旧活动报告读数七厘米。";
    let event = store.record_education_event(
        &lid,
        &old,
        "simulated",
        "observation",
        &json!({"text":text}),
    )?;
    let current = s(&store.create_session(&lid)?, "session_id").to_owned();
    // Recreate the persisted shape produced before the current-source write
    // guard. This test is not a new permitted model application or a migration.
    let mut state = store.session(&current)?["state"].clone();
    state["reported_result"] = json!(text);
    state["reported_result_source"] = json!("historical");
    state["field_evidence"]["reported_result"] = json!({"source_event_ids":[event["event_id"]],"historical":true,"original_source_type":"simulated","hardware_verified":false});
    store.checkpoint(&current, &state)?;
    let actual = context(&store, &lid, &current, "这条记录来自哪里？")?;
    ensure!(actual["task"]["reported_result"] == text);
    relation(
        &actual,
        s(&event, "event_id"),
        "outside_selected_chain",
        Value::Null,
    )?;
    ensure!(actual["slow_memory"].as_array().unwrap().is_empty());
    ensure!(
        actual["evidence_scope"]["activity_recall_scope"]
            == "current_and_explicitly_selected_ancestors"
    );
    ensure!(actual["evidence_scope"].get("activity_facts").is_none());
    ensure!(
        s(&actual["evidence_scope"], "existing_required_state")
            .contains("preexisting_outside_chain_sources")
    );
    ensure!(actual.get("continuation").is_none());
    Ok(())
}

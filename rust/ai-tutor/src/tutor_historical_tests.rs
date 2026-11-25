//! Real A -> completed B -> fresh C; no models/devices. Red before the guard fix.
use super::*;
use crate::{
    context::build_context,
    store::{empty_state, uid},
};

const PLAN: &str = "学习者同意下次把两个程序的初值都设为零后重新运行；这是已接受、尚未执行的计划。";
const COMPLETION: &str =
    "本次只完成了共同零初值的重新运行；这条报告没有给出输出数字。原先的待做步骤已结束。";
const QUESTION: &str = "共同零初值的重新运行现在仍是未做计划吗？记录给出了哪些新的输出数字？";

struct Fixture {
    store: Store,
    lid: String,
    b_sid: String,
    b_activity: String,
    sid: String,
    plan: String,
    completion: String,
    question: String,
    request: Value,
    _directory: tempfile::TempDir,
}
fn continued(store: &Store, lid: &str, source: &str) -> Result<String> {
    let generation = store.session(source)?["generation"].as_i64().unwrap();
    let nonce = uid("choice");
    ensure!(
        store
            .continuation_replay(lid, source, &nonce, source, generation)?
            .is_none()
    );
    let cancelled = store.bump(source, "idle")?;
    Ok(s(
        &store.continue_finished_session(lid, source, &nonce, source, generation, cancelled)?,
        "session_id",
    )
    .to_owned())
}
fn request(store: &Store, lid: &str, sid: &str, eid: &str, text: &str) -> Result<Value> {
    let session = store.session(sid)?;
    let context = build_context(store, &session, &store.learner(lid)?, text, eid, &json!({}))?;
    Ok(
        json!({"request_id":"history-guard","learner_id":lid,"session_id":sid,"generation":session["generation"],"context":context}),
    )
}
fn fixture() -> Result<Fixture> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("history-guard.db"), "simulator")?;
    let lid = s(
        &store.create_learner("synthetic", "zh", "guided")?,
        "learner_id",
    )
    .to_owned();
    let a = s(&store.create_session(&lid)?, "session_id").to_owned();
    let plan = store.record_education_event(
        &lid,
        &a,
        "simulated",
        "plan",
        &json!({"text":PLAN,"accepted":true}),
    )?;
    let mut state = empty_state();
    state["pending_step"] = json!(PLAN);
    state["field_evidence"]["pending_step"] = json!({"source_event_ids":[plan["event_id"]]});
    store.checkpoint(&a, &state)?;
    store.finish_session(&a)?;
    let b = continued(&store, &lid, &a)?;
    let b_activity = s(&store.session(&b)?, "activity_id").to_owned();
    let completion = store.record_education_event(
        &lid,
        &b,
        "simulated",
        "observation",
        &json!({"text":COMPLETION}),
    )?;
    let mut state = empty_state();
    state["stage"] = json!("completed");
    state["field_evidence"]["stage"] = json!({"source_event_ids":[completion["event_id"]]});
    store.checkpoint(&b, &state)?;
    store.finish_session(&b)?;
    let sid = continued(&store, &lid, &b)?;
    ensure!(store.session(&sid)?["state"] == empty_state());
    let question = store.record_education_event(
        &lid,
        &sid,
        "simulated",
        "learner_statement",
        &json!({"text":QUESTION}),
    )?;
    let request = request(&store, &lid, &sid, s(&question, "event_id"), QUESTION)?;
    ensure!(
        request["context"]["continuation"]["pending_resolution"]["source_event_ids"]
            == json!([completion["event_id"]])
    );
    Ok(Fixture {
        store,
        lid,
        b_sid: b,
        b_activity,
        sid,
        plan: s(&plan, "event_id").into(),
        completion: s(&completion, "event_id").into(),
        question: s(&question, "event_id").into(),
        request,
        _directory: directory,
    })
}
fn reply() -> Value {
    json!({"schema_version":"1.0","request_id":"history-guard","reply_text":"旧活动报告重新运行已经完成，但没有提供输出数字。","teaching_move":"answer","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false})
}
// Exact two task values and memory text from result-descendant_completed.json;
// opaque source IDs alone are rebound to this independent real Store fixture.
fn observed_reply(f: &Fixture) -> Value {
    let mut reply = reply();
    reply["used_evidence_ids"] = json!([f.completion, f.plan]);
    reply["task_update_candidates"] = json!([
        {"field":"stage","value":"completed","source_event_ids":[f.completion]},
        {"field":"reported_result","value":"据报告，本次只完成了共同零初值的重新运行，未给出输出数字；原先待做步骤已结束。","source_event_ids":[f.completion]}
    ]);
    reply["memory_candidates"] = json!([{"content":"据报告，共同零初值的重新运行已完成，原先待做步骤已结束；本次报告没有给出输出数字。","kind":"reported_progress","scope":"current_activity","source_event_ids":[f.completion]}]);
    reply
}

#[test]
fn actual_historical_completion_candidates_cannot_mark_fresh_continuation_completed() -> Result<()>
{
    let f = fixture()?;
    let value = observed_reply(&f);
    validate_reply(&value, &f.request)?;
    let before = f.store.session(&f.sid)?["state"].clone();
    let result = apply_reply(&f.store, &f.sid, &value, 0)?;
    ensure!(
        result["task_updates"] == 0 && result["rejected_candidates"] == 2,
        "Historical B task candidates must be rejected for C"
    );
    ensure!(f.store.session(&f.sid)?["state"] == before);
    ensure!(
        result["applied_memory_ids"].as_array().unwrap().len() == 1,
        "Legitimate B memory remains independent of current-task rejection"
    );
    Ok(())
}

#[test]
fn all_current_task_fields_reject_historical_support_even_on_explicit_parent_chain() -> Result<()> {
    let f = fixture()?;
    let before = f.store.session(&f.sid)?["state"].clone();
    let mut value = reply();
    value["used_evidence_ids"] = json!([f.completion]);
    value["task_update_candidates"] = json!([
        {"field":"goal","value":"沿用旧活动目标","source_event_ids":[f.completion]},
        {"field":"stage","value":"completed","source_event_ids":[f.completion]},
        {"field":"reported_result","value":"沿用旧完成报告","source_event_ids":[f.completion]},
        {"field":"pending_step","value":"沿用旧计划","source_event_ids":[f.completion]}
    ]);
    validate_reply(&value, &f.request)?;
    let result = apply_reply(&f.store, &f.sid, &value, 0)?;
    ensure!(result["task_updates"] == 0 && result["rejected_candidates"] == 4);
    ensure!(f.store.session(&f.sid)?["state"] == before);
    Ok(())
}

#[test]
fn historical_completion_plus_current_question_cannot_become_current_completion() -> Result<()> {
    let f = fixture()?;
    let mut value = observed_reply(&f);
    value["memory_candidates"] = json!([]);
    value["used_evidence_ids"] = json!([f.completion, f.question]);
    for candidate in value["task_update_candidates"].as_array_mut().unwrap() {
        candidate["source_event_ids"] = json!([f.completion, f.question]);
    }
    validate_reply(&value, &f.request)?;
    let before = f.store.session(&f.sid)?["state"].clone();
    let result = apply_reply(&f.store, &f.sid, &value, 0)?;
    ensure!(result["task_updates"] == 0 && result["rejected_candidates"] == 2);
    ensure!(f.store.session(&f.sid)?["state"] == before);
    Ok(())
}

#[test]
fn current_report_updates_remain_simulated_and_not_historical() -> Result<()> {
    let f = fixture()?;
    let text = "本次新续做已完成记录整理，没有新增实验输出数字。";
    let current = f.store.record_education_event(
        &f.lid,
        &f.sid,
        "simulated",
        "observation",
        &json!({"text":text}),
    )?;
    let request = request(&f.store, &f.lid, &f.sid, s(&current, "event_id"), text)?;
    let mut value = reply();
    value["used_evidence_ids"] = json!([current["event_id"]]);
    value["task_update_candidates"] = json!([
        {"field":"stage","value":"completed","source_event_ids":[current["event_id"]]},
        {"field":"reported_result","value":text,"source_event_ids":[current["event_id"]]}
    ]);
    validate_reply(&value, &request)?;
    let result = apply_reply(&f.store, &f.sid, &value, 0)?;
    ensure!(result["task_updates"] == 2 && result["rejected_candidates"] == 0);
    let state = f.store.session(&f.sid)?["state"].clone();
    for field in ["stage", "reported_result"] {
        ensure!(state[format!("{field}_source")] == "simulated");
        ensure!(
            state["field_evidence"][field]["historical"] == false
                && state["field_evidence"][field]["original_source_type"] == "simulated"
        );
    }
    ensure!(
        state["attempts"][0]["historical"] == false
            && state["attempts"][0]["original_source_type"] == "simulated"
    );
    Ok(())
}

#[test]
fn current_session_preference_cannot_reuse_historical_or_mixed_support() -> Result<()> {
    let f = fixture()?;
    for ids in [json!([f.completion]), json!([f.completion, f.question])] {
        let mut value = reply();
        value["used_evidence_ids"] = ids.clone();
        value["memory_candidates"] = json!([{"kind":"help_preference","scope":"current_session","content":"这次一次只讲一步","source_event_ids":ids}]);
        validate_reply(&value, &f.request)?;
        let before = f.store.session(&f.sid)?["state"].clone();
        let result = apply_reply(&f.store, &f.sid, &value, 0)?;
        ensure!(result["rejected_candidates"] == 1);
        ensure!(f.store.session(&f.sid)?["state"] == before);
    }
    Ok(())
}

#[test]
fn valid_current_preference_and_historical_memory_keep_separate_scopes() -> Result<()> {
    let f = fixture()?;
    let text = "本次活动请一次只讲一步。";
    let current = f.store.record_education_event(
        &f.lid,
        &f.sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    let request = request(&f.store, &f.lid, &f.sid, s(&current, "event_id"), text)?;
    let mut value = observed_reply(&f);
    value["task_update_candidates"] = json!([]);
    value["used_evidence_ids"] = json!([f.completion, current["event_id"]]);
    value["memory_candidates"].as_array_mut().unwrap().push(json!({"kind":"help_preference","scope":"current_session","content":text,"source_event_ids":[current["event_id"]]}));
    validate_reply(&value, &request)?;
    let result = apply_reply(&f.store, &f.sid, &value, 0)?;
    ensure!(
        result["rejected_candidates"] == 0
            && result["applied_memory_ids"].as_array().unwrap().len() == 1
    );
    let state = f.store.session(&f.sid)?["state"].clone();
    ensure!(
        state["session_preferences"]["explicit_request"] == text
            && state["reported_result"] == ""
            && state["stage"] == "goal_setting"
    );
    let memory = f
        .store
        .memory_evidence(&f.lid, result["applied_memory_ids"][0].as_str().unwrap())?;
    ensure!(
        memory["memory"]["source_type"] == "simulated"
            && memory["activity_binding"]["activity_id"] == f.b_activity
    );
    ensure!(
        memory["sources"][0]["session_id"] == f.b_sid
            && memory["memory"]["source_event_ids"] == json!([f.completion])
    );
    Ok(())
}

//! Pending-step application is source-exact; fixtures use SQLite only.
use ai_tutor::{
    store::{Store, s, uid},
    tutor::{apply_reply, validate_reply},
};
use anyhow::Result;
use serde_json::{Value, json};

const STOP: &str = "先让小车停下来。";
// Exact unsupported pending value from qwen-joint-first/semantic-review.json.
// The frozen report/database is never modified or used as a writable fixture.
const EXPANDED: &str = "先在不靠近运动部件的情况下，使用设备设计的停止按钮或电源开关停车；若做不到，请现场熟悉设备的人协助。";
const OLD_STEP: &str = "下次记录静止小车与标记线的距离。";

struct Fixture {
    store: Store,
    lid: String,
    sid: String,
    _directory: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(
            directory.path().join("pending-grounding.sqlite3"),
            "simulator",
        )?;
        let learner = store.create_learner("SYNTHETIC_PENDING_GROUNDING", "zh", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        Ok(Self {
            store,
            lid,
            sid,
            _directory: directory,
        })
    }
    fn event(&self, kind: &str, content: Value) -> Result<String> {
        Ok(s(
            &self.store.record_education_event(
                &self.lid,
                &self.sid,
                "simulated",
                kind,
                &content,
            )?,
            "event_id",
        )
        .to_owned())
    }
    fn value(&self, candidates: Value, ids: &[String]) -> Value {
        json!({"schema_version":"1.0","request_id":uid("pending-grounding"),"reply_text":EXPANDED,
            "teaching_move":"give_hint","task_update_candidates":candidates,"memory_candidates":[],
            "used_evidence_ids":ids,"needs_clarification":false})
    }
    fn apply(&self, reply: &Value, ids: &[String]) -> Result<Value> {
        let request = json!({"request_id":reply["request_id"],"learner_id":self.lid,"session_id":self.sid,
            "context":{"evidence":ids.iter().map(|id| json!({"event_id":id})).collect::<Vec<_>>()}});
        validate_reply(reply, &request)?;
        apply_reply(
            &self.store,
            &self.sid,
            reply,
            self.store.learner(&self.lid)?["epoch"].as_i64().unwrap(),
        )
    }
    fn apply_step(&self, value: Value, ids: &[String]) -> Result<Value> {
        let reply = self.value(
            json!([{"field":"pending_step","value":value,"source_event_ids":ids}]),
            ids,
        );
        self.apply(&reply, ids)
    }
    fn state(&self) -> Result<Value> {
        Ok(self.store.session(&self.sid)?["state"].clone())
    }
    fn seed(&self) -> Result<Value> {
        let id = self.event("plan", json!({"text":OLD_STEP,"accepted":true}))?;
        assert_eq!(self.apply_step(json!(OLD_STEP), &[id])?["task_updates"], 1);
        self.state()
    }
}

#[test]
fn observed_expanded_method_is_rejected_without_losing_goal_intent_or_delivered_advice()
-> Result<()> {
    let f = Fixture::new()?;
    let before = f.seed()?;
    let id = f.event("learner_statement", json!({"text":STOP}))?;
    let mut state = f.state()?;
    state["latest_input"] = json!(STOP);
    f.store.checkpoint(&f.sid, &state)?;
    let ids = [id.clone()];
    let reply = f.value(
        json!([
            {"field":"pending_step","value":EXPANDED,"source_event_ids":ids},
            {"field":"goal","value":"让小车先停止移动","source_event_ids":ids}
        ]),
        &ids,
    );
    let generation = f.store.session(&f.sid)?["generation"].as_i64().unwrap();
    f.store.add_turn(
        &f.sid,
        s(&reply, "request_id"),
        generation,
        "user",
        STOP,
        "shown",
        &ids,
    )?;
    let turn = f.store.add_turn(
        &f.sid,
        s(&reply, "request_id"),
        generation,
        "assistant",
        EXPANDED,
        "generated",
        &ids,
    )?;
    // A displayed tutor recommendation is retained as advice, not learner acceptance.
    f.store.acknowledge(&f.sid, &turn, "shown")?;
    let applied = f.apply(&reply, &ids)?;
    assert_eq!(applied["task_updates"], 1);
    assert_eq!(applied["rejected_candidates"], 1);
    let state = f.state()?;
    assert_eq!(state["pending_step"], before["pending_step"]);
    assert_eq!(
        state["field_evidence"]["pending_step"],
        before["field_evidence"]["pending_step"]
    );
    assert_eq!(state["goal"], "让小车先停止移动");
    assert_eq!(state["latest_input"], STOP);
    assert_eq!(state["last_delivered_hint"], EXPANDED);
    assert_eq!(reply["reply_text"], EXPANDED);
    assert_eq!(
        f.store.source_events(&f.lid, &ids)?[0]["content"]["text"],
        STOP
    );
    assert!(f.store.memories(&f.lid)?.is_empty());
    Ok(())
}

#[test]
fn immediate_bilingual_requests_do_not_require_historical_future_words() -> Result<()> {
    for raw in [STOP, "Stop the rover first."] {
        let f = Fixture::new()?;
        let id = f.event("learner_statement", json!({"text":format!(" \n{raw}\t")}))?;
        let result = f.apply_step(json!(raw), &[id.clone()])?;
        assert_eq!(result["task_updates"], 1);
        assert_eq!(result["rejected_candidates"], 0);
        assert_eq!(f.state()?["pending_step"], raw);
        assert_eq!(
            f.state()?["field_evidence"]["pending_step"]["source_event_ids"],
            json!([id])
        );
        assert_eq!(
            f.state()?["field_evidence"]["pending_step"]["hardware_verified"],
            false
        );
    }
    Ok(())
}

#[test]
fn accepted_plan_and_declared_pending_text_work_without_accepting_unknown_or_unaccepted_plans()
-> Result<()> {
    for (kind, content, expected) in [
        ("plan", json!({"text":OLD_STEP,"accepted":true}), true),
        ("plan", json!({"text":OLD_STEP,"accepted":false}), false),
        ("plan", json!({"text":OLD_STEP}), false),
        ("plan", json!({"text":OLD_STEP,"accepted":null}), false),
        (
            "plan",
            json!({"text":"Other text","pending_experiment":OLD_STEP,"accepted":true}),
            true,
        ),
        (
            "plan",
            json!({"pending_experiment":OLD_STEP,"accepted":false}),
            false,
        ),
        (
            "observation",
            json!({"report":"No measurement yet.","pending_experiment":OLD_STEP}),
            true,
        ),
        ("observation", json!({"text":OLD_STEP}), false),
    ] {
        let f = Fixture::new()?;
        let id = f.event(kind, content)?;
        let applied = f.apply_step(json!(OLD_STEP), &[id])?;
        assert_eq!(applied["task_updates"], if expected { 1 } else { 0 });
        assert_eq!(applied["rejected_candidates"], if expected { 0 } else { 1 });
    }
    let f = Fixture::new()?;
    let legacy = f
        .store
        .event(&f.lid, &f.sid, "simulated", &json!({"report":OLD_STEP}))?;
    assert_eq!(f.apply_step(json!(OLD_STEP), &[legacy])?["task_updates"], 1);
    Ok(())
}

#[test]
fn a_source_quote_cannot_be_replaced_with_a_paraphrase_negation_loss_question_or_unit_change()
-> Result<()> {
    for (raw, candidate) in [
        ("先不要启动电机。", "先启动电机。"),
        ("Do not move the rover.", "Move the rover."),
        ("要不要先停车？", "先停车。"),
        ("Should we stop the rover?", "Stop the rover."),
        ("下次测量 10 cm。", "下次测量 20 cm。"),
        ("Measure 10 cm.", "Measure 10 m."),
        ("Calculate 5!", "Calculate 5"),
        ("I will stop the rover.", "Stop the car."),
    ] {
        let f = Fixture::new()?;
        let before = f.seed()?;
        let id = f.event("learner_statement", json!({"text":raw}))?;
        let result = f.apply_step(json!(candidate), &[id.clone()])?;
        assert_eq!(result["task_updates"], 0, "{raw}");
        assert_eq!(result["rejected_candidates"], 1);
        assert_eq!(f.state()?["pending_step"], before["pending_step"]);
        assert_eq!(
            f.store.source_events(&f.lid, &[id])?[0]["content"]["text"],
            raw
        );
    }
    Ok(())
}

#[test]
fn every_cited_source_must_support_the_same_value_and_still_belong_to_this_session() -> Result<()> {
    let mut f = Fixture::new()?;
    let one = f.event("learner_statement", json!({"text":STOP}))?;
    let two = f.event("learner_statement", json!({"text":STOP}))?;
    assert_eq!(
        f.apply_step(json!(STOP), &[one.clone(), two])?["task_updates"],
        1
    );
    let unrelated = f.event("learner_statement", json!({"text":"我没有看到停止按钮。"}))?;
    let result = f.apply_step(json!(STOP), &[one.clone(), unrelated])?;
    assert_eq!(result["task_updates"], 0);
    // create_session finishes the previous activity; target the fresh session,
    // rather than accidentally testing a write to an already finished one.
    f.sid = s(&f.store.create_session(&f.lid)?, "session_id").to_owned();
    let current = f.event("learner_statement", json!({"text":STOP}))?;
    assert_eq!(
        f.apply_step(json!(STOP), &[current.clone(), one])?["task_updates"],
        0
    );
    let other = f
        .store
        .create_learner("OTHER_SYNTHETIC_LEARNER", "zh", "guided")?;
    let other_session = f.store.create_session(s(&other, "learner_id"))?;
    let foreign = f.store.record_education_event(
        s(&other, "learner_id"),
        s(&other_session, "session_id"),
        "simulated",
        "learner_statement",
        &json!({"text":STOP}),
    )?;
    assert_eq!(
        f.apply_step(
            json!(STOP),
            &[current.clone(), s(&foreign, "event_id").to_owned()]
        )?["task_updates"],
        0
    );
    f.store
        .db
        .execute("DELETE FROM events WHERE event_id=?", [&current])?;
    assert_eq!(f.apply_step(json!(STOP), &[current])?["task_updates"], 0);
    Ok(())
}

#[test]
fn only_explicit_empty_pending_declarations_authorize_clearing() -> Result<()> {
    for (kind, content, value, expected) in [
        (
            "learner_statement",
            json!({"text":"Clear the plan."}),
            "",
            false,
        ),
        (
            "learner_statement",
            json!({"text":"No pending experiment.","pending_experiment":null}),
            "",
            false,
        ),
        (
            "learner_statement",
            json!({"pending_experiment":false}),
            "",
            false,
        ),
        (
            "learner_statement",
            json!({"pending_experiment":" "}),
            "",
            false,
        ),
        (
            "learner_statement",
            json!({"pending_experiment":""}),
            " ",
            false,
        ),
        (
            "learner_statement",
            json!({"text":"没有待做实验。","pending_experiment":""}),
            "",
            true,
        ),
        (
            "plan",
            json!({"accepted":false,"pending_experiment":""}),
            "",
            false,
        ),
        ("plan", json!({"pending_experiment":""}), "", false),
        (
            "plan",
            json!({"accepted":true,"pending_experiment":""}),
            "",
            true,
        ),
    ] {
        let f = Fixture::new()?;
        let before = f.seed()?;
        let id = f.event(kind, content)?;
        let applied = f.apply_step(json!(value), &[id.clone()])?;
        assert_eq!(applied["task_updates"], if expected { 1 } else { 0 });
        assert_eq!(applied["rejected_candidates"], if expected { 0 } else { 1 });
        let state = f.state()?;
        if expected {
            assert_eq!(state["pending_step"], "");
            assert_eq!(
                state["field_evidence"]["pending_step"]["source_event_ids"],
                json!([id])
            );
        } else {
            assert_eq!(state["pending_step"], before["pending_step"]);
            assert_eq!(
                state["field_evidence"]["pending_step"],
                before["field_evidence"]["pending_step"]
            );
        }
    }
    Ok(())
}

#[test]
fn direct_store_defense_rejects_nontext_pending_values_without_discarding_other_candidates()
-> Result<()> {
    let f = Fixture::new()?;
    let id = f.event(
        "learner_statement",
        json!({"text":STOP,"pending_experiment":""}),
    )?;
    // Bypass validate_reply only to exercise the Store-side defense, as existing stage tests do.
    let reply = f.value(
        json!([
            {"field":"pending_step","value":null,"source_event_ids":[id]},
            {"field":"goal","value":"让小车停止移动","source_event_ids":[id]}
        ]),
        &[id],
    );
    let applied = apply_reply(&f.store, &f.sid, &reply, 0)?;
    assert_eq!(applied["task_updates"], 1);
    assert_eq!(applied["rejected_candidates"], 1);
    assert_eq!(f.state()?["goal"], "让小车停止移动");
    Ok(())
}

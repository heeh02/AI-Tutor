//! Legacy fast-state projection: real temporary SQLite, no model or device.
use ai_tutor::{
    context::build_context,
    store::{Store, s},
};
use anyhow::Result;
use serde_json::{Value, json};

const STOP: &str = "先让小车停下来。";
// Observed stored counterexample, not a retained raw provider candidate.
const EXPANDED: &str = "先在不靠近运动部件的情况下，使用设备设计的停止按钮或电源开关停车；若做不到，请现场熟悉设备的人协助。";
const QUESTION: &str = "What should we do now?";

struct Fixture {
    store: Store,
    lid: String,
    sid: String,
    _dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("pending-projection.db"), "simulator")?;
        let lid = s(
            &store.create_learner("SYNTHETIC_PENDING_PROJECTION", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        Ok(Self {
            store,
            lid,
            sid,
            _dir: dir,
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
    fn legacy_pending(&self, value: Value, ids: &[String]) -> Result<()> {
        let mut state = self.store.session(&self.sid)?["state"].clone();
        state["pending_step"] = value;
        state["pending_step_source"] = json!("simulated");
        state["field_evidence"]["pending_step"] = json!({"source_event_ids":ids,"status":"model_interpreted_report","hardware_verified":false,"historical":false,"original_source_type":"simulated"});
        state["latest_input"] = json!(STOP);
        state["last_delivered_hint"] = json!(EXPANDED);
        self.store.checkpoint(&self.sid, &state)
    }
    fn raw_state(&self) -> Result<String> {
        Ok(self.store.db.query_row(
            "SELECT state FROM sessions WHERE session_id=?",
            [&self.sid],
            |row| row.get(0),
        )?)
    }
    fn context(&self) -> Result<Value> {
        let id = self.event("learner_statement", json!({"text":QUESTION}))?;
        let mut session = self.store.session(&self.sid)?;
        // A stale/forged display annotation is not authority for model context.
        session["pending_step_needs_confirmation"] = json!(false);
        build_context(
            &self.store,
            &session,
            &self.store.learner(&self.lid)?,
            QUESTION,
            &id,
            &json!({}),
        )
    }
    fn assert_omitted(&self) -> Result<()> {
        let before = self.raw_state()?;
        let session = self.store.session(&self.sid)?;
        assert_eq!(session["pending_step_needs_confirmation"], true);
        let context = self.context()?;
        assert!(context["task"].get("pending_step").is_none());
        assert!(context["task"].get("pending_step_source").is_none());
        assert!(
            context["task"]["field_evidence"]
                .get("pending_step")
                .is_none()
        );
        assert_eq!(context["task"]["pending_step_needs_confirmation"], true);
        for evidence in context["evidence"].as_array().unwrap() {
            assert!(
                !evidence["content_reference"]["task_fields"]
                    .as_array()
                    .is_some_and(|fields| fields.iter().any(|f| f == "pending_step"))
            );
        }
        assert_eq!(self.raw_state()?, before);
        assert_eq!(session["state"], serde_json::from_str::<Value>(&before)?);
        Ok(())
    }
}

#[test]
fn preserved_counterexample_is_not_a_current_task_claim_but_original_and_advice_survive()
-> Result<()> {
    let f = Fixture::new()?;
    let source = f.event("learner_statement", json!({"text":STOP}))?;
    f.legacy_pending(json!(EXPANDED), &[source.clone()])?;
    let before_source = f.store.source_events(&f.lid, &[source.clone()])?;
    let before_state = f.raw_state()?;
    f.assert_omitted()?;
    let context = f.context()?;
    assert_eq!(context["task"]["last_delivered_hint"], EXPANDED);
    assert_eq!(f.store.session(&f.sid)?["state"]["latest_input"], STOP);
    assert_eq!(f.store.source_events(&f.lid, &[source])?, before_source);
    assert_eq!(f.raw_state()?, before_state);
    // Ending the activity still does not promote the unsupported stored step.
    f.store.consolidate_and_finish(&f.sid)?;
    assert!(f.store.memories(&f.lid)?.is_empty());
    assert_eq!(f.store.session(&f.sid)?["state"]["pending_step"], EXPANDED);
    assert_eq!(
        f.store.session(&f.sid)?["pending_step_needs_confirmation"],
        true
    );
    Ok(())
}

#[test]
fn source_exact_current_requests_plans_and_declarations_project_without_future_regex() -> Result<()>
{
    for (kind, content, value) in [
        ("learner_statement", json!({"text":STOP}), STOP),
        (
            "learner_statement",
            json!({"text":" \nStop the rover first.\t"}),
            "Stop the rover first.",
        ),
        (
            "plan",
            json!({"accepted":true,"text":"Measure 10 cm."}),
            "Measure 10 cm.",
        ),
        (
            "observation",
            json!({"report":"No measurement yet.","pending_experiment":"Check the wheel."}),
            "Check the wheel.",
        ),
    ] {
        let f = Fixture::new()?;
        let id = f.event(kind, content)?;
        f.legacy_pending(json!(value), &[id.clone()])?;
        let before = f.raw_state()?;
        let session = f.store.session(&f.sid)?;
        assert_eq!(session["pending_step_needs_confirmation"], false);
        let context = f.context()?;
        assert_eq!(context["task"]["pending_step"], value);
        assert_eq!(
            context["task"]["field_evidence"]["pending_step"]["source_event_ids"],
            json!([id])
        );
        assert_eq!(context["task"]["pending_step_source"], "simulated");
        assert!(
            context["task"]
                .get("pending_step_needs_confirmation")
                .is_none()
        );
        assert_eq!(f.raw_state()?, before);
    }
    Ok(())
}

#[test]
fn missing_deleted_mixed_and_unaccepted_sources_are_not_projected() -> Result<()> {
    for mode in [
        "missing",
        "deleted",
        "inactive",
        "mixed",
        "unaccepted",
        "unknown_acceptance",
    ] {
        let f = Fixture::new()?;
        let first = f.event("learner_statement", json!({"text":STOP}))?;
        let mut ids = vec![first.clone()];
        match mode {
            "missing" => ids.clear(),
            "deleted" => {
                f.store
                    .db
                    .execute("DELETE FROM events WHERE event_id=?", [&first])?;
            }
            "inactive" => {
                f.store.db.execute(
                    "UPDATE education_event_meta SET status='invalidated' WHERE event_id=?",
                    [&first],
                )?;
            }
            "mixed" => ids.push(f.event(
                "learner_statement",
                json!({"text":"Do not start the motor."}),
            )?),
            "unaccepted" => ids.push(f.event("plan", json!({"text":STOP,"accepted":false}))?),
            "unknown_acceptance" => ids.push(f.event("plan", json!({"text":STOP}))?),
            _ => unreachable!(),
        }
        f.legacy_pending(json!(STOP), &ids)?;
        f.assert_omitted()?;
    }
    Ok(())
}

#[test]
fn malformed_source_metadata_cannot_be_reduced_to_a_matching_subset() -> Result<()> {
    let f = Fixture::new()?;
    let id = f.event("learner_statement", json!({"text":STOP}))?;
    for ids in [
        json!([id, null]),
        json!([id, ""]),
        json!([id, false]),
        json!(id),
        json!(vec![id.clone(); 9]),
    ] {
        f.legacy_pending(json!(STOP), &[id.clone()])?;
        let mut state = f.store.session(&f.sid)?["state"].clone();
        state["field_evidence"]["pending_step"]["source_event_ids"] = ids;
        f.store.checkpoint(&f.sid, &state)?;
        f.assert_omitted()?;
    }
    Ok(())
}

#[test]
fn same_learner_and_owning_session_are_checked_for_every_source() -> Result<()> {
    let mut f = Fixture::new()?;
    let old = f.event("learner_statement", json!({"text":STOP}))?;
    f.sid = s(&f.store.create_session(&f.lid)?, "session_id").to_owned();
    let current = f.event("learner_statement", json!({"text":STOP}))?;
    f.legacy_pending(json!(STOP), &[current.clone(), old])?;
    f.assert_omitted()?;
    let other_lid = s(
        &f.store.create_learner("OTHER_SYNTHETIC", "zh", "guided")?,
        "learner_id",
    )
    .to_owned();
    let other_sid = s(&f.store.create_session(&other_lid)?, "session_id").to_owned();
    let foreign = f.store.record_education_event(
        &other_lid,
        &other_sid,
        "simulated",
        "learner_statement",
        &json!({"text":STOP}),
    )?;
    f.legacy_pending(json!(STOP), &[current, s(&foreign, "event_id").to_owned()])?;
    f.assert_omitted()?;
    Ok(())
}

#[test]
fn empty_default_and_existing_explicit_clear_remain_empty_without_becoming_read_mutations()
-> Result<()> {
    let f = Fixture::new()?;
    let before = f.raw_state()?;
    assert_eq!(
        f.store.session(&f.sid)?["pending_step_needs_confirmation"],
        false
    );
    let context = f.context()?;
    assert_eq!(context["task"]["pending_step"], "");
    assert!(
        context["task"]
            .get("pending_step_needs_confirmation")
            .is_none()
    );
    assert_eq!(f.raw_state()?, before);
    let declaration = f.event("learner_statement", json!({"pending_experiment":""}))?;
    f.legacy_pending(json!(""), &[declaration])?;
    let before_clear = f.raw_state()?;
    assert_eq!(
        f.store.session(&f.sid)?["pending_step_needs_confirmation"],
        false
    );
    assert_eq!(f.context()?["task"]["pending_step"], "");
    assert_eq!(f.raw_state()?, before_clear);
    f.legacy_pending(json!(" "), &[])?;
    f.assert_omitted()?;
    Ok(())
}

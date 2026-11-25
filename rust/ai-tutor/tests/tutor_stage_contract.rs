//! Static stage domain matches the model-visible schema; live Store guards remain.
use ai_tutor::{
    store::{Store, s},
    tutor::{STAGES, apply_reply, validate_reply},
};
use anyhow::Result;
use serde_json::{Value, json};

fn request(rid: &str, eid: &str) -> Value {
    json!({"request_id":rid,"context":{"evidence":[{"event_id":eid}]}})
}
fn response(rid: &str, eid: &str, stage: &str) -> Value {
    // Candidate shape from the frozen physical static_measurement_en response.
    // Test identities are synthetic; the original result file is never changed.
    json!({"schema_version":"1.0","request_id":rid,
        "reply_text":"Use a fixed reference point and record the distance with units.",
        "teaching_move":"answer","used_evidence_ids":[eid],"needs_clarification":false,
        "memory_candidates":[],"task_update_candidates":[
            {"field":"goal","value":"Measure how far the stationary rover's stopping position is from the marked line.","source_event_ids":[eid]},
            {"field":"stage","value":stage,"source_event_ids":[eid]},
            {"field":"reported_result","value":"Learner reports that the power is off and the wheels are no longer turning; the distance has not yet been reported.","source_event_ids":[eid]}]})
}

#[test]
fn recorded_unknown_stage_rejects_generation_and_store_defense_still_skips_only_that_candidate()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("stage-contract.sqlite3"), "simulator")?;
    let learner = store.create_learner("SYNTHETIC_STAGE_CONTRACT", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let event = store.record_education_event(lid, sid, "simulated", "learner_statement",
        &json!({"text":"I have switched the power off, and the wheels are no longer turning. I only want to measure how far its stopping position is from the marked line."}))?;
    let eid = s(&event, "event_id");
    let reply = response("stage-contract", eid, "static_observation");
    let error = validate_reply(&reply, &request("stage-contract", eid)).unwrap_err();
    assert_eq!(error.to_string(), "教学阶段未允许");
    assert_eq!(
        reply["task_update_candidates"][1]["value"],
        "static_observation"
    );
    // Deliberately bypass validation only to exercise the unchanged Store defense.
    let applied = apply_reply(&store, sid, &reply, learner["epoch"].as_i64().unwrap())?;
    assert_eq!(applied["task_updates"], 2);
    assert_eq!(applied["rejected_candidates"], 1);
    let state = store.session(sid)?["state"].clone();
    assert_eq!(state["stage"], "goal_setting");
    assert_eq!(state["reported_result_source"], "simulated");
    assert_eq!(
        state["reported_result"],
        reply["task_update_candidates"][2]["value"]
    );
    Ok(())
}

#[test]
fn exact_stage_domain_is_accepted_without_constraining_other_text_fields() -> Result<()> {
    let request = request("stage-contract", "e");
    for stage in STAGES {
        let mut reply = response("stage-contract", "e", stage);
        reply["task_update_candidates"][0]["value"] =
            json!("static_observation is arbitrary goal text here");
        validate_reply(&reply, &request)?;
    }
    for stage in ["static_observation", "", "OBSERVATION", "observation "] {
        assert!(validate_reply(&response("stage-contract", "e", stage), &request).is_err());
    }
    Ok(())
}

#[test]
fn model_visible_stage_enum_matches_the_static_business_domain() -> Result<()> {
    let schema: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"
    )))?;
    let item = &schema["properties"]["task_update_candidates"]["items"];
    let rule = &item["allOf"][0];
    assert_eq!(rule["if"]["properties"]["field"]["const"], "stage");
    assert_eq!(rule["if"]["required"], json!(["field"]));
    assert_eq!(rule["then"]["properties"]["value"]["enum"], json!(STAGES));
    assert_eq!(item["properties"]["value"]["type"], "string");
    assert_eq!(item["properties"]["value"]["maxLength"], 800);
    Ok(())
}

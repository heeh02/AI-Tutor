use super::*;
use crate::tutor::validate_reply;
use serde_json::json;

const EVENT: &str = "event_11111111111111111111111111111111";
const UNKNOWN: &str = "event_22222222222222222222222222222222";
const MEMORY: &str = "memory_33333333333333333333333333333333";
const SESSION: &str = "session_44444444444444444444444444444444";

fn request() -> Value {
    json!({"request_id":"citation-test","session_id":SESSION,"generation":7,
        "context":{"evidence":[{"event_id":EVENT}],"slow_memory":[{"memory_id":MEMORY,"source_event_ids":[EVENT]}]}})
}
fn reply() -> Value {
    json!({"schema_version":"1.0","request_id":"citation-test","reply_text":"A synthetic answer.",
        "teaching_move":"answer","used_evidence_ids":[EVENT],"needs_clarification":false,
        "task_update_candidates":[],"memory_candidates":[]})
}

#[test]
fn three_reply_entry_points_identify_the_failing_field_and_reference() -> Result<()> {
    for (field, bad, shape) in [
        ("used_evidence_ids", MEMORY, "memory_id"),
        (
            "task_update_candidates.source_event_ids",
            SESSION,
            "session_id",
        ),
        ("memory_candidates.source_event_ids", UNKNOWN, "event_id"),
    ] {
        let mut output = reply();
        let ids = json!([EVENT, bad]);
        if field == "used_evidence_ids" {
            output[field] = ids;
        } else if field.starts_with("task") {
            output["task_update_candidates"] = json!([
                {"field":"goal","value":"Valid first candidate","source_event_ids":[EVENT]},
                {"field":"pending_step","value":"Not applied","source_event_ids":ids}]);
        } else {
            output["memory_candidates"] = json!([
                {"kind":"reported_progress","scope":"current_activity","content":"Valid first candidate","source_event_ids":[EVENT]},
                {"kind":"reported_progress","scope":"current_activity","content":"Not applied","source_event_ids":ids}]);
        }
        let error = validate_reply(&output, &request()).unwrap_err();
        assert_eq!(error.to_string(), "模型引用了本轮不可用的来源");
        let value = diagnostic(&error).unwrap();
        assert_eq!(value["field"], field);
        assert_eq!(
            value["candidate_index"],
            if field == "used_evidence_ids" {
                Value::Null
            } else {
                json!(1)
            }
        );
        assert_eq!(value["reference_count"], 2);
        assert_eq!(value["invalid_reference_count"], 1);
        assert_eq!(value["count_scope"], "failing_field_only");
        assert_eq!(value["invalid_references"][0]["reference_index"], 1);
        assert_eq!(value["invalid_references"][0]["id_shape"], shape);
        assert_eq!(
            value["invalid_references"][0]["matches_request_non_event_identity"],
            bad != UNKNOWN
        );
        for id in [EVENT, UNKNOWN, MEMORY, SESSION] {
            assert!(!value.to_string().contains(id));
        }
    }
    Ok(())
}

#[test]
fn malicious_values_and_wrapped_errors_export_no_content_or_error_context() {
    let mut output = reply();
    output["used_evidence_ids"] =
        json!([EVENT, "PRIVATE_TOKEN /secret\nbody", {"PRIVATE_OBJECT":"PRIVATE_VALUE"}, ""]);
    let error = validate_reply(&output, &request())
        .unwrap_err()
        .context("PRIVATE_ERROR_CONTEXT");
    let value = diagnostic(&error).unwrap();
    assert_eq!(value["invalid_reference_count"], 3);
    assert_eq!(value["invalid_references"][0]["id_shape"], "other_string");
    assert_eq!(value["invalid_references"][1]["id_shape"], "non_string");
    assert_eq!(value["invalid_references"][2]["id_shape"], "empty_string");
    assert!(!value.to_string().contains("PRIVATE"));
    assert!(!value.to_string().contains("secret"));
    output["used_evidence_ids"] = json!([{"PRIVATE_OBJECT":true}, UNKNOWN]);
    let error = validate_reply(&output, &request()).unwrap_err();
    assert_eq!(error.to_string(), "模型来源ID无效");
    assert_eq!(diagnostic(&error).unwrap()["code"], "citation_id_type");
    assert!(diagnostic(&anyhow::anyhow!("PRIVATE_OTHER_ERROR")).is_none());
}

#[test]
fn strict_shapes_do_not_accept_lookalikes_or_treat_prose_as_identity() {
    for value in [
        "event_deadbeef",
        "event_11111111111111111111111111111111tail",
        "event_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "event_１１１１１１１１１１１１１１１１１１１１１１１１１１１１１１１１",
    ] {
        assert_eq!(id_shape(&json!(value)), "other_string");
    }
    let mut request = request();
    request["context"]["user_input"] = json!(UNKNOWN);
    assert!(!matches_identity(&request, UNKNOWN));
    assert!(matches_identity(&request, MEMORY));
    assert!(matches_identity(&request, SESSION));
}

#[test]
fn original_acceptance_and_cardinality_limits_remain_unchanged() -> Result<()> {
    let request = request();
    let mut output = reply();
    output["used_evidence_ids"] = json!([EVENT, EVENT]); // Existing whole-reply contract allows duplicates.
    validate_reply(&output, &request)?;
    output["used_evidence_ids"] = json!([]);
    validate_reply(&output, &request)?;
    output["used_evidence_ids"] = json!(vec![UNKNOWN; 20]);
    let error = validate_reply(&output, &request).unwrap_err();
    let value = diagnostic(&error).unwrap();
    assert_eq!(value["invalid_references"].as_array().unwrap().len(), 20);
    assert_eq!(value["invalid_reference_count"], 20);
    assert!(value.to_string().len() < 3000);
    output["used_evidence_ids"] = json!(vec![UNKNOWN; 21]);
    let error = validate_reply(&output, &request).unwrap_err();
    assert_eq!(error.to_string(), "模型来源数量无效");
    assert!(diagnostic(&error).is_none()); // Schema/cardinality failure is not falsely a scanned citation set.
    output["used_evidence_ids"] = json!("PRIVATE_NOT_ARRAY");
    let error = validate_reply(&output, &request).unwrap_err();
    assert_eq!(error.to_string(), "模型来源字段无效");
    assert!(diagnostic(&error).is_none());
    Ok(())
}

#[test]
fn diagnostics_do_not_change_a_legacy_available_non_uuid_source_id() -> Result<()> {
    let request =
        json!({"request_id":"citation-test","context":{"evidence":[{"event_id":"legacy-source"}]}});
    let mut output = reply();
    output["used_evidence_ids"] = json!(["legacy-source"]);
    validate_reply(&output, &request)?;
    Ok(())
}

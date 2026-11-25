use super::*;
use ai_tutor::store::s;
use std::collections::HashSet;

#[test]
fn actual_48_promotions_preserve_old_inputs_without_claiming_new_coverage_success() -> Result<()> {
    let frozen = load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))?;
    let projected = project(&frozen)?;
    ensure!(
        projected.summary["projected_count"] == 48
            && projected.summary["excluded_count"] == 12
            && projected.inputs.len() == 240
    );
    ensure!(projected.summary["kind_counts"]["context_constraint"] == 6);
    ensure!(
        projected.summary["legacy42_text_and_metadata_equal"] == 42
            && projected.summary["legacy192_query_inputs_equal"] == 192
    );
    ensure!(projected.summary["availability"]["overall"]["original_positive"] == 72);
    ensure!(projected.summary["availability"]["overall"]["original_negative"] == 24);
    ensure!(projected.summary["availability"]["overall"]["typed48_available_positive"] == 60);
    ensure!(
        projected.summary["availability"]["cross_language"]["typed48_available_positive"] == 18
    );
    ensure!(projected.summary["availability"]["mixed_multi"]["typed48_available_positive"] == 11);
    ensure!(
        projected
            .decisions
            .iter()
            .filter(|d| d["projected"] == false)
            .all(|d| d.get("encoding_text").is_none())
    );
    for d in projected
        .decisions
        .iter()
        .filter(|d| d["projection_kind"] == "context_constraint")
    {
        ensure!(
            d["live_imported_event"]["event_kind"] == "learner_statement"
                && d["memory_evidence"]["education_metadata"]["support_level"] == "unknown"
                && d["memory_evidence"]["memory"]["kind"] == "context_constraint"
        );
    }
    projected.verify_live()
}

#[test]
fn declared_mapping_never_relabels_constraints_plans_or_temporary_requests() -> Result<()> {
    let frozen = load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))?;
    let source = frozen
        .sources
        .iter()
        .find(|s| s["claim_type"] == "condition_limit")
        .unwrap();
    ensure!(kind(source) == Some("context_constraint"));
    for (field, value) in [
        ("event_kind", "learner_answer"),
        ("event_kind", "plan"),
        ("support_level", "hinted"),
    ] {
        let mut wrong = source.clone();
        wrong[field] = json!(value);
        ensure!(kind(&wrong).is_none());
    }
    ensure!(
        frozen
            .sources
            .iter()
            .filter(|v| ["unaccepted_plan", "temporary_request"].contains(&s(v, "claim_type")))
            .all(|v| kind(v).is_none())
    );
    Ok(())
}

#[test]
fn availability_keeps_complete_original_alternatives_instead_of_trimming() {
    let available = HashSet::from(["a".to_owned(), "c".to_owned()]);
    assert!(!complete_set_available(&json!([["a", "b"]]), &available));
    assert!(complete_set_available(
        &json!([["a", "b"], ["c"]]),
        &available
    ));
    assert!(!complete_set_available(&json!([]), &available));
    assert!(!complete_set_available(&json!([[]]), &available));
}

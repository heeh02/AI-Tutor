use super::*;

#[test]
fn fixed_cases_cover_four_scenarios_in_both_languages_without_extra_attempts() -> Result<()> {
    let rows = cases()?;
    ensure!(rows.len() == 8);
    let mut pairs = std::collections::HashSet::new();
    for case in &rows {
        ensure!(
            [
                "unknown_stop",
                "remote_stop",
                "static_measurement",
                "workbench_stop"
            ]
            .contains(&s(case, "scenario"))
        );
        ensure!(["zh", "en"].contains(&s(case, "language")));
        ensure!(pairs.insert((s(case, "scenario"), s(case, "language"))));
        ensure!(!s(case, "text").is_empty());
    }
    let prepared: Vec<_> = rows.iter().map(|c| json!({"case_id":c["id"]})).collect();
    ensure!(selected(&prepared, None)? == (0..8).collect::<Vec<_>>());
    for (index, row) in prepared.iter().enumerate() {
        ensure!(selected(&prepared, Some(s(row, "case_id")))? == vec![index]);
    }
    ensure!(selected(&prepared, Some("unknown")).is_err());
    Ok(())
}

#[test]
fn real_store_contexts_keep_only_current_source_and_no_review_or_hardware_injection() -> Result<()>
{
    for case in cases()? {
        let (row, review) = build_case(&case)?;
        let context = &row["request"]["context"];
        ensure!(context["user_input"] == case["text"] && context["language"] == case["language"]);
        ensure!(
            context["namespace"] == "simulator" && context["current_hardware_status"] == "unknown"
        );
        ensure!(
            context["task"]["goal"] == ""
                && context["task"]["reported_result"] == ""
                && context["task"]["modules"].is_null()
        );
        ensure!(context["task"]["requires_inventory_confirmation"] == true);
        ensure!(context["evidence"].as_array().unwrap().len() == 1);
        ensure!(
            review["actual_source"]["source_type"] == "simulated"
                && review["actual_source"]["content"] == json!({"text":case["text"]})
        );
        ensure!(context["current_user_event_id"] == review["actual_source"]["event_id"]);
        ensure!(review["actual_session"]["session_id"] == row["request"]["session_id"]);
        ensure!(
            context["slow_memory"] == json!([])
                && context["recent_delivered_dialogue"] == json!([])
        );
        ensure!(row["request"].get("review").is_none() && context.get("scenario").is_none());
        ensure!(context.to_string().chars().count() <= 18_000);
    }
    Ok(())
}

#[test]
fn failed_attempt_is_not_retried_and_other_unattempted_case_remains_selectable() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let rows = vec![json!({"case_id":"one"}), json!({"case_id":"two"})];
    preflight_attempts(dir.path(), &rows, &[0, 1])?;
    write_new(
        &dir.path().join("attempt-one.json"),
        &json!({"terminal":"protocol_failed"}),
    )?;
    ensure!(preflight_attempts(dir.path(), &rows, &[0, 1]).is_err());
    ensure!(preflight_attempts(dir.path(), &rows, &selected(&rows, Some("one"))?).is_err());
    preflight_attempts(dir.path(), &rows, &selected(&rows, Some("two"))?)?;
    ensure!(
        write_new(
            &dir.path().join("attempt-one.json"),
            &json!({"overwrite":true})
        )
        .is_err()
    );
    Ok(())
}

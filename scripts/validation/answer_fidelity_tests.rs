use super::*;

#[test]
fn fixed_fidelity_cases_cover_five_bilingual_groups_and_ten_first_attempts() -> Result<()> {
    let rows = cases()?;
    ensure!(rows.len() == 10);
    let mut pairs = std::collections::HashSet::new();
    for case in &rows {
        ensure!(
            [
                "difference_help",
                "original_error",
                "explicit_negative",
                "actual_vs_plan",
                "unreported_measurement"
            ]
            .contains(&s(case, "group"))
        );
        ensure!(["zh", "en"].contains(&s(case, "language")));
        ensure!(pairs.insert((s(case, "group"), s(case, "language"))));
        ensure!(!s(case, "text").is_empty());
        ensure!(
            case["history"].as_array().unwrap().is_empty()
                == (case["group"] == "unreported_measurement")
        );
    }
    let method: Value = serde_json::from_str(METHOD)?;
    ensure!(method["declared_cases"] == 10 && method["model_attempt_ceiling"] == 10);
    ensure!(method["attempts_per_case"] == 1 && method["automatic_retries"] == 0);
    Ok(())
}

#[test]
fn actual_store_continuation_keeps_source_text_and_new_fast_state_separate() -> Result<()> {
    for case in cases()? {
        let (row, review) = build_case(&case)?;
        let context = &row["request"]["context"];
        ensure!(context["user_input"] == case["text"] && context["language"] == case["language"]);
        ensure!(
            context["namespace"] == "simulator" && context["current_hardware_status"] == "unknown"
        );
        ensure!(
            context["task"]["reported_result"] == "" && context["task"]["attempts"] == json!([])
        );
        ensure!(context["task"]["stage"] == "goal_setting" && context["task"]["modules"].is_null());
        ensure!(review["declared_label"] == "positive_fidelity");
        ensure!(row["request"].get("preparation").is_none() && context.get("review").is_none());
        let history = case["history"].as_array().unwrap();
        ensure!(review["stored_memories"].as_array().unwrap().len() == history.len());
        if !history.is_empty() {
            ensure!(review["actual_source_session"]["status"] == "finished");
            ensure!(
                context["continuation"]["source_session_id"]
                    == review["actual_source_session"]["session_id"]
            );
            ensure!(row["request"]["session_id"] != review["actual_source_session"]["session_id"]);
        }
        for (source, original) in review["actual_sources_and_memories"]
            .as_array()
            .unwrap()
            .iter()
            .zip(history)
        {
            ensure!(source["event"]["content"] == original["content"]);
            ensure!(source["event"]["source_type"] == "simulated");
            let stored = review["stored_memories"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["memory_id"] == source["memory_id"])
                .unwrap();
            ensure!(stored["content"] == original["content"]["text"]);
            ensure!(stored["source_event_ids"] == json!([source["event"]["event_id"]]));
        }
        // If production recall ever drops a source, preparation records it;
        // this construction test must not rewrite the question or positive label.
        let complete = review["source_coverage"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|check| check["covered"] == true);
        ensure!(row["preparation"]["coverage_complete"] == complete);
    }
    Ok(())
}

#[test]
fn unselected_history_is_preserved_as_insufficient_and_cannot_be_called_or_retried() -> Result<()> {
    let cases = cases()?;
    let historical = cases
        .iter()
        .find(|case| case["id"] == "difference_help_zh")
        .unwrap();
    let (missing, review) = build_case_with_selection(historical, false)?;
    ensure!(missing["preparation"]["status"] == "prepared_insufficient");
    ensure!(review["declared_label"] == "positive_fidelity");
    ensure!(missing["request"]["context"]["slow_memory"] == json!([]));
    for source in review["actual_sources_and_memories"].as_array().unwrap() {
        if source["memory_id"].is_string() {
            ensure!(
                !missing["request"]
                    .to_string()
                    .contains(s(&source["event"], "event_id"))
            );
        }
    }
    let current = cases
        .iter()
        .find(|case| case["id"] == "unreported_measurement_en")
        .unwrap();
    let (ready, _) = build_case(current)?;
    let rows = vec![missing, ready];
    let (eligible, insufficient) = eligible_indices(&rows, &[0, 1]);
    ensure!(eligible == vec![1] && insufficient == vec![0]);
    let dir = tempfile::tempdir()?;
    preflight_attempts(dir.path(), &rows, &[0, 1])?;
    write_new(
        &dir.path().join("not-run-difference_help_zh.json"),
        &json!({"status":"prepared_insufficient"}),
    )?;
    ensure!(preflight_attempts(dir.path(), &rows, &[0]).is_err());
    preflight_attempts(dir.path(), &rows, &[1])?;
    write_new(
        &dir.path().join("attempt-unreported_measurement_en.json"),
        &json!({"status":"failed"}),
    )?;
    ensure!(preflight_attempts(dir.path(), &rows, &[1]).is_err());
    ensure!(
        write_new(
            &dir.path().join("attempt-unreported_measurement_en.json"),
            &json!({"overwrite":true})
        )
        .is_err()
    );
    Ok(())
}

use super::*;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn visit_keys(value: &Value, found: &mut HashSet<String>) {
    match value {
        Value::Object(rows) => {
            for (key, value) in rows {
                found.insert(key.clone());
                visit_keys(value, found);
            }
        }
        Value::Array(rows) => {
            for value in rows {
                visit_keys(value, found);
            }
        }
        _ => {}
    }
}
fn contains_text(value: &Value, text: &str) -> bool {
    match value {
        Value::String(value) => value.contains(text),
        Value::Object(values) => values.values().any(|v| contains_text(v, text)),
        Value::Array(values) => values.iter().any(|v| contains_text(v, text)),
        _ => false,
    }
}

#[test]
fn frozen36_preserves_original_labels_and_distinguishes_retrieval_misses() -> Result<()> {
    let suite = suite(&root())?;
    ensure!(suite.requests.len() == 36 && suite.review.len() == 36);
    let group = |name| suite.review.iter().filter(|v| v["group"] == name).count();
    ensure!(group("negative_original_label") == 24);
    ensure!(group("positive_sufficient_retrieved") == 9);
    ensure!(group("positive_insufficient_retrieved") == 3);
    let missing: Vec<_> = suite
        .review
        .iter()
        .filter(|v| v["group"] == "positive_insufficient_retrieved")
        .map(|v| s(v, "query_id"))
        .collect();
    ensure!(missing == ["germination_q06", "evaporation_q06", "evaporation_q08"]);
    ensure!(
        suite
            .review
            .iter()
            .filter(|v| v["group"] == "positive_insufficient_retrieved")
            .all(|v| v["original_label"]["answerable"] == true)
    );
    Ok(())
}

#[test]
fn only_selected_snapshot_content_and_versions_enter_teacher_context() -> Result<()> {
    let root = root();
    let suite = suite(&root)?;
    let queries = unique(
        records(&read(&root.join(FIXTURE).join("queries.jsonl"))?)?,
        "id",
    )?;
    let sources = unique(
        records(&read(&root.join(FIXTURE).join("sources.jsonl"))?)?,
        "id",
    )?;
    for (row, review) in suite.requests.iter().zip(&suite.review) {
        let request = &row["request"];
        let context = &request["context"];
        let q = &queries[s(row, "query_id")];
        ensure!(context["user_input"] == q["text"] && context["task"]["goal"] == q["goal"]);
        ensure!(
            context["task"]["goal_source"] == "synthetic_fixture_context"
                && context["task"]["field_evidence"] == json!({})
        );
        ensure!(
            context["namespace"] == "simulator" && context["current_hardware_status"] == "unknown"
        );
        let memories = context["slow_memory"].as_array().unwrap();
        let evidence = context["evidence"].as_array().unwrap();
        ensure!(memories.len() == 5 && evidence.len() == 6);
        ensure!(
            memories
                .iter()
                .map(|m| s(m, "content").chars().count())
                .sum::<usize>()
                <= 2400
        );
        let selected: HashSet<_> = strings(&review["retrieved_source_ids"])
            .into_iter()
            .collect();
        for (id, source) in &sources {
            if !selected.contains(id) {
                ensure!(
                    !contains_text(request, s(source, "text")),
                    "Unretrieved source leaked"
                );
            }
        }
        for mapping in review["source_mapping"].as_array().unwrap() {
            let memory = memories
                .iter()
                .find(|m| m["memory_id"] == mapping["memory_id"])
                .unwrap();
            let event = evidence
                .iter()
                .find(|e| e["event_id"] == mapping["event_id"])
                .unwrap();
            ensure!(
                memory["content"]
                    .as_str()
                    .unwrap()
                    .ends_with(s(&sources[s(mapping, "source_id")], "text"))
            );
            ensure!(memory["education_metadata"].is_object());
            ensure!(
                memory["source_event_versions"][s(mapping, "event_id")] == mapping["event_version"]
            );
            ensure!(
                event["content_reference"]["memory_id"] == mapping["memory_id"]
                    && event.get("content").is_none()
            );
            ensure!(
                event["source_type"] == "simulated"
                    && event["event_status"] == "active"
                    && event["event_version"] == mapping["event_version"]
            );
        }
        let mut keys = HashSet::new();
        visit_keys(request, &mut keys);
        for forbidden in [
            "source_id",
            "query_id",
            "original_label",
            "claim_checks",
            "sufficient_evidence_sets",
            "forbidden_claims",
            "no_evidence_reason",
            "score",
            "cosine",
            "relevance_status",
            "quality_pass",
        ] {
            ensure!(
                !keys.contains(forbidden),
                "Review field reached model: {forbidden}"
            );
        }
    }
    Ok(())
}

#[test]
fn valid_in_request_citations_are_not_automatically_semantic_success() -> Result<()> {
    let suite = suite(&root())?;
    let review = &suite.review[2]; // germination_q13, a frozen corpus-negative.
    let request = &suite.requests[2]["request"];
    let reply = json!({"schema_version":"1.0","request_id":request["request_id"],
        "reply_text":"The learner scored 99 on the science exam.","teaching_move":"answer",
        "task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[review["source_mapping"][0]["event_id"]],"needs_clarification":false});
    // Existing production validation accepts the available ID. The diagnostic
    // must retain this unsupported-looking statement for content review.
    ai_tutor::tutor::validate_reply(&reply, request)?;
    let check = citation_check(&reply, request, review);
    ensure!(
        check["semantic_review_required"] == true
            && check["citations_cover_complete_original_set"].is_null()
    );
    ensure!(check.get("semantic_quality_pass").is_none());
    Ok(())
}

#[test]
fn fixed_batch_and_single_case_selection_cannot_expand_the36() -> Result<()> {
    let method: Value = serde_json::from_str(METHOD)?;
    ensure!(selected_indices(&method, &Selection::Batch(1))? == (0..6).collect::<Vec<_>>());
    ensure!(selected_indices(&method, &Selection::Batch(6))? == (30..36).collect::<Vec<_>>());
    ensure!(selected_indices(&method, &Selection::Case("germination_q13".into()))? == vec![2]);
    ensure!(selected_indices(&method, &Selection::Case("germination_q01".into())).is_err());
    ensure!(selected_indices(&method, &Selection::Batch(0)).is_err());
    ensure!(selected_indices(&method, &Selection::Batch(7)).is_err());
    Ok(())
}

#[test]
fn preparation_is_local_and_refuses_to_overwrite_evidence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let output = dir.path().join("prepared");
    prepare(&root(), &output)?;
    let manifest: Value = serde_json::from_slice(&read(&output.join("manifest.json"))?)?;
    ensure!(
        manifest["model_called"] == false
            && manifest["learner_database_opened"] == false
            && manifest["semantic_quality_pass"].is_null()
    );
    ensure!(fs::read_dir(&output)?.count() == 4);
    let before = read(&output.join("manifest.json"))?;
    ensure!(prepare(&root(), &output).is_err());
    ensure!(read(&output.join("manifest.json"))? == before);
    Ok(())
}

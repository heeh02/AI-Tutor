use super::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn frozen_semantic_pairs_keep_five_positive_one_negative_and_dense_then_hybrid() -> Result<()> {
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(&root())?;
    let batches = method["batches"].as_array().unwrap();
    ensure!(batches.len() == 6 && ids(&method).len() == 12);
    let original = [
        "germination_q06",
        "paper_bridge_q02",
        "fractions_q16",
        "reading_q09",
        "loops_q07",
        "evaporation_q06",
    ];
    ensure!(batches.iter().map(|b| s(b, "query_id")).eq(original));
    let mut positive = 0;
    for (index, batch) in batches.iter().enumerate() {
        let id = s(batch, "query_id");
        let label = frozen
            .labels
            .iter()
            .find(|l| s(l, "query_id") == id)
            .unwrap();
        positive += usize::from(label["answerable"] == true);
        ensure!(
            strings(&batch["case_ids"]) == [format!("{id}-dense_only"), format!("{id}-hybrid_rrf")]
        );
        ensure!(selected(&method, &Selection::Case(id.into()))? == vec![index * 2, index * 2 + 1]);
        ensure!(selected(&method, &Selection::Batch(index + 1))? == vec![index * 2, index * 2 + 1]);
    }
    ensure!(positive == 5 && selected(&method, &Selection::Batch(0)).is_err());
    ensure!(selected(&method, &Selection::Case("parent_chain".into())).is_err());
    ensure!(
        selected(
            &method,
            &Selection::Case("germination_q06-dense_only".into())
        )? == vec![0]
    );
    ensure!(
        selected(
            &method,
            &Selection::Case("germination_q06-hybrid_rrf".into())
        )? == vec![1]
    );
    let directory = tempfile::tempdir()?;
    let order = ids(&method);
    validate_attempt_selection(directory.path(), &order, &[0, 1])?;
    validate_attempt_selection(directory.path(), &order, &[0])?;
    ensure!(validate_attempt_selection(directory.path(), &order, &[1]).is_err());
    // A recorded failure still counts as the first attempt, never as permission
    // to retry it. The other arm may be explicitly attempted once afterward.
    write_new(
        &directory.path().join(format!("attempt-{}.json", order[0])),
        &json!({"status":"failed"}),
    )?;
    ensure!(validate_attempt_selection(directory.path(), &order, &[0, 1]).is_err());
    ensure!(validate_attempt_selection(directory.path(), &order, &[0]).is_err());
    validate_attempt_selection(directory.path(), &order, &[1])?;
    write_new(
        &directory.path().join(format!("attempt-{}.json", order[1])),
        &json!({"status":"attempted"}),
    )?;
    ensure!(validate_attempt_selection(directory.path(), &order, &[1]).is_err());
    Ok(())
}

#[test]
fn six_real_stores_import_32_plus_16_and_build_same_snapshot_scoped_context_pairs() -> Result<()> {
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(&root())?;
    let vectors = load_vectors(&root(), &method)?;
    for batch in method["batches"].as_array().unwrap() {
        let (requests, reviews, audit) = build_pair(&frozen, &vectors, s(batch, "query_id"))?;
        ensure!(requests.len() == 2 && reviews.len() == 2);
        ensure!(
            audit["cache_import"]["batch_sizes"] == json!([32, 16])
                && audit["cache_import"]["commits"].as_array().unwrap().len() == 48
        );
        ensure!(
            audit["cache_import"]["remaining_work"] == 0
                && audit["ranking"]["eligible_scoped_pool"]
                    .as_array()
                    .unwrap()
                    .len()
                    == 8
        );
        ensure!(
            requests[0]["request"]["session_id"] == requests[1]["request"]["session_id"]
                && requests[0]["request"]["request_id"] != requests[1]["request"]["request_id"]
        );
        ensure!(
            requests[0]["request"]["context"]["current_user_event_id"]
                == requests[1]["request"]["context"]["current_user_event_id"]
        );
        for (request, review) in requests.iter().zip(&reviews) {
            assert_no_leakage(&request["request"], &frozen)?;
            let context = &request["request"]["context"];
            ensure!(
                context["task"]["goal"] == ""
                    && context["task"]["reported_result"] == ""
                    && context["task"]["stage"] == "goal_setting"
            );
            ensure!(context["continuation"]["historical"] == true && context["language"] == "zh");
            ensure!(context["user_input"] == review["original_query"]["text"]);
            ensure!(review["candidate_top5"].as_array().unwrap().len() == 5);
            ensure!(context.to_string().chars().count() <= 18_000);
            for field in [
                "cosine",
                "ranking_score",
                "semantic_rank",
                "lexical_rank",
                "source_alias",
            ] {
                ensure!(!context.to_string().contains(field));
            }
        }
    }
    Ok(())
}

#[test]
fn fifth_place_tie_audit_keeps_unfavorable_legal_choices_and_rejects_budget_assumptions()
-> Result<()> {
    let mut rows = vec![];
    for (alias, score) in [
        ("a", 0.8),
        ("b", 0.7),
        ("c", 0.6),
        ("s06", 1.0 / 64.0),
        ("s09", 1.0 / 64.0),
        ("other", 1.0 / 64.0),
    ] {
        rows.push(json!({"source_alias":alias,"memory_id":alias,"score":score,"whole_chars":100}));
    }
    let label = json!({"answerable":true,"sufficient_evidence_sets":[["s06"]]});
    let audit = tie_audit(&rows, &label)?;
    let alternatives = audit["fifth_place_cut_group"]["alternatives"]
        .as_array()
        .unwrap();
    ensure!(alternatives.len() == 3);
    ensure!(
        alternatives
            .iter()
            .filter(|a| a["candidate_coverage"]["complete_original_set"] == true)
            .count()
            == 2
    );
    ensure!(
        alternatives
            .iter()
            .filter(|a| a["candidate_coverage"]["complete_original_set"] == false)
            .count()
            == 1
    );
    rows[0]["whole_chars"] = json!(2400);
    ensure!(tie_audit(&rows, &label).is_err());
    Ok(())
}

#[test]
fn preparation_rejects_changed_query_text_instead_of_using_an_old_vector() -> Result<()> {
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(&root())?;
    let mut vectors = load_vectors(&root(), &method)?;
    vectors
        .inputs
        .get_mut("current_query:germination_q06")
        .unwrap()["text"] = json!("different question");
    ensure!(build_pair(&frozen, &vectors, "germination_q06").is_err());
    Ok(())
}

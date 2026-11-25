use super::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn frozen_order_is_five_original_positive_one_negative_and_six_distinct_boundaries() -> Result<()> {
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(&root())?;
    let order = ids(&method);
    ensure!(
        order[..6]
            == [
                "germination_q06",
                "paper_bridge_q02",
                "fractions_q16",
                "reading_q09",
                "loops_q07",
                "evaporation_q06"
            ]
    );
    let labels: Vec<_> = order[..6]
        .iter()
        .map(|id| {
            frozen
                .labels
                .iter()
                .find(|l| s(l, "query_id") == id)
                .unwrap()
        })
        .collect();
    ensure!(labels.iter().filter(|l| l["answerable"] == true).count() == 5);
    ensure!(labels[2]["answerable"] == false);
    ensure!(selected(&method, &Selection::Batch(1))? == (0..6).collect::<Vec<_>>());
    ensure!(selected(&method, &Selection::Batch(2))? == (6..12).collect::<Vec<_>>());
    ensure!(selected(&method, &Selection::Batch(0)).is_err());
    ensure!(selected(&method, &Selection::Case("not_a_frozen_case".into())).is_err());
    Ok(())
}

#[test]
fn leakage_guard_rejects_fixture_metadata_without_rejecting_natural_source_language() -> Result<()>
{
    let frozen = typed48::load(&root())?;
    assert_no_leakage(
        &json!({"text":"The learner suggested reading the bottom of the meniscus."}),
        &frozen,
    )?;
    assert_no_leakage(&json!({"text":"reading"}), &frozen)?;
    for bad in [
        json!({"fixture_source_id":"opaque"}),
        json!({"pack_id":"reading"}),
        json!({"text":"See germination_s06"}),
        json!({"text":"reading_q09"}),
        json!({"label":{"answerable":true}}),
        json!({"text":"synthetic_workshop_learner"}),
    ] {
        ensure!(assert_no_leakage(&bad, &frozen).is_err());
    }
    Ok(())
}

#[test]
fn all_twelve_actual_store_contexts_keep_real_sources_and_unmodified_question() -> Result<()> {
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(&root())?;
    for id in ids(&method) {
        let (row, review) = build_case(&frozen, &method, &id)?;
        let context = &row["request"]["context"];
        ensure!(context["language"] == "zh" && review["teaching_language"] == "zh");
        ensure!(
            context["task"]["goal"] == ""
                && context["task"]["reported_result"] == ""
                && context["task"]["stage"] == "goal_setting"
        );
        ensure!(
            context["task"]["modules"].is_null() && context["current_hardware_status"] == "unknown"
        );
        let expected = if let Some(q) = frozen.queries.iter().find(|q| s(q, "id") == id) {
            &q["text"]
        } else {
            &method["boundary_cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| s(c, "id") == id)
                .unwrap()["question"]
        };
        ensure!(&context["user_input"] == expected);
        let actual = review["actual_context_live_sources"].as_array().unwrap();
        ensure!(actual.len() == context["evidence"].as_array().unwrap().len());
        for source in actual {
            ensure!(source["source_type"] == "simulated" && source["event_status"] == "active");
            ensure!(review["source_mapping"].as_array().unwrap().iter().any(|m|m["event_id"]==source["event_id"] && m["visible_in_context"]==true));
        }
        match id.as_str() {
            "historical_plan" => ensure!(
                context["continuation"]["pending_step"]["completion_status"] == "not_revalidated"
            ),
            "descendant_completed" => {
                ensure!(context["continuation"]["pending_step"].is_null());
                ensure!(
                    context["continuation"]["pending_resolution"]["kind"] == "completed_activity"
                );
                ensure!(
                    context["evidence"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|e| e["content"]["text"]
                            == method["boundary_source_texts"]["completion_report"])
                );
            }
            "forgotten_selection" => ensure!(context.get("continuation").is_none()),
            "new_choice_after_forget" => ensure!(context["continuation"]["ancestor_count"] == 1),
            _ => {}
        }
    }
    Ok(())
}

#[test]
fn citation_coverage_includes_continuation_but_never_hidden_source() -> Result<()> {
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(&root())?;
    let (_, review) = build_case(&frozen, &method, "loops_q07")?;
    let plan = review["source_mapping"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source_alias"] == "loops_s07")
        .unwrap();
    ensure!(plan["visible_in_context"] == true);
    let checked = citation_check(
        &json!({"used_evidence_ids":[plan["event_id"]],"task_update_candidates":[],"memory_candidates":[]}),
        &review,
    );
    ensure!(
        checked["complete_original_set_cited"] == true
            && checked["cited_actual_context_sources"]
                .as_array()
                .unwrap()
                .len()
                == 1
    );
    let mut hidden = review.clone();
    hidden["source_mapping"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|m| m["source_alias"] == "loops_s07")
        .unwrap()["visible_in_context"] = json!(false);
    ensure!(
        citation_check(&json!({"used_evidence_ids":[plan["event_id"]]}), &hidden)["complete_original_set_cited"]
            == false
    );
    Ok(())
}

#[test]
fn attempt_artifact_is_create_new_and_never_overwrites_previous_result() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("attempt.json");
    let original = json!({"request_id":"first","retries":0});
    let sha = write_new(&path, &original)?;
    ensure!(write_new(&path, &json!({"request_id":"second"})).is_err());
    ensure!(serde_json::from_slice::<Value>(&checked(&path, &sha)?)? == original);
    ensure!(checked(&path, "wrong").is_err());
    Ok(())
}

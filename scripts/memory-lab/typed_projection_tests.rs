use super::*;

fn frozen() -> Result<Frozen> {
    load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
fn source(claim: &str, support: &str) -> Result<Value> {
    frozen()?
        .sources
        .into_iter()
        .find(|row| s(row, "claim_type") == claim && s(row, "support_level") == support)
        .ok_or_else(|| anyhow!("Frozen test source missing"))
}

#[test]
fn all_sources_use_real_typed_projection_without_inventing_progress() -> Result<()> {
    let frozen = frozen()?;
    let (report, inputs) = project(&frozen)?;
    ensure!(
        report["projected_count"] == 42 && report["excluded_count"] == 18 && inputs.len() == 234
    );
    let counts = &report["kind_counts"];
    ensure!(
        counts["trial_observation"] == 12
            && counts["concept_evidence"] == 12
            && counts["learning_goal"] == 6
            && counts["accepted_pending_experiment"] == 6
            && counts["correction"] == 6
    );
    ensure!(
        report["persistent_preference_coverage"] == 0
            && report["measured_hint_effectiveness_coverage"] == 0
    );
    let decisions = report["decisions"].as_array().unwrap();
    for (support, count) in [("hinted", 6), ("independent", 6)] {
        ensure!(
            decisions
                .iter()
                .filter(|row| row["projected"] == true
                    && row["projection_kind"] == "concept_evidence"
                    && row["memory_evidence"]["education_metadata"]["support_level"] == support)
                .count()
                == count
        );
    }
    for row in decisions.iter().filter(|row| row["projected"] == false) {
        ensure!(row.get("encoding_text").is_none() && row.get("memory_id").is_none());
    }
    for row in decisions
        .iter()
        .filter(|row| row["projection_kind"] == "accepted_pending_experiment")
    {
        ensure!(
            row["memory_evidence"]["memory"]["kind"] == "pending_experiment"
                && s(row, "encoding_text").starts_with("已接受、尚待完成的实验：")
        );
    }
    ensure!(
        report["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["progress_state_unchanged"] == true)
    );
    Ok(())
}

#[test]
fn hinted_evidence_cannot_be_relabelled_as_independent() -> Result<()> {
    let source = source("task_specific_response", "hinted")?;
    let f = Fixture::new()?;
    let sid = f.session()?;
    let event = f.event(&sid, &source)?;
    let claim = f.claim(&sid, std::slice::from_ref(&event), CLOCK_MS)?;
    let valid = candidate(&source, &event, "concept_evidence");
    let mut wrong = valid.clone();
    wrong["support_level"] = json!("independent");
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[wrong], CLOCK_MS + 1)
            .is_err()
    );
    ensure!(f.store.memories(&f.lid)?.is_empty());
    let staged = f
        .store
        .stage_memory_candidates(&claim["receipt"], &[valid], CLOCK_MS + 2)?;
    let applied = f.store.apply_memory_candidate(
        &f.lid,
        s(&staged[0], "candidate_id"),
        &["concept_evidence"],
        CLOCK_MS + 3,
    )?;
    ensure!(
        applied["education_metadata"]["support_level"] == "hinted"
            && s(&applied["memory"], "content").starts_with("本次回答证据，不代表已掌握：")
    );
    Ok(())
}

#[test]
fn unaccepted_plan_and_temporary_request_cannot_pass_promotion() -> Result<()> {
    // Malformed candidate proposals are adversarial test inputs. Source event
    // acceptance/preference fields are never strengthened to make them pass.
    for (claim_type, target, target_claim) in [
        (
            "unaccepted_plan",
            "accepted_pending_experiment",
            "accepted_plan",
        ),
        (
            "temporary_request",
            "help_preference",
            "explicit_preference",
        ),
    ] {
        let source = source(claim_type, "unknown")?;
        ensure!(kind(&source).is_none());
        let f = Fixture::new()?;
        let sid = f.session()?;
        let event = f.event(&sid, &source)?;
        let claim = f.claim(&sid, std::slice::from_ref(&event), CLOCK_MS)?;
        let mut wrong = candidate(&source, &event, target);
        wrong["claim_type"] = json!(target_claim);
        if target == "help_preference" {
            wrong["scope"] = json!("cross_session_explicit");
        }
        let staged = f
            .store
            .stage_memory_candidates(&claim["receipt"], &[wrong], CLOCK_MS + 1)?;
        ensure!(
            f.store
                .apply_memory_candidate(
                    &f.lid,
                    s(&staged[0], "candidate_id"),
                    &[target],
                    CLOCK_MS + 2
                )
                .is_err()
        );
        ensure!(f.store.memories(&f.lid)?.is_empty());
        let current = f
            .store
            .source_events(&f.lid, &[s(&event, "event_id").to_owned()])?;
        ensure!(current[0]["content"] == event["content"]);
    }
    Ok(())
}

#[test]
fn exact_quote_version_and_live_correction_remain_authoritative() -> Result<()> {
    let source = source("reported_observation", "unknown")?;
    let f = Fixture::new()?;
    let sid = f.session()?;
    let event = f.event(&sid, &source)?;
    let claim = f.claim(&sid, std::slice::from_ref(&event), CLOCK_MS)?;
    let valid = candidate(&source, &event, "trial_observation");
    let mut wrong = valid.clone();
    wrong["source_event_versions"][s(&event, "event_id")] = json!(2);
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[wrong], CLOCK_MS + 1)
            .is_err()
    );
    let mut wrong = valid.clone();
    wrong["source_quotes"][0]["quote"] = json!("NOT_PRESENT_IN_SYNTHETIC_SOURCE");
    ensure!(
        f.store
            .stage_memory_candidates(&claim["receipt"], &[wrong], CLOCK_MS + 2)
            .is_err()
    );
    let staged = f
        .store
        .stage_memory_candidates(&claim["receipt"], &[valid], CLOCK_MS + 3)?;
    let applied = f.store.apply_memory_candidate(
        &f.lid,
        s(&staged[0], "candidate_id"),
        &["trial_observation"],
        CLOCK_MS + 4,
    )?;
    let mid = s(&applied["memory"], "memory_id");
    f.store.revise_education_event(
        &f.lid,
        &sid,
        s(&event, "event_id"),
        "simulated",
        &json!({"text":"合成更正：原观察不能再作为当前记录。"}),
    )?;
    ensure!(f.store.memory_evidence(&f.lid, mid).is_err());
    ensure!(
        f.store
            .embedding_work(&f.lid, &EmbeddingProfile::BgeM3NativeFp32.model_key(), 32)?
            .is_empty()
    );
    Ok(())
}

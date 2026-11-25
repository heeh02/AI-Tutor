//! Canonical comparison context from real Store receipts, deliberately lossy
//! summaries and synthetic vectors. No provider, encoder, browser or device.
use ai_tutor::{
    context::{MAX_CONTEXT_CHARS, build_context, quote_windows_arg},
    memory_embedding::EmbeddingProfile,
    store::{CandidateRanking, QueryEmbedding, Store, empty_state, s, uid},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};

struct Fixture {
    store: Store,
    path: PathBuf,
    lid: String,
    sid: String,
    _directory: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("comparison-context.sqlite3");
        let store = Store::open(&path, "simulator")?;
        let lid = s(
            &store.create_learner("Synthetic comparison", "en", "guided")?,
            "learner_id",
        )
        .to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        Ok(Self {
            store,
            path,
            lid,
            sid,
            _directory: directory,
        })
    }
    fn reopen(self) -> Result<Self> {
        let Self {
            store,
            path,
            lid,
            sid,
            _directory,
        } = self;
        drop(store);
        let store = Store::open(&path, "simulator")?;
        Ok(Self {
            store,
            path,
            lid,
            sid,
            _directory,
        })
    }
}

fn comparison_request() -> Value {
    json!({"submission_id":uid("comparison-context"),
        "question":"Does changing the stopping threshold change the result?",
        "changed_variable":"stopping threshold",
        "conditions":[{"name":"same track","status":"reported_same"},
            {"name":"motor speed has not been checked","status":"not_confirmed"}],
        "trials":[{"label":"A","setting":"30 cm","status":"reported",
            "result":"The learner reported that A stopped near the wall.","trial_count":null},
            {"label":"B","setting":"50 cm","status":"not_tested","result":null,"trial_count":null}],
        "pending_plan":{"text":"Next time test B at 50 cm; speed and repeat count are not decided.","accepted":true},
        "source_type":"simulated"})
}

fn save_comparison(
    store: &Store,
    lid: &str,
    sid: &str,
    request: &Value,
) -> Result<(String, Vec<Value>)> {
    let saved = store.save_comparison(lid, sid, request)?;
    let cid = s(&saved["item"], "comparison_id").to_owned();
    let sources: Vec<_> = store
        .recent_evidence(sid)?
        .into_iter()
        .filter(|row| row["content"]["comparison_id"] == cid)
        .collect();
    ensure!(
        sources.len() == 3,
        "fixture must have observation, untested and accepted-plan siblings"
    );
    Ok((cid, sources))
}

fn observation(sources: &[Value]) -> &Value {
    sources
        .iter()
        .find(|source| source["event_kind"] == "observation")
        .unwrap()
}

fn pending(sources: &[Value]) -> &Value {
    sources
        .iter()
        .find(|source| source["content"].get("pending_plan").is_some())
        .unwrap()
}

fn remember(store: &Store, lid: &str, source: &Value, kind: &str, content: &str) -> Result<String> {
    let id = s(source, "event_id");
    store
        .add_memory(
            lid,
            &json!({"kind":kind,"content":content,"scope":"current_activity",
        "source_event_ids":[id],"source_event_versions":{id:source["event_version"]}}),
            store.learner(lid)?["epoch"].as_i64().unwrap(),
        )?
        .ok_or_else(|| anyhow!("fixture summary must have a valid real source binding"))
}

fn model_context(store: &Store, lid: &str, sid: &str, text: &str) -> Result<Value> {
    let input = store.record_education_event(
        lid,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    build_context(
        store,
        &store.session(sid)?,
        &store.learner(lid)?,
        text,
        s(&input, "event_id"),
        &json!({}),
    )
}

fn groups(context: &Value) -> &[Value] {
    match context.get("comparison_contexts") {
        None => &[],
        Some(groups) => groups
            .as_array()
            .expect("present canonical comparison groups must be an array"),
    }
}

fn memory_ids(context: &Value) -> Vec<&str> {
    context["slow_memory"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| s(row, "memory_id"))
        .collect()
}

fn assert_complete_group(
    group: &Value,
    request: &Value,
    cid: &str,
    f: &Fixture,
    sources: &[Value],
) -> Result<()> {
    ensure!(
        group["comparison_id"] == cid
            && group["learner_id"] == f.lid
            && group["session_id"] == f.sid
    );
    ensure!(group["activity_id"] == f.store.session(&f.sid)?["activity_id"]);
    ensure!(group["namespace"] == "simulator" && group["source_type"] == "simulated");
    ensure!(group["historical"] == true && group["hardware_verified"] == false);
    for field in [
        "question",
        "changed_variable",
        "conditions",
        "trials",
        "pending_plan",
    ] {
        ensure!(
            group[field] == request[field],
            "canonical {field} was reconstructed from the lossy summary or truncated"
        );
    }
    ensure!(
        group["trials"][1]["status"] == "not_tested"
            && group["trials"][1]["result"].is_null()
            && group["trials"][1]["trial_count"].is_null()
    );
    ensure!(group["pending_plan"]["accepted"] == true);
    ensure!(group["limits"]["reported_labels"] == json!(["A"]));
    ensure!(group["limits"]["untested_labels"] == json!(["B"]));
    ensure!(
        group["limits"]["unknown_count_labels"] == json!(["A"]),
        "untested B is not a reported unknown count"
    );
    let unconfirmed: Vec<_> = request["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|condition| condition["status"] == "not_confirmed")
        .cloned()
        .collect();
    ensure!(group["limits"]["unconfirmed_conditions"] == json!(unconfirmed));
    ensure!(
        group["limits"]["comparison_incomplete"] == true
            && group["limits"]["causal_effect_established"] == false
    );
    let links = group["source_links"].as_array().unwrap();
    ensure!(links.len() == 3);
    let roles: HashSet<_> = links.iter().map(|link| s(link, "role")).collect();
    ensure!(roles == HashSet::from(["observation", "untested", "pending_plan"]));
    for link in links {
        let original = sources
            .iter()
            .find(|source| source["event_id"] == link["event_id"])
            .ok_or_else(|| anyhow!("comparison group invented a source"))?;
        for field in ["event_id", "event_version", "event_kind", "source_type"] {
            ensure!(link[field] == original[field]);
        }
    }
    Ok(())
}

fn assert_all_linked_evidence_once(context: &Value, group: &Value) -> Result<()> {
    let evidence = context["evidence"].as_array().unwrap();
    for link in group["source_links"].as_array().unwrap() {
        let matching: Vec<_> = evidence
            .iter()
            .filter(|row| row["event_id"] == link["event_id"])
            .collect();
        ensure!(
            matching.len() == 1,
            "all sibling sources must be cited once, even if the summary only cited A"
        );
        for field in ["event_version", "event_kind", "source_type"] {
            ensure!(matching[0][field] == link[field]);
        }
    }
    Ok(())
}

fn vector() -> Vec<f32> {
    let mut vector = vec![0.0; 1024];
    vector[0] = 1.0;
    vector
}

fn cache_memory(store: &Store, lid: &str, mid: &str) -> Result<String> {
    let key = EmbeddingProfile::BgeM3NativeFp32.model_key();
    let snapshot = store
        .embedding_work(lid, &key, 32)?
        .into_iter()
        .find(|work| work.memory_id == mid)
        .ok_or_else(|| anyhow!("valid comparison memory must be embedding eligible"))?;
    ensure!(store.commit_memory_embedding(&snapshot, &key, &vector())?);
    Ok(key)
}

fn assert_invalid_comparison_not_recalled(
    f: &Fixture,
    mid: &str,
    cid: &str,
    primary: &str,
    key: &str,
) -> Result<()> {
    ensure!(
        f.store.source_events(&f.lid, &[primary.to_owned()])?.len() == 1,
        "primary summary source must still be valid"
    );
    let evidence = f.store.memory_evidence(&f.lid, mid)?;
    ensure!(evidence["comparison_context"]["status"] == "unavailable");
    ensure!(
        evidence["comparison_context"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    ensure!(
        f.store
            .memories(&f.lid)?
            .iter()
            .any(|memory| memory["memory_id"] == mid),
        "historical summary remains inspectable"
    );
    ensure!(
        f.store
            .recall_for_session(&f.sid, "comparison snapshot", 5)?
            .iter()
            .all(|row| row["memory_id"] != mid)
    );
    ensure!(
        f.store
            .embedding_work(&f.lid, key, 32)?
            .iter()
            .all(|row| row.memory_id != mid)
    );
    let values = vector();
    for ranking in [CandidateRanking::DenseOnly, CandidateRanking::HybridRrf] {
        let recalled = f.store.recall_candidates_for_session_ranked(
            &f.sid,
            "comparison snapshot",
            QueryEmbedding {
                model_key: key,
                values: &values,
            },
            5,
            ranking,
        )?;
        ensure!(recalled.iter().all(|row| row.memory["memory_id"] != mid));
    }
    let context = model_context(&f.store, &f.lid, &f.sid, "comparison snapshot")?;
    ensure!(!memory_ids(&context).contains(&mid));
    ensure!(
        groups(&context)
            .iter()
            .all(|group| group["comparison_id"] != cid)
    );
    Ok(())
}

#[test]
fn lossy_report_memory_carries_original_untested_counts_conditions_and_accepted_plan() -> Result<()>
{
    let f = Fixture::new()?;
    let request = comparison_request();
    let (cid, sources) = save_comparison(&f.store, &f.lid, &f.sid, &request)?;
    let mid = remember(
        &f.store,
        &f.lid,
        observation(&sources),
        "reported_progress",
        "comparison snapshot: A stopped.",
    )?;
    // Exercise the optional memory path independently of required task facts.
    f.store.checkpoint(&f.sid, &empty_state())?;
    let evidence = f.store.memory_evidence(&f.lid, &mid)?;
    ensure!(evidence["comparison_context"]["status"] == "complete");
    let context = model_context(&f.store, &f.lid, &f.sid, "comparison snapshot")?;
    ensure!(memory_ids(&context) == vec![mid.as_str()]);
    ensure!(groups(&context).len() == 1);
    assert_complete_group(&groups(&context)[0], &request, &cid, &f, &sources)?;
    ensure!(context["slow_memory"][0]["content"] == "comparison snapshot: A stopped.");
    ensure!(context["slow_memory"][0]["comparison_context_ids"] == json!([cid]));
    ensure!(context["slow_memory"][0]["comparison_context_status"] == "complete");
    assert_all_linked_evidence_once(&context, &groups(&context)[0])?;
    Ok(())
}

#[test]
fn task_and_two_memories_share_one_comparison_and_deduplicate_all_sibling_evidence() -> Result<()> {
    let f = Fixture::new()?;
    let request = comparison_request();
    let (cid, sources) = save_comparison(&f.store, &f.lid, &f.sid, &request)?;
    let first = remember(
        &f.store,
        &f.lid,
        observation(&sources),
        "reported_progress",
        "comparison snapshot: A stopped.",
    )?;
    let second = remember(
        &f.store,
        &f.lid,
        observation(&sources),
        "reported_progress",
        "comparison snapshot: stopping was reported.",
    )?;
    let context = model_context(&f.store, &f.lid, &f.sid, "comparison snapshot")?;
    ensure!(
        memory_ids(&context).into_iter().collect::<HashSet<_>>()
            == HashSet::from([first.as_str(), second.as_str()])
    );
    ensure!(groups(&context).len() == 1);
    assert_complete_group(&groups(&context)[0], &request, &cid, &f, &sources)?;
    ensure!(context["task"]["comparison_context_ids"] == json!([cid]));
    ensure!(context["task"]["comparison_context_status"] == "complete");
    for memory in context["slow_memory"].as_array().unwrap() {
        ensure!(memory["comparison_context_ids"] == context["task"]["comparison_context_ids"]);
        ensure!(memory["comparison_context_status"] == "complete");
    }
    assert_all_linked_evidence_once(&context, &groups(&context)[0])?;
    Ok(())
}

#[test]
fn comparisons_follow_only_the_explicitly_selected_activity_chain() -> Result<()> {
    let f = Fixture::new()?;
    let request = comparison_request();
    let (selected_cid, selected_sources) = save_comparison(&f.store, &f.lid, &f.sid, &request)?;
    let selected_mid = remember(
        &f.store,
        &f.lid,
        observation(&selected_sources),
        "reported_progress",
        "comparison snapshot selected A report",
    )?;
    f.store.finish_session(&f.sid)?;
    let sibling_sid = s(&f.store.create_session(&f.lid)?, "session_id").to_owned();
    let mut sibling_request = comparison_request();
    sibling_request["question"] = json!("HIDDEN SIBLING QUESTION must not enter another activity");
    let (sibling_cid, sibling_sources) =
        save_comparison(&f.store, &f.lid, &sibling_sid, &sibling_request)?;
    let sibling_mid = remember(
        &f.store,
        &f.lid,
        observation(&sibling_sources),
        "reported_progress",
        "comparison snapshot HIDDEN SIBLING report",
    )?;
    let fresh_sid = s(&f.store.create_session(&f.lid)?, "session_id").to_owned();
    let unselected = model_context(&f.store, &f.lid, &fresh_sid, "comparison snapshot")?;
    ensure!(groups(&unselected).is_empty() && memory_ids(&unselected).is_empty());
    let generation = f.store.session(&fresh_sid)?["generation"].as_i64().unwrap();
    let nonce = uid("select-comparison");
    ensure!(
        f.store
            .continuation_replay(&f.lid, &f.sid, &nonce, &fresh_sid, generation)?
            .is_none()
    );
    let after = f.store.bump(&fresh_sid, "idle")?;
    let continued = f
        .store
        .continue_finished_session(&f.lid, &f.sid, &nonce, &fresh_sid, generation, after)?;
    let context = model_context(
        &f.store,
        &f.lid,
        s(&continued, "session_id"),
        "comparison snapshot",
    )?;
    ensure!(memory_ids(&context).contains(&selected_mid.as_str()));
    ensure!(!memory_ids(&context).contains(&sibling_mid.as_str()));
    ensure!(groups(&context).len() == 1 && groups(&context)[0]["comparison_id"] == selected_cid);
    for hidden in [&sibling_sid, &sibling_cid, &sibling_mid] {
        ensure!(!context.to_string().contains(hidden.as_str()));
    }
    ensure!(!context.to_string().contains("HIDDEN SIBLING"));
    assert_all_linked_evidence_once(&context, &groups(&context)[0])?;
    Ok(())
}

#[test]
fn forgetting_a_plan_sibling_keeps_primary_inspectable_but_blocks_lexical_and_semantic_recall()
-> Result<()> {
    let f = Fixture::new()?;
    let (cid, sources) = save_comparison(&f.store, &f.lid, &f.sid, &comparison_request())?;
    let primary = s(observation(&sources), "event_id").to_owned();
    let mid = remember(
        &f.store,
        &f.lid,
        observation(&sources),
        "reported_progress",
        "comparison snapshot A stopped",
    )?;
    let pending_memory = remember(
        &f.store,
        &f.lid,
        pending(&sources),
        "pending_experiment",
        s(&pending(&sources)["content"], "text"),
    )?;
    let key = cache_memory(&f.store, &f.lid, &mid)?;
    let values = vector();
    ensure!(
        f.store
            .recall_candidates_for_session_ranked(
                &f.sid,
                "comparison snapshot",
                QueryEmbedding {
                    model_key: &key,
                    values: &values
                },
                5,
                CandidateRanking::DenseOnly
            )?
            .iter()
            .any(|row| row.memory["memory_id"] == mid)
    );
    f.store.forget(&f.lid, Some(&pending_memory))?;
    assert_invalid_comparison_not_recalled(&f, &mid, &cid, &primary, &key)?;
    let f = f.reopen()?;
    assert_invalid_comparison_not_recalled(&f, &mid, &cid, &primary, &key)?;
    Ok(())
}

#[test]
fn revising_an_untested_sibling_never_reuses_the_old_comparison_background() -> Result<()> {
    let f = Fixture::new()?;
    let (cid, sources) = save_comparison(&f.store, &f.lid, &f.sid, &comparison_request())?;
    let primary = s(observation(&sources), "event_id").to_owned();
    let mid = remember(
        &f.store,
        &f.lid,
        observation(&sources),
        "reported_progress",
        "comparison snapshot A stopped",
    )?;
    let key = cache_memory(&f.store, &f.lid, &mid)?;
    let untested = sources
        .iter()
        .find(|row| row["event_kind"] == "plan" && row["content"]["accepted"] == false)
        .unwrap();
    f.store.revise_education_event(
        &f.lid,
        &f.sid,
        s(untested, "event_id"),
        "simulated",
        &json!({"text":"Correction: the untested B setting has not yet been chosen."}),
    )?;
    assert_invalid_comparison_not_recalled(&f, &mid, &cid, &primary, &key)?;
    let f = f.reopen()?;
    assert_invalid_comparison_not_recalled(&f, &mid, &cid, &primary, &key)?;
    Ok(())
}

#[test]
fn optional_comparison_memory_is_omitted_as_a_whole_instead_of_truncating_conditions() -> Result<()>
{
    let f = Fixture::new()?;
    let mut request = comparison_request();
    request["question"] = json!("comparison question ".repeat(10));
    request["changed_variable"] = json!("threshold variable ".repeat(10));
    request["conditions"] = json!(
        (0..8)
            .map(|index| json!({
                "name":format!("condition-{index}:{}", "c".repeat(180)),
                "status":if index == 0 { "reported_same" } else { "not_confirmed" }
            }))
            .collect::<Vec<_>>()
    );
    request["trials"][0]["result"] = json!(format!("A reported stopping; {}", "r".repeat(950)));
    request["pending_plan"]["text"] = json!(format!("Next time test B; {}", "p".repeat(950)));
    let (cid, sources) = save_comparison(&f.store, &f.lid, &f.sid, &request)?;
    let mid = remember(
        &f.store,
        &f.lid,
        observation(&sources),
        "reported_progress",
        "comparison snapshot A stopped",
    )?;
    f.store.checkpoint(&f.sid, &empty_state())?;
    let small = model_context(&f.store, &f.lid, &f.sid, "comparison snapshot")?;
    ensure!(
        memory_ids(&small).contains(&mid.as_str()),
        "fixture must fit before current-input pressure"
    );
    assert_complete_group(&groups(&small)[0], &request, &cid, &f, &sources)?;
    let mut omitted = false;
    // Use the real unchanged limits, locating the transition instead of relying
    // on a brittle serialized overhead estimate or changing a budget constant.
    for size in (512..MAX_CONTEXT_CHARS).step_by(512) {
        let text = format!("comparison snapshot {}", "x".repeat(size));
        let context = model_context(&f.store, &f.lid, &f.sid, &text)?;
        ensure!(context["user_input"] == text);
        ensure!(context.to_string().chars().count() <= MAX_CONTEXT_CHARS - 256);
        ensure!(
            quote_windows_arg(&context.to_string())
                .encode_utf16()
                .count()
                <= 21_000 - 256
        );
        if memory_ids(&context).contains(&mid.as_str()) {
            ensure!(groups(&context).len() == 1);
            assert_complete_group(&groups(&context)[0], &request, &cid, &f, &sources)?;
        } else {
            ensure!(
                groups(&context)
                    .iter()
                    .all(|group| group["comparison_id"] != cid),
                "omitted memory must not leave a partial comparison group"
            );
            ensure!(
                context["context_selection"]["omitted_memories"]
                    .as_u64()
                    .unwrap()
                    >= 1
            );
            omitted = true;
            break;
        }
    }
    ensure!(
        omitted,
        "optional comparison should be omitted before required input reaches its limit"
    );
    Ok(())
}

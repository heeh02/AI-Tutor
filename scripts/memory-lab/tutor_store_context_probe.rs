//! Actual temporary Store -> production context; synthetic diagnostic only.
#[allow(dead_code)]
#[path = "typed48_common.rs"]
mod typed48;

use ai_tutor::{
    context::build_context,
    kimi::KimiProvider,
    store::{Store, empty_state, s, strings, uid},
};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
use typed48::{Frozen, Projected, hash};

const METHOD: &str = include_str!("tutor_store_context_method_v1.json");
const SOURCE: &[u8] = include_bytes!("tutor_store_context_probe.rs");
const HELPER: &[u8] = include_bytes!("typed48_common.rs");
const POLICY: &str = "prompts/tutor_core.md";
const SCHEMA: &str =
    "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json";
const HELP: &str = "tutor-store-context-probe --root PATH --output NEW_DIR\nDefault: prepare12 from real temporary simulator Stores, no model.\ntutor-store-context-probe --root PATH --prepared DIR --prepared-manifest-sha256 SHA --real-synthetic (--case ID | --batch 1|2 | --all)\nOne attempt each; no delivery/apply. Exit0 is not semantic quality passed.";

fn read(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() <= 24 * 1024 * 1024,
        "Diagnostic file exceeds bound"
    );
    Ok(fs::read(path)?)
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = read(path)?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen file changed: {}",
        path.display()
    );
    Ok(bytes)
}
fn write_new(path: &Path, value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(hash(&bytes))
}
fn ids(method: &Value) -> Vec<String> {
    method["batches"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| strings(&b["case_ids"]))
        .collect()
}
fn source_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut paths = vec![
        "rust/ai-tutor/src/context.rs".to_owned(),
        "rust/ai-tutor/src/store.rs".to_owned(),
        "rust/ai-tutor/src/tutor.rs".to_owned(),
        "rust/ai-tutor/src/kimi.rs".to_owned(),
        "scripts/memory-lab/typed48_common.rs".to_owned(),
        "scripts/memory-lab/typed48_projection_method_v1.json".to_owned(),
    ];
    for entry in fs::read_dir(root.join("rust/ai-tutor/src"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with("memory_") || name.starts_with("kimi_")) && name.ends_with(".rs") {
            paths.push(format!("rust/ai-tutor/src/{name}"));
        }
    }
    for entry in fs::read_dir(root.join("rust/ai-tutor/src/store"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".rs") {
            paths.push(format!("rust/ai-tutor/src/store/{name}"));
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| Ok((path.clone(), hash(&read(&root.join(path))?))))
        .collect()
}
fn executable() -> Result<Value> {
    let path = std::env::current_exe()?.canonicalize()?;
    // Release executables can exceed the small text-input bound.
    let bytes = fs::read(&path)?;
    Ok(json!({"current_exe":path.to_string_lossy(),"actual_executable_sha256":hash(&bytes)}))
}

fn decision<'a>(p: &'a Projected, alias: &str) -> Result<&'a Value> {
    p.decisions
        .iter()
        .find(|d| s(d, "source_id") == alias)
        .ok_or_else(|| anyhow!("Missing fixture source"))
}
fn pack_session(p: &Projected, pack: &str) -> Result<String> {
    let rows: Vec<_> = p
        .decisions
        .iter()
        .filter(|d| s(&d["original_source"], "pack_id") == pack)
        .collect();
    ensure!(rows.len() == 10, "Pack needs all ten actual source events");
    let sid = s(&rows[0]["live_imported_event"], "session_id").to_owned();
    ensure!(
        !sid.is_empty()
            && rows
                .iter()
                .all(|d| s(&d["live_imported_event"], "session_id") == sid),
        "Pack/session mapping differs"
    );
    Ok(sid)
}
fn seed_history(p: &Projected) -> Result<()> {
    let packs: HashSet<_> = p
        .decisions
        .iter()
        .map(|d| s(&d["original_source"], "pack_id"))
        .collect();
    ensure!(packs.len() == 6, "Expected six activities");
    // Finish before rebuilding historical state, keeping the helper's48 memories
    // unchanged. This fixture reconstructs sourced fields, not an LLM extraction.
    for pack in &packs {
        p.store.finish_session(&pack_session(p, pack)?)?;
    }
    for pack in packs {
        let sid = pack_session(p, pack)?;
        let goal = decision(p, &format!("{pack}_s10"))?;
        let plan = decision(p, &format!("{pack}_s07"))?;
        ensure!(
            goal["original_source"]["claim_type"] == "stated_goal"
                && plan["original_source"]["accepted"] == true,
            "Historical field source changed"
        );
        let mut state = p.store.session(&sid)?["state"].clone();
        for (field, d) in [("goal", goal), ("pending_step", plan)] {
            state[field] = d["original_source"]["text"].clone();
            state["field_evidence"][field] = json!({"source_event_ids":[d["event_id"]],"source_event_versions":{(s(d,"event_id")):1}});
        }
        p.store.checkpoint(&sid, &state)?;
    }
    ensure!(
        p.store.memories(&p.learner_id)?.len() == 48,
        "History reconstruction changed memory pool"
    );
    Ok(())
}
fn continue_from(store: &Store, lid: &str, source: &str, current: &str) -> Result<String> {
    let generation = store.session(current)?["generation"]
        .as_i64()
        .ok_or_else(|| anyhow!("Missing generation"))?;
    let nonce = uid("choice");
    ensure!(
        store
            .continuation_replay(lid, source, &nonce, current, generation)?
            .is_none(),
        "Fresh selection reused receipt"
    );
    let after = store.bump(current, "idle")?;
    let new = store.continue_finished_session(lid, source, &nonce, current, generation, after)?;
    ensure!(
        new["session"]["state"] == empty_state(),
        "Continuation copied old current state"
    );
    Ok(s(&new, "session_id").to_owned())
}
fn observation(
    p: &Projected,
    sid: &str,
    text: &str,
    alias: &str,
    mapping: &mut Vec<Value>,
) -> Result<String> {
    let event = p.store.record_education_event(
        &p.learner_id,
        sid,
        "simulated",
        "observation",
        &json!({"text":text}),
    )?;
    let memory=p.store.add_memory(&p.learner_id,&json!({"kind":"reported_progress","scope":"current_activity","content":text,"source_event_ids":[event["event_id"]]}),p.store.learner(&p.learner_id)?["epoch"].as_i64().unwrap())?.ok_or_else(||anyhow!("Observation not saved"))?;
    mapping.push(json!({"source_alias":alias,"event_id":event["event_id"],"session_id":sid,"original_text":text,"memory_id":memory}));
    Ok(s(&event, "event_id").to_owned())
}

// Fail rather than sanitizing a production request. Natural words such as
// "reading" are allowed inside prose, but identity fields/aliases are not.
fn assert_no_leakage(value: &Value, frozen: &Frozen) -> Result<()> {
    const KEYS: &[&str] = &[
        "fixture_source_id",
        "synthetic_fixture",
        "pack_id",
        "source_alias",
        "query_id",
        "candidate_pool_ids",
        "sufficient_evidence_sets",
        "answerable",
        "expected",
        "original_label",
        "semantic_review",
        "expected_answer",
        "label",
        "labels",
    ];
    fn walk(v: &Value, f: &Frozen) -> Result<()> {
        match v {
            Value::Object(map) => {
                for (key, value) in map {
                    ensure!(
                        !KEYS.contains(&key.as_str()),
                        "Fixture/review key leaked into production request: {key}"
                    );
                    walk(value, f)?;
                }
            }
            Value::Array(rows) => {
                for v in rows {
                    walk(v, f)?;
                }
            }
            Value::String(text) => {
                ensure!(
                    !f.sources.iter().any(|v| text.contains(s(v, "id")))
                        && !f.queries.iter().any(|v| text.contains(s(v, "id"))),
                    "Fixture alias leaked into production request"
                );
                ensure!(
                    !f.sources.iter().any(|v| [
                        s(v, "learner_id"),
                        s(v, "session_id"),
                        s(v, "activity_id")
                    ]
                    .contains(&text.as_str())),
                    "Fixture identity leaked into request"
                );
            }
            _ => {}
        }
        Ok(())
    }
    walk(value, frozen)
}
fn available_aliases(context: &Value, mapping: &[Value]) -> HashSet<String> {
    let visible: HashSet<_> = context["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| s(e, "event_id"))
        .collect();
    mapping
        .iter()
        .filter(|m| !s(m, "source_alias").is_empty() && visible.contains(s(m, "event_id")))
        .map(|m| s(m, "source_alias").to_owned())
        .collect()
}
fn build_case(frozen: &Frozen, method: &Value, id: &str) -> Result<(Value, Value)> {
    let p = typed48::project(frozen)?;
    seed_history(&p)?;
    let boundary = method["boundary_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| s(c, "id") == id);
    let query = frozen.queries.iter().find(|q| s(q, "id") == id);
    let (pack, text) = if let Some(q) = query {
        (s(q, "pack_id"), s(q, "text"))
    } else {
        let b = boundary.ok_or_else(|| anyhow!("Unknown frozen case"))?;
        (s(b, "pack"), s(b, "question"))
    };
    let source = pack_session(&p, pack)?;
    // All originals are finished. Selecting the source itself as the current
    // finished session is a supported production continuation precondition.
    let current = source.clone();
    let mut sid = continue_from(&p.store, &p.learner_id, &source, &current)?;
    let first_new = sid.clone();
    let mut allowed = HashSet::from([source.clone(), sid.clone()]);
    let mut mapping:Vec<Value>=p.decisions.iter().map(|d|json!({"source_alias":d["source_id"],"event_id":d["event_id"],"session_id":d["live_imported_event"]["session_id"],"original_text":d["original_source"]["text"],"memory_id":d["memory_id"]})).collect();
    let mut operations = vec![
        json!({"operation":"explicit_continue","source_session_id":source,"new_session_id":sid}),
    ];
    match id {
        "parent_chain" | "sibling_branch" => {
            observation(
                &p,
                &sid,
                s(&method["boundary_source_texts"], "parent_observation"),
                "boundary_parent_observation",
                &mut mapping,
            )?;
            p.store.finish_session(&sid)?;
            if id == "parent_chain" {
                sid = continue_from(&p.store, &p.learner_id, &sid, &sid)?;
                allowed.insert(sid.clone());
            } else {
                let sibling = sid.clone();
                sid = continue_from(&p.store, &p.learner_id, &source, &sid)?;
                allowed.remove(&sibling);
                allowed.insert(sid.clone());
                observation(
                    &p,
                    &sid,
                    s(&method["boundary_source_texts"], "sibling_observation"),
                    "boundary_current_observation",
                    &mut mapping,
                )?;
            }
            operations.push(json!({"operation":id,"intermediate_session_id":first_new,"current_session_id":sid}));
        }
        "descendant_completed" => {
            let text = s(&method["boundary_source_texts"], "completion_report");
            let event = p.store.record_education_event(
                &p.learner_id,
                &sid,
                "simulated",
                "observation",
                &json!({"text":text}),
            )?;
            mapping.push(json!({"source_alias":"boundary_completion_report","event_id":event["event_id"],"session_id":sid,"original_text":text}));
            let mut state = empty_state();
            state["stage"] = json!("completed");
            state["field_evidence"]["stage"] = json!({"source_event_ids":[event["event_id"]]});
            p.store.checkpoint(&sid, &state)?;
            p.store.finish_session(&sid)?;
            sid = continue_from(&p.store, &p.learner_id, &sid, &sid)?;
            allowed.insert(sid.clone());
            ensure!(
                p.store.session(&sid)?["continuation"]["pending_resolution"]["source_event_ids"]
                    == json!([event["event_id"]]),
                "Completion resolution missing"
            );
            operations.push(json!({"operation":"reported_completed_stage","source_event_id":event["event_id"],"current_session_id":sid}));
        }
        "forgotten_selection" | "new_choice_after_forget" => {
            observation(
                &p,
                &sid,
                s(&method["boundary_source_texts"], "retained_observation"),
                "boundary_retained_observation",
                &mut mapping,
            )?;
            let removed = decision(&p, &format!("{pack}_s01"))?;
            p.store
                .forget(&p.learner_id, Some(s(removed, "memory_id")))?;
            let selection = s(
                &p.store.session(&sid)?["continuation"],
                "selection_event_id",
            )
            .to_owned();
            ensure!(
                !selection.is_empty(),
                "Independent memory deletion withdrew the explicit selection"
            );
            p.store.revise_education_event(
                &p.learner_id,
                &sid,
                &selection,
                "simulated",
                &json!({"text":"撤销这次续做选择；不再继承旧活动。"}),
            )?;
            ensure!(
                p.store.session(&sid)?["continuation"].is_null(),
                "Revoked selection remains live"
            );
            allowed = HashSet::from([sid.clone()]);
            if id == "new_choice_after_forget" {
                p.store.finish_session(&sid)?;
                sid = continue_from(&p.store, &p.learner_id, &sid, &sid)?;
                allowed.insert(sid.clone());
                ensure!(
                    p.store.session(&sid)?["continuation"]["ancestor_count"] == 1,
                    "New choice jumped broken ancestry"
                );
            }
            operations.push(json!({"operation":id,"forgotten_memory_id":removed["memory_id"],"current_session_id":sid}));
        }
        _ => {}
    }
    let rid = uid("request");
    let generation = p.store.bump(&sid, "generating")?;
    let event = p.store.record_education_event(
        &p.learner_id,
        &sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    let eid = s(&event, "event_id");
    let turn = p.store.add_turn(
        &sid,
        &rid,
        generation,
        "user",
        text,
        "shown",
        &[eid.to_owned()],
    )?;
    let mut state = p.store.session(&sid)?["state"].clone();
    state["latest_input"] = json!(text);
    p.store.checkpoint(&sid, &state)?;
    let session = p.store.session(&sid)?;
    let context = build_context(
        &p.store,
        &session,
        &p.store.learner(&p.learner_id)?,
        text,
        eid,
        &json!({"selected_status":"unknown","selected_track_key":null,"frame_age_ms":null}),
    )?;
    let request = json!({"request_id":rid,"learner_id":p.learner_id,"session_id":sid,"generation":generation,"context":context});
    assert_no_leakage(&request, frozen)?;
    let visible_ids: Vec<_> = request["context"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| s(e, "event_id").to_owned())
        .collect();
    let live = p.store.source_events(&p.learner_id, &visible_ids)?;
    ensure!(live.len() == visible_ids.len(), "Context evidence not live");
    ensure!(
        live.iter().all(|e| allowed.contains(s(e, "session_id"))),
        "Production context leaked another activity or sibling"
    );
    // Account for all real IDs, including question/choice sources without aliases.
    for source in &live {
        if !mapping.iter().any(|m| m["event_id"] == source["event_id"]) {
            mapping.push(json!({"source_alias":null,"event_id":source["event_id"],"session_id":source["session_id"],
                "source_role":if source["event_id"]==event["event_id"] {"current_question"} else if source["content"]["action"]=="continue_finished_activity" {"explicit_activity_choice"} else {"other_actual_context_source"},
                "original_content":source["content"]}));
        }
    }
    for row in &mut mapping {
        row["live_after_operations"] = json!(
            !p.store
                .source_events(&p.learner_id, &[s(row, "event_id").to_owned()])?
                .is_empty()
        );
        row["visible_in_context"] = json!(visible_ids.iter().any(|id| id == s(row, "event_id")));
    }
    let aliases = available_aliases(&request["context"], &mapping);
    let label = frozen
        .labels
        .iter()
        .find(|l| s(l, "query_id") == id)
        .cloned()
        .unwrap_or(Value::Null);
    let sufficient = if label.is_null() || label["answerable"] == false {
        Value::Null
    } else {
        json!(typed48::complete_set_available(
            &label["sufficient_evidence_sets"],
            &aliases
        ))
    };
    let review = json!({"case_id":id,"original_query":query,"original_label":label,"boundary_spec":boundary,
        "actual_context_complete_original_set":sufficient,"source_mapping":mapping,"actual_context_live_sources":live,
        "actual_session":session,"actual_user_turn_id":turn,"operations":operations,"allowed_session_ids":allowed,
        "teaching_language":request["context"]["language"],"temporary_store_only":true,"context_unmodified_after_build":true,"model_called":false,
        "semantic_review":{"status":"pending_review","historical_claims_supported":null,"activity_and_branch_scope":null,"temporal_plan_status":null,"unsupported_updates":null},"semantic_quality_pass":null});
    Ok((json!({"case_id":id,"request":request}), review))
}

fn prepare(root: &Path, output: &Path) -> Result<()> {
    checked(
        &root.join("scripts/memory-lab/tutor_store_context_probe.rs"),
        &hash(SOURCE),
    )?;
    checked(
        &root.join("scripts/memory-lab/tutor_store_context_method_v1.json"),
        &hash(METHOD.as_bytes()),
    )?;
    checked(
        &root.join("scripts/memory-lab/typed48_common.rs"),
        &hash(HELPER),
    )?;
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(root)?;
    let policy = read(&root.join(POLICY))?;
    let schema_bytes = read(&root.join(SCHEMA))?;
    let schema: Value = serde_json::from_slice(&schema_bytes)?;
    let order = ids(&method);
    ensure!(
        order.len() == 12 && order.iter().collect::<HashSet<_>>().len() == 12,
        "Expected twelve unique cases"
    );
    let mut requests = vec![];
    let mut reviews = vec![];
    for id in order {
        let (request, review) = build_case(&frozen, &method, &id)?;
        let prompt = ai_tutor::kimi::build_prompt(
            std::str::from_utf8(&policy)?,
            &schema,
            &request["request"],
        )?;
        ensure!(!prompt.is_empty(), "Empty production prompt");
        requests.push(request);
        reviews.push(review);
    }
    fs::create_dir(output)?;
    let requests_sha = write_new(&output.join("requests.json"), &json!(requests))?;
    let review_sha = write_new(&output.join("review-cases.json"), &json!(reviews))?;
    let method_sha = write_new(&output.join("method.json"), &method)?;
    write_new(
        &output.join("manifest.json"),
        &json!({"status":"prepared_no_model","method_source_sha256":hash(METHOD.as_bytes()),"method_file_sha256":method_sha,
        "probe_source_sha256":hash(SOURCE),"helper_source_sha256":hash(HELPER),"executable":executable()?,"production_sources":source_hashes(root)?,
        "fixture_manifest_sha256":typed48::FIXTURE_SHA,"fixture_files":frozen.files,
        "legacy_projection_report_sha256":typed48::LEGACY_REPORT_SHA,"legacy_encoder_inputs_sha256":typed48::LEGACY_INPUTS_SHA,
        "requests_sha256":requests_sha,"review_cases_sha256":review_sha,"policy_sha256":hash(&policy),"schema_sha256":hash(&schema_bytes),
        "case_count":12,"temporary_store_count":12,"production_database_opened":false,"provider_constructed":false,"model_called":false,
        "delivered":false,"candidates_applied":false,"semantic_quality_pass":null}),
    )?;
    println!(
        "Prepared12 actual-Store synthetic contexts at {}",
        output.display()
    );
    Ok(())
}

enum Selection {
    Case(String),
    Batch(usize),
    All,
}
fn selected(method: &Value, selection: &Selection) -> Result<Vec<usize>> {
    let order = ids(method);
    match selection {
        Selection::Case(id) => Ok(vec![
            order
                .iter()
                .position(|v| v == id)
                .ok_or_else(|| anyhow!("Unknown frozen case"))?,
        ]),
        Selection::Batch(n) => {
            ensure!((1..=2).contains(n), "Batch must be1 or2");
            Ok(((n - 1) * 6..n * 6).collect())
        }
        Selection::All => Ok((0..12).collect()),
    }
}
fn citation_check(reply: &Value, review: &Value) -> Value {
    let used: HashSet<_> = strings(&reply["used_evidence_ids"]).into_iter().collect();
    let aliases: HashSet<String> = review["source_mapping"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            !s(m, "source_alias").is_empty()
                && m["visible_in_context"] == true
                && used.contains(s(m, "event_id"))
        })
        .map(|m| s(m, "source_alias").to_owned())
        .collect();
    let label = &review["original_label"];
    let cited_actual_sources:Vec<_>=review["actual_context_live_sources"].as_array().unwrap().iter().filter(|e|used.contains(s(e,"event_id"))).map(|e|json!({"event_id":e["event_id"],"event_version":e["event_version"],"session_id":e["session_id"],"activity_id":e["activity_id"]})).collect();
    json!({"cited_actual_context_sources":cited_actual_sources,"cited_visible_source_aliases":aliases,"complete_original_set_cited":if label["answerable"]==true {json!(typed48::complete_set_available(&label["sufficient_evidence_sets"],&aliases))}else{Value::Null},
        "task_candidate_count":reply["task_update_candidates"].as_array().map(Vec::len),"memory_candidate_count":reply["memory_candidates"].as_array().map(Vec::len),
        "semantic_review_required":true,"meaning":"ID coverage is not entailment; actual continuation/resolution sources are included, hidden sources cannot count"})
}
async fn run(root: &Path, prepared: &Path, manifest_sha: &str, selection: Selection) -> Result<()> {
    let manifest: Value =
        serde_json::from_slice(&checked(&prepared.join("manifest.json"), manifest_sha)?)?;
    ensure!(
        manifest["probe_source_sha256"] == hash(SOURCE)
            && manifest["helper_source_sha256"] == hash(HELPER)
            && manifest["method_source_sha256"] == hash(METHOD.as_bytes()),
        "Prepared diagnostic code changed"
    );
    ensure!(
        manifest["executable"] == executable()?,
        "Actual executable/path changed; prepare a new experiment"
    );
    ensure!(
        manifest["production_sources"] == json!(source_hashes(root)?),
        "Production source changed; prepare a new experiment"
    );
    checked(&root.join(POLICY), s(&manifest, "policy_sha256"))?;
    checked(&root.join(SCHEMA), s(&manifest, "schema_sha256"))?;
    let frozen = typed48::load(root)?;
    ensure!(
        manifest["fixture_files"] == frozen.files,
        "Frozen fixture files changed"
    );
    let method: Value = serde_json::from_slice(&checked(
        &prepared.join("method.json"),
        s(&manifest, "method_file_sha256"),
    )?)?;
    ensure!(
        method == serde_json::from_str::<Value>(METHOD)?,
        "Method changed"
    );
    let requests: Vec<Value> = serde_json::from_slice(&checked(
        &prepared.join("requests.json"),
        s(&manifest, "requests_sha256"),
    )?)?;
    let reviews: Vec<Value> = serde_json::from_slice(&checked(
        &prepared.join("review-cases.json"),
        s(&manifest, "review_cases_sha256"),
    )?)?;
    let order = ids(&method);
    ensure!(
        requests.len() == 12
            && reviews.len() == 12
            && requests
                .iter()
                .zip(&order)
                .all(|(r, id)| s(r, "case_id") == id)
            && reviews
                .iter()
                .zip(&order)
                .all(|(r, id)| s(r, "case_id") == id),
        "Prepared order changed"
    );
    for row in &requests {
        assert_no_leakage(&row["request"], &frozen)?;
    }
    let indices = selected(&method, &selection)?;
    for &i in &indices {
        ensure!(
            !prepared.join(format!("attempt-{}.json", order[i])).exists(),
            "Case already attempted; implicit retry refused"
        );
    }
    let provider = KimiProvider::new(root)?;
    let run_id = uid("run");
    let start = Instant::now();
    let mut outcomes = vec![];
    let operation:Result<()>=async {
        let audit=provider.probe().await?;
        ensure!(audit["available"]==true,"Verified existing tool-free provider required");
        let identity=json!({"version":audit["version"],"binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"],"tools":audit["tools"],"subagents":audit["subagents"],"model_identity_independently_frozen":false,"sampling_independently_frozen":false});
        write_new(&prepared.join(format!("{run_id}-start.json")),&json!({"provider":identity,"case_ids":indices.iter().map(|&i|&order[i]).collect::<Vec<_>>(),"prepared_manifest_sha256":manifest_sha,"retries":0}))?;
        for &i in &indices {
            let id=&order[i];
            let request=&requests[i]["request"];
            write_new(&prepared.join(format!("attempt-{id}.json")),&json!({"case_id":id,"request_id":request["request_id"],"request_sha256":hash(&serde_json::to_vec(request)?),"run_id":run_id,"attempted_at":chrono::Utc::now().to_rfc3339(),"retries":0}))?;
            let began=Instant::now();
            let reply=provider.generate(request).await;
            let mut record=json!({"case_id":id,"run_id":run_id,"elapsed_ms":began.elapsed().as_secs_f64()*1000.0,"provider":identity,"reply_validated":false,"reply":null,"error":null,"semantic_review_status":"pending_review","semantic_quality_pass":null,"delivered":false,"candidates_applied":false});
            match reply {
                Ok(reply)=>{record["citation_checks"]=citation_check(&reply,&reviews[i]);record["reply_validated"]=json!(true);record["reply"]=reply;write_new(&prepared.join(format!("result-{id}.json")),&record)?;outcomes.push(json!({"case_id":id,"status":"validated_pending_semantic_review"}));},
                Err(error)=>{record["error"]=json!(error.to_string());write_new(&prepared.join(format!("result-{id}.json")),&record)?;outcomes.push(json!({"case_id":id,"status":"provider_or_protocol_failed"}));return Err(error);}
            }
        }
        Ok(())
    }.await;
    let closed = provider.close().await;
    write_new(
        &prepared.join(format!("{run_id}-summary.json")),
        &json!({"requested_case_ids":indices.iter().map(|&i|&order[i]).collect::<Vec<_>>(),"outcomes":outcomes,
        "operation_error":operation.as_ref().err().map(ToString::to_string),"provider_close_ok":closed.is_ok(),"provider_close_error":closed.as_ref().err().map(ToString::to_string),"provider_running_after_close":provider.status()["running"],"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,
        "cleanup_scope":"Existing provider close/running count only; no invented remote cancellation acknowledgment","semantic_quality_pass":null,"delivered":false,"candidates_applied":false}),
    )?;
    operation?;
    closed?;
    println!("Synthetic results saved for semantic review; no quality decision made.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let (mut output, mut prepared, mut manifest_sha, mut selection) = (None, None, None, None);
    let mut real = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--root" => root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root"))?),
            "--output" => {
                ensure!(output.is_none(), "Duplicate output");
                output = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing output"))?,
                ));
            }
            "--prepared" => {
                ensure!(prepared.is_none(), "Duplicate prepared");
                prepared = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing prepared"))?,
                ));
            }
            "--prepared-manifest-sha256" => {
                ensure!(manifest_sha.is_none(), "Duplicate hash");
                manifest_sha = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("Missing manifest hash"))?,
                );
            }
            "--real-synthetic" => {
                ensure!(!real, "Duplicate real flag");
                real = true;
            }
            "--case" | "--batch" | "--all" => {
                ensure!(selection.is_none(), "Choose one selector");
                selection = Some(match arg.as_str() {
                    "--case" => {
                        Selection::Case(args.next().ok_or_else(|| anyhow!("Missing case"))?)
                    }
                    "--batch" => Selection::Batch(
                        args.next()
                            .ok_or_else(|| anyhow!("Missing batch"))?
                            .parse()?,
                    ),
                    _ => Selection::All,
                });
            }
            _ => bail!("Unknown diagnostic argument"),
        }
    }
    let root = root.canonicalize()?;
    if real {
        ensure!(output.is_none(), "Real execution uses frozen preparation");
        run(
            &root,
            &prepared.ok_or_else(|| anyhow!("--prepared required"))?,
            &manifest_sha.ok_or_else(|| anyhow!("--prepared-manifest-sha256 required"))?,
            selection.ok_or_else(|| anyhow!("Explicit selector required"))?,
        )
        .await
    } else {
        ensure!(
            prepared.is_none() && manifest_sha.is_none() && selection.is_none(),
            "--real-synthetic required; no model called"
        );
        prepare(
            &root,
            &output.ok_or_else(|| anyhow!("--output NEW_DIR required"))?,
        )
    }
}

#[cfg(test)]
#[path = "tutor_store_context_tests.rs"]
mod tests;

//! Real Store/selected-ancestor contexts for ten synthetic answer-fidelity cases.
#[path = "tutor_probe_support.rs"]
mod tutor_probe_support;
use ai_tutor::{
    context::build_context,
    store::{Store, s, uid},
};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tutor_probe_support::*;

const SOURCE: &[u8] = include_bytes!("answer_fidelity_probe.rs");
const FIXTURE: &str = include_str!("answer_fidelity_cases_v1.json");
const METHOD: &str = include_str!("answer_fidelity_method_v1.json");
const HELP: &str = "answer-fidelity-probe --root PATH --output NEW_DIR\nDefault: prepare ten real-Store contexts, no provider.\nanswer-fidelity-probe --root PATH --prepared DIR --manifest-sha256 SHA --real-synthetic (--case ID | --all)\nOne attempt per sufficiently prepared case; no delivery/application. Insufficient sources stay positive cases marked NOT_RUN, not repaired prompts.";

fn cases() -> Result<Vec<Value>> {
    let fixture: Value = serde_json::from_str(FIXTURE)?;
    Ok(fixture["cases"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing cases"))?
        .clone())
}

fn continue_history(store: &Store, lid: &str, source: &str) -> Result<String> {
    let expected = store.session(source)?["generation"].as_i64().unwrap();
    let nonce = uid("fidelity_choice");
    ensure!(
        store
            .continuation_replay(lid, source, &nonce, source, expected)?
            .is_none()
    );
    let after = store.bump(source, "idle")?;
    let next = store.continue_finished_session(lid, source, &nonce, source, expected, after)?;
    Ok(s(&next, "session_id").to_owned())
}

fn source_coverage(context: &Value, required: &[Value]) -> Value {
    let checks: Vec<Value> = required
        .iter()
        .map(|item| {
            let event = &item["event"];
            let id = s(event, "event_id");
            let historical = item["memory_id"].is_string();
            let expected_relation = if historical {
                "selected_ancestor"
            } else {
                "current"
            };
            let evidence = context["evidence"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| s(row, "event_id") == id);
            let relation = evidence.is_some_and(|row| {
                row["session_id"] == event["session_id"]
                    && row["source_type"] == "simulated"
                    && row["session_relation"] == expected_relation
                    && row["ancestor_depth"] == if historical { json!(1) } else { json!(0) }
            });
            let visible_text = if historical {
                context["slow_memory"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|memory| {
                        memory["memory_id"] == item["memory_id"]
                            && memory["content"] == event["content"]["text"]
                            && memory["source_event_ids"]
                                .as_array()
                                .is_some_and(|ids| ids.contains(&json!(id)))
                    })
            } else {
                context["current_user_event_id"] == id
                    && context["user_input"] == event["content"]["text"]
                    && evidence.is_some_and(|row| {
                        row["content_reference"]["path"] == "user_input"
                            && row["content_reference"]["verbatim"] == true
                    })
            };
            json!({"event_id":id,"historical":historical,"expected_relation":expected_relation,
            "identity_relation_matches":relation,"verbatim_text_visible":visible_text,
            "covered":relation && visible_text})
        })
        .collect();
    let complete = !checks.is_empty() && checks.iter().all(|row| row["covered"] == true);
    json!({"complete":complete,"expected_sources":checks.len(),
        "missing_source_event_ids":checks.iter().filter(|row| row["covered"] != true)
            .map(|row| row["event_id"].clone()).collect::<Vec<_>>(),"checks":checks})
}

fn build_case(case: &Value) -> Result<(Value, Value)> {
    build_case_with_selection(case, true)
}

fn build_case_with_selection(case: &Value, select_history: bool) -> Result<(Value, Value)> {
    let directory = TemporaryStore(std::env::temp_dir().join(uid("answer-fidelity")));
    fs::create_dir(&directory.0)?;
    let store = Store::open(directory.0.join("case.sqlite3"), "simulator")?;
    let learner =
        store.create_learner("SYNTHETIC_ANSWER_FIDELITY", s(case, "language"), "guided")?;
    let lid = s(&learner, "learner_id");
    let initial = store.create_session(lid)?;
    let initial_sid = s(&initial, "session_id");
    let mut required = Vec::new();
    let history = case["history"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing history"))?;
    for source in history {
        let event = store.record_education_event(
            lid,
            initial_sid,
            "simulated",
            s(source, "event_kind"),
            &source["content"],
        )?;
        // Verbatim seed, stored through production provenance and activity guards.
        // This is not an LLM consolidation result or a hand-injected context row.
        let mid = store
            .add_memory(
                lid,
                &json!({"kind":source["memory_kind"],
            "scope":"current_activity","content":source["content"]["text"],
            "source_event_ids":[event["event_id"]]}),
                learner["epoch"].as_i64().unwrap(),
            )?
            .ok_or_else(|| anyhow!("Fixed synthetic source was not accepted by Store"))?;
        required.push(json!({"event":event,"memory_id":mid}));
    }
    let sid = if history.is_empty() {
        initial_sid.to_owned()
    } else {
        store.finish_session(initial_sid)?;
        if select_history {
            continue_history(&store, lid, initial_sid)?
        } else {
            s(&store.create_session(lid)?, "session_id").to_owned()
        }
    };
    let rid = uid("fidelity_request");
    let generation = store.bump(&sid, "generating")?;
    let text = s(case, "text");
    let current = store.record_education_event(
        lid,
        &sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    let eid = s(&current, "event_id");
    let turn = store.add_turn(
        &sid,
        &rid,
        generation,
        "user",
        text,
        "shown",
        &[eid.to_owned()],
    )?;
    let mut state = store.session(&sid)?["state"].clone();
    state["latest_input"] = json!(text);
    store.checkpoint(&sid, &state)?;
    let session = store.session(&sid)?;
    let context = build_context(
        &store,
        &session,
        &learner,
        text,
        eid,
        &json!({"selected_status":"unknown","selected_track_key":null,"frame_age_ms":null}),
    )?;
    ensure!(context["user_input"] == text && context["language"] == case["language"]);
    ensure!(
        context["task"]["reported_result"] == ""
            && context["task"]["modules"].is_null()
            && context["task"]["stage"] == "goal_setting",
        "History leaked into current fast state"
    );
    required.push(json!({"event":current,"memory_id":null}));
    let coverage = source_coverage(&context, &required);
    let request = json!({"request_id":rid,"learner_id":lid,"session_id":sid,"generation":generation,"context":context});
    ensure!(
        !request.to_string().contains(s(case, "id"))
            && !request.to_string().contains("\"review\"")
            && !request.to_string().contains("\"expected\"")
            && !request.to_string().contains("\"failures\""),
        "Review labels leaked into request"
    );
    Ok((
        json!({"case_id":case["id"],"language":case["language"],"request":request,
        "preparation":{"coverage_complete":coverage["complete"],
            "status":if coverage["complete"] == true {"ready"} else {"prepared_insufficient"}}}),
        json!({"case_id":case["id"],"group":case["group"],"language":case["language"],
            "declared_label":"positive_fidelity","review_criteria":case["review"],
            "actual_sources_and_memories":required,"stored_memories":store.memories(lid)?,
            "actual_source_session":store.session(initial_sid)?,"actual_current_session":session,
            "actual_current_user_turn_id":turn,"source_coverage":coverage,
            "explicit_history_selected":!history.is_empty() && select_history,
            "context_unmodified":true,"semantic_quality_pass":null,
            "semantic_review_status":"pending_independent_review"}),
    ))
}

fn prepare(root: &Path, output: &Path) -> Result<()> {
    for (name, bytes) in [
        ("answer_fidelity_probe.rs", SOURCE),
        ("tutor_probe_support.rs", SUPPORT_SOURCE),
        ("answer_fidelity_cases_v1.json", FIXTURE.as_bytes()),
        ("answer_fidelity_method_v1.json", METHOD.as_bytes()),
    ] {
        checked(&root.join("scripts/validation").join(name), &hash(bytes))?;
    }
    let mut requests = vec![];
    let mut reviews = vec![];
    let policy = read(&root.join(POLICY))?;
    let schema: Value = serde_json::from_slice(&read(&root.join(SCHEMA))?)?;
    for case in cases()? {
        let (row, review) = build_case(&case)?;
        ai_tutor::kimi::build_prompt(std::str::from_utf8(&policy)?, &schema, &row["request"])?;
        requests.push(row);
        reviews.push(review);
    }
    ensure!(requests.len() == 10, "Expected ten fixed cases");
    let ready = requests
        .iter()
        .filter(|row| row["preparation"]["coverage_complete"] == true)
        .count();
    fs::create_dir(output)?;
    let requests_sha = write_new(&output.join("requests.json"), &json!(requests))?;
    let reviews_sha = write_new(&output.join("review-cases.json"), &json!(reviews))?;
    let method_sha = write_new(&output.join("method.json"), &serde_json::from_str(METHOD)?)?;
    let fixture_sha = write_new(
        &output.join("fixture.json"),
        &serde_json::from_str(FIXTURE)?,
    )?;
    write_new(
        &output.join("manifest.json"),
        &json!({"status":"prepared_no_provider",
        "probe_source_sha256":hash(SOURCE),"support_source_sha256":hash(SUPPORT_SOURCE),
        "fixture_source_sha256":hash(FIXTURE.as_bytes()),"method_source_sha256":hash(METHOD.as_bytes()),
        "executable":executable()?,"production_sources":source_hashes(root)?,"policy_sha256":hash(&policy),
        "requests_sha256":requests_sha,"review_cases_sha256":reviews_sha,
        "method_file_sha256":method_sha,"fixture_file_sha256":fixture_sha,
        "case_count":10,"temporary_simulator_stores":10,"ready_cases":ready,"prepared_insufficient_cases":10-ready,
        "provider_constructed":false,"devices_opened":false,"displayed":false,"played":false,
        "candidates_applied":false,"semantic_quality_pass":null}),
    )?;
    println!(
        "Prepared ten synthetic real-Store contexts: {ready} have all required sources; no provider constructed."
    );
    Ok(())
}

fn eligible_indices(rows: &[Value], selected: &[usize]) -> (Vec<usize>, Vec<usize>) {
    selected
        .iter()
        .copied()
        .partition(|&i| rows[i]["preparation"]["coverage_complete"] == true)
}

async fn run(root: &Path, prepared: &Path, manifest_sha: &str, case: Option<&str>) -> Result<()> {
    let manifest: Value =
        serde_json::from_slice(&checked(&prepared.join("manifest.json"), manifest_sha)?)?;
    ensure!(
        manifest["probe_source_sha256"] == hash(SOURCE)
            && manifest["support_source_sha256"] == hash(SUPPORT_SOURCE)
            && manifest["fixture_source_sha256"] == hash(FIXTURE.as_bytes())
            && manifest["method_source_sha256"] == hash(METHOD.as_bytes()),
        "Prepared diagnostic source changed"
    );
    ensure!(
        manifest["executable"] == executable()?
            && manifest["production_sources"] == json!(source_hashes(root)?),
        "Prepared executable/production sources changed"
    );
    let rows: Vec<Value> = serde_json::from_slice(&checked(
        &prepared.join("requests.json"),
        s(&manifest, "requests_sha256"),
    )?)?;
    for (name, key) in [
        ("review-cases.json", "review_cases_sha256"),
        ("method.json", "method_file_sha256"),
        ("fixture.json", "fixture_file_sha256"),
    ] {
        checked(&prepared.join(name), s(&manifest, key))?;
    }
    let fixture = cases()?;
    ensure!(
        rows.len() == 10
            && rows
                .iter()
                .zip(&fixture)
                .all(|(row, case)| row["case_id"] == case["id"]
                    && row["request"]["context"]["user_input"] == case["text"]),
        "Prepared case order/input changed"
    );
    let indices = selected(&rows, case)?;
    preflight_attempts(prepared, &rows, &indices)?;
    let (eligible, insufficient) = eligible_indices(&rows, &indices);
    for &i in &insufficient {
        write_new(
            &prepared.join(format!("not-run-{}.json", s(&rows[i], "case_id"))),
            &json!({"case_id":rows[i]["case_id"],"request_id":rows[i]["request"]["request_id"],
                "status":"prepared_insufficient","declared_label":"positive_fidelity",
                "model_called":false,"semantic_quality_pass":null,"reason":"Required source absent from unchanged production context"}),
        )?;
    }
    let selection_id = uid("selection");
    write_new(
        &prepared.join(format!("{selection_id}-start.json")),
        &json!({
        "selected_cases":indices.iter().map(|&i| &rows[i]["case_id"]).collect::<Vec<_>>(),
        "model_eligible_cases":eligible.iter().map(|&i| &rows[i]["case_id"]).collect::<Vec<_>>(),
        "prepared_insufficient_cases":insufficient.iter().map(|&i| &rows[i]["case_id"]).collect::<Vec<_>>(),
        "prepared_manifest_sha256":manifest_sha,"automatic_retries":0}),
    )?;
    let operation = if eligible.is_empty() {
        Ok(())
    } else {
        run_model_cases(root, prepared, manifest_sha, &rows, &eligible).await
    };
    write_new(
        &prepared.join(format!("{selection_id}-summary.json")),
        &json!({
        "selected_count":indices.len(),"model_eligible_count":eligible.len(),"prepared_insufficient_count":insufficient.len(),
        "operation_error":operation.as_ref().err().map(ToString::to_string),
        "protocol_scope_passed":operation.is_ok() && insufficient.is_empty(),"semantic_quality_pass":null,
        "displayed":false,"played":false,"candidates_applied":false,"devices_opened":false}),
    )?;
    operation?;
    ensure!(
        insufficient.is_empty(),
        "Prepared source coverage incomplete; cases preserved as NOT_RUN"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let (mut output, mut prepared, mut manifest_sha, mut case) = (None, None, None, None);
    let (mut real, mut all) = (false, false);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--root" => root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root"))?),
            "--output" => {
                ensure!(output.is_none());
                output = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing output"))?,
                ));
            }
            "--prepared" => {
                ensure!(prepared.is_none());
                prepared = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing preparation"))?,
                ));
            }
            "--manifest-sha256" => {
                ensure!(manifest_sha.is_none());
                manifest_sha = Some(args.next().ok_or_else(|| anyhow!("Missing hash"))?);
            }
            "--case" => {
                ensure!(case.is_none() && !all);
                case = Some(args.next().ok_or_else(|| anyhow!("Missing case"))?);
            }
            "--all" => {
                ensure!(case.is_none() && !all);
                all = true;
            }
            "--real-synthetic" => {
                ensure!(!real);
                real = true;
            }
            _ => bail!("Unknown diagnostic argument"),
        }
    }
    let root = root.canonicalize()?;
    if real {
        ensure!(
            output.is_none() && (case.is_some() || all),
            "Explicit case or all required"
        );
        run(
            &root,
            &prepared.ok_or_else(|| anyhow!("--prepared required"))?,
            &manifest_sha.ok_or_else(|| anyhow!("--manifest-sha256 required"))?,
            case.as_deref(),
        )
        .await
    } else {
        ensure!(
            prepared.is_none() && manifest_sha.is_none() && case.is_none() && !all,
            "Explicit --real-synthetic required; no provider called"
        );
        prepare(
            &root,
            &output.ok_or_else(|| anyhow!("--output NEW_DIR required"))?,
        )
    }
}

#[cfg(test)]
#[path = "answer_fidelity_tests.rs"]
mod tests;

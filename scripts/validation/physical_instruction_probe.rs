//! Eight frozen synthetic physical-instruction cases; no device or delivery.
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

const SOURCE: &[u8] = include_bytes!("physical_instruction_probe.rs");
const FIXTURE: &str = include_str!("physical_instruction_cases_v1.json");
const METHOD: &str = include_str!("physical_instruction_method_v1.json");
const HELP: &str = "physical-instruction-probe --root PATH --output NEW_DIR\nDefault: prepare eight real-Store contexts, no provider.\nphysical-instruction-probe --root PATH --prepared DIR --manifest-sha256 SHA --real-synthetic (--case ID | --all)\nOne attempt per selected case; validated text retained for review, never displayed/played/applied.";

fn cases() -> Result<Vec<Value>> {
    let fixture: Value = serde_json::from_str(FIXTURE)?;
    Ok(fixture["cases"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing fixture cases"))?
        .clone())
}
fn build_case(case: &Value) -> Result<(Value, Value)> {
    let directory = TemporaryStore(std::env::temp_dir().join(uid("physical-instruction")));
    fs::create_dir(&directory.0)?;
    // Store is dropped before its precisely owned temporary directory.
    let store = Store::open(directory.0.join("case.sqlite3"), "simulator")?;
    let learner = store.create_learner(
        "SYNTHETIC_PHYSICAL_INSTRUCTION",
        s(case, "language"),
        "guided",
    )?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let rid = uid("physical_request");
    let generation = store.bump(sid, "generating")?;
    let text = s(case, "text");
    let event = store.record_education_event(
        lid,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    let eid = s(&event, "event_id");
    let turn = store.add_turn(
        sid,
        &rid,
        generation,
        "user",
        text,
        "shown",
        &[eid.to_owned()],
    )?;
    let mut state = store.session(sid)?["state"].clone();
    state["latest_input"] = json!(text);
    store.checkpoint(sid, &state)?;
    let session = store.session(sid)?;
    let context = build_context(
        &store,
        &session,
        &learner,
        text,
        eid,
        &json!({"selected_status":"unknown","selected_track_key":null,"frame_age_ms":null}),
    )?;
    ensure!(
        context["user_input"] == text && context["language"] == case["language"],
        "Production context changed current input"
    );
    ensure!(
        context["slow_memory"] == json!([])
            && context["task"]["modules"].is_null()
            && context["task"]["reported_result"] == ""
            && context["task"]["stage"] == "goal_setting",
        "Fixture seeded unsupported state"
    );
    ensure!(
        context["evidence"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1 && rows[0]["event_id"] == event["event_id"]),
        "Unexpected source injection"
    );
    let request = json!({"request_id":rid,"learner_id":lid,"session_id":sid,"generation":generation,"context":context});
    // Review expectations stay outside the unmodified production request.
    ensure!(
        !request.to_string().contains(s(case, "id"))
            && !request.to_string().contains("\"expected\"")
            && !request.to_string().contains("\"failures\""),
        "Review data leaked into request"
    );
    Ok((
        json!({"case_id":case["id"],"language":case["language"],"request":request}),
        json!({"case_id":case["id"],"scenario":case["scenario"],"language":case["language"],"review_criteria":case["review"],
        "actual_source":event,"actual_session":session,"actual_user_turn_id":turn,"context_unmodified":true,
        "semantic_review_status":"pending_independent_review","semantic_quality_pass":null}),
    ))
}
fn prepare(root: &Path, output: &Path) -> Result<()> {
    for (name, bytes) in [
        ("physical_instruction_probe.rs", SOURCE),
        ("tutor_probe_support.rs", SUPPORT_SOURCE),
        ("physical_instruction_cases_v1.json", FIXTURE.as_bytes()),
        ("physical_instruction_method_v1.json", METHOD.as_bytes()),
    ] {
        checked(&root.join("scripts/validation").join(name), &hash(bytes))?;
    }
    let mut requests = vec![];
    let mut reviews = vec![];
    let policy = read(&root.join(POLICY))?;
    let schema: Value = serde_json::from_slice(&read(&root.join(SCHEMA))?)?;
    for case in cases()? {
        let (request, review) = build_case(&case)?;
        ai_tutor::kimi::build_prompt(std::str::from_utf8(&policy)?, &schema, &request["request"])?;
        requests.push(request);
        reviews.push(review);
    }
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
        &json!({"status":"prepared_no_provider","probe_source_sha256":hash(SOURCE),"support_source_sha256":hash(SUPPORT_SOURCE),
        "fixture_source_sha256":hash(FIXTURE.as_bytes()),"method_source_sha256":hash(METHOD.as_bytes()),
        "executable":executable()?,"production_sources":source_hashes(root)?,"policy_sha256":hash(&policy),
        "requests_sha256":requests_sha,"review_cases_sha256":reviews_sha,"method_file_sha256":method_sha,"fixture_file_sha256":fixture_sha,
        "case_count":8,"temporary_simulator_stores":8,"provider_constructed":false,"devices_opened":false,
        "displayed":false,"played":false,"candidates_applied":false,"semantic_quality_pass":null}),
    )?;
    println!("Prepared eight synthetic real-Store contexts; no provider constructed.");
    Ok(())
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
        rows.len() == 8
            && rows
                .iter()
                .zip(&fixture)
                .all(|(r, c)| r["case_id"] == c["id"]
                    && r["request"]["context"]["user_input"] == c["text"]),
        "Prepared case order/input changed"
    );
    let indices = selected(&rows, case)?;
    run_model_cases(root, prepared, manifest_sha, &rows, &indices).await
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
                ensure!(output.is_none(), "Duplicate output");
                output = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing output"))?,
                ));
            }
            "--prepared" => {
                ensure!(prepared.is_none(), "Duplicate preparation");
                prepared = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing preparation"))?,
                ));
            }
            "--manifest-sha256" => {
                ensure!(manifest_sha.is_none(), "Duplicate hash");
                manifest_sha = Some(args.next().ok_or_else(|| anyhow!("Missing hash"))?);
            }
            "--case" => {
                ensure!(case.is_none() && !all, "Choose one selector");
                case = Some(args.next().ok_or_else(|| anyhow!("Missing case"))?);
            }
            "--all" => {
                ensure!(case.is_none() && !all, "Choose one selector");
                all = true;
            }
            "--real-synthetic" => {
                ensure!(!real, "Duplicate intent");
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
#[path = "physical_instruction_tests.rs"]
mod tests;

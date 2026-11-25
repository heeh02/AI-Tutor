//! Three unobserved bilingual/mode slots; all prior successes and failures stay separate.
use super::deepseek_client::{ENDPOINT, MAX_TOKENS, MODEL};
use super::*;
use crate::{store::Store, tutor::apply_reply};

const PLAN: &str = include_str!("../../../scripts/validation/deepseek_flash_three_plan_v1.json");
const MAX_CALLS: u64 = 3;
const SCHEMA: &str = "regular/implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json";

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write_new(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    Ok(())
}
struct Pair {
    id: String,
    request: Value,
    user: String,
    system: String,
    source_sha: String,
}
fn load_pairs(archive: &Path, plan: &Value, output: &Path) -> Result<Vec<Pair>> {
    for (relative, hash) in plan["input_sha256"]
        .as_object()
        .ok_or_else(|| anyhow!("input_manifest_missing"))?
    {
        ensure!(
            sha_file(&archive.join(relative))? == hash.as_str().unwrap_or(""),
            "archive_hash_mismatch"
        );
    }
    let policy = fs::read_to_string(archive.join("regular/prompts/tutor_core.md"))?;
    let schema: Value =
        serde_json::from_slice(&read_bounded_file(&archive.join(SCHEMA), 128 * 1024)?)?;
    let agent =
        fs::read_to_string(archive.join("regular/prompts/tutor-agent.md"))?.replace("\r\n", "\n");
    ensure!(agent.starts_with("---\n"), "agent_header_invalid");
    let system = agent[4..]
        .split_once("\n---\n")
        .ok_or_else(|| anyhow!("agent_body_missing"))?
        .1
        .trim()
        .to_owned();
    let mut pairs = vec![];
    for id in ["zh", "en"] {
        let request: Value = serde_json::from_slice(&read_bounded_file(
            &archive.join(format!("request-{id}.json")),
            128 * 1024,
        )?)?;
        ensure!(
            request["context"]["language"] == id && request["context"]["namespace"] == "simulator",
            "archive_case_mismatch"
        );
        let user = stream_prompt(&policy, schema.clone(), &request)?;
        ensure!(
            digest(user.as_bytes())
                == plan["pairs"][id]["user_prompt_sha256"]
                    .as_str()
                    .unwrap_or(""),
            "archived_user_prompt_changed"
        );
        let db = archive.join(format!("source-{id}.sqlite3"));
        for suffix in ["-wal", "-shm"] {
            ensure!(
                !PathBuf::from(format!("{}{suffix}", db.display())).exists(),
                "archive_database_not_closed"
            );
        }
        for arm in ["non_thinking", "thinking_low"] {
            fs::copy(&db, output.join(format!("{id}-{arm}.sqlite3")))?;
        }
        write_new(&output.join(format!("request-{id}.json")), &request)?;
        pairs.push(Pair {
            id: id.into(),
            request,
            user,
            system: system.clone(),
            source_sha: plan["pairs"][id]["source_sha256"]
                .as_str()
                .ok_or_else(|| anyhow!("source_hash_missing"))?
                .into(),
        });
    }
    Ok(pairs)
}
fn validate_plan(plan: &Value) -> Result<()> {
    ensure!(
        plan["endpoint"] == ENDPOINT
            && plan["model"] == MODEL
            && plan["max_tokens"] == MAX_TOKENS
            && plan["request_deadline_seconds"] == 90
            && plan["maximum_calls"] == MAX_CALLS
            && plan["automatic_retries"] == 0,
        "fixed_plan_changed"
    );
    ensure!(
        plan["schedule"]
            == json!([{"case":"zh","arm":"thinking_low"},{"case":"en","arm":"thinking_low"},{"case":"en","arm":"non_thinking"}]),
        "fixed_order_changed"
    );
    ensure!(
        plan["arms"]
            == json!({"non_thinking":{"thinking":{"type":"disabled"}},"thinking_low":{"thinking":{"type":"enabled"},"reasoning_effort":"low"}}),
        "fixed_arm_parameters_changed"
    );
    Ok(())
}
async fn run(root: &Path, output: &Path, report: &mut Value) -> Result<()> {
    let plan: Value = serde_json::from_str(PLAN)?;
    validate_plan(&plan)?;
    let archive = root.join(s(&plan, "archive_relative"));
    let pairs = load_pairs(&archive, &plan, output)?;
    let prereg = json!({"plan":plan,"plan_sha256":digest(PLAN.as_bytes()),"actual_executable_sha256":sha_file(&std::env::current_exe()?)?,
        "pairs":pairs.iter().map(|p|json!({"case":p.id,"request_sha256":digest(&serde_json::to_vec(&p.request).unwrap()),"source_sha256":p.source_sha,
            "materialized_user_sha256":digest(p.user.as_bytes()),"materialized_system_sha256":digest(p.system.as_bytes())})).collect::<Vec<_>>(),
        "clock":"Instant at call invocation before DPAPI/header preparation; HTTP send offset retained separately. Complete-segment client receipts, not server token times.",
        "credential_path_scope":"LOCALAPPDATA/AI Tutor/secrets/deepseek.dpapi; CurrentUser DPAPI, no entropy, sensitive per-request header",
        "raw_reasoning_retained":false,"collected_not_ui_or_audio_delivered":true});
    write_new(&output.join("preregistered.json"), &prereg)?;
    report["preregistration_sha256"] = json!(sha_file(&output.join("preregistered.json"))?);
    let client = super::deepseek_client::new_client()?;
    for (index, slot) in plan["schedule"].as_array().unwrap().iter().enumerate() {
        let case = s(slot, "case");
        let arm = s(slot, "arm");
        let pair = pairs
            .iter()
            .find(|p| p.id == case)
            .ok_or_else(|| anyhow!("case_missing"))?;
        let store = Store::open(output.join(format!("{case}-{arm}.sqlite3")), "simulator")?;
        let ids: Vec<_> = pair.request["context"]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| s(v, "event_id").to_owned())
            .collect();
        ensure!(
            digest(&serde_json::to_vec(
                &store.source_events(s(&pair.request, "learner_id"), &ids)?
            )?) == pair.source_sha,
            "source_records_changed"
        );
        let mut row = json!({"slot":index,"case":case,"arm":arm,"request_attempted":false,"mechanical_pass":false,
            "requested_model":MODEL,"requested_parameters":plan["arms"][arm],"max_tokens":MAX_TOKENS,"http_request_ids":{},"http_eof":false,
            "source_sha256":pair.source_sha,"materialized_user_sha256":digest(pair.user.as_bytes()),"materialized_system_sha256":digest(pair.system.as_bytes()),
            "semantic_review":"REQUIRED","collected_not_delivered":true});
        let result = super::deepseek_client::call(
            &client,
            &pair.request,
            &pair.system,
            &pair.user,
            arm,
            &mut row,
        )
        .await;
        let checked = (|| -> Result<()> {
            let value = result
                .as_ref()
                .map_err(|error| anyhow!(error.to_string()))?;
            row["final_reply"] = value.reply().clone();
            let epoch = store.learner(s(&pair.request, "learner_id"))?["epoch"]
                .as_i64()
                .unwrap();
            row["candidate_application"] =
                apply_reply(&store, s(&pair.request, "session_id"), value.reply(), epoch)?;
            row["state_after_application"] =
                store.session(s(&pair.request, "session_id"))?["state"].clone();
            row["memories_after_application"] =
                json!(store.memories(s(&pair.request, "learner_id"))?);
            Ok(())
        })();
        row["mechanical_pass"] = json!(checked.is_ok());
        row["failure_code"] = checked
            .as_ref()
            .err()
            .map(|e| json!(e.to_string()))
            .unwrap_or(Value::Null);
        report["calls"].as_array_mut().unwrap().push(row);
        report["requests_attempted"] = json!(
            report["calls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["request_attempted"] == true)
                .count()
        );
        report["not_attempted_slots"] =
            json!(MAX_CALLS - report["requests_attempted"].as_u64().unwrap());
        report["unvisited_schedule_slots"] =
            json!(MAX_CALLS as usize - report["calls"].as_array().unwrap().len());
        write_new(
            &output.join(format!("call-{}.json", index + 1)),
            &report["calls"][index],
        )?;
        checked?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Three separate DeepSeek Flash mode/language calls; prior trials stay unchanged"]
async fn real_deepseek_flash_three_diagnostic_only() -> Result<()> {
    let path = PathBuf::from(
        std::env::var_os("AI_TUTOR_DEEPSEEK_FLASH_PLAN")
            .ok_or_else(|| anyhow!("explicit_paths_required"))?,
    );
    let paths: Value = serde_json::from_slice(&read_bounded_file(&path, 16 * 1024)?)?;
    let root = PathBuf::from(s(&paths, "root"));
    let output = PathBuf::from(s(&paths, "output"));
    ensure!(
        root.is_absolute() && root.is_dir() && output.is_absolute() && !output.exists(),
        "absolute_root_and_fresh_output_required"
    );
    fs::create_dir(&output)?;
    write_new(
        &output.join("owner.json"),
        &json!({"pid":std::process::id(),"plan_sha256":digest(PLAN.as_bytes()),"maximum_calls":MAX_CALLS}),
    )?;
    let mut report = json!({"mechanical_pass":false,"calls":[],"requests_attempted":0,"maximum_calls":MAX_CALLS,"not_attempted_slots":MAX_CALLS,"unvisited_schedule_slots":MAX_CALLS,"automatic_retries":0,
        "semantic_review":"PENDING_NOT_CLASSROOM_ACCEPTANCE","raw_reasoning_retained":false,"production_provider_changed":false,
        "identity_scope":"Requested alias and returned metadata are recorded, not independent backend revision attestation.",
        "application_scope":"Current evaluator differs from archived Kimi application code; compare raw candidates separately."});
    let result = run(&root, &output, &mut report).await;
    report["mechanical_pass"] = json!(result.is_ok());
    report["failure_code"] = result
        .as_ref()
        .err()
        .map(|e| json!(e.to_string()))
        .unwrap_or(Value::Null);
    write_new(&output.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"mechanical_pass":result.is_ok(),"requests_attempted":report["requests_attempted"],"semantic_review":"REQUIRED","report":output.join("report.json")})
    );
    result
}

#[test]
fn fixed_three_call_plan_remains_valid() -> Result<()> {
    let plan: Value = serde_json::from_str(PLAN)?;
    validate_plan(&plan)
}

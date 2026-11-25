//! Explicit, hash-bound paired teaching evaluation. Never selects a production model.
use super::deepseek_client;
use super::deepseek_client::StreamContract;
use super::*;
use crate::{context::build_context, store::Store, tutor::apply_reply};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: String,
    sha256: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    case: String,
    policy: String,
    mode: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    experiment_id: String,
    model: String,
    maximum_calls: usize,
    automatic_retries: u32,
    max_tokens: u64,
    request_deadline_seconds: u64,
    #[serde(default)]
    contract: Option<String>,
    cases: Artifact,
    baseline_policy: Artifact,
    revised_policy: Artifact,
    agent: Artifact,
    reply_schema: Artifact,
    schedule: Vec<Slot>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    language: String,
    text: String,
    #[serde(default)]
    history: Vec<Value>,
    #[serde(default)]
    group: Option<String>,
}
struct Prepared {
    request: Value,
    source_sha256: String,
    users: BTreeMap<String, String>,
}
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
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'_')
}
fn read_artifact(root: &Path, output: &Path, name: &str, item: &Artifact) -> Result<Vec<u8>> {
    let path = PathBuf::from(&item.path).canonicalize()?;
    ensure!(path.starts_with(root), "input_outside_project");
    let bytes = read_bounded_file(&path, 256 * 1024)?;
    ensure!(digest(&bytes) == item.sha256, "input_hash_mismatch");
    let target = output.join("inputs").join(name);
    use std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?
        .write_all(&bytes)?;
    Ok(bytes)
}
fn parse_cases(bytes: &[u8]) -> Result<Vec<Case>> {
    let bundle: Value = serde_json::from_slice(bytes)?;
    ensure!(bundle["synthetic_only"] == true, "synthetic_cases_required");
    let cases: Vec<Case> = serde_json::from_value(bundle["cases"].clone())?;
    ensure!((1..=8).contains(&cases.len()), "case_budget_invalid");
    let mut ids = BTreeSet::new();
    for case in &cases {
        ensure!(
            valid_id(&case.id) && ids.insert(case.id.clone()),
            "case_id_invalid"
        );
        ensure!(
            ["zh", "en"].contains(&case.language.as_str()),
            "case_language_invalid"
        );
        ensure!(
            !case.text.trim().is_empty() && case.text.chars().count() <= 8000,
            "case_text_invalid"
        );
        ensure!(
            case.history.is_empty(),
            "this_version_requires_current_statements"
        );
        // group is evaluator bookkeeping; it never enters a request.
        let _ = &case.group;
    }
    Ok(cases)
}
fn plan_contract(plan: &Plan) -> Result<StreamContract> {
    StreamContract::parse(plan.contract.as_deref().unwrap_or("legacy_v1"))
}
fn validate_plan(plan: &Plan, cases: &[Case]) -> Result<()> {
    plan_contract(plan)?;
    ensure!(
        plan.schema_version == 1 && valid_id(&plan.experiment_id),
        "plan_version_or_id_invalid"
    );
    ensure!(
        plan.model == "deepseek-flash"
            && plan.max_tokens == 8192
            && plan.request_deadline_seconds == 90
            && plan.automatic_retries == 0,
        "fixed_transport_budget_changed"
    );
    ensure!(
        plan.maximum_calls == cases.len() * 2
            && plan.maximum_calls <= 16
            && plan.schedule.len() == plan.maximum_calls,
        "paired_request_budget_invalid"
    );
    let ids: BTreeSet<_> = cases.iter().map(|c| c.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    let mut modes = BTreeMap::new();
    for slot in &plan.schedule {
        ensure!(
            ids.contains(slot.case.as_str())
                && ["baseline", "revised"].contains(&slot.policy.as_str()),
            "slot_binding_invalid"
        );
        ensure!(
            ["non_thinking", "thinking_low"].contains(&slot.mode.as_str()),
            "mode_invalid"
        );
        ensure!(
            seen.insert((slot.case.clone(), slot.policy.clone())),
            "duplicate_slot"
        );
        if let Some(old) = modes.insert(slot.case.clone(), slot.mode.clone()) {
            ensure!(old == slot.mode, "paired_modes_differ");
        }
    }
    Ok(())
}
fn prepare_case(
    output: &Path,
    case: &Case,
    policies: &BTreeMap<String, String>,
    schema: &Value,
    contract: StreamContract,
) -> Result<Prepared> {
    let template = output.join(format!("source-{}.sqlite3", case.id));
    let store = Store::open(&template, "simulator")?;
    let learner = store.create_learner("SYNTHETIC_TEACHING_EVAL", &case.language, "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let generation = store.bump(sid, "idle")?;
    let event = store.record_education_event(
        lid,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":case.text}),
    )?;
    let mut state = store.session(sid)?["state"].clone();
    state["latest_input"] = json!(case.text);
    store.checkpoint(sid, &state)?;
    let context = build_context(
        &store,
        &store.session(sid)?,
        &learner,
        &case.text,
        s(&event, "event_id"),
        &json!({}),
    )?;
    let request = json!({"request_id":format!("quality-{}",uuid::Uuid::new_v4()),
        "learner_id":lid,"session_id":sid,"generation":generation,"context":context});
    let source_sha256 = digest(&serde_json::to_vec(
        &store.source_events(lid, &[s(&event, "event_id").to_owned()])?,
    )?);
    store.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(store);
    for policy in ["baseline", "revised"] {
        fs::copy(
            &template,
            output.join(format!("{}-{policy}.sqlite3", case.id)),
        )?;
    }
    let mut users = BTreeMap::new();
    for (name, policy) in policies {
        let user = match contract {
            StreamContract::LegacyV1 => stream_prompt(policy, schema.clone(), &request)?,
            StreamContract::MetadataV2 => stream_prompt_metadata(policy, schema.clone(), &request)?,
        };
        use std::io::Write;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join(format!("prompt-{}-{name}.txt", case.id)))?
            .write_all(user.as_bytes())?;
        users.insert(name.clone(), user);
    }
    write_new(&output.join(format!("request-{}.json", case.id)), &request)?;
    Ok(Prepared {
        request,
        source_sha256,
        users,
    })
}
fn model_contract_failure(row: &Value, error: &anyhow::Error) -> bool {
    row["api_stream_complete"] == true
        && row["contract_failure_code"]
            .as_str()
            .is_some_and(|v| !v.is_empty() && v == error.to_string())
        && row["metrics"]["reported_model"] == "deepseek-flash"
}
fn record_cleanup(row: &mut Value, stop: &mut Option<anyhow::Error>, result: Result<()>) {
    row["database_checkpoint_passed"] = json!(result.is_ok());
    if let Err(error) = result {
        row["database_cleanup_error"] = json!(error.to_string());
        if row["failure_kind"].is_null() {
            row["failure_kind"] = json!("database_cleanup");
        }
        if stop.is_none() {
            *stop = Some(error);
        }
    }
}
fn summarize(report: &mut Value, maximum: usize) {
    let rows = report["calls"].as_array().unwrap();
    let visited = rows.len();
    let attempted = rows
        .iter()
        .filter(|r| r["request_attempted"] == true)
        .count();
    let passed = rows
        .iter()
        .filter(|r| r["model_contract_pass"] == true)
        .count();
    let failed = rows
        .iter()
        .filter(|r| r["failure_kind"] == "model_contract")
        .count();
    report["requests_attempted"] = json!(attempted);
    report["model_contract_passes"] = json!(passed);
    report["model_contract_failures"] = json!(failed);
    report["unvisited_slots"] = json!(maximum - visited);
    report["not_attempted_slots"] = json!(maximum - attempted);
}
async fn run(root: &Path, output: &Path, plan_bytes: &[u8], report: &mut Value) -> Result<()> {
    let plan: Plan = serde_json::from_slice(plan_bytes)?;
    fs::create_dir(output.join("inputs"))?;
    let case_bytes = read_artifact(root, output, "cases.json", &plan.cases)?;
    let cases = parse_cases(&case_bytes)?;
    validate_plan(&plan, &cases)?;
    let contract = plan_contract(&plan)?;
    let baseline = String::from_utf8(read_artifact(
        root,
        output,
        "baseline.md",
        &plan.baseline_policy,
    )?)?;
    let revised = String::from_utf8(read_artifact(
        root,
        output,
        "revised.md",
        &plan.revised_policy,
    )?)?;
    ensure!(baseline != revised, "policies_must_differ");
    let policies = BTreeMap::from([("baseline".into(), baseline), ("revised".into(), revised)]);
    let agent = String::from_utf8(read_artifact(root, output, "agent.md", &plan.agent)?)?
        .replace("\r\n", "\n");
    ensure!(agent.starts_with("---\n"), "agent_header_invalid");
    let system = agent[4..]
        .split_once("\n---\n")
        .ok_or_else(|| anyhow!("agent_body_missing"))?
        .1
        .trim()
        .to_owned();
    let schema: Value = serde_json::from_slice(&read_artifact(
        root,
        output,
        "reply-schema.json",
        &plan.reply_schema,
    )?)?;
    let mut prepared = BTreeMap::new();
    for case in &cases {
        prepared.insert(
            case.id.clone(),
            prepare_case(output, case, &policies, &schema, contract)?,
        );
    }
    write_new(
        &output.join("preregistered.json"),
        &json!({"plan":serde_json::from_slice::<Value>(plan_bytes)?,
        "plan_sha256":digest(plan_bytes),"binary_sha256":sha_file(&std::env::current_exe()?)?,
        "system_sha256":digest(system.as_bytes()),"maximum_calls":plan.maximum_calls,
        "stream_contract":contract.label(),"reply_text_origin":contract.reply_text_origin(),
        "pairs":prepared.iter().map(|(id,p)|json!({"case":id,"request_sha256":digest(&serde_json::to_vec(&p.request).unwrap()),
            "source_sha256":p.source_sha256,"user_prompt_sha256":p.users.iter().map(|(k,v)|(k.clone(),digest(v.as_bytes()))).collect::<BTreeMap<_,_>>()})).collect::<Vec<_>>(),
        "evaluation_only":true,"collected_not_delivered":true,"rubric_not_loaded":true,
        "failure_policy":"Record and continue only typed model-contract failures with a clean completed API stream. All infrastructure/application failures stop; never retry a slot."}),
    )?;
    report["maximum_calls"] = json!(plan.maximum_calls);
    report["unvisited_slots"] = json!(plan.maximum_calls);
    report["not_attempted_slots"] = json!(plan.maximum_calls);
    let http = deepseek_client::new_client()?;
    for (index, slot) in plan.schedule.iter().enumerate() {
        let pair = &prepared[&slot.case];
        let db_path = output.join(format!("{}-{}.sqlite3", slot.case, slot.policy));
        let store = Store::open(&db_path, "simulator")?;
        let source_ids: Vec<_> = pair.request["context"]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| s(v, "event_id").to_owned())
            .collect();
        ensure!(
            digest(&serde_json::to_vec(
                &store.source_events(s(&pair.request, "learner_id"), &source_ids)?
            )?) == pair.source_sha256,
            "source_records_changed"
        );
        let mut row = json!({"slot":index,"case":slot.case,"policy":slot.policy,"mode":slot.mode,
            "request_attempted":false,"model_contract_pass":false,"candidate_application_complete":false,
            "requested_model":"deepseek-flash","max_tokens":8192,"http_request_ids":{},"http_eof":false,
            "source_sha256":pair.source_sha256,"materialized_user_sha256":digest(pair.users[&slot.policy].as_bytes()),
            "materialized_system_sha256":digest(system.as_bytes()),"semantic_review":"REQUIRED","collected_not_delivered":true});
        let result = deepseek_client::call_with_contract(
            &http,
            &pair.request,
            &system,
            &pair.users[&slot.policy],
            &slot.mode,
            contract,
            &mut row,
        )
        .await;
        let mut stop = None;
        match result {
            Ok(reply) => {
                row["model_contract_pass"] = json!(true);
                row["final_reply"] = reply.reply().clone();
                let applied = (|| -> Result<()> {
                    let epoch = store.learner(s(&pair.request, "learner_id"))?["epoch"]
                        .as_i64()
                        .unwrap();
                    row["candidate_application"] =
                        apply_reply(&store, s(&pair.request, "session_id"), reply.reply(), epoch)?;
                    row["state_after_application"] =
                        store.session(s(&pair.request, "session_id"))?["state"].clone();
                    row["memories_after_application"] =
                        json!(store.memories(s(&pair.request, "learner_id"))?);
                    Ok(())
                })();
                if let Err(error) = applied {
                    row["failure_kind"] = json!("application");
                    row["failure_code"] = json!(error.to_string());
                    stop = Some(error);
                } else {
                    row["candidate_application_complete"] = json!(true);
                    row["failure_kind"] = Value::Null;
                }
            }
            Err(error) => {
                row["failure_code"] = json!(error.to_string());
                if model_contract_failure(&row, &error) {
                    row["failure_kind"] = json!("model_contract");
                } else {
                    row["failure_kind"] = json!("infrastructure");
                    stop = Some(error);
                }
            }
        }
        record_cleanup(
            &mut row,
            &mut stop,
            store
                .db
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(Into::into),
        );
        drop(store);
        report["calls"].as_array_mut().unwrap().push(row);
        summarize(report, plan.maximum_calls);
        write_new(
            &output.join(format!("call-{:03}.json", index + 1)),
            &report["calls"][index],
        )?;
        if let Some(error) = stop {
            return Err(error);
        }
    }
    report["experiment_completed"] = json!(true);
    Ok(())
}

#[tokio::test]
#[ignore = "Explicit hash-bound teaching evaluation; up to16 real Flash requests with root-owned DPAPI credential"]
async fn real_teaching_policy_evaluation_only() -> Result<()> {
    let launch = PathBuf::from(
        std::env::var_os("AI_TUTOR_TEACHING_EVAL_PLAN")
            .ok_or_else(|| anyhow!("explicit_launch_required"))?,
    );
    let paths: Value = serde_json::from_slice(&read_bounded_file(&launch, 16 * 1024)?)?;
    let root = PathBuf::from(s(&paths, "root")).canonicalize()?;
    let output = PathBuf::from(s(&paths, "output"));
    ensure!(
        root.is_dir() && output.is_absolute() && !output.exists(),
        "fresh_absolute_output_required"
    );
    let plan_path = PathBuf::from(s(&paths, "plan_path")).canonicalize()?;
    ensure!(plan_path.starts_with(&root), "plan_outside_project");
    let plan_bytes = read_bounded_file(&plan_path, 128 * 1024)?;
    ensure!(
        digest(&plan_bytes) == s(&paths, "plan_sha256"),
        "plan_hash_mismatch"
    );
    fs::create_dir(&output)?;
    write_new(
        &output.join("owner.json"),
        &json!({"pid":std::process::id(),"launch_sha256":sha_file(&launch)?,"plan_sha256":digest(&plan_bytes)}),
    )?;
    let mut report = json!({"experiment_completed":false,"infrastructure_pass":false,"calls":[],
        "requests_attempted":0,"model_contract_passes":0,"model_contract_failures":0,
        "automatic_retries":0,"semantic_review":"PENDING","production_default_changed":false,
        "scope":"Diagnostic completion is not a model-quality pass. No UI/audio delivery, no rubric/model-grader calls."});
    let result = run(&root, &output, &plan_bytes, &mut report).await;
    report["infrastructure_pass"] = json!(result.is_ok());
    report["infrastructure_error"] = result
        .as_ref()
        .err()
        .map(|e| json!(e.to_string()))
        .unwrap_or(Value::Null);
    write_new(&output.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"experiment_completed":report["experiment_completed"],"requests_attempted":report["requests_attempted"],
        "model_contract_passes":report["model_contract_passes"],"model_contract_failures":report["model_contract_failures"],"semantic_review":"REQUIRED"})
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> Plan {
        let a = Artifact {
            path: "unused".into(),
            sha256: "unused".into(),
        };
        Plan {
            schema_version: 1,
            experiment_id: "unit".into(),
            model: "deepseek-flash".into(),
            maximum_calls: 2,
            automatic_retries: 0,
            max_tokens: 8192,
            request_deadline_seconds: 90,
            contract: None,
            cases: a.clone(),
            baseline_policy: a.clone(),
            revised_policy: a.clone(),
            agent: a.clone(),
            reply_schema: a,
            schedule: vec![
                Slot {
                    case: "case_id_canary".into(),
                    policy: "baseline".into(),
                    mode: "thinking_low".into(),
                },
                Slot {
                    case: "case_id_canary".into(),
                    policy: "revised".into(),
                    mode: "thinking_low".into(),
                },
            ],
        }
    }
    fn case() -> Case {
        Case {
            id: "case_id_canary".into(),
            language: "en".into(),
            text: "I have not driven it yet.".into(),
            history: vec![],
            group: Some("private_group_canary".into()),
        }
    }
    #[test]
    fn plan_requires_one_equal_mode_pair_per_case_without_retries() -> Result<()> {
        let cases = vec![case()];
        let mut p = plan();
        validate_plan(&p, &cases)?;
        p.schedule[1].mode = "non_thinking".into();
        assert!(validate_plan(&p, &cases).is_err());
        p = plan();
        p.schedule[1].policy = "baseline".into();
        assert!(validate_plan(&p, &cases).is_err());
        p = plan();
        p.maximum_calls = 18;
        assert!(validate_plan(&p, &cases).is_err());
        p = plan();
        p.automatic_retries = 1;
        assert!(validate_plan(&p, &cases).is_err());
        Ok(())
    }
    #[test]
    fn evaluation_contract_defaults_to_legacy_and_requires_explicit_metadata_choice() -> Result<()>
    {
        let cases = vec![case()];
        let mut value = plan();
        assert_eq!(plan_contract(&value)?, StreamContract::LegacyV1);
        value.contract = Some("metadata_v2".into());
        validate_plan(&value, &cases)?;
        assert_eq!(plan_contract(&value)?, StreamContract::MetadataV2);
        value.contract = Some("automatic".into());
        assert!(validate_plan(&value, &cases).is_err());
        Ok(())
    }
    #[test]
    fn metadata_evaluation_prepares_both_policies_with_one_unchanged_request() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let policy =
            "Return one JSON object only, exactly matching the supplied TutorReply schema.";
        let policies = BTreeMap::from([
            ("baseline".into(), policy.into()),
            ("revised".into(), format!("Be concise. {policy}")),
        ]);
        let schema: Value = serde_json::from_str(include_str!(
            "../../../implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"
        ))?;
        let prepared = prepare_case(
            temp.path(),
            &case(),
            &policies,
            &schema,
            StreamContract::MetadataV2,
        )?;
        let mut inputs = vec![];
        for prompt in prepared.users.values() {
            let after = prompt.split_once("\n\nOUTPUT_SCHEMA:\n").unwrap().1;
            let (schema, data) = after.split_once("\n\nREQUEST_DATA:\n").unwrap();
            let schema: Value = serde_json::from_str(schema)?;
            assert_eq!(
                schema["oneOf"][1]["properties"]["type"]["const"],
                "final_metadata"
            );
            let data: Value = serde_json::from_str(data)?;
            assert_eq!(data["request_id"], prepared.request["request_id"]);
            assert_eq!(data["context"]["user_input"], case().text);
            assert!(!prompt.contains("private_group_canary") && !prompt.contains("case_id_canary"));
            inputs.push(data);
        }
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0], inputs[1]);
        Ok(())
    }
    #[test]
    fn cases_and_real_context_keep_evaluator_fields_out_of_prompts() -> Result<()> {
        let bad = json!({"synthetic_only":true,"cases":[{"id":"unit","language":"en","text":"Hello","review":"RUBRIC_CANARY"}]});
        assert!(parse_cases(&serde_json::to_vec(&bad)?).is_err());
        let temp = tempfile::tempdir()?;
        let policy =
            "Return one JSON object only, exactly matching the supplied TutorReply schema.";
        let policies = BTreeMap::from([
            ("baseline".into(), policy.into()),
            ("revised".into(), format!("Be concise. {policy}")),
        ]);
        let prepared = prepare_case(
            temp.path(),
            &case(),
            &policies,
            &json!({"type":"object"}),
            StreamContract::LegacyV1,
        )?;
        assert_eq!(prepared.request["context"]["user_input"], case().text);
        for text in prepared.users.values() {
            assert!(text.contains(&case().text));
            assert!(
                !text.contains("case_id_canary")
                    && !text.contains("private_group_canary")
                    && !text.contains("RUBRIC_CANARY")
            );
        }
        Ok(())
    }
    #[test]
    fn a_drained_model_failure_is_counted_but_a_transport_failure_cannot_continue() {
        let mut failed = json!({"api_stream_complete":true,"contract_failure_code":"final_text_mismatch",
            "metrics":{"reported_model":"deepseek-flash"},"request_attempted":true,"failure_kind":"model_contract","model_contract_pass":false});
        assert!(model_contract_failure(
            &failed,
            &anyhow!("final_text_mismatch")
        ));
        assert!(!model_contract_failure(
            &failed,
            &anyhow!("http_body_failed")
        ));
        failed["api_stream_complete"] = json!(false);
        assert!(!model_contract_failure(
            &failed,
            &anyhow!("final_text_mismatch")
        ));
        failed["api_stream_complete"] = json!(true);
        let mut stop = None;
        record_cleanup(&mut failed, &mut stop, Err(anyhow!("checkpoint_failed")));
        assert!(stop.is_some());
        assert_eq!(failed["failure_kind"], "model_contract");
        assert_eq!(failed["contract_failure_code"], "final_text_mismatch");
        let mut report = json!({"calls":[failed,{"request_attempted":true,"model_contract_pass":true,"failure_kind":null}]});
        summarize(&mut report, 2);
        assert_eq!(report["requests_attempted"], 2);
        assert_eq!(report["model_contract_passes"], 1);
        assert_eq!(report["model_contract_failures"], 1);
        assert_eq!(report["unvisited_slots"], 0);
        let mut application =
            json!({"failure_kind":"application","failure_code":"application_failed"});
        let mut first = Some(anyhow!("application_failed"));
        record_cleanup(
            &mut application,
            &mut first,
            Err(anyhow!("checkpoint_failed")),
        );
        assert_eq!(first.unwrap().to_string(), "application_failed");
        assert_eq!(application["failure_code"], "application_failed");
        assert_eq!(application["database_cleanup_error"], "checkpoint_failed");
    }
}

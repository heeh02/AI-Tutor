//! Opt-in four-call experiment. Reuses production ACP, validation and cleanup.
use super::*;
use crate::{context::build_context, store::Store, tutor::apply_reply};

const PLAN: &str = include_str!("../../../scripts/validation/kimi_highspeed_plan_v1.json");
const INPUTS: &[&str] = &[
    "prompts/tutor-agent.md",
    "prompts/tutor_core.md",
    "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json",
    "runtime/kimi/verification-rust.json",
];
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
fn validate_plan(plan: &Value) -> Result<()> {
    ensure!(
        plan["maximum_inference_requests"] == 4
            && plan["automatic_retries"] == 0
            && plan["thinking_effort"] == "low",
        "fixed budget/effort changed"
    );
    ensure!(
        plan["arms"]
            == json!({"regular":{"alias":"kimi-code/k3.1-pro","model_id":"k3.1-pro"},
        "highspeed":{"alias":"kimi-code/k3.1-pro-highspeed","model_id":"k3.1-pro-highspeed"}}),
        "fixed model arms changed"
    );
    ensure!(
        plan["schedule"]
            == json!([{"case":"zh","arm":"regular"},{"case":"zh","arm":"highspeed"},
        {"case":"en","arm":"highspeed"},{"case":"en","arm":"regular"}]),
        "fixed four-slot order changed"
    );
    let cases = plan["cases"]
        .as_array()
        .ok_or_else(|| anyhow!("cases missing"))?;
    ensure!(
        cases.len() == 2
            && cases[0]["id"] == "zh"
            && cases[0]["language"] == "zh"
            && cases[1]["id"] == "en"
            && cases[1]["language"] == "en"
            && cases.iter().all(|case| case["text"]
                .as_str()
                .is_some_and(|v| !v.is_empty() && v.len() < 2000)),
        "fixed bilingual cases invalid"
    );
    Ok(())
}
struct Pair {
    id: String,
    request: Value,
    marker: String,
    source_sha256: String,
    user_prompt_sha256: String,
    system_prompt_sha256: String,
}
fn prepare_pair(root: &Path, output: &Path, case: &Value) -> Result<Pair> {
    let id = s(case, "id");
    ensure!(["zh", "en"].contains(&id), "unexpected fixed case");
    let template = output.join(format!("source-{id}.sqlite3"));
    let store = Store::open(&template, "simulator")?;
    let learner = store.create_learner("SYNTHETIC_HIGHSPEED_AB", s(case, "language"), "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let generation = store.bump(sid, "idle")?;
    let event = store.record_education_event(
        lid,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":case["text"]}),
    )?;
    let mut state = store.session(sid)?["state"].clone();
    state["latest_input"] = case["text"].clone();
    store.checkpoint(sid, &state)?;
    let context = build_context(
        &store,
        &store.session(sid)?,
        &learner,
        s(case, "text"),
        s(&event, "event_id"),
        &json!({}),
    )?;
    let request = json!({"request_id":format!("highspeed-ab-{id}"),"learner_id":lid,"session_id":sid,"generation":generation,"context":context});
    let source_sha256 = digest(&serde_json::to_vec(
        &store.source_events(lid, &[s(&event, "event_id").to_owned()])?,
    )?);
    store.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(store);
    for arm in ["regular", "highspeed"] {
        fs::copy(&template, output.join(format!("{id}-{arm}.sqlite3")))?;
    }
    write_new(&output.join(format!("request-{id}.json")), &request)?;
    let marker = format!("AI_TUTOR_AB_PROFILE_{}", uuid::Uuid::new_v4().simple());
    let policy = fs::read_to_string(root.join("prompts/tutor_core.md"))?;
    let schema = serde_json::from_slice(&read_bounded_file(&root.join(INPUTS[2]), 128 * 1024)?)?;
    let user_prompt_sha256 = digest(stream_prompt(&policy, schema, &request)?.as_bytes());
    let agent = fs::read_to_string(root.join("prompts/tutor-agent.md"))?.replace("\r\n", "\n");
    ensure!(agent.starts_with("---\n"), "agent frontmatter missing");
    let (_, body) = agent[4..]
        .split_once("\n---\n")
        .ok_or_else(|| anyhow!("agent body missing"))?;
    let system_prompt_sha256 =
        digest(format!("{}\n\nProfile verification marker: {marker}", body.trim()).as_bytes());
    Ok(Pair {
        id: id.into(),
        request,
        marker,
        source_sha256,
        user_prompt_sha256,
        system_prompt_sha256,
    })
}
fn prepare_provider(root: &Path, output: &Path, arm: &str, alias: &str) -> Result<KimiProvider> {
    let fixture = output.join(arm);
    fs::create_dir(&fixture)?;
    for relative in INPUTS {
        let target = fixture.join(relative);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(root.join(relative), target)?;
    }
    let mut provider = KimiProvider::new(&fixture)?;
    provider.diagnostic_model_alias = Some(alias.into());
    Ok(provider)
}
fn model_snapshot(provider: &KimiProvider, expected: &Value) -> Result<Value> {
    let config = provider.safe_config()?;
    let metadata = model_metadata(&config)?;
    ensure!(
        metadata["configured_alias"] == expected["alias"]
            && metadata["configured_model_id"] == expected["model_id"]
            && metadata["always_thinking"] == true
            && metadata["min_supported_effort"] == "low",
        "fixed model identity/capabilities unavailable"
    );
    let alias = s(expected, "alias");
    ensure!(
        config["models"][alias]["provider"].as_str() == Some("managed:kimi-code"),
        "managed provider differs"
    );
    Ok(metadata)
}
async fn run(root: &Path, output: &Path, report: &mut Value) -> Result<()> {
    let plan: Value = serde_json::from_str(PLAN)?;
    validate_plan(&plan)?;
    let mut regular = prepare_provider(
        root,
        output,
        "regular",
        s(&plan["arms"]["regular"], "alias"),
    )?;
    let mut highspeed = prepare_provider(
        root,
        output,
        "highspeed",
        s(&plan["arms"]["highspeed"], "alias"),
    )?;
    ensure!(
        regular.normal_home == highspeed.normal_home,
        "OAuth home differs"
    );
    let normal_config = regular.normal_home.join("config.toml");
    let config_before = fs::read(&normal_config)?;
    let configured = json!({"regular":model_snapshot(&regular,&plan["arms"]["regular"])? ,"highspeed":model_snapshot(&highspeed,&plan["arms"]["highspeed"])?});
    let pairs: Vec<Pair> = plan["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| prepare_pair(root, output, case))
        .collect::<Result<_>>()?;
    let inputs: Value = INPUTS
        .iter()
        .map(|relative| Ok((relative.to_string(), json!(sha_file(&root.join(relative))?))))
        .collect::<Result<serde_json::Map<String, Value>>>()?
        .into();
    let preregistered = json!({"plan":plan,"plan_sha256":digest(PLAN.as_bytes()),"configured_models":configured,
        "request_and_source_hash_encoding":"serde_json::to_vec compact UTF-8; pretty request files are audit copies",
        "prompt_hash_scope":"Application-materialized string bytes; excludes ACP envelopes and opaque client/server scaffolding",
        "input_sha256":inputs,"actual_executable_sha256":sha_file(&std::env::current_exe()?)?,
        "pairs":pairs.iter().map(|pair|json!({"case":pair.id,"request_sha256":digest(&serde_json::to_vec(&pair.request).unwrap()),"source_sha256":pair.source_sha256,"profile_marker_sha256":digest(pair.marker.as_bytes()),"materialized_user_sha256":pair.user_prompt_sha256,"materialized_system_sha256":pair.system_prompt_sha256})).collect::<Vec<_>>(),
        "raw_thoughts_saved_in_report":false,"normal_default_model_mutated":false,"semantic_review":"REQUIRED_AFTER_MEASUREMENT"});
    write_new(&output.join("preregistered.json"), &preregistered)?;
    report["preregistration_sha256"] = json!(sha_file(&output.join("preregistered.json"))?);
    report["configured_models"] = configured;
    let experiment = async {
        // --version plus prior canary receipt only; never --verify-kimi or a new canary call.
        for provider in [&regular,&highspeed] {
            for relative in INPUTS {
                ensure!(sha_file(&provider.root.join(relative))? == inputs[*relative].as_str().unwrap(),"paired input file drift");
            }
            let audit = provider.probe().await?;
            ensure!(audit["available"] == true && audit["verified"] == true,"existing native verification unavailable");
        }
        for (index, slot) in plan["schedule"].as_array().unwrap().iter().enumerate() {
            let case = s(slot,"case"); let arm = s(slot,"arm");
            let pair = pairs.iter().find(|pair|pair.id==case).ok_or_else(||anyhow!("unknown case"))?;
            let provider = if arm=="regular" { &mut regular } else { &mut highspeed };
            provider.diagnostic_profile_marker = Some(pair.marker.clone());
            ensure!(model_snapshot(provider,&plan["arms"][arm])? == report["configured_models"][arm],"catalog changed before request");
            let store = Store::open(output.join(format!("{case}-{arm}.sqlite3")),"simulator")?;
            let source_ids: Vec<String> = pair.request["context"]["evidence"].as_array().unwrap().iter().map(|event|s(event,"event_id").to_owned()).collect();
            ensure!(digest(&serde_json::to_vec(&store.source_events(s(&pair.request,"learner_id"),&source_ids)?)?) == pair.source_sha256,"paired sources changed");
            let generation = pair.request["generation"].as_u64().ok_or_else(||anyhow!("generation missing"))?;
            let (sender,mut receiver) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
            let start = Instant::now();
            let (result,segments) = tokio::join!(provider.generate_segments(&pair.request,generation,sender),async {
                let mut segments=vec![];
                while let Some(segment)=receiver.recv().await {
                    segments.push(json!({"sequence":segment.sequence(),"text":segment.text(),"used_evidence_ids":segment.used_evidence_ids(),"received_from_invocation_ms":start.elapsed().as_secs_f64()*1000.0}));
                }
                segments
            });
            let metrics = provider.status()["metrics"]["last_acp_segments"].clone();
            let mut row = json!({"index":index,"case":case,"arm":arm,"request_sha256":digest(&serde_json::to_vec(&pair.request)?),"source_sha256":pair.source_sha256,
                "metrics":metrics,"validated_segments":segments,"collected_not_delivered":true,"semantic_review":"PENDING","mechanical_pass":false});
            let checked = (|| -> Result<()> {
                let validated = result.as_ref().map_err(|_|anyhow!("ACP generation failed; see structured metrics"))?;
                ensure!(metrics["configured_model"] == report["configured_models"][arm]
                    && metrics["effective_thinking"] == "low" && metrics["process_reaped"] == true
                    && metrics["owned_directory_removed"] == true && metrics["stderr_drained"] == true,
                    "model/profile/cleanup receipt mismatch");
                ensure!(metrics["resolved_model_at_session_new"] == plan["arms"][arm]["model_id"] || metrics["resolved_model_at_session_new"] == plan["arms"][arm]["alias"],"resolved model mismatch");
                ensure!(metrics["resolved_model_after_thinking"] == metrics["resolved_model_at_session_new"],"thinking update changed model");
                ensure!(metrics["materialized_system_sha256"]==pair.system_prompt_sha256
                    && metrics["materialized_user_sha256"]==pair.user_prompt_sha256,"materialized prompts differ from preregistration");
                row["final_reply"] = validated.reply().clone();
                let epoch = store.learner(s(&pair.request,"learner_id"))?["epoch"].as_i64().unwrap();
                row["candidate_application"] = apply_reply(&store,s(&pair.request,"session_id"),validated.reply(),epoch)?;
                row["state_after_application"] = store.session(s(&pair.request,"session_id"))?["state"].clone();
                row["memories_after_application"] = json!(store.memories(s(&pair.request,"learner_id"))?);
                Ok(())
            })();
            row["mechanical_pass"] = json!(checked.is_ok());
            row["failure"] = checked.as_ref().err().map(|e|json!(e.to_string())).unwrap_or(Value::Null);
            // Preserve the attempted call even if its separate artifact write fails.
            report["calls"].as_array_mut().unwrap().push(row);
            write_new(&output.join(format!("call-{}.json",index+1)),&report["calls"][index])?;
            checked?;
        }
        for pair in &pairs {
            let rows: Vec<_> = report["calls"].as_array().unwrap().iter().filter(|row|row["case"]==pair.id).collect();
            ensure!(rows.len()==2,"pair incomplete");
            for key in ["materialized_system_sha256","materialized_user_sha256"] {
                ensure!(rows[0]["metrics"][key].as_str().is_some() && rows[0]["metrics"][key]==rows[1]["metrics"][key],"materialized pair prompts differ");
            }
        }
        report["paired_materialized_prompts_equal"] = json!(true);
        Ok::<(),anyhow::Error>(())
    }.await;
    report["normal_config_unchanged"] =
        json!(fs::read(&normal_config).is_ok_and(|after| after == config_before));
    report["requests_started"] = json!(
        report["calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["metrics"]["prompt_write_attempted"] == true)
            .count()
    );
    report["not_attempted_slots"] = json!(4 - report["calls"].as_array().unwrap().len());
    report["all_recorded_cli_scratch_removed"] = json!(
        report["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["metrics"]["owned_directory_removed"] == true)
    );
    ensure!(
        report["normal_config_unchanged"] == true,
        "normal config changed; never restored automatically"
    );
    experiment
}

#[tokio::test]
#[ignore = "Four paid managed-OAuth model calls, two Highspeed at approximately 2x quota; explicit diagnostic only"]
async fn real_kimi_highspeed_ab_diagnostic_only() -> Result<()> {
    let path = PathBuf::from(
        std::env::var_os("AI_TUTOR_KIMI_HIGHSPEED_PLAN")
            .ok_or_else(|| anyhow!("explicit diagnostic paths missing"))?,
    );
    let args: Value = serde_json::from_slice(&read_bounded_file(&path, 16 * 1024)?)?;
    let root = PathBuf::from(s(&args, "root"));
    let output = PathBuf::from(s(&args, "output"));
    ensure!(
        root.is_absolute() && root.is_dir() && output.is_absolute() && !output.exists(),
        "absolute root and fresh output required"
    );
    fs::create_dir(&output)?;
    write_new(
        &output.join("owner.json"),
        &json!({"pid":std::process::id(),"scope":"isolated four-call ACP diagnostic","plan_sha256":digest(PLAN.as_bytes())}),
    )?;
    let mut report = json!({"mechanical_pass":false,"semantic_review":"PENDING_NOT_GOAL_ACCEPTANCE","calls":[],"requests_started":0,"not_attempted_slots":4,"automatic_retries":0,"maximum_inference_requests":4,
        "raw_thoughts_saved_in_report":false,"raw_thoughts_archived":false,"normal_default_model_switched":false,"cli_scratch_policy":"Existing ephemeral CLI files are removed by owned cleanup; inspect per-call receipts on any failure.",
        "identity_scope":"Resolved ACP model selection, not independent attestation of remote model weights."});
    let result = run(&root, &output, &mut report).await;
    report["mechanical_pass"] = json!(result.is_ok());
    report["failure"] = result
        .as_ref()
        .err()
        .map(|e| json!(e.to_string()))
        .unwrap_or(Value::Null);
    write_new(&output.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"mechanical_pass":result.is_ok(),"calls":report["calls"].as_array().unwrap().len(),"semantic_review":"REQUIRED","report":output.join("report.json")})
    );
    result
}

#[test]
fn plan_admission_rejects_extra_calls_reordered_slots_or_a_different_speed_model() -> Result<()> {
    let plan: Value = serde_json::from_str(PLAN)?;
    validate_plan(&plan)?;
    let mut extra = plan.clone();
    extra["schedule"]
        .as_array_mut()
        .unwrap()
        .push(plan["schedule"][0].clone());
    ensure!(validate_plan(&extra).is_err());
    let mut reordered = plan.clone();
    reordered["schedule"].as_array_mut().unwrap().swap(0, 1);
    ensure!(validate_plan(&reordered).is_err());
    let mut other = plan.clone();
    other["arms"]["highspeed"]["model_id"] = json!("kimi-for-coding-highspeed");
    ensure!(validate_plan(&other).is_err());
    let mut retries = plan;
    retries["automatic_retries"] = json!(1);
    ensure!(validate_plan(&retries).is_err());
    Ok(())
}

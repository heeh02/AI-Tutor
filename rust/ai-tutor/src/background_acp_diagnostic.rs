//! One fresh background job, generated-output preemption through real Submit,
//! and same-job recovery. No local speech services or devices are requested.
use super::voice_loop_model_diagnostic::{call, generation, observation};
use super::voice_loop_soak_diagnostic::{bound_provider_diagnostics, protected_soak};
use super::*;
use crate::audio::voice_model_chain_diagnostic::{hash, write_new};
use std::{collections::BTreeMap, fs, path::Path};

const METHOD: &str = include_str!("../../../scripts/validation/background_acp_method_v1.json");
const SOURCE: &[u8] = include_bytes!("background_acp_diagnostic.rs");
const METHOD_PATH: &str = "scripts/validation/background_acp_method_v1.json";
const SOURCE_PATH: &str = "rust/ai-tutor/src/background_acp_diagnostic.rs";
const TEST_NAME: &str = "server::background_acp_diagnostic::real_background_acp_preemption";

fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn protections(root: &Path) -> Result<Value> {
    let mut value = protected_soak(root)?;
    for name in [METHOD_PATH, "prompts/memory_consolidation.md"] {
        value[name] = json!(hash(&root.join(name))?);
    }
    Ok(value)
}

fn method() -> Result<Value> {
    let value: Value = serde_json::from_str(METHOD)?;
    ensure!(
        value["method"] == "background-acp-generation-preemption-v1-20261104",
        "method changed"
    );
    ensure!(
        value["model_call_ceiling"]
            == json!({"foreground_teaching":1,"background_memory":2,"total":3}),
        "method budget changed"
    );
    Ok(value)
}

fn elapsed(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn set_stage(out: &Path, report: &mut Value, stage: &str, started: Instant) -> Result<()> {
    report["stage"] = json!(stage);
    fs::write(
        out.join("progress.json"),
        serde_json::to_vec_pretty(&json!({
        "stage":stage,"elapsed_ms":elapsed(started),"test":TEST_NAME}))?,
    )?;
    Ok(())
}

// The supplied root build archive binds all declared sources to this exact
// archived test executable. Preparation does not create or trust a fresh build.
fn verify_build(root: &Path, build_path: &Path) -> Result<Value> {
    let build_path = build_path.canonicalize()?;
    ensure!(
        build_path.starts_with(root.join("runtime/goal-baselines")),
        "build archive outside baseline"
    );
    let build = read(&build_path)?;
    let sources = build["source_files_sha256"]
        .as_object()
        .ok_or_else(|| anyhow!("build source manifest absent"))?;
    for required in [
        SOURCE_PATH,
        METHOD_PATH,
        "rust/ai-tutor/src/server.rs",
        "rust/ai-tutor/src/memory_schedule.rs",
        "rust/ai-tutor/src/memory_worker.rs",
        "rust/ai-tutor/src/kimi.rs",
        "rust/ai-tutor/src/kimi_acp_memory.rs",
        "rust/ai-tutor/src/kimi_acp_stream.rs",
        "rust/ai-tutor/src/kimi_acp_common.rs",
        "rust/ai-tutor/src/tutor_delivery.rs",
        "rust/ai-tutor/src/store/consolidation.rs",
        "rust/ai-tutor/src/background_acp_diagnostic_tests.rs",
        "rust/ai-tutor/src/voice_loop_model_diagnostic.rs",
        "rust/ai-tutor/src/voice_loop_soak_diagnostic.rs",
        "rust/ai-tutor/src/audio/voice_model_chain_diagnostic.rs",
        "prompts/memory_consolidation.md",
        "prompts/tutor_core.md",
        "prompts/tutor-agent.md",
    ] {
        ensure!(
            sources.contains_key(required),
            "build source manifest incomplete"
        );
    }
    for (name, expected) in sources {
        let path = root.join(name).canonicalize()?;
        ensure!(
            path.starts_with(root) && expected.as_str() == Some(hash(&path)?.as_str()),
            "build source changed"
        );
    }
    ensure!(
        fs::read(root.join(SOURCE_PATH))? == SOURCE,
        "compiled harness source changed"
    );
    ensure!(
        fs::read_to_string(root.join(METHOD_PATH))? == METHOD,
        "compiled method changed"
    );
    let exe = std::env::current_exe()?.canonicalize()?;
    let name = exe
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| anyhow!("test filename absent"))?;
    let entry = build["executables"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["name"] == name)
        .ok_or_else(|| anyhow!("test executable missing from build"))?;
    ensure!(
        entry["sha256"] == hash(&exe)?
            && exe == build_path.parent().unwrap().join(name).canonicalize()?,
        "test executable differs from archive"
    );
    Ok(
        json!({"path":build_path,"sha256":hash(&build_path)?,"test_executable":exe,
        "test_executable_sha256":entry["sha256"],"source_files_verified":sources.len()}),
    )
}

fn prepare_fixture(fixture: &Path, method: &Value) -> Result<Value> {
    fs::create_dir_all(fixture.join("runtime/data"))?;
    write_new(
        &fixture.join("runtime/memory-policy.json"),
        &json!({"mode":"shadow","namespace":"simulator","limits":method["limits"]}),
    )?;
    write_new(
        &fixture.join("runtime/tutor-delivery.json"),
        &json!({"protocol_version":1,"mode":"segments","segment_speech":"disabled"}),
    )?;
    let store = Store::open(fixture.join("runtime/data/simulator.sqlite3"), "simulator")?;
    let background = store.create_learner("SYNTHETIC_ACP_PENDULUM_BACKGROUND", "en", "guided")?;
    let bg = store.create_session(s(&background, "learner_id"))?;
    let fixed = &method["fixed_source"];
    let event = store.record_education_event(
        s(&background, "learner_id"),
        s(&bg, "session_id"),
        s(fixed, "source_type"),
        s(fixed, "event_kind"),
        &fixed["content"],
    )?;
    store.finish_session(s(&bg, "session_id"))?;
    let foreground = store.create_learner("SYNTHETIC_ACP_FOREGROUND", "en", "guided")?;
    let fg = store.create_session(s(&foreground, "learner_id"))?;
    ensure!(
        store.rows("SELECT job_id FROM memory_jobs", [])?.is_empty(),
        "prepare created background job"
    );
    store.checkpoint_wal()?;
    Ok(
        json!({"background_learner_id":background["learner_id"],"background_session_id":bg["session_id"],
        "source_event":event,"finished_background_session":store.session(s(&bg,"session_id"))?,
        "foreground_learner_id":foreground["learner_id"],"foreground_session_id":fg["session_id"],
        "foreground_session":fg,"foreground_question":method["foreground_question"],
        "provider_constructed":false,"model_calls":0,"devices_requested":false}),
    )
}

fn prepare(root: &Path, out: &Path, build_path: &Path) -> Result<()> {
    ensure!(!out.exists(), "output already exists");
    let build = verify_build(root, build_path)?;
    let fixed = method()?;
    fs::create_dir(out)?;
    let data = prepare_fixture(&out.join("fixture"), &fixed)?;
    write_new(&out.join("fixture.json"), &data)?;
    write_new(&out.join("method.json"), &fixed)?;
    let files = [
        "fixture.json",
        "method.json",
        "fixture/runtime/data/simulator.sqlite3",
        "fixture/runtime/memory-policy.json",
        "fixture/runtime/tutor-delivery.json",
    ];
    let mut hashes = BTreeMap::new();
    for name in files {
        hashes.insert(name, hash(&out.join(name))?);
    }
    write_new(
        &out.join("manifest.json"),
        &json!({"test":TEST_NAME,"mode":"prepared_no_provider",
        "build":build,"files_sha256":hashes,"method_source_sha256":hash(&root.join(METHOD_PATH))?,
        "method_status_is_frozen_historical_description":true,"harness_now_implemented_but_not_model_tested":true,
        "model_calls":0,"devices_requested":false,"limits":fixed["limits"],"model_call_ceiling":fixed["model_call_ceiling"]}),
    )?;
    println!(
        "{}",
        json!({"prepared":true,"manifest_sha256":hash(&out.join("manifest.json"))?})
    );
    Ok(())
}

fn job(app: &Workstation, fixture: &Value) -> Result<Option<Value>> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let rows=store.rows("SELECT job_id,lease_token IS NULL AS lease_released FROM memory_jobs WHERE learner_id=? AND session_id=?",
        rusqlite::params![s(fixture,"background_learner_id"),s(fixture,"background_session_id")])?;
    ensure!(rows.len() <= 1, "replacement job created");
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let mut value =
        store.memory_job_status(s(fixture, "background_learner_id"), s(row, "job_id"))?;
    let tokens = store.rows(
        "SELECT lease_token FROM memory_job_attempts WHERE job_id=? ORDER BY attempt_no",
        [s(row, "job_id")],
    )?;
    value["lease_released_observed"] = json!(row["lease_released"] == 1);
    value["new_lease_observed"] = json!(
        tokens.len() == 2
            && !s(&tokens[0], "lease_token").is_empty()
            && s(&tokens[0], "lease_token") != s(&tokens[1], "lease_token")
    );
    Ok(Some(value))
}

fn bound_generation(job: &Value, active: &Value, fixture: &Value) -> bool {
    job["status"] == "leased"
        && job["attempts"] == 1
        && job["attempt_receipts"][0]["status"] == "started"
        && job["attempt_receipts"][0]["started_at_ms"]
            .as_i64()
            .is_some()
        && active["transport"] == "acp_memory_v1"
        && active["job_id"] == job["job_id"]
        && active["session_id"] == fixture["background_session_id"]
        && active["attempt_no"] == 1
        && !s(active, "request_id").is_empty()
        && !s(active, "observation_id").is_empty()
        && active["phase"] == "prompt_response"
        && active["prompt_write_completed"] == true
        && active["nonempty_thought_event_observed"] == true
        && active["first_nonempty_thought_event_ms"]
            .as_f64()
            .is_some_and(|v| v >= 0.0)
        && active["prompt_pending"] == true
}

fn same_observation(expected: &Value, actual: &Value) -> bool {
    [
        "job_id",
        "session_id",
        "attempt_no",
        "request_id",
        "observation_id",
    ]
    .iter()
    .all(|key| !expected[*key].is_null() && expected[*key] == actual[*key])
}

fn preempted(job: &Value) -> bool {
    job["status"] == "pending"
        && job["attempts"] == 1
        && job["attempt_receipts"][0]["status"] == "preempted"
        && job["lease_released_observed"] == true
        && job["candidates"].as_array().is_some_and(Vec::is_empty)
}

fn no_audio(app: &Workstation) -> bool {
    let audio = app.audio.status();
    let asr = app.local_asr_status();
    let speech = app.local_speech_status();
    audio["recording"] != true
        && audio["capture_pending_result"] != true
        && !["recording", "transcribing", "speaking", "synthesizing"].contains(&s(&audio, "state"))
        && audio["tts_configured_engine"].is_null()
        && asr["active"] != true
        && speech["active"] != true
        && speech["busy"] != true
}

async fn wait_generation(
    app: &Arc<Workstation>,
    fixture: &Value,
    report: &mut Value,
    origin: Instant,
) -> Result<Value> {
    let began = Instant::now();
    loop {
        ensure!(no_audio(app), "audio activity in text-only probe");
        if let Some(value) = job(app, fixture)? {
            let active = app.provider.status()["metrics"]["active_acp_memory"].clone();
            *report = json!({"job":value,"active":active,"observed_ms":elapsed(origin)});
            if bound_generation(&report["job"], &report["active"], fixture) {
                return Ok(report.clone());
            }
            ensure!(
                value["attempts"].as_u64().unwrap_or(0) <= 1,
                "generation branch missed before retry"
            );
            ensure!(
                value["attempts"] == 0
                    || ["reserved", "started"]
                        .contains(&s(&value["attempt_receipts"][0], "status")),
                "generation branch ended before observation"
            );
            ensure!(
                !["completed", "shadow_ready", "exhausted", "invalidated"]
                    .contains(&s(&value, "status")),
                "generation branch ended before observation"
            );
        }
        ensure!(
            began.elapsed() < Duration::from_secs(45),
            "generation observation deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn observe_preemption(
    app: &Workstation,
    fixture: &Value,
    before: &Value,
    report: &mut Value,
    origin: Instant,
) -> Result<()> {
    let current = job(app, fixture)?.ok_or_else(|| anyhow!("background job disappeared"))?;
    report["last_job"] = current.clone();
    let terminal = app.provider.status()["metrics"]["last_acp_memory"].clone();
    report["last_provider_metric"] = terminal.clone();
    report["last_provider_metric_matches_observation"] =
        json!(same_observation(&before["active"], &terminal));
    ensure!(
        current["job_id"] == before["job"]["job_id"]
            && current["source_snapshot"] == before["job"]["source_snapshot"],
        "background identity or source changed"
    );
    ensure!(
        current["attempts"].as_u64().unwrap_or(0) <= 1,
        "recovery began before foreground completion"
    );
    ensure!(
        current["attempt_receipts"][0]["status"] != "response_received",
        "background finished before preemption"
    );
    if preempted(&current)
        && same_observation(&before["active"], &terminal)
        && report["observed"] != true
    {
        report["observed"] = json!(true);
        report["observed_ms"] = json!(elapsed(origin));
        report["provider_terminal"] = terminal;
    }
    Ok(())
}

async fn foreground(
    app: &Arc<Workstation>,
    fixture: &Value,
    before: &Value,
    report: &mut Value,
    preemption: &mut Value,
    origin: Instant,
) -> Result<Instant> {
    let sid = s(fixture, "foreground_session_id");
    report["submit_started_ms"] = json!(elapsed(origin));
    let submit = call(
        app,
        Method::POST,
        &format!("/api/sessions/{sid}/turns"),
        json!({
        "text":fixture["foreground_question"],"expected_generation":generation(app,sid)?,
        "auto_speak":false,"client_submission_id":"background-acp-fixed-foreground-v1"}),
    )
    .await?;
    report["submit"] = submit.clone();
    report["submit_returned_ms"] = json!(elapsed(origin));
    ensure!(
        submit["delivery_mode"] == "segments" && submit["speech_gate"] == "disabled",
        "foreground delivery configuration changed"
    );
    let rid = s(&submit, "request_id");
    let began = Instant::now();
    loop {
        observe_preemption(app, fixture, before, preemption, origin)?;
        let (snapshot, rows) = observation(app, sid, rid)?;
        report["last_snapshot"] = snapshot.clone();
        report["segments"] = json!(rows);
        ensure!(no_audio(app), "audio activity in text-only probe");
        ensure!(
            !["failed", "cancelled", "interrupted"].contains(&s(&snapshot, "phase")),
            "foreground stream failed"
        );
        if snapshot["final_validated"] == true {
            // Hold synthetic ACKs until final so Pending.reply remains available
            // for complete candidate review; this is not display-latency testing.
            let reply = app
                .pending
                .lock()
                .unwrap()
                .get(s(&snapshot, "turn_id"))
                .map(|p| p.reply.clone());
            if let Some(reply) = reply {
                report["final_reply"] = reply;
                break;
            }
            report["final_before_pending_observed"] = json!(true);
        }
        ensure!(
            began.elapsed() < Duration::from_secs(110),
            "foreground generation deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let rows = report["segments"]
        .as_array()
        .ok_or_else(|| anyhow!("foreground segments absent"))?;
    ensure!(!rows.is_empty(), "empty foreground reply");
    let count = rows.len();
    let mut final_ack_started = None;
    for sequence in 0..count {
        if sequence + 1 == count {
            final_ack_started = Some(Instant::now());
            report["final_ack_started_ms"] = json!(elapsed(origin));
        }
        call(
            app,
            Method::POST,
            &format!("/api/sessions/{sid}/streams/{rid}/display"),
            json!({"generation":submit["generation"],"sequence":sequence}),
        )
        .await?;
    }
    report["final_ack_returned_ms"] = json!(elapsed(origin));
    let (snapshot, rows) = observation(app, sid, rid)?;
    ensure!(
        snapshot["phase"] == "completed"
            && snapshot["final_validated"] == true
            && snapshot["shown_through"].as_i64() == Some(count as i64 - 1),
        "display final not complete"
    );
    for row in &rows {
        ensure!(
            row["shown"] == 1
                && row["speech_state"] == "not_started"
                && row["speech_started_receipt"].is_null()
                && row["speech_terminal_receipt"].is_null(),
            "unexpected speech receipt"
        );
    }
    ensure!(
        !app.pending
            .lock()
            .unwrap()
            .contains_key(s(&snapshot, "turn_id")),
        "foreground guard retained after display"
    );
    report["last_snapshot"] = snapshot;
    report["segments"] = json!(rows);
    report["provider_diagnostics"] =
        bound_provider_diagnostics(report, &app.provider.status()["metrics"]);
    report["context_after"] = call(
        app,
        Method::GET,
        &format!("/api/sessions/{sid}/context"),
        Value::Null,
    )
    .await?;
    report["completed"] = json!(true);
    report["completed_observed_ms"] = json!(elapsed(origin));
    ensure!(
        preemption["observed"] == true,
        "background preemption not observed"
    );
    Ok(final_ack_started.unwrap())
}

fn verify_shadow(job: &Value, fixture: &Value) -> Result<()> {
    let event = &fixture["source_event"];
    let eid = s(event, "event_id");
    let candidates = job["candidates"]
        .as_array()
        .ok_or_else(|| anyhow!("candidate list absent"))?;
    ensure!(!candidates.is_empty(), "empty shadow response");
    for candidate in candidates {
        let p = &candidate["payload"];
        ensure!(
            candidate["status"] == "shadow" && candidate["memory_id"].is_null(),
            "background candidate applied"
        );
        ensure!(
            p["source_grade"] == "simulated"
                && p["source_count"] == 1
                && p["source_event_ids"] == json!([eid])
                && p["source_event_versions"][eid] == event["event_version"],
            "shadow source mismatch"
        );
        let quotes = p["source_quotes"]
            .as_array()
            .ok_or_else(|| anyhow!("shadow quote absent"))?;
        ensure!(
            quotes.len() == 1
                && quotes[0]["event_id"] == eid
                && !s(&quotes[0], "quote").is_empty()
                && s(&event["content"], "text").contains(s(&quotes[0], "quote")),
            "shadow quote mismatch"
        );
    }
    Ok(())
}

async fn wait_recovery(
    app: &Arc<Workstation>,
    fixture: &Value,
    before: &Value,
    report: &mut Value,
    earliest_release: Instant,
    origin: Instant,
) -> Result<()> {
    let began = Instant::now();
    loop {
        let value = job(app, fixture)?.ok_or_else(|| anyhow!("recovery job absent"))?;
        report["job"] = value.clone();
        report["observed_ms"] = json!(elapsed(origin));
        report["last_provider_metric"] =
            app.provider.status()["metrics"]["last_acp_memory"].clone();
        ensure!(
            value["job_id"] == before["job"]["job_id"]
                && value["source_snapshot"] == before["job"]["source_snapshot"],
            "recovery identity or source changed"
        );
        let attempts = value["attempts"].as_u64().unwrap_or(0);
        ensure!(attempts <= 2, "background attempt budget exceeded");
        if attempts == 1 {
            report["last_no_second_attempt_ms_since_ack_start"] = json!(elapsed(earliest_release));
        }
        if attempts == 2 && report["second_attempt_first_observed_ms"].is_null() {
            let delay = elapsed(earliest_release);
            report["second_attempt_first_observed_ms"] = json!(elapsed(origin));
            report["second_attempt_first_observed_ms_since_ack_start"] = json!(delay);
            ensure!(
                delay >= 10000.0,
                "recovery observed before real idle window"
            );
        }
        if ["shadow_ready", "completed"].contains(&s(&value, "status")) {
            ensure!(
                attempts == 2
                    && value["new_lease_observed"] == true
                    && value["lease_released_observed"] == true
                    && value["attempt_receipts"][0]["status"] == "preempted"
                    && value["attempt_receipts"][1]["status"] == "response_received",
                "recovery receipts incomplete"
            );
            let terminal = app.provider.status()["metrics"]["last_acp_memory"].clone();
            ensure!(
                terminal["job_id"] == value["job_id"]
                    && terminal["attempt_no"] == 2
                    && terminal["session_id"] == fixture["background_session_id"]
                    && terminal["success"] == true
                    && terminal["observation_id"] != before["active"]["observation_id"]
                    && terminal["request_id"] != before["active"]["request_id"],
                "recovery provider binding mismatch"
            );
            report["provider_terminal"] = terminal;
            report["same_job_recovered"] = json!(true);
            verify_shadow(&value, fixture)?;
            report["nonempty_shadow_source_contract"] = json!(true);
            return Ok(());
        }
        ensure!(
            !["exhausted", "invalidated"].contains(&s(&value, "status")),
            "recovery failed without extra attempt"
        );
        ensure!(
            began.elapsed() < Duration::from_secs(110),
            "recovery deadline"
        );
        ensure!(no_audio(app), "audio activity in text-only probe");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn store_audit(app: &Workstation, fixture: &Value, foreground: &Value) -> Result<Value> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let mut memories = store.rows("SELECT * FROM memories ORDER BY rowid", [])?;
    for memory in &mut memories {
        let ids: Value = serde_json::from_str(s(memory, "source_event_ids"))?;
        memory["source_event_ids"] = ids.clone();
        memory["current_sources"] =
            json!(store.source_events(s(memory, "learner_id"), &strings(&ids))?);
    }
    let counts=store.one("SELECT COUNT(*) AS jobs,(SELECT COUNT(*) FROM memory_job_attempts) AS reservations,(SELECT COUNT(*) FROM memory_job_attempts WHERE started_at_ms IS NOT NULL) AS starts FROM memory_jobs",[])?.unwrap();
    let bg_count = memories
        .iter()
        .filter(|v| v["learner_id"] == fixture["background_learner_id"])
        .count();
    let sid = s(fixture, "foreground_session_id");
    let rid = s(&foreground["submit"], "request_id");
    let turns = store.rows(
        "SELECT * FROM turns WHERE session_id=? ORDER BY rowid",
        [sid],
    )?;
    let mut cited = Vec::<String>::new();
    for turn in &turns {
        for id in strings(&serde_json::from_str::<Value>(s(turn, "evidence"))?) {
            if !cited.contains(&id) {
                cited.push(id);
            }
        }
    }
    let cited_sources = store.source_events(s(fixture, "foreground_learner_id"), &cited)?;
    let foreign_citations = cited_sources.len() != cited.len()
        || cited_sources
            .iter()
            .any(|source| source["learner_id"] != fixture["foreground_learner_id"]);
    let source = store.source_events(
        s(fixture, "background_learner_id"),
        &[s(&fixture["source_event"], "event_id").to_owned()],
    )?;
    Ok(
        json!({"counts":counts,"whole_database_memories":memories,"background_memory_count":bg_count,
        "foreground_turns":turns,"foreground_request_id":rid,"foreground_cited_sources":cited_sources,
        "foreign_citation_detected":foreign_citations,"background_source_after":source,
        "raw_foreground_candidates_retained":!foreground["final_reply"].is_null(),
        "content_semantic_review":"pending_independent_review"}),
    )
}

fn cleanup_receipts(report: &Value, metrics: &Value) -> bool {
    let first = &report["preemption"]["provider_terminal"];
    let second = &report["recovery"]["provider_terminal"];
    let mut observed = BTreeMap::new();
    for value in [first, second, &metrics["last_acp_memory"]] {
        if !s(value, "observation_id").is_empty() {
            observed.insert(s(value, "observation_id"), value);
        }
    }
    let memory_ok = metrics["consolidation_requests"].as_u64() == Some(observed.len() as u64)
        && observed.values().all(|v| {
            v["cleanup_confirmed"] == true
                && v["process_reaped"] == true
                && v["owned_directory_removed"] == true
                && v["stderr_drained"] == true
        });
    let fg = &metrics["last_acp_segments"];
    let teaching_ok = if metrics["acp_segment_requests"].as_u64().unwrap_or(0) == 0 {
        true
    } else {
        metrics["acp_segment_requests"] == 1
            && fg["request_id"] == report["foreground"]["submit"]["request_id"]
            && fg["generation"] == report["foreground"]["submit"]["generation"]
            && fg["process_reaped"] == true
            && fg["owned_directory_removed"] == true
            && fg["stderr_drained"] == true
    };
    memory_ok && teaching_ok
}

fn error_code(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "generation observation deadline" => "generation_output_not_observed",
        "generation branch ended before observation" | "generation branch missed before retry" => {
            "generation_branch_not_covered"
        }
        "background finished before preemption" => "background_completed_before_submit_preemption",
        "background preemption not observed" => "preemption_not_observed",
        "recovery began before foreground completion"
        | "recovery observed before real idle window" => "idle_recovery_boundary_failed",
        "empty shadow response" => "empty_shadow_substantive_coverage_missing",
        "shadow source mismatch" | "shadow quote mismatch" => "shadow_provenance_mismatch",
        "recovery identity or source changed" | "background identity or source changed" => {
            "job_or_source_changed"
        }
        "recovery receipts incomplete" | "recovery provider binding mismatch" => {
            "recovery_receipt_mismatch"
        }
        "recovery failed without extra attempt" => "recovery_failed_no_extra_attempt",
        "recovery deadline" => "recovery_deadline",
        "foreground stream failed" | "foreground generation deadline" => {
            "foreground_generation_failed"
        }
        "display final not complete" | "final candidate snapshot absent" => {
            "foreground_display_commit_failed"
        }
        "audio activity in text-only probe" | "unexpected speech receipt" => {
            "text_only_scope_violated"
        }
        "model budget mismatch" | "persistent budget mismatch" => "model_budget_mismatch",
        "cooperative stop requested" => "cooperative_stop_requested",
        "overall deadline" => "overall_deadline",
        _ => "dependency_failed_see_stage_and_bound_receipts",
    }
}

async fn stop_requested(out: &Path) {
    loop {
        if out.join("stop-request.json").exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run(root: &Path, out: &Path, build_path: &Path, expected_manifest: &str) -> Result<()> {
    ensure!(
        hash(&out.join("manifest.json"))? == expected_manifest,
        "prepared manifest changed"
    );
    let manifest = read(&out.join("manifest.json"))?;
    ensure!(
        manifest["build"] == verify_build(root, build_path)?,
        "prepared build changed"
    );
    for (name, expected) in manifest["files_sha256"]
        .as_object()
        .ok_or_else(|| anyhow!("prepared files absent"))?
    {
        ensure!(
            expected.as_str() == Some(hash(&out.join(name))?.as_str()),
            "prepared fixture changed"
        );
    }
    let fixed = method()?;
    let fixture = read(&out.join("fixture.json"))?;
    ensure!(
        fixture["source_event"]["content"] == fixed["fixed_source"]["content"]
            && fixture["foreground_question"] == fixed["foreground_question"],
        "fixed source or question changed"
    );
    write_new(
        &out.join("run-start.json"),
        &json!({"manifest_sha256":expected_manifest,
        "started_at":chrono::Utc::now().to_rfc3339(),"model_call_ceiling":fixed["model_call_ceiling"],"automatic_retries":0}),
    )?;
    let before = protections(root)?;
    let origin = Instant::now();
    let mut report = json!({"test":TEST_NAME,"passed":false,"mechanical_pass":false,"semantic_quality_pass":null,
        "stage":"provider_setup","model_call_ceiling":fixed["model_call_ceiling"],"fixture":fixture,
        "method_sha256":hash(&out.join("method.json"))?,"prepared_manifest_sha256":expected_manifest,
        "microphone_requested":false,"camera_requested":false,"audio_requested":false,"ptz_requested":false,
        "local_model_services_started":false,"network_http_listener_tested":false,"simulated_display_ack":true,
        "display_ack_waits_for_final_to_capture_candidates":true,"remote_compute_stop_confirmed":false,
        "protected_before_sha256":before,"independent_semantic_review":"pending",
        "foreground":{},"preemption":{"observed":false},"recovery":{}});
    let mut app_holder = None;
    let mut schedule = None;
    let operation: Result<()> = tokio::select! {biased;
        _=stop_requested(out)=>Err(anyhow!("cooperative stop requested")),
        result=tokio::time::timeout(Duration::from_secs(300),async {
            let mut app=Workstation::new(out.join("fixture"),"synthetic-background-acp-preflight".into())?;
            Arc::get_mut(&mut app).ok_or_else(||anyhow!("unexpected shared fixture"))?.provider=Arc::new(KimiProvider::new(root)?);
            app_holder=Some(app.clone());
            let provider=app.provider.probe().await?;
            let identity=json!({"available":provider["available"],"verified":provider["verified"],
                "version":provider["version"],"binary_sha256":provider["binary_sha256"],
                "agent_sha256":provider["agent_sha256"],"tools":provider["tools"],"subagents":provider["subagents"],
                "test_transport":provider["test_transport"],"model_alias_and_sampling_independently_frozen":false});
            report["provider_audit"]=identity.clone();
            ensure!(provider["available"]==true && provider["verified"]==true && provider["test_transport"].is_null(),"real provider not verified");
            ensure!(app.provider.status()["metrics"]["requests"]==0,"unexpected probe model request");
            write_new(&out.join("provider-audit.json"),&identity)?;
            let selected=call(&app,Method::POST,"/api/sessions",json!({"learner_id":fixture["foreground_learner_id"],"resume":true})).await?;
            ensure!(selected["session_id"]==fixture["foreground_session_id"],"wrong foreground session selected");
            report["selected_foreground_session"]=selected;
            report["device_state_before"]=json!({"audio":app.audio.status(),"asr":app.local_asr_status(),"speech":app.local_speech_status(),"camera":app.camera.status(),"ptz":app.ptz.status()});
            ensure!(no_audio(&app),"audio activity in text-only probe");
            schedule=app.start_memory_schedule()?;ensure!(schedule.is_some(),"scheduler not started");
            set_stage(out,&mut report,"waiting_for_real_idle_and_generation",origin)?;
            let mut observed=Value::Null;let outcome=wait_generation(&app,&fixture,&mut observed,origin).await;
            report["generation_observation"]=observed;
            let before=outcome?;
            write_new(&out.join("generation-observed.json"),&before)?;
            set_stage(out,&mut report,"foreground_submit_and_preemption",origin)?;
            let mut fg=json!({});let mut preemption=json!({"observed":false});
            let outcome=foreground(&app,&fixture,&before,&mut fg,&mut preemption,origin).await;
            write_new(&out.join("foreground.json"),&fg)?;
            report["foreground"]=fg;report["preemption"]=preemption;
            let earliest_release=outcome?;
            let terminal=&report["preemption"]["provider_terminal"];
            report["agent_cancel_terminal_observed"]=json!(terminal["agent_cancel_terminal_observed"]==true
                && terminal["cancel_observation_code"]=="matching_prompt_cancelled_before_close");
            set_stage(out,&mut report,"same_job_recovery_after_real_idle",origin)?;
            let mut recovery=json!({});
            let outcome=wait_recovery(&app,&fixture,&before,&mut recovery,earliest_release,origin).await;
            report["recovery"]=recovery;outcome?;
            let metrics=app.provider.status()["metrics"].clone();
            ensure!(metrics["requests"]==3 && metrics["consolidation_requests"]==2 && metrics["acp_memory_requests"]==2
                && metrics["acp_segment_requests"]==1 && metrics["retries"]==0,"model budget mismatch");
            let audit=store_audit(&app,&fixture,&report["foreground"])?;
            ensure!(audit["counts"]==json!({"jobs":1,"reservations":2,"starts":2}) && audit["background_memory_count"]==0,"persistent budget mismatch");
            ensure!(audit["foreign_citation_detected"]==false,"foreign citation detected");
            let sources=audit["background_source_after"].as_array().ok_or_else(||anyhow!("background source changed"))?;
            ensure!(sources.len()==1 && ["event_id","event_version","event_status","source_type","learner_id","session_id","content"].iter()
                .all(|key|sources[0][*key]==fixture["source_event"][*key]),"background source changed");
            report["store_audit_before_cleanup"]=audit;
            set_stage(out,&mut report,"operation_completed",origin)?;
            Ok(())
        })=>result.unwrap_or_else(|_|Err(anyhow!("overall deadline")))
    };
    report["operation_completed"] = json!(operation.is_ok());
    report["operation_error_code"] = operation
        .as_ref()
        .err()
        .map(error_code)
        .map_or(Value::Null, |v| json!(v));
    let _ = set_stage(out, &mut report, "cleanup", origin);
    if let Some(app) = &app_holder {
        report["provider_before_cleanup"] = app.provider.status()["metrics"].clone();
        app.begin_shutdown().await;
        app.close().await;
    }
    let scheduler_joined = match schedule {
        Some(mut task) => match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
            Ok(value) => value.is_ok(),
            Err(_) => {
                task.abort();
                let _ = task.await;
                false
            }
        },
        None => false,
    };
    report["scheduler_join_confirmed"] = json!(scheduler_joined);
    if let Some(app) = &app_holder {
        let status = app.provider.status();
        let metrics = &status["metrics"];
        report["provider_after_cleanup"] = metrics.clone();
        report["provider_running_after_cleanup"] = status["running"].clone();
        report["application_running_empty"] = json!(app.running.lock().unwrap().is_empty());
        report["application_pending_empty"] = json!(app.pending.lock().unwrap().is_empty());
        report["scheduler_stopped"] = json!(!app.memory_schedule_started.load(Ordering::SeqCst));
        report["memory_worker_idle"] = json!(app.memory_worker.is_idle());
        report["final_job"] = match job(app, &fixture) {
            Ok(value) => value.unwrap_or(Value::Null),
            Err(_) => json!({"observation_failed":true}),
        };
        report["store_audit_after_cleanup"] =
            match store_audit(app, &fixture, &report["foreground"]) {
                Ok(value) => value,
                Err(_) => json!({"observation_failed":true}),
            };
        report["device_state_after"] = json!({"audio":app.audio.status(),"asr":app.local_asr_status(),"speech":app.local_speech_status(),"camera":app.camera.status(),"ptz":app.ptz.status()});
        report["started_call_cleanup_confirmed"] = json!(cleanup_receipts(&report, metrics));
        report["cleanup_confirmed"] = json!(
            scheduler_joined
                && status["running"] == 0
                && report["application_running_empty"] == true
                && report["application_pending_empty"] == true
                && report["scheduler_stopped"] == true
                && report["memory_worker_idle"] == true
                && report["started_call_cleanup_confirmed"] == true
                && no_audio(app)
        );
    }
    let after = protections(root).unwrap_or(Value::Null);
    report["protected_files_unchanged"] =
        json!(!after.is_null() && after == report["protected_before_sha256"]);
    report["protected_after_sha256"] = after;
    report["total_ms"] = json!(elapsed(origin));
    report["idle_observation_scope"] = json!(
        "Attempt2 observations are relative to the start of final display ACK, an earliest possible guard-release bound. Production IdleAdmission supplies the continuous10-second gate; polling timestamps do not locate the exact claim instant."
    );
    report["mechanical_pass"] = json!(
        operation.is_ok()
            && report["cleanup_confirmed"] == true
            && report["agent_cancel_terminal_observed"] == true
            && report["protected_files_unchanged"] == true
    );
    report["passed"] = report["mechanical_pass"].clone();
    let _ = set_stage(
        out,
        &mut report,
        if operation.is_ok() {
            "finished"
        } else {
            "failed"
        },
        origin,
    );
    write_new(&out.join("report.json"), &report)?;
    ensure!(
        report["passed"] == true,
        "background ACP preflight failed; report retained, no retry"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Explicit prepare/run of one fresh background job and one real foreground request; no devices"]
async fn real_background_acp_preemption() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_BACKGROUND_ACP_PREFLIGHT").as_deref() == Ok("1"),
        "explicit ACP preflight opt-in required"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let input = PathBuf::from(std::env::var("AI_TUTOR_BACKGROUND_ACP_OUTPUT")?);
    ensure!(input.is_absolute(), "absolute output required");
    let parent = input
        .parent()
        .ok_or_else(|| anyhow!("output parent missing"))?
        .canonicalize()?;
    ensure!(
        parent.starts_with(root.join("runtime/goal-baselines")),
        "output outside isolated baselines"
    );
    let name = input
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| anyhow!("output name absent"))?;
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'-'),
        "output name invalid"
    );
    let out = parent.join(name);
    let build = PathBuf::from(std::env::var("AI_TUTOR_BACKGROUND_ACP_BUILD_MANIFEST")?);
    match std::env::var("AI_TUTOR_BACKGROUND_ACP_MODE")?.as_str() {
        "prepare" => prepare(&root, &out, &build),
        "run" => {
            run(
                &root,
                &out,
                &build,
                &std::env::var("AI_TUTOR_BACKGROUND_ACP_MANIFEST_SHA256")?,
            )
            .await
        }
        _ => bail!("explicit prepare or run required"),
    }
}

#[cfg(test)]
#[path = "background_acp_diagnostic_tests.rs"]
mod tests;

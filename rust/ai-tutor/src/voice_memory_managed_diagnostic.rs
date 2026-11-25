//! Explicit two-round integration of managed Qwen final, TTS and scheduler.
//! Only the WAV source, display ACK and heartbeat are simulated. No PTZ motion.
use super::voice_loop_model_diagnostic::{
    call, generation, heartbeat, round_with_before_submit_expected_asr,
};
use super::voice_loop_soak_diagnostic::{
    CAMERA_ASSETS, bound_foreground_provider_diagnostics, bound_provider_diagnostics, projection,
    protected_soak, sample_camera_with_transition,
};
use super::*;
use crate::audio::voice_model_chain_diagnostic::{
    CORPUS, fixed_source, hash, stage_asr, write_new,
};
use std::{fs, path::Path};

#[path = "voice_memory_managed_soak.rs"]
mod soak;

const SOAK_METHOD: &str =
    include_str!("../../../scripts/validation/managed_voice_memory_soak_v1.json");
const CASES: &str = include_str!("../../../scripts/validation/managed_voice_memory_cases_v1.json");
const METHOD: &str =
    include_str!("../../../scripts/validation/managed_voice_memory_method_qwen_v1.json");
const FLASH_METHOD: &str =
    include_str!("../../../scripts/validation/managed_voice_flash_joint_v1.json");
// This is the captured AudioService result, not the private service wire label.
const FINAL_PROVIDER: &str = "Qwen3-ASR-1.7B / local GPU final";

fn gpu_pair_active(app: &Workstation) -> bool {
    let speech = app.local_speech_status();
    let final_asr = app.final_asr_status();
    ["ready", "warmup_completed", "active"]
        .iter()
        .all(|key| speech[*key] == true && final_asr[*key] == true)
        && app.audio.status()["asr_precise_ready"] == true
}

fn protected_joint(root: &Path) -> Result<Value> {
    let mut value = protected_soak(root)?;
    for name in [
        "runtime/gpu-resource-policy.json",
        "runtime/local-final-asr-policy.json",
        "runtime/teaching-provider-policy.json",
        "runtime/kimi/verification-rust.json",
        "prompts/memory_consolidation.md",
        "runtime/goal-baselines/20261103/sapi-corpus/zh_short.wav",
        "runtime/goal-baselines/20261103/sapi-corpus/en_short.wav",
    ] {
        let path = root.join(name);
        value[name] = if path.is_file() {
            json!(hash(&path)?)
        } else {
            Value::Null
        };
    }
    Ok(value)
}

fn progress(out: &Path, report: &Value, stage: &str, started: Instant) -> Result<()> {
    let completed = report["rounds"].as_array().map_or(0, |rows| {
        rows.iter().filter(|row| row["completed"] == true).count()
    });
    fs::write(
        out.join("progress.json"),
        serde_json::to_vec_pretty(&json!({
        "stage":stage,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"completed_rounds":completed}))?,
    )?;
    Ok(())
}
fn set_stage(out: &Path, report: &mut Value, stage: &str, started: Instant) -> Result<()> {
    report["stage"] = json!(stage);
    progress(out, report, stage, started)
}

// Match only our finite diagnostic messages. Never serialize an arbitrary
// anyhow chain, provider stderr, generated text or private credential.
fn diagnostic_error_code(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "managed service preparation failed" => "managed_preparation_failed",
        "managed preparation observation deadline" => "managed_preparation_deadline",
        "managed voice owner installation absent" => "voice_installation_unconfirmed",
        "managed Qwen final installation absent" => "final_asr_installation_unconfirmed",
        "GPU pair no longer active" => "gpu_pair_inactive_during_workload",
        "GPU pair or TTS instance changed" => "soak_owner_identity_changed",
        "foreground cap reached before1800s; no idle padding" => {
            "soak_foreground_cap_before_duration"
        }
        "language transition exceeded5s" => "soak_language_transition_deadline",
        "late background seed outside active window"
        | "late background first start missed active window"
        | "late background or bilingual phase not exercised" => {
            "soak_late_or_bilingual_coverage_missing"
        }
        "soak bounded finish deadline" | "soak recovery finish deadline" => "soak_finish_deadline",
        "per-round memory audit failed" => "soak_round_memory_audit_failed",
        "protected files changed during workload" => "soak_protected_files_changed",
        "real verified provider required" => "teaching_provider_not_verified",
        "Flash foreground configuration unavailable" => "flash_foreground_not_configured",
        "Flash foreground receipt mismatch" => "flash_foreground_receipt_mismatch",
        "background first start not observed" => "background_start_not_observed",
        "background completed or failed before preemption; branch not covered"
        | "background naturally completed before observed preemption; branch not covered" => {
            "background_finished_before_preemption_not_covered"
        }
        "first start missed; no replacement attempt allowed"
        | "preemption pending gap not observed before recovery; branch not covered" => {
            "preemption_transition_not_observed"
        }
        "preemption coordination was not reached" => "preemption_coordination_deadline",
        "in-flight preemption not observed within budget" => "preemption_observation_deadline",
        "background failed before required preemption" => "background_preemption_failed",
        "consolidation completed with empty candidates; substantive coverage insufficient" => {
            "recovery_empty_candidates"
        }
        "candidate type does not match fixed educational evidence" => {
            "recovery_candidate_type_mismatch"
        }
        "candidate source/version differs"
        | "candidate quotation does not match fixed source"
        | "fixed source absent from snapshot"
        | "fixed source version missing" => "recovery_candidate_provenance_mismatch",
        "substantive simulated content absent" | "shadow candidate applied or invalidated" => {
            "recovery_candidate_state_invalid"
        }
        "background recovery job or source snapshot changed" => {
            "recovery_identity_or_source_changed"
        }
        "same-job recovery receipts incomplete" => "recovery_receipt_incomplete",
        "background recovery failed; no third attempt" => "recovery_exhausted_or_invalidated",
        "background recovery observation deadline" => "recovery_observation_deadline",
        "background attempt budget exceeded"
        | "provider budget/count mismatch"
        | "persistent budget/application mismatch" => "call_or_application_budget_mismatch",
        "fixed source unexpectedly created replacement jobs" => "unexpected_replacement_job",
        "production shadow scheduler not started" => "scheduler_not_started",
        "explicit camera could not open" | "camera readiness failed" => "camera_preparation_failed",
        "camera or heartbeat observation failed" => "background_observer_failed",
        "teaching stream failed" => "teaching_stream_failed",
        "segment lacks real matching CPAL completion" => "native_playback_receipt_invalid",
        "teaching/playback deadline" => "foreground_delivery_deadline",
        "fixed sentence transcription differs; no correction or retry" => {
            "asr_fixed_transcript_mismatch"
        }
        "real managed final missing" => "asr_final_contract_failed",
        "cooperative diagnostic stop requested" => "cooperative_stop_requested",
        "joint operation deadline" => "joint_operation_deadline",
        _ => "dependency_operation_failed_see_stage_and_owner_receipts",
    }
}

#[derive(Clone)]
struct Fixture {
    learner: String,
    session: String,
    event: String,
    case: Value,
}
type FixtureSlot = Arc<Mutex<Option<Fixture>>>;

fn seed(app: &Workstation, case: &Value) -> Result<Fixture> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let learner = store.create_learner(
        &format!("SYNTHETIC_MANAGED_MEMORY_{}", s(case, "id")),
        s(case, "language"),
        "guided",
    )?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let events = case["events"]
        .as_array()
        .ok_or_else(|| anyhow!("fixed events absent"))?;
    ensure!(
        events.len() == 1 && events[0]["content"]["synthetic_fixture"] == true,
        "fixed source contract changed"
    );
    let row = &events[0];
    let event =
        store.record_education_event(lid, sid, "simulated", s(row, "kind"), &row["content"])?;
    // Finish before discovery: the production finish can advance source state.
    store.finish_session(sid)?;
    Ok(Fixture {
        learner: lid.into(),
        session: sid.into(),
        event: s(&event, "event_id").into(),
        case: case.clone(),
    })
}

// A private, read-only diagnostic projection. Never serialize raw lease tokens.
fn job(app: &Workstation, fixture: &Fixture) -> Result<Option<Value>> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let rows = store.rows("SELECT job_id,lease_token IS NULL AS lease_released FROM memory_jobs WHERE learner_id=? AND session_id=?", rusqlite::params![fixture.learner,fixture.session])?;
    ensure!(
        rows.len() <= 1,
        "fixed source unexpectedly created replacement jobs"
    );
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let rid = s(row, "job_id");
    let mut result = store.memory_job_status(&fixture.learner, rid)?;
    let tokens = store.rows(
        "SELECT lease_token FROM memory_job_attempts WHERE job_id=? ORDER BY attempt_no",
        [rid],
    )?;
    result["lease_released_observed"] = json!(row["lease_released"] == 1);
    result["second_lease_differs"] = json!(
        tokens.len() == 2
            && !s(&tokens[0], "lease_token").is_empty()
            && s(&tokens[0], "lease_token") != s(&tokens[1], "lease_token")
    );
    result["expected_source_event_id"] = json!(fixture.event);
    Ok(Some(result))
}

// Complete final-memory evidence from the fresh simulator, not the UI's
// first page. Foreground application is normal production behavior and is
// retained for semantic review, rather than forced to zero or hidden.
fn memory_audit(app: &Workstation, voice_session: Option<&str>) -> Result<Value> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let voice_learner = voice_session
        .map(|sid| {
            store
                .session(sid)
                .map(|session| s(&session, "learner_id").to_owned())
        })
        .transpose()?;
    let mut rows = store.rows("SELECT * FROM memories ORDER BY rowid", [])?;
    let voice_count = rows
        .iter()
        .filter(|row| row["learner_id"].as_str() == voice_learner.as_deref())
        .count();
    for row in &mut rows {
        for key in ["source_event_ids", "supersedes"] {
            if let Some(raw) = row[key].as_str() {
                row[key] = serde_json::from_str(raw)?;
            }
        }
        let ids = strings(&row["source_event_ids"]);
        let sources = store.source_events(s(row, "learner_id"), &ids)?;
        let missing: Vec<_> = ids
            .iter()
            .filter(|id| {
                !sources
                    .iter()
                    .any(|source| source["event_id"].as_str() == Some(id.as_str()))
            })
            .collect();
        row["provenance_links"] = json!(store.rows(
            "SELECT * FROM memory_source_links WHERE memory_id=? ORDER BY event_id",
            [s(row, "memory_id")]
        )?);
        row["missing_or_inactive_source_ids"] = json!(missing);
        row["current_sources"] = json!(sources);
    }
    Ok(
        json!({"namespace":"simulator","all_statuses_included":true,"whole_database_memory_count":rows.len(),
        "voice_learner_id":voice_learner,"voice_learner_memory_count":voice_count,"records":rows,
        "semantic_review":"pending_if_nonempty","raw_foreground_candidate_arrays_retained":false}),
    )
}
fn first_attempt(value: &Value) -> &Value {
    &value["attempt_receipts"][0]
}
fn first_started(value: &Value) -> bool {
    value["status"] == "leased"
        && value["attempts"] == 1
        && first_attempt(value)["status"] == "started"
        && first_attempt(value)["started_at_ms"].as_i64().is_some()
}
fn preemption_observed(value: &Value) -> bool {
    value["status"] == "pending"
        && value["attempts"] == 1
        && first_attempt(value)["status"] == "preempted"
        && first_attempt(value)["started_at_ms"].as_i64().is_some()
        && value["lease_released_observed"] == true
        && value["candidates"].as_array().is_some_and(Vec::is_empty)
}

async fn wait_started(app: &Arc<Workstation>, fixture: &Fixture, report: &mut Value) -> Result<()> {
    let started = Instant::now();
    *report = json!({"observed":false,"elapsed_ms":0,
        "observation_scope":"owned provider process started; remote acceptance/generation not proven"});
    loop {
        if let Some(value) = job(app, fixture)? {
            report["job"] = value.clone();
            report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            if first_started(&value) {
                report["observed"] = json!(true);
                return Ok(());
            }
            ensure!(
                value["attempts"].as_u64().unwrap_or(0) <= 1,
                "first start missed; no replacement attempt allowed"
            );
            ensure!(
                !["completed", "shadow_ready", "exhausted", "invalidated"]
                    .contains(&s(&value, "status")),
                "background completed or failed before preemption; branch not covered"
            );
        }
        ensure!(
            started.elapsed() < Duration::from_secs(45),
            "background first start not observed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// Runs concurrently with the unchanged foreground round. It observes rather
// than causes cancellation, and records the pending/no-candidate gap before retry.
async fn observe_preemption(
    app: &Arc<Workstation>,
    slot: FixtureSlot,
    report: &mut Value,
) -> Result<()> {
    let overall = Instant::now();
    let mut active_at = None;
    *report = json!({"covered":false,"observed_pending_without_candidates":false,
        "observation_scope":"owned provider process started; remote acceptance/generation not proven"});
    loop {
        let fixture = slot.lock().unwrap().clone();
        if let Some(fixture) = fixture {
            if let Some(value) = job(app, &fixture)? {
                report["last_job"] = value.clone();
                if first_attempt(&value)["started_at_ms"].as_i64().is_some() {
                    active_at.get_or_insert_with(Instant::now);
                }
                if preemption_observed(&value) {
                    report["covered"] = json!(true);
                    report["observed_pending_without_candidates"] = json!(true);
                    report["observed_after_first_start_ms"] =
                        json!(active_at.map_or(0.0, |at| at.elapsed().as_secs_f64() * 1000.0));
                    return Ok(());
                }
                if first_attempt(&value)["status"] == "response_received" {
                    report["outcome"] =
                        json!("naturally_completed_before_observed_preemption_not_covered");
                    bail!(
                        "background naturally completed before observed preemption; branch not covered"
                    );
                }
                ensure!(
                    value["attempts"].as_u64().unwrap_or(0) <= 1,
                    "preemption pending gap not observed before recovery; branch not covered"
                );
                ensure!(
                    !["exhausted", "invalidated"].contains(&s(&value, "status")),
                    "background failed before required preemption"
                );
            }
        }
        ensure!(
            overall.elapsed() < Duration::from_secs(90),
            "preemption coordination was not reached"
        );
        ensure!(
            active_at.is_none_or(|at: Instant| at.elapsed() < Duration::from_secs(5)),
            "in-flight preemption not observed within budget"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn substantive_shadow(value: &Value, fixture: &Fixture) -> Result<()> {
    let candidates = value["candidates"]
        .as_array()
        .ok_or_else(|| anyhow!("missing shadow candidates"))?;
    ensure!(
        !candidates.is_empty(),
        "consolidation completed with empty candidates; substantive coverage insufficient"
    );
    let expected = &fixture.case["expected"];
    let source = &fixture.case["events"][0]["content"]["text"];
    let snapshot = value["source_snapshot"]
        .as_array()
        .ok_or_else(|| anyhow!("missing source snapshot"))?;
    let version = snapshot
        .iter()
        .find(|row| row["event_id"] == fixture.event)
        .ok_or_else(|| anyhow!("fixed source absent from snapshot"))?["event_version"]
        .clone();
    ensure!(
        snapshot.len() == 1 && version.as_u64().is_some(),
        "fixed source version missing"
    );
    for candidate in candidates {
        let payload = &candidate["payload"];
        ensure!(
            candidate["status"] == "shadow" && candidate["memory_id"].is_null(),
            "shadow candidate applied or invalidated"
        );
        ensure!(
            ["kind", "claim_type", "scope", "support_level"]
                .iter()
                .all(|key| payload[*key] == expected[*key]),
            "candidate type does not match fixed educational evidence"
        );
        ensure!(
            !s(payload, "content").trim().is_empty()
                && payload["source_grade"] == "simulated"
                && payload["source_count"] == 1,
            "substantive simulated content absent"
        );
        ensure!(
            payload["source_event_ids"] == json!([fixture.event])
                && payload["source_event_versions"][&fixture.event] == version,
            "candidate source/version differs"
        );
        let quotes = payload["source_quotes"]
            .as_array()
            .ok_or_else(|| anyhow!("source quotes absent"))?;
        ensure!(
            quotes.len() == 1
                && quotes[0]["event_id"] == fixture.event
                && !s(&quotes[0], "quote").is_empty()
                && source
                    .as_str()
                    .unwrap_or("")
                    .contains(s(&quotes[0], "quote")),
            "candidate quotation does not match fixed source"
        );
    }
    Ok(())
}
async fn wait_recovered(
    app: &Arc<Workstation>,
    fixture: &Fixture,
    baseline: &Value,
    report: &mut Value,
) -> Result<()> {
    let started = Instant::now();
    *report = json!({"same_job_recovered":false,"substantive_type_source_coverage":false,"semantic_review":"pending_independent_review"});
    loop {
        let value = job(app, fixture)?.ok_or_else(|| anyhow!("recovery job disappeared"))?;
        report["job"] = value.clone();
        ensure!(
            [
                "job_id",
                "generation",
                "epoch",
                "state_revision",
                "source_snapshot"
            ]
            .iter()
            .all(|key| value[*key] == baseline[*key]),
            "background recovery job or source snapshot changed"
        );
        report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        ensure!(
            value["attempts"].as_u64().unwrap_or(0) <= 2,
            "background attempt budget exceeded"
        );
        if ["shadow_ready", "completed"].contains(&s(&value, "status")) {
            ensure!(
                value["attempts"] == 2
                    && value["second_lease_differs"] == true
                    && first_attempt(&value)["status"] == "preempted"
                    && value["attempt_receipts"][1]["status"] == "response_received"
                    && value["attempt_receipts"][1]["started_at_ms"]
                        .as_i64()
                        .is_some()
                    && value["lease_released_observed"] == true,
                "same-job recovery receipts incomplete"
            );
            report["same_job_recovered"] = json!(true);
            substantive_shadow(&value, fixture)?;
            report["substantive_type_source_coverage"] = json!(true);
            return Ok(());
        }
        ensure!(
            !["exhausted", "invalidated"].contains(&s(&value, "status")),
            "background recovery failed; no third attempt"
        );
        ensure!(
            started.elapsed() < Duration::from_secs(110),
            "background recovery observation deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Explicit real camera, Qwen final/native Qwen output and 2+4 maximum teaching-provider calls; synthetic WAV, no microphone/browser/PTZ"]
async fn real_managed_voice_camera_memory() -> Result<()> {
    run_managed_voice_camera_memory(false, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Explicit 1800s current managed Qwen/camera/native output/shadow memory; fixed WAVs, no microphone/browser/PTZ"]
async fn real_managed_voice_camera_memory_soak() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_REAL_MANAGED_VOICE_MEMORY_SOAK").as_deref() == Ok("1"),
        "explicit managed soak opt-in required"
    );
    run_managed_voice_camera_memory(true, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Explicit 2 Flash foreground + 4 maximum Kimi memory calls with real Qwen/camera/native output; fixed bilingual WAVs, no microphone/browser/PTZ"]
async fn real_managed_voice_camera_memory_flash_joint() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_REAL_FLASH_VOICE_JOINT").as_deref() == Ok("1"),
        "explicit Flash joint opt-in required"
    );
    run_managed_voice_camera_memory(false, true).await
}

async fn run_managed_voice_camera_memory(soak_mode: bool, flash_mode: bool) -> Result<()> {
    ensure!(!(soak_mode && flash_mode), "Flash joint cannot select soak");
    ensure!(
        std::env::var("AI_TUTOR_REAL_MANAGED_VOICE_MEMORY").as_deref() == Ok("1"),
        "explicit managed integration opt-in required"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let out = PathBuf::from(std::env::var("AI_TUTOR_MANAGED_JOINT_OUTPUT")?).canonicalize()?;
    ensure!(
        out.starts_with(root.join("runtime/goal-baselines")),
        "isolated baseline output required"
    );
    let setup: Value = serde_json::from_slice(&fs::read(out.join("setup.json"))?)?;
    let method: Value = serde_json::from_str(if flash_mode {
        FLASH_METHOD
    } else if soak_mode {
        SOAK_METHOD
    } else {
        METHOD
    })?;
    let method_file = if soak_mode {
        "managed_voice_memory_soak_v1.json"
    } else if flash_mode {
        "managed_voice_flash_joint_v1.json"
    } else {
        "managed_voice_memory_method_qwen_v1.json"
    };
    let cases: Value = serde_json::from_str(CASES)?;
    ensure!(
        setup["method"] == method["method"]
            && setup["test"] == method["test"]
            && setup["model_service_owned_by_runner"] == false,
        "runner contract mismatch"
    );
    ensure!(
        setup["cases_sha256"]
            == hash(&root.join("scripts/validation/managed_voice_memory_cases_v1.json"))?
            && serde_json::from_slice::<Value>(&fs::read(
                root.join("scripts/validation/managed_voice_memory_cases_v1.json")
            )?)? == cases,
        "compiled/source cases differ"
    );
    ensure!(
        serde_json::from_slice::<Value>(&fs::read(
            root.join("scripts/validation").join(method_file)
        )?)? == method,
        "compiled/source method differs"
    );
    ensure!(
        setup["test_executable_sha256"] == hash(&std::env::current_exe()?)?,
        "test executable differs"
    );
    let worker = PathBuf::from(s(&setup, "camera_worker"));
    ensure!(
        worker.is_absolute() && setup["camera_worker_sha256"] == hash(&worker)?,
        "camera worker differs"
    );
    let camera_id = std::env::var("AI_TUTOR_SOAK_CAMERA_ID")?;
    ensure!(
        fs::read_to_string(root.join("runtime/rust_camera_device.txt"))?.trim() == camera_id
            && !camera_id.trim().is_empty(),
        "explicit saved camera changed"
    );
    let before = protected_joint(&root)?;
    let (source_id, source_sha, reference) = fixed_source("zh")?;
    let wav = root.join(CORPUS).join(format!("{source_id}.wav"));
    ensure!(hash(&wav)? == source_sha, "fixed source changed");
    if soak_mode || flash_mode {
        let (id, expected, _) = fixed_source("en")?;
        ensure!(
            hash(&root.join(CORPUS).join(format!("{id}.wav")))? == expected,
            "fixed English WAV changed"
        );
    }
    let mut report = json!({"test_type":"managed_workstation_camera_voice_shadow","passed":false,"method":method,
        "language":if soak_mode || flash_mode {"zh_then_en"} else {"zh"},"microphone_opened":false,"ptz_enabled":false,"speaker_output_measured":false,
        "browser_tested":false,"acoustic_loop_tested":false,"expected_asr_provider":FINAL_PROVIDER,
        "simulated_display_ack":true,"simulated_client_heartbeat":true,"raw_media_saved":false,
        "release_main_startup_tested":false,"thirty_minute_soak":soak_mode,"automatic_teaching_retry":false,
        "service_lifecycle_owner":"same_workstation_production_runtime","rounds":[],"jobs":{},"stage":"prepare_fixture",
        "gpu_pair_active_observation":"every400ms heartbeat and operation completion; no automatic model-service Retry",
        "preemption_observation_scope":"owned provider process started; remote acceptance/generation not proven",
        "remote_generation_inflight_preemption_proven":false,
        "independent_semantic_review":"pending","protected_before_sha256":before});
    if flash_mode {
        report["foreground_provider"] = json!("deepseek_flash");
    }
    let overall = Instant::now();
    progress(&out, &report, "prepare_fixture", overall)?;
    let mut app_holder = None;
    let mut source_holder = None;
    let mut all_sources = Vec::new();
    let mut schedule = None;
    let mut heartbeat_task = None;
    let mut camera_task = None;
    let samples = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (stop_tx, stop_rx) = watch::channel(false);
    let (failure_tx, mut failure_rx) = watch::channel(None::<&'static str>);
    let mut active_start = None;
    let mut voice_session = None;
    let a_slot: FixtureSlot = Arc::new(Mutex::new(None));
    let b_slot: FixtureSlot = Arc::new(Mutex::new(None));
    let mut operation: Result<()> = tokio::select! {biased;
        _=stop_requested(&out)=>Err(anyhow!("cooperative diagnostic stop requested")),
        _=failure_rx.changed()=>Err(anyhow!("camera or heartbeat observation failed")),
        result=tokio::time::timeout(Duration::from_secs(if soak_mode { 2540 } else { 1200 }),async {
            let (fixture,asr_assets)=stage_asr(&root,&out)?;
            for (name,expected) in CAMERA_ASSETS {
                ensure!(hash(&root.join(name))?==expected,"camera asset changed");
                let target=fixture.join(name);fs::create_dir_all(target.parent().unwrap())?;
                fs::copy(root.join(name),&target)?;ensure!(hash(&target)?==expected,"camera asset copy changed");
            }
            write_new(&fixture.join("runtime/diagnostic-camera-worker.json"),&json!({"protocol_version":1,"executable":worker,"sha256":setup["camera_worker_sha256"]}))?;
            write_new(&fixture.join("runtime/tutor-delivery.json"),&json!({"protocol_version":1,"mode":"segments","segment_speech":"after_display"}))?;
            write_new(&fixture.join("runtime/memory-policy.json"),&json!({"mode":"shadow","namespace":"simulator","limits":method["limits"]}))?;
            report["asr_assets"]=asr_assets;
            report["final_asr_assets"]=setup["final_asr_assets"].clone();
            let verification_sha=if flash_mode { Some(hash(&fixture.join("runtime/kimi/verification-rust.json"))?) } else { None };
            let mut app=Workstation::new(fixture,"synthetic-managed-voice-memory".into())?;
            if !flash_mode {
                Arc::get_mut(&mut app).ok_or_else(||anyhow!("unexpected shared fixture"))?.provider=Arc::new(KimiProvider::new(&root)?);
            }
            app_holder=Some(app.clone());
            let audit=app.provider.probe().await?;
            ensure!(audit["available"]==true && audit["verified"]==true && audit["test_transport"].is_null(),"real verified provider required");
            if flash_mode {
                ensure!(app.provider.status()["metrics"]["requests"]==0,"provider budget/count mismatch");
                let foreground=app.provider.foreground_status();
                ensure!(foreground["provider"]=="deepseek_flash" && foreground["available"]==true
                    && foreground["thinking"]=="non_thinking" && foreground["stream_contract"]=="metadata_v2"
                    && foreground["reply_text_origin"]=="server_segment_concatenation",
                    "Flash foreground configuration unavailable");
                let mut foreground_audit=json!({});let mut kimi_audit=json!({});
                for key in ["provider","available","model","thinking","tools","readiness_basis","stream_contract","reply_text_origin","remote_verified_by_status_poll"] {
                    foreground_audit[key]=foreground[key].clone();
                }
                for key in ["available","verified","version","binary_sha256","agent_sha256","tools"] {
                    kimi_audit[key]=audit[key].clone();
                }
                report["provider_preflight"]=json!({"foreground":foreground_audit,"kimi":kimi_audit,"verification_receipt_sha256":verification_sha});
            }
            app.start_local_speech_runtime();app.start_local_asr_runtime();app.start_final_asr_runtime();
            set_stage(&out,&mut report,"managed_preparation",overall)?;
            let prepared=Instant::now();
            loop {
                let speech=app.local_speech_status();let asr=app.local_asr_status();let final_asr=app.final_asr_status();
                report["speech_preparation"]=speech.clone();report["asr_preparation"]=asr.clone();
                report["final_asr_preparation"]=final_asr.clone();
                if ["ready","warmup_completed","active"].iter().all(|key|speech[*key]==true && asr[*key]==true && final_asr[*key]==true) {
                    ensure!(speech["installation"]["applied"]==true && app.audio.status()["tts_configured_engine"]=="qwen_pcm","managed voice owner installation absent");
                    ensure!(app.audio.status()["asr_precise_ready"]==true,"managed Qwen final installation absent");
                    break
                }
                ensure!(!["degraded","configuration_error","cleanup_failed","stopped"].contains(&s(&speech,"phase"))
                    && !["degraded","configuration_error","cleanup_failed","stopped"].contains(&s(&asr,"phase"))
                    && !["disabled","degraded","configuration_error","cleanup_failed","stopped"].contains(&s(&final_asr,"phase")),"managed service preparation failed");
                ensure!(prepared.elapsed()<Duration::from_secs(380),"managed preparation observation deadline");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            report["services_ready_ms"]=json!(overall.elapsed().as_secs_f64()*1000.0);
            if flash_mode {
                report["final_asr_instance_id"]=json!(app.final_asr.diagnostic_instance_id());
                ensure!(report["final_asr_instance_id"].as_str().is_some_and(|value| !value.is_empty()),
                    "managed Qwen final installation absent");
            }
            set_stage(&out,&mut report,"prepare_camera",overall)?;
            app.camera.open(Some(camera_id)).map_err(|_|anyhow!("explicit camera could not open"))?;
            let camera_ready=Instant::now();
            let epoch=loop {
                let c=app.camera.status();
                if c["state"]=="running" && c["frame_age_ms"].as_f64().is_some_and(|v|v<=2000.0)
                    && c["frame_id"].as_u64().is_some_and(|v|v>0) {break s(&c,"camera_session_id").to_owned()}
                ensure!(camera_ready.elapsed()<Duration::from_secs(30) && c["state"]!="error","camera readiness failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let (mut input,mut source)=app.audio.install_voice_loop_wav(&wav)?;source_holder=Some(source.clone());
            all_sources.push(source.clone());
            let sid={let stores=app.stores.lock().unwrap();let store=&stores["simulator"];
                let learner=store.create_learner("SYNTHETIC_MANAGED_VOICE","zh","guided")?;
                s(&store.create_session(s(&learner,"learner_id"))?,"session_id").to_owned()};
            voice_session=Some(sid.clone());
            *app.current.lock().unwrap()=Some(("simulator".into(),sid.clone()));
            let enabled=call(&app,Method::POST,&format!("/api/sessions/{sid}/voice-loop/enable"),json!({
                "client_loop_id":"real-managed-voice-memory","expected_generation":generation(&app,&sid)?,"language":"zh",
                "device_key":input.device_key,"expected_input_topology_revision":input.topology_revision})).await?;
            ensure!(enabled["phase"]=="armed" && source.status()["opens"]==0,"loop enable opened capture");
            let tag=json!({"client_loop_id":enabled["client_loop_id"],"loop_epoch":enabled["loop_epoch"]});
            let heartbeat_tag=Arc::new(tokio::sync::Mutex::new(tag.clone()));
            let transition=Arc::new(std::sync::atomic::AtomicU64::new(0));
            let start=Instant::now();active_start=Some(start);
            report["active_started_after_setup_ms"]=json!(overall.elapsed().as_secs_f64()*1000.0);
            samples.lock().unwrap().push(projection(&app.camera.status(),&app.camera.diagnostic_owner_status(),0.0,"armed:idle"));
            let h_app=app.clone();let h_sid=sid.clone();let h_tag=heartbeat_tag.clone();let mut h_stop=stop_rx.clone();let h_failure=failure_tx.clone();
            heartbeat_task=Some(tokio::spawn(async move {let result=async {loop {
                tokio::select!{biased;_=h_stop.changed()=>return Ok::<(),anyhow::Error>(()),
                    _=tokio::time::sleep(Duration::from_millis(400))=>{
                        let tag=h_tag.lock().await;
                        if *h_stop.borrow(){return Ok(())}
                        let state=heartbeat(&h_app,&h_sid,&tag).await?;
                        if *h_stop.borrow(){return Ok(())} ensure!(state["phase"]!="paused","heartbeat lease lost");
                        ensure!(gpu_pair_active(&h_app),"GPU pair no longer active");
                    }}
            }}.await;if result.is_err(){h_failure.send_replace(Some("heartbeat_or_gpu_pair_failed"));}result}));
            let c_app=app.clone();let c_path=out.join("camera-samples.jsonl");let c_samples=samples.clone();let c_stop=stop_rx.clone();let c_failure=failure_tx.clone();
            let c_transition=transition.clone();
            camera_task=Some(tokio::spawn(async move {let result=sample_camera_with_transition(c_app,epoch,start,c_path,c_samples,c_stop,Some(c_transition)).await;
                if result.is_err(){c_failure.send_replace(Some("camera_failed"));}result}));
            // Real armed idle, without changing last_input or the scheduler clock.
            set_stage(&out,&mut report,"initial_armed_idle",overall)?;
            while start.elapsed()<Duration::from_secs(10) {tokio::time::sleep(Duration::from_millis(20)).await;}
            report["initial_armed_idle_ms"]=json!(start.elapsed().as_secs_f64()*1000.0);
            schedule=app.start_memory_schedule()?;ensure!(schedule.is_some(),"production shadow scheduler not started");
            let a=seed(&app,&cases["tasks"][0])?;*a_slot.lock().unwrap()=Some(a.clone());
            set_stage(&out,&mut report,"A_wait_started",overall)?;let mut a_started=Value::Null;
            let started_result=wait_started(&app,&a,&mut a_started).await;
            report["jobs"]["A"]["first_start"]=a_started;started_result?;
            report["jobs"]["A"]["started_active_ms"]=json!(start.elapsed().as_secs_f64()*1000.0);
            if soak_mode {ensure!(start.elapsed()<Duration::from_secs(120),"early background start missed");}
            let mut first=Value::Null;let mut a_preempt=Value::Null;
            let first_started_ms=start.elapsed().as_secs_f64()*1000.0;
            set_stage(&out,&mut report,"round1_Start_preemption",overall)?;
            let joint=tokio::try_join!(round_with_before_submit_expected_asr(&app,&sid,&tag,&input,&source,1,reference,FINAL_PROVIDER,
                std::future::ready(Ok(Value::Null)),&mut first),observe_preemption(&app,a_slot.clone(),&mut a_preempt));
            first["provider_diagnostics"]=bound_provider_diagnostics(&first,&app.provider.status()["metrics"]);
            if soak_mode || flash_mode {
                soak::record_round(&app,&root,&out,&sid,&mut report,&mut first,1,"zh",first_started_ms,start)?;
            } else {
                write_new(&out.join("round-001.json"),&first)?;report["rounds"].as_array_mut().unwrap().push(first);
            }
            report["jobs"]["A"]["preemption"]=a_preempt;joint?;
            set_stage(&out,&mut report,"A_recovery",overall)?;let mut recovered=Value::Null;
            let baseline=report["jobs"]["A"]["preemption"]["last_job"].clone();
            let recovered_result=wait_recovered(&app,&a,&baseline,&mut recovered).await;
            report["jobs"]["A"]["recovery"]=recovered;recovered_result?;
            if flash_mode {
                set_stage(&out,&mut report,"switch_to_english",overall)?;
                let switched=soak::switch_to_english(&mut soak::Run {
                    app:&app,root:&root,out:&out,sid:&sid,report:&mut report,
                    input:&mut input,source:&mut source,all_sources:&mut all_sources,
                    heartbeat_tag:heartbeat_tag.clone(),transition:transition.clone(),
                    start,overall,b_slot:b_slot.clone(),
                }).await;
                source_holder=Some(source.clone());
                switched?;
            }
            if soak_mode {
                let tail=soak::run_tail(soak::Run {
                    app:&app,root:&root,out:&out,sid:&sid,report:&mut report,
                    input:&mut input,source:&mut source,all_sources:&mut all_sources,
                    heartbeat_tag:heartbeat_tag.clone(),transition:transition.clone(),
                    start,overall,b_slot:b_slot.clone(),
                }).await;
                source_holder=Some(source.clone());
                tail?;
            } else {
            let hook_app=app.clone();let hook_case=cases["tasks"][1].clone();let hook_slot=b_slot.clone();
            let hook=async move {
                let b=seed(&hook_app,&hook_case)?;*hook_slot.lock().unwrap()=Some(b.clone());
                let mut result=Value::Null;wait_started(&hook_app,&b,&mut result).await?;Ok(result)
            };
            let mut second=Value::Null;let mut b_preempt=Value::Null;
            let second_tag=heartbeat_tag.lock().await.clone();
            let second_reference=if flash_mode { fixed_source("en")?.2 } else { reference };
            let second_started_ms=start.elapsed().as_secs_f64()*1000.0;
            set_stage(&out,&mut report,"round2_Submit_preemption",overall)?;
            let joint=tokio::try_join!(round_with_before_submit_expected_asr(&app,&sid,&second_tag,&input,&source,2,second_reference,FINAL_PROVIDER,hook,&mut second),
                observe_preemption(&app,b_slot.clone(),&mut b_preempt));
            second["provider_diagnostics"]=bound_provider_diagnostics(&second,&app.provider.status()["metrics"]);
            if flash_mode {
                soak::record_round(&app,&root,&out,&sid,&mut report,&mut second,2,"en",second_started_ms,start)?;
            } else {
                write_new(&out.join("round-002.json"),&second)?;report["rounds"].as_array_mut().unwrap().push(second);
            }
            report["jobs"]["B"]["preemption"]=b_preempt;joint?;
            let b=b_slot.lock().unwrap().clone().ok_or_else(||anyhow!("B fixture was not introduced"))?;
            set_stage(&out,&mut report,"B_recovery",overall)?;let mut recovered=Value::Null;
            let baseline=report["jobs"]["B"]["preemption"]["last_job"].clone();
            let recovered_result=wait_recovered(&app,&b,&baseline,&mut recovered).await;
            report["jobs"]["B"]["recovery"]=recovered;recovered_result?;
            }
            let b=b_slot.lock().unwrap().clone().ok_or_else(||anyhow!("B fixture was not introduced"))?;
            report["gpu_pair_observed_active_at_completion"]=json!(gpu_pair_active(&app));
            ensure!(report["gpu_pair_observed_active_at_completion"]==true,"GPU pair no longer active");
            let metrics=app.provider.status()["metrics"].clone();
            let foreground=report["rounds"].as_array().unwrap().len();
            let transport_counts=if flash_mode {
                metrics["deepseek_segment_requests"]==2 && metrics["acp_segment_requests"].as_u64().unwrap_or(0)==0
            } else { metrics["acp_segment_requests"]==foreground };
            ensure!((if soak_mode {(2..=96).contains(&foreground)} else {foreground==2})
                && metrics["requests"]==foreground+4 && transport_counts
                && metrics["consolidation_requests"]==4 && metrics["retries"]==0,"provider budget/count mismatch");
            if flash_mode {
                for round in report["rounds"].as_array().unwrap() {
                    let bound=&round["provider_diagnostics"];let last=&bound["last_foreground_segments"];
                    ensure!(bound["matched"]==true && last["transport"]=="deepseek_http"
                        && last["stream_contract"]=="metadata_v2" && last["reply_text_origin"]=="server_segment_concatenation"
                        && last["success"]==true && last["operation_success"]==true,"Flash foreground receipt mismatch");
                }
            }
            let stores=app.stores.lock().unwrap();let store=&stores["simulator"];
            let counts=store.one("SELECT COUNT(*) AS jobs,(SELECT COUNT(*) FROM memory_job_attempts) AS reservations,(SELECT COUNT(*) FROM memory_job_attempts WHERE started_at_ms IS NOT NULL) AS starts,(SELECT COUNT(*) FROM memories WHERE learner_id IN (?,?)) AS background_memories FROM memory_jobs",rusqlite::params![a.learner,b.learner])?.unwrap();
            ensure!(counts["jobs"]==2 && counts["reservations"]==4 && counts["starts"]==4 && counts["background_memories"]==0,"persistent budget/application mismatch");
            report["memory_counts"]=counts;
            report["foreground_memory_count"]=store.one("SELECT COUNT(*) AS count FROM memories WHERE learner_id NOT IN (?,?)",rusqlite::params![a.learner,b.learner])?.unwrap()["count"].clone();
            report["foreground_candidate_scope"]=json!("normal production apply in isolated simulator; raw candidate arrays are not retained by round()");
            set_stage(&out,&mut report,"completed",overall)?;Ok(())
        })=>result.unwrap_or_else(|_|Err(anyhow!("joint operation deadline")))
    };
    // Freeze the observed active end before cleanup; preparation is excluded.
    let observed_active_ms = active_start.map_or(0.0, |at| at.elapsed().as_secs_f64() * 1000.0);
    if soak_mode && operation.is_ok() && observed_active_ms > 2100000.0 {
        operation = Err(anyhow!("soak bounded finish deadline"));
    }
    report["operation_completed"] = json!(operation.is_ok());
    report["operation_error_code"] = operation
        .as_ref()
        .err()
        .map_or(Value::Null, |error| json!(diagnostic_error_code(error)));
    report["observer_failure_code"] = json!(*failure_rx.borrow());
    report["active_duration_ms"] = json!(observed_active_ms);
    // Progress failure cannot skip cleanup or erase the operation's last stage.
    let _ = progress(&out, &report, "cleanup", overall);
    stop_tx.send_replace(true);
    if let Some(app) = &app_holder {
        app.begin_shutdown().await;
    }
    let heartbeat_clean = match heartbeat_task {
        Some(task) => matches!(task.await, Ok(Ok(()))),
        None => true,
    };
    let camera_sampler_clean = match camera_task {
        Some(task) => matches!(task.await, Ok(Ok(()))),
        None => true,
    };
    report["heartbeat_finished_cleanly"] = json!(heartbeat_clean);
    report["camera_sampler_finished_cleanly"] = json!(camera_sampler_clean);
    if let Some(app) = &app_holder {
        report["provider_before_cleanup"] = app.provider.status()["metrics"].clone();
        report["owned_camera_stop"] = app.camera.close_diagnostic().await;
        app.close().await;
        report["provider_cleanup_confirmed"] =
            json!(app.provider.close().await.is_ok() && app.provider.status()["running"] == 0);
        report["provider_after_cleanup"] = app.provider.status()["metrics"].clone();
        report["speech_after_cleanup"] = app.local_speech_status();
        report["asr_after_cleanup"] = app.local_asr_status();
        report["final_asr_after_cleanup"] = app.final_asr_status();
        report["audio_after_cleanup"] = json!({"tts_configured_engine":app.audio.status()["tts_configured_engine"],
                "asr_precise_ready":app.audio.status()["asr_precise_ready"]});
        for (name, slot) in [("A", &a_slot), ("B", &b_slot)] {
            if let Some(fixture) = slot.lock().unwrap().clone() {
                report["jobs"][name]["after_cleanup"] = match job(app, &fixture) {
                    Ok(value) => value.unwrap_or(Value::Null),
                    Err(_) => json!({"observation_failed":true}),
                };
            }
        }
    }
    let scheduler_clean = match schedule {
        Some(mut task) => match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
            Ok(result) => result.is_ok(),
            Err(_) => {
                task.abort();
                let _ = task.await;
                false
            }
        },
        None => false,
    };
    report["scheduler_join_confirmed"] = json!(scheduler_clean);
    let memory_audit_ok = if let Some(app) = &app_holder {
        match memory_audit(app, voice_session.as_deref()) {
            Ok(audit) => {
                report["final_memory_audit"] = audit;
                true
            }
            Err(_) => {
                report["final_memory_audit"] = json!({"observation_failed":true});
                false
            }
        }
    } else {
        false
    };
    report["source_lifecycle"] = source_holder
        .as_ref()
        .map_or(Value::Null, |source| source.status());
    report["source_lifecycles"] = json!(
        all_sources
            .iter()
            .map(|source| source.status())
            .collect::<Vec<_>>()
    );
    let capture_clean = !all_sources.is_empty()
        && all_sources.iter().all(|source| {
            let value = source.status();
            value["active"] == false
                && value["opens"] == value["closes"]
                && value["closed_streams"].as_array().is_some_and(|rows| {
                    Some(rows.len() as u64) == value["opens"].as_u64()
                        && rows.iter().all(|row| row["producer_joined"] == true)
                })
        });
    report["capture_cleanup_confirmed"] = json!(capture_clean);
    let after = protected_joint(&root).unwrap_or(Value::Null);
    report["protected_files_unchanged"] =
        json!(!after.is_null() && report["protected_before_sha256"] == after);
    report["protected_after_sha256"] = after;
    let active_end = report["active_duration_ms"].as_f64().unwrap_or(0.0);
    let observations = samples.lock().unwrap();
    let active: Vec<_> = observations
        .iter()
        .filter(|row| {
            row["observed_ms"]
                .as_f64()
                .is_some_and(|at| at <= active_end)
        })
        .collect();
    let gap = active
        .windows(2)
        .map(|pair| {
            pair[1]["observed_ms"].as_f64().unwrap() - pair[0]["observed_ms"].as_f64().unwrap()
        })
        .fold(0.0, f64::max);
    let tail = active
        .last()
        .map(|row| active_end - row["observed_ms"].as_f64().unwrap());
    let coverage =
        !active.is_empty() && gap <= 3000.0 && tail.is_some_and(|v| (0.0..=3000.0).contains(&v));
    report["camera"] = json!({"samples_count":active.len(),"maximum_sample_gap_ms":gap,"tail_sample_gap_ms":tail,
        "observation_coverage_complete":coverage,"first":active.first(),"last":active.last()});
    let final_asr = &report["final_asr_after_cleanup"];
    let final_stop = &final_asr["ownership_stop"];
    let final_cleanup = final_asr["cleanup_confirmed"] == true
        && final_asr["active"] == false
        && final_asr["busy"] == false
        && final_stop["cleanup_confirmed"] == true
        && final_stop["protocol_failed"] == false
        && final_stop["linux_cleanup_confirmed"] == true
        && final_stop["normal_owner_stop"] == true
        && final_stop["bridge_exit_code"] == 0
        && final_stop["receipt"]["state"] == "owned_stopped"
        && final_stop["receipt"]["linux_cleanup_confirmed"] == true
        && final_stop["receipt"]["process_started"] == true
        && final_stop["receipt"]["service_exit_code"] == 0
        && final_stop["receipt"]["forced_group_stop"] == false
        && report["audio_after_cleanup"]["asr_precise_ready"] == false;
    report["final_asr_cleanup_confirmed"] = json!(final_cleanup);
    let asr = &report["asr_after_cleanup"];
    let speech = &report["speech_after_cleanup"];
    let cleanup = asr["cleanup_confirmed"] == true
        && asr["active"] == false
        && [
            "cleanup_confirmed",
            "child_exited",
            "interpreter_exited",
            "launcher_job_empty",
            "interpreter_job_empty",
            "interpreter_job_owned",
        ]
        .iter()
        .all(|key| asr["ownership_stop"][*key] == true)
        && speech["linux_cleanup_confirmed"] == true
        && speech["client_deactivation_confirmed"] == true
        && speech["active"] == false
        && speech["ownership_stop"]["bridge_exit_code"] == 0
        && speech["ownership_stop"]["normal_owner_stop"] == true
        && speech["ownership_stop"]["receipt"]["service_exit_code"] == 0
        && speech["ownership_stop"]["receipt"]["forced_group_stop"] == false
        && report["audio_after_cleanup"]["tts_configured_engine"].is_null()
        && report["owned_camera_stop"]["cleanup_confirmed"] == true
        && report["provider_cleanup_confirmed"] == true
        && scheduler_clean
        && capture_clean
        && final_cleanup;
    report["cleanup_confirmed"] = json!(cleanup);
    report["total_ms"] = json!(overall.elapsed().as_secs_f64() * 1000.0);
    report["passed"] = json!(
        operation.is_ok()
            && cleanup
            && coverage
            && heartbeat_clean
            && camera_sampler_clean
            && memory_audit_ok
            && report["protected_files_unchanged"] == true
    );
    let mut failure_codes = Vec::<String>::new();
    if let Some(code) = report["operation_error_code"].as_str() {
        failure_codes.push(code.into());
    }
    for (passed, code) in [
        (cleanup, "owned_cleanup_unconfirmed"),
        (final_cleanup, "final_asr_owned_cleanup_unconfirmed"),
        (coverage, "camera_observation_coverage_incomplete"),
        (heartbeat_clean, "heartbeat_observer_failed"),
        (camera_sampler_clean, "camera_observer_failed"),
        (memory_audit_ok, "final_memory_audit_observation_failed"),
        (
            report["protected_files_unchanged"] == true,
            "protected_files_changed_or_unreadable",
        ),
    ] {
        if !passed {
            failure_codes.push(code.into());
        }
    }
    report["failure_codes"] = json!(failure_codes);
    write_new(&out.join("report.json"), &report)?;
    let _ = progress(
        &out,
        &report,
        if report["passed"] == true {
            "terminal_passed"
        } else {
            "terminal_failed"
        },
        overall,
    );
    ensure!(
        report["passed"] == true,
        "managed integration failed; preserved report, no retry"
    );
    Ok(())
}

#[cfg(test)]
#[path = "voice_memory_managed_tests.rs"]
mod tests;

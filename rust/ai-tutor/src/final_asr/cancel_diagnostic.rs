//! Explicit cancellation/resume diagnostic for the real owned local GPU service.
#![cfg(all(test, windows))]

use super::{
    Failure, FinalAsrClient, HEALTH_TIMEOUT, HealthReceipt, PROVIDER,
    owned::{FinalAsrConfig, OwnedFinalAsrService},
};
use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

const CASES: [(&str, &str, usize); 2] = [
    (
        "00607",
        "a2caaefe6ac8f6cfa500a0904fef79826391ff3019c944c14d86175485942c11",
        189600,
    ),
    (
        "00167",
        "5cde60727e04678702e12da1593fd3360513d5b94783b3851d6bf789cb14bb30",
        11200,
    ),
];
const BUSY_OBSERVATION: Duration = Duration::from_secs(2);
const IDLE_OBSERVATION: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    pcm_file: PathBuf,
    pcm_sha256: String,
    samples: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    root: PathBuf,
    output: PathBuf,
    config: FinalAsrConfig,
    cases: Vec<Case>,
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_sha(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut state = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        state.update(&buffer[..count]);
    }
    Ok(format!("{:x}", state.finalize()))
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}
fn log(file: &mut File, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}
fn public_progress(value: Value, elapsed_ms: u64) -> Value {
    let mut clean = json!({"elapsed_ms":elapsed_ms});
    for key in [
        "protocol_version",
        "instance_id",
        "state",
        "pid",
        "process_group",
        "ready",
        "model_ready",
        "warmup_completed",
    ] {
        if let Some(field) = value.get(key) {
            clean[key] = field.clone();
        }
    }
    clean
}
fn client_error(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "final_asr_canceled" => "final_asr_canceled",
        "final_asr_timeout" => "final_asr_timeout",
        "final_asr_transport" => "final_asr_transport",
        "final_asr_busy" => "final_asr_busy",
        "final_asr_not_ready" => "final_asr_not_ready",
        "final_asr_engine_failed" => "final_asr_engine_failed",
        "final_asr_stale_generation" => "final_asr_stale_generation",
        _ => "other_client_error",
    }
}
fn matching_active(client: &FinalAsrClient, request: &str) -> Result<bool> {
    let state = client
        .state
        .lock()
        .map_err(|_| anyhow!("client_state_unavailable"))?;
    Ok(state
        .active
        .as_ref()
        .is_some_and(|active| active.id == request && active.generation == 1 && !active.canceled))
}

/// Fresh health, unlike check_warm's cached readiness. The production transport
/// and identity validation still apply; no endpoint or credential is returned.
async fn health(client: &FinalAsrClient, started: Instant) -> Result<Value> {
    let began = started.elapsed().as_millis() as u64;
    let observed = async {
        let bytes = client
            .exchange(
                client.http.get(format!("{}/health", client.base_url)),
                HEALTH_TIMEOUT,
            )
            .await?;
        let health: HealthReceipt = super::parse(&bytes)?;
        client.check_identity(health.protocol_version, &health.instance_id)?;
        if health.provider != PROVIDER
            || health.inference_mode != "offline_final"
            || health.phase != "warm_ready"
            || !health.model_ready
            || !health.ready
            || !health.warmup_completed
            || health.closing
        {
            return Err(Failure::protocol("final_asr_health_contract"));
        }
        Ok(health)
    }
    .await;
    let value = client.finish_result(observed)?;
    Ok(
        json!({"sample_started_ms":began,"sample_finished_ms":started.elapsed().as_millis() as u64,
        "instance_id":value.instance_id,"phase":value.phase,"busy":value.busy,
        "ready":value.ready,"warmup_completed":value.warmup_completed,"closing":value.closing}),
    )
}

async fn collect_first(task: &mut Option<JoinHandle<Value>>) -> Result<Value> {
    let mut task = task.take().ok_or_else(|| anyhow!("first_task_missing"))?;
    match tokio::time::timeout(Duration::from_secs(6), &mut task).await {
        Ok(value) => value.map_err(|_| anyhow!("first_task_failed")),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(anyhow!("first_task_wait_expired"))
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit GPU cancellation/resume: one warmup and at most two transcribe calls; never auto-retry"]
async fn final_asr_cancel_diagnostic_only() -> Result<()> {
    let plan_file = PathBuf::from(
        std::env::var_os("AI_TUTOR_FINAL_ASR_CANCEL_DIAGNOSTIC_PLAN")
            .ok_or_else(|| anyhow!("explicit_final_asr_cancel_plan_required"))?,
    );
    ensure!(
        std::fs::metadata(&plan_file)?.len() <= 32768,
        "cancel_plan_limit"
    );
    let plan_bytes = std::fs::read(&plan_file)?;
    let plan: Plan = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan.output.is_absolute() && !plan.output.exists(),
        "cancel_output_must_be_new_absolute_path"
    );
    std::fs::create_dir_all(&plan.output)?;
    let started = Instant::now();
    let mut report = json!({"passed":false,"status":"FAILED","plan_sha256":sha(&plan_bytes),
        "scope":"Real owned service cancellation and resumed Rust final request; no devices",
        "warmup_budget":1,"transcription_budget":2,"transcribe_attempts":0,"cancel_budget":1,"retries":0,
        "busy_probe_transcribe_calls":0,"busy_observation_budget_ms":2000,"idle_observation_budget_ms":10000,
        "poll_interval_ms":10,"first_request":null,"cancel":null,"resumed_request":null,
        "health_samples":[],"progress":[],"ownership_stop":null,
        "selection":"00607 is the sole longest ASCEND row by sample_count; 00167 is the fixed short No case",
        "selection_rows_sha256":"f005011573edda05469f4c6b3ef3c894e52abf050dc87011a2e8680086e81652",
        "limits":["busy health plus a pending matching client request observes logical service work, not a particular CUDA kernel or generated token.",
            "A cancel receipt invalidates delivery; it does not prove physical GPU preemption.",
            "Idle timing is based on health response observations; it is not an exact kernel-retirement timestamp.",
            "No timed re-attempt if the busy/pending window or server cancellation is missed; report NOT_EXERCISED.",
            "Root supplies source binding. Actual executing binary is hashed here. No ASR/TTS co-residency or microphone acceptance."]});
    let mut events = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(plan.output.join("events.jsonl"))?;
    let (cancel_owner, cancellation) = watch::channel(false);
    let mut owner: Option<OwnedFinalAsrService> = None;
    let mut first_task: Option<JoinHandle<Value>> = None;
    let mut stage = "preflight".to_owned();

    let outcome: Result<bool> = async {
        ensure!(plan.root.is_absolute() && plan.root.is_dir() && plan.cases.len() == 2, "cancel_fixture_or_cases_invalid");
        plan.config.validate()?;
        let mut input = Vec::new();
        for (case, (id, hash, samples)) in plan.cases.iter().zip(CASES) {
            ensure!(case.id == id && case.pcm_sha256 == hash && case.samples == samples
                && case.pcm_file.is_absolute(), "cancel_fixed_case_metadata_mismatch");
            let pcm = std::fs::read(&case.pcm_file)?;
            ensure!(pcm.len() == samples * 2 && sha(&pcm) == hash, "cancel_fixed_pcm_mismatch");
            input.push(pcm);
        }
        let executable = std::env::current_exe()?;
        report["actual_executable"] = json!({"path":executable,"sha256":file_sha(&executable)?,"bytes":std::fs::metadata(&executable)?.len()});
        let (send, mut receive) = mpsc::channel(16);
        let mut progress_file = OpenOptions::new().write(true).create_new(true).open(plan.output.join("progress.jsonl"))?;
        let progress_reader = tokio::spawn(async move {
            let mut values = Vec::new();
            while let Some(value) = receive.recv().await {
                let value = public_progress(value, started.elapsed().as_millis() as u64);
                log(&mut progress_file, &value)?;
                values.push(value);
            }
            Ok::<_, anyhow::Error>(values)
        });
        stage = "prepare_owned_service".into();
        let prepared = OwnedFinalAsrService::prepare(&plan.root, &plan.config, cancellation, send).await;
        owner = prepared.owner;
        report["progress"] = json!(progress_reader.await.map_err(|_| anyhow!("progress_reader_failed"))??);
        let client = prepared.ready?;
        report["instance_id"] = json!(client.instance_id);
        report["preparation_ms"] = json!(started.elapsed().as_millis() as u64);
        let initial = health(&client, started).await?;
        ensure!(initial["busy"] == false && !client.unavailable(), "service_initially_busy_or_unavailable");
        report["health_samples"].as_array_mut().unwrap().push(initial.clone());
        log(&mut events, &json!({"stage":"initial_health","observation":initial}))?;

        stage = "first_transcribe".into();
        let request_id = uuid::Uuid::new_v4().to_string();
        let first_start = started.elapsed().as_millis() as u64;
        report["first_request"] = json!({"id":"00607","request_id":request_id,"recording_generation":1,
            "pcm_sha256":plan.cases[0].pcm_sha256,"samples":189600,"sample_rate":16000,
            "started_ms":first_start,"attempted":true,"result":null});
        report["transcribe_attempts"] = json!(1);
        log(&mut events, &json!({"stage":"first_request_attempted","request":report["first_request"]}))?;
        let first_client = client.clone();
        let first_id = request_id.clone();
        let first_pcm = input.remove(0);
        first_task = Some(tokio::spawn(async move {
            let result = first_client.transcribe(&first_id, 1, first_pcm).await;
            match result {
                Ok(answer) => json!({"finished_ms":started.elapsed().as_millis() as u64,"delivered":true,
                    "text":answer.text,"detected_language":answer.detected_language,"http_final_ms":answer.elapsed_ms}),
                Err(error) => json!({"finished_ms":started.elapsed().as_millis() as u64,"delivered":false,"error_code":client_error(&error)}),
            }
        }));
        stage = "observe_busy_and_pending".into();
        let deadline = Instant::now() + BUSY_OBSERVATION;
        let mut observed = false;
        while Instant::now() < deadline && !first_task.as_ref().unwrap().is_finished() {
            let mut sample = match tokio::time::timeout_at(deadline, health(&client, started)).await {
                Ok(result) => result?,
                Err(_) => break,
            };
            let pending = !first_task.as_ref().unwrap().is_finished();
            let matching = matching_active(&client, &request_id)?;
            sample["first_client_task_pending"] = json!(pending);
            sample["matching_active_uuid_and_generation"] = json!(matching);
            log(&mut events, &json!({"stage":"busy_observation","observation":sample}))?;
            report["health_samples"].as_array_mut().unwrap().push(sample.clone());
            if sample["busy"] == true && pending && matching { observed = true; break; }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let still_pending = !first_task.as_ref().unwrap().is_finished() && matching_active(&client, &request_id)?;
        report["busy_and_pending_observed"] = json!(observed);
        report["matching_request_pending_before_cancel"] = json!(still_pending);
        if !observed || !still_pending {
            report["not_exercised_reason"] = json!("busy_matching_pending_window_not_observed");
            report["first_request"]["result"] = collect_first(&mut first_task).await?;
            return Ok(false);
        }

        stage = "cancel_matching_request".into();
        let cancel_started = started.elapsed().as_millis() as u64;
        report["cancel"] = json!({"attempted":true,"request_id":request_id,"recording_generation":1,"started_ms":cancel_started});
        log(&mut events, &json!({"stage":"cancel_attempted","cancel":report["cancel"]}))?;
        let receipt = client.cancel(&request_id, 1).await?;
        let cancel_finished = started.elapsed().as_millis() as u64;
        report["cancel"]["receipt"] = receipt.clone();
        report["cancel"]["finished_ms"] = json!(cancel_finished);
        report["cancel"]["duration_ms"] = json!(cancel_finished - cancel_started);
        log(&mut events, &json!({"stage":"cancel_response","cancel":report["cancel"]}))?;
        report["first_request"]["result"] = collect_first(&mut first_task).await?;
        log(&mut events, &json!({"stage":"first_request_terminal","result":report["first_request"]["result"]}))?;
        if receipt["canceled"] != true {
            report["not_exercised_reason"] = json!("worker_retired_before_server_cancel_matched");
            return Ok(false);
        }
        ensure!(receipt["worker_retired"] == false && receipt["gpu_busy"] == true,
            "cancel_did_not_observe_unretired_worker");
        ensure!(report["first_request"]["result"]["delivered"] == false
            && report["first_request"]["result"]["error_code"] == "final_asr_canceled",
            "canceled_request_delivered_or_failed_for_another_reason");
        report["client_available_after_cancel"] = json!(!client.unavailable());
        ensure!(!client.unavailable(), "cancel_poisoned_client");

        stage = "observe_worker_idle".into();
        let idle_deadline = Instant::now() + IDLE_OBSERVATION;
        let mut idle = None;
        while Instant::now() < idle_deadline {
            let sample = match tokio::time::timeout_at(idle_deadline, health(&client, started)).await {
                Ok(result) => result?,
                Err(_) => break,
            };
            log(&mut events, &json!({"stage":"retirement_observation","observation":sample}))?;
            report["health_samples"].as_array_mut().unwrap().push(sample.clone());
            if sample["busy"] == false { idle = Some(sample); break; }
            report["last_post_cancel_busy_observation"] = sample;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let idle = idle.ok_or_else(|| anyhow!("worker_idle_not_observed_in_budget"))?;
        let idle_finished = idle["sample_finished_ms"].as_u64().unwrap();
        report["first_idle_observation"] = idle;
        report["cancel_dispatch_to_idle_observed_ms"] = json!(idle_finished - cancel_started);
        report["cancel_response_to_idle_observed_ms"] = json!(idle_finished - cancel_finished);
        ensure!(!client.unavailable(), "client_unavailable_after_worker_retirement");

        stage = "resumed_no_final".into();
        let resume_id = uuid::Uuid::new_v4().to_string();
        report["resumed_request"] = json!({"id":"00167","request_id":resume_id,"recording_generation":2,
            "pcm_sha256":plan.cases[1].pcm_sha256,"samples":11200,"sample_rate":16000,"attempted":true,"completed":false});
        report["transcribe_attempts"] = json!(2);
        log(&mut events, &json!({"stage":"resumed_request_attempted","request":report["resumed_request"]}))?;
        let answer = client.transcribe(&resume_id, 2, input.remove(0)).await?;
        report["resumed_request"]["text"] = json!(answer.text);
        report["resumed_request"]["detected_language"] = json!(answer.detected_language);
        report["resumed_request"]["http_final_ms"] = json!(answer.elapsed_ms);
        report["resumed_request"]["completed"] = json!(true);
        let is_no = answer.text.trim_matches(|c: char| c.is_whitespace() || ".!?。！？".contains(c)).eq_ignore_ascii_case("no");
        report["resumed_request"]["recognized_no_case_and_edge_punctuation_ignored"] = json!(is_no);
        report["same_client_instance_after_resume"] = json!(client.instance_id);
        report["client_available_after_resume"] = json!(!client.unavailable());
        log(&mut events, &json!({"stage":"resumed_request_terminal","request":report["resumed_request"]}))?;
        ensure!(is_no && !client.unavailable(), "resumed_no_final_or_client_invalid");
        let final_health = health(&client, started).await?;
        ensure!(final_health["busy"] == false, "service_not_idle_after_resumed_final");
        report["health_samples"].as_array_mut().unwrap().push(final_health);
        ensure!(owner.as_mut().ok_or_else(|| anyhow!("owner_missing"))?.is_running()?, "owner_changed_or_exited");
        Ok(true)
    }.await;

    // A failed observation must still collect/retire this one caller; never retry it.
    if first_task.is_some() {
        match collect_first(&mut first_task).await {
            Ok(value) => report["first_request"]["result"] = value,
            Err(_) => report["first_request"]["caller_wait_expired_and_aborted"] = json!(true),
        }
    }
    if outcome.is_err() {
        report["failure_stage"] = json!(stage);
        report["error_code"] = json!("cancel_diagnostic_step_failed");
    }
    let _ = cancel_owner.send(true);
    let normal_cleanup = if let Some(owner) = owner.as_mut() {
        match owner.stop().await {
            Ok(receipt) => {
                let normal = receipt["linux_cleanup_confirmed"] == true
                    && receipt["normal_owner_stop"] == true;
                report["ownership_stop"] = receipt;
                normal
            }
            Err(_) => {
                report["ownership_stop"] = json!({"linux_cleanup_confirmed":false,"error_code":"owned_linux_cleanup_unconfirmed"});
                false
            }
        }
    } else {
        false
    };
    report["owner_created"] = json!(owner.is_some());
    report["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    report["status"] = json!(match outcome {
        Ok(true) if normal_cleanup => "PASSED",
        Ok(false) if normal_cleanup => "NOT_EXERCISED",
        _ => "FAILED",
    });
    report["passed"] = json!(report["status"] == "PASSED");
    save(&plan.output.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"status":report["status"],"passed":report["passed"],"transcribe_attempts":report["transcribe_attempts"],
        "linux_cleanup_confirmed":report["ownership_stop"]["linux_cleanup_confirmed"]})
    );
    ensure!(
        report["passed"] == true,
        "final_asr_cancel_not_passed_see_report"
    );
    Ok(())
}

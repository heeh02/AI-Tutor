//! Explicit owned GPU diagnostic. Fake capture opens no input/output devices.
#![cfg(all(test, windows))]

use super::owned::{FinalAsrConfig, OwnedFinalAsrService};
use crate::audio::{AudioService, recording_test::TestRecordingOwner};
use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

const CASES: [(&str, &str, usize); 4] = [
    (
        "00025",
        "c52110fc2467f2ec45d8b6b88390745799f60c0fe11bb35a9c394fcd3ac2b654",
        58240,
    ),
    (
        "00007",
        "f2aca0e01669286411d4b67b823c086aceed8781eabbe769d15be45b10efb4c7",
        75040,
    ),
    (
        "00009",
        "437d74ada660e3812d05d83be40720ace521bdf35e093334127315a71b32696e",
        64480,
    ),
    (
        "00167",
        "5cde60727e04678702e12da1593fd3360513d5b94783b3851d6bf789cb14bb30",
        11200,
    ),
];

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
    audio_case_id: String,
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_sha(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
fn public_progress(value: Value, elapsed_ms: u64) -> Value {
    let mut clean = json!({"elapsed_ms":elapsed_ms});
    // In particular, do not persist base_url, credentials or arbitrary fields.
    for key in [
        "protocol_version",
        "instance_id",
        "state",
        "pid",
        "process_group",
        "model_ready",
        "ready",
        "warmup_completed",
        "busy",
        "closing",
        "inference_mode",
    ] {
        if let Some(field) = value.get(key) {
            clean[key] = field.clone();
        }
    }
    clean
}

async fn pump_task<T: Send + 'static>(
    owner: &mut TestRecordingOwner,
    task: JoinHandle<Result<T>>,
    budget: Duration,
) -> Result<T> {
    let deadline = Instant::now() + budget;
    loop {
        if catch_unwind(AssertUnwindSafe(|| owner.pump())).is_err() {
            task.abort();
            let _ = task.await;
            return Err(anyhow!("fake_capture_contract_failed"));
        }
        if task.is_finished() {
            return task
                .await
                .map_err(|_| anyhow!("fake_capture_task_failed"))?;
        }
        if Instant::now() >= deadline {
            task.abort();
            let _ = task.await;
            return Err(anyhow!("fake_capture_task_deadline"));
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit owned Qwen GPU run: one warmup, one fake-capture final, four exact-PCM finals"]
async fn final_asr_owned_diagnostic_only() -> Result<()> {
    let plan_file = PathBuf::from(
        std::env::var_os("AI_TUTOR_FINAL_ASR_DIAGNOSTIC_PLAN")
            .ok_or_else(|| anyhow!("explicit_final_asr_diagnostic_plan_required"))?,
    );
    ensure!(
        std::fs::metadata(&plan_file)?.len() <= 32768,
        "diagnostic_plan_limit"
    );
    let plan_bytes = std::fs::read(&plan_file)?;
    let plan: Plan = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan.output.is_absolute() && !plan.output.exists(),
        "diagnostic_output_must_be_new_absolute_path"
    );
    std::fs::create_dir_all(&plan.output)?;
    let started = Instant::now();
    let mut report = json!({"passed":false,"scope":"Real owned WSL Qwen plus Rust HTTP client and AudioService with fake capture",
        "plan_sha256":sha(&plan_bytes),"warmup_budget":1,"transcription_budget":5,"retries":0,
        "input_or_output_devices_opened":false,"streaming_asr_installed":false,"tts_model_started":false,
        "preparation_budget_seconds":180,"references_or_language_hints_sent":false,
        "requests":[],"audio_service":null,"progress":[],"ownership_stop":null,
        "limits":["Fake capture: no microphone, acoustic latency, browser, real speaker or VAD acceptance.",
            "Four direct calls use exact original PCM; AudioService uses PCM-to-float-to-PCM quantization separately.",
            "No ASR/TTS co-residency or actual GPU cancellation acceptance in this run.",
            "Source binding is supplied by the root-runner manifest; this report records the actual executing binary."]});
    let mut stage = "preflight".to_owned();
    let mut model_owner: Option<OwnedFinalAsrService> = None;
    let mut audio: Option<Arc<AudioService>> = None;
    let mut capture: Option<TestRecordingOwner> = None;
    let (cancel, cancellation) = watch::channel(false);
    let mut request_log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(plan.output.join("requests.jsonl"))?;

    let outcome: Result<()> = async {
        ensure!(plan.root.is_absolute() && plan.root.is_dir(), "fixture_root_required");
        // If the new backend fails, prohibit accidentally spending another model call on Whisper.
        ensure!(!plan.root.join("models/ggml-base.bin").exists(), "fixture_must_exclude_whisper_fallback_weights");
        ensure!(plan.cases.len() == CASES.len() && plan.audio_case_id == "00167", "fixed_cases_required");
        plan.config.validate()?;
        let mut inputs = Vec::new();
        for (case, (id, hash, samples)) in plan.cases.iter().zip(CASES) {
            ensure!(case.id == id && case.pcm_sha256 == hash && case.samples == samples
                && case.pcm_file.is_absolute(), "fixed_pcm_metadata_mismatch");
            let pcm = std::fs::read(&case.pcm_file)?;
            ensure!(pcm.len() == samples * 2 && sha(&pcm) == hash, "fixed_pcm_hash_mismatch");
            inputs.push(pcm);
        }
        let executable = std::env::current_exe()?;
        report["actual_executable"] = json!({"path":executable,"sha256":file_sha(&executable)?,
            "bytes":std::fs::metadata(&executable)?.len()});
        report["input_cases"] = json!(plan.cases.iter().map(|case| json!({"id":case.id,
            "pcm_sha256":case.pcm_sha256,"samples":case.samples,"sample_rate":16000})).collect::<Vec<_>>());
        let (send, mut receive) = mpsc::channel(16);
        let mut progress_file = OpenOptions::new().write(true).create_new(true).open(plan.output.join("progress.jsonl"))?;
        let progress_reader = tokio::spawn(async move {
            let mut values = Vec::new();
            while let Some(value) = receive.recv().await {
                let clean = public_progress(value, started.elapsed().as_millis() as u64);
                serde_json::to_writer(&mut progress_file, &clean)?;
                progress_file.write_all(b"\n")?;
                progress_file.flush()?;
                values.push(clean);
            }
            Ok::<_, anyhow::Error>(values)
        });
        stage = "prepare_owned_service".into();
        let prepared = OwnedFinalAsrService::prepare(&plan.root, &plan.config, cancellation, send).await;
        model_owner = prepared.owner;
        report["owner_created"] = json!(model_owner.is_some());
        report["preparation_ms"] = json!(started.elapsed().as_millis() as u64);
        report["progress"] = json!(progress_reader.await.map_err(|_| anyhow!("progress_reader_failed"))??);
        let client = prepared.ready?;
        report["instance_id"] = json!(client.instance_id);

        // Run this first: a fresh AudioService naturally reserves generation 1.
        let (service, mut fake) = AudioService::recording_test_channel(plan.root.clone());
        fake.samples = inputs[3].chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0).collect();
        let expected_quantized = fake.samples.iter().flat_map(|value| {
            ((value.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()
        }).collect::<Vec<_>>();
        let service = Arc::new(service);
        service.install_final_asr(client.clone());
        audio = Some(service.clone());
        capture = Some(fake);
        report["audio_service"] = json!({"id":"00167","original_pcm_sha256":plan.cases[3].pcm_sha256,
            "input_samples":11200,"sample_rate":16000,"float_conversion":"i16 / 32768.0",
            "live_quantization":"round(clamp(sample,-1,1) * 32767.0) as i16",
            "expected_wire_pcm_sha256_from_current_quantizer":sha(&expected_quantized),
            "wire_payload_independently_captured":false,"attempted":false,"completed":false});
        stage = "audio_service_start".into();
        let begin_audio = service.clone();
        let began = Instant::now();
        let start = pump_task(capture.as_mut().unwrap(), tokio::spawn(async move { begin_audio.start_recording(None).await }), Duration::from_secs(10)).await?;
        report["audio_service"]["start_response"] = start.clone();
        report["audio_service"]["start_call_ms"] = json!(began.elapsed().as_millis() as u64);
        ensure!(start["recording_generation"] == 1, "audio_generation_must_be_first");
        stage = "audio_service_final".into();
        report["audio_service"]["attempted"] = json!(true);
        save(&plan.output.join("audio-service-attempt.json"), &report["audio_service"])?;
        let finish_audio = service.clone();
        let began = Instant::now();
        let final_value = pump_task(capture.as_mut().unwrap(), tokio::spawn(async move { finish_audio.stop_recording("en").await }), Duration::from_secs(15)).await?;
        report["audio_service"]["stop_call_ms"] = json!(began.elapsed().as_millis() as u64);
        report["audio_service"]["response"] = final_value.clone();
        let precise_delivered = final_value["provider"] == "Qwen3-ASR-1.7B / local GPU final"
            && final_value["requires_confirmation"] == true && final_value["fallback_reason"].is_null()
            && final_value["error"].is_null() && final_value["text"].as_str().is_some_and(|text| !text.trim().is_empty())
            && final_value["sample_count"] == 11200 && final_value["sample_rate"] == 16000;
        report["audio_service"]["completed"] = json!(precise_delivered);
        report["audio_service"]["asr_ms_from_status"] = service.status()["asr_ms"].clone();
        save(&plan.output.join("audio-service-result.json"), &report["audio_service"])?;
        ensure!(precise_delivered, "audio_service_did_not_deliver_precise_final");

        for (index, (case, pcm)) in plan.cases.iter().zip(inputs).enumerate() {
            stage = format!("direct_final:{}", case.id);
            let generation = index as u64 + 2;
            let request = uuid::Uuid::new_v4().to_string();
            let item = json!({"id":case.id,"request_id":request,"recording_generation":generation,
                "pcm_sha256":case.pcm_sha256,"samples":case.samples,"sample_rate":16000,
                "attempted":true,"completed":false,"input_path":"exact_original_pcm"});
            report["requests"].as_array_mut().unwrap().push(item.clone());
            serde_json::to_writer(&mut request_log, &json!({"stage":"attempted","request":item}))?;
            request_log.write_all(b"\n")?;
            request_log.flush()?;
            let answer = client.transcribe(&request, generation, pcm).await?;
            let item = &mut report["requests"][index];
            item["completed"] = json!(true);
            item["text"] = json!(answer.text);
            item["detected_language"] = json!(answer.detected_language);
            item["http_final_ms"] = json!(answer.elapsed_ms);
            item["model_contract_verified_by_client"] = json!(true);
            serde_json::to_writer(&mut request_log, &json!({"stage":"completed","request":item}))?;
            request_log.write_all(b"\n")?;
            request_log.flush()?;
        }
        ensure!(capture.as_ref().unwrap().starts == 1 && capture.as_ref().unwrap().stops == 1,
            "unexpected_fake_capture_count");
        Ok(())
    }.await;

    if outcome.is_err() {
        report["failure_stage"] = json!(stage);
        report["error_code"] = json!("owned_final_diagnostic_step_failed");
    }
    // This revokes the captured client only after all direct generations are done.
    if let Some(service) = &audio {
        if let Some(owner) = &model_owner {
            report["client_cleared"] = json!(service.clear_final_asr(&owner.instance_id));
        }
        if let Some(fake) = capture.as_mut() {
            let close_audio = service.clone();
            report["fake_audio_closed"] = json!(
                pump_task(
                    fake,
                    tokio::spawn(async move {
                        close_audio.close().await;
                        Ok(())
                    }),
                    Duration::from_secs(5)
                )
                .await
                .is_ok()
            );
            fake.samples.fill(0.0);
            report["fake_capture_counts"] =
                json!({"starts":fake.starts,"stops":fake.stops,"discards":fake.discards});
        }
    }
    let _ = cancel.send(true);
    let cleanup_ok = if let Some(owner) = model_owner.as_mut() {
        match owner.stop().await {
            Ok(receipt) => {
                let confirmed = receipt["linux_cleanup_confirmed"] == true;
                report["ownership_stop"] = receipt;
                confirmed
            }
            Err(_) => {
                report["ownership_stop"] = json!({"cleanup_confirmed":false,"linux_cleanup_confirmed":false,
                    "error_code":"owned_linux_cleanup_unconfirmed"});
                false
            }
        }
    } else {
        true
    };
    report["owner_created"] = json!(model_owner.is_some());
    report["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    let complete = report["requests"]
        .as_array()
        .is_some_and(|rows| rows.len() == 4 && rows.iter().all(|row| row["completed"] == true));
    report["passed"] = json!(
        outcome.is_ok()
            && cleanup_ok
            && complete
            && report["audio_service"]["completed"] == true
            && report["client_cleared"] == true
            && report["fake_audio_closed"] == true
    );
    save(&plan.output.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"passed":report["passed"],"direct_completed":report["requests"].as_array().unwrap().iter().filter(|r| r["completed"] == true).count(),
        "audio_service_completed":report["audio_service"]["completed"],
        "linux_cleanup_confirmed":report["ownership_stop"]["linux_cleanup_confirmed"]})
    );
    ensure!(
        report["passed"] == true,
        "owned_final_asr_diagnostic_failed_see_report"
    );
    Ok(())
}

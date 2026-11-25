//! Opt-in, retained-evidence diagnostic. Never opens capture/playback devices.
use super::*;
use crate::local_asr::{AsrMode, LocalAsrPolicy, MODEL_DIRECTORY, OwnedAsrService};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::PathBuf};

fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let size = file.read(&mut bytes)?;
        if size == 0 {
            break;
        }
        hash.update(&bytes[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn write_new(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
fn pcm16(path: &Path) -> Result<Vec<f32>> {
    let wav = fs::read(path)?;
    ensure!(
        (12..=1_000_000).contains(&wav.len()),
        "invalid fixed WAV length"
    );
    ensure!(
        &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE",
        "invalid WAV header"
    );
    let mut offset = 12;
    let mut format = None;
    let mut data = None;
    while offset + 8 <= wav.len() {
        let length = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start + length;
        ensure!(end <= wav.len(), "truncated fixed WAV");
        if &wav[offset..offset + 4] == b"fmt " && length >= 16 {
            format = Some((
                u16::from_le_bytes(wav[start..start + 2].try_into()?),
                u16::from_le_bytes(wav[start + 2..start + 4].try_into()?),
                u32::from_le_bytes(wav[start + 4..start + 8].try_into()?),
                u16::from_le_bytes(wav[start + 14..start + 16].try_into()?),
            ));
        } else if &wav[offset..offset + 4] == b"data" {
            data = Some((start, end));
        }
        offset = end + length % 2;
    }
    ensure!(
        format == Some((1, 1, 16000, 16)),
        "expected mono PCM16 16kHz"
    );
    let (start, end) = data.ok_or_else(|| anyhow!("missing WAV data"))?;
    ensure!(
        (end - start) % 2 == 0 && end - start <= 640000,
        "invalid WAV data"
    );
    Ok(wav[start..end]
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
        .collect())
}
fn pcm_digest(samples: &[f32]) -> String {
    let mut hash = Sha256::new();
    for sample in samples {
        hash.update(sample.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}
fn info() -> Arc<Mutex<Value>> {
    Arc::new(Mutex::new(json!({})))
}

async fn utterance(
    client: Arc<AsrClient>,
    path: &Path,
    row: &Value,
    token: u64,
) -> (Value, Result<()>) {
    let status = info();
    let mut samples = Vec::new();
    let mut original = None;
    let mut revisions = Vec::new();
    let mut stage = "read_fixed_wav";
    let mut transcript = None;
    let mut worker_elapsed_ms = None;
    let mut final_tail_ms = None;
    let attempt_started = Instant::now();
    let operation: Result<()> = async {
        samples = crate::audio::resample(&pcm16(path)?, 16000, 44100);
        original = Some(pcm_digest(&samples));
        stage = "begin_managed_stream";
        let mut stream = begin(None, Some(client), status.clone(), Arc::new(AtomicU64::new(token)), token)
            .ok_or_else(|| anyhow!("managed stream did not start"))?;
        let started = Instant::now();
        let mut last_revision = 0;
        stage = "paced_capture_pump";
        for count in (882..samples.len()).step_by(882).chain(std::iter::once(samples.len())) {
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                started + Duration::from_secs_f64(count as f64 / 44100.0),
            )).await;
            stream.pump(&samples[..count], 44100, false);
            let current = status.lock().unwrap().clone();
            if let Some(revision) = current["asr_partial"]["revision"].as_u64() {
                if revision > last_revision {
                    revisions.push(json!({"revision":revision,"observed_ms":started.elapsed().as_secs_f64()*1000.0,
                        "text":current["asr_partial"]["text"]}));
                    last_revision = revision;
                }
            }
            ensure!(current["asr_fallback_reason"].is_null(), "stream unexpectedly entered fallback");
        }
        stage = "finish_wait";
        let final_started = Instant::now();
        let result = stream.finish(&samples, 44100).wait().await;
        final_tail_ms = Some(final_started.elapsed().as_secs_f64() * 1000.0);
        let final_text = result.map_err(|e| anyhow!(e))?;
        worker_elapsed_ms = Some(final_text.elapsed_ms);
        transcript = Some(final_text.text);
        stage = "validate_final";
        ensure!(original.as_ref() == Some(&pcm_digest(&samples)), "stream mutated full fallback PCM");
        ensure!(!transcript.as_ref().unwrap().trim().is_empty(), "empty final transcript");
        stage = "completed";
        Ok(())
    }.await;
    let intact = original.as_ref().map(|hash| *hash == pcm_digest(&samples));
    samples.fill(0.0);
    let last_status = status.lock().unwrap().clone();
    let report = json!({"id":row["id"],"language":row["language"],"reference":row["text"],
        "status":if operation.is_ok(){"completed"}else{"failed"},"stage":stage,
        "error":operation.as_ref().err().map(ToString::to_string),
        "attempt_elapsed_ms":attempt_started.elapsed().as_secs_f64()*1000.0,
        "transcript":transcript,"first_partial_observed_ms":revisions.first().map(|r| &r["observed_ms"]),
        "revisions":revisions,"final_tail_ms":final_tail_ms,"worker_elapsed_ms":worker_elapsed_ms,
        "original_fallback_pcm_unchanged":intact,"fallback":!last_status["asr_fallback_reason"].is_null(),
        "last_status":last_status});
    (report, operation)
}

async fn cancel_then_reuse(
    client: Arc<AsrClient>,
    path: &Path,
    row: &Value,
    evidence: &mut Value,
) -> Result<()> {
    *evidence = json!({"status":"running","stage":"read_fixed_wav","source_id":row["id"],
        "cancel_completed":false,"same_service_new_stream":{"status":"not_run"},
        "service_or_model_restarted":false,"overlapping_old_new_stream_tested":false});
    let operation: Result<()> = async {
        let samples = crate::audio::resample(&pcm16(path)?, 16000, 44100);
        let original = pcm_digest(&samples);
        let status = info();
        let generation = Arc::new(AtomicU64::new(1001));
        evidence["stage"] = json!("begin_cancellation_stream");
        let mut stream = begin(
            None,
            Some(client.clone()),
            status.clone(),
            generation.clone(),
            1001,
        )
        .ok_or_else(|| anyhow!("managed cancellation stream did not start"))?;
        evidence["stage"] = json!("paced_capture_pump");
        let started = Instant::now();
        for count in (882..=44100.min(samples.len())).step_by(882) {
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                started + Duration::from_secs_f64(count as f64 / 44100.0),
            ))
            .await;
            stream.pump(&samples[..count], 44100, false);
        }
        let before_cancel = status.lock().unwrap().clone();
        evidence["before_cancel_status"] = before_cancel.clone();
        evidence["stage"] = json!("require_healthy_before_cancel");
        ensure!(
            before_cancel["asr_streaming_state"] == "streaming"
                && before_cancel["asr_fallback_reason"].is_null()
                && !client.unavailable(),
            "cancellation requires a live healthy stream, not an already-failed worker"
        );
        // Await this old worker before opening another stream in the same service.
        // The sentinel tests generation isolation, not concurrent capture overlap.
        let result = stream
            .result
            .take()
            .ok_or_else(|| anyhow!("missing worker result"))?;
        generation.store(1002, Ordering::SeqCst);
        let sentinel =
            json!({"asr_streaming_state":"new_owner_sentinel","asr_partial":{"text":"new_owner"}});
        *status.lock().unwrap() = sentinel.clone();
        let cancel_started = Instant::now();
        evidence["stage"] = json!("cancel_worker_wait");
        drop(stream);
        let waited = tokio::time::timeout(Duration::from_secs(2), result).await;
        let cancellation_ms = cancel_started.elapsed().as_secs_f64() * 1000.0;
        evidence["cancel_wait_elapsed_ms"] = json!(cancellation_ms);
        evidence["last_old_worker_status"] = status.lock().unwrap().clone();
        let cancelled = waited??;
        evidence["stage"] = json!("validate_cancelled_worker");
        ensure!(
            cancelled.is_err(),
            "cancelled worker returned a final transcript"
        );
        ensure!(
            *status.lock().unwrap() == sentinel,
            "late old worker changed new owner state"
        );
        ensure!(
            pcm_digest(&samples) == original,
            "cancellation mutated full PCM"
        );
        ensure!(
            !client.unavailable(),
            "ordinary cancel marked healthy service unavailable"
        );
        evidence["cancel_completed"] = json!(true);
        evidence["cancel_worker_drained_ms"] = json!(cancellation_ms);
        evidence["cancelled_from_live_stream"] = json!(true);
        evidence["new_owner_sentinel_unchanged_after_old_worker_exit"] = json!(true);
        evidence["original_fallback_pcm_unchanged"] = json!(true);
        evidence["service_available_after_cancel"] = json!(true);
        evidence["stage"] = json!("same_service_new_stream");
        let (after, outcome) = utterance(client, path, row, 1002).await;
        evidence["same_service_new_stream"] = after;
        outcome?;
        evidence["stage"] = json!("completed");
        Ok(())
    }
    .await;
    evidence["status"] = json!(if operation.is_ok() {
        "completed"
    } else {
        "failed"
    });
    evidence["error"] = json!(operation.as_ref().err().map(ToString::to_string));
    operation
}

#[tokio::test]
#[ignore = "Explicit managed local ASR plus fixed adult synthetic WAVs; no capture or playback"]
async fn real_managed_service_fixed_synthetic_wav_pipeline() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_REAL_MANAGED_ASR_SYNTHETIC").as_deref() == Ok("1"),
        "explicit synthetic diagnostic opt-in required"
    );
    let run = std::env::var("AI_TUTOR_MANAGED_ASR_RUN")?;
    ensure!(
        !run.is_empty()
            && run.len() < 80
            && run
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid diagnostic run name"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let out = root.join("runtime/goal-baselines/20261104").join(run);
    fs::create_dir(&out)?;
    let corpus = root.join("runtime/goal-baselines/20261103/sapi-corpus");
    let manifest: Value = serde_json::from_slice(&fs::read(corpus.join("manifest.json"))?)?;
    ensure!(
        manifest["audible_output"] == false,
        "synthetic silent corpus only"
    );
    let rows = manifest["samples"]
        .as_array()
        .ok_or_else(|| anyhow!("missing corpus"))?;
    ensure!(rows.len() == 12, "expected original twelve fixed samples");
    let mut files = Vec::new();
    for row in rows {
        ensure!(
            row["source_type"] == "synthetic_sapi_adult_voice",
            "synthetic corpus only"
        );
        let id = row["id"].as_str().ok_or_else(|| anyhow!("missing id"))?;
        ensure!(
            id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid id"
        );
        let path = corpus.join(format!("{id}.wav"));
        let sha = digest(&path)?;
        ensure!(row["sha256"] == sha, "fixed source changed: {id}");
        files.push(json!({"kind":"source","name":id,"sha256":sha}));
    }
    let protected = [
        "runtime/speech-service.json",
        "runtime/local-asr-policy.json",
        "bin/ai-tutor.exe",
    ];
    let before: Vec<_> = protected
        .iter()
        .map(|p| {
            let path = root.join(p);
            Ok(if path.is_file() {
                Some(digest(&path)?)
            } else {
                None
            })
        })
        .collect::<Result<_>>()?;
    let fixture = out.join("fixture");
    fs::create_dir_all(fixture.join("bin/local-asr"))?;
    fs::create_dir_all(fixture.join(MODEL_DIRECTORY))?;
    for name in ["managed_asr.py", "asr_service.py", "recognizer.py"] {
        let src = root.join("scripts/speech-service").join(name);
        fs::copy(&src, fixture.join("bin/local-asr").join(name))?;
        files.push(json!({"kind":"service","name":name,"sha256":digest(&src)?}));
    }
    for (name, sha) in [
        (
            "tokens.txt",
            "a8e0e4ec53810e433789b54a5c0134a7eaa2ffca595a6334d54c00da858841d3",
        ),
        (
            "encoder-epoch-99-avg-1.int8.onnx",
            "8fa764187a261844f859d7143ebaa563af5d10adfece4c18a8f414c88cba2a9b",
        ),
        (
            "decoder-epoch-99-avg-1.onnx",
            "2e3b5ec371f8899ee6acd829fd753ba45772df57a91bdf37cde3136354e7db7d",
        ),
        (
            "joiner-epoch-99-avg-1.int8.onnx",
            "1ed689c5ed19dbaa725d9d191bb4822b5f4855a39e1ffd28cbc1f340d25b2ee0",
        ),
    ] {
        let src = root.join(MODEL_DIRECTORY).join(name);
        ensure!(digest(&src)? == sha, "fixed model changed");
        fs::hard_link(src, fixture.join(MODEL_DIRECTORY).join(name))?;
        files.push(json!({"kind":"model","name":name,"sha256":sha}));
    }
    for name in [
        "audio/streaming_asr.rs",
        "audio/streaming_asr_managed_diagnostic.rs",
        "local_asr.rs",
        "local_asr/owned.rs",
    ] {
        files.push(json!({"kind":"rust","name":name,"sha256":digest(&root.join("rust/ai-tutor/src").join(name))?}));
    }
    let method = json!({"version":2,"frozen_before_service_start":true,"files":files,
        "wire_chunk_samples":CHUNK_SAMPLES,"wire_sample_rate":16000,"queued_audio_limit_ms":QUEUE_CHUNKS*CHUNK_SAMPLES*1000/16000,
        "test_executable_sha256":digest(&std::env::current_exe()?)?,
        "method":"OwnedAsrService private pipe bootstrap; managed AsrClient; actual capture pump; 16k->44.1k->16k; real-time paced20ms; twelve original WAVs, then cancel1s, await old worker, and start a new recognition stream in the same service with first WAV; no service/model restart or overlapping capture",
        "partial_sampling_resolution_ms":20,"targets":{"first_partial_p95_ms":700,"final_tail_p95_ms":500},
        "quality_scoring":"report exact references/transcripts; no natural/child accuracy pass inferred",
        "microphone":false,"speaker":false,"ui_latency_measured":false,"cold_model_start_measured":false,
        "legacy_descriptor_used":false,"production_policy_written":false});
    write_new(&out.join("method.json"), &method)?;
    let policy = LocalAsrPolicy {
        protocol_version: 1,
        mode: AsrMode::Managed,
        python_executable: Some(root.join("runtime/speech-lab/.venv/Scripts/python.exe")),
    };
    let started = Instant::now();
    let mut owner = None;
    let (_cancel, mut receiver) = tokio::sync::watch::channel(false);
    let mut progress = Vec::new();
    let mut results: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id":row["id"],"language":row["language"],"reference":row["text"],
                "status":"not_run","stage":"not_started"
            })
        })
        .collect();
    let mut cancellation = json!({"status":"not_run","stage":"not_started",
        "cancel_completed":false,"same_service_new_stream":{"status":"not_run"},
        "service_or_model_restarted":false,"overlapping_old_new_stream_tested":false});
    let mut operation_stage = "spawn";
    let operation: Result<()> = async {
        owner = Some(OwnedAsrService::spawn(&fixture, &policy)?);
        operation_stage = "prepare";
        let client = owner.as_mut().unwrap().prepare(&mut receiver,|event| {
            progress.push(json!({"state":event["state"],"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}));
        }).await?;
        operation_stage = "fixed_samples";
        for (index,row) in rows.iter().enumerate() {
            let id = row["id"].as_str().unwrap();
            let (report, outcome) = utterance(client.clone(),&corpus.join(format!("{id}.wav")),row,index as u64+1).await;
            results[index] = report;
            write_new(&out.join(format!("sample-{id}.json")),&results[index])?;
            outcome?;
        }
        operation_stage = "cancel_and_same_service_reuse";
        let row = &rows[0];
        cancel_then_reuse(client,&corpus.join(format!("{}.wav",row["id"].as_str().unwrap())),row,&mut cancellation).await?;
        operation_stage = "completed";
        Ok(())
    }.await;
    // Every Result error after a successful spawn reaches this owned cleanup.
    // Spawn failure also receives a report, without inventing a stop receipt.
    let stop = match owner.as_mut() {
        Some(owner) => owner.stop().await,
        None => json!({"status":"not_started","cleanup_confirmed":null}),
    };
    let unchanged = protected.iter().zip(&before).all(|(name, expected)| {
        let path = root.join(name);
        if path.is_file() {
            digest(&path).ok().as_ref() == expected.as_ref()
        } else {
            expected.is_none()
        }
    });
    let passed = operation.is_ok() && stop["cleanup_confirmed"] == true && unchanged;
    write_new(
        &out.join("report.json"),
        &json!({"completed":operation.is_ok(),"integration_passed":passed,
        "stage":operation_stage,"error":operation.as_ref().err().map(ToString::to_string),"method_sha256":digest(&out.join("method.json"))?,
        "progress":progress,"results":results,"cancellation":cancellation,"stop":stop,"protected_files_unchanged":unchanged,
        "microphone":false,"speaker":false,"ui_latency_measured":false,"whisper_fallback_inference_run":false}),
    )?;
    operation?;
    ensure!(
        passed,
        "managed diagnostic cleanup or protected-file check failed"
    );
    println!(
        "Managed ASR synthetic diagnostic retained at {}",
        out.display()
    );
    Ok(())
}

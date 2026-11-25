//! Explicit real-model, synthetic-file transport chain. No capture/output device.
//! PCM received here is never a SpeechStarted/Completed or speaker measurement.
use super::*;
use crate::{
    audio::streaming_tts::{self, SynthesisOutcome, TtsClient},
    context::build_context,
    kimi::{KimiProvider, acp_stream::SEGMENT_CHANNEL_CAPACITY, tutor_segments::TutorSegment},
    local_asr::{AsrMode, LocalAsrPolicy, MODEL_DIRECTORY, OwnedAsrService},
    store::{Store, s, uid},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};

pub(crate) const CORPUS: &str = "runtime/goal-baselines/20261103/sapi-corpus";
const MODELS: [(&str, &str); 4] = [
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
];
const PROTECTED: [&str; 7] = [
    "bin/ai-tutor.exe",
    "runtime/server.json",
    "runtime/speech-service.json",
    "runtime/tts-service.json",
    "runtime/local-asr-policy.json",
    "runtime/local-speech-policy.json",
    "runtime/tutor-delivery.json",
];
pub(crate) fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let read = file.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        digest.update(&bytes[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub(crate) fn write_new(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
pub(crate) fn protected(root: &Path) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for name in PROTECTED {
        let path = root.join(name);
        result.insert(
            name.into(),
            if path.is_file() {
                json!(hash(&path)?)
            } else {
                Value::Null
            },
        );
    }
    Ok(result.into())
}
pub(crate) fn fixed_source(language: &str) -> Result<(&'static str, &'static str, &'static str)> {
    match language {
        "zh" => Ok((
            "zh_short",
            "ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33",
            "先让小车停下来。",
        )),
        "en" => Ok((
            "en_short",
            "670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f",
            "First, stop the car.",
        )),
        _ => anyhow::bail!("only frozen zh/en synthetic sources are allowed"),
    }
}
fn read_pcm(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path)?;
    ensure!(
        (44..=1_000_000).contains(&bytes.len()),
        "fixed WAV length invalid"
    );
    ensure!(
        &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "fixed WAV header invalid"
    );
    let (mut offset, mut format, mut data) = (12, None, None);
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start + size;
        ensure!(end <= bytes.len(), "truncated fixed WAV");
        match &bytes[offset..offset + 4] {
            b"fmt " if size >= 16 => {
                format = Some((
                    u16::from_le_bytes(bytes[start..start + 2].try_into()?),
                    u16::from_le_bytes(bytes[start + 2..start + 4].try_into()?),
                    u32::from_le_bytes(bytes[start + 4..start + 8].try_into()?),
                    u16::from_le_bytes(bytes[start + 14..start + 16].try_into()?),
                ))
            }
            b"data" => data = Some((start, end)),
            _ => {}
        }
        offset = end + size % 2;
    }
    ensure!(
        format == Some((1, 1, 16000, 16)),
        "expected fixed mono PCM16 16kHz"
    );
    let (start, end) = data.ok_or_else(|| anyhow!("fixed WAV data absent"))?;
    ensure!(
        end > start && (end - start) % 2 == 0 && end - start <= 640000,
        "fixed WAV PCM invalid"
    );
    Ok(bytes[start..end]
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect())
}
pub(crate) fn stage_asr(root: &Path, out: &Path) -> Result<(PathBuf, Value)> {
    let fixture = out.join("fixture");
    fs::create_dir_all(fixture.join("bin/local-asr"))?;
    fs::create_dir_all(fixture.join(MODEL_DIRECTORY))?;
    let mut files = vec![];
    for name in ["managed_asr.py", "asr_service.py", "recognizer.py"] {
        let source = root.join("scripts/speech-service").join(name);
        let destination = fixture.join("bin/local-asr").join(name);
        fs::copy(&source, &destination)?;
        let expected = hash(&source)?;
        ensure!(hash(&destination)? == expected, "ASR staged source changed");
        files.push(json!({"kind":"service","name":name,"sha256":expected}));
    }
    for (name, expected) in MODELS {
        let source = root.join(MODEL_DIRECTORY).join(name);
        ensure!(hash(&source)? == expected, "fixed ASR model changed");
        fs::hard_link(source, fixture.join(MODEL_DIRECTORY).join(name))?;
        files.push(json!({"kind":"model","name":name,"sha256":expected}));
    }
    Ok((fixture, json!(files)))
}
pub(crate) fn lab_tts(root: &Path, engine: &str) -> Result<TtsClient> {
    let (port, model) = match engine {
        "qwen" => (18868, "tutor-qwen-tts"),
        "cosy" => (18869, "tutor-cosy-tts"),
        _ => anyhow::bail!("explicit qwen or cosy laboratory service required"),
    };
    // The coordinator gives only this diagnostic child its fresh owned-service
    // credential. No production key/descriptor is read or written.
    let mut token = std::env::var("AI_TUTOR_VOICE_CHAIN_TTS_TOKEN")?.into_bytes();
    let token_text = std::str::from_utf8(&token)?.trim();
    let mut configuration = json!({"protocol_version":1,
        "endpoint":format!("http://127.0.0.1:{port}/v1/audio/speech"),
        "model":model,"bearer_token":token_text});
    if engine == "cosy" {
        configuration["references"] = json!({
            "zh":{"wav_path":root.join(CORPUS).join("zh_concept.wav"),
                "sha256":"4f9ce1ec17a18b88b30ef1a5ade80bd8052e6ddc2e69fbfb190d445b68e2da0f",
                "text":"测距模块告诉主控离墙有多远，电机让小车动起来。"},
            "en":{"wav_path":root.join(CORPUS).join("en_concept.wav"),
                "sha256":"c20d6cc9419de35f20148763e2438618eaae9cc7baf1593a8f858449871c9437",
                "text":"The distance sensor tells the controller how far away the wall is. The motor moves the car."}
        });
    }
    let mut bytes = serde_json::to_vec(&configuration)?;
    let result = TtsClient::from_configuration(&bytes, root);
    bytes.fill(0);
    token.fill(0);
    configuration["bearer_token"] = Value::Null;
    result
}

pub(crate) async fn transcribe(
    client: Arc<AsrClient>,
    wav: &Path,
    report: &mut Value,
) -> Result<String> {
    let mut samples = crate::audio::resample(&read_pcm(wav)?, 16000, 44100);
    let status = Arc::new(Mutex::new(json!({})));
    let token = 1;
    let result: Result<String> = async {
        let mut stream = begin(
            None,
            Some(client),
            status.clone(),
            Arc::new(AtomicU64::new(token)),
            token,
        )
        .ok_or_else(|| anyhow!("managed ASR stream did not start"))?;
        let started = Instant::now();
        let mut revisions = vec![];
        let mut previous_revision = 0;
        for count in (882..samples.len())
            .step_by(882)
            .chain(std::iter::once(samples.len()))
        {
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                started + Duration::from_secs_f64(count as f64 / 44100.0),
            ))
            .await;
            stream.pump(&samples[..count], 44100, false);
            let current = status.lock().unwrap().clone();
            if let Some(revision) = current["asr_partial"]["revision"].as_u64() {
                if revision > previous_revision {
                    revisions.push(json!({"revision":revision,
                        "observed_ms":started.elapsed().as_secs_f64()*1000.0}));
                    previous_revision = revision;
                }
            }
            ensure!(
                current["asr_fallback_reason"].is_null(),
                "real ASR unexpectedly entered fallback"
            );
        }
        report["source_duration_ms"] = json!(samples.len() as f64 / 44.1);
        report["partial_revision_observations"] = json!(revisions);
        let final_started = Instant::now();
        let final_text = stream
            .finish(&samples, 44100)
            .wait()
            .await
            .map_err(|_| anyhow!("managed ASR final failed"))?;
        report["final_tail_ms"] = json!(final_started.elapsed().as_secs_f64() * 1000.0);
        report["worker_elapsed_ms"] = json!(final_text.elapsed_ms);
        ensure!(
            !final_text.text.trim().is_empty(),
            "managed ASR returned empty final"
        );
        report["transcript"] = json!(final_text.text);
        Ok(final_text.text)
    }
    .await;
    samples.fill(0.0);
    report["completed"] = json!(result.is_ok());
    report["fallback_requested"] = json!(!status.lock().unwrap()["asr_fallback_reason"].is_null());
    // FinalWait can request fallback, but this diagnostic never calls Whisper.
    report["whisper_fallback_used"] = json!(false);
    result
}

fn request_from_final(store: &Store, language: &str, text: &str, wav_sha: &str) -> Result<Value> {
    let learner = store.create_learner("SYNTHETIC FIXED WAV MODEL CHAIN", language, "guided")?;
    let lid = s(&learner, "learner_id");
    let initial = store.create_session(lid)?;
    let sid = s(&initial, "session_id");
    let generation = store.bump(sid, "generating")?;
    let event = store.record_education_event(
        lid,
        sid,
        "simulated",
        "learner_statement",
        &json!({"text":text,"synthetic_source":"fixed_adult_sapi_wav","wav_sha256":wav_sha}),
    )?;
    let eid = s(&event, "event_id");
    let rid = uid("synthetic-voice-chain");
    store.add_turn(sid, &rid, generation, "user", text, "shown", &[eid.into()])?;
    let mut fast = store.session(sid)?["state"].clone();
    fast["latest_input"] = json!(text);
    store.checkpoint(sid, &fast)?;
    let session = store.session(sid)?;
    let context = build_context(store, &session, &learner, text, eid, &json!({}))?;
    let request = json!({"request_id":rid,"learner_id":lid,"session_id":sid,
        "generation":generation,"context":context});
    store.create_tutor_stream_with_speech(
        sid,
        &rid,
        generation,
        learner["epoch"].as_i64().unwrap_or(0),
        None,
        None,
    )?;
    store.start_run(&rid, lid, sid, request.to_string().chars().count())?;
    Ok(request)
}

async fn teach(
    provider: &KimiProvider,
    store: &Store,
    request: &Value,
    report: &mut Value,
) -> Result<Vec<TutorSegment>> {
    let started = Instant::now();
    let (send, mut receive) = tokio::sync::mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
    let operation =
        provider.generate_segments(request, request["generation"].as_u64().unwrap(), send);
    tokio::pin!(operation);
    let mut segments = vec![];
    let mut first_segment_observed_ms = None;
    let mut channel_open = true;
    let result = loop {
        tokio::select! { biased;
            result=&mut operation=>break result,
            segment=receive.recv(),if channel_open=>match segment {
                Some(segment)=>{
                    first_segment_observed_ms.get_or_insert(started.elapsed().as_secs_f64()*1000.0);
                    segments.push(segment);
                },
                None=>channel_open=false,
            }
        }
    };
    while let Ok(segment) = receive.try_recv() {
        first_segment_observed_ms.get_or_insert(started.elapsed().as_secs_f64() * 1000.0);
        segments.push(segment);
    }
    report["first_validated_segment_observed_ms"] = json!(first_segment_observed_ms);
    report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["completed"] = json!(result.is_ok());
    let sid = s(request, "session_id");
    let rid = s(request, "request_id");
    store.end_run(
        rid,
        if result.is_ok() {
            "completed"
        } else {
            "failed"
        },
        started.elapsed().as_millis() as u64,
    )?;
    let validated = result?;
    ensure!(
        !segments.is_empty() && segments.len() <= 12,
        "validated reply has no bounded segments"
    );
    for segment in &segments {
        store.append_tutor_segment(sid, rid, segment)?;
    }
    store.mark_tutor_final(sid, rid)?;
    report["validated_reply"] = validated.into_reply();
    report["segment_count"] = json!(segments.len());
    report["stream_phase"] = store.tutor_stream_snapshot(sid, rid)?["phase"].clone();
    report["display_ack_sent"] = json!(false);
    report["memory_or_task_candidates_applied"] = json!(false);
    Ok(segments)
}

async fn synthesize_to_sink(
    client: &TtsClient,
    segment: &TutorSegment,
    language: &str,
) -> (Value, Result<()>) {
    let start = Instant::now();
    let epoch = Arc::new(AtomicU64::new(1));
    let (send, mut receive) = tokio::sync::mpsc::channel(streaming_tts::QUEUE_CAPACITY);
    let operation = client.synthesize(segment.text(), language, "normal", epoch, 1, send);
    tokio::pin!(operation);
    let (mut count, mut nonzero, mut first, mut hash) = (0usize, 0usize, None, Sha256::new());
    let mut sink = |mut packet: Vec<i16>| {
        first.get_or_insert(start.elapsed().as_secs_f64() * 1000.0);
        count += packet.len();
        nonzero += packet.iter().filter(|value| **value != 0).count();
        for sample in &packet {
            hash.update(sample.to_le_bytes());
        }
        packet.fill(0);
    };
    let mut channel_open = true;
    let outcome = loop {
        tokio::select! { biased;
            result=&mut operation=>break result,
            packet=receive.recv(),if channel_open=>match packet {
                Some(packet)=>sink(packet), None=>channel_open=false,
            }
        }
    };
    while let Ok(packet) = receive.try_recv() {
        sink(packet);
    }
    drop(sink);
    let mut evidence = json!({"sequence":segment.sequence(),"text":segment.text(),
        "elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"first_pcm_received_ms":first,
        "samples_received":count,"nonzero_samples":nonzero,
        "pcm_sha256":format!("{:x}",hash.finalize()),"speaker_output":false,
        "playback_receipt_created":false,"raw_pcm_saved":false});
    let checked: Result<()> = (|| {
        let SynthesisOutcome::Completed(metrics) = outcome? else {
            anyhow::bail!("TTS transport cancelled");
        };
        evidence["transport_metrics"] = metrics.clone();
        ensure!(
            count > 0 && nonzero > 0 && metrics["samples"].as_u64() == Some(count as u64),
            "TTS PCM sink did not receive the complete nonempty transport"
        );
        Ok(())
    })();
    evidence["completed"] = json!(checked.is_ok());
    (evidence, checked)
}

#[tokio::test]
#[ignore = "Explicit fixed synthetic WAV -> real owned ASR -> verified Kimi -> real laboratory TTS PCM sink; no devices"]
async fn real_synthetic_asr_teaching_tts_transport_chain() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC").as_deref() == Ok("1"),
        "explicit synthetic opt-in required"
    );
    let language = std::env::var("AI_TUTOR_VOICE_CHAIN_LANGUAGE")?;
    let engine = std::env::var("AI_TUTOR_VOICE_CHAIN_TTS_ENGINE")?;
    let run = std::env::var("AI_TUTOR_VOICE_CHAIN_RUN")?;
    ensure!(
        !run.is_empty()
            && run.len() <= 80
            && run
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid unique run name"
    );
    ensure!(
        ["qwen", "cosy"].contains(&engine.as_str()),
        "explicit laboratory TTS engine required"
    );
    let (source_id, source_hash, reference) = fixed_source(&language)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let out = root.join("runtime/goal-baselines/20261104").join(run);
    fs::create_dir(&out)?;
    let mut report = json!({"test_type":"real_synthetic_asr_teaching_tts_transport_chain","passed":false,
        "language":language,"tts_engine":engine,"source_id":source_id,"source_sha256":source_hash,"reference":reference,
        "microphone_opened":false,"speaker_output":false,"ambient_audio_used":false,
        "raw_pcm_saved":false,"input_closed_claimed":false,"playback_receipts_created":false,
        "voice_loop_permission_or_rearm_tested":false,"browser_or_acoustic_quality_tested":false,
        "production_policy_or_descriptor_written":false,"legacy_asr_service_contacted":false,
        "tts_service_owned_or_stopped_by_probe":false,"tts_service_must_be_owned_and_warmed_by_coordinator":true,
        "automatic_retry":false,"teaching_request_limit":1,"tts_segment_limit":12,
        "teaching_final_precedes_tts":true,"asr":{},"teaching":{},"tts":[],"stage":"preflight"});
    let mut owner = None;
    let mut provider = None;
    let mut before = None;
    let mut progress = vec![];
    let started = Instant::now();
    let operation: Result<()> = async {
        before = Some(protected(&root)?);
        report["protected_before_sha256"] = before.clone().unwrap();
        let wav = root.join(CORPUS).join(format!("{source_id}.wav"));
        ensure!(hash(&wav)? == source_hash, "fixed synthetic WAV hash changed");
        let manifest: Value = serde_json::from_slice(&fs::read(root.join(CORPUS).join("manifest.json"))?)?;
        ensure!(manifest["audible_output"] == false && manifest["samples"].as_array().is_some_and(|rows|
            rows.iter().any(|row|row["id"]==source_id && row["sha256"]==source_hash && row["source_type"]=="synthetic_sapi_adult_voice")), "frozen synthetic source provenance mismatch");
        let (fixture, files) = stage_asr(&root, &out)?;
        write_new(&out.join("method.json"), &json!({"version":1,"files":files,
            "test_executable_sha256":hash(&std::env::current_exe()?)?,
            "diagnostic_sha256":hash(&root.join("rust/ai-tutor/src/audio/voice_model_chain_diagnostic.rs"))?,
            "source_sha256":source_hash,"source_type":"synthetic_sapi_adult_voice",
            "asr":"private owned instance; 16k WAV -> 44.1k paced20ms capture transport -> 16k HTTP PCM",
            "teaching":"one real verified ACP call; full SegmentDecoder validation; isolated simulator Store",
            "tts":"sequential validated segments after final; bounded PCM sink; no playback owner or device",
            "cold_cache_measured":false,"tts_preparation_run_by_probe":false}))?;
        report["method_sha256"] = json!(hash(&out.join("method.json"))?);
        report["stage"] = json!("verify_teaching_and_tts_ready");
        provider = Some(KimiProvider::new(&root)?);
        let audit = provider.as_ref().unwrap().probe().await?;
        ensure!(audit["available"] == true && audit["verified"] == true && audit["test_transport"].is_null(), "existing native Kimi canary verification required; no fake transport");
        report["teaching_audit"] = json!({"available":audit["available"],"verified":audit["verified"],
            "version":audit["version"],"binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"]});
        let tts = lab_tts(&root, &engine)?;
        tts.check_ready().await?;
        report["tts_ready"] = json!(true);
        report["stage"] = json!("prepare_owned_asr");
        let policy = LocalAsrPolicy { protocol_version:1, mode:AsrMode::Managed,
            python_executable:Some(root.join("runtime/speech-lab/.venv/Scripts/python.exe")) };
        owner = Some(OwnedAsrService::spawn(&fixture, &policy)?);
        let (_cancel, mut cancelled) = tokio::sync::watch::channel(false);
        let client = owner.as_mut().unwrap().prepare(&mut cancelled, |event| {
            progress.push(json!({"state":event["state"],"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}));
        }).await?;
        report["stage"] = json!("transcribe_fixed_wav");
        let transcript = transcribe(client, &wav, &mut report["asr"]).await?;
        write_new(&out.join("asr.json"), &report["asr"])?;
        report["stage"] = json!("teach_from_real_final");
        let store = Store::open(out.join("synthetic.sqlite3"), "simulator")?;
        let request = request_from_final(&store, &language, &transcript, source_hash)?;
        let segments = teach(provider.as_ref().unwrap(), &store, &request, &mut report["teaching"]).await?;
        write_new(&out.join("teaching.json"), &report["teaching"])?;
        report["stage"] = json!("synthesize_validated_segments");
        for segment in &segments {
            let (evidence, outcome) = synthesize_to_sink(&tts, segment, &language).await;
            write_new(&out.join(format!("tts-segment-{}.json",segment.sequence())), &evidence)?;
            report["tts"].as_array_mut().unwrap().push(evidence);
            outcome?;
        }
        store.checkpoint_wal()?;
        report["stage"] = json!("completed");
        Ok(())
    }.await;
    report["asr_progress"] = json!(progress);
    // All Result failures after spawn still wait for exact owned cleanup.
    let stop = match owner.as_mut() {
        Some(owner) => owner.stop().await,
        None => json!({"not_started":true,"cleanup_confirmed":true}),
    };
    report["owned_asr_stop"] = stop.clone();
    let provider_clean = match provider.as_ref() {
        Some(provider) => {
            report["provider_metrics"] = provider.status()["metrics"].clone();
            provider.close().await.is_ok()
        }
        None => true,
    };
    report["provider_cleanup_confirmed"] = json!(provider_clean);
    let after = protected(&root);
    let unchanged = matches!((&before,&after),(Some(before),Ok(after)) if before==after);
    report["protected_after_sha256"] = after.unwrap_or(Value::Null);
    report["protected_files_unchanged"] = json!(unchanged);
    report["total_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    let passed =
        operation.is_ok() && stop["cleanup_confirmed"] == true && provider_clean && unchanged;
    report["passed"] = json!(passed);
    report["error"] = if operation.is_err() {
        json!("stage_failed; raw provider/service error omitted")
    } else {
        Value::Null
    };
    write_new(&out.join("report.json"), &report)?;
    println!(
        "Real synthetic model-chain report: {}",
        out.join("report.json").display()
    );
    operation?;
    ensure!(
        passed,
        "owned cleanup or protected-file verification failed"
    );
    Ok(())
}

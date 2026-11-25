//! Real model/output experiment with one simulated input source and display.
//! No lease phase, endpoint candidate, or speech receipt is assigned here.
use super::*;
use crate::{
    audio::{
        InputBinding,
        voice_loop_source::WavSource,
        voice_model_chain_diagnostic::{
            CORPUS, fixed_source, hash, lab_tts, protected, stage_asr, write_new,
        },
    },
    local_asr::{AsrMode, LocalAsrPolicy, OwnedAsrService},
};
use std::fs;

pub(super) async fn call(
    app: &Arc<Workstation>,
    method: Method,
    path: &str,
    body: Value,
) -> Result<Value> {
    dispatch(app.clone(), &method, path, &body).await
}
pub(super) fn generation(app: &Workstation, sid: &str) -> Result<Value> {
    Ok(app.stores.lock().unwrap()["simulator"].session(sid)?["generation"].clone())
}
pub(super) fn observation(app: &Workstation, sid: &str, rid: &str) -> Result<(Value, Vec<Value>)> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let snapshot = store.tutor_stream_snapshot(sid, rid)?;
    let mut rows = store.rows(
        "SELECT * FROM tutor_segments WHERE request_id=? ORDER BY sequence",
        [rid],
    )?;
    for row in &mut rows {
        for field in ["speech_started_receipt", "speech_terminal_receipt"] {
            if let Some(raw) = row[field].as_str() {
                row[field] = serde_json::from_str(raw)?;
            }
        }
    }
    Ok((snapshot, rows))
}
pub(super) async fn heartbeat(app: &Arc<Workstation>, sid: &str, tag: &Value) -> Result<Value> {
    call(
        app,
        Method::POST,
        &format!(
            "/api/sessions/{sid}/voice-loop/{}/heartbeat",
            s(tag, "client_loop_id")
        ),
        json!({"loop_epoch":tag["loop_epoch"]}),
    )
    .await
}
fn normalized_transcript(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn verify_user_turn(
    store: &Store,
    sid: &str,
    rid: &str,
    index: usize,
    actual_final: &Value,
) -> Result<u64> {
    let count = store.rows(
        "SELECT COUNT(*) AS count FROM turns WHERE session_id=? AND role='user'",
        [sid],
    )?;
    let users = store.rows(
        "SELECT content FROM turns WHERE session_id=? AND request_id=? AND role='user'",
        [sid, rid],
    )?;
    ensure!(
        count[0]["count"].as_u64() == Some(index as u64) && users.len() == 1,
        "duplicate or missing user turn"
    );
    ensure!(
        users[0]["content"] == actual_final["text"],
        "teaching input differs from the actual ASR final"
    );
    Ok(index as u64)
}
pub(super) async fn round(
    app: &Arc<Workstation>,
    sid: &str,
    tag: &Value,
    input: &InputBinding,
    source: &WavSource,
    index: usize,
    reference: &str,
    report: &mut Value,
) -> Result<()> {
    round_with_before_submit(
        app,
        sid,
        tag,
        input,
        source,
        index,
        reference,
        std::future::ready(Ok(Value::Null)),
        report,
    )
    .await
}

// Test-only coordination after the real final, before unmodified submission.
// Existing callers use a ready no-op future; no production state is assigned.
pub(super) async fn round_with_before_submit<F>(
    app: &Arc<Workstation>,
    sid: &str,
    tag: &Value,
    input: &InputBinding,
    source: &WavSource,
    index: usize,
    reference: &str,
    before_submit: F,
    report: &mut Value,
) -> Result<()>
where
    F: std::future::Future<Output = Result<Value>>,
{
    round_with_before_submit_expected_asr(
        app,
        sid,
        tag,
        input,
        source,
        index,
        reference,
        "sherpa-onnx / bilingual Zipformer / local streaming",
        before_submit,
        report,
    )
    .await
}

// The new joint diagnostic requires the actual captured Qwen final. Old callers
// retain their exact Zipformer assertion; no caller silently accepts fallback.
pub(super) async fn round_with_before_submit_expected_asr<F>(
    app: &Arc<Workstation>,
    sid: &str,
    tag: &Value,
    input: &InputBinding,
    source: &WavSource,
    index: usize,
    reference: &str,
    expected_provider: &str,
    before_submit: F,
    report: &mut Value,
) -> Result<()>
where
    F: std::future::Future<Output = Result<Value>>,
{
    let started = Instant::now();
    let source_before = source.status();
    ensure!(
        source_before["active"] == false && source_before["opens"] == source_before["closes"],
        "source is not idle before next round"
    );
    let source_index = source_before["opens"]
        .as_u64()
        .ok_or_else(|| anyhow!("source open count absent"))?
        + 1;
    *report = json!({"case":format!("round_{index}"),"completed":false,"passed":false,
        "rearm_observed":false,"whisper_fallback_used":false,"overlap_observed":false,"stage":"start",
        "observed_offsets_ms":{"round_start":0.0}});
    report["source_round_index"] = json!(source_index);
    let start_body = json!({"session_id":sid,"expected_generation":generation(app,sid)?,
        "device_key":input.device_key,"expected_input_topology_revision":input.topology_revision,
        "endpoint_mode":"voice_endpoint_v1","voice_loop":tag});
    let start = call(
        app,
        Method::POST,
        "/api/audio/record/start",
        start_body.clone(),
    )
    .await?;
    report["start"] = start.clone();
    report["observed_offsets_ms"]["capture_started"] =
        json!(started.elapsed().as_secs_f64() * 1000.0);
    ensure!(
        start["voice_loop"]["phase"] == "listening" && source.status()["active"] == true,
        "real capture did not enter listening"
    );
    let token = start["recording_generation"]
        .as_u64()
        .ok_or_else(|| anyhow!("missing capture token"))?;
    ensure!(
        call(app, Method::POST, "/api/audio/record/start", start_body)
            .await
            .is_err(),
        "duplicate Start accepted"
    );
    ensure!(
        app.audio.status()["recording_generation"] == token
            && source.status()["opens"] == source_index,
        "duplicate Start changed capture"
    );
    report["duplicate_start_rejected"] = json!(true);
    let endpoint_deadline = Instant::now() + Duration::from_secs(24);
    loop {
        let status = app.audio.status();
        ensure!(
            status["recording_generation"] == token,
            "capture generation revoked"
        );
        ensure!(status["state"] != "speaking", "capture/output overlap");
        if status["recording_endpoint"]["recording_generation"] == token
            && status["recording_endpoint"]["input_closed"] == true
        {
            report["recording_endpoint"] = status["recording_endpoint"].clone();
            report["source_after_endpoint"] = source.status();
            ensure!(
                source.status()["active"] == false && source.status()["closes"] == source_index,
                "endpoint preceded source join"
            );
            ensure!(
                status["recording_endpoint"]["reason"] == "speech_end",
                "real VAD did not identify speech_end"
            );
            break;
        }
        ensure!(
            Instant::now() < endpoint_deadline,
            "endpoint observation deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    report["stage"] = json!("finalize");
    report["observed_offsets_ms"]["endpoint_closed"] =
        json!(started.elapsed().as_secs_f64() * 1000.0);
    let ticket = json!({"session_id":start["session_id"],"session_generation":start["session_generation"],
        "language":start["language"],"recording_generation":token});
    let final_text = call(app, Method::POST, "/api/audio/record/stop", ticket.clone()).await?;
    report["asr"] = final_text.clone();
    report["expected_asr_provider"] = json!(expected_provider);
    report["observed_offsets_ms"]["asr_final"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["asr_fallback_requested"] = json!(!final_text["fallback_reason"].is_null());
    report["asr_text_matches_fixed_source"] =
        json!(normalized_transcript(s(&final_text, "text")) == normalized_transcript(reference));
    ensure!(
        !s(&final_text, "text").trim().is_empty()
            && final_text["provider"] == expected_provider
            && final_text["fallback_reason"].is_null()
            && final_text["recording_endpoint"]["reason"] == "speech_end"
            && final_text["recording_endpoint"]["input_closed"] == true,
        "real managed final missing"
    );
    ensure!(
        report["asr_text_matches_fixed_source"] == true,
        "fixed sentence transcription differs; no correction or retry"
    );
    ensure!(
        call(app, Method::POST, "/api/audio/record/stop", ticket)
            .await
            .is_err(),
        "duplicate final accepted"
    );
    ensure!(
        heartbeat(app, sid, tag).await?["phase"] == "ready_to_submit",
        "final not ready to submit"
    );
    while app.last_input.lock().unwrap().elapsed() <= Duration::from_millis(1550) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let coordination_started = Instant::now();
    report["observed_offsets_ms"]["before_submit_coordination_begin"] =
        json!(started.elapsed().as_secs_f64() * 1000.0);
    let coordinated = before_submit.await;
    report["before_submit_coordination_elapsed_ms"] =
        json!(coordination_started.elapsed().as_secs_f64() * 1000.0);
    report["before_submit_coordination_completed"] = json!(coordinated.is_ok());
    report["before_submit_coordination"] = coordinated?;
    let body = json!({"text":final_text["text"],"expected_generation":final_text["session_generation"],
        "voice_loop":tag,"recording_generation":token,"auto_speak":true,"client_submission_id":format!("real-loop-round-{index}")});
    report["stage"] = json!("teaching_and_playback");
    report["observed_offsets_ms"]["submit_begin"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    let reply = call(
        app,
        Method::POST,
        &format!("/api/sessions/{sid}/turns"),
        body.clone(),
    )
    .await?;
    report["submit"] = reply.clone();
    report["observed_offsets_ms"]["submit_accepted"] =
        json!(started.elapsed().as_secs_f64() * 1000.0);
    ensure!(
        reply["voice_loop"]["phase"] == "reply",
        "submit did not enter reply"
    );
    ensure!(
        call(
            app,
            Method::POST,
            &format!("/api/sessions/{sid}/turns"),
            body
        )
        .await
        .is_err(),
        "duplicate bound Send accepted"
    );
    let rid = s(&reply, "request_id");
    let mut acknowledged = 0usize;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut checked_early_start = false;
    loop {
        let (snapshot, rows) = observation(app, sid, rid)?;
        report["last_snapshot"] = snapshot.clone();
        report["segments"] = json!(rows);
        if snapshot["final_validated"] == true
            && report["observed_offsets_ms"]["text_final"].is_null()
        {
            report["observed_offsets_ms"]["text_final"] =
                json!(started.elapsed().as_secs_f64() * 1000.0);
        }
        if rows
            .iter()
            .any(|row| row["speech_started_receipt"]["milestone"] == "first_callback_submission")
            && report["observed_offsets_ms"]["first_callback_receipt"].is_null()
        {
            report["observed_offsets_ms"]["first_callback_receipt"] =
                json!(started.elapsed().as_secs_f64() * 1000.0);
        }
        ensure!(
            source.status()["active"] == false,
            "input source reopened before output completion"
        );
        ensure!(
            !["failed", "cancelled", "interrupted"].contains(&s(&snapshot, "phase")),
            "teaching stream failed"
        );
        if rows
            .iter()
            .any(|row| row["speech_started_receipt"]["milestone"] == "first_callback_submission")
            && snapshot["final_validated"] == false
        {
            report["overlap_observed"] = json!(true);
        }
        if !checked_early_start && rows.iter().any(|row| row["speech_state"] == "started") {
            let opens = source.status()["opens"].clone();
            let attempted = json!({"session_id":sid,"expected_generation":generation(app,sid)?,
                "device_key":input.device_key,"expected_input_topology_revision":input.topology_revision,
                "endpoint_mode":"voice_endpoint_v1","voice_loop":tag});
            ensure!(
                call(app, Method::POST, "/api/audio/record/start", attempted)
                    .await
                    .is_err(),
                "Start overlapped output"
            );
            ensure!(
                source.status()["opens"] == opens,
                "rejected Start opened source"
            );
            checked_early_start = true;
        }
        if acknowledged < rows.len() {
            call(
                app,
                Method::POST,
                &format!("/api/sessions/{sid}/streams/{rid}/display"),
                json!({"generation":reply["generation"],"sequence":acknowledged}),
            )
            .await?;
            acknowledged += 1;
            continue;
        }
        if snapshot["phase"] == "completed" && snapshot["speech_complete"] == true {
            ensure!(!rows.is_empty(), "empty spoken response");
            for row in &rows {
                let start = &row["speech_started_receipt"];
                let end = &row["speech_terminal_receipt"];
                ensure!(
                    row["speech_state"] == "completed"
                        && row["speech_end_origin"] == "owner_receipt"
                        && start["milestone"] == "first_callback_submission"
                        && start["engine_kind"] == "qwen_pcm"
                        && end["state"] == "completed"
                        && end["engine_kind"] == "qwen_pcm"
                        && end["playback_id"] == start["playback_id"]
                        && end["epoch"] == start["epoch"]
                        && end["callback_submitted_frames"]
                            .as_u64()
                            .is_some_and(|v| v > 0)
                        && end["predicted_output_drained"] == true
                        && end["speaker_output_measured"] == false,
                    "segment lacks real matching CPAL completion"
                );
            }
            break;
        }
        ensure!(Instant::now() < deadline, "teaching/playback deadline");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    report["start_during_output_rejected"] = json!(checked_early_start);
    report["stage"] = json!("rearm");
    report["observed_offsets_ms"]["speech_complete"] =
        json!(started.elapsed().as_secs_f64() * 1000.0);
    let deadline = Instant::now() + Duration::from_secs(5);
    let rearm_started = Instant::now();
    let mut cooldown_observed = false;
    loop {
        let status = heartbeat(app, sid, tag).await?;
        cooldown_observed |= status["phase"] == "cooldown";
        if status["rearm_ready"] == true {
            ensure!(
                status["phase"] == "armed"
                    && app.audio.status()["state"] == "idle"
                    && source.status()["active"] == false,
                "rearm without idle owners"
            );
            report["rearm"] = status;
            report["rearm_observed"] = json!(true);
            report["cooldown_observed"] = json!(cooldown_observed);
            report["rearm_wait_observed_ms"] =
                json!(rearm_started.elapsed().as_secs_f64() * 1000.0);
            break;
        }
        ensure!(
            status["phase"] != "paused" && Instant::now() < deadline,
            "completed output did not rearm"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let session = store.session(sid)?;
    // session.turns is a bounded display page, not the complete history.
    let count = verify_user_turn(store, sid, rid, index, &final_text)?;
    report["submitted_actual_asr_text_unchanged"] = json!(true);
    report["user_turn_total"] = json!(count);
    report["request_user_turn_count"] = json!(1);
    report["fixture_state_after"] = session;
    report["source_lifecycle"] = source.status();
    report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["observed_offsets_ms"]["rearm_complete"] = report["elapsed_ms"].clone();
    report["candidate_application_completed"] = json!(true);
    report["completed"] = json!(true);
    report["passed"] = json!(true);
    report["stage"] = json!("completed");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Explicit fixed WAV through real capture/VAD/managed ASR/Kimi/CPAL, two voice-loop rounds; DEFAULT OUTPUT PLAYBACK, no microphone/camera/PTZ"]
async fn real_synthetic_voice_loop_two_rounds() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_REAL_VOICE_LOOP").as_deref() == Ok("1"),
        "explicit real voice-loop opt-in required"
    );
    let language = std::env::var("AI_TUTOR_VOICE_CHAIN_LANGUAGE")?;
    let engine = std::env::var("AI_TUTOR_VOICE_CHAIN_TTS_ENGINE")?;
    ensure!(engine == "qwen", "Qwen required");
    let run = std::env::var("AI_TUTOR_VOICE_CHAIN_RUN")?;
    ensure!(
        !run.is_empty()
            && run.len() <= 80
            && run
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid unique run name"
    );
    let (root_source, source_sha, reference) = fixed_source(&language)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let out = root.join("runtime/goal-baselines/20261104").join(run);
    fs::create_dir(&out)?;
    let mut report = json!({"test_type":"real_synthetic_voice_loop_two_rounds","passed":false,"language":language,
        "production_capture_loop":true,"synthetic_capture_source":true,"production_audio_owner":true,
        "simulated_display_ack":true,"simulated_client_heartbeat":true,"microphone_opened":false,"camera_opened":false,
        "ptz_enabled":false,"raw_pcm_saved":false,"speaker_output_measured":false,"system_volume_changed":false,
        "ambient_audio_used":false,"browser_tested":false,"acoustic_loop_tested":false,"production_policy_or_descriptor_written":false,
        "tts_service_lifecycle_owner":"outer_runner","asr_service_lifecycle_owner":"diagnostic_owned_instance",
        "same_main_managed_joint_lifecycle_tested":false,"automatic_retry":false,"teaching_request_limit":2,
        "source_id":root_source,"source_sha256":source_sha,"reference":reference,"rounds":[],"stage":"preflight"});
    let mut owner = None;
    let mut app_holder = None;
    let mut source_holder = None;
    let mut heartbeat_task = None;
    let (mut heartbeat_stop, mut tag) = (None, Value::Null);
    let traces = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mut before = None;
    let started = Instant::now();
    let operation:Result<()>=async {
        before=Some(protected(&root)?);
        let wav=root.join(CORPUS).join(format!("{root_source}.wav"));ensure!(hash(&wav)?==source_sha,"fixed WAV changed");
        let corpus:Value=serde_json::from_slice(&fs::read(root.join(CORPUS).join("manifest.json"))?)?;
        ensure!(corpus["audible_output"]==false&&corpus["samples"].as_array().is_some_and(|rows|rows.iter().any(|r|
            r["id"]==root_source&&r["sha256"]==source_sha&&r["source_type"]=="synthetic_sapi_adult_voice")),"fixed synthetic provenance changed");
        let(fixture,files)=stage_asr(&root,&out)?;
        fs::create_dir_all(fixture.join("runtime"))?;
        write_new(&fixture.join("runtime/tutor-delivery.json"),&json!({"protocol_version":1,"mode":"segments","segment_speech":"after_display"}))?;
        let mut app=Workstation::new(fixture.clone(),"synthetic-real-voice-loop".into())?;
        Arc::get_mut(&mut app).ok_or_else(||anyhow!("unexpected shared fixture"))?.provider=Arc::new(KimiProvider::new(&root)?);
        app_holder=Some(app.clone());
        let mut hashes=serde_json::Map::new();
        for file in ["audio.rs","audio/input_devices.rs","audio/voice_loop_source.rs","audio/voice_endpoint.rs","audio/capture_endpoint.rs","voice_loop_model_diagnostic.rs","voice_loop.rs","recording.rs","segment_speech.rs","store/tutor_speech.rs"]{
            hashes.insert(file.into(),json!(hash(&root.join("rust/ai-tutor/src").join(file))?));
        }
        write_new(&out.join("method.json"),&json!({"version":1,"source_files":hashes,"asr_assets":files,
            "test_executable_sha256":hash(&std::env::current_exe()?)?,"wav_sha256":source_sha,
            "capture":"fixed 16k WAV plus explicitly declared 4000ms quiet fixture tail; resampled44.1k paced20ms; real capture_loop and Sonora, no microphone",
            "input_closed":"source Drop signals and joins synthetic producer before the unchanged capture owner records closure; not physical microphone closure",
            "speech":"real Qwen/native CPAL receipts, callback submission and predicted drain only",
            "loop":"production dispatch transitions; simulated display/client heartbeat; no phase assignments or receipt fabrication",
            "application":"normal production candidate commit in isolated simulator DB; no claim that candidates remain shadow"}))?;
        report["method_sha256"]=json!(hash(&out.join("method.json"))?);
        let audit=app.provider.probe().await?;
        ensure!(audit["available"]==true&&audit["verified"]==true&&audit["test_transport"].is_null(),"real verified Kimi required");
        report["provider_audit"]=json!({"version":audit["version"],"binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"]});
        let tts=Arc::new(lab_tts(&root,&engine)?);tts.check_ready().await?;
        let installed=app.audio.configure_tts(Some(tts),app.audio.speech_epoch()).await?;
        ensure!(installed.applied&&installed.engine_kind==Some("qwen_pcm"),"real Qwen client not installed");
        report["tts_install_receipt"]=json!(installed);
        report["stage"]=json!("prepare_asr");
        owner=Some(OwnedAsrService::spawn(&fixture,&LocalAsrPolicy{protocol_version:1,mode:AsrMode::Managed,
            python_executable:Some(root.join("runtime/speech-lab/.venv/Scripts/python.exe"))})?);
        let(_cancel,mut cancelled)=watch::channel(false);
        let client=owner.as_mut().unwrap().prepare(&mut cancelled,|_|{}).await?;
        app.audio.install_streaming_asr(client);
        let(input,source)=app.audio.install_voice_loop_wav(&wav)?;source_holder=Some(source.clone());
        let sid={let stores=app.stores.lock().unwrap();let store=&stores["simulator"];
            let learner=store.create_learner("SYNTHETIC REAL VOICE LOOP",&language,"guided")?;
            s(&store.create_session(s(&learner,"learner_id"))?,"session_id").to_owned()};
        *app.current.lock().unwrap()=Some(("simulator".into(),sid.clone()));
        report["session_id"]=json!(sid);
        let enabled=call(&app,Method::POST,&format!("/api/sessions/{sid}/voice-loop/enable"),json!({
            "client_loop_id":"real-fixed-wav-two-rounds","expected_generation":generation(&app,&sid)?,"language":language,
            "device_key":input.device_key,"expected_input_topology_revision":input.topology_revision})).await?;
        ensure!(enabled["phase"]=="armed"&&source.status()["opens"]==0,"enable opened capture");
        tag=json!({"client_loop_id":enabled["client_loop_id"],"loop_epoch":enabled["loop_epoch"]});report["enabled"]=enabled;
        let(stop,mut stopped)=watch::channel(false);heartbeat_stop=Some(stop);
        let heartbeat_app=app.clone();let heartbeat_sid=sid.clone();let heartbeat_tag=tag.clone();let trace=traces.clone();
        heartbeat_task=Some(tokio::spawn(async move {
            let mut last=Value::Null;
            loop {
                tokio::select! {biased; _=stopped.changed()=>break,
                    _=tokio::time::sleep(Duration::from_millis(400))=>{
                        let status=heartbeat(&heartbeat_app,&heartbeat_sid,&heartbeat_tag).await?;
                        ensure!(status["phase"]!="paused","client heartbeat observed revoked lease");
                        if status["phase"]!=last {last=status["phase"].clone();let mut rows=trace.lock().unwrap();
                            if rows.len()<64 {rows.push(json!({"phase":last,"observed_ms":started.elapsed().as_secs_f64()*1000.0,"revision":status["revision"]}));}}
                    }
                }
            }
            Ok::<(),anyhow::Error>(())
        }));
        for index in 1..=2 {
            report["stage"]=json!(format!("round_{index}"));let mut detail=Value::Null;
            let result=round(&app,&sid,&tag,&input,&source,index,reference,&mut detail).await;
            write_new(&out.join(format!("round-{index}.json")),&detail)?;
            report["rounds"].as_array_mut().unwrap().push(detail);result?;
        }
        ensure!(source.status()["opens"]==2&&source.status()["closes"]==2&&!source.status()["closed_streams"].as_array().unwrap().iter().any(|v|v["producer_joined"]!=true),"source ownership incomplete");
        let metrics=app.provider.status()["metrics"].clone();
        ensure!(metrics["requests"]==2&&metrics["acp_segment_requests"]==2,"unexpected model invocation count");
        report["stage"]=json!("completed");Ok(())
    }.await;
    if let Some(stop) = heartbeat_stop {
        let _ = stop.send(true);
    }
    let heartbeat_clean = match heartbeat_task {
        Some(task) => matches!(task.await, Ok(Ok(()))),
        None => true,
    };
    report["heartbeat_finished_cleanly"] = json!(heartbeat_clean);
    report["phase_observations"] = json!(traces.lock().unwrap().clone());
    let provider_clean = if let Some(app) = &app_holder {
        report["provider_metrics"] = app.provider.status()["metrics"].clone();
        if !tag.is_null() {
            let sid = s(&report, "session_id");
            report["explicit_stop"] = call(
                app,
                Method::POST,
                &format!(
                    "/api/sessions/{sid}/voice-loop/{}/stop",
                    s(&tag, "client_loop_id")
                ),
                json!({"loop_epoch":tag["loop_epoch"],"reason":"diagnostic_complete"}),
            )
            .await
            .unwrap_or(Value::Null);
        }
        app.close().await;
        app.provider.close().await.is_ok() && app.provider.status()["running"] == 0
    } else {
        true
    };
    report["source_lifecycle"] = source_holder
        .as_ref()
        .map_or(Value::Null, |source| source.status());
    report["provider_cleanup_confirmed"] = json!(provider_clean);
    let stop = match owner.as_mut() {
        Some(owner) => owner.stop().await,
        None => json!({"not_started":true,"cleanup_confirmed":true}),
    };
    report["owned_asr_stop"] = stop.clone();
    let after = protected(&root);
    let unchanged = matches!((&before,&after),(Some(a),Ok(b))if a==b);
    report["protected_files_unchanged"] = json!(unchanged);
    report["protected_before_sha256"] = before.unwrap_or(Value::Null);
    report["protected_after_sha256"] = after.unwrap_or(Value::Null);
    report["total_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    let passed = operation.is_ok()
        && heartbeat_clean
        && provider_clean
        && stop["cleanup_confirmed"] == true
        && unchanged;
    report["passed"] = json!(passed);
    report["error"] = if operation.is_err() {
        json!("stage_failed; raw provider/service error omitted")
    } else {
        Value::Null
    };
    write_new(&out.join("report.json"), &report)?;
    println!(
        "Real voice-loop report: {}",
        out.join("report.json").display()
    );
    operation?;
    ensure!(passed, "cleanup or protected-file check failed");
    Ok(())
}

#[test]
fn fixed_transcript_comparison_ignores_punctuation_not_missing_or_changed_words() {
    assert_eq!(
        normalized_transcript("First, stop the car."),
        normalized_transcript("FIRST STOP THE CAR!")
    );
    assert_eq!(
        normalized_transcript("先让小车停下来。"),
        normalized_transcript("先让小车停下来")
    );
    assert_ne!(
        normalized_transcript("First stop the car."),
        normalized_transcript("Stop the car.")
    );
    assert_ne!(
        normalized_transcript("先让小车停下来"),
        normalized_transcript("先让小车动起来")
    );
}

#[test]
fn accumulated_user_turn_check_exceeds_display_page_and_still_rejects_duplicates() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("simulator.sqlite3"), "simulator")?;
    let learner = store.create_learner("SYNTHETIC HISTORY", "en", "guided")?;
    let session = store.create_session(s(&learner, "learner_id"))?;
    let sid = s(&session, "session_id");
    for index in 1..=30 {
        store.add_turn(
            sid,
            &format!("req-{index}"),
            0,
            "user",
            "ACTUAL FINAL",
            "shown",
            &[],
        )?;
    }
    assert!(store.session(sid)?["turns"].as_array().unwrap().len() < 30);
    assert_eq!(
        verify_user_turn(&store, sid, "req-30", 30, &json!({"text":"ACTUAL FINAL"}))?,
        30
    );
    assert!(
        verify_user_turn(&store, sid, "req-30", 30, &json!({"text":"REPLACED FINAL"})).is_err()
    );
    store.add_turn(sid, "req-30", 0, "user", "ACTUAL FINAL", "shown", &[])?;
    assert!(verify_user_turn(&store, sid, "req-30", 31, &json!({"text":"ACTUAL FINAL"})).is_err());
    Ok(())
}

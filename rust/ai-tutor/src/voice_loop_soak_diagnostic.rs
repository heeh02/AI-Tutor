//! Explicit 1800 s real-model/native-output soak. Only capture input and display
//! ACK are synthetic; camera frames stay in the existing in-memory pipeline.
use super::voice_loop_model_diagnostic::{call, generation, heartbeat, round};
use super::*;
use crate::audio::voice_model_chain_diagnostic::{
    CORPUS, fixed_source, hash, lab_tts, protected, stage_asr, write_new,
};
use crate::local_asr::{AsrMode, LocalAsrPolicy, OwnedAsrService};
use std::path::Path;
use std::{collections::BTreeMap, fs, io::Write};

const ACTIVE_SECONDS: u64 = 1800;
const DRAIN_SECONDS: u64 = 180;
const MAX_REQUESTS: usize = 96;
const MAX_SAMPLE_GAP_MS: f64 = 3000.0;
pub(super) const CAMERA_ASSETS: [(&str, &str); 3] = [
    (
        "models/yolov8n-face-lindevs.onnx",
        "8d0bfb0c3383c5bd7a78dd24ef79a21e2aa456619b6ab5e53867092d1c7dc414",
    ),
    (
        "models/native/onnxruntime.dll",
        "2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492",
    ),
    (
        "models/native/onnxruntime_providers_shared.dll",
        "6adee08db23ba190fd1061ee99c32a5a802cac1e3576defaac820c157294c138",
    ),
];

fn elapsed(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn sample_gap_valid(previous_ms: f64, next_ms: f64) -> bool {
    previous_ms.is_finite()
        && next_ms.is_finite()
        && next_ms >= previous_ms
        && next_ms - previous_ms <= MAX_SAMPLE_GAP_MS
}

// Provider status holds the most recently completed call, not necessarily this
// round. A timeout or observer failure can end the round before its call exits.
pub(super) fn bound_provider_diagnostics(round: &Value, metrics: &Value) -> Value {
    let request_id = round["submit"]["request_id"]
        .as_str()
        .filter(|value| !value.is_empty());
    let generation = round["submit"]["generation"]
        .as_u64()
        .filter(|value| *value > 0);
    let last = &metrics["last_acp_segments"];
    let matched = request_id.is_some()
        && generation.is_some()
        && last["request_id"].as_str() == request_id
        && last["generation"].as_u64() == generation;
    json!({"request_id":request_id,"generation":generation,"matched":matched,
        "last_acp_segments":if matched { last.clone() } else { Value::Null }})
}

// Flash has an independent last-call field. Never borrow an older Kimi ACP
// receipt to fill a missing HTTP result; caller checks the transport contract.
pub(super) fn bound_foreground_provider_diagnostics(round: &Value, metrics: &Value) -> Value {
    let request_id = round["submit"]["request_id"]
        .as_str()
        .filter(|value| !value.is_empty());
    let generation = round["submit"]["generation"]
        .as_u64()
        .filter(|value| *value > 0);
    let last = &metrics["last_foreground_segments"];
    let matched = request_id.is_some()
        && generation.is_some()
        && last["request_id"].as_str() == request_id
        && last["generation"].as_u64() == generation;
    json!({"request_id":request_id,"generation":generation,"matched":matched,
        "last_foreground_segments":if matched { last.clone() } else { Value::Null }})
}
pub(super) fn projection(status: &Value, owner: &Value, at_ms: f64, activity: &str) -> Value {
    let mut row = json!({"observed_ms":at_ms,"activity":activity,
        "accepted_processed_frames":owner["accepted_processed_frames"]});
    for key in [
        "state",
        "camera_session_id",
        "frame_id",
        "frame_age_ms",
        "width",
        "height",
        "captured_at_ms",
        "fps",
        "capture_fps",
        "inference_ms",
        "processing_ms",
        "read_sample_ms",
        "decode_ms",
    ] {
        row[key] = status[key].clone();
    }
    // Copy only known numeric timings, never tracks, native pose or diagnostics.
    for key in [
        "preprocess",
        "ort_run",
        "output_decode",
        "nms",
        "tracking",
        "association_metadata",
        "jpeg_encode",
        "preview_prepare",
        "latest_slot_wait",
        "capture_to_process",
    ] {
        if let Some(value) = status["timings_ms"][key].as_f64() {
            if row["timings_ms"].is_null() {
                row["timings_ms"] = json!({});
            }
            row["timings_ms"][key] = json!(value);
        }
    }
    row
}

pub(super) async fn sample_camera(
    app: Arc<Workstation>,
    epoch: String,
    start: Instant,
    path: PathBuf,
    samples: Arc<Mutex<Vec<Value>>>,
    stopped: watch::Receiver<bool>,
) -> Result<()> {
    sample_camera_with_transition(app, epoch, start, path, samples, stopped, None).await
}

// Only the new diagnostic's bounded language switch may pause its voice lease.
// Camera epoch/freshness/coverage checks remain unconditional.
pub(super) async fn sample_camera_with_transition(
    app: Arc<Workstation>,
    epoch: String,
    start: Instant,
    path: PathBuf,
    samples: Arc<Mutex<Vec<Value>>>,
    mut stopped: watch::Receiver<bool>,
    transition: Option<Arc<std::sync::atomic::AtomicU64>>,
) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    if let Some(origin) = samples.lock().unwrap().first() {
        file.write_all(&serde_json::to_vec(origin)?)?;
        file.write_all(b"\n")?;
        file.flush()?;
    }
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased; _=stopped.changed()=>break, _=tick.tick()=>{} }
        if *stopped.borrow() {
            break;
        }
        let status = app.camera.status();
        let owner = app.camera.diagnostic_owner_status();
        let stamp_before = transition
            .as_ref()
            .map_or(0, |stamp| stamp.load(std::sync::atomic::Ordering::SeqCst));
        let phase = app.voice_loop_status();
        let stamp_after = transition
            .as_ref()
            .map_or(0, |stamp| stamp.load(std::sync::atomic::Ordering::SeqCst));
        // Capture with the phase, not after potentially slow evidence I/O.
        // A complete transition crossing this snapshot still has a new stamp.
        let planned_pause =
            stamp_before % 2 == 1 || stamp_after % 2 == 1 || stamp_before != stamp_after;
        let audio = app.audio.status();
        let activity = format!("{}:{}", s(&phase, "phase"), s(&audio, "state"));
        let mut row = projection(&status, &owner, elapsed(start), &activity);
        row["planned_voice_transition"] = json!(planned_pause);
        row["voice_transition_stamps"] = json!([stamp_before, stamp_after]);
        file.write_all(&serde_json::to_vec(&row)?)?;
        file.write_all(b"\n")?;
        file.flush()?;
        let previous_ms = {
            let mut rows = samples.lock().unwrap();
            let previous = rows
                .last()
                .and_then(|v| v["observed_ms"].as_f64())
                .unwrap_or(0.0);
            let current = row["observed_ms"].as_f64().unwrap();
            rows.push(row);
            (previous, current)
        };
        if *stopped.borrow() {
            break;
        }
        ensure!(
            sample_gap_valid(previous_ms.0, previous_ms.1),
            "camera observation gap exceeded3000ms; coverage lost"
        );
        ensure!(
            status["state"] == "running"
                && status["camera_session_id"] == epoch
                && status["frame_age_ms"]
                    .as_f64()
                    .is_some_and(|age| age <= 2000.0)
                && owner["child_started_count"] == 1
                && status["error"].is_null(),
            "camera freshness, epoch or owner changed during active soak"
        );
        ensure!(
            phase["phase"] != "paused" || planned_pause,
            "voice lease revoked during active soak"
        );
    }
    Ok(())
}

fn windows(rounds: &[Value], samples: &[Value], duration_ms: f64) -> Vec<Value> {
    (0..6).map(|index| {
        let begin=index as f64*300_000.0;let end=begin+300_000.0;
        let mut phases:BTreeMap<&str,f64>=BTreeMap::new();
        let mut starts=0;let mut finishes=0;let mut finals=0;let mut submits=0;
        for r in rounds {
            let base=r["soak_started_ms"].as_f64().unwrap_or(0.0);
            let offsets=&r["observed_offsets_ms"];
            let in_window=|value:f64|value>=begin&&value<end;
            starts+=usize::from(in_window(base));
            if r["completed"]==true {finishes+=usize::from(in_window(base+r["elapsed_ms"].as_f64().unwrap_or(0.0)));}
            if let Some(at)=offsets["asr_final"].as_f64(){ finals+=usize::from(in_window(base+at)); }
            if let Some(at)=offsets["submit_accepted"].as_f64(){ submits+=usize::from(in_window(base+at)); }
            for (name,from,to) in [("capture","round_start","endpoint_closed"),("finalize","endpoint_closed","asr_final"),
                ("submit_wait","asr_final","submit_begin"),("teaching_playback","submit_begin","speech_complete"),("rearm","speech_complete","rearm_complete")] {
                if let (Some(a),Some(b))=(offsets[from].as_f64(),offsets[to].as_f64()) {
                    *phases.entry(name).or_default()+=(end.min(base+b)-begin.max(base+a)).max(0.0);
                }
            }
        }
        let rows:Vec<_>=samples.iter().filter(|r|r["observed_ms"].as_f64().is_some_and(|at|at>=begin&&at<end)).collect();
        let mut activity:BTreeMap<&str,usize>=BTreeMap::new();
        for r in &rows {*activity.entry(s(r,"activity")).or_default()+=1;}
        let counter_at=|at:f64|samples.iter().rev().find(|r|r["observed_ms"].as_f64().is_some_and(|v|v<=at))
            .and_then(|r|r["accepted_processed_frames"].as_u64());
        let delta=match(counter_at(begin),counter_at(end)){(Some(a),Some(b))=>b.saturating_sub(a),_=>0};
        json!({"index":index+1,"start_ms":begin,"end_ms":end,"covered_ms":(duration_ms.min(end)-begin).clamp(0.0,300_000.0),
            "rounds_started":starts,"rounds_completed":finishes,"asr_finals":finals,"teaching_submissions":submits,
            "accepted_processed_frames_delta":delta,"sampled_observations":rows.len(),"sampled_activity_counts":activity,"phase_elapsed_ms":phases})
    }).collect()
}

fn progress(path: &Path, report: &Value, samples: &[Value], elapsed_ms: f64) -> Result<()> {
    let rounds = report["rounds"].as_array().unwrap();
    fs::write(
        path,
        serde_json::to_vec_pretty(&json!({"test_type":"real_synthetic_voice_loop_camera_soak",
        "stage":report["stage"],"elapsed_active_ms":elapsed_ms,"duration_target_ms":ACTIVE_SECONDS*1000,
        "completed_rounds":rounds.iter().filter(|r|r["completed"]==true).count(),"teaching_request_limit":MAX_REQUESTS,
        "last_camera_sample":samples.last(),"five_minute_windows":windows(rounds,samples,elapsed_ms)}))?,
    )?;
    Ok(())
}

pub(super) fn protected_soak(root: &Path) -> Result<Value> {
    let mut value = protected(root)?;
    for name in [
        "runtime/rust_camera_device.txt",
        "runtime/ptz-osc-config.json",
        "runtime/ptz-calibration-rust.json",
        "runtime/memory-policy.json",
        "prompts/tutor_core.md",
        "prompts/tutor-agent.md",
    ] {
        let p = root.join(name);
        value[name] = if p.is_file() {
            json!(hash(&p)?)
        } else {
            Value::Null
        };
    }
    Ok(value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Explicit 1800s same-Workstation camera+real voice loop; real default speaker output; synthetic WAV, no microphone/PTZ"]
async fn real_synthetic_voice_loop_camera_soak() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_REAL_VOICE_LOOP_SOAK").as_deref() == Ok("1"),
        "explicit soak opt-in required"
    );
    ensure!(
        std::env::var("AI_TUTOR_VOICE_CHAIN_TTS_ENGINE").as_deref() == Ok("qwen"),
        "Qwen required"
    );
    ensure!(
        std::env::var("AI_TUTOR_VOICE_CHAIN_LANGUAGE").unwrap_or_else(|_| "zh".into()) == "zh",
        "fixed zh only"
    );
    let run = std::env::var("AI_TUTOR_VOICE_CHAIN_RUN")?;
    ensure!(
        !run.is_empty()
            && run.len() <= 80
            && run
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid fresh run name"
    );
    let camera_id = std::env::var("AI_TUTOR_SOAK_CAMERA_ID")?;
    ensure!(
        !camera_id.trim().is_empty(),
        "explicit camera ID required; no default selection"
    );
    let worker = PathBuf::from(std::env::var("AI_TUTOR_SOAK_CAMERA_WORKER")?);
    let worker_sha = std::env::var("AI_TUTOR_SOAK_CAMERA_WORKER_SHA256")?;
    ensure!(
        worker.is_absolute() && hash(&worker)? == worker_sha,
        "camera worker path/hash mismatch"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let out = root.join("runtime/goal-baselines/20261104").join(run);
    fs::create_dir(&out)?;
    let (source_id, source_sha, reference) = fixed_source("zh")?;
    let mut report = json!({"test_type":"real_synthetic_voice_loop_camera_soak","passed":false,"language":"zh",
        "production_capture_loop":true,"synthetic_capture_source":true,"production_audio_owner":true,
        "simulated_display_ack":true,"simulated_client_heartbeat":true,"microphone_opened":false,"camera_opened":false,
        "metadata_only":true,"ptz_enabled":false,"raw_pcm_saved":false,"jpeg_saved":false,"faces_or_tracks_saved":false,
        "speaker_output_measured":false,"system_volume_changed":false,"ambient_audio_used":false,"browser_tested":false,
        "acoustic_loop_tested":false,"production_policy_or_descriptor_written":false,"tts_service_lifecycle_owner":"outer_runner",
        "asr_service_lifecycle_owner":"diagnostic_owned_instance","same_main_managed_joint_lifecycle_tested":false,
        "same_workstation_joint_load":true,"memory_background":"not_exercised","automatic_retry":false,
        "duration_target_ms":ACTIVE_SECONDS*1000,"post_deadline_drain_limit_ms":DRAIN_SECONDS*1000,
        "teaching_request_limit":MAX_REQUESTS,"budget_exhausted_before_duration":false,"completed_rounds":0,
        "source_id":source_id,"source_sha256":source_sha,"reference":reference,"rounds":[],"stage":"preflight"});
    let overall = Instant::now();
    let mut active_start = None;
    let mut owner = None;
    let mut app_holder = None;
    let mut source_holder = None;
    let mut before = None;
    let mut tag = Value::Null;
    let mut sid = String::new();
    let samples = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (stop_tx, stop_rx) = watch::channel(false);
    let (failure_tx, mut failure_rx) = watch::channel(None::<&'static str>);
    let mut heartbeat_task = None;
    let mut camera_task = None;
    let operation:Result<()>=async {
        before=Some(protected_soak(&root)?);
        let wav=root.join(CORPUS).join(format!("{source_id}.wav"));ensure!(hash(&wav)?==source_sha,"fixed WAV changed");
        let corpus:Value=serde_json::from_slice(&fs::read(root.join(CORPUS).join("manifest.json"))?)?;
        ensure!(corpus["audible_output"]==false&&corpus["samples"].as_array().is_some_and(|rows|rows.iter().any(|r|
            r["id"]==source_id&&r["sha256"]==source_sha&&r["source_type"]=="synthetic_sapi_adult_voice")),"fixed synthetic provenance changed");
        let(fixture,asr_assets)=stage_asr(&root,&out)?;let mut camera_assets=Vec::new();
        for (name,expected) in CAMERA_ASSETS {
            let origin=root.join(name);ensure!(hash(&origin)?==expected,"camera asset changed");
            let target=fixture.join(name);fs::create_dir_all(target.parent().unwrap())?;fs::copy(origin,&target)?;
            ensure!(hash(&target)?==expected,"camera fixture copy mismatch");camera_assets.push(json!({"path":name,"sha256":expected}));
        }
        fs::create_dir_all(fixture.join("runtime"))?;
        write_new(&fixture.join("runtime/diagnostic-camera-worker.json"),&json!({"protocol_version":1,"executable":worker,"sha256":worker_sha}))?;
        write_new(&fixture.join("runtime/tutor-delivery.json"),&json!({"protocol_version":1,"mode":"segments","segment_speech":"after_display"}))?;
        let mut app=Workstation::new(fixture.clone(),"synthetic-real-voice-camera-soak".into())?;
        Arc::get_mut(&mut app).ok_or_else(||anyhow!("unexpected shared fixture"))?.provider=Arc::new(KimiProvider::new(&root)?);
        app_holder=Some(app.clone());
        let mut hashes=serde_json::Map::new();
        for file in ["audio.rs","audio/input_devices.rs","audio/voice_loop_source.rs","audio/voice_endpoint.rs","audio/capture_endpoint.rs",
            "voice_loop_model_diagnostic.rs","voice_loop_soak_diagnostic.rs","voice_loop.rs","recording.rs","segment_speech.rs","store/tutor_speech.rs","vision.rs","vision_test_worker.rs",
            "context.rs","tutor.rs","tutor/citation.rs","tutor_segments.rs","tutor_delivery.rs","kimi.rs","kimi_acp_common.rs","kimi_acp_stream.rs","store.rs","server.rs"] {
            hashes.insert(file.into(),json!(hash(&root.join("rust/ai-tutor/src").join(file))?));
        }
        write_new(&out.join("method.json"),&json!({"version":1,"diagnostics_revision":2,"source_files":hashes,"asr_assets":asr_assets,"camera_assets":camera_assets,
            "source_hash_scope":"Current source files and runtime prompts; actual test/worker hashes identify binaries. Source-to-build provenance requires the separately archived build manifest.",
            "provider_diagnostics":"Each round retains only exact request_id/generation last_acp_segments; no match means unobserved. Before/after-cleanup snapshots and the final round after-cleanup binding are separate observations, never retroactive replacements.",
            "camera_worker_sha256":worker_sha,"test_executable_sha256":hash(&std::env::current_exe()?)?,"wav_sha256":source_sha,
            "tutor_policy_sha256":hash(&root.join("prompts/tutor_core.md"))?,"language":"zh","duration_seconds":ACTIVE_SECONDS,
            "tutor_agent_sha256":hash(&root.join("prompts/tutor-agent.md"))?,
            "duration_origin":"all services ready, first fresh camera processed frame, and production voice-loop armed",
            "request_limit":MAX_REQUESTS,"post_deadline_round_drain_seconds":DRAIN_SECONDS,"memory_background":"not_exercised",
            "camera_sample_interval_ms":1000,"maximum_camera_observation_gap_ms":MAX_SAMPLE_GAP_MS,
            "observation_gap_scope":"coverage between adjacent metadata observations and final observation to operation end; not a200ms latency target",
            "camera":"same Workstation native worker; 1Hz scalar metadata only; accepted_processed_frames counts actual accepted worker results, not inferred FPS",
            "capture":"fixed adult synthetic WAV plus declared4000ms quiet tail; paced20ms into real capture_loop/Sonora; no physical microphone",
            "output":"real Qwen and CPAL owner receipts; callback/predicted drain, no acoustic measurement",
            "application":"normal production commit in isolated simulator DB; not shadow-only",
            "sampling":"observed phase counts are not precise utilization; per-round stage intervals are consumer observations, not pure model compute"}))?;
        report["method_sha256"]=json!(hash(&out.join("method.json"))?);
        let audit=app.provider.probe().await?;ensure!(audit["available"]==true&&audit["verified"]==true&&audit["test_transport"].is_null(),"real verified Kimi required");
        let tts=Arc::new(lab_tts(&root,"qwen")?);tts.check_ready().await?;
        let installed=app.audio.configure_tts(Some(tts),app.audio.speech_epoch()).await?;
        ensure!(installed.applied&&installed.engine_kind==Some("qwen_pcm"),"Qwen client not installed");report["tts_install_receipt"]=json!(installed);
        report["stage"]=json!("prepare_asr");
        owner=Some(OwnedAsrService::spawn(&fixture,&LocalAsrPolicy{protocol_version:1,mode:AsrMode::Managed,
            python_executable:Some(root.join("runtime/speech-lab/.venv/Scripts/python.exe"))})?);
        let(_cancel,mut cancelled)=watch::channel(false);let client=owner.as_mut().unwrap().prepare(&mut cancelled,|_|{}).await?;
        app.audio.install_streaming_asr(client);
        report["stage"]=json!("prepare_camera");
        // Some(exact ID) is required; CameraService itself verifies discovery membership.
        app.camera.open(Some(camera_id)).map_err(|_|anyhow!("explicit camera could not open"))?;
        report["camera_opened"]=json!(true);
        let ready_deadline=Instant::now()+Duration::from_secs(30);
        let epoch=loop {
            let c=app.camera.status();
            if c["state"]=="running"&&c["frame_age_ms"].as_f64().is_some_and(|v|v<=2000.0)
                &&c["frame_id"].as_u64().is_some_and(|v|v>0) {break s(&c,"camera_session_id").to_owned();}
            ensure!(Instant::now()<ready_deadline&&c["state"]!="error","camera readiness failed");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let(input,source)=app.audio.install_voice_loop_wav(&wav)?;source_holder=Some(source.clone());
        sid={let stores=app.stores.lock().unwrap();let store=&stores["simulator"];
            let learner=store.create_learner("SYNTHETIC CAMERA VOICE SOAK","zh","guided")?;
            s(&store.create_session(s(&learner,"learner_id"))?,"session_id").to_owned()};
        *app.current.lock().unwrap()=Some(("simulator".into(),sid.clone()));report["session_id"]=json!(sid);
        let enabled=call(&app,Method::POST,&format!("/api/sessions/{sid}/voice-loop/enable"),json!({
            "client_loop_id":"real-fixed-wav-camera-soak","expected_generation":generation(&app,&sid)?,"language":"zh",
            "device_key":input.device_key,"expected_input_topology_revision":input.topology_revision})).await?;
        ensure!(enabled["phase"]=="armed"&&source.status()["opens"]==0,"enable unexpectedly opened capture");
        tag=json!({"client_loop_id":enabled["client_loop_id"],"loop_epoch":enabled["loop_epoch"]});report["enabled"]=enabled;
        let start=Instant::now();active_start=Some(start);
        // An origin sample makes each first window counter delta well-defined.
        samples.lock().unwrap().push(projection(&app.camera.status(),&app.camera.diagnostic_owner_status(),0.0,"armed:idle"));
        let h_app=app.clone();let h_sid=sid.clone();let h_tag=tag.clone();let mut h_stop=stop_rx.clone();let h_fail=failure_tx.clone();
        heartbeat_task=Some(tokio::spawn(async move {
            let result:Result<()>=async {loop {tokio::select! {biased;_=h_stop.changed()=>break,
                _=tokio::time::sleep(Duration::from_millis(400))=>{
                    let status=heartbeat(&h_app,&h_sid,&h_tag).await?;
                    if *h_stop.borrow() { break; }
                    ensure!(status["phase"]!="paused","heartbeat lost live lease");
                }}}Ok(())}.await;
            if result.is_err(){let _=h_fail.send_replace(Some("heartbeat_failed"));}result
        }));
        let c_app=app.clone();let c_samples=samples.clone();let c_stop=stop_rx.clone();let c_fail=failure_tx.clone();let path=out.join("camera-samples.jsonl");
        camera_task=Some(tokio::spawn(async move {
            let result=sample_camera(c_app,epoch,start,path,c_samples,c_stop).await;
            if result.is_err(){let _=c_fail.send_replace(Some("camera_sampling_failed"));}result
        }));
        let active_deadline=start+Duration::from_secs(ACTIVE_SECONDS);
        let drain_deadline=tokio::time::Instant::from_std(active_deadline+Duration::from_secs(DRAIN_SECONDS));
        for index in 1..=MAX_REQUESTS {
            if Instant::now()>=active_deadline { break; }
            ensure!(failure_rx.borrow().is_none(),"background observation failed");
            report["stage"]=json!(format!("round_{index}"));
            progress(&out.join("progress.json"),&report,&samples.lock().unwrap(),elapsed(start))?;
            if Instant::now()>=active_deadline { break; }
            let round_start=elapsed(start);let mut detail=Value::Null;
            let result:Result<()>=tokio::select! {biased;
                _=failure_rx.changed()=>Err(anyhow!("background observation failed")),
                value=tokio::time::timeout_at(drain_deadline,round(&app,&sid,&tag,&input,&source,index,reference,&mut detail))=>
                    value.unwrap_or_else(|_|Err(anyhow!("whole in-flight round exceeded final180s drain budget")))
            };
            detail["soak_started_ms"]=json!(round_start);detail["soak_finished_ms"]=json!(elapsed(start));
            detail["provider_diagnostics"]=bound_provider_diagnostics(&detail,&app.provider.status()["metrics"]);
            write_new(&out.join(format!("round-{index:03}.json")),&detail)?;
            report["rounds"].as_array_mut().unwrap().push(detail);
            report["completed_rounds"]=json!(report["rounds"].as_array().unwrap().iter().filter(|r|r["completed"]==true).count());
            progress(&out.join("progress.json"),&report,&samples.lock().unwrap(),elapsed(start))?;
            result?;
        }
        let duration=elapsed(start);report["active_duration_ms"]=json!(duration);
        report["budget_exhausted_before_duration"]=json!(Instant::now()<active_deadline);
        ensure!(Instant::now()>=active_deadline,"request budget exhausted before1800s; no idle padding");
        let completed=report["completed_rounds"].as_u64().unwrap();let metrics=app.provider.status()["metrics"].clone();
        ensure!(completed>1&&completed<=MAX_REQUESTS as u64&&metrics["requests"]==completed&&metrics["acp_segment_requests"]==completed&&metrics["retries"]==0,"unexpected teaching budget/count");
        ensure!(source.status()["opens"]==completed&&source.status()["closes"]==completed,"source ownership count mismatch");
        ensure!(failure_rx.borrow().is_none(),"background observation failed at completion");
        report["stage"]=json!("completed");Ok(())
    }.await;
    // Freeze the active end before any cleanup; failure reports must not count
    // shutdown time as continued inference/camera coverage.
    report["active_duration_ms"] = json!(active_start.map_or(0.0, elapsed));
    report["diagnostics_revision"] = json!(2);
    report["provider_metrics_before_cleanup"] = app_holder
        .as_ref()
        .map_or(Value::Null, |app| app.provider.status()["metrics"].clone());
    report["provider_metrics_after_cleanup"] = Value::Null;
    report["provider_metrics"] = Value::Null;
    report["last_round_provider_diagnostics_after_cleanup"] = Value::Null;
    // Immediate production invalidation precedes waiting for observers/models.
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
    let provider_clean = if let Some(app) = &app_holder {
        report["explicit_stop"] = app.voice_loop_status();
        report["owned_camera_stop"] = app.camera.close_diagnostic().await;
        app.close().await;
        let close_ok = app.provider.close().await.is_ok();
        let status = app.provider.status();
        report["provider_metrics_after_cleanup"] = status["metrics"].clone();
        // Keep the existing aggregate field for count consumers, now explicitly
        // the after-cleanup snapshot. Do not overwrite any round's earlier view.
        report["provider_metrics"] = status["metrics"].clone();
        report["last_round_provider_diagnostics_after_cleanup"] = report["rounds"]
            .as_array()
            .and_then(|rounds| rounds.last())
            .map_or(Value::Null, |round| {
                bound_provider_diagnostics(round, &status["metrics"])
            });
        close_ok && status["running"] == 0
    } else {
        true
    };
    report["source_lifecycle"] = source_holder
        .as_ref()
        .map_or(Value::Null, |source| source.status());
    report["provider_cleanup_confirmed"] = json!(provider_clean);
    report["owned_asr_stop"] = match owner.as_mut() {
        Some(owner) => owner.stop().await,
        None => json!({"not_started":true,"cleanup_confirmed":true}),
    };
    let after = protected_soak(&root);
    let unchanged = matches!((&before,&after),(Some(a),Ok(b))if a==b);
    report["protected_before_sha256"] = before.unwrap_or(Value::Null);
    report["protected_after_sha256"] = after.unwrap_or(Value::Null);
    report["protected_files_unchanged"] = json!(unchanged);
    let samples = samples.lock().unwrap();
    let active_end = report["active_duration_ms"].as_f64().unwrap_or(0.0);
    // A sample that races with the stop signal after operation end cannot fill
    // an unobserved tail of the active interval.
    let active_samples: Vec<_> = samples
        .iter()
        .filter(|v| v["observed_ms"].as_f64().is_some_and(|at| at <= active_end))
        .collect();
    let maximum_gap = active_samples
        .windows(2)
        .map(|v| {
            v[1]["observed_ms"].as_f64().unwrap_or(0.0)
                - v[0]["observed_ms"].as_f64().unwrap_or(0.0)
        })
        .fold(0.0, f64::max);
    let last_active = active_samples
        .last()
        .and_then(|v| v["observed_ms"].as_f64());
    let tail_gap = last_active.map(|at| active_end - at);
    let coverage_ok = !active_samples.is_empty()
        && active_samples.windows(2).all(|rows| {
            sample_gap_valid(
                rows[0]["observed_ms"].as_f64().unwrap(),
                rows[1]["observed_ms"].as_f64().unwrap(),
            )
        })
        && last_active.is_some_and(|at| sample_gap_valid(at, active_end));
    report["camera"] = json!({"metadata_only":true,"samples_file":"camera-samples.jsonl","samples_count":samples.len(),
        "accepted_processed_frames":samples.last().map(|s|&s["accepted_processed_frames"]),"epoch_changes":samples.windows(2).filter(|s|s[0]["camera_session_id"]!=s[1]["camera_session_id"]).count(),
        "maximum_sample_gap_ms":maximum_gap,"tail_sample_gap_ms":tail_gap,"maximum_allowed_sample_gap_ms":MAX_SAMPLE_GAP_MS,
        "observation_coverage_complete":coverage_ok,"samples_after_active_end":samples.len()-active_samples.len(),
        "first":samples.first(),"last":samples.last(),"last_active":active_samples.last()});
    report["five_minute_windows"] = json!(windows(
        report["rounds"].as_array().unwrap(),
        &samples,
        report["active_duration_ms"].as_f64().unwrap_or(0.0)
    ));
    let windows_ok = report["five_minute_windows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|w| {
            w["covered_ms"] == 300000.0
                && w["rounds_completed"].as_u64().unwrap_or(0) > 0
                && w["accepted_processed_frames_delta"].as_u64().unwrap_or(0) > 0
        });
    let passed = operation.is_ok()
        && heartbeat_clean
        && camera_sampler_clean
        && coverage_ok
        && provider_clean
        && unchanged
        && windows_ok
        && report["owned_asr_stop"]["cleanup_confirmed"] == true
        && report["owned_camera_stop"]["cleanup_confirmed"] == true;
    report["passed"] = json!(passed);
    report["total_ms"] = json!(elapsed(overall));
    report["error"] = if operation.is_err() {
        json!("stage_failed; raw model/service/identity details omitted")
    } else if !passed {
        json!("cleanup_or_continuity_failed")
    } else {
        Value::Null
    };
    progress(
        &out.join("progress.json"),
        &report,
        &samples,
        report["active_duration_ms"].as_f64().unwrap_or(0.0),
    )?;
    write_new(&out.join("report.json"), &report)?;
    println!(
        "Real camera voice-loop soak report: {}",
        out.join("report.json").display()
    );
    operation?;
    ensure!(passed, "cleanup/continuous-window/protected checks failed");
    Ok(())
}

#[test]
fn metadata_projection_never_copies_pixels_tracks_or_pose() {
    let p = projection(
        &json!({"state":"running","frame_id":3,"tracks":[{"face":"private"}],"jpeg":"private",
        "native_pose":{"angles":"private"},"diagnostics":["private"],"timings_ms":{"ort_run":4.0,"private":"private"}}),
        &json!({"accepted_processed_frames":2}),
        10.0,
        "reply:speaking",
    );
    assert_eq!(p["frame_id"], 3);
    assert_eq!(p["timings_ms"]["ort_run"], 4.0);
    assert!(!p.to_string().contains("private"));
}

#[test]
fn five_minute_statistics_clip_crossing_rounds_without_inventing_completed_work() {
    let rounds = vec![
        json!({"soak_started_ms":290000.0,"elapsed_ms":30000.0,"completed":true,
        "observed_offsets_ms":{"round_start":0.0,"endpoint_closed":5000.0,"asr_final":5100.0,"submit_begin":5200.0,"submit_accepted":5300.0,"speech_complete":29500.0,"rearm_complete":30000.0}}),
    ];
    let samples = vec![
        json!({"observed_ms":0.0,"accepted_processed_frames":10,"activity":"armed:idle"}),
        json!({"observed_ms":299000.0,"accepted_processed_frames":1010,"activity":"reply:speaking"}),
        json!({"observed_ms":320000.0,"accepted_processed_frames":1100,"activity":"armed:idle"}),
    ];
    let w = windows(&rounds, &samples, 320000.0);
    assert_eq!(w[0]["rounds_started"], 1);
    assert_eq!(w[0]["rounds_completed"], 0);
    assert_eq!(w[1]["rounds_started"], 0);
    assert_eq!(w[1]["rounds_completed"], 1);
    assert_eq!(w[0]["phase_elapsed_ms"]["teaching_playback"], 4800.0);
    assert_eq!(w[1]["phase_elapsed_ms"]["teaching_playback"], 19500.0);
    assert_eq!(w[1]["covered_ms"], 20000.0);
    assert_eq!(w[2]["covered_ms"], 0.0);
}

#[test]
fn observation_coverage_accepts_boundary_but_rejects_internal_or_tail_gaps() {
    assert!(sample_gap_valid(0.0, 3000.0));
    assert!(sample_gap_valid(3000.0, 6000.0));
    assert!(!sample_gap_valid(3000.0, 6000.001));
    let observations = [0.0, 1000.0, 5001.0, 6000.0];
    assert!(
        !observations
            .windows(2)
            .all(|r| sample_gap_valid(r[0], r[1]))
    );
    // Later recovery does not erase the internal gap, and an unobserved tail
    // is measured from the last active observation, not a post-stop sample.
    assert!(sample_gap_valid(6000.0, 9000.0));
    assert!(!sample_gap_valid(6000.0, 9000.001));
    assert!(!sample_gap_valid(6000.0, 5999.0));
    assert!(!sample_gap_valid(0.0, f64::NAN));
}

#[test]
fn provider_diagnostics_require_both_accepted_request_and_generation() {
    let round = json!({"submit":{"request_id":"current","generation":8}});
    for last in [
        json!({"request_id":"previous","generation":8}),
        json!({"request_id":"current","generation":7}),
        json!({"request_id":"current","generation":"8"}),
        Value::Null,
    ] {
        let bound = bound_provider_diagnostics(&round, &json!({"last_acp_segments":last}));
        assert_eq!(bound["request_id"], "current");
        assert_eq!(bound["generation"], 8);
        assert_eq!(bound["matched"], false);
        assert!(bound["last_acp_segments"].is_null());
    }
    // A pre-submit failure has no accepted request; missing values cannot match.
    let bound = bound_provider_diagnostics(&json!({"stage":"finalize"}), &json!({}));
    assert_eq!(bound["matched"], false);
    assert!(bound["request_id"].is_null() && bound["last_acp_segments"].is_null());
}

#[test]
fn flash_diagnostics_never_borrow_an_acp_receipt_or_a_previous_http_request() {
    let round = json!({"submit":{"request_id":"current","generation":8}});
    let current = json!({"request_id":"current","generation":8,
        "transport":"deepseek_http","stream_contract":"metadata_v2","success":true});
    for last in [
        json!({"request_id":"previous","generation":8}),
        json!({"request_id":"current","generation":7}),
        Value::Null,
    ] {
        let bound = bound_foreground_provider_diagnostics(
            &round,
            &json!({"last_acp_segments":current,"last_foreground_segments":last}),
        );
        assert_eq!(bound["matched"], false);
        assert!(bound["last_foreground_segments"].is_null());
        assert!(bound.get("last_acp_segments").is_none());
    }
    let bound =
        bound_foreground_provider_diagnostics(&round, &json!({"last_foreground_segments":current}));
    assert_eq!(bound["matched"], true);
    assert_eq!(bound["last_foreground_segments"], current);
}

#[test]
fn late_cleanup_diagnostics_do_not_rewrite_the_round_end_observation() {
    let round = json!({"submit":{"request_id":"current","generation":8}});
    let before =
        json!({"last_acp_segments":{"request_id":"previous","generation":6,"success":true}});
    let at_round_end = bound_provider_diagnostics(&round, &before);
    let after = json!({"last_acp_segments":{"request_id":"current","generation":8,
        "success":false,"operation_failure_code":"final_schema_invalid",
        "citation_diagnostic":{"field":"used_evidence_ids","invalid_reference_count":1},
        "cleanup_failure_code":"acp_cleanup_failed","process_reaped":false}});
    let after_cleanup = bound_provider_diagnostics(&round, &after);
    assert_eq!(at_round_end["matched"], false);
    assert!(at_round_end["last_acp_segments"].is_null());
    assert_eq!(after_cleanup["matched"], true);
    assert_eq!(
        after_cleanup["last_acp_segments"],
        after["last_acp_segments"]
    );
    assert_eq!(before["last_acp_segments"]["request_id"], "previous");
    assert_eq!(
        after_cleanup["last_acp_segments"]["citation_diagnostic"]["invalid_reference_count"],
        1
    );
}

//! Explicit synthetic-WAV diagnostic through the production display/speech
//! scheduler and native output owner. Display ACKs are simulated. Only CPAL's
//! actual callback supplies a PCM Started receipt; no acoustic timing is claimed.
use super::*;
use crate::{
    audio::voice_model_chain_diagnostic::{
        CORPUS, fixed_source, hash, lab_tts, protected, stage_asr, transcribe, write_new,
    },
    local_asr::{AsrMode, LocalAsrPolicy, OwnedAsrService},
};
use std::fs;

fn fixture_state(app: &Workstation, sid: &str) -> Result<Value> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let session = store.session(sid)?;
    Ok(json!({"session":session,"memories":store.memories(s(&session,"learner_id"))?}))
}

fn observation(app: &Workstation, sid: &str, rid: &str) -> Result<(Value, Vec<Value>)> {
    // All production writes use this same lock. A real Started receipt visible
    // alongside final_validated=false proves callback submission preceded final.
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let snapshot = store.tutor_stream_snapshot(sid, rid)?;
    let mut rows = store.rows(
        "SELECT * FROM tutor_segments WHERE request_id=? ORDER BY sequence",
        [rid],
    )?;
    for row in &mut rows {
        for key in ["speech_started_receipt", "speech_terminal_receipt"] {
            if let Some(raw) = row[key].as_str() {
                row[key] = serde_json::from_str(raw)?;
            }
        }
    }
    Ok((snapshot, rows))
}

async fn bound_close(app: &Arc<Workstation>, sid: &str, submit: &Value) -> Result<Value> {
    dispatch(
        app.clone(),
        &Method::POST,
        &format!(
            "/api/sessions/{sid}/streams/{}/speech/close",
            s(submit, "request_id")
        ),
        &json!({"generation":submit["generation"],"reason":"user"}),
    )
    .await
}

async fn playback_round(
    app: &Arc<Workstation>,
    sid: &str,
    transcript: &str,
    old_submit: Option<&Value>,
    report: &mut Value,
) -> Result<Value> {
    let stop_first = old_submit.is_none();
    let case = if stop_first {
        "stop_then_late_ack"
    } else {
        "restart_complete"
    };
    *report = json!({"case":case,"passed":false,"overlap_observed":false,
        "display_acks":[],"observations":[],"late_ack_count":0,
        "candidate_application_completed":false,"coverage_not_exercised":[],"stage":"submit"});
    report["fixture_state_before"] = fixture_state(app, sid)?;
    // Respect the production debounce and clock. This is another explicit
    // request, not an automatic retry of a failed provider operation.
    while app.last_input.lock().unwrap().elapsed() <= Duration::from_millis(1550) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let generation = app.stores.lock().unwrap()["simulator"].session(sid)?["generation"].clone();
    let started = Instant::now();
    let submit = dispatch(
        app.clone(),
        &Method::POST,
        &format!("/api/sessions/{sid}/turns"),
        &json!({"text":transcript,"expected_generation":generation,"auto_speak":true,
            "client_submission_id":format!("synthetic-production-{case}")}),
    )
    .await?;
    report["submit"] = submit.clone();
    let rid = s(&submit, "request_id");
    let mut acknowledged = 0usize;
    let mut closed = false;
    let mut replayed_old_close = false;
    let mut first_callback = None;
    let mut final_observed = None;
    let mut last_observation = Value::Null;
    report["stage"] = json!("display_and_play");
    loop {
        ensure!(
            started.elapsed() < Duration::from_secs(180),
            "bounded production playback round timed out"
        );
        let (snapshot, rows) = observation(app, sid, rid)?;
        report["final_snapshot"] = snapshot.clone();
        report["segments"] = json!(rows);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if snapshot["final_validated"] == true {
            final_observed.get_or_insert(elapsed);
        }
        let change = json!({"phase":snapshot["phase"],"final_validated":snapshot["final_validated"],
            "shown_through":snapshot["shown_through"],"speech_gate":snapshot["speech_gate"],
            "speech_complete":snapshot["speech_complete"],"segments":rows});
        if change != last_observation {
            report["observations"]
                .as_array_mut()
                .unwrap()
                .push(json!({"observed_ms":elapsed,"state":change}));
            last_observation = change;
        }
        ensure!(
            !matches!(
                s(&snapshot, "phase"),
                "failed" | "cancelled" | "interrupted"
            ),
            "production text stream did not complete"
        );
        for row in &rows {
            let receipt = &row["speech_started_receipt"];
            if !receipt.is_null() {
                ensure!(
                    receipt["engine_kind"] == "qwen_pcm"
                        && receipt["milestone"] == "first_callback_submission",
                    "expected actual Qwen PCM callback; fallback is not a Qwen-path pass"
                );
                first_callback.get_or_insert(elapsed);
                if snapshot["final_validated"] == false {
                    report["overlap_observed"] = json!(true);
                }
            }
        }
        report["first_callback_observed_ms"] = json!(first_callback);
        report["final_observed_ms"] = json!(final_observed);
        if first_callback.is_some() && stop_first && !closed {
            report["stop_observed_first_segment_state"] = rows[0]["speech_state"].clone();
            let receipt = &rows[0]["speech_started_receipt"];
            let before_close = app.audio.status();
            let playing = rows[0]["speech_state"] == "started"
                && before_close["state"] == "speaking"
                && before_close["playback_id"] == receipt["playback_id"]
                && before_close["playback_epoch"] == receipt["epoch"];
            report["stop_during_matching_active_output"] = json!(playing);
            report["audio_before_close"] = before_close;
            let close_started = Instant::now();
            report["close_response"] = bound_close(app, sid, &submit).await?;
            report["close_request_ms"] = json!(close_started.elapsed().as_secs_f64() * 1000.0);
            ensure!(
                report["close_response"]["speech_gate"] == "closed",
                "bound Stop did not close speech gate"
            );
            closed = true;
            // Observe this same native owner retire before any next submit or
            // extra Stop can help. Gate closure is not an owner terminal receipt.
            loop {
                let status = app.audio.status();
                let retired = status["state"] == "idle"
                    && status["playback_id"] == receipt["playback_id"]
                    && status["playback_epoch"] == receipt["epoch"]
                    && status["pcm_playback"].is_object();
                report["audio_after_close"] = status;
                report["native_owner_idle_observed"] = json!(retired);
                report["close_to_native_idle_observed_ms"] =
                    json!(close_started.elapsed().as_secs_f64() * 1000.0);
                if retired {
                    break;
                }
                ensure!(
                    close_started.elapsed() < Duration::from_secs(2),
                    "bound close did not retire its matching native output owner"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Text may already be completed in the old gate=open snapshot.
            // Re-read after close rather than asserting on that stale gate.
            continue;
        }
        if first_callback.is_some() && !stop_first && !replayed_old_close {
            let before_replay = app.audio.status();
            if let Some(active) = rows.iter().find(|row| {
                row["speech_state"] == "started"
                    && before_replay["state"] == "speaking"
                    && before_replay["playback_id"] == row["speech_started_receipt"]["playback_id"]
                    && before_replay["playback_epoch"] == row["speech_started_receipt"]["epoch"]
            }) {
                let epoch = app.audio.speech_epoch();
                let response = bound_close(app, sid, old_submit.unwrap()).await?;
                ensure!(
                    app.audio.speech_epoch() == epoch,
                    "old bound close revoked the new output epoch"
                );
                report["old_close_replay"] = json!({"response":response,"audio_epoch_before":epoch,
                    "audio_epoch_after":app.audio.speech_epoch(),"new_epoch_preserved":true,
                    "matching_active_output_observed":true,"active_sequence":active["sequence"],
                    "audio_before":before_replay,"audio_after":app.audio.status()});
                replayed_old_close = true;
                continue;
            }
        }
        // In the first case only the first displayed segment can start before
        // Stop. All subsequent ACKs exercise the already-closed production gate.
        let limit = if stop_first && !closed {
            rows.len().min(1)
        } else {
            rows.len()
        };
        while acknowledged < limit {
            let ack_started = Instant::now();
            let response = dispatch(
                app.clone(),
                &Method::POST,
                &format!("/api/sessions/{sid}/streams/{rid}/display"),
                &json!({"generation":submit["generation"],"sequence":acknowledged}),
            )
            .await?;
            report["display_acks"]
                .as_array_mut()
                .unwrap()
                .push(json!({"sequence":acknowledged,
                "after_bound_close":closed,"observed_ms":started.elapsed().as_secs_f64()*1000.0,
                "request_ms":ack_started.elapsed().as_secs_f64()*1000.0,"response":response}));
            if closed {
                report["late_ack_count"] = json!(report["late_ack_count"].as_u64().unwrap() + 1);
            }
            acknowledged += 1;
        }
        if snapshot["phase"] == "completed"
            && (stop_first && closed || !stop_first && snapshot["speech_complete"] == true)
        {
            ensure!(
                !rows.is_empty() && acknowledged == rows.len(),
                "display ACK coverage incomplete"
            );
            if stop_first {
                ensure!(
                    snapshot["speech_gate"] == "closed"
                        && rows
                            .iter()
                            .skip(1)
                            .all(|row| row["speech_started_receipt"].is_null()
                                && row["speech_terminal_receipt"].is_null()
                                && row["speech_state"] == "not_started"),
                    "late display ACK started audio after bound close"
                );
                report["late_ack_exercised"] =
                    json!(report["late_ack_count"].as_u64().unwrap() > 0);
                report["no_later_segment_started"] = if report["late_ack_exercised"] == true {
                    json!(true)
                } else {
                    Value::Null
                };
                if report["late_ack_exercised"] != true {
                    report["coverage_not_exercised"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("late_display_ack_after_close"));
                }
                if report["stop_during_matching_active_output"] != true {
                    report["coverage_not_exercised"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("stop_during_matching_active_output"));
                }
            } else {
                if !replayed_old_close {
                    report["coverage_not_exercised"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("old_close_during_matching_active_output"));
                }
                for row in &rows {
                    let start = &row["speech_started_receipt"];
                    let end = &row["speech_terminal_receipt"];
                    ensure!(
                        row["speech_state"] == "completed"
                            && row["speech_end_origin"] == "owner_receipt"
                            && end["state"] == "completed"
                            && end["engine_kind"] == "qwen_pcm"
                            && end["playback_id"] == start["playback_id"]
                            && end["epoch"] == start["epoch"]
                            && end["callback_submitted_frames"]
                                .as_u64()
                                .is_some_and(|n| n > 0)
                            && end["predicted_output_drained"] == true
                            && end["speaker_output_measured"] == false,
                        "completed segment lacks matching real PCM owner receipts"
                    );
                }
            }
            report["candidate_application_completed"] = json!(true);
            report["fixture_state_after"] = fixture_state(app, sid)?;
            report["provider_metrics"] = app.provider.status()["metrics"].clone();
            report["audio_status_after"] = app.audio.status();
            report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            report["stage"] = json!("completed");
            report["passed"] = json!(
                report["coverage_not_exercised"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            return Ok(submit);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[ignore = "Explicit synthetic WAV and real owned models, production dispatch and DEFAULT OUTPUT PLAYBACK; no microphone/camera/PTZ"]
async fn real_synthetic_production_segment_playback() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_REAL_SEGMENT_PLAYBACK").as_deref() == Ok("1")
            && std::env::var("AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC").as_deref() == Ok("1"),
        "explicit synthetic production playback opt-in required"
    );
    let language = std::env::var("AI_TUTOR_VOICE_CHAIN_LANGUAGE")?;
    let engine = std::env::var("AI_TUTOR_VOICE_CHAIN_TTS_ENGINE")?;
    ensure!(
        engine == "qwen",
        "this fixed diagnostic requires the coordinator-owned Qwen service"
    );
    let run = std::env::var("AI_TUTOR_VOICE_CHAIN_RUN")?;
    ensure!(
        !run.is_empty()
            && run.len() <= 80
            && run
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid unique run name"
    );
    let (source_id, source_hash, reference) = fixed_source(&language)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let out = root.join("runtime/goal-baselines/20261104").join(run);
    fs::create_dir(&out)?;
    let mut report = json!({"test_type":"real_synthetic_production_segment_playback","passed":false,
        "language":language,"tts_engine":engine,"source_id":source_id,"source_sha256":source_hash,"reference":reference,
        "production_audio_owner":true,"simulated_display_ack":true,"speaker_output_measured":false,
        "microphone_opened":false,"camera_opened":false,"ptz_enabled":false,"raw_pcm_saved":false,
        "ambient_audio_used":false,"system_volume_changed":false,"input_closed_claimed":false,
        "voice_loop_permission_or_rearm_tested":false,"browser_or_acoustic_quality_tested":false,
        "production_policy_or_descriptor_written":false,"legacy_asr_service_contacted":false,
        "tts_service_lifecycle_owner":"outer_runner","same_main_managed_joint_lifecycle_tested":false,
        "automatic_retry":false,"teaching_request_limit":2,"round_deadline_seconds":180,
        "overlap_observed":false,"asr":{},"rounds":[],"stage":"preflight"});
    let mut owner = None;
    let mut workstation: Option<Arc<Workstation>> = None;
    let mut before = None;
    let mut progress = vec![];
    let started = Instant::now();
    let operation: Result<()> = async {
        before = Some(protected(&root)?);
        report["protected_before_sha256"] = before.clone().unwrap();
        let wav = root.join(CORPUS).join(format!("{source_id}.wav"));
        ensure!(hash(&wav)?==source_hash, "fixed synthetic WAV hash changed");
        let manifest: Value=serde_json::from_slice(&fs::read(root.join(CORPUS).join("manifest.json"))?)?;
        ensure!(manifest["audible_output"]==false && manifest["samples"].as_array().is_some_and(|rows|
            rows.iter().any(|row|row["id"]==source_id && row["sha256"]==source_hash && row["source_type"]=="synthetic_sapi_adult_voice")),
            "frozen synthetic provenance mismatch");
        let (fixture,files)=stage_asr(&root,&out)?;
        fs::create_dir_all(fixture.join("runtime"))?;
        write_new(&fixture.join("runtime/tutor-delivery.json"),
            &json!({"protocol_version":1,"mode":"segments","segment_speech":"after_display"}))?;
        let mut app=Workstation::new(fixture.clone(),"synthetic-production-playback".into())?;
        // This changes only the fixture's provider root before sharing its Arc.
        // The actual audio owner and production coordinator are never replaced.
        Arc::get_mut(&mut app).ok_or_else(||anyhow!("unexpected shared fixture"))?.provider=Arc::new(KimiProvider::new(&root)?);
        workstation=Some(app.clone());
        let mut sources=serde_json::Map::new();
        for name in ["server.rs","segment_playback_diagnostic.rs","tutor_delivery.rs","segment_speech.rs",
            "speech_delivery.rs","audio.rs","audio/speech_output.rs","audio/pcm_output.rs",
            "audio/streaming_tts.rs","audio/voice_model_chain_diagnostic.rs","store/tutor_speech.rs"] {
            sources.insert(name.into(),json!(hash(&root.join("rust/ai-tutor/src").join(name))?));
        }
        write_new(&out.join("method.json"),&json!({"version":1,"files":files,"sources":sources,
            "test_executable_sha256":hash(&std::env::current_exe()?)?,"source_sha256":source_hash,
            "asr":"private owned instance, paced fixed synthetic WAV, no input owner/device",
            "teaching":"two actual production dispatch requests with unchanged real ACP validation",
            "display":"simulated ordered ACKs; first case holds later ACKs until bound close",
            "tts":"outer runner owns ready/warmed Qwen; real production scheduler and native CPAL output",
            "timing":"consumer observations; same-lock Started before final proves overlap, not acoustic onset",
            "candidate_application":"normal production final commit in isolated synthetic simulator database"}))?;
        report["method_sha256"]=json!(hash(&out.join("method.json"))?);
        report["stage"]=json!("verify_teaching_and_tts_ready");
        let audit=app.provider.probe().await?;
        ensure!(audit["available"]==true && audit["verified"]==true && audit["test_transport"].is_null(),"existing real native Kimi verification required");
        report["teaching_audit"]=json!({"available":audit["available"],"verified":audit["verified"],
            "version":audit["version"],"binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"]});
        let tts=Arc::new(lab_tts(&root,&engine)?);
        tts.check_ready().await?;
        let installed=app.audio.configure_tts(Some(tts),app.audio.speech_epoch()).await?;
        report["tts_install_receipt"]=json!(installed);
        ensure!(installed.applied && installed.engine_kind==Some("qwen_pcm"),"Qwen client not installed into real idle owner");
        report["stage"]=json!("prepare_owned_asr");
        let policy=LocalAsrPolicy{protocol_version:1,mode:AsrMode::Managed,
            python_executable:Some(root.join("runtime/speech-lab/.venv/Scripts/python.exe"))};
        owner=Some(OwnedAsrService::spawn(&fixture,&policy)?);
        let (_cancel,mut cancelled)=watch::channel(false);
        let client=owner.as_mut().unwrap().prepare(&mut cancelled,|event|
            progress.push(json!({"state":event["state"],"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}))).await?;
        report["stage"]=json!("transcribe_fixed_wav");
        let transcript=transcribe(client,&wav,&mut report["asr"]).await?;
        write_new(&out.join("asr.json"),&report["asr"])?;
        let sid={
            let stores=app.stores.lock().unwrap();
            let store=&stores["simulator"];
            let learner=store.create_learner("SYNTHETIC REAL PRODUCTION PLAYBACK",&language,"guided")?;
            s(&store.create_session(s(&learner,"learner_id"))?,"session_id").to_owned()
        };
        *app.current.lock().unwrap()=Some(("simulator".into(),sid.clone()));
        let mut previous=None;
        for index in 0..2 {
            report["stage"]=json!(if index==0{"stop_then_late_ack"}else{"restart_complete"});
            let mut round=Value::Null;
            let result=playback_round(&app,&sid,&transcript,previous.as_ref(),&mut round).await;
            write_new(&out.join(format!("round-{index}.json")),&round)?;
            report["overlap_observed"]=json!(report["overlap_observed"]==true||round["overlap_observed"]==true);
            report["rounds"].as_array_mut().unwrap().push(round);
            previous=Some(result?);
        }
        let metrics=app.provider.status()["metrics"].clone();
        ensure!(metrics["requests"]==2 && metrics["acp_segment_requests"]==2,"unexpected teaching invocation count");
        let session=app.stores.lock().unwrap()["simulator"].session(&sid)?;
        ensure!(session["turns"].as_array().unwrap().iter().filter(|turn|turn["role"]=="user").count()==2,
            "expected exactly two explicit synthetic user turns");
        report["stage"]=json!("completed");
        Ok(())
    }.await;
    report["asr_progress"] = json!(progress);
    // Stop playback/provider first even on assertion or model failure. The outer
    // runner independently owns and finally stops its TTS process group.
    let provider_clean = if let Some(app) = &workstation {
        report["provider_metrics"] = app.provider.status()["metrics"].clone();
        app.close().await;
        app.provider.close().await.is_ok() && app.provider.status()["running"] == 0
    } else {
        true
    };
    report["provider_cleanup_confirmed"] = json!(provider_clean);
    let stop = match owner.as_mut() {
        Some(owner) => owner.stop().await,
        None => json!({"not_started":true,"cleanup_confirmed":true}),
    };
    report["owned_asr_stop"] = stop.clone();
    let after = protected(&root);
    let unchanged = matches!((&before,&after),(Some(before),Ok(after))if before==after);
    report["protected_after_sha256"] = after.unwrap_or(Value::Null);
    report["protected_files_unchanged"] = json!(unchanged);
    report["total_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    let coverage_complete = report["rounds"].as_array().is_some_and(|rounds| {
        rounds.len() == 2 && rounds.iter().all(|round| round["passed"] == true)
    });
    report["coverage_complete"] = json!(coverage_complete);
    let passed = operation.is_ok()
        && coverage_complete
        && provider_clean
        && stop["cleanup_confirmed"] == true
        && unchanged;
    report["passed"] = json!(passed);
    report["error"] = if operation.is_err() {
        json!("stage_failed; raw provider/service error omitted")
    } else if !coverage_complete {
        json!("coverage_not_exercised; no automatic retry")
    } else {
        Value::Null
    };
    write_new(&out.join("report.json"), &report)?;
    println!(
        "Production segment playback report: {}",
        out.join("report.json").display()
    );
    operation?;
    ensure!(
        passed,
        "coverage, owned cleanup or protected-file verification failed"
    );
    Ok(())
}

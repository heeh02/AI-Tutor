//! Four explicitly authorized HTTP text calls through ordinary Workstation APIs.
use super::voice_loop_model_diagnostic::{call, generation, observation};
use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

const PLAN: &str = include_str!("../../../scripts/validation/teaching_provider_real_plan_v1.json");
const PLAN_PATH: &str = "scripts/validation/teaching_provider_real_plan_v1.json";
const MAX_CALLS: usize = 4;
const CODE: &[(&str, &[u8])] = &[
    (
        "rust/ai-tutor/src/teaching_provider_real_diagnostic.rs",
        include_bytes!("teaching_provider_real_diagnostic.rs"),
    ),
    ("rust/ai-tutor/src/server.rs", include_bytes!("server.rs")),
    ("rust/ai-tutor/src/kimi.rs", include_bytes!("kimi.rs")),
    (
        "rust/ai-tutor/src/kimi_acp_stream.rs",
        include_bytes!("kimi_acp_stream.rs"),
    ),
    (
        "rust/ai-tutor/src/foreground_provider.rs",
        include_bytes!("foreground_provider.rs"),
    ),
    (
        "rust/ai-tutor/src/deepseek_client.rs",
        include_bytes!("deepseek_client.rs"),
    ),
    (
        "rust/ai-tutor/src/deepseek_flash_sse.rs",
        include_bytes!("deepseek_flash_sse.rs"),
    ),
    (
        "rust/ai-tutor/src/tutor_segments.rs",
        include_bytes!("tutor_segments.rs"),
    ),
    (
        "rust/ai-tutor/src/tutor_delivery.rs",
        include_bytes!("tutor_delivery.rs"),
    ),
    ("rust/ai-tutor/src/tutor.rs", include_bytes!("tutor.rs")),
    ("rust/ai-tutor/src/context.rs", include_bytes!("context.rs")),
    ("rust/ai-tutor/src/store.rs", include_bytes!("store.rs")),
    (
        "rust/ai-tutor/src/store/tutor_delivery.rs",
        include_bytes!("store/tutor_delivery.rs"),
    ),
    (
        "rust/ai-tutor/src/voice_loop_model_diagnostic.rs",
        include_bytes!("voice_loop_model_diagnostic.rs"),
    ),
];
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = [0; 64 * 1024];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn read(path: &Path, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    fs::File::open(path)?
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= cap, "input_limit");
    Ok(bytes)
}
fn write_new(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    Ok(())
}
fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}
fn fixed_plan() -> Result<Value> {
    let plan: Value = serde_json::from_str(PLAN)?;
    ensure!(
        plan["maximum_calls"] == 4
            && plan["automatic_retries"] == 0
            && plan["replacement_calls"] == 0
            && plan["thinking"] == "non_thinking"
            && plan["provider"] == "deepseek_flash"
            && plan["per_slot_deadline_seconds"] == 110
            && plan["operation_deadline_seconds"] == 460,
        "fixed_budget_changed"
    );
    ensure!(
        plan["schedule"]
            == json!([
                {"slot":0,"route":"zh_stream","case_id":"powered_off_ruler_units_zh","language":"zh"},
                {"slot":1,"route":"en_stream","case_id":"motion_disabled_sensor_units_en","language":"en"},
                {"slot":2,"route":"cancel_active_stream","case_id":"motion_disabled_sensor_units_en","language":"en"},
                {"slot":3,"route":"whole_reply","case_id":"powered_off_ruler_units_zh","language":"zh"}
            ]),
        "fixed_schedule_changed"
    );
    Ok(plan)
}
fn checked(root: &Path, relative: &str, expected: &str) -> Result<Vec<u8>> {
    let path = root.join(relative).canonicalize()?;
    ensure!(path.starts_with(root), "source_outside_root");
    let bytes = read(&path, 256 * 1024)?;
    ensure!(digest(&bytes) == expected, "source_hash_mismatch");
    Ok(bytes)
}
fn stage_fixture(root: &Path, fixture: &Path, plan: &Value, whole: bool) -> Result<()> {
    fs::create_dir(fixture)?;
    fs::create_dir(fixture.join("runtime"))?;
    for artifact in plan["artifacts"]
        .as_array()
        .ok_or_else(|| anyhow!("artifacts_missing"))?
    {
        let bytes = checked(root, s(artifact, "source"), s(artifact, "sha256"))?;
        let target = fixture.join(s(artifact, "target"));
        fs::create_dir_all(target.parent().unwrap())?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?
            .write_all(&bytes)?;
    }
    write_new(
        &fixture.join("runtime/teaching-provider-policy.json"),
        &json!({"protocol_version":1,"provider":"deepseek_flash","thinking":"non_thinking"}),
    )?;
    write_new(
        &fixture.join("runtime/tutor-delivery.json"),
        &if whole {
            json!({"protocol_version":1,"mode":"whole_reply"})
        } else {
            json!({"protocol_version":1,"mode":"segments","segment_speech":"disabled"})
        },
    )?;
    write_new(
        &fixture.join("runtime/memory-policy.json"),
        &json!({"mode":"disabled"}),
    )?;
    ensure!(
        !fixture.join("models").exists() && !fixture.join("bin").exists(),
        "unexpected_local_model_assets"
    );
    Ok(())
}
fn devices(app: &Workstation) -> Value {
    json!({"audio":app.audio.status(),"camera":app.camera.status(),"ptz":app.ptz.status(),
        "asr":app.local_asr_status(),"final_asr":app.final_asr_status(),"tts":app.local_speech_status(),
        "memory_schedule_started":app.memory_schedule_started.load(Ordering::SeqCst)})
}
fn text_only(app: &Workstation) -> Result<()> {
    let state = devices(app);
    ensure!(
        state["camera"]["state"] == "closed"
            && state["ptz"]["mode"] == "off"
            && state["ptz"]["commands_sent"] == 0
            && state["ptz"]["command_attempts"] == 0,
        "unexpected_camera_or_ptz_activity"
    );
    let audio = &state["audio"];
    ensure!(
        audio["recording"] != true
            && audio["capture_pending_result"] != true
            && !["recording", "transcribing", "speaking", "synthesizing"]
                .contains(&s(audio, "state"))
            && audio["tts_configured_engine"].is_null(),
        "unexpected_audio_activity"
    );
    for key in ["asr", "final_asr", "tts"] {
        ensure!(
            state[key]["active"] != true && state[key]["busy"] != true,
            "local_service_started"
        );
    }
    ensure!(
        state["memory_schedule_started"] == false,
        "memory_scheduler_started"
    );
    let metrics = app.provider.status()["metrics"].clone();
    ensure!(
        metrics["consolidation_requests"].as_u64().unwrap_or(0) == 0
            && metrics["acp_segment_requests"].as_u64().unwrap_or(0) == 0,
        "unexpected_kimi_inference"
    );
    Ok(())
}
fn effects(app: &Workstation, sid: &str, lid: &str) -> Result<Value> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    Ok(json!({"session":store.session(sid)?,"memories":store.memories(lid)?}))
}
fn source(app: &Workstation, sid: &str, rid: &str, lid: &str, text: &str) -> Result<Value> {
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    let turn = store
        .one(
            "SELECT * FROM turns WHERE session_id=? AND request_id=? AND role='user'",
            [sid, rid],
        )?
        .ok_or_else(|| anyhow!("submitted_turn_missing"))?;
    ensure!(turn["content"] == text, "submitted_text_changed");
    let ids: Vec<String> = serde_json::from_str(s(&turn, "evidence"))?;
    let events = store.source_events(lid, &ids)?;
    ensure!(
        events.len() == 1
            && events[0]["session_id"] == sid
            && events[0]["source_type"] == "simulated"
            && events[0]["event_kind"] == "learner_statement"
            && events[0]["event_version"] == 1
            && events[0]["content"]["text"] == text,
        "submitted_source_mismatch"
    );
    Ok(
        json!({"turn":turn,"events":events,"source_sha256":digest(&serde_json::to_vec(&events)?),
        "text_utf8_sha256":digest(text.as_bytes())}),
    )
}
fn pending_reply(
    app: &Workstation,
    sid: &str,
    rid: &str,
    generation: i64,
) -> Option<(String, Value)> {
    app.pending
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p)| p.sid == sid && p.generation == generation && p.reply["request_id"] == rid)
        .map(|(id, p)| (id.clone(), p.reply.clone()))
}
fn provider_metric(app: &Workstation, rid: &str, generation: i64) -> Value {
    let metric = app.provider.status()["metrics"]["last_foreground_segments"].clone();
    if metric["request_id"] == rid && metric["generation"] == generation {
        metric
    } else {
        Value::Null
    }
}
async fn retired(app: &Workstation, rid: &str, lid: &str) -> Result<()> {
    let began = Instant::now();
    while app.provider.active_foreground_for_diagnostic(rid, lid)
        || app.provider.foreground_status()["running"] != 0
        || app.running.lock().unwrap().contains_key(rid)
    {
        ensure!(
            began.elapsed() < Duration::from_secs(8),
            "owned_request_did_not_retire"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}
fn cancel_proven(row: &Value) -> bool {
    row["active_exact_registration_before_stop"] == true
        && row["persisted_prefix_count"]
            .as_u64()
            .is_some_and(|v| v > 0)
        && row["provider_metrics"]["request_id"] == row["submit"]["request_id"]
        && row["provider_metrics"]["generation"] == row["submit"]["generation"]
        && row["provider_metrics"]["request_attempted"] == true
        && row["provider_metrics"]["success"] == false
        && row["provider_metrics"]["failure_code"] == "foreground_cancelled"
        && row["owned_request_retired"] == true
        && row["cancelled_candidates_not_applied"] == true
}
fn observe_segments(row: &mut Value, snapshot: &Value, rows: &[Value], started: Instant) {
    if !rows.is_empty() && row["first_persisted_observed_ms"].is_null() {
        row["first_persisted_observed_ms"] = json!(ms(started));
        row["first_persisted_before_final_observed"] = json!(snapshot["final_validated"] != true);
    }
    let old = row["segment_observations"].as_array().unwrap().len();
    for value in rows.iter().skip(old) {
        row["segment_observations"].as_array_mut().unwrap().push(json!({"sequence":value["sequence"],
            "text":value["text"],"first_observed_ms":ms(started),"final_already_validated":snapshot["final_validated"]}));
    }
    row["last_stream_snapshot"] = snapshot.clone();
}
async fn stream_slot(
    app: &Arc<Workstation>,
    sid: &str,
    lid: &str,
    row: &mut Value,
    started: Instant,
    cancel: bool,
) -> Result<()> {
    let rid = s(&row["submit"], "request_id").to_owned();
    let generation = row["submit"]["generation"]
        .as_i64()
        .ok_or_else(|| anyhow!("generation_missing"))?;
    loop {
        text_only(app)?;
        let (snapshot, rows) = observation(app, sid, &rid)?;
        observe_segments(row, &snapshot, &rows, started);
        ensure!(
            !["failed", "cancelled", "interrupted"].contains(&s(&snapshot, "phase")),
            "stream_failed_before_acceptance"
        );
        if cancel && !rows.is_empty() {
            row["persisted_prefix_count"] = json!(rows.len());
            row["active_exact_registration_before_stop"] = json!(
                app.provider.active_foreground_for_diagnostic(&rid, lid)
                    && snapshot["final_validated"] != true
            );
            row["stop_started_ms"] = json!(ms(started));
            row["stop_reply"] = call(
                app,
                Method::POST,
                &format!("/api/sessions/{sid}/cancel"),
                json!({}),
            )
            .await?;
            row["stop_returned_ms"] = json!(ms(started));
            retired(app, &rid, lid).await?;
            row["owned_request_retired"] = json!(true);
            row["provider_metrics"] = provider_metric(app, &rid, generation);
            let after = effects(app, sid, lid)?;
            let before = &row["effects_before_delivery"];
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let stream = store
                .one(
                    "SELECT * FROM tutor_streams WHERE session_id=? AND request_id=?",
                    [sid, rid.as_str()],
                )?
                .ok_or_else(|| anyhow!("cancelled_stream_missing"))?;
            let turn = store
                .one(
                    "SELECT * FROM turns WHERE turn_id=?",
                    [s(&stream, "turn_id")],
                )?
                .ok_or_else(|| anyhow!("cancelled_turn_missing"))?;
            let unchanged = after["session"]["state"] == before["session"]["state"]
                && after["memories"] == before["memories"];
            ensure!(
                after["session"]["generation"].as_i64().unwrap_or(0) > generation
                    && stream["phase"] == "cancelled"
                    && turn["content"] == ""
                    && !app
                        .pending
                        .lock()
                        .unwrap()
                        .contains_key(s(&stream, "turn_id"))
                    && unchanged,
                "cancelled_reply_changed_current_state_or_delivery"
            );
            row["cancelled_candidates_not_applied"] = json!(true);
            row["cancelled_stream"] = stream;
            row["cancelled_turn"] = turn;
            row["effects_after_delivery"] = after;
            row["cancel_result"] = json!(if cancel_proven(row) {
                "EXERCISED"
            } else {
                "NOT_EXERCISED"
            });
            row["display_ack_scope"] =
                json!("No ACK for cancellation prefix; no visible completion is claimed");
            return Ok(());
        }
        if snapshot["final_validated"] == true {
            if row["final_observed_ms"].is_null() {
                row["final_observed_ms"] = json!(ms(started));
            }
            if let Some((tid, reply)) = pending_reply(app, sid, &rid, generation) {
                row["final_reply"] = reply;
                row["turn_id"] = json!(tid);
                ensure!(!rows.is_empty(), "final_without_segments");
                for sequence in 0..rows.len() {
                    let began = ms(started);
                    let receipt = call(
                        app,
                        Method::POST,
                        &format!("/api/sessions/{sid}/streams/{rid}/display"),
                        json!({"generation":generation,"sequence":sequence}),
                    )
                    .await?;
                    let returned = ms(started);
                    if sequence == 0 {
                        row["first_ack_returned_ms"] = json!(returned);
                    }
                    row["ack_receipts"].as_array_mut().unwrap().push(json!({"sequence":sequence,"started_ms":began,"returned_ms":returned,"receipt":receipt}));
                }
                let (after, segments) = observation(app, sid, &rid)?;
                ensure!(
                    after["phase"] == "completed"
                        && after["final_validated"] == true
                        && !app.pending.lock().unwrap().contains_key(&tid),
                    "display_commit_incomplete"
                );
                ensure!(
                    segments.iter().all(|v| v["shown"] == 1
                        && v["speech_state"] == "not_started"
                        && v["speech_started_receipt"].is_null()
                        && v["speech_terminal_receipt"].is_null()),
                    "unexpected_speech_receipt"
                );
                row["final_committed_observed_ms"] = json!(ms(started));
                row["last_stream_snapshot"] = after;
                row["persisted_segments_after_delivery"] = json!(segments);
                return Ok(());
            }
        }
        ensure!(
            started.elapsed() < Duration::from_secs(110),
            "stream_observation_deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn whole_slot(
    app: &Arc<Workstation>,
    sid: &str,
    row: &mut Value,
    started: Instant,
) -> Result<()> {
    let rid = s(&row["submit"], "request_id").to_owned();
    let generation = row["submit"]["generation"].as_i64().unwrap();
    loop {
        text_only(app)?;
        if let Some((tid, reply)) = pending_reply(app, sid, &rid, generation) {
            row["first_persisted_observed_ms"] = json!(ms(started));
            row["final_observed_ms"] = json!(ms(started));
            row["turn_id"] = json!(tid);
            row["final_reply"] = reply;
            let began = ms(started);
            row["whole_delivery_reply"] = call(
                app,
                Method::POST,
                &format!("/api/sessions/{sid}/delivery"),
                json!({"turn_id":tid,"status":"shown"}),
            )
            .await?;
            row["first_ack_returned_ms"] = json!(ms(started));
            row["ack_started_ms"] = json!(began);
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let turn = store
                .one("SELECT * FROM turns WHERE turn_id=?", [tid.as_str()])?
                .ok_or_else(|| anyhow!("whole_turn_missing"))?;
            ensure!(
                turn["status"] == "shown"
                    && turn["content"] == row["final_reply"]["reply_text"]
                    && !app.pending.lock().unwrap().contains_key(&tid),
                "whole_delivery_incomplete"
            );
            row["shown_turn"] = turn;
            row["final_committed_observed_ms"] = json!(ms(started));
            return Ok(());
        }
        ensure!(
            app.running.lock().unwrap().contains_key(&rid),
            "whole_request_finished_without_pending_reply"
        );
        ensure!(
            started.elapsed() < Duration::from_secs(110),
            "whole_observation_deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn run_slot(
    app: &Arc<Workstation>,
    case: &Value,
    slot: &Value,
    row: &mut Value,
) -> Result<()> {
    text_only(app)?;
    ensure!(
        app.provider.foreground_status()["provider"] == "deepseek_flash"
            && app.provider.foreground_status()["available"] == true
            && app.provider.foreground_status()["thinking"] == "non_thinking",
        "selected_foreground_not_prepared"
    );
    ensure!(
        app.provider.status()["available"] != true,
        "kimi_was_unexpectedly_probed"
    );
    ensure!(
        app.provider.status()["test_transport"].is_null(),
        "scripted_transport_not_allowed"
    );
    let learner=call(app,Method::POST,"/api/learners",json!({"name":"SYNTHETIC_TEXT_ACCEPTANCE","language":case["language"],"help_style":"guided","namespace":"simulator"})).await?;
    let lid = s(&learner, "learner_id").to_owned();
    let session = call(
        app,
        Method::POST,
        "/api/sessions",
        json!({"learner_id":lid,"resume":false}),
    )
    .await?;
    let sid = s(&session, "session_id").to_owned();
    let wait = Duration::from_millis(1600).saturating_sub(app.last_input.lock().unwrap().elapsed());
    row["existing_throttle_wait_ms"] = json!(wait.as_millis());
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    let started = Instant::now();
    row["status"] = json!("SUBMITTING");
    row["learner_id"] = json!(lid);
    row["session_id"] = json!(sid);
    row["submit"]=call(app,Method::POST,&format!("/api/sessions/{sid}/turns"),json!({"text":case["text"],
        "expected_generation":generation(app,&sid)?,"auto_speak":false,"client_submission_id":format!("flash-static-slot-{}",slot["slot"])})).await?;
    row["submit_returned_ms"] = json!(ms(started));
    row["status"] = json!("SUBMITTED");
    let rid = s(&row["submit"], "request_id").to_owned();
    let bound_generation = row["submit"]["generation"].as_i64().unwrap();
    row["source"] = source(app, &sid, &rid, &lid, s(case, "text"))?;
    row["effects_before_delivery"] = effects(app, &sid, &lid)?;
    let whole = slot["route"] == "whole_reply";
    ensure!(
        row["submit"]["delivery_mode"] == if whole { "whole_reply" } else { "segments" }
            && row["submit"]["speech_gate"] == "disabled",
        "delivery_configuration_changed"
    );
    let outcome = if whole {
        whole_slot(app, &sid, row, started).await
    } else {
        stream_slot(
            app,
            &sid,
            &lid,
            row,
            started,
            slot["route"] == "cancel_active_stream",
        )
        .await
    };
    // Retain metrics/effects even if a model/protocol/observation step failed.
    let drained = retired(app, &rid, &lid).await;
    row["provider_metrics"] = provider_metric(app, &rid, bound_generation);
    row["request_attempted"] = row["provider_metrics"]["request_attempted"].clone();
    row["effects_after_delivery"] = effects(app, &sid, &lid)?;
    row["owned_request_retired"] = json!(drained.is_ok());
    row["observed_duration_ms"] = json!(ms(started));
    outcome?;
    drained?;
    text_only(app)?;
    ensure!(
        row["provider_metrics"]["request_id"] == rid
            && row["provider_metrics"]["generation"] == bound_generation
            && row["provider_metrics"]["reported_model"] == "deepseek-flash"
            && row["request_attempted"] == true,
        "provider_receipt_not_bound"
    );
    if slot["route"] != "cancel_active_stream" {
        ensure!(
            row["provider_metrics"]["success"] == true
                && row["provider_metrics"]["api_stream_complete"] == true,
            "normal_request_not_completed"
        );
    }
    row["status"] = json!(
        if slot["route"] == "cancel_active_stream" && !cancel_proven(row) {
            "NOT_EXERCISED"
        } else {
            "RUNTIME_PASS"
        }
    );
    Ok(())
}

async fn close_fixture(app: &Arc<Workstation>) -> Value {
    let close = tokio::time::timeout(Duration::from_secs(15), app.close()).await;
    let provider = tokio::time::timeout(Duration::from_secs(3), app.provider.close()).await;
    let status = app.provider.status();
    let clear = close.is_ok()
        && matches!(&provider, Ok(Ok(())))
        && status["running"] == 0
        && app.running.lock().unwrap().is_empty();
    json!({"local_cleanup_confirmed":clear,"workstation_close_returned":close.is_ok(),
        "provider_close_returned_ok":matches!(provider,Ok(Ok(()))),"provider":status,
        "server_tasks_remaining":app.running.lock().unwrap().len(),"devices":devices(app),
        "remote_compute_stop_confirmed":false,"process_exit_still_verified_by_outer_runner":true})
}
async fn run(
    root: &Path,
    output: &Path,
    report: &mut Value,
    owners: &mut Vec<Arc<Workstation>>,
) -> Result<()> {
    let plan = fixed_plan()?;
    ensure!(
        read(&root.join(PLAN_PATH), 128 * 1024)? == PLAN.as_bytes(),
        "compiled_plan_file_changed"
    );
    let mut bindings = json!({});
    for (path, bytes) in CODE {
        ensure!(
            hash_file(&root.join(path))? == digest(bytes),
            "compiled_source_changed"
        );
        bindings[*path] = json!(digest(bytes));
    }
    let corpus_bytes = checked(
        root,
        s(&plan["corpus"], "path"),
        s(&plan["corpus"], "sha256"),
    )?;
    let corpus: Value = serde_json::from_slice(&corpus_bytes)?;
    ensure!(
        corpus["synthetic_only"] == true,
        "synthetic_corpus_required"
    );
    let mut cases = HashMap::new();
    for spec in plan["inputs"].as_array().unwrap() {
        let case = corpus["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == spec["id"])
            .ok_or_else(|| anyhow!("fixed_input_missing"))?;
        let text = s(case, "text");
        ensure!(
            case["language"] == spec["language"]
                && case["history"].as_array().is_some_and(Vec::is_empty)
                && text == text.trim()
                && digest(text.as_bytes()) == s(spec, "text_sha256"),
            "fixed_input_changed"
        );
        cases.insert(
            s(spec, "id").to_owned(),
            json!({"language":case["language"],"text":text}),
        );
    }
    fs::create_dir(output.join("inputs"))?;
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output.join("inputs/cases.json"))?
        .write_all(&corpus_bytes)?;
    write_new(
        &output.join("preregistered.json"),
        &json!({"plan":plan,"plan_sha256":digest(PLAN.as_bytes()),"source_bindings":bindings,
        "actual_executable_sha256":hash_file(&std::env::current_exe()?)?,"maximum_calls":MAX_CALLS,
        "inputs":cases,"simulated_display_ack":true,"api_dispatch_helpers":true,"loopback_auth_or_browser_paint_tested":false,
        "timing_scope":"Provider first_segment_enqueued_ms uses provider invocation clock; persistence/ACK/final observations use submit-start clock and10ms polling. They are separate observables, not server-token/acoustic clocks.",
        "raw_candidate_scope":"Final candidates captured before normal delivery ACK; application counters are not exposed by normal handlers, so recorded effects do not distinguish every rejected versus omitted candidate."}),
    )?;
    stage_fixture(root, &output.join("segments-fixture"), &plan, false)?;
    let app = Workstation::new(
        output.join("segments-fixture"),
        "synthetic-no-listener-token".into(),
    )?;
    owners.push(app.clone());
    report["segments_fixture_initial"] = json!({"teaching":app.provider.foreground_status(),"kimi":app.provider.status(),"devices":devices(&app)});
    let mut whole = None;
    for (index, slot) in plan["schedule"].as_array().unwrap().iter().enumerate() {
        ensure!(index < MAX_CALLS, "request_budget_exhausted");
        let active = if index == 3 {
            ensure!(
                app.provider.status()["running"] == 0 && app.running.lock().unwrap().is_empty(),
                "first_owner_not_retired"
            );
            stage_fixture(root, &output.join("whole-fixture"), &plan, true)?;
            let next = Workstation::new(
                output.join("whole-fixture"),
                "synthetic-no-listener-token".into(),
            )?;
            owners.push(next.clone());
            report["whole_fixture_initial"] = json!({"teaching":next.provider.foreground_status(),"kimi":next.provider.status(),"devices":devices(&next)});
            whole = Some(next);
            whole.as_ref().unwrap()
        } else {
            &app
        };
        // Record the slot before awaiting any Submit or HTTP work, including
        // when the outer deadline drops this monitor while an owner is active.
        report["calls"].as_array_mut().unwrap().push(json!({"slot":index,"route":slot["route"],"case_id":slot["case_id"],"status":"STARTED",
            "request_attempted":null,"segment_observations":[],"ack_receipts":[],"semantic_review":"REQUIRED",
            "provider_publication_scope":if index==3 {"internal whole-reply collector; not UI segment publication"} else {"typed segment channel to Workstation persistence"}}));
        write_new(
            &output.join(format!("attempt-{}.json", index + 1)),
            &json!({"slot":index,
            "route":slot["route"],"attempt_reserved":true,"http_attempted_yet":null,"maximum_calls":MAX_CALLS}),
        )?;
        let result = tokio::time::timeout(
            Duration::from_secs(110),
            run_slot(
                active,
                &cases[s(slot, "case_id")],
                slot,
                &mut report["calls"][index],
            ),
        )
        .await;
        let result = result
            .map_err(|_| anyhow!("slot_deadline_exceeded"))
            .and_then(|v| v);
        if let Err(error) = &result {
            report["calls"][index]["status"] = json!("FAILED");
            report["calls"][index]["failure"] = json!(error.to_string());
        }
        write_new(
            &output.join(format!("call-{}.json", index + 1)),
            &report["calls"][index],
        )?;
        result?;
    }
    ensure!(
        app.provider.status()["metrics"]["deepseek_segment_requests"] == 3
            && whole.as_ref().unwrap().provider.status()["metrics"]["deepseek_segment_requests"]
                == 1,
        "unexpected_foreground_call_count"
    );
    report["all_fixed_slots_completed"] = json!(true);
    ensure!(
        report["calls"][2]["status"] == "RUNTIME_PASS",
        "cancellation_not_exercised"
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    root: PathBuf,
    output: PathBuf,
}
#[tokio::test]
#[ignore = "Four explicit Flash text requests through isolated Workstation; no services or devices requested"]
async fn real_flash_foreground_acceptance_only() -> Result<()> {
    let path = PathBuf::from(
        std::env::var_os("AI_TUTOR_TEACHING_PROVIDER_REAL_PLAN")
            .ok_or_else(|| anyhow!("explicit_launch_required"))?,
    );
    let launch: Launch = serde_json::from_slice(&read(&path, 16 * 1024)?)?;
    let root = launch.root.canonicalize()?;
    let output = launch.output;
    ensure!(
        root.is_dir() && output.is_absolute() && !output.exists(),
        "fresh_absolute_output_required"
    );
    fs::create_dir(&output)?;
    write_new(
        &output.join("owner.json"),
        &json!({"pid":std::process::id(),"maximum_calls":MAX_CALLS,"launch_sha256":hash_file(&path)?,"plan_sha256":digest(PLAN.as_bytes())}),
    )?;
    let mut report = json!({"runtime_acceptance_passed":false,"calls":[],"maximum_calls":MAX_CALLS,"automatic_retries":0,
        "semantic_review":"REQUIRED","default_changed":false,"simulated_display_ack":true,"browser_paint_or_audio_measured":false,
        "runtime_pass_scope":"API route, contract, source binding, candidate commit and ownership. Early persistence before final is a separate observed boolean, not implied by RUNTIME_PASS.",
        "call_artifact_scope":"Per-call files precede final fixture cleanup; report may add separately labeled post-cleanup receipts without replacing original observations."});
    let mut owners = vec![];
    let result = tokio::time::timeout(
        Duration::from_secs(460),
        run(&root, &output, &mut report, &mut owners),
    )
    .await
    .map_err(|_| anyhow!("diagnostic_operation_deadline"))
    .and_then(|v| v);
    report["operation_failure"] = result
        .as_ref()
        .err()
        .map(|v| json!(v.to_string()))
        .unwrap_or(Value::Null);
    let mut cleanup = vec![];
    for owner in &owners {
        cleanup.push(close_fixture(owner).await);
    }
    let cleaned = cleanup.iter().all(|v| v["local_cleanup_confirmed"] == true);
    report["cleanup"] = json!(cleanup);
    report["local_cleanup_confirmed"] = json!(cleaned);
    for row in report["calls"].as_array_mut().unwrap() {
        let owner_index = if row["slot"] == 3 { 1 } else { 0 };
        if let (Some(owner), Some(rid), Some(generation)) = (
            owners.get(owner_index),
            row["submit"]["request_id"].as_str(),
            row["submit"]["generation"].as_i64(),
        ) {
            let metric = provider_metric(owner, rid, generation);
            if row["request_attempted"].is_null() {
                row["request_attempted"] = metric["request_attempted"].clone();
            }
            row["provider_metrics_after_cleanup"] = metric;
        }
        if !["RUNTIME_PASS", "NOT_EXERCISED", "FAILED"].contains(&s(row, "status")) {
            row["status"] = json!("INTERRUPTED_BY_OPERATION_FAILURE");
        }
    }
    let calls = report["calls"].as_array().unwrap();
    let attempted = calls
        .iter()
        .filter(|v| v["request_attempted"] == true)
        .count();
    let unknown = calls
        .iter()
        .filter(|v| v["request_attempted"].is_null())
        .count();
    report["http_requests_attempted"] = json!(attempted);
    report["http_attempt_status_unknown"] = json!(unknown);
    report["unvisited_slots"] = json!(MAX_CALLS - report["calls"].as_array().unwrap().len());
    report["runtime_acceptance_passed"] = json!(result.is_ok() && cleaned);
    write_new(&output.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"runtime_acceptance_passed":report["runtime_acceptance_passed"],"http_requests_attempted":attempted,
        "cancel_result":report["calls"][2]["cancel_result"],"semantic_review":"REQUIRED","local_cleanup_confirmed":cleaned})
    );
    result?;
    ensure!(cleaned, "local_cleanup_unconfirmed");
    Ok(())
}

#[test]
fn fixed_acceptance_budget_has_no_retry_or_replacement_slot() -> Result<()> {
    let plan = fixed_plan()?;
    assert_eq!(plan["schedule"].as_array().unwrap().len(), MAX_CALLS);
    Ok(())
}
#[test]
fn cancellation_acceptance_requires_matching_active_request_and_actual_cancel_outcome() {
    let mut row = json!({"active_exact_registration_before_stop":true,"persisted_prefix_count":1,
        "submit":{"request_id":"current","generation":7},"provider_metrics":{"request_id":"current","generation":7,
        "request_attempted":true,"success":false,"failure_code":"foreground_cancelled"},
        "owned_request_retired":true,"cancelled_candidates_not_applied":true});
    assert!(cancel_proven(&row));
    row["provider_metrics"]["success"] = json!(true);
    assert!(!cancel_proven(&row));
    row["provider_metrics"]["success"] = json!(false);
    row["provider_metrics"]["request_id"] = json!("old");
    assert!(!cancel_proven(&row));
    row["provider_metrics"]["request_id"] = json!("current");
    row["active_exact_registration_before_stop"] = json!(false);
    assert!(!cancel_proven(&row));
}

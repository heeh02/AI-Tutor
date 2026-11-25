//! Test-only long workload; parent owns the shared preparation and cleanup.
use super::*;
use crate::audio::{InputBinding, voice_loop_source::WavSource};

const ACTIVE_SECONDS: u64 = 1800;
const SWITCH_SECONDS: u64 = 900;
const LATE_SECONDS: u64 = 1500;
const FINISH_SECONDS: u64 = 300;
const MAX_ROUNDS: usize = 96;

pub(super) struct Run<'a> {
    pub app: &'a Arc<Workstation>,
    pub root: &'a Path,
    pub out: &'a Path,
    pub sid: &'a str,
    pub report: &'a mut Value,
    pub input: &'a mut InputBinding,
    pub source: &'a mut Arc<WavSource>,
    pub all_sources: &'a mut Vec<Arc<WavSource>>,
    pub heartbeat_tag: Arc<tokio::sync::Mutex<Value>>,
    pub transition: Arc<std::sync::atomic::AtomicU64>,
    pub start: Instant,
    pub overall: Instant,
    pub b_slot: FixtureSlot,
}

// Append the actual attempted round before artifact I/O can fail. Audit prose
// stays pending; this function only preserves evidence and lifecycle guards.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_round(
    app: &Arc<Workstation>,
    root: &Path,
    out: &Path,
    sid: &str,
    report: &mut Value,
    detail: &mut Value,
    index: usize,
    language: &str,
    started_ms: f64,
    active_start: Instant,
) -> Result<()> {
    let (source_id, source_sha, _) = fixed_source(language)?;
    detail["language"] = json!(language);
    detail["source_id"] = json!(source_id);
    detail["source_sha256"] = json!(source_sha);
    detail["soak_started_ms"] = json!(started_ms);
    detail["soak_finished_ms"] = json!(active_start.elapsed().as_secs_f64() * 1000.0);
    detail["shadow_preemption"] = if index == 1 {
        json!("A")
    } else if detail["before_submit_coordination"].is_object() {
        json!("B")
    } else {
        Value::Null
    };
    detail["managed_after_round"] = json!({
        "speech":app.local_speech_status(),"final_asr":app.final_asr_status(),
        "precise_installed":app.audio.status()["asr_precise_ready"]});
    let same_tts = app.local_speech_status()["instance_id"]
        == report["speech_preparation"]["instance_id"]
        && report["speech_preparation"]["instance_id"]
            .as_str()
            .is_some_and(|v| !v.is_empty());
    let protected = protected_joint(root);
    let unchanged = protected
        .as_ref()
        .is_ok_and(|v| v == &report["protected_before_sha256"]);
    detail["protected_files_unchanged"] = json!(unchanged);
    detail["gpu_pair_active_same_tts_instance"] = json!(gpu_pair_active(app) && same_tts);
    let final_asr_instance = app.final_asr.diagnostic_instance_id();
    let same_final_asr = final_asr_instance.as_deref().is_some_and(|instance| {
        !instance.is_empty() && Some(instance) == report["final_asr_instance_id"].as_str()
    });
    if report["foreground_provider"] == "deepseek_flash" {
        detail["final_asr_instance_id"] = json!(final_asr_instance);
        detail["same_final_asr_instance"] = json!(same_final_asr);
    }
    let audit = memory_audit(app, Some(sid));
    detail["memory_audit"] = audit
        .as_ref()
        .map_or(json!({"observation_failed":true}), |v| v.clone());
    detail["provider_diagnostics"] = if report["foreground_provider"] == "deepseek_flash" {
        bound_foreground_provider_diagnostics(detail, &app.provider.status()["metrics"])
    } else {
        bound_provider_diagnostics(detail, &app.provider.status()["metrics"])
    };
    report["rounds"]
        .as_array_mut()
        .unwrap()
        .push(detail.clone());
    write_new(&out.join(format!("round-{index:03}.json")), detail)?;
    ensure!(audit.is_ok(), "per-round memory audit failed");
    ensure!(unchanged, "protected files changed during workload");
    ensure!(
        gpu_pair_active(app) && same_tts,
        "GPU pair or TTS instance changed"
    );
    if report["foreground_provider"] == "deepseek_flash" {
        ensure!(same_final_asr, "GPU pair or TTS instance changed");
    }
    Ok(())
}

pub(super) async fn switch_to_english(run: &mut Run<'_>) -> Result<()> {
    // Holding this diagnostic-only target mutex serializes against an in-flight
    // heartbeat. Production lease/generation changes still use normal APIs.
    let heartbeat_target = run.heartbeat_tag.clone();
    let mut tag = heartbeat_target.lock().await;
    let before_generation = generation(run.app, run.sid)?;
    let before_binding = json!({"device_key":run.input.device_key,
        "topology_revision":run.input.topology_revision});
    run.transition
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst); // Odd: this one planned transition is active.
    let began = Instant::now();
    let operation = tokio::time::timeout(Duration::from_secs(5), async {
        ensure!(
            run.source.status()["active"] == false
                && run.source.status()["opens"] == run.source.status()["closes"],
            "language switch with active capture"
        );
        let settings = call(
            run.app,
            Method::POST,
            &format!("/api/sessions/{}/settings", run.sid),
            json!({"language":"en"}),
        )
        .await?;
        ensure!(
            settings["ok"] == true && settings["settings"]["language"] == "en",
            "English setting not applied"
        );
        let (id, expected, _) = fixed_source("en")?;
        let wav = run.root.join(CORPUS).join(format!("{id}.wav"));
        ensure!(hash(&wav)? == expected, "fixed English WAV changed");
        let (input, source) = run.app.audio.install_voice_loop_wav(&wav)?;
        *run.input = input;
        *run.source = source;
        run.all_sources.push(run.source.clone());
        let enabled = call(
            run.app,
            Method::POST,
            &format!("/api/sessions/{}/voice-loop/enable", run.sid),
            json!({"client_loop_id":"real-managed-soak-en",
            "expected_generation":generation(run.app,run.sid)?,"language":"en",
            "device_key":run.input.device_key,
            "expected_input_topology_revision":run.input.topology_revision}),
        )
        .await?;
        ensure!(
            enabled["phase"] == "armed" && run.source.status()["opens"] == 0,
            "English loop enable opened capture or failed"
        );
        *tag =
            json!({"client_loop_id":enabled["client_loop_id"],"loop_epoch":enabled["loop_epoch"]});
        Ok::<(), anyhow::Error>(())
    })
    .await
    .unwrap_or_else(|_| Err(anyhow!("language transition exceeded5s")));
    run.transition
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst); // Even: completed/failed; no later pauses allowed.
    let bounded = began.elapsed() <= Duration::from_secs(5);
    run.report["language_transition"] = json!({"attempted":true,"completed":operation.is_ok() && bounded,
        "operation_completed":operation.is_ok(),"actual_elapsed_within_budget":bounded,
        "at_active_ms":run.start.elapsed().as_secs_f64()*1000.0,
        "elapsed_ms":began.elapsed().as_secs_f64()*1000.0,"from":"zh","to":"en",
        "generation_before":before_generation,"generation_after":generation(run.app,run.sid).ok(),
        "binding_before":before_binding,"binding_after":{"device_key":run.input.device_key,
            "topology_revision":run.input.topology_revision},
        "normal_settings_and_enable_apis":true,"camera_reopened":false});
    operation?;
    ensure!(bounded, "language transition exceeded5s");
    Ok(())
}

pub(super) async fn run_tail(mut run: Run<'_>) -> Result<()> {
    let cases: Value = serde_json::from_str(CASES)?;
    let active_deadline = run.start + Duration::from_secs(ACTIVE_SECONDS);
    let finish_deadline =
        tokio::time::Instant::from_std(active_deadline + Duration::from_secs(FINISH_SECONDS));
    let mut language = "zh";
    let mut recovered_b = false;
    run.report["soak"] = json!({"minimum_active_ms":ACTIVE_SECONDS*1000,
        "foreground_limit":MAX_ROUNDS,"late_b_not_before_ms":LATE_SECONDS*1000,
        "finish_budget_ms":FINISH_SECONDS*1000,"idle_padding":false});
    while Instant::now() < active_deadline {
        let index = run.report["rounds"].as_array().unwrap().len() + 1;
        ensure!(
            index <= MAX_ROUNDS,
            "foreground cap reached before1800s; no idle padding"
        );
        if language == "zh" && run.start.elapsed() >= Duration::from_secs(SWITCH_SECONDS) {
            switch_to_english(&mut run).await?;
            language = "en";
        }
        ensure!(gpu_pair_active(run.app), "GPU pair no longer active");
        let tag = run.heartbeat_tag.lock().await.clone();
        let (_, _, reference) = fixed_source(language)?;
        if Instant::now() >= active_deadline {
            break;
        }
        let begun_ms = run.start.elapsed().as_secs_f64() * 1000.0;
        let late = !recovered_b && run.start.elapsed() >= Duration::from_secs(LATE_SECONDS);
        let hook_app = run.app.clone();
        let hook_case = cases["tasks"][1].clone();
        let hook_slot = run.b_slot.clone();
        let active_start = run.start;
        let hook = async move {
            if !late {
                return Ok(Value::Null);
            }
            let seeded_ms = active_start.elapsed().as_secs_f64() * 1000.0;
            ensure!(
                (LATE_SECONDS as f64 * 1000.0..ACTIVE_SECONDS as f64 * 1000.0).contains(&seeded_ms),
                "late background seed outside active window"
            );
            let b = seed(&hook_app, &hook_case)?;
            *hook_slot.lock().unwrap() = Some(b.clone());
            let mut first = Value::Null;
            wait_started(&hook_app, &b, &mut first).await?;
            first["seeded_active_ms"] = json!(seeded_ms);
            first["started_active_ms"] = json!(active_start.elapsed().as_secs_f64() * 1000.0);
            ensure!(
                active_start.elapsed() < Duration::from_secs(ACTIVE_SECONDS),
                "late background first start missed active window"
            );
            Ok(first)
        };
        let mut detail = Value::Null;
        let mut preempt = Value::Null;
        set_stage(
            run.out,
            run.report,
            if late {
                "late_B_Submit_preemption"
            } else {
                "soak_round"
            },
            run.overall,
        )?;
        let result = tokio::time::timeout_at(finish_deadline, async {
            if late {
                tokio::try_join!(
                    round_with_before_submit_expected_asr(
                        run.app,
                        run.sid,
                        &tag,
                        run.input,
                        run.source,
                        index,
                        reference,
                        FINAL_PROVIDER,
                        hook,
                        &mut detail
                    ),
                    observe_preemption(run.app, run.b_slot.clone(), &mut preempt)
                )?;
            } else {
                round_with_before_submit_expected_asr(
                    run.app,
                    run.sid,
                    &tag,
                    run.input,
                    run.source,
                    index,
                    reference,
                    FINAL_PROVIDER,
                    hook,
                    &mut detail,
                )
                .await?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("soak bounded finish deadline")));
        if late {
            run.report["jobs"]["B"]["first_start"] = detail["before_submit_coordination"].clone();
            run.report["jobs"]["B"]["started_active_ms"] =
                detail["before_submit_coordination"]["started_active_ms"].clone();
            run.report["jobs"]["B"]["preemption"] = preempt;
        }
        record_round(
            run.app,
            run.root,
            run.out,
            run.sid,
            run.report,
            &mut detail,
            index,
            language,
            begun_ms,
            run.start,
        )?;
        result?;
        if late {
            let b = run
                .b_slot
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow!("late B source absent"))?;
            let baseline = run.report["jobs"]["B"]["preemption"]["last_job"].clone();
            let mut recovered = Value::Null;
            let recovery = tokio::time::timeout_at(
                finish_deadline,
                wait_recovered(run.app, &b, &baseline, &mut recovered),
            )
            .await
            .unwrap_or_else(|_| Err(anyhow!("soak recovery finish deadline")));
            run.report["jobs"]["B"]["recovery"] = recovered;
            recovery?;
            recovered_b = true;
        }
        progress(run.out, run.report, "soak_round_complete", run.overall)?;
    }
    ensure!(
        run.start.elapsed() >= Duration::from_secs(ACTIVE_SECONDS),
        "soak active duration incomplete"
    );
    ensure!(
        recovered_b && language == "en",
        "late background or bilingual phase not exercised"
    );
    ensure!(
        run.all_sources.len() == 2,
        "unexpected source installation count"
    );
    let total_opens: u64 = run
        .all_sources
        .iter()
        .map(|s| s.status()["opens"].as_u64().unwrap_or(0))
        .sum();
    ensure!(
        total_opens == run.report["rounds"].as_array().unwrap().len() as u64,
        "soak source round ownership count mismatch"
    );
    run.report["soak"]["active_window_reached"] = json!(true);
    run.report["soak"]["late_b_recovered"] = json!(true);
    run.report["soak"]["bilingual_phases_completed"] = json!(true);
    Ok(())
}

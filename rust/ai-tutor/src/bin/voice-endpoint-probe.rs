//! Explicit offline endpoint probe. Never creates AudioService, CPAL or an ASR.
#[path = "voice_endpoint_probe/fixture.rs"]
mod fixture;

use ai_tutor::audio::voice_endpoint::{
    EndpointCandidate, EndpointReason, FRAME_MS, FRAME_SAMPLES, Frame, METHOD_ID, SAMPLE_RATE,
    VoiceEndpoint,
};
use anyhow::{Result, anyhow, ensure};
use fixture::{Case, Kind};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Instant};

fn timing(samples: &[f64]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| sorted[((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1)];
    json!({"unit":"microseconds_per_10ms_frame", "frames":samples.len(),
        "p50":percentile(0.50),"p95":percentile(0.95),"p99":percentile(0.99),
        "max":sorted.last(),"frames_over_10ms":samples.iter().filter(|v| **v >= 10_000.0).count(),
        "p99_target_under_10ms":percentile(0.99) < 10_000.0})
}

fn acoustic_target(case: &Case, terminal: Option<EndpointCandidate>) -> Option<bool> {
    if matches!(case.kind, Kind::Pause4000Risk) {
        return None;
    }
    let Some(terminal) = terminal else {
        return Some(false);
    };
    Some(match case.kind {
        Kind::Clean | Kind::Pause2000 => {
            let source_end_ms =
                case.supports.last().unwrap()[1] as f64 * 1000.0 / SAMPLE_RATE as f64;
            let lag = terminal.elapsed_audio_ms as f64 - source_end_ms;
            terminal.reason == EndpointReason::SpeechEnd && (2500.0..=3500.0).contains(&lag)
        }
        Kind::Continuous => {
            terminal.reason == EndpointReason::DurationLimit && terminal.elapsed_audio_ms == 20_000
        }
        Kind::Silence | Kind::LowNoise | Kind::LowHum => {
            terminal.reason == EndpointReason::NoSpeech && terminal.elapsed_audio_ms == 10_000
        }
        Kind::DeviceError => {
            terminal.reason == EndpointReason::DeviceError && terminal.elapsed_audio_ms == 2_000
        }
        Kind::Pause4000Risk => unreachable!(),
    })
}

fn run_case(case: &Case, generation: u64) -> Value {
    let original_hash = fixture::signal_hash(&case.input);
    let start = Instant::now();
    let mut detector = VoiceEndpoint::new(generation);
    let initialization_us = start.elapsed().as_secs_f64() * 1e6;
    let mut times = Vec::new();
    let mut probabilities = Vec::new();
    let mut terminal = None;
    let mut terminal_count = 0;
    for samples in case.input.chunks_exact(FRAME_SAMPLES) {
        let frame: &Frame = samples.try_into().unwrap();
        let start = Instant::now();
        let candidate = detector.process_10ms(generation, frame);
        times.push(start.elapsed().as_secs_f64() * 1e6);
        if let Some(probability) = detector.last_probability() {
            probabilities.push(probability);
        }
        if let Some(candidate) = candidate {
            terminal_count += 1;
            terminal = Some(candidate);
            break;
        }
    }
    if matches!(case.kind, Kind::DeviceError) {
        if let Some(candidate) = detector.device_error(generation) {
            terminal_count += 1;
            terminal = Some(candidate);
        }
    }
    // Repeated input and error delivery must not emit a second terminal.
    if terminal.is_some() {
        terminal_count += usize::from(
            detector
                .process_10ms(generation, &[0.0; FRAME_SAMPLES])
                .is_some(),
        );
        terminal_count += usize::from(detector.device_error(generation).is_some());
    }
    let terminal_at_samples = terminal.map(|v| v.elapsed_audio_ms as usize * SAMPLE_RATE / 1000);
    let source_end_ms = case
        .supports
        .last()
        .map(|v| v[1] as f64 * 1000.0 / SAMPLE_RATE as f64);
    let lag = terminal
        .zip(source_end_ms)
        .map(|(v, end)| v.elapsed_audio_ms as f64 - end);
    let future_speech = terminal_at_samples.map(|at| case.supports.iter().any(|span| span[1] > at));
    let target = acoustic_target(case, terminal);
    let unchanged = original_hash == fixture::signal_hash(&case.input);
    let timing = timing(&times);
    let invariant_pass = unchanged && terminal_count <= 1 && detector.terminal() == terminal;
    json!({
        "id":case.id,"kind":case.kind.name(),"recording_generation":generation,
        "input_sha256_f32le":original_hash,"input_samples":case.input.len(),"input_unchanged":unchanged,
        "source_support_intervals_samples":case.supports,"last_source_support_end_ms":source_end_ms,
        "terminal":terminal,"terminal_count":terminal_count,
        "lag_from_last_nonzero_source_boundary_ms":lag,
        "future_source_speech_after_terminal":future_speech,
        "acoustic_target_pass":target,
        "known_semantic_ambiguity_risk":matches!(case.kind, Kind::Pause4000Risk),
        "invariants_pass":invariant_pass,
        "initialization_us":initialization_us,"processing":timing,"simd_backend":detector.backend_name(),
        "vad_probability_min":probabilities.iter().copied().reduce(f32::min),
        "vad_probability_max":probabilities.iter().copied().reduce(f32::max),
        "probability_frames_ge_onset":probabilities.iter().filter(|v| **v >= 0.60).count(),
        "input_closed":false,"devices_used":false,"automatic_send":false,
        "metric_boundary":"exact synthetic source support; not a manually annotated spoken-word boundary"
    })
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut offline = false;
    let mut fixture_path = None;
    let mut expected_hash = None;
    let mut output = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--offline-synthetic" if !offline => offline = true,
            "--fixture" if fixture_path.is_none() => {
                fixture_path = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing fixture"))?,
                ))
            }
            "--method-sha256" if expected_hash.is_none() => {
                expected_hash = Some(args.next().ok_or_else(|| anyhow!("Missing method SHA"))?)
            }
            "--output" if output.is_none() => {
                output = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow!("Missing output directory"))?,
                ))
            }
            _ => return Err(anyhow!("Unknown or duplicate argument: {argument}")),
        }
    }
    ensure!(
        offline,
        "Explicit --offline-synthetic is required; no live mode exists"
    );
    let directory = fixture_path.ok_or_else(|| anyhow!("--fixture is required"))?;
    let expected_hash = expected_hash.ok_or_else(|| anyhow!("--method-sha256 is required"))?;
    let output = output.ok_or_else(|| anyhow!("--output is required"))?;
    let (method, cases) = fixture::load(&directory, &expected_hash)?;
    ensure!(!output.exists(), "Output directory must be new");
    // The parent directory must already exist; never reuse an earlier report.
    std::fs::create_dir(&output)?;
    let rows: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, case)| run_case(case, i as u64 + 1))
        .collect();
    let invariants = rows.iter().all(|v| v["invariants_pass"] == true);
    let acoustic_targets = rows.iter().all(|v| v["acoustic_target_pass"] != false);
    let timing_targets = rows
        .iter()
        .all(|v| v["processing"]["p99_target_under_10ms"] == true);
    let targets_pass = invariants && acoustic_targets && timing_targets;
    let report = json!({
        "protocol_version":1,"method_id":METHOD_ID,"method_sha256":fixture::METHOD_SHA256,
        "status":"executed_offline_synthetic","method":method,"case_count":rows.len(),"cases":rows,
        "required_targets_pass":targets_pass,"invariants_pass":invariants,
        "acoustic_targets_pass_excluding_explicit_risk_rows":acoustic_targets,"processing_targets_pass":timing_targets,
        "production_auto_mode_acceptance":false,"real_room_or_child_validation":false,
        "external_asr_or_tts_services_started":false,"devices_used":false,"frame_ms":FRAME_MS,
        "executable_sha256":fixture::hash(&std::fs::read(std::env::current_exe()?)?),
        "endpoint_policy_wait_ms":2500,
        "latency_note":"Policy wait contributes to spoken-turn-to-reply delay; CPU inference timing is not end-to-end latency.",
        "known_unresolved_limit":"A long thinking pause can cause an early acoustic terminal. The 4s risk rows are never counted as successful full turns."
    });
    std::fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "Endpoint report saved; required targets pass: {targets_pass}; production acceptance: false"
    );
    ensure!(
        targets_pass,
        "One or more frozen targets failed; every row is retained in report.json"
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

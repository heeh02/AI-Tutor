//! Explicit offline synthetic AEC probe. Does not instantiate AudioService or CPAL.
#[path = "../audio/echo_cancel.rs"]
mod echo_cancel;
#[path = "aec_probe/fixture.rs"]
mod fixture;
#[path = "aec_probe/metrics.rs"]
mod metrics;

use anyhow::{Result, anyhow, ensure};
use echo_cancel::{EchoCanceller16k, FRAME_SAMPLES, Frame, SAMPLE_RATE};
use fixture::{CHANGE_AT, Mixture, NEAR_START, STOP_TAIL_SAMPLES, Scenario};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Instant};

struct Run {
    output: Vec<f32>,
    valid: Vec<bool>,
    pair_us: Vec<f64>,
    initialization_us: f64,
    reset_us: Vec<f64>,
    dropped_frames: usize,
}

fn process(mixture: &Mixture, high_pass: bool) -> Result<Run> {
    let started = Instant::now();
    let mut aec = if high_pass {
        EchoCanceller16k::with_high_pass(true)
    } else {
        EchoCanceller16k::new()
    };
    let initialization_us = started.elapsed().as_secs_f64() * 1e6;
    let mut run = Run {
        output: vec![0.0; mixture.capture.len()],
        valid: vec![true; mixture.capture.len()],
        pair_us: Vec::new(),
        initialization_us,
        reset_us: Vec::new(),
        dropped_frames: 0,
    };
    let mut reset_next = false;
    for at in (0..mixture.capture.len()).step_by(FRAME_SAMPLES) {
        if matches!(mixture.scenario, Scenario::ReferenceGap) && at == CHANGE_AT {
            // There is real echo in the capture frame, but its reference packet is
            // deliberately missing. Do not present invented silence to the AEC.
            run.valid[at..at + FRAME_SAMPLES].fill(false);
            run.dropped_frames += 1;
            reset_next = true;
            continue;
        }
        if reset_next {
            let reset = Instant::now();
            aec.reset();
            run.reset_us.push(reset.elapsed().as_secs_f64() * 1e6);
            reset_next = false;
        }
        let render: &Frame = mixture.render[at..at + FRAME_SAMPLES].try_into()?;
        let capture: &Frame = mixture.capture[at..at + FRAME_SAMPLES].try_into()?;
        let started = Instant::now();
        let output = aec.process(render, capture, mixture.scenario.delay_ms(at) as i32)?;
        run.pair_us.push(started.elapsed().as_secs_f64() * 1e6);
        run.output[at..at + FRAME_SAMPLES].copy_from_slice(&output);
    }
    Ok(run)
}

fn report_case(mixture: &Mixture, run: &Run, calibration: &metrics::LagCalibration) -> Value {
    let len = mixture.capture.len();
    let mut report = json!({"scenario":mixture.scenario.name(),"parameters":mixture.parameters(),
        "initialization_us":run.initialization_us,"reset_us":run.reset_us,"dropped_reference_frames":run.dropped_frames,
        "excluded_output_samples":run.valid.iter().filter(|v| !**v).count(),
        "output_sha256_f32le":fixture::signal_hash(&run.output),"timing":metrics::timing(&run.pair_us),
        "actual_devices_used":false,"quality_pass":null,
        "quality_status":"measurements_only_no_predeclared_acceptance_threshold"});

    let far_end = match mixture.scenario {
        Scenario::Near => 0,
        Scenario::Double(_) => NEAR_START - SAMPLE_RATE / 2,
        _ => len,
    };
    if far_end > 0 {
        let mut windows = vec![
            ("full_far_only_window", 0..far_end),
            ("after_3s_adaptation", 3 * SAMPLE_RATE..far_end),
        ];
        match mixture.scenario {
            Scenario::Stop => {
                windows.push((
                    "render_stop_and_tail",
                    CHANGE_AT..CHANGE_AT + STOP_TAIL_SAMPLES,
                ));
                windows.push((
                    "first_second_after_stop",
                    CHANGE_AT..CHANGE_AT + SAMPLE_RATE,
                ));
            }
            Scenario::DelayJump | Scenario::ReferenceGap => {
                windows.push((
                    "first_second_after_change",
                    CHANGE_AT..CHANGE_AT + SAMPLE_RATE,
                ));
                windows.push(("after_3s_recovery", CHANGE_AT + 3 * SAMPLE_RATE..far_end));
            }
            _ => {}
        }
        report["far_only"]=json!(windows.into_iter().map(|(name,window)| {
            let zero=metrics::erle(&mixture.echo,&run.output,&run.valid,0,window.clone());
            let calibrated=calibration.usable.then(|| metrics::erle(&mixture.echo,&run.output,&run.valid,calibration.lag,window.clone()));
            json!({"window":name,"start_ms":window.start/16,"end_ms":window.end/16,"zero_lag":zero,
                "fixed_calibrated_lag":calibrated,"near_speech_absent_in_window":true})
        }).collect::<Vec<_>>());
        if matches!(mixture.scenario, Scenario::Stop) {
            let start = CHANGE_AT + SAMPLE_RATE; // Beyond maximum known FIR delay + 20 ms alignment margin.
            report["after_known_echo_tail"] =
                metrics::erle(&mixture.echo, &run.output, &run.valid, 0, start..len);
        }
    }
    if mixture.scenario.near_present() {
        let start = if matches!(mixture.scenario, Scenario::Near) {
            3 * SAMPLE_RATE
        } else {
            NEAR_START + SAMPLE_RATE / 2
        };
        let window = start..len - metrics::MAX_LAG;
        report["near_fidelity"] = json!({"start_ms":window.start/16,"end_ms":window.end/16,
            "unprocessed_capture":metrics::fidelity(&mixture.near,&mixture.capture,&run.valid,0,window.clone()),
            "aec_zero_lag":metrics::fidelity(&mixture.near,&run.output,&run.valid,0,window.clone()),
            "aec_fixed_calibrated_lag":calibration.usable.then(||metrics::fidelity(&mixture.near,&run.output,&run.valid,calibration.lag,window))});
    }
    report
}

fn comparison_case(
    mixture: &Mixture,
    outcome: &Result<Run>,
    calibration: &metrics::LagCalibration,
    all_times: &mut Vec<f64>,
) -> Value {
    match outcome {
        Ok(run) => {
            all_times.extend_from_slice(&run.pair_us);
            let mut report = report_case(mixture, run, calibration);
            report["processing_status"] = json!("processed");
            report["frozen_input_hashes_verified"] = json!(true);
            report
        }
        Err(error) => json!({"scenario":mixture.scenario.name(),"parameters":mixture.parameters(),
            "processing_status":"processing_failed","error":error.to_string().chars().take(512).collect::<String>(),
            "frozen_input_hashes_verified":true,"quality_pass":null,
            "output_sha256_f32le":null,"timing":null,"far_only":null,"near_fidelity":null,
            "metrics_unavailable_reason":"aec_processing_failed","actual_devices_used":false}),
    }
}

fn run_comparison_cell(
    cell: &fixture::ComparisonCell,
    mut processor: impl FnMut(&Mixture, bool) -> Result<Run>,
) -> (Value, Vec<f64>) {
    // Only this near-only control chooses the lag. Failure never selects another
    // control, a different waveform, or a larger calibration search window.
    let near_control = fixture::make(&cell.corpus, Scenario::Near);
    let near_run = processor(&near_control, cell.high_pass);
    let calibration = match &near_run {
        Ok(run) => metrics::calibrate(&near_control.near, &run.output, &run.valid),
        Err(_) => metrics::LagCalibration {
            lag: 0,
            usable: false,
            report: json!({"usable":false,"lag_samples":null,"lag_ms":null,
                "invalid_reason":"near_only_processing_failed","search_samples":[0,metrics::MAX_LAG],
                "window_ms":[3000,8000],"selection":"single_near_only_control_then_frozen_for_all_scenarios"}),
        },
    };
    let mut cases = Vec::new();
    let mut all_times = Vec::new();
    for scenario in fixture::ORDINARY_SCENARIOS {
        if matches!(scenario, Scenario::Near) {
            cases.push(comparison_case(
                &near_control,
                &near_run,
                &calibration,
                &mut all_times,
            ));
        } else {
            let mixture = fixture::make(&cell.corpus, scenario);
            let outcome = processor(&mixture, cell.high_pass);
            cases.push(comparison_case(
                &mixture,
                &outcome,
                &calibration,
                &mut all_times,
            ));
        }
    }
    let failures = cases
        .iter()
        .filter(|case| case["processing_status"] != "processed")
        .count();
    (
        json!({"cell_id":cell.manifest["cell_id"],"manifest_sha256":cell.manifest_sha256,
        "algorithm":cell.manifest["algorithm"],"input":cell.corpus.provenance,
        "parameters":cell.manifest["parameters"],"lag_calibration":calibration.report,
        "cases":cases,"scenario_count":7,"processing_failed_count":failures,
        "aggregate_processing_time":metrics::timing(&all_times),"quality_pass":null}),
        all_times,
    )
}

fn run_comparison(protocol_path: &std::path::Path, load_note: &str) -> Result<Value> {
    let started_at = chrono::Utc::now();
    let elapsed = Instant::now();
    let comparison = fixture::load_comparison(protocol_path)?;
    let preflight_ms = elapsed.elapsed().as_secs_f64() * 1000.0;
    let mut cells = Vec::new();
    let mut all_times = Vec::new();
    for cell in &comparison.cells {
        let (report, times) = run_comparison_cell(cell, process);
        all_times.extend(times);
        cells.push(report);
        eprintln!("completed synthetic comparison cell {}/4", cells.len());
    }
    let failures: u64 = cells
        .iter()
        .map(|cell| cell["processing_failed_count"].as_u64().unwrap_or(7))
        .sum();
    Ok(
        json!({"schema_version":1,"probe":"offline_synthetic_aec3_2x2",
        "protocol_sha256":fixture::PROTOCOL_SHA256,"frozen_protocol":comparison.protocol,
        "metrics_plan":comparison.metrics_plan,"all_input_hashes_verified_before_processing":true,
        "started_at":started_at.to_rfc3339(),"finished_at":chrono::Utc::now().to_rfc3339(),
        "elapsed_ms":elapsed.elapsed().as_secs_f64()*1000.0,"preflight_ms":preflight_ms,
        "algorithm":{"crate":"sonora","version":"0.2.0","source_commit":"a024d6ef8351add55be5e8b1d6cc35f555787660",
            "crate_sha256":"2e1ce442ab5cb392c4067b8f2cced20be4cf12e479f6dfbf44acbe9b14ac9250",
            "license":"BSD-3-Clause","transparent_mode":"Legacy","noise_suppression":false,"agc":false,
            "unreleased_render_buffer_headroom_fix_included":false},
        "cells":cells,"cell_count":4,"scenario_count":28,"processing_failed_count":failures,
        "all_scenarios_processed":failures==0,"quality_pass":null,
        "aggregate_processing_time":metrics::timing(&all_times),"load_note":load_note,
        "build_profile":if cfg!(debug_assertions){"debug"}else{"release"},
        "target_os":std::env::consts::OS,"target_arch":std::env::consts::ARCH,
        "frame_ms":10,"paced":false,"actual_devices_used":false,"production_enabled":false,
        "voice_barge_in_enabled":false,"raw_audio_saved":false,"synthetic_audio_saved":false,
        "asr_or_tts_inference_called":false,
        "limitations":["Previously generated adult synthetic WAVs; not real learners, room acoustics or independent new texts",
            "Only the near waveform family and enforced capture high-pass setting vary; no suppressor tuning",
            "All four cells, three echo delays and three SER conditions are reported even when worse or failed",
            "The same known near target is used with HPF off/on; HPF is not removed from the fidelity penalty",
            "Each cell uses only its own near-only control for one fixed lag; zero-lag metrics remain visible",
            "No quality acceptance threshold, production enablement or physical voice-interruption acceptance is inferred",
            "Non-paced CPU processing cost excludes devices, clock drift and ASR/TTS inference"]}),
    )
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut manifest = None;
    let mut protocol = None;
    let mut output = None;
    let mut load_note =
        String::from("unspecified; timing must not be treated as a quiet-machine baseline");
    while let Some(argument) = args.next() {
        if argument == "--help" {
            println!(
                "aec-probe (--corpus <fixed-sapi-manifest.json> | --protocol <frozen-2x2-protocol.json>) --output <new-report.json> [--load-note <description>]\nOffline synthetic files only; no devices, model inference or production integration."
            );
            return Ok(());
        }
        let value = args
            .next()
            .ok_or_else(|| anyhow!("Missing value for {argument}"))?;
        match argument.as_str() {
            "--corpus" => manifest = Some(PathBuf::from(value)),
            "--protocol" => protocol = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--load-note" => {
                ensure!(value.len() <= 300, "Load note is too long");
                load_note = value;
            }
            _ => return Err(anyhow!("Unknown argument {argument}")),
        }
    }
    ensure!(
        manifest.is_some() != protocol.is_some(),
        "Provide exactly one of --corpus or --protocol"
    );
    let output = output.ok_or_else(|| anyhow!("--output is required"))?;
    ensure!(
        !output.exists(),
        "Use a new report path to preserve previous evidence"
    );
    if let Some(protocol) = protocol {
        let report = run_comparison(&protocol, &load_note)?;
        write_report(&output, &report)?;
        println!(
            "{}",
            json!({"report_written":true,"scenario_count":28,
            "processing_failed_count":report["processing_failed_count"],"devices_used":false,"quality_pass":null})
        );
        ensure!(
            report["all_scenarios_processed"] == true,
            "Comparison report retained failed processing rows"
        );
        return Ok(());
    }
    let manifest = manifest.ok_or_else(|| anyhow!("--corpus is required"))?;
    let started_at = chrono::Utc::now();
    let elapsed = Instant::now();
    let corpus = fixture::load(&manifest)?;
    // This control is processed first, once. Mixture results cannot tune its lag.
    let near_control = fixture::make(&corpus, Scenario::Near);
    let near_run = process(&near_control, false)?;
    let calibration = metrics::calibrate(&near_control.near, &near_run.output, &near_run.valid);
    let mut cases = Vec::new();
    let mut all_times = Vec::new();
    for scenario in fixture::SCENARIOS {
        if matches!(scenario, Scenario::Near) {
            all_times.extend_from_slice(&near_run.pair_us);
            cases.push(report_case(&near_control, &near_run, &calibration));
        } else {
            let mixture = fixture::make(&corpus, scenario);
            mixture.validate_event_input()?;
            let run = process(&mixture, false)?;
            all_times.extend_from_slice(&run.pair_us);
            cases.push(report_case(&mixture, &run, &calibration));
        }
        eprintln!(
            "completed synthetic case {}/{}",
            cases.len(),
            fixture::SCENARIOS.len()
        );
    }
    let report = json!({"schema_version":1,"probe":"offline_synthetic_aec3","started_at":started_at.to_rfc3339(),
        "finished_at":chrono::Utc::now().to_rfc3339(),"elapsed_ms":elapsed.elapsed().as_secs_f64()*1000.0,
        "algorithm":{"crate":"sonora","version":"0.2.0","source_commit":"a024d6ef8351add55be5e8b1d6cc35f555787660",
            "crate_sha256":"2e1ce442ab5cb392c4067b8f2cced20be4cf12e479f6dfbf44acbe9b14ac9250",
            "license":"BSD-3-Clause","noise_suppression":false,"agc":false,"high_pass_filter":false,
            "unreleased_render_buffer_headroom_fix_included":false},
        "input":corpus.provenance,"lag_calibration":calibration.report,"cases":cases,
        "aggregate_processing_time":metrics::timing(&all_times),"load_note":load_note,
        "build_profile":if cfg!(debug_assertions){"debug"}else{"release"},"target_os":std::env::consts::OS,"target_arch":std::env::consts::ARCH,
        "frame_ms":10,"paced":false,"actual_devices_used":false,"production_enabled":false,"voice_barge_in_enabled":false,
        "raw_audio_saved":false,"synthetic_audio_saved":false,"asr_or_tts_inference_called":false,
        "limitations":["Synthetic adult SAPI speech and fixed linear FIR; not children, a real room, speaker sound or clock drift",
            "Wall-clock processing cost is not capture latency or audible stop latency",
            "Double-talk uses known near-speech fidelity, never ERLE on mixed speech",
            "Reference-gap output is explicitly excluded; reset is not recovery of the lost reference",
            "Null metrics have explicit silent/degenerate reasons; no epsilon produces an infinite quality score",
            "Main-branch Sonora fixes and physical microphone/speaker acceptance remain unverified"]});
    write_report(&output, &report)?;
    println!(
        "{}",
        json!({"report_written":true,"scenario_count":fixture::SCENARIOS.len(),"lag_calibration_usable":calibration.usable,
        "devices_used":false,"quality_pass":null})
    );
    Ok(())
}

fn write_report(output: &std::path::Path, report: &Value) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    // Keep the no-overwrite contract even if another invocation writes meanwhile.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&serde_json::to_vec_pretty(report)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_comparison_control_and_cases_are_all_retained_without_substitution() {
        let cell = fixture::ComparisonCell {
            manifest: json!({"cell_id":"test_hpf_on"}),
            manifest_sha256: "test".into(),
            corpus: fixture::Corpus {
                far: vec![0.1],
                near: vec![0.1],
                provenance: Value::Null,
            },
            high_pass: true,
        };
        let mut attempted = Vec::new();
        let (report, times) = run_comparison_cell(&cell, |mixture, high_pass| {
            assert!(high_pass);
            attempted.push(mixture.scenario.name());
            Err(anyhow!("injected processing failure"))
        });
        assert_eq!(attempted.len(), 7);
        assert_eq!(attempted[0], "near_only");
        assert_eq!(
            attempted.iter().filter(|name| *name == "near_only").count(),
            1
        );
        assert_eq!(report["processing_failed_count"], 7);
        assert_eq!(
            report["lag_calibration"]["invalid_reason"],
            "near_only_processing_failed"
        );
        assert!(times.is_empty());
        let cases = report["cases"].as_array().unwrap();
        for (case, scenario) in cases.iter().zip(fixture::ORDINARY_SCENARIOS) {
            assert_eq!(case["scenario"], scenario.name());
            assert!(case["parameters"]["capture_sha256_f32le"].is_string());
            assert!(case["output_sha256_f32le"].is_null());
        }
    }

    #[test]
    fn unusable_control_keeps_zero_lag_erle_without_claiming_calibrated_scores() {
        let mixture = Mixture {
            scenario: Scenario::Echo(80),
            render: vec![0.1; 1000],
            echo: vec![0.1; 1000],
            near: vec![0.0; 1000],
            capture: vec![0.1; 1000],
            common_gain: 1.0,
        };
        let run = Run {
            output: vec![0.01; 1000],
            valid: vec![true; 1000],
            pair_us: vec![],
            initialization_us: 0.0,
            reset_us: vec![],
            dropped_frames: 0,
        };
        let calibration = metrics::calibrate(&[0.0; 1000], &[0.0; 1000], &[true; 1000]);
        assert!(!calibration.usable);
        let report = report_case(&mixture, &run, &calibration);
        assert!(report["far_only"][0]["fixed_calibrated_lag"].is_null());
        assert!(
            (report["far_only"][0]["zero_lag"]["erle_db"]
                .as_f64()
                .unwrap()
                - 20.0)
                .abs()
                < 1e-5
        );
    }

    #[test]
    fn missing_reference_excludes_exactly_one_frame_and_resets_without_claiming_output()
    -> Result<()> {
        let count = CHANGE_AT + 2 * FRAME_SAMPLES;
        let mixture = Mixture {
            scenario: Scenario::ReferenceGap,
            render: vec![0.0; count],
            echo: vec![0.0; count],
            near: vec![0.0; count],
            capture: vec![0.0; count],
            common_gain: 1.0,
        };
        let result = process(&mixture, false)?;
        ensure!(result.dropped_frames == 1 && result.reset_us.len() == 1);
        ensure!(result.valid.iter().filter(|v| !**v).count() == FRAME_SAMPLES);
        ensure!(result.pair_us.len() == count / FRAME_SAMPLES - 1);
        ensure!(result.output.iter().all(|v| v.is_finite()));
        Ok(())
    }
}

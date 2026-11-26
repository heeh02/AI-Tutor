//! Reproducible, explicitly synthetic speech baseline. No microphone or speaker.
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Instant};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
#[allow(dead_code)]
#[path = "audio_probe/synthetic_asr.rs"]
mod synthetic_asr;

fn main() -> Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let output = std::env::args().nth(2).map(PathBuf::from).ok_or_else(|| {
        anyhow!("usage: speech-corpus PROJECT_ROOT NEW_OUTPUT_DIRECTORY [--whisper]")
    })?;
    ensure!(
        !output.exists(),
        "Use a new output directory to preserve previous evidence"
    );
    let corpus: Value = serde_json::from_slice(&std::fs::read(
        root.join("scripts/speech-lab/education_speech_v1.json"),
    )?)?;
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    let mut options = WhisperContextParameters::default();
    options.use_gpu(false);
    let model_start = Instant::now();
    let model = if std::env::args().any(|arg| arg == "--whisper") {
        Some(WhisperContext::new_with_params(
            root.join("models/ggml-base.bin")
                .to_str()
                .ok_or_else(|| anyhow!("Invalid model path"))?,
            options,
        )?)
    } else {
        None
    };
    let load_ms = model.as_ref().map(|_| model_start.elapsed().as_millis());
    let mut rows = Vec::new();
    for item in corpus["samples"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing corpus samples"))?
    {
        let id = item["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing sample id"))?;
        ensure!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "Invalid sample id"
        );
        let language = item["language"].as_str().unwrap_or("en");
        let text = item["text"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing sample text"))?;
        let path = output.join(format!("{id}.wav"));
        let start = Instant::now();
        let mut samples = synthetic_asr::synthesize(&path, language, text)?;
        let mut row = item.clone();
        row["wav"] = json!(path);
        row["source_type"] = json!("synthetic_sapi_adult_voice");
        row["speaker_output"] = json!(false);
        row["sample_rate"] = json!(16000);
        row["duration_ms"] = json!(samples.len() as f64 / 16.0);
        row["synthesis_ms"] = json!(start.elapsed().as_millis());
        row["sha256"] = json!(format!("{:x}", Sha256::digest(std::fs::read(&path)?)));
        if let Some(model) = &model {
            let start = Instant::now();
            let mut state = model.create_state()?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_n_threads(4);
            params.set_language(Some(language));
            params.set_translate(false);
            params.set_no_context(true);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_print_special(false);
            params.set_initial_prompt(
                "主控、电机、测距、超声波、停车距离、绿灯。control, motor, distance, light.",
            );
            state.full(params, &samples)?;
            let mut transcript = String::new();
            for segment in state.as_iter() {
                if segment.no_speech_probability() < 0.6 {
                    transcript.push_str(segment.to_str()?);
                }
            }
            row["whisper_base_cpu"] = json!({"transcript":transcript.trim(),"full_utterance_inference_ms":start.elapsed().as_millis(),"threads":4,"partial_available":false,"audible_latency_measured":false});
        }
        samples.fill(0.0);
        println!(
            "{}",
            json!({"sample":id,"duration_ms":row["duration_ms"],"whisper":row["whisper_base_cpu"]})
        );
        rows.push(row);
        // Persist progress even when a later model/device call fails.
        std::fs::write(
            output.join("manifest.json"),
            serde_json::to_vec_pretty(
                &json!({"version":1,"source_type":"authored_synthetic_fixture","natural_speech_verified":false,"child_speech_verified":false,"audible_output":false,"whisper_load_ms":load_ms,"limitations":corpus["limitations"],"samples":rows}),
            )?,
        )?;
    }
    Ok(())
}

//! Synthetic typed-48 projection only. Encoding and retrieval are later phases.
mod typed48_common;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use typed48_common::*;

fn save_new(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 4 && args[0] == "--root" && args[2] == "--output",
        "usage: typed48-projection-probe --root ROOT --output NEW_DIRECTORY"
    );
    let root = PathBuf::from(&args[1]).canonicalize()?;
    let output = PathBuf::from(&args[3]);
    ensure!(!output.exists(), "Use a new output directory");
    let frozen = load(&root)?;
    let projected = project(&frozen)?;
    projected.verify_live()?;
    let mut encoded = vec![];
    for input in &projected.inputs {
        serde_json::to_writer(&mut encoded, input)?;
        encoded.push(b'\n');
    }
    let mut projection = projected.summary.clone();
    projection["decisions"] = json!(projected.decisions);
    let report = json!({"status":"projection_completed_no_encoding","method":serde_json::from_str::<Value>(METHOD)?,
        "method_sha256":hash(METHOD.as_bytes()),"probe_sha256":hash(include_bytes!("typed48_projection_probe.rs")),
        "common_sha256":hash(include_bytes!("typed48_common.rs")),"fixture_manifest_sha256":FIXTURE_SHA,
        "original_fixture_files":frozen.files,"baseline42_report_sha256":LEGACY_REPORT_SHA,"baseline234_inputs_sha256":LEGACY_INPUTS_SHA,
        "projection":projection,"encoder_inputs":{"file":"encoder_inputs.jsonl","records":240,"sha256":hash(&encoded)},
        "models_executed":false,"ranking_executed":false,"production_changed":false,"devices_used":false,
        "database":"owned disposable synthetic Store; no user database"});
    fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
    fs::create_dir(&output)?;
    save_new(&output.join("encoder_inputs.jsonl"), &encoded)?;
    save_new(
        &output.join("report.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{{\"status\":\"projection_completed_no_encoding\",\"projected\":48,\"excluded\":12,\"encoder_inputs\":240}}"
    );
    Ok(())
}

#[cfg(test)]
#[path = "typed48_projection_tests.rs"]
mod tests;

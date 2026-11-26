//! Deterministic synthetic mixtures; no device handles and no external recordings.
use crate::echo_cancel::{FRAME_SAMPLES, SAMPLE_RATE};
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const SECONDS: usize = 40; // Each disjoint tape traverses all six source clips.
pub const NEAR_START: usize = 6 * SAMPLE_RATE;
// Fixed from the input WAV/FIR energies only; 9 seconds was a silent interval.
pub const CHANGE_AT: usize = 8 * SAMPLE_RATE;
pub const STOP_TAIL_SAMPLES: usize = 1280 + 432; // 80 ms device delay + 27 ms FIR tail.
pub const TAPS: [(usize, f32); 3] = [(0, 0.55), (192, 0.18), (432, -0.08)];

// These hashes belong to the project's author-written SAPI-to-file corpus.
// The manifest's arbitrary paths/text are never used or included in the report.
const SOURCES: [(&str, &str); 12] = [
    (
        "zh_short",
        "ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33",
    ),
    (
        "en_short",
        "670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f",
    ),
    (
        "zh_concept",
        "4f9ce1ec17a18b88b30ef1a5ade80bd8052e6ddc2e69fbfb190d445b68e2da0f",
    ),
    (
        "en_concept",
        "c20d6cc9419de35f20148763e2438618eaae9cc7baf1593a8f858449871c9437",
    ),
    (
        "zh_correction",
        "5a67a1ddc8cbf38138d765989616d8012d8d0f026ab1f93ba378334e7b138e68",
    ),
    (
        "en_correction",
        "1b371cd1a734c914231c1d3d1c36603b8e15178f637abd62a67bbbda9adc1e82",
    ),
    (
        "zh_hint",
        "fc522dc95540761ed9b413bd783dfa74013243b526284419978d0ac212527e96",
    ),
    (
        "en_hint",
        "6f2b1a9e0f276734394c96322f5ca21830facb914543a8f6661e610f8c752987",
    ),
    (
        "zh_number",
        "8730678022f69650f4d377ed84c50013355468674d6f482d45a423d3871e07e2",
    ),
    (
        "en_number",
        "f6f2844ae27b7496d0fb96ec55122b5c3ae5744f619d60bcd01ee583b49affe7",
    ),
    (
        "mixed_zh",
        "764b9f2b979df772d071e002234dada4025ae2df08e474959a39e807256de55c",
    ),
    (
        "mixed_en",
        "d975317ad8dbf9de29fe223efb758325500f7a684be2766ad85a9002093a92ff",
    ),
];
const FAR_IDS: [usize; 6] = [0, 3, 4, 7, 8, 11];
const NEAR_IDS: [usize; 6] = [1, 2, 5, 6, 9, 10];

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn signal_hash(samples: &[f32]) -> String {
    let mut hash = Sha256::new();
    for sample in samples {
        hash.update(sample.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

pub struct Corpus {
    pub far: Vec<f32>,
    pub near: Vec<f32>,
    pub provenance: Value,
}

pub const PROTOCOL_SHA256: &str =
    "74d6e6f0010f789a6bcea40e31647f883b1c53a6351174850a7535934e784b6e";
const METRICS_SHA256: &str = "b060810a10c2e37f4292cc86a61793f64bd8715f25855e2291eb526a9503d411";
const PROVENANCE_SHA256: &str = "a35bb8e1e2714cd5ed2a58a639d1caddf5ff856d57e949cb664947ab48ebeaa0";
const SIGNAL_HASH_FIELDS: [&str; 4] = [
    "render_sha256_f32le",
    "capture_sha256_f32le",
    "near_sha256_f32le",
    "echo_sha256_f32le",
];

pub struct Comparison {
    pub protocol: Value,
    pub metrics_plan: Value,
    pub cells: Vec<ComparisonCell>,
}

pub struct ComparisonCell {
    pub manifest: Value,
    pub manifest_sha256: String,
    pub corpus: Corpus,
    pub high_pass: bool,
}

fn read_verified(path: &Path, expected_hash: &str, limit: u64) -> Result<Vec<u8>> {
    ensure!(
        std::fs::metadata(path)?.len() <= limit,
        "Frozen fixture file is too large"
    );
    let bytes = std::fs::read(path)?;
    ensure!(
        sha256(&bytes) == expected_hash,
        "Frozen fixture hash mismatch: {}",
        path.display()
    );
    Ok(bytes)
}

fn verified_json(path: &Path, expected_hash: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&read_verified(
        path,
        expected_hash,
        128 * 1024,
    )?)?)
}

fn frozen_tape(directory: &Path, entries: &Value) -> Result<Vec<f32>> {
    let entries = entries
        .as_array()
        .ok_or_else(|| anyhow!("Missing frozen sources"))?;
    ensure!(
        entries.len() == 6,
        "A frozen tape must contain all six sources"
    );
    let mut tape = Vec::new();
    for entry in entries {
        // Paths and hashes come only from the hash-pinned manifest, never an arbitrary corpus.
        let wav = entry["wav"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing frozen WAV path"))?;
        let expected = entry["sha256"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing frozen WAV hash"))?;
        let bytes = read_verified(&directory.join(wav), expected, 2 * 1024 * 1024)?;
        tape.extend(pcm16(&bytes)?);
        tape.extend([0.0; 3200]);
    }
    Ok(tape)
}

/// Verify every manifest and source before any AEC instance is created. This
/// intentionally accepts one frozen experiment, not user-selected recordings.
pub fn load_comparison(protocol_path: &Path) -> Result<Comparison> {
    let protocol = verified_json(protocol_path, PROTOCOL_SHA256)?;
    let directory = protocol_path.parent().unwrap_or_else(|| Path::new("."));
    let metrics_plan = verified_json(&directory.join("metrics-plan.json"), METRICS_SHA256)?;
    verified_json(&directory.join("input-provenance.json"), PROVENANCE_SHA256)?;
    let entries = protocol["all_cells"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing frozen cells"))?;
    ensure!(entries.len() == 4, "The comparison requires all four cells");
    let mut cells = Vec::new();
    for entry in entries {
        let relative = entry["manifest"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing cell manifest"))?;
        let expected = entry["sha256"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing cell hash"))?;
        let manifest = verified_json(&directory.join(relative), expected)?;
        let high_pass = manifest["algorithm"]["enforce_high_pass_filtering"]
            .as_bool()
            .ok_or_else(|| anyhow!("Missing high-pass setting"))?;
        ensure!(
            manifest["cell_id"] == entry["cell_id"]
                && manifest["parameters"] == protocol["parameters"],
            "Frozen cell does not match protocol"
        );
        let corpus = Corpus {
            far: frozen_tape(directory, &manifest["far_sources"])?,
            near: frozen_tape(directory, &manifest["near_sources"])?,
            provenance: json!({"source_type":"frozen_synthetic_comparison",
                "far_sources":manifest["far_sources"],"near_sources":manifest["near_sources"],
                "human_speech":false,"independent_new_text_heldout":false,
                "waveform_interpretation":manifest["waveform_interpretation"]}),
        };
        cells.push(ComparisonCell {
            manifest,
            manifest_sha256: expected.into(),
            corpus,
            high_pass,
        });
    }
    let comparison = Comparison {
        protocol,
        metrics_plan,
        cells,
    };
    // A source or mixture implementation drift must fail before scoring any cell.
    for cell in &comparison.cells {
        for scenario in ORDINARY_SCENARIOS {
            cell.verify_mixture(&make(&cell.corpus, scenario))?;
        }
    }
    Ok(comparison)
}

impl ComparisonCell {
    pub fn verify_mixture(&self, mixture: &Mixture) -> Result<()> {
        let expected = self.manifest["expected_input_signals"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing frozen signal hashes"))?;
        ensure!(
            expected.len() == ORDINARY_SCENARIOS.len(),
            "Frozen scenario count changed"
        );
        let name = mixture.scenario.name();
        let matching: Vec<_> = expected
            .iter()
            .filter(|row| row["scenario"] == name)
            .collect();
        ensure!(
            matching.len() == 1,
            "Missing or duplicate frozen scenario {name}"
        );
        let actual = mixture.parameters();
        for key in SIGNAL_HASH_FIELDS {
            ensure!(
                actual[key] == matching[0][key],
                "Frozen input mismatch for {name}: {key}"
            );
        }
        Ok(())
    }
}

pub fn load(manifest_path: &Path) -> Result<Corpus> {
    ensure!(
        std::fs::metadata(manifest_path)?.len() <= 128 * 1024,
        "Synthetic manifest is too large"
    );
    let bytes = std::fs::read(manifest_path)?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    let entries = manifest["samples"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing synthetic samples"))?;
    ensure!(
        entries.len() == SOURCES.len() && manifest["audible_output"] == false,
        "Not the fixed SAPI corpus"
    );
    let directory = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let mut samples = Vec::new();
    let mut provenance = Vec::new();
    for (id, expected_hash) in SOURCES {
        let matching: Vec<_> = entries.iter().filter(|v| v["id"] == id).collect();
        ensure!(
            matching.len() == 1,
            "Missing or duplicated synthetic source {id}"
        );
        let entry = matching[0];
        ensure!(
            entry["source_type"] == "synthetic_sapi_adult_voice"
                && entry["speaker_output"] == false
                && entry["sample_rate"] == SAMPLE_RATE
                && entry["sha256"] == expected_hash,
            "Synthetic source provenance mismatch: {id}"
        );
        let path = directory.join(format!("{id}.wav"));
        ensure!(
            std::fs::metadata(&path)?.len() <= 2 * 1024 * 1024,
            "Synthetic WAV is too large"
        );
        let wav = std::fs::read(&path).with_context(|| format!("Read fixed synthetic WAV {id}"))?;
        ensure!(
            sha256(&wav) == expected_hash,
            "Synthetic WAV hash mismatch: {id}"
        );
        let pcm = pcm16(&wav)?;
        provenance.push(json!({"id":id,"sha256":expected_hash,"sample_count":pcm.len()}));
        samples.push(pcm);
    }
    let tape = |indices: &[usize]| {
        let mut result = Vec::new();
        for &index in indices {
            result.extend(&samples[index]);
            result.extend([0.0; 3200]);
        }
        result
    };
    Ok(Corpus {
        far: tape(&FAR_IDS),
        near: tape(&NEAR_IDS),
        provenance: json!({"manifest_sha256":sha256(&bytes),"samples":provenance,
            "far_ids":FAR_IDS.map(|i| SOURCES[i].0),"near_ids":NEAR_IDS.map(|i| SOURCES[i].0),
            "source_type":"fixed_synthetic_sapi_adult_voice","inter_clip_silence_ms":200}),
    })
}

fn pcm16(wav: &[u8]) -> Result<Vec<f32>> {
    ensure!(
        wav.len() >= 12 && &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE",
        "Invalid WAV header"
    );
    let riff_end = u32::from_le_bytes(wav[4..8].try_into()?) as usize + 8;
    ensure!(riff_end == wav.len(), "Truncated or trailing RIFF data");
    let mut offset = 12;
    let mut format = None;
    let mut data = None;
    while offset + 8 <= riff_end {
        let length = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(length)
            .ok_or_else(|| anyhow!("WAV chunk overflow"))?;
        ensure!(end <= riff_end, "Truncated WAV chunk");
        match &wav[offset..offset + 4] {
            b"fmt " => {
                ensure!(format.is_none() && length >= 16, "Invalid WAV format chunk");
                format = Some((
                    u16::from_le_bytes(wav[start..start + 2].try_into()?),
                    u16::from_le_bytes(wav[start + 2..start + 4].try_into()?),
                    u32::from_le_bytes(wav[start + 4..start + 8].try_into()?),
                    u16::from_le_bytes(wav[start + 12..start + 14].try_into()?),
                    u16::from_le_bytes(wav[start + 14..start + 16].try_into()?),
                ));
            }
            b"data" => {
                ensure!(
                    data.is_none() && length > 0 && length % 2 == 0,
                    "Invalid WAV data chunk"
                );
                data = Some((start, end));
            }
            _ => {}
        }
        offset = end + length % 2;
    }
    ensure!(
        offset == riff_end && format == Some((1, 1, 16_000, 2, 16)),
        "WAV must be PCM16 mono 16 kHz"
    );
    let (start, end) = data.ok_or_else(|| anyhow!("WAV data missing"))?;
    Ok(wav[start..end]
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
        .collect())
}

#[derive(Clone, Copy, Debug)]
pub enum Scenario {
    Echo(u32),
    Near,
    Double(i32),
    Stop,
    DelayJump,
    ReferenceGap,
}
pub const SCENARIOS: [Scenario; 10] = [
    Scenario::Echo(20),
    Scenario::Echo(80),
    Scenario::Echo(160),
    Scenario::Near,
    Scenario::Double(-6),
    Scenario::Double(0),
    Scenario::Double(6),
    Scenario::Stop,
    Scenario::DelayJump,
    Scenario::ReferenceGap,
];
pub const ORDINARY_SCENARIOS: [Scenario; 7] = [
    Scenario::Echo(20),
    Scenario::Echo(80),
    Scenario::Echo(160),
    Scenario::Near,
    Scenario::Double(-6),
    Scenario::Double(0),
    Scenario::Double(6),
];

impl Scenario {
    pub fn name(self) -> String {
        match self {
            Self::Echo(delay) => format!("echo_only_{delay}ms"),
            Self::Near => "near_only".into(),
            Self::Double(ratio) => format!("double_talk_ser_{ratio}db"),
            Self::Stop => "render_stop_tail".into(),
            Self::DelayJump => "device_delay_80_to_160ms".into(),
            Self::ReferenceGap => "reference_gap_10ms_then_reset".into(),
        }
    }
    pub fn delay_ms(self, sample: usize) -> u32 {
        match self {
            Self::Echo(delay) => delay,
            Self::Near => 0,
            Self::DelayJump if sample >= CHANGE_AT => 160,
            _ => 80,
        }
    }
    pub fn near_present(self) -> bool {
        matches!(self, Self::Near | Self::Double(_))
    }
}

pub struct Mixture {
    pub scenario: Scenario,
    pub render: Vec<f32>,
    pub echo: Vec<f32>,
    pub near: Vec<f32>,
    pub capture: Vec<f32>,
    pub common_gain: f32,
}
fn rms(values: &[f32]) -> f64 {
    (values.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / values.len().max(1) as f64).sqrt()
}
fn normalize(values: &mut [f32], target: f64) {
    let gain = target / rms(values).max(1e-12);
    for v in values {
        *v *= gain as f32;
    }
}

pub fn make(corpus: &Corpus, scenario: Scenario) -> Mixture {
    let count = SECONDS * SAMPLE_RATE;
    // Identical render timeline across scenarios: differences are the named variable.
    let mut render: Vec<_> = (0..count)
        .map(|i| corpus.far[i % corpus.far.len()])
        .collect();
    normalize(&mut render, 0.10);
    let unmodified_render_peak = render.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    if matches!(scenario, Scenario::Near) {
        render.fill(0.0);
    }
    if matches!(scenario, Scenario::Stop) {
        render[CHANGE_AT..].fill(0.0);
    }
    let echo: Vec<f32> = (0..count)
        .map(|i| {
            TAPS.iter()
                .map(|(offset, gain)| {
                    i.checked_sub(scenario.delay_ms(i) as usize * 16 + offset)
                        .map_or(0.0, |at| render[at] * gain)
                })
                .sum()
        })
        .collect();
    let mut near = vec![0.0; count];
    if scenario.near_present() {
        let start = if matches!(scenario, Scenario::Near) {
            0
        } else {
            NEAR_START
        };
        for i in start..count {
            near[i] = corpus.near[i % corpus.near.len()];
        }
        let target = match scenario {
            Scenario::Double(db) => rms(&echo[start..]) * 10f64.powf(db as f64 / 20.0),
            _ => 0.08,
        };
        normalize(&mut near[start..], target);
    }
    let mut capture: Vec<_> = echo.iter().zip(&near).map(|(e, n)| e + n).collect();
    let mut peak = render
        .iter()
        .chain(&capture)
        .map(|v| v.abs())
        .fold(0.0f32, f32::max);
    if matches!(scenario, Scenario::Stop) {
        // Stopping early must not amplify the prefix by removing a later peak.
        peak = peak.max(unmodified_render_peak);
    }
    let common_gain = (0.8 / peak.max(0.8)).min(1.0);
    let scale = |values: Vec<f32>| values.into_iter().map(|v| v * common_gain).collect();
    for v in &mut capture {
        *v *= common_gain;
    }
    Mixture {
        scenario,
        render: scale(render),
        echo: scale(echo),
        near: scale(near),
        capture,
        common_gain,
    }
}

impl Mixture {
    /// Called on the hash-verified real corpus before running any AEC. These
    /// checks concern the stimulus, never the algorithm's output or score.
    pub fn validate_event_input(&self) -> Result<()> {
        if !matches!(
            self.scenario,
            Scenario::Stop | Scenario::DelayJump | Scenario::ReferenceGap
        ) {
            return Ok(());
        }
        ensure!(
            rms(&self.render[CHANGE_AT - FRAME_SAMPLES..CHANGE_AT]) > 0.01,
            "Event fixture lacks active render immediately before the fixed event"
        );
        match self.scenario {
            Scenario::Stop => {
                ensure!(
                    rms(&self.echo[CHANGE_AT..CHANGE_AT + STOP_TAIL_SAMPLES]) > 0.01,
                    "Stop fixture does not exercise a nonzero echo tail"
                );
                ensure!(
                    self.render[CHANGE_AT..].iter().all(|v| *v == 0.0)
                        && self.echo[CHANGE_AT + STOP_TAIL_SAMPLES..]
                            .iter()
                            .all(|v| *v == 0.0),
                    "Stop fixture exceeds its declared render or FIR tail boundary"
                );
            }
            Scenario::DelayJump => ensure!(
                rms(&self.echo[CHANGE_AT..CHANGE_AT + SAMPLE_RATE]) > 0.01,
                "Delay-change fixture lacks active echo in its first-second window"
            ),
            Scenario::ReferenceGap => ensure!(
                rms(&self.render[CHANGE_AT..CHANGE_AT + FRAME_SAMPLES]) > 0.01
                    && rms(&self.capture[CHANGE_AT..CHANGE_AT + FRAME_SAMPLES]) > 0.01,
                "Missing-reference fixture must discard an active reference with real echo"
            ),
            _ => unreachable!(),
        }
        Ok(())
    }

    pub fn parameters(&self) -> Value {
        let event = matches!(self.scenario,Scenario::Stop|Scenario::DelayJump|Scenario::ReferenceGap).then(|| {
            let echo_end = if matches!(self.scenario,Scenario::Stop) {CHANGE_AT+STOP_TAIL_SAMPLES}
                else if matches!(self.scenario,Scenario::ReferenceGap) {CHANGE_AT+FRAME_SAMPLES}
                else {CHANGE_AT+SAMPLE_RATE};
            json!({"at_sample":CHANGE_AT,"at_ms":CHANGE_AT/16,
                "selection":"fixed_8000ms_verified_using_input_and_known_FIR_only",
                "pre_render_window_samples":[CHANGE_AT-FRAME_SAMPLES,CHANGE_AT],
                "pre_render_rms":rms(&self.render[CHANGE_AT-FRAME_SAMPLES..CHANGE_AT]),
                "post_render_10ms_rms":rms(&self.render[CHANGE_AT..CHANGE_AT+FRAME_SAMPLES]),
                "echo_window_samples":[CHANGE_AT,echo_end],"echo_window_ms":[CHANGE_AT/16,echo_end/16],
                "echo_window_rms":rms(&self.echo[CHANGE_AT..echo_end]),
                "capture_first_10ms_rms":rms(&self.capture[CHANGE_AT..CHANGE_AT+FRAME_SAMPLES]),
                "minimum_required_event_rms":0.01})
        });
        json!({"duration_ms":SECONDS*1000,"sample_rate":SAMPLE_RATE,"frame_samples":FRAME_SAMPLES,
            "event":event,
            "device_delay_ms_initial":self.scenario.delay_ms(0),"device_delay_ms_final":self.scenario.delay_ms(CHANGE_AT),
            "fir_taps":TAPS.map(|(samples,gain)| json!({"offset_samples":samples,"gain":gain})),
            "near_start_ms":if matches!(self.scenario,Scenario::Double(_)){Some(6000)}else{None},
            "common_gain_to_avoid_clipping":self.common_gain,
            "render_sha256_f32le":signal_hash(&self.render),"capture_sha256_f32le":signal_hash(&self.capture),
            "near_sha256_f32le":signal_hash(&self.near),"echo_sha256_f32le":signal_hash(&self.echo)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_comparison_rejects_protocol_changes_before_loading_any_audio() -> Result<()> {
        let path = std::env::temp_dir().join(format!("aec-protocol-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"{\"all_cells\":[]}")?;
        let result = load_comparison(&path);
        std::fs::remove_file(path)?;
        assert!(result.err().unwrap().to_string().contains("hash mismatch"));
        Ok(())
    }

    #[test]
    fn frozen_signal_check_rejects_changed_capture_even_when_sources_are_unchanged() -> Result<()> {
        let mut mixture = Mixture {
            scenario: Scenario::Near,
            render: vec![0.0; 160],
            echo: vec![0.0; 160],
            near: vec![0.1; 160],
            capture: vec![0.1; 160],
            common_gain: 1.0,
        };
        let mut expected: Vec<_> = ORDINARY_SCENARIOS
            .iter()
            .map(|case| json!({"scenario":case.name()}))
            .collect();
        let mut near = mixture.parameters();
        near["scenario"] = json!("near_only");
        expected[3] = near;
        let cell = ComparisonCell {
            manifest: json!({"expected_input_signals":expected}),
            manifest_sha256: "test".into(),
            corpus: toy_corpus(),
            high_pass: false,
        };
        cell.verify_mixture(&mixture)?;
        mixture.capture[0] += 0.01;
        assert!(cell.verify_mixture(&mixture).is_err());
        Ok(())
    }

    #[test]
    #[ignore = "requires the frozen 18-WAV preregistration bundle; no AEC or device execution"]
    fn fixed_comparison_sources_and_all_28_input_hashes_match_preregistration() -> Result<()> {
        let comparison = load_comparison(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../runtime/goal-baselines/20261104/aec-2x2-preregistered/protocol.json"),
        )?;
        assert_eq!(comparison.cells.len(), 4);
        for pair in comparison.cells.chunks_exact(2) {
            assert!(!pair[0].high_pass && pair[1].high_pass);
            assert_eq!(pair[0].corpus.far, pair[1].corpus.far);
            assert_eq!(pair[0].corpus.near, pair[1].corpus.near);
        }
        assert_eq!(
            comparison.cells[0].corpus.far,
            comparison.cells[2].corpus.far
        );
        assert_ne!(
            comparison.cells[0].corpus.near,
            comparison.cells[2].corpus.near
        );
        Ok(())
    }

    fn toy_corpus() -> Corpus {
        Corpus {
            far: (0..4000).map(|i| (i as f32 * 0.10).sin() * 0.1).collect(),
            near: (0..5000).map(|i| (i as f32 * 0.17).sin() * 0.1).collect(),
            provenance: Value::Null,
        }
    }
    #[test]
    fn mixtures_preserve_components_and_named_change_boundaries() {
        let corpus = toy_corpus();
        for scenario in SCENARIOS {
            let mixture = make(&corpus, scenario);
            mixture.validate_event_input().unwrap();
            assert_eq!(mixture.capture.len(), SECONDS * SAMPLE_RATE);
            assert!(mixture.capture.iter().all(|v| v.abs() <= 0.800001));
            assert!(
                mixture
                    .capture
                    .iter()
                    .zip(&mixture.echo)
                    .zip(&mixture.near)
                    .all(|((m, e), n)| (m - e - n).abs() < 1e-6)
            );
        }
        let stopped = make(&corpus, Scenario::Stop);
        assert!(stopped.render[CHANGE_AT..].iter().all(|v| *v == 0.0));
        assert!(
            stopped.echo[CHANGE_AT..CHANGE_AT + 1280]
                .iter()
                .any(|v| v.abs() > 0.001)
        );
        assert!(
            stopped.echo[CHANGE_AT + 1280 + 432..]
                .iter()
                .all(|v| *v == 0.0)
        );
        let double = make(&corpus, Scenario::Double(6));
        assert!(double.near[..NEAR_START].iter().all(|v| *v == 0.0));
        assert!(
            (20.0 * (rms(&double.near[NEAR_START..]) / rms(&double.echo[NEAR_START..])).log10()
                - 6.0)
                .abs()
                < 1e-4
        );
    }
    #[test]
    fn event_checks_reject_a_silent_tail_instead_of_claiming_a_successful_stop() {
        let mut stopped = make(&toy_corpus(), Scenario::Stop);
        stopped.validate_event_input().unwrap();
        stopped.echo[CHANGE_AT..].fill(0.0);
        assert!(stopped.validate_event_input().is_err());
    }
    #[test]
    #[ignore = "requires the hash-verified generated SAPI corpus; the real probe enforces these checks on every run"]
    fn fixed_sapi_event_stimuli_are_active_and_share_the_unchanged_prefix() -> Result<()> {
        let corpus = load(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../runtime/goal-baselines/20261103/sapi-corpus/manifest.json"),
        )?;
        let reference = make(&corpus, Scenario::Echo(80));
        for scenario in [Scenario::Stop, Scenario::DelayJump, Scenario::ReferenceGap] {
            let mixture = make(&corpus, scenario);
            mixture.validate_event_input()?;
            ensure!(
                mixture.render[..CHANGE_AT] == reference.render[..CHANGE_AT]
                    && mixture.capture[..CHANGE_AT] == reference.capture[..CHANGE_AT],
                "Event case changed the pre-event reference or capture"
            );
        }
        Ok(())
    }
    #[test]
    fn wav_parser_rejects_truncation_and_wrong_format() {
        let mut wav = b"RIFF".to_vec();
        wav.extend(38u32.to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(32000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(2u32.to_le_bytes());
        wav.extend(16384i16.to_le_bytes());
        assert_eq!(pcm16(&wav).unwrap(), [0.5]);
        assert!(pcm16(&wav[..wav.len() - 1]).is_err());
        wav[22] = 2;
        assert!(pcm16(&wav).is_err());
    }
}

//! Hash-pinned adult synthetic sources and deterministic challenge composition.
use anyhow::{Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

pub const METHOD_SHA256: &str = "fc66389737aa85015094548e43c793e2398893c3a5f0e0c44e142f4e5cc0d4fe";
const RATE: usize = 16_000;

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn signal_hash(samples: &[f32]) -> String {
    let mut digest = Sha256::new();
    for sample in samples {
        digest.update(sample.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

#[derive(Clone, Copy)]
pub enum Kind {
    Clean,
    Pause2000,
    Pause4000Risk,
    Continuous,
    Silence,
    LowNoise,
    LowHum,
    DeviceError,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Pause2000 => "pause_2000",
            Self::Pause4000Risk => "pause_4000_risk",
            Self::Continuous => "continuous",
            Self::Silence => "silence",
            Self::LowNoise => "low_noise",
            Self::LowHum => "low_hum",
            Self::DeviceError => "device_error",
        }
    }
}

pub struct Case {
    pub id: String,
    pub kind: Kind,
    pub input: Vec<f32>,
    /// Exact nonzero source support intervals, not manual word annotations.
    pub supports: Vec<[usize; 2]>,
}

impl Case {
    fn new(id: &str, kind: Kind, leading_ms: usize) -> Self {
        Self {
            id: id.into(),
            kind,
            input: vec![0.0; leading_ms * RATE / 1000],
            supports: Vec::new(),
        }
    }

    fn source(&mut self, samples: &[f32]) {
        let start = self.input.len();
        self.input.extend_from_slice(samples);
        self.supports.push([start, self.input.len()]);
    }

    fn silence(&mut self, ms: usize) {
        self.input.resize(self.input.len() + ms * RATE / 1000, 0.0);
    }

    fn align(&mut self) {
        self.input.resize(self.input.len().div_ceil(160) * 160, 0.0);
    }
}

fn pcm16(bytes: &[u8]) -> Result<Vec<f32>> {
    ensure!(bytes.len() >= 12, "Truncated WAV");
    ensure!(
        &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "Expected RIFF WAV"
    );
    ensure!(
        u32::from_le_bytes(bytes[4..8].try_into()?) as usize + 8 == bytes.len(),
        "WAV size mismatch"
    );
    let mut at = 12;
    let mut format = None;
    let mut data = None;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        let start = at + 8;
        let end = start
            .checked_add(len)
            .ok_or_else(|| anyhow::anyhow!("WAV size overflow"))?;
        ensure!(end <= bytes.len(), "Truncated WAV chunk");
        match &bytes[at..at + 4] {
            b"fmt " => {
                ensure!(
                    format.is_none() && len >= 16,
                    "Duplicate or short WAV format"
                );
                format = Some((
                    u16::from_le_bytes(bytes[start..start + 2].try_into()?),
                    u16::from_le_bytes(bytes[start + 2..start + 4].try_into()?),
                    u32::from_le_bytes(bytes[start + 4..start + 8].try_into()?),
                    u16::from_le_bytes(bytes[start + 12..start + 14].try_into()?),
                    u16::from_le_bytes(bytes[start + 14..start + 16].try_into()?),
                ));
            }
            b"data" => {
                ensure!(data.is_none() && len % 2 == 0, "Duplicate or odd PCM data");
                data = Some(&bytes[start..end]);
            }
            _ => {}
        }
        at = end + (len % 2);
    }
    ensure!(at == bytes.len(), "Incomplete WAV chunk header");
    ensure!(
        format == Some((1, 1, RATE as u32, 2, 16)),
        "Expected mono 16k PCM16"
    );
    let data = data.ok_or_else(|| anyhow::anyhow!("Missing PCM data"))?;
    let samples: Vec<f32> = data
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
        .collect();
    // Only zero-valued outer padding is removed from the derived case. No
    // energy threshold is selected by examining VAD outcomes.
    let first = samples
        .iter()
        .position(|v| *v != 0.0)
        .ok_or_else(|| anyhow::anyhow!("Silent speech fixture"))?;
    let end = samples.iter().rposition(|v| *v != 0.0).unwrap() + 1;
    Ok(samples[first..end].to_vec())
}

pub fn load(directory: &Path, supplied_sha256: &str) -> Result<(Value, Vec<Case>)> {
    ensure!(
        supplied_sha256 == METHOD_SHA256,
        "Explicit frozen method SHA is required"
    );
    let method = std::fs::read(directory.join("method.json"))?;
    ensure!(
        hash(&method) == METHOD_SHA256,
        "Frozen endpoint method changed"
    );
    let method: Value = serde_json::from_slice(&method)?;
    let entries = method["sources"].as_array().unwrap(); // Hash-pinned schema.
    let mut sources = Vec::new();
    for source in entries {
        let id = source["id"].as_str().unwrap();
        let path = directory.join(source["wav"].as_str().unwrap());
        ensure!(
            std::fs::metadata(&path)?.len() <= 2_000_000,
            "Speech fixture too large"
        );
        let bytes = std::fs::read(path)?;
        ensure!(
            hash(&bytes) == source["sha256"].as_str().unwrap(),
            "Frozen WAV changed: {id}"
        );
        sources.push((id, pcm16(&bytes)?));
    }
    let mut cases = Vec::new();
    for (id, source) in &sources {
        let mut case = Case::new(id, Kind::Clean, 500);
        case.source(source);
        case.silence(4000);
        case.align();
        cases.push(case);
    }
    for (first, second, pair) in [(0, 1, "zh_en"), (1, 0, "en_zh")] {
        for (pause, kind) in [(2000, Kind::Pause2000), (4000, Kind::Pause4000Risk)] {
            let mut case = Case::new(&format!("pause_{pause}_{pair}"), kind, 500);
            case.source(&sources[first].1);
            case.silence(pause);
            case.source(&sources[second].1);
            case.silence(4000);
            case.align();
            cases.push(case);
        }
    }
    for (index, language) in [(0, "zh"), (1, "en")] {
        let mut case = Case::new(&format!("continuous_{language}"), Kind::Continuous, 500);
        let end = 21 * RATE + RATE / 2;
        while case.input.len() < end {
            let remaining = end - case.input.len();
            let source = &sources[index].1;
            case.source(&source[..source.len().min(remaining)]);
            if case.input.len() < end {
                case.input
                    .resize((case.input.len() + RATE / 10).min(end), 0.0);
            }
        }
        case.align();
        cases.push(case);
    }
    cases.push(Case::new("silence", Kind::Silence, 11_000));
    let mut noise = Case::new("low_noise", Kind::LowNoise, 11_000);
    let mut seed = 0x51a7_2026_u32;
    for sample in &mut noise.input {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        *sample = (((seed >> 16) as f32 / 65535.0) * 2.0 - 1.0) * 0.002;
    }
    cases.push(noise);
    let mut hum = Case::new("low_hum", Kind::LowHum, 11_000);
    for (i, sample) in hum.input.iter_mut().enumerate() {
        *sample = (std::f64::consts::TAU * 100.0 * i as f64 / RATE as f64).sin() as f32 * 0.002;
    }
    cases.push(hum);
    cases.push(Case::new("device_error", Kind::DeviceError, 2000));
    Ok((method, cases))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_source_hashes_and_all_challenge_rows_are_required() {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/voice_endpoint_v1");
        let (_, cases) = load(&directory, METHOD_SHA256).unwrap();
        assert_eq!(cases.len(), 14);
        assert!(cases.iter().all(|case| case.input.len() % 160 == 0));
        assert_eq!(
            cases
                .iter()
                .filter(|case| matches!(case.kind, Kind::Pause4000Risk))
                .count(),
            2
        );
        for case in cases
            .iter()
            .filter(|case| matches!(case.kind, Kind::Pause2000))
        {
            assert_eq!(case.supports[1][0] - case.supports[0][1], 2 * RATE);
        }
        assert!(load(&directory, "unfrozen").is_err());
    }

    #[test]
    fn fixed_wav_rejects_truncated_or_wrong_rate_input() {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/voice_endpoint_v1/audio");
        let bytes = std::fs::read(directory.join("zh_short.wav")).unwrap();
        assert!(pcm16(&bytes[..bytes.len() - 1]).is_err());
        let mut wrong_rate = bytes;
        wrong_rate[24..28].copy_from_slice(&48000_u32.to_le_bytes());
        assert!(pcm16(&wrong_rate).is_err());
    }
}

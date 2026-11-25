//! Bounded synthetic AEC/VAD comparison. No devices, model services, or saved PCM.
use super::{
    barge_in::{BargeInOnset, ProcessedFrame},
    echo_cancel::{EchoCanceller16k, FRAME_SAMPLES, Frame, SAMPLE_RATE},
    render_reference::ReferenceMode,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{EchoCanceller, TransparentModeType},
};
use sonora_agc2::vad_wrapper::VoiceActivityDetectorWrapper;
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

const EN_HASH: &str = "670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f";
const ZH_HASH: &str = "ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33";
const FRAMES: usize = 1000;

fn fixed_sapi(id: &str, hash: &str) -> Result<Vec<f32>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/goal-baselines/20261103/sapi-corpus")
        .join(format!("{id}.wav"));
    let bytes = std::fs::read(path).context("fixed author-written SAPI WAV required")?;
    ensure!(format!("{:x}", Sha256::digest(&bytes)) == hash, "fixture SHA mismatch");
    ensure!(bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE");
    let (mut at, mut valid_format, mut pcm) = (12, false, None);
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        let end = at + 8 + len;
        ensure!(end <= bytes.len());
        match &bytes[at..at + 4] {
            b"fmt " => {
                let f = &bytes[at + 8..end];
                ensure!(f.len() >= 16);
                valid_format = u16::from_le_bytes(f[..2].try_into()?) == 1
                    && u16::from_le_bytes(f[2..4].try_into()?) == 1
                    && u32::from_le_bytes(f[4..8].try_into()?) == 16000
                    && u16::from_le_bytes(f[14..16].try_into()?) == 16;
            }
            b"data" => {
                ensure!(pcm.is_none() && len % 2 == 0);
                pcm = Some(&bytes[at + 8..end]);
            }
            _ => {}
        }
        at = end + (len % 2);
    }
    ensure!(valid_format);
    Ok(pcm
        .context("missing PCM data")?
        .chunks_exact(2)
        .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0)
        .collect())
}

#[derive(Clone, Copy)]
enum Profile {
    Production,
    LegacyOffMetrics,
    OfficialDefaultMetrics,
    HmmOnMetrics,
}
impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Production => "production_legacy_hp_off_unmodified",
            Self::LegacyOffMetrics => "legacy_hp_off_with_echo_detector",
            Self::OfficialDefaultMetrics => "official_default_legacy_hp_on_with_echo_detector",
            Self::HmmOnMetrics => "hmm_hp_on_with_echo_detector",
        }
    }
}
enum Processor {
    Production(EchoCanceller16k),
    Metrics(AudioProcessing),
}
impl Processor {
    fn new(profile: Profile) -> Self {
        if matches!(profile, Profile::Production) {
            return Self::Production(EchoCanceller16k::new());
        }
        let echo = match profile {
            Profile::LegacyOffMetrics => {
                EchoCanceller { enforce_high_pass_filtering: false, transparent_mode: TransparentModeType::Legacy }
            }
            Profile::HmmOnMetrics => {
                EchoCanceller { enforce_high_pass_filtering: true, transparent_mode: TransparentModeType::Hmm }
            }
            _ => EchoCanceller::default(),
        };
        Self::Metrics(
            AudioProcessing::builder()
                .config(Config { echo_canceller: Some(echo), ..Default::default() })
                .capture_config(StreamConfig::new(SAMPLE_RATE as u32, 1))
                .render_config(StreamConfig::new(SAMPLE_RATE as u32, 1))
                .echo_detector(true)
                .build(),
        )
    }
    fn process(&mut self, render: &Frame, capture: &Frame, delay_ms: i32) -> Result<Frame> {
        match self {
            Self::Production(p) => p.process(render, capture, delay_ms),
            Self::Metrics(p) => {
                let mut scratch = [0.0; FRAME_SAMPLES];
                let mut output = [0.0; FRAME_SAMPLES];
                p.process_render_f32(&[render], &mut [&mut scratch])?;
                p.set_stream_delay_ms(delay_ms)?;
                p.process_capture_f32(&[capture], &mut [&mut output])?;
                Ok(output)
            }
        }
    }
    fn metrics(&self) -> Value {
        match self {
            Self::Production(_) => Value::Null,
            Self::Metrics(p) => {
                let s = p.statistics();
                json!({"residual_echo_likelihood": s.residual_echo_likelihood,
                    "residual_echo_likelihood_recent_max": s.residual_echo_likelihood_recent_max,
                    "echo_return_loss_db": s.echo_return_loss,
                    "echo_return_loss_enhancement_db": s.echo_return_loss_enhancement,
                    "estimated_delay_ms": s.delay_ms})
            }
        }
    }
}

#[derive(Default)]
struct Energy {
    input: f64,
    output: f64,
    samples: usize,
}
impl Energy {
    fn add(&mut self, input: &Frame, output: &Frame) {
        self.input += input.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
        self.output += output.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
        self.samples += FRAME_SAMPLES;
    }
    fn value(&self) -> Value {
        let attenuation = (self.input > 0.0 && self.output > 0.0).then(|| 10.0 * (self.input / self.output).log10());
        json!({"samples": self.samples, "input_sum_squares": self.input,
            "output_sum_squares": self.output, "attenuation_db": attenuation})
    }
}

fn run(
    profile: Profile,
    far_present: bool,
    near_frame: Option<usize>,
    delays: (usize, i32),
    speech: (&[f32], &[f32]),
    monitor_start: usize,
) -> Result<Value> {
    let (wave_ms, api_ms) = delays;
    let (zh, en) = speech;
    let mut processor = Processor::new(profile);
    let mut detector: Option<BargeInOnset> = None;
    // Observational VAD trace only; production observe() still decides candidates.
    let mut trace_vad: Option<VoiceActivityDetectorWrapper> = None;
    let (mut trace_votes, mut first) = (0_u32, None);
    let mut monitored_energy = Energy::default();
    let (mut total, mut pre_near, mut post_near) = (Energy::default(), Energy::default(), Energy::default());
    let (mut snapshots, mut around_start) = (Vec::new(), Vec::new());
    let at = Instant::now();
    let far = |sample: usize| zh.get(sample % (zh.len() + 3200)).copied().unwrap_or(0.0) * 0.6;
    let delay = wave_ms * SAMPLE_RATE / 1000;
    for index in 0..FRAMES + monitor_start {
        let render: Frame = std::array::from_fn(|i| if far_present { far(index * 160 + i) } else { 0.0 });
        let capture: Frame = std::array::from_fn(|i| {
            let sample = index * 160 + i;
            let echo = if far_present && sample >= delay { 0.4 * far(sample - delay) } else { 0.0 };
            let near = near_frame
                .filter(|onset| index >= *onset)
                .and_then(|onset| en.get((index - onset) * 160 + i))
                .copied()
                .unwrap_or(0.0);
            echo + near
        });
        ensure!(capture.iter().all(|v| v.is_finite() && v.abs() <= 1.0), "synthetic mix clipped");
        let output = processor.process(&render, &capture, api_ms)?;
        ensure!(output.iter().all(|v| v.is_finite()), "nonfinite processed frame");
        let event = ProcessedFrame {
            absolute_start: index * FRAME_SAMPLES,
            captured_at: Some(at + Duration::from_millis(index as u64 * 10)),
            samples: output,
            aec_applied: true,
            reference_valid: true,
            reference_mode: if far_present { ReferenceMode::Native } else { ReferenceMode::Idle },
            stream_id: far_present.then_some(1),
            speech_epoch: far_present.then_some(7),
            filter_revision: 1,
        };
        // Warm rows feed real render/capture through the same AEC for ten seconds
        // before constructing the detector. Cold controls still start at frame 0.
        let candidate = if index >= monitor_start {
            monitored_energy.add(&capture, &output);
            detector.get_or_insert_with(|| BargeInOnset::new(13, 7)).observe(&event, true)
        } else {
            None
        };
        if candidate.is_some() {
            first = Some(index);
        }
        let analysis = output.map(|v| (v * 32768.0).clamp(-32768.0, 32767.0));
        let probability = (index >= monitor_start).then(|| {
            trace_vad
                .get_or_insert_with(|| VoiceActivityDetectorWrapper::new(sonora_simd::detect_backend(), 16000))
                .analyze(&analysis)
        });
        if let Some(p) = probability {
            trace_votes = ((trace_votes << 1) | u32::from(p >= 0.60)) & ((1 << 20) - 1);
        }
        let mut frame_energy = Energy::default();
        frame_energy.add(&capture, &output);
        if (monitor_start..monitor_start + 60).contains(&index) {
            around_start.push(json!({"frame": index, "vad_probability": probability,
                "trace_votes_of_20": trace_votes.count_ones(), "candidate": candidate.is_some(),
                "energy": frame_energy.value()}));
        }
        total.add(&capture, &output);
        if let Some(onset) = near_frame {
            if index < onset {
                pre_near.add(&capture, &output);
            } else {
                post_near.add(&capture, &output);
            }
        }
        if index % 100 == 0
            || candidate.is_some()
            || index + 1 == FRAMES + monitor_start
            || near_frame.is_some_and(|onset| index == onset || index == onset + 50)
        {
            snapshots.push(json!({"frame": index, "vad_probability": probability,
                "trace_votes_of_20": trace_votes.count_ones(), "aec": processor.metrics(),
                "energy": frame_energy.value()}));
        }
    }
    let false_before_near = first.is_some_and(|f| near_frame.is_none_or(|onset| f < onset));
    let near_delay_ms = near_frame.and_then(|onset| first.filter(|f| *f >= onset).map(|f| (f + 1 - onset) * 10));
    let status = match (near_frame, first) {
        (None, None) => "no_candidate_in_echo_only_window",
        (None, Some(_)) => "false_candidate_on_echo_only",
        (Some(_), None) => "near_speech_missed",
        (Some(onset), Some(f)) if f < onset => "false_candidate_preempted_near_speech",
        _ => "candidate_after_near_onset_not_proof_of_echo_rejection",
    };
    Ok(json!({"profile": profile.name(), "far_present": far_present,
        "near_onset_frame": near_frame, "near_onset_ms": near_frame.map(|f| f * 10),
        "waveform_echo_delay_ms": wave_ms, "api_stream_delay_ms": api_ms,
        "frames": FRAMES + monitor_start, "monitor_start_frame": monitor_start,
        "first_candidate_frame_zero_based": first,
        "first_candidate_confirmation_ms": first.map(|f| (f + 1) * 10),
        "near_confirmation_delay_ms": near_delay_ms,
        "false_candidate_before_near_or_on_echo_only": false_before_near, "semantic_status": status,
        "energy": {"all": total.value(), "monitored": monitored_energy.value(),
            "before_near": pre_near.value(), "after_near": post_near.value()},
        "first_600ms_monitor_vad_trace": around_start, "metric_snapshots": snapshots}))
}

#[test]
#[ignore = "Offline fixed-SAPI AEC/VAD diagnostic; requires an explicit new report path; no devices/models/network"]
fn fixed_sapi_aec_vad_configuration_diagnostic() -> Result<()> {
    let path = PathBuf::from(
        std::env::var_os("AI_TUTOR_BARGE_DIAGNOSTIC_REPORT")
            .context("set AI_TUTOR_BARGE_DIAGNOSTIC_REPORT to an explicit new absolute JSON path")?,
    );
    ensure!(path.is_absolute(), "diagnostic report path must be absolute");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .context("report must not already exist; parent directory must exist")?;
    let (zh, en) = (fixed_sapi("zh_short", ZH_HASH)?, fixed_sapi("en_short", EN_HASH)?);
    let profiles =
        [Profile::Production, Profile::LegacyOffMetrics, Profile::OfficialDefaultMetrics, Profile::HmmOnMetrics];
    let mut rows = Vec::new();
    for profile in profiles {
        // First row is exactly the original 1,000-frame echo-only input/adapter.
        rows.push(run(profile, true, None, (80, 80), (&zh, &en), 0)?);
        for onset in [100, 400] {
            rows.push(run(profile, false, Some(onset), (0, 0), (&zh, &en), 0)?);
            rows.push(run(profile, true, Some(onset), (80, 80), (&zh, &en), 0)?);
        }
        // Same acoustic waveform, separate device-buffering interpretation.
        rows.push(run(profile, true, None, (80, 0), (&zh, &en), 0)?);
    }
    for delay in [40, 160] {
        rows.push(run(Profile::Production, true, None, (delay, delay as i32), (&zh, &en), 0)?);
        rows.push(run(Profile::Production, true, Some(400), (delay, delay as i32), (&zh, &en), 0)?);
    }
    rows.push(run(Profile::Production, true, None, (80, 80), (&zh, &en), 1000)?);
    rows.push(run(Profile::Production, true, Some(1100), (80, 80), (&zh, &en), 1000)?);
    let reproduced = rows[0]["first_candidate_frame_zero_based"] == json!(38);
    let report = json!({"schema": 1, "structural_status": "completed",
        "original_frame38_reproduced": reproduced, "sonora_version": "0.2.0",
        "fixtures": {"zh_short_sha256": ZH_HASH, "en_short_sha256": EN_HASH},
        "thresholds_unchanged": {"probability": 0.60, "required_frames": 12, "window_frames": 20},
        "notes": ["Statistics only; no PCM, devices, real user input, models or network.",
            "Official EchoCanceller default is Legacy+HPon, identical to explicit Legacy+HPon; not an independent strategy.",
            "Unmodified production adapter remains the first control; residual detector is opt-in on other profiles.",
            "API stream delay describes device buffering, not waveform propagation delay; the 80/0 rows isolate that distinction.",
            "Candidate is one-shot: no reset, gating, or threshold change. Early false candidates preempt later near speech.",
            "Two explicitly warm diagnostic rows adapt the same AEC on actual far audio for 10 seconds before creating the monitor; this is not a proposed production gate.",
            "Trace VAD is observational and continues after the production detector latches; it never decides candidates.",
            "Near onset is fixture insertion time, including fixture leading silence; mixed candidates alone do not establish near sensitivity.",
            "Energy attenuation measures the entire capture mix, not isolated residual echo during double talk. Zero energy gives null dB.",
            "Residual echo likelihood may be unavailable or immature in this 10-second window; no likelihood threshold is used.",
            "Structural test completion is not semantic acceptance or acoustic/device validation."],
        "cases": rows});
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    println!(
        "AEC/VAD diagnostic: {} cases; original_frame38_reproduced={reproduced}; report={}",
        report["cases"].as_array().unwrap().len(),
        path.display()
    );
    Ok(())
}

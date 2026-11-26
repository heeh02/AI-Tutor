//! Known-signal offline metrics. No fitted per-case lag and no epsilon-inflated scores.
use crate::echo_cancel::SAMPLE_RATE;
use serde_json::{Value, json};
use std::ops::Range;

const POWER_FLOOR: f64 = 1e-16;
pub const MAX_LAG: usize = SAMPLE_RATE / 50; // 20 ms, not automatically enlarged.

fn db(numerator: f64, denominator: f64) -> Option<f64> {
    (numerator > POWER_FLOOR && denominator > POWER_FLOOR)
        .then(|| 10.0 * (numerator / denominator).log10())
}

#[derive(Default)]
struct Energy {
    count: usize,
    target: f64,
    output: f64,
    dot: f64,
    error: f64,
}
impl Energy {
    fn add(&mut self, target: f32, output: f32) {
        let (target, output) = (target as f64, output as f64);
        self.count += 1;
        self.target += target * target;
        self.output += output * output;
        self.dot += target * output;
        self.error += (output - target).powi(2);
    }
    fn report(&self) -> Value {
        let count = self.count.max(1) as f64;
        let target = self.target / count;
        let output = self.output / count;
        let error = self.error / count;
        let gain = (target > POWER_FLOOR).then(|| self.dot / self.target);
        let projection = gain.map_or(0.0, |gain| gain * gain * target);
        let residual = (output - projection).max(0.0);
        let reason = if self.count == 0 {
            Some("no_valid_samples")
        } else if target <= POWER_FLOOR {
            Some("target_silent")
        } else if output <= POWER_FLOOR {
            Some("output_silent")
        } else if projection <= POWER_FLOOR {
            Some("zero_projection")
        } else if residual <= POWER_FLOOR {
            Some("projection_residual_below_floor")
        } else {
            None
        };
        json!({"sample_count":self.count,"target_rms":target.sqrt(),"output_rms":output.sqrt(),
            "error_rms":error.sqrt(),"sdr_db":db(target,error),
            "sdr_undefined_reason":if self.count==0 {Some("no_valid_samples")} else if target<=POWER_FLOOR {Some("target_silent")} else if error<=POWER_FLOOR {Some("error_below_floor")} else {None},
            "si_sdr_db":if reason.is_none(){db(projection,residual)}else{None},"si_sdr_undefined_reason":reason,
            "projection_gain_linear":gain,"projection_gain_db":db(projection,target),
            "rms_gain_db":db(output,target),"projection_residual_rms":residual.sqrt()})
    }
}

fn paired_energy(
    target: &[f32],
    output: &[f32],
    valid: &[bool],
    lag: usize,
    window: Range<usize>,
) -> Energy {
    let mut energy = Energy::default();
    for at in window {
        let shifted = at + lag;
        if at < target.len() && shifted < output.len() && valid[shifted] {
            energy.add(target[at], output[shifted]);
        }
    }
    energy
}

pub fn fidelity(
    target: &[f32],
    output: &[f32],
    valid: &[bool],
    lag: usize,
    window: Range<usize>,
) -> Value {
    paired_energy(target, output, valid, lag, window).report()
}

/// ERLE is only meaningful when the target is pure echo, with no near speech.
pub fn erle(
    echo: &[f32],
    output: &[f32],
    valid: &[bool],
    lag: usize,
    window: Range<usize>,
) -> Value {
    let energy = paired_energy(echo, output, valid, lag, window);
    let count = energy.count.max(1) as f64;
    let input = energy.target / count;
    let residual = energy.output / count;
    json!({"sample_count":energy.count,"echo_rms":input.sqrt(),"residual_rms":residual.sqrt(),
        "erle_db":db(input,residual),"undefined_reason":if energy.count==0 {Some("no_valid_samples")}
            else if input<=POWER_FLOOR {Some("echo_silent")} else if residual<=POWER_FLOOR {Some("output_silent")} else {None}})
}

pub struct LagCalibration {
    pub lag: usize,
    pub usable: bool,
    pub report: Value,
}

pub fn calibrate(near: &[f32], output: &[f32], valid: &[bool]) -> LagCalibration {
    // Fixed, declared window from the single near-only control. Not tuned on mixtures.
    let end = (8 * SAMPLE_RATE).min(near.len().saturating_sub(MAX_LAG));
    let start = if end > 3 * SAMPLE_RATE {
        3 * SAMPLE_RATE
    } else {
        0
    };
    let mut best = (0, -1.0);
    for lag in 0..=MAX_LAG {
        let energy = paired_energy(near, output, valid, lag, start..end);
        let scale = (energy.target * energy.output).sqrt();
        if scale > POWER_FLOOR {
            let correlation = energy.dot / scale;
            if correlation > best.1 {
                best = (lag, correlation);
            }
        }
    }
    let boundary = best.0 == MAX_LAG;
    let usable = best.1 >= 0.5 && !boundary;
    LagCalibration {
        lag: best.0,
        usable,
        report: json!({"lag_samples":best.0,"lag_ms":best.0 as f64/16.0,
        "normalized_correlation":if best.1>=-0.5{Some(best.1)}else{None},"usable":usable,
        "range_exhausted":boundary,"search_samples":[0,MAX_LAG],"window_ms":[start/16,end/16],
        "selection":"single_near_only_control_then_frozen_for_all_scenarios",
        "invalid_reason":if boundary{Some("lag_at_search_boundary")}else if !usable{Some("near_only_correlation_below_0.5")}else{None}}),
    }
}

pub fn timing(microseconds: &[f64]) -> Value {
    if microseconds.is_empty() {
        return json!({"count":0});
    }
    let mut sorted = microseconds.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        sorted[((sorted.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1)]
    };
    json!({"count":sorted.len(),"unit":"microseconds_per_10ms_pair","p50":percentile(0.5),"p95":percentile(0.95),
        "p99":percentile(0.99),"max":sorted[sorted.len()-1],"mean":sorted.iter().sum::<f64>()/sorted.len() as f64,
        "pairs_over_10ms":sorted.iter().filter(|v| **v>10_000.0).count(),"includes":"input_validation_render_delay_set_capture_output_validation"})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suppression_scores_do_not_reward_silence_as_speech_fidelity() {
        let target = vec![0.1; 1000];
        let quiet = vec![0.0; 1000];
        let valid = vec![true; 1000];
        let fidelity = fidelity(&target, &quiet, &valid, 0, 0..1000);
        assert_eq!(fidelity["si_sdr_undefined_reason"], "output_silent");
        assert!(fidelity["si_sdr_db"].is_null());
        assert_eq!(fidelity["sdr_db"], 0.0);
        assert_eq!(fidelity["projection_gain_linear"], 0.0);
        let echo = erle(&target, &quiet, &valid, 0, 0..1000);
        assert!(echo["erle_db"].is_null());
        assert_eq!(echo["undefined_reason"], "output_silent");
    }
    #[test]
    fn known_attenuation_and_invalid_frames_have_explicit_metrics() {
        let target: Vec<_> = (0..1000).map(|i| (i as f32 * 0.17).sin() * 0.1).collect();
        let output: Vec<_> = target.iter().map(|v| v * 0.1).collect();
        let mut valid = vec![true; 1000];
        valid[..100].fill(false);
        let result = erle(&target, &output, &valid, 0, 0..1000);
        assert_eq!(result["sample_count"], 900);
        assert!((result["erle_db"].as_f64().unwrap() - 20.0).abs() < 1e-5);
        let near = fidelity(&target, &output, &valid, 0, 0..1000);
        assert!((near["projection_gain_db"].as_f64().unwrap() + 20.0).abs() < 1e-5);
        assert!(near["sdr_db"].as_f64().unwrap() < 1.0);
    }
    #[test]
    fn fixed_lag_calibration_detects_delay_and_refuses_search_boundary() {
        let n = 5000;
        let mut state = 42u32;
        let target: Vec<_> = (0..n)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state as f64 / u32::MAX as f64 - 0.5) as f32 * 0.2
            })
            .collect();
        for lag in [0, 64, MAX_LAG] {
            let mut output = vec![0.0; n];
            output[lag..].copy_from_slice(&target[..n - lag]);
            let calibration = calibrate(&target, &output, &vec![true; n]);
            assert_eq!(calibration.lag, lag);
            assert_eq!(calibration.usable, lag != MAX_LAG);
        }
    }
    #[test]
    fn nearest_rank_timing_reports_max_and_deadline_misses() {
        let result = timing(&[1.0, 2.0, 11_000.0]);
        assert_eq!(result["p50"], 2.0);
        assert_eq!(result["p99"], 11_000.0);
        assert_eq!(result["pairs_over_10ms"], 1);
    }
}

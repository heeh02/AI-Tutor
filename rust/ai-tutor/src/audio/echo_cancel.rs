//! Sonora AEC3 adapter: mono capture at 16 kHz and independently framed render.
//! The paired 16 kHz entry point remains available to the offline probe.
use anyhow::{Result, ensure};
use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{EchoCanceller, TransparentModeType},
};

pub const SAMPLE_RATE: usize = 16_000;
pub const FRAME_SAMPLES: usize = 160;
pub type Frame = [f32; FRAME_SAMPLES];

pub struct EchoCanceller16k {
    processor: AudioProcessing,
    render_scratch: Vec<f32>,
    render_rate: u32,
    high_pass: bool,
}

impl EchoCanceller16k {
    pub fn new() -> Self {
        Self::with_high_pass(false)
    }

    /// Only the preregistered offline comparison changes this one setting.
    pub fn with_high_pass(high_pass: bool) -> Self {
        Self::configured(SAMPLE_RATE as u32, high_pass)
    }

    pub fn with_render_rate(render_rate: u32) -> Result<Self> {
        ensure!(
            (8_000..=384_000).contains(&render_rate) && render_rate % 100 == 0,
            "AEC render rate must form complete supported 10 ms frames"
        );
        Ok(Self::configured(render_rate, false))
    }

    fn configured(render_rate: u32, high_pass: bool) -> Self {
        let config = Config {
            echo_canceller: Some(EchoCanceller {
                // EchoCanceller::default() otherwise enables a high-pass filter.
                enforce_high_pass_filtering: high_pass,
                transparent_mode: TransparentModeType::Legacy,
            }),
            ..Default::default()
        };
        Self {
            processor: AudioProcessing::builder()
                .config(config)
                .capture_config(StreamConfig::new(SAMPLE_RATE as u32, 1))
                .render_config(StreamConfig::new(render_rate, 1))
                .build(),
            render_scratch: vec![0.0; render_rate as usize / 100],
            render_rate,
            high_pass,
        }
    }

    /// The supplied delay is device buffering, not a fitted alignment parameter.
    /// A discontinuity must be reported by resetting, never by hiding missing
    /// reference samples in a nominally valid all-zero frame.
    pub fn process(&mut self, render: &Frame, capture: &Frame, delay_ms: i32) -> Result<Frame> {
        // Validate the entire pair before advancing render state, as before.
        ensure!(
            self.render_rate == SAMPLE_RATE as u32,
            "Paired AEC requires 16 kHz render"
        );
        validate_capture(capture, delay_ms)?;
        self.push_render(render)?;
        self.process_capture(capture, delay_ms)
    }

    pub fn push_render(&mut self, render: &[f32]) -> Result<()> {
        ensure!(
            render.len() == self.render_scratch.len()
                && render.iter().all(|v| v.is_finite() && v.abs() <= 1.0),
            "AEC render frame must contain one finite normalized 10 ms frame"
        );
        self.processor
            .process_render_f32(&[render], &mut [&mut self.render_scratch])?;
        Ok(())
    }

    pub fn process_capture(&mut self, capture: &Frame, delay_ms: i32) -> Result<Frame> {
        validate_capture(capture, delay_ms)?;
        self.processor.set_stream_delay_ms(delay_ms)?;
        let mut output = [0.0; FRAME_SAMPLES];
        self.processor
            .process_capture_f32(&[capture], &mut [&mut output])?;
        ensure!(
            output.iter().all(|v| v.is_finite()),
            "AEC produced non-finite PCM"
        );
        Ok(output)
    }

    pub fn reset(&mut self) {
        *self = Self::configured(self.render_rate, self.high_pass);
    }
}

fn validate_capture(capture: &Frame, delay_ms: i32) -> Result<()> {
    ensure!(
        (0..=500).contains(&delay_ms),
        "AEC delay is outside 0..500 ms"
    );
    ensure!(
        capture.iter().all(|v| v.is_finite() && v.abs() <= 1.0),
        "AEC frame must contain finite normalized PCM"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_render_capture_preserves_paired_adapter_and_native_rate_reset() -> Result<()> {
        let mut paired = EchoCanceller16k::new();
        let mut split = EchoCanceller16k::with_render_rate(16_000)?;
        let render = std::array::from_fn(|i| (i as f32 * 0.04).sin() * 0.1);
        let capture = std::array::from_fn(|i| render[i] * 0.5);
        for _ in 0..20 {
            let expected = paired.process(&render, &capture, 30)?;
            split.push_render(&render)?;
            assert_eq!(split.process_capture(&capture, 30)?, expected);
        }
        let mut native = EchoCanceller16k::with_render_rate(48_000)?;
        native.push_render(&[0.0; 480])?;
        assert!(
            native
                .process_capture(&capture, 30)?
                .iter()
                .all(|v| v.is_finite())
        );
        native.reset();
        assert!(native.push_render(&[0.0; 160]).is_err());
        native.push_render(&[0.0; 480])?;
        Ok(())
    }

    #[test]
    fn high_pass_setting_survives_reset_without_enabling_other_processors() -> Result<()> {
        let render = [0.1; FRAME_SAMPLES];
        let capture = [0.04; FRAME_SAMPLES];
        for high_pass in [false, true] {
            let mut used = EchoCanceller16k::with_high_pass(high_pass);
            let mut fresh = EchoCanceller16k::with_high_pass(high_pass);
            used.process(&render, &capture, 80)?;
            used.reset();
            let config = used.processor.config();
            assert_eq!(
                config
                    .echo_canceller
                    .as_ref()
                    .unwrap()
                    .enforce_high_pass_filtering,
                high_pass
            );
            assert!(matches!(
                config.echo_canceller.as_ref().unwrap().transparent_mode,
                TransparentModeType::Legacy
            ));
            assert!(config.noise_suppression.is_none() && config.gain_controller2.is_none());
            assert_eq!(
                used.process(&render, &capture, 80)?,
                fresh.process(&render, &capture, 80)?
            );
        }
        Ok(())
    }

    #[test]
    fn reset_reproduces_fresh_processor_and_invalid_inputs_do_not_advance_it() -> Result<()> {
        let mut used = EchoCanceller16k::new();
        let mut fresh = EchoCanceller16k::new();
        let render = std::array::from_fn(|i| (i as f32 * 0.13).sin() * 0.1);
        let capture = std::array::from_fn(|i| render[i] * 0.4);
        for _ in 0..30 {
            used.process(&render, &capture, 20)?;
        }
        used.reset();
        ensure!(used.process(&render, &capture, -1).is_err());
        ensure!(used.process(&render, &capture, 501).is_err());
        let mut invalid = capture;
        invalid[0] = f32::NAN;
        ensure!(used.process(&render, &invalid, 20).is_err());
        ensure!(used.process(&render, &capture, 20)? == fresh.process(&render, &capture, 20)?);
        let config = used.processor.config();
        ensure!(config.noise_suppression.is_none() && config.gain_controller2.is_none());
        ensure!(config.high_pass_filter.is_none());
        ensure!(
            !config
                .echo_canceller
                .as_ref()
                .unwrap()
                .enforce_high_pass_filtering
        );
        Ok(())
    }
}

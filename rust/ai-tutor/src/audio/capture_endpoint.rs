//! Explicit endpoint analysis of the existing full capture buffer, never its owner.
//! Only capture_loop closes a stream and commits an EndpointReceipt afterwards.
use super::voice_endpoint::{self, EndpointCandidate, Frame, VoiceEndpoint};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum EndpointMode {
    #[default]
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "voice_endpoint_v1")]
    VoiceEndpointV1,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct EndpointReceipt {
    pub protocol_version: u8,
    pub method_id: &'static str,
    pub revision: u64,
    #[serde(flatten)]
    pub candidate: EndpointCandidate,
    pub cause: Option<&'static str>,
    pub input_closed: bool,
    pub captured_samples: usize,
    pub sample_rate: u32,
    pub capture_closed_elapsed_ms: u64,
}

pub(super) struct CaptureEndpoint {
    generation: u64,
    detector: VoiceEndpoint,
    next_sample: u64,
    receipt: Option<EndpointReceipt>,
}
impl CaptureEndpoint {
    pub(super) fn new(generation: u64) -> Self {
        Self {
            generation,
            detector: VoiceEndpoint::new(generation),
            next_sample: 0,
            receipt: None,
        }
    }
    pub(super) fn receipt(&self) -> Option<&EndpointReceipt> {
        self.receipt.as_ref()
    }
    /// Copy only one complete frame while the capture-buffer lock is held.
    /// The returned stack frame is analysed after the caller releases that lock.
    pub(super) fn next_frame(&mut self, samples: &[f32], rate: u32) -> Option<Frame> {
        if self.detector.terminal().is_some() || rate == 0 {
            return None;
        }
        let frame = frame_at(samples, rate, self.next_sample)?;
        self.next_sample += voice_endpoint::FRAME_SAMPLES as u64;
        Some(frame)
    }
    pub(super) fn process(&mut self, generation: u64, frame: &Frame) -> Option<EndpointCandidate> {
        self.detector.process_10ms(generation, frame)
    }
    pub(super) fn error(&mut self, generation: u64) -> Option<EndpointCandidate> {
        self.detector.device_error(generation)
    }
    pub(super) fn limit(&mut self, generation: u64) -> Option<EndpointCandidate> {
        self.detector.duration_limit(generation)
    }
    /// Call only after the input stream's Drop returned and identity was rechecked.
    pub(super) fn record_closed(
        &mut self,
        candidate: EndpointCandidate,
        cause: Option<&'static str>,
        captured_samples: usize,
        sample_rate: u32,
        capture_closed_elapsed_ms: u64,
    ) -> Option<EndpointReceipt> {
        if self.receipt.is_some() || candidate.recording_generation != self.generation {
            return None;
        }
        let receipt = EndpointReceipt {
            protocol_version: 1,
            method_id: voice_endpoint::METHOD_ID,
            revision: 1,
            candidate,
            cause,
            input_closed: true,
            captured_samples,
            sample_rate,
            capture_closed_elapsed_ms,
        };
        self.receipt = Some(receipt.clone());
        Some(receipt)
    }
}

fn frame_at(samples: &[f32], rate: u32, next_sample: u64) -> Option<Frame> {
    let target_rate = voice_endpoint::SAMPLE_RATE as u64;
    let last_position = (next_sample + voice_endpoint::FRAME_SAMPLES as u64 - 1) * rate as u64;
    let last_right = last_position.div_ceil(target_rate) as usize;
    if last_right >= samples.len() {
        return None;
    }
    let mut frame = [0.0; voice_endpoint::FRAME_SAMPLES];
    for (index, sample) in frame.iter_mut().enumerate() {
        let position = (next_sample + index as u64) * rate as u64;
        let left = (position / target_rate) as usize;
        let remainder = position % target_rate;
        *sample = if remainder == 0 {
            samples[left]
        } else {
            let fraction = remainder as f32 / target_rate as f32;
            samples[left] * (1.0 - fraction) + samples[left + 1] * fraction
        };
    }
    Some(frame)
}

pub(super) fn duration_reached(elapsed: std::time::Duration, samples: usize, rate: u32) -> bool {
    elapsed >= std::time::Duration::from_secs(20) || (rate > 0 && samples >= rate as usize * 20)
}

#[cfg(test)]
mod tests {
    use super::voice_endpoint::EndpointReason;
    use super::*;
    #[test]
    fn native_rate_cursor_is_chunk_independent_and_does_not_pad_incomplete_frames() {
        for rate in [16_000, 44_100, 48_000] {
            let source = (0..rate / 10)
                .map(|i| i as f32 / rate as f32)
                .collect::<Vec<_>>();
            let original = source.clone();
            let mut endpoint = CaptureEndpoint::new(3);
            assert!(endpoint.next_frame(&source[..10], rate).is_none());
            assert_eq!(endpoint.next_sample, 0);
            let mut output = Vec::new();
            for available in (1..=source.len())
                .step_by(137)
                .chain(std::iter::once(source.len()))
            {
                while let Some(frame) = endpoint.next_frame(&source[..available], rate) {
                    output.extend(frame);
                }
            }
            assert_eq!(output.len(), 1_600);
            for (i, sample) in output.iter().enumerate() {
                assert!((*sample - i as f32 / 16_000.0).abs() < 0.000001);
            }
            assert_eq!(source, original);
            assert!(endpoint.next_frame(&source, rate).is_none());
        }
    }
    #[test]
    fn receipt_keeps_candidate_and_real_close_times_separate_and_never_rewrites() {
        let mut endpoint = CaptureEndpoint::new(3);
        let candidate = endpoint.limit(3).unwrap();
        let receipt = endpoint
            .record_closed(candidate, None, 31_000, 16_000, 20_005)
            .unwrap();
        assert_eq!(receipt.candidate.elapsed_audio_ms, 0);
        assert_eq!(receipt.capture_closed_elapsed_ms, 20_005);
        assert_eq!(receipt.captured_samples, 31_000);
        assert!(receipt.input_closed);
        assert!(
            endpoint
                .record_closed(candidate, Some("late"), 0, 0, 0)
                .is_none()
        );
    }
    #[test]
    fn wall_or_sample_cap_precedes_analysis_without_fabricating_silence() {
        assert!(duration_reached(
            std::time::Duration::from_secs(20),
            0,
            48_000
        ));
        assert!(duration_reached(
            std::time::Duration::from_secs(1),
            960_000,
            48_000
        ));
        assert!(!duration_reached(
            std::time::Duration::from_millis(19_999),
            959_999,
            48_000
        ));
        let mut endpoint = CaptureEndpoint::new(3);
        assert!(endpoint.limit(2).is_none());
        assert!(endpoint.error(2).is_none());
        let end = endpoint.limit(3).unwrap();
        assert_eq!(end.reason, EndpointReason::DurationLimit);
        assert_eq!(end.elapsed_audio_ms, 0);
    }
}

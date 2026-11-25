//! Near-speech onset candidates from qualified processed frames, never raw fallback.
//! A candidate is bound evidence; the owner must still validate and cancel its reply.
use super::{
    echo_cancel::{FRAME_SAMPLES, Frame, SAMPLE_RATE},
    render_reference::ReferenceMode,
};
use sonora_agc2::vad_wrapper::VoiceActivityDetectorWrapper;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(super) const PRE_ROLL_SAMPLES: usize = SAMPLE_RATE * 300 / 1000;
const PRE_ROLL_FRAMES: usize = PRE_ROLL_SAMPLES / FRAME_SAMPLES;
const WINDOW_FRAMES: u32 = 20;
const REQUIRED_FRAMES: u32 = 12;
const ONSET_PROBABILITY: f32 = 0.60;

#[derive(Clone, Debug)]
pub(crate) struct ProcessedFrame {
    pub absolute_start: usize,
    pub captured_at: Option<Instant>,
    pub samples: Frame,
    /// Successful processing and valid reference mechanics, not proof of echo removal.
    pub aec_applied: bool,
    pub reference_valid: bool,
    pub reference_mode: ReferenceMode,
    pub stream_id: Option<u64>,
    pub speech_epoch: Option<u64>,
    pub filter_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OnsetCandidate {
    pub monitor_token: u64,
    pub audio_epoch: u64,
    pub pre_roll_start: usize,
    pub confirmed_sample: usize,
    pub pre_roll_captured_at: Instant,
    pub confirmed_at: Instant,
    pub stream_id: Option<u64>,
    pub filter_revision: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Continuity {
    stream_id: Option<u64>,
    speech_epoch: Option<u64>,
    filter_revision: u64,
}

pub(super) struct BargeInOnset {
    monitor_token: u64,
    audio_epoch: u64,
    vad: VoiceActivityDetectorWrapper,
    window: u32,
    last_end: Option<usize>,
    last_capture: Option<Instant>,
    continuity: Option<Continuity>,
    pre_roll: VecDeque<ProcessedFrame>,
    candidate: Option<OnsetCandidate>,
}

impl BargeInOnset {
    pub(super) fn new(monitor_token: u64, audio_epoch: u64) -> Self {
        Self {
            monitor_token,
            audio_epoch,
            vad: VoiceActivityDetectorWrapper::new(
                sonora_simd::detect_backend(),
                SAMPLE_RATE as i32,
            ),
            window: 0,
            last_end: None,
            last_capture: None,
            continuity: None,
            pre_roll: VecDeque::with_capacity(PRE_ROLL_FRAMES),
            candidate: None,
        }
    }
    fn clear_window(&mut self) {
        if self.last_end.is_some() || !self.pre_roll.is_empty() {
            self.vad = VoiceActivityDetectorWrapper::new(
                sonora_simd::detect_backend(),
                SAMPLE_RATE as i32,
            );
        }
        for frame in &mut self.pre_roll {
            frame.samples.fill(0.0);
        }
        self.pre_roll.clear();
        self.window = 0;
        self.last_end = None;
        self.last_capture = None;
        self.continuity = None;
    }
    pub(super) fn candidate(&self) -> Option<OnsetCandidate> {
        self.candidate
    }
    fn qualify(&mut self, frame: &ProcessedFrame, owner_target_valid: bool) -> bool {
        if self.candidate.is_some() {
            return false;
        }
        let source_matches = match frame.reference_mode {
            ReferenceMode::Native => {
                frame.stream_id.is_some() && frame.speech_epoch == Some(self.audio_epoch)
            }
            ReferenceMode::Idle => owner_target_valid,
            ReferenceMode::Unavailable => false,
        };
        if !owner_target_valid
            || !source_matches
            || !frame.aec_applied
            || !frame.reference_valid
            || frame.captured_at.is_none()
            || frame
                .samples
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1.0)
        {
            self.clear_window();
            return false;
        }
        let continuity = Continuity {
            stream_id: frame.stream_id,
            speech_epoch: frame.speech_epoch,
            filter_revision: frame.filter_revision,
        };
        let captured_at = frame.captured_at.unwrap();
        let clock_gap = self.last_capture.is_some_and(|last| {
            let expected = last + Duration::from_millis(10);
            let distance = if captured_at >= expected {
                captured_at.duration_since(expected)
            } else {
                expected.duration_since(captured_at)
            };
            distance > Duration::from_millis(2)
        });
        if self.last_end.is_some_and(|end| end != frame.absolute_start)
            || self.continuity.is_some_and(|prior| prior != continuity)
            || clock_gap
        {
            self.clear_window();
        }
        self.continuity = Some(continuity);
        self.last_end = Some(frame.absolute_start + FRAME_SAMPLES);
        self.last_capture = Some(captured_at);
        true
    }
    fn accept_probability(
        &mut self,
        frame: &ProcessedFrame,
        probability: f32,
    ) -> Option<OnsetCandidate> {
        if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
            self.clear_window();
            return None;
        }
        if self.pre_roll.len() == PRE_ROLL_FRAMES {
            if let Some(mut removed) = self.pre_roll.pop_front() {
                removed.samples.fill(0.0);
            }
        }
        self.pre_roll.push_back(frame.clone());
        self.window = ((self.window << 1) | u32::from(probability >= ONSET_PROBABILITY))
            & ((1 << WINDOW_FRAMES) - 1);
        if self.window.count_ones() < REQUIRED_FRAMES {
            return None;
        }
        let first = self.pre_roll.front().unwrap();
        let candidate = OnsetCandidate {
            monitor_token: self.monitor_token,
            audio_epoch: self.audio_epoch,
            pre_roll_start: first.absolute_start,
            confirmed_sample: frame.absolute_start + FRAME_SAMPLES,
            pre_roll_captured_at: first.captured_at.unwrap(),
            confirmed_at: frame.captured_at.unwrap() + Duration::from_millis(10),
            stream_id: frame.stream_id,
            filter_revision: frame.filter_revision,
        };
        self.candidate = Some(candidate);
        Some(candidate)
    }
    pub(super) fn observe(
        &mut self,
        frame: &ProcessedFrame,
        owner_target_valid: bool,
    ) -> Option<OnsetCandidate> {
        if !self.qualify(frame, owner_target_valid) {
            return None;
        }
        let mut analysis = [0.0; FRAME_SAMPLES];
        for (dst, src) in analysis.iter_mut().zip(&frame.samples) {
            *dst = (*src * 32768.0).clamp(-32768.0, 32767.0);
        }
        let probability = self.vad.analyze(&analysis);
        self.accept_probability(frame, probability)
    }
    pub(super) fn wipe(&mut self) {
        self.clear_window();
        self.candidate = None;
    }
}
impl Drop for BargeInOnset {
    fn drop(&mut self) {
        self.wipe();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(index: usize) -> ProcessedFrame {
        ProcessedFrame {
            absolute_start: index * FRAME_SAMPLES,
            captured_at: Some(Instant::now()),
            samples: [0.03; FRAME_SAMPLES],
            aec_applied: true,
            reference_valid: true,
            reference_mode: ReferenceMode::Native,
            stream_id: Some(1),
            speech_epoch: Some(7),
            filter_revision: 1,
        }
    }
    fn probability(
        detector: &mut BargeInOnset,
        index: usize,
        at: Instant,
        value: f32,
    ) -> Option<OnsetCandidate> {
        let mut frame = frame(index);
        frame.captured_at = Some(at + Duration::from_millis(index as u64 * 10));
        if !detector.qualify(&frame, true) {
            return None;
        }
        detector.accept_probability(&frame, value)
    }
    #[test]
    fn onset_latches_300ms_prefix_once_with_exact_target() {
        let mut detector = BargeInOnset::new(13, 7);
        let at = Instant::now();
        for index in 0..40 {
            assert!(probability(&mut detector, index, at, 0.0).is_none());
        }
        let mut candidate = None;
        for index in 40..52 {
            candidate = probability(&mut detector, index, at, 0.9).or(candidate);
        }
        let candidate = candidate.unwrap();
        assert_eq!((candidate.monitor_token, candidate.audio_epoch), (13, 7));
        assert_eq!(candidate.pre_roll_start, 22 * 160);
        assert_eq!(candidate.confirmed_sample, 52 * 160);
        assert_eq!(
            candidate.confirmed_sample - candidate.pre_roll_start,
            PRE_ROLL_SAMPLES
        );
        assert!(probability(&mut detector, 52, at, 0.9).is_none());
        assert_eq!(detector.candidate(), Some(candidate));
    }
    #[test]
    fn bypass_wrong_epoch_filter_change_and_gap_cannot_combine_votes() {
        for condition in 0..4 {
            let mut detector = BargeInOnset::new(13, 7);
            let at = Instant::now();
            for index in 0..11 {
                assert!(probability(&mut detector, index, at, 0.9).is_none());
            }
            let mut changed = frame(11);
            changed.captured_at = Some(at + Duration::from_millis(110));
            match condition {
                0 => changed.reference_valid = false,
                1 => changed.speech_epoch = Some(8),
                2 => changed.filter_revision = 2,
                _ => changed.absolute_start += 160,
            }
            if detector.qualify(&changed, true) {
                assert!(detector.accept_probability(&changed, 0.9).is_none());
            }
            assert!(detector.candidate().is_none());
        }
    }
    #[test]
    fn real_vad_silence_and_unqualified_loud_frames_never_trigger() {
        let mut detector = BargeInOnset::new(13, 7);
        let at = Instant::now();
        for index in 0..400 {
            let mut frame = frame(index);
            frame.captured_at = Some(at + Duration::from_millis(index as u64 * 10));
            if index < 200 {
                frame.samples.fill(0.0);
            } else {
                frame.aec_applied = false;
                frame.samples.fill(0.8);
            }
            assert!(detector.observe(&frame, true).is_none());
        }
        assert!(detector.candidate().is_none());
        assert!(detector.pre_roll.is_empty());
    }

    fn fixed_sapi(id: &str, expected_hash: &str) -> Vec<f32> {
        use sha2::{Digest, Sha256};
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../runtime/goal-baselines/20261103/sapi-corpus")
            .join(format!("{id}.wav"));
        let bytes = std::fs::read(path).expect("author-written fixed SAPI fixture required");
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected_hash);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let mut at = 12;
        let mut pcm = None;
        let mut valid_format = false;
        while at + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let end = at + 8 + len;
            assert!(end <= bytes.len());
            match &bytes[at..at + 4] {
                b"fmt " => {
                    let f = &bytes[at + 8..end];
                    assert!(f.len() >= 16);
                    valid_format = u16::from_le_bytes(f[..2].try_into().unwrap()) == 1
                        && u16::from_le_bytes(f[2..4].try_into().unwrap()) == 1
                        && u32::from_le_bytes(f[4..8].try_into().unwrap()) == 16000
                        && u16::from_le_bytes(f[14..16].try_into().unwrap()) == 16;
                }
                b"data" => {
                    assert!(pcm.is_none());
                    pcm = Some(&bytes[at + 8..end]);
                }
                _ => {}
            }
            at = end + (len % 2);
        }
        assert!(valid_format);
        pcm.unwrap()
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
            .collect()
    }

    #[test]
    #[ignore = "Fixed author-written adult synthetic WAV; real Sonora AEC/VAD, no devices or ASR/model service"]
    fn real_vad_processed_near_voice_keeps_bounded_prefix() {
        use super::super::echo_cancel::EchoCanceller16k;
        let speech = fixed_sapi(
            "en_short",
            "670191397605e3f21f2b0e331852b665f91e5ee21cb9573f2ffdd93aff825a9f",
        );
        let mut aec = EchoCanceller16k::new();
        let mut detector = BargeInOnset::new(13, 7);
        let at = Instant::now();
        let mut found = None;
        for index in 0..600 {
            let mut near = [0.0; 160];
            if index >= 300 {
                for (offset, value) in near.iter_mut().enumerate() {
                    *value = speech
                        .get((index - 300) * 160 + offset)
                        .copied()
                        .unwrap_or(0.0);
                }
            }
            let output = aec.process(&[0.0; 160], &near, 0).unwrap();
            let mut event = frame(index);
            event.samples = output;
            event.captured_at = Some(at + Duration::from_millis(index as u64 * 10));
            if let Some(candidate) = detector.observe(&event, true) {
                assert!(found.replace(candidate).is_none());
            }
        }
        let candidate = found.expect("actual VAD should find synthetic near speech after AEC");
        assert!(candidate.confirmed_sample > 300 * 160);
        assert!(candidate.confirmed_sample - candidate.pre_roll_start <= PRE_ROLL_SAMPLES);
        assert_eq!(candidate.audio_epoch, 7);
    }

    #[test]
    #[ignore = "Fixed author-written synthetic far speech with linear echo; real Sonora AEC/VAD, no acoustic acceptance"]
    fn real_vad_aec_echo_only_does_not_interrupt() {
        use super::super::echo_cancel::EchoCanceller16k;
        let speech = fixed_sapi(
            "zh_short",
            "ad72efd95a1abc540737554f9ccb63b7a9933effb75e5e458824a90363bb9a33",
        );
        let period = speech.len() + 3200;
        let delay = 1280;
        let far = |sample: usize| speech.get(sample % period).copied().unwrap_or(0.0) * 0.6;
        let mut aec = EchoCanceller16k::new();
        let mut detector = BargeInOnset::new(13, 7);
        let at = Instant::now();
        for index in 0..1000 {
            let render = std::array::from_fn(|offset| far(index * 160 + offset));
            let capture = std::array::from_fn(|offset| {
                let sample = index * 160 + offset;
                if sample >= delay {
                    0.4 * far(sample - delay)
                } else {
                    0.0
                }
            });
            let mut event = frame(index);
            event.samples = aec.process(&render, &capture, 80).unwrap();
            event.captured_at = Some(at + Duration::from_millis(index as u64 * 10));
            assert!(
                detector.observe(&event, true).is_none(),
                "echo-only onset at synthetic frame {index}"
            );
        }
        assert!(detector.candidate().is_none());
    }
}

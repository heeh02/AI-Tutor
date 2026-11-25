//! Acoustic endpoint candidate; explicit capture integration owns stream closure.
//!
//! A terminal is a *candidate*: only the capture owner can close its input and
//! publish `input_closed: true`. Time here is received audio, not wall-clock time.
//! Never use this detector alone as permission to send text or reopen a device.
use serde::Serialize;
use sonora_agc2::vad_wrapper::VoiceActivityDetectorWrapper;

pub const METHOD_ID: &str = "voice_endpoint_v1";
pub const SAMPLE_RATE: usize = 16_000;
pub const FRAME_SAMPLES: usize = 160;
pub const FRAME_MS: u32 = 10;
pub type Frame = [f32; FRAME_SAMPLES];

const ONSET_PROBABILITY: f32 = 0.60;
const ONSET_WINDOW_FRAMES: u32 = 20;
const ONSET_REQUIRED_FRAMES: u32 = 12;
const ACTIVITY_PROBABILITY: f32 = 0.35;
const ENDING_QUIET_MS: u32 = 2_500;
const NO_SPEECH_MS: u32 = 10_000;
const DURATION_LIMIT_MS: u32 = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointReason {
    SpeechEnd,
    NoSpeech,
    DurationLimit,
    DeviceError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EndpointCandidate {
    pub recording_generation: u64,
    pub reason: EndpointReason,
    pub elapsed_audio_ms: u32,
    /// Confirmation time of the rolling-window rule, not the first spoken word.
    pub speech_confirmed_at_ms: Option<u32>,
    pub last_activity_at_ms: Option<u32>,
}

#[derive(Debug)]
struct EndpointState {
    generation: u64,
    elapsed_ms: u32,
    onset_window: u32,
    speech_confirmed_at_ms: Option<u32>,
    last_activity_at_ms: Option<u32>,
    terminal: Option<EndpointCandidate>,
}

impl EndpointState {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            elapsed_ms: 0,
            onset_window: 0,
            speech_confirmed_at_ms: None,
            last_activity_at_ms: None,
            terminal: None,
        }
    }

    fn accepts(&self, generation: u64) -> bool {
        self.generation == generation && self.terminal.is_none()
    }

    fn finish(&mut self, generation: u64, reason: EndpointReason) -> Option<EndpointCandidate> {
        if !self.accepts(generation) {
            return None;
        }
        let candidate = EndpointCandidate {
            recording_generation: self.generation,
            reason,
            elapsed_audio_ms: self.elapsed_ms,
            speech_confirmed_at_ms: self.speech_confirmed_at_ms,
            last_activity_at_ms: self.last_activity_at_ms,
        };
        self.terminal = Some(candidate);
        Some(candidate)
    }

    fn probability(&mut self, generation: u64, probability: f32) -> Option<EndpointCandidate> {
        if !self.accepts(generation) {
            return None;
        }
        if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
            return self.finish(generation, EndpointReason::DeviceError);
        }
        self.elapsed_ms += FRAME_MS;
        self.onset_window = ((self.onset_window << 1)
            | u32::from(probability >= ONSET_PROBABILITY))
            & ((1 << ONSET_WINDOW_FRAMES) - 1);
        if self.speech_confirmed_at_ms.is_none()
            && self.onset_window.count_ones() >= ONSET_REQUIRED_FRAMES
        {
            self.speech_confirmed_at_ms = Some(self.elapsed_ms);
        }
        if self.speech_confirmed_at_ms.is_some() && probability >= ACTIVITY_PROBABILITY {
            self.last_activity_at_ms = Some(self.elapsed_ms);
        }

        // A hard duration boundary must not be upgraded into automatic sending.
        if self.elapsed_ms >= DURATION_LIMIT_MS {
            self.finish(generation, EndpointReason::DurationLimit)
        } else if let Some(last_activity) = self.last_activity_at_ms {
            if self.elapsed_ms - last_activity >= ENDING_QUIET_MS {
                self.finish(generation, EndpointReason::SpeechEnd)
            } else {
                None
            }
        } else if self.elapsed_ms >= NO_SPEECH_MS {
            self.finish(generation, EndpointReason::NoSpeech)
        } else {
            None
        }
    }
}

/// One VAD and one endpoint state per recording generation. No shared history.
#[derive(Debug)]
pub struct VoiceEndpoint {
    state: EndpointState,
    vad: VoiceActivityDetectorWrapper,
    last_probability: Option<f32>,
    backend_name: &'static str,
}

impl VoiceEndpoint {
    pub fn new(recording_generation: u64) -> Self {
        let backend = sonora_simd::detect_backend();
        Self {
            state: EndpointState::new(recording_generation),
            vad: VoiceActivityDetectorWrapper::new(backend, SAMPLE_RATE as i32),
            last_probability: None,
            backend_name: backend.name(),
        }
    }

    /// Input is unchanged. Only this stack-sized analysis copy uses FloatS16.
    /// Call exactly once per complete 10ms mono frame; never pad missing capture.
    pub fn process_10ms(
        &mut self,
        recording_generation: u64,
        input: &Frame,
    ) -> Option<EndpointCandidate> {
        if !self.state.accepts(recording_generation) {
            return None;
        }
        if input.iter().any(|sample| !sample.is_finite()) {
            return self.device_error(recording_generation);
        }
        let mut analysis = [0.0; FRAME_SAMPLES];
        for (dst, src) in analysis.iter_mut().zip(input) {
            *dst = (src * 32768.0).clamp(-32768.0, 32767.0);
        }
        let probability = self.vad.analyze(&analysis);
        self.last_probability = Some(probability);
        self.state.probability(recording_generation, probability)
    }

    pub fn device_error(&mut self, recording_generation: u64) -> Option<EndpointCandidate> {
        self.state
            .finish(recording_generation, EndpointReason::DeviceError)
    }

    /// The capture owner's wall/sample cap can precede analysis of all frames.
    /// Keep actual analysed audio time; never fill missing frames with silence.
    pub fn duration_limit(&mut self, recording_generation: u64) -> Option<EndpointCandidate> {
        self.state
            .finish(recording_generation, EndpointReason::DurationLimit)
    }

    pub fn terminal(&self) -> Option<EndpointCandidate> {
        self.state.terminal
    }

    pub fn last_probability(&self) -> Option<f32> {
        self.last_probability
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_duration_limit_keeps_audio_time_and_is_generation_bound_once() {
        let mut detector = VoiceEndpoint::new(17);
        assert!(detector.duration_limit(16).is_none());
        detector.process_10ms(17, &[0.0; FRAME_SAMPLES]);
        let end = detector.duration_limit(17).unwrap();
        assert_eq!(end.reason, EndpointReason::DurationLimit);
        assert_eq!(end.elapsed_audio_ms, 10);
        assert!(detector.duration_limit(17).is_none());
        assert!(detector.device_error(17).is_none());
        assert!(detector.process_10ms(17, &[0.0; FRAME_SAMPLES]).is_none());
    }

    fn advance(
        state: &mut EndpointState,
        frames: usize,
        probability: f32,
    ) -> Option<EndpointCandidate> {
        let mut terminal = None;
        for _ in 0..frames {
            if let Some(candidate) = state.probability(17, probability) {
                assert!(
                    terminal.replace(candidate).is_none(),
                    "terminal emitted twice"
                );
            }
        }
        terminal
    }

    #[test]
    fn silence_ends_at_ten_seconds_once() {
        let mut state = EndpointState::new(17);
        assert!(advance(&mut state, 999, 0.0).is_none());
        let end = advance(&mut state, 1, 0.0).unwrap();
        assert_eq!(end.reason, EndpointReason::NoSpeech);
        assert_eq!(end.elapsed_audio_ms, 10_000);
        assert!(advance(&mut state, 2_000, 1.0).is_none());
        assert!(state.finish(17, EndpointReason::DeviceError).is_none());
        assert_eq!(state.terminal, Some(end));
    }

    #[test]
    fn onset_requires_twelve_frames_in_one_two_hundred_ms_window() {
        let mut state = EndpointState::new(17);
        advance(&mut state, 11, 0.9);
        advance(&mut state, 20, 0.0);
        advance(&mut state, 11, 0.9);
        assert!(state.speech_confirmed_at_ms.is_none());
        advance(&mut state, 1, 0.9);
        assert_eq!(state.speech_confirmed_at_ms, Some(430));
    }

    #[test]
    fn onset_window_tolerates_short_gaps_but_not_cumulative_old_noise() {
        let mut state = EndpointState::new(17);
        for _ in 0..3 {
            advance(&mut state, 4, 0.7);
            advance(&mut state, 2, 0.1);
        }
        assert_eq!(state.speech_confirmed_at_ms, Some(160));
        assert_eq!(state.last_activity_at_ms, Some(160));
    }

    #[test]
    fn two_second_thinking_pause_is_retained_then_activity_restarts_quiet_timer() {
        let mut state = EndpointState::new(17);
        advance(&mut state, 40, 0.8);
        assert!(advance(&mut state, 200, 0.1).is_none());
        advance(&mut state, 1, 0.4); // Activity hysteresis; no new onset required.
        assert!(advance(&mut state, 249, 0.1).is_none());
        let end = advance(&mut state, 1, 0.1).unwrap();
        assert_eq!(end.reason, EndpointReason::SpeechEnd);
        assert_eq!(end.elapsed_audio_ms, 4_910);
        assert_eq!(end.last_activity_at_ms, Some(2_410));
    }

    #[test]
    fn long_thinking_pause_exposes_acoustic_ambiguity_instead_of_resuming_terminal() {
        let mut state = EndpointState::new(17);
        advance(&mut state, 20, 1.0);
        let end = advance(&mut state, 400, 0.0).unwrap();
        assert_eq!(end.reason, EndpointReason::SpeechEnd);
        assert_eq!(end.elapsed_audio_ms, 2_700);
        assert!(advance(&mut state, 200, 1.0).is_none());
    }

    #[test]
    fn duration_limit_wins_over_simultaneous_quiet_endpoint() {
        let mut state = EndpointState::new(17);
        advance(&mut state, 1_750, 0.9);
        assert!(advance(&mut state, 249, 0.0).is_none());
        let end = advance(&mut state, 1, 0.0).unwrap();
        assert_eq!(end.reason, EndpointReason::DurationLimit);
        assert_eq!(end.elapsed_audio_ms, 20_000);
    }

    #[test]
    fn confirmed_speech_before_no_speech_deadline_remains_a_speech_turn() {
        let mut state = EndpointState::new(17);
        advance(&mut state, 980, 0.0);
        advance(&mut state, 20, 1.0);
        assert!(state.terminal.is_none());
        let end = advance(&mut state, 250, 0.0).unwrap();
        assert_eq!(end.reason, EndpointReason::SpeechEnd);
    }

    #[test]
    fn old_generation_cannot_advance_or_terminate_new_recording() {
        let mut state = EndpointState::new(17);
        for _ in 0..2_100 {
            assert!(state.probability(16, 1.0).is_none());
        }
        assert!(state.finish(16, EndpointReason::DeviceError).is_none());
        assert_eq!(state.elapsed_ms, 0);
        assert!(state.speech_confirmed_at_ms.is_none());
        let end = state.finish(17, EndpointReason::DeviceError).unwrap();
        assert_eq!(end.recording_generation, 17);
        assert!(state.finish(17, EndpointReason::DeviceError).is_none());
    }

    #[test]
    fn invalid_analysis_probability_is_not_silently_interpreted_as_no_speech() {
        let mut state = EndpointState::new(17);
        let end = state.probability(17, f32::NAN).unwrap();
        assert_eq!(end.reason, EndpointReason::DeviceError);
        assert_eq!(end.elapsed_audio_ms, 0);
    }

    #[test]
    fn real_vad_preserves_input_and_ignores_stale_invalid_frames() {
        let mut detector = VoiceEndpoint::new(17);
        let invalid = [f32::NAN; FRAME_SAMPLES];
        assert!(detector.process_10ms(16, &invalid).is_none());
        assert!(detector.last_probability().is_none());
        let mut input = [0.0; FRAME_SAMPLES];
        input[0] = -1.0;
        input[1] = 1.0;
        input[2] = 0.125;
        let original = input;
        detector.process_10ms(17, &input);
        assert_eq!(input, original);
        assert!(detector.last_probability().unwrap().is_finite());
        assert!(!detector.backend_name().is_empty());
        let end = detector.process_10ms(17, &invalid).unwrap();
        assert_eq!(end.reason, EndpointReason::DeviceError);
        assert_eq!(detector.terminal(), Some(end));
        assert!(detector.process_10ms(17, &original).is_none());
    }

    #[test]
    fn real_rnn_silence_reaches_no_speech_without_asr_or_energy_shortcuts() {
        let mut detector = VoiceEndpoint::new(17);
        let mut end = None;
        for _ in 0..1_000 {
            if let Some(candidate) = detector.process_10ms(17, &[0.0; FRAME_SAMPLES]) {
                assert!(end.replace(candidate).is_none());
            }
        }
        assert_eq!(end.unwrap().reason, EndpointReason::NoSpeech);
    }

    #[test]
    fn frozen_method_and_operating_constants_cannot_silently_diverge() {
        let method: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/voice_endpoint_v1/method.json"
        ))
        .unwrap();
        let endpoint = &method["endpoint"];
        assert_eq!(method["method_id"], METHOD_ID);
        assert_eq!(method["vad"]["sample_rate_hz"], SAMPLE_RATE);
        assert_eq!(method["vad"]["frame_samples"], FRAME_SAMPLES);
        assert_eq!(method["vad"]["frame_ms"], FRAME_MS);
        assert_eq!(
            endpoint["onset_probability"].as_f64().unwrap() as f32,
            ONSET_PROBABILITY
        );
        assert_eq!(
            endpoint["activity_probability"].as_f64().unwrap() as f32,
            ACTIVITY_PROBABILITY
        );
        assert_eq!(endpoint["onset_window_frames"], ONSET_WINDOW_FRAMES);
        assert_eq!(endpoint["onset_required_frames"], ONSET_REQUIRED_FRAMES);
        assert_eq!(endpoint["ending_quiet_ms"], ENDING_QUIET_MS);
        assert_eq!(endpoint["no_speech_ms"], NO_SPEECH_MS);
        assert_eq!(endpoint["duration_limit_ms"], DURATION_LIMIT_MS);
    }
}

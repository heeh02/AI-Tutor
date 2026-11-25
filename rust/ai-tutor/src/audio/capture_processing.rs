//! Optional capture-owner AEC. Device callbacks only record bounded timing facts.
//! VAD and every ASR path consume this same 16 kHz output; no speech interruption.
use super::{
    barge_in::{BargeInOnset, OnsetCandidate, ProcessedFrame},
    echo_cancel::{EchoCanceller16k, FRAME_SAMPLES, Frame, SAMPLE_RATE},
    render_reference::{
        ECHO_TAIL, ReferenceBlock, ReferenceMode, ReferenceReader, ReferenceState, RenderReference,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::File,
    io::Read,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

const TIMING_CAPACITY: usize = 128;
const MAX_CLEAN_SAMPLES: usize = SAMPLE_RATE * 20;
const TIMING_TOLERANCE: Duration = Duration::from_millis(2);

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Off,
    RenderReferenceAecV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    protocol_version: u8,
    mode: Mode,
    #[serde(default)]
    barge_in: bool,
}

pub(super) struct ProcessingPolicy {
    mode: Mode,
    barge_in: bool,
    error: Option<&'static str>,
}

impl ProcessingPolicy {
    pub(super) fn load(root: &Path) -> Self {
        let file = match File::open(root.join("runtime/audio-processing-policy.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self {
                    mode: Mode::Off,
                    barge_in: false,
                    error: None,
                };
            }
            Err(_) => {
                return Self {
                    mode: Mode::Off,
                    barge_in: false,
                    error: Some("audio_processing_policy_unreadable"),
                };
            }
        };
        let read = (|| -> Option<PolicyFile> {
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes).ok()?;
            if bytes.len() > 4096 {
                return None;
            }
            let value: PolicyFile = serde_json::from_slice(&bytes).ok()?;
            (value.protocol_version == 1
                && (!value.barge_in || matches!(value.mode, Mode::RenderReferenceAecV1)))
            .then_some(value)
        })();
        match read {
            Some(value) => Self {
                mode: value.mode,
                barge_in: value.barge_in,
                error: None,
            },
            None => Self {
                mode: Mode::Off,
                barge_in: false,
                error: Some("audio_processing_policy_invalid"),
            },
        }
    }
    pub(super) fn enabled(&self) -> bool {
        matches!(self.mode, Mode::RenderReferenceAecV1) && self.error.is_none()
    }
    pub(super) fn barge_in_enabled(&self) -> bool {
        self.enabled() && self.barge_in
    }
    pub(super) fn status(&self) -> Value {
        json!({"mode":if self.enabled() {"render_reference_aec_v1"} else {"off"},
            "enabled":self.enabled(),"phase":if self.error.is_some() {"configuration_error"} else if self.enabled() {"configured"} else {"disabled"},
            "error_code":self.error,"startup_fixed":true,"barge_in_enabled":self.barge_in_enabled(),
            "automatic_interruption":false,
            "acoustic_quality_verified":false})
    }
}

#[derive(Clone, Copy)]
struct CaptureStamp {
    start: usize,
    end: usize,
    rate: u32,
    at: Option<Instant>,
}

pub(super) struct CaptureTiming {
    sender: SyncSender<CaptureStamp>,
    state: Mutex<TimingState>,
    missed: AtomicU64,
}

// Only the consumer acquires this mutex. Callback writes use the separate
// preallocated channel, so timestamp lookup cannot make a callback lose data.
struct TimingState {
    receiver: Receiver<CaptureStamp>,
    stamps: VecDeque<CaptureStamp>,
}
impl TimingState {
    fn drain(&mut self) {
        // The same bound also limits work if callbacks continue during a drain.
        for _ in 0..TIMING_CAPACITY {
            let Ok(stamp) = self.receiver.try_recv() else {
                break;
            };
            if self.stamps.len() == TIMING_CAPACITY {
                self.stamps.pop_front();
            }
            self.stamps.push_back(stamp);
        }
    }
}
impl CaptureTiming {
    pub(super) fn new() -> Arc<Self> {
        let (sender, receiver) = mpsc::sync_channel(TIMING_CAPACITY);
        Arc::new(Self {
            sender,
            state: Mutex::new(TimingState {
                receiver,
                stamps: VecDeque::with_capacity(TIMING_CAPACITY),
            }),
            missed: AtomicU64::new(0),
        })
    }
    /// Called under the raw append lock. Never waits for the capture worker.
    pub(super) fn record(
        &self,
        native_start: usize,
        count: usize,
        rate: u32,
        captured_at: Option<Instant>,
    ) {
        if count == 0 {
            return;
        }
        let Some(end) = native_start.checked_add(count) else {
            self.missed.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if self
            .sender
            .try_send(CaptureStamp {
                start: native_start,
                end,
                rate,
                at: captured_at,
            })
            .is_err()
        {
            self.missed.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn frame_time(&self, next: usize, rate: u32, count: usize) -> Option<Instant> {
        if rate == 0 || count == 0 {
            return None;
        }
        let start_numerator = next as u64 * rate as u64;
        let start = (start_numerator / SAMPLE_RATE as u64) as usize;
        let end =
            (((next + count - 1) as u64 * rate as u64).div_ceil(SAMPLE_RATE as u64) + 1) as usize;
        let mut state = self.state.lock().unwrap();
        state.drain();
        let mut cursor = start;
        let mut first = None;
        for stamp in state.stamps.iter().filter(|stamp| stamp.end > start) {
            if stamp.start > cursor || stamp.rate != rate {
                return None;
            }
            let at = stamp.at? + seconds((cursor - stamp.start) as f64 / rate as f64);
            if let Some(first_at) = first {
                let expected = first_at + seconds((cursor - start) as f64 / rate as f64);
                if distance(at, expected) > TIMING_TOLERANCE {
                    return None;
                }
            } else {
                first = Some(at);
            }
            cursor = stamp.end.min(end);
            if cursor == end {
                // Preserve the fractional native position of this 16 kHz sample.
                return first.map(|at| {
                    at + seconds(
                        (start_numerator % SAMPLE_RATE as u64) as f64
                            / SAMPLE_RATE as f64
                            / rate as f64,
                    )
                });
            }
        }
        None
    }
    fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.drain();
        state.stamps.clear();
    }
}

fn seconds(value: f64) -> Duration {
    Duration::from_secs_f64(value)
}
fn distance(a: Instant, b: Instant) -> Duration {
    if a >= b {
        a.duration_since(b)
    } else {
        b.duration_since(a)
    }
}
fn signed_millis(a: Instant, b: Instant) -> f64 {
    if a >= b {
        a.duration_since(b).as_secs_f64() * 1000.0
    } else {
        -b.duration_since(a).as_secs_f64() * 1000.0
    }
}
fn device_delay(
    render: Instant,
    analyze: Instant,
    process: Instant,
    capture: Instant,
) -> Option<i32> {
    // Sonora/WebRTC's two signed terms must be added before any range decision.
    let delay = signed_millis(render, analyze) + signed_millis(process, capture);
    (delay.is_finite() && (0.0..=500.0).contains(&delay)).then(|| delay.round() as i32)
}

#[derive(Default)]
struct Counts {
    cleaned: u64,
    bypassed: u64,
    idle_silence: u64,
    stopped_tail_silence: u64,
    render_frames: u64,
    native_zero_frames: u64,
    timing_missing: u64,
    invalid_delay: u64,
    stale_reference: u64,
    resets: u64,
    partial_tail: usize,
}

pub(super) struct CaptureProcessing {
    reader: ReferenceReader,
    timing: Arc<CaptureTiming>,
    rate: u32,
    clean: Vec<f32>,
    raw_origin: usize,
    clean_origin: usize,
    next_sample: usize,
    frame_events: VecDeque<ProcessedFrame>,
    frame_applied: bool,
    filter_revision: u64,
    monitor: Option<BargeInOnset>,
    monitor_target_valid: bool,
    promoted_origin: Option<usize>,
    blocks: Vec<ReferenceBlock>,
    processor: Option<EchoCanceller16k>,
    stream_id: Option<u64>,
    speech_epoch: Option<u64>,
    render_rate: u32,
    next_device_frame: Option<u64>,
    next_playback_at: Option<Instant>,
    render_partial: Vec<f32>,
    idle_render: Vec<f32>,
    partial_at: Option<Instant>,
    last_render: Option<(Instant, Instant)>,
    last_capture: Option<Instant>,
    last_issue: Option<&'static str>,
    reference_dropped: u64,
    counts: Counts,
    finished: bool,
}

impl CaptureProcessing {
    pub(super) fn new(
        reference: Arc<RenderReference>,
        timing: Arc<CaptureTiming>,
        rate: u32,
    ) -> Self {
        Self {
            reader: reference.reader(),
            timing,
            rate,
            clean: Vec::with_capacity(MAX_CLEAN_SAMPLES),
            raw_origin: 0,
            clean_origin: 0,
            next_sample: 0,
            frame_events: VecDeque::with_capacity(8),
            frame_applied: false,
            filter_revision: 0,
            monitor: None,
            monitor_target_valid: false,
            promoted_origin: None,
            blocks: Vec::with_capacity(512),
            processor: None,
            stream_id: None,
            speech_epoch: None,
            render_rate: 16_000,
            next_device_frame: None,
            next_playback_at: None,
            render_partial: Vec::with_capacity(1920),
            idle_render: vec![0.0; FRAME_SAMPLES],
            partial_at: None,
            last_render: None,
            last_capture: None,
            last_issue: None,
            reference_dropped: 0,
            counts: Counts::default(),
            finished: false,
        }
    }

    fn reset_filter(&mut self) {
        if self.processor.take().is_some() || self.last_render.is_some() {
            self.counts.resets += 1;
            self.filter_revision = self.filter_revision.wrapping_add(1);
        }
        self.last_render = None;
    }
    fn reset_reference(&mut self) {
        self.reset_filter();
        self.render_partial.fill(0.0);
        self.render_partial.clear();
        self.partial_at = None;
        self.next_device_frame = None;
        self.next_playback_at = None;
    }
    fn ensure_processor(&mut self) -> bool {
        if self.processor.is_none() {
            self.processor = EchoCanceller16k::with_render_rate(self.render_rate).ok();
            if self.processor.is_some() {
                self.filter_revision = self.filter_revision.wrapping_add(1);
            }
        }
        self.processor.is_some()
    }
    fn feed_render(&mut self, block: &ReferenceBlock) {
        if block.len == 0
            || block.len > block.samples.len()
            || !(8_000..=192_000).contains(&block.rate)
            || block.rate % 100 != 0
        {
            self.reset_reference();
            self.last_issue = Some("reference_format_invalid");
            return;
        }
        let new_stream = self.stream_id != Some(block.stream_id)
            || self.render_rate != block.rate
            || self.speech_epoch != Some(block.speech_epoch);
        if new_stream
            || self
                .next_device_frame
                .is_some_and(|next| next != block.first_device_frame)
        {
            self.reset_reference();
        }
        self.stream_id = Some(block.stream_id);
        self.speech_epoch = Some(block.speech_epoch);
        if self.render_rate != block.rate {
            self.idle_render.resize(block.rate as usize / 100, 0.0);
        }
        self.render_rate = block.rate;
        self.next_device_frame = Some(block.first_device_frame + block.len as u64);
        let Some(block_at) = block.predicted_playback_at else {
            self.reset_reference();
            self.last_issue = Some("reference_time_unknown");
            return;
        };
        if let Some(expected) = self.next_playback_at {
            if distance(expected, block_at) > TIMING_TOLERANCE {
                self.reset_reference();
                self.next_device_frame = Some(block.first_device_frame + block.len as u64);
            }
        }
        self.next_playback_at = Some(block_at + seconds(block.len as f64 / block.rate as f64));
        let frame_size = block.rate as usize / 100;
        let mut offset = 0;
        while offset < block.len {
            if self.render_partial.is_empty() {
                self.partial_at = Some(block_at + seconds(offset as f64 / block.rate as f64));
            }
            let count = (frame_size - self.render_partial.len()).min(block.len - offset);
            self.render_partial
                .extend_from_slice(&block.samples[offset..offset + count]);
            offset += count;
            if self.render_partial.len() == frame_size {
                if !self.ensure_processor() {
                    self.reset_reference();
                    self.last_issue = Some("reference_rate_unsupported");
                    return;
                }
                let analyzed_at = Instant::now();
                if self
                    .processor
                    .as_mut()
                    .unwrap()
                    .push_render(&self.render_partial)
                    .is_err()
                {
                    self.reset_reference();
                    self.last_issue = Some("reference_processing_failed");
                    return;
                }
                self.counts.render_frames += 1;
                if self.render_partial.iter().all(|value| *value == 0.0) {
                    self.counts.native_zero_frames += 1;
                }
                self.last_render = Some((self.partial_at.unwrap(), analyzed_at));
                self.render_partial.clear();
                self.partial_at = None;
            }
        }
    }

    fn drain_reference(&mut self) -> ReferenceState {
        self.drain_reference_at(Instant::now())
    }
    fn drain_reference_at(&mut self, now: Instant) -> ReferenceState {
        let mut blocks = std::mem::take(&mut self.blocks);
        let state = self.reader.drain_into(&mut blocks, now);
        self.reference_dropped = state.dropped_samples;
        if state.discontinuity {
            self.reset_reference();
        }
        if matches!(state.mode, ReferenceMode::Unavailable) {
            self.reset_reference();
            self.last_issue = Some("reference_unavailable");
        } else {
            for block in &blocks {
                self.feed_render(block);
            }
        }
        for block in &mut blocks {
            block.samples.fill(0.0);
        }
        blocks.clear();
        self.blocks = blocks;
        state
    }

    fn capture_frame(
        &mut self,
        frame: &Frame,
        captured_at: Option<Instant>,
        reference: &ReferenceState,
    ) -> Frame {
        self.frame_applied = false;
        let Some(captured_at) = captured_at else {
            self.counts.timing_missing += 1;
            self.reset_filter();
            self.last_capture = None;
            self.last_issue = Some("capture_time_unknown");
            self.counts.bypassed += 1;
            return *frame;
        };
        if self.last_capture.is_some_and(|at| {
            distance(at + Duration::from_millis(10), captured_at) > TIMING_TOLERANCE
        }) {
            self.reset_filter();
            self.last_capture = Some(captured_at);
            self.last_issue = Some("capture_clock_gap");
            self.counts.bypassed += 1;
            return *frame;
        }
        self.last_capture = Some(captured_at);
        if captured_at > Instant::now() {
            self.reset_filter();
            self.last_issue = Some("capture_clock_gap");
            self.counts.bypassed += 1;
            return *frame;
        }
        if matches!(reference.mode, ReferenceMode::Unavailable) {
            self.last_issue = Some("reference_unavailable");
            self.counts.bypassed += 1;
            return *frame;
        }
        if matches!(reference.mode, ReferenceMode::Native)
            && reference.stream_active
            && reference.last_playback_end.is_some_and(|end| {
                captured_at
                    .checked_duration_since(end)
                    .is_some_and(|age| age > ECHO_TAIL)
            })
        {
            self.reset_filter();
            self.last_issue = Some("reference_stale");
            self.counts.stale_reference += 1;
            self.counts.bypassed += 1;
            return *frame;
        }
        let idle = matches!(reference.mode, ReferenceMode::Idle);
        let stopped_tail = matches!(reference.mode, ReferenceMode::Native)
            && !reference.stream_active
            && self.last_render.is_some();
        let after_render = reference
            .last_playback_end
            .map_or(idle, |end| captured_at >= end);
        if (idle || stopped_tail) && after_render {
            // Known silence after the last scheduled callback advances render
            // through the stopped stream's echo tail without resetting adaptation.
            if !self.ensure_processor() {
                self.counts.bypassed += 1;
                return *frame;
            }
            let analyze = Instant::now();
            if self
                .processor
                .as_mut()
                .unwrap()
                .push_render(&self.idle_render)
                .is_err()
            {
                self.reset_filter();
                self.last_issue = Some("reference_processing_failed");
                self.counts.bypassed += 1;
                return *frame;
            }
            self.last_render = Some((captured_at, analyze));
            if idle {
                self.counts.idle_silence += 1;
            } else {
                self.counts.stopped_tail_silence += 1;
            }
        }
        let Some((render, analyze)) = self.last_render else {
            self.last_issue = Some("reference_not_ready");
            self.counts.bypassed += 1;
            return *frame;
        };
        let Some(delay) = device_delay(render, analyze, Instant::now(), captured_at) else {
            self.counts.invalid_delay += 1;
            self.reset_filter();
            self.last_issue = Some("reference_delay_out_of_range");
            self.counts.bypassed += 1;
            return *frame;
        };
        match self
            .processor
            .as_mut()
            .unwrap()
            .process_capture(frame, delay)
        {
            Ok(output) => {
                self.counts.cleaned += 1;
                self.frame_applied = true;
                self.last_issue = None;
                output
            }
            Err(_) => {
                self.reset_filter();
                self.last_issue = Some("capture_processing_failed");
                self.counts.bypassed += 1;
                *frame
            }
        }
    }

    fn sample_count(&self, raw: &[f32]) -> usize {
        let origin = self
            .candidate()
            .map(|value| value.pre_roll_start)
            .or(self.promoted_origin)
            .unwrap_or(if self.monitor.is_some() {
                self.clean_origin
            } else {
                0
            });
        (((self.raw_origin as u64 + raw.len() as u64) * SAMPLE_RATE as u64 / self.rate as u64)
            .min(origin.saturating_add(MAX_CLEAN_SAMPLES) as u64)) as usize
    }
    fn read_frame(&self, raw: &[f32], flush: bool) -> Option<Frame> {
        if self.next_sample + FRAME_SAMPLES > self.sample_count(raw) {
            return None;
        }
        let mut frame = [0.0; FRAME_SAMPLES];
        for (offset, sample) in frame.iter_mut().enumerate() {
            *sample = resampled(
                raw,
                self.raw_origin,
                self.rate,
                self.next_sample + offset,
                flush,
            )?;
        }
        Some(frame)
    }
    fn append_frame(&mut self, frame: &Frame, reference: &ReferenceState, detect_onset: bool) {
        let captured = self
            .timing
            .frame_time(self.next_sample, self.rate, FRAME_SAMPLES);
        let output = self.capture_frame(frame, captured, reference);
        let reference_matches = match reference.mode {
            ReferenceMode::Native => {
                self.last_render.is_some()
                    && self.stream_id == reference.stream_id
                    && self.speech_epoch == reference.speech_epoch
                    && self.stream_id.is_some()
            }
            ReferenceMode::Idle => reference
                .last_playback_end
                .is_none_or(|end| captured.is_some_and(|at| at >= end)),
            ReferenceMode::Unavailable => false,
        };
        let event = ProcessedFrame {
            absolute_start: self.next_sample,
            captured_at: captured,
            samples: output,
            aec_applied: self.frame_applied,
            reference_valid: self.frame_applied && reference_matches && !reference.discontinuity,
            reference_mode: reference.mode,
            stream_id: reference.stream_id,
            speech_epoch: reference.speech_epoch,
            filter_revision: self.filter_revision,
        };
        if detect_onset {
            if let Some(monitor) = self.monitor.as_mut() {
                monitor.observe(&event, self.monitor_target_valid);
            }
        }
        if self.frame_events.len() == 8 {
            if let Some(mut old) = self.frame_events.pop_front() {
                old.samples.fill(0.0);
            }
        }
        self.frame_events.push_back(event);
        self.clean.extend_from_slice(&output);
        self.next_sample += FRAME_SAMPLES;
    }
    fn finish_tail(&mut self, raw: &[f32]) {
        let count = self.sample_count(raw);
        while self.next_sample < count {
            let Some(sample) = resampled(raw, self.raw_origin, self.rate, self.next_sample, true)
            else {
                break;
            };
            self.clean.push(sample);
            self.next_sample += 1;
            self.counts.partial_tail += 1;
        }
        self.finished = true;
    }
    /// Use only for a stable slice (tests or after the input stream has joined).
    pub(super) fn pump(&mut self, raw: &[f32], flush: bool) {
        if self.finished || self.rate == 0 {
            return;
        }
        let reference = self.drain_reference();
        let budget = if flush {
            MAX_CLEAN_SAMPLES / FRAME_SAMPLES
        } else {
            4
        };
        for _ in 0..budget {
            let Some(frame) = self.read_frame(raw, flush) else {
                break;
            };
            self.append_frame(&frame, &reference, !flush);
        }
        if flush {
            self.finish_tail(raw);
        }
    }
    /// Live path: only copy/resample one stack frame under the raw-data lock.
    /// Timing lookup, render analysis and AEC run after that lock is released.
    pub(super) fn pump_shared(&mut self, raw: &Arc<Mutex<Vec<f32>>>, flush: bool) {
        if self.finished || self.rate == 0 {
            return;
        }
        let reference = self.drain_reference();
        let budget = if flush {
            MAX_CLEAN_SAMPLES / FRAME_SAMPLES
        } else {
            4
        };
        for _ in 0..budget {
            let frame = {
                let samples = raw.lock().unwrap();
                self.read_frame(&samples, flush)
            };
            let Some(frame) = frame else {
                break;
            };
            self.append_frame(&frame, &reference, !flush);
        }
        if flush {
            self.finish_tail(&raw.lock().unwrap());
        }
    }
    pub(super) fn samples(&self) -> &[f32] {
        &self.clean
    }
    /// The owner validates its live token/audio epoch before pumping. This only
    /// arms a candidate detector and never starts a device or cancels a reply.
    pub(super) fn enable_monitor(&mut self, expected_audio_epoch: u64, monitor_token: u64) {
        self.monitor = Some(BargeInOnset::new(monitor_token, expected_audio_epoch));
        self.monitor_target_valid = true;
        self.promoted_origin = None;
    }
    pub(super) fn set_monitor_target_valid(&mut self, valid: bool) {
        self.monitor_target_valid = valid;
        if !valid {
            if let Some(monitor) = self.monitor.as_mut() {
                monitor.wipe();
            }
        }
    }
    pub(super) fn candidate(&self) -> Option<OnsetCandidate> {
        self.monitor.as_ref().and_then(BargeInOnset::candidate)
    }
    pub(super) fn promote(&mut self) -> Option<OnsetCandidate> {
        let candidate = self.candidate()?;
        self.trim_before(candidate.pre_roll_start);
        self.promoted_origin = Some(candidate.pre_roll_start);
        self.monitor = None;
        self.monitor_target_valid = false;
        Some(candidate)
    }
    pub(super) fn clean_origin(&self) -> usize {
        self.clean_origin
    }
    pub(super) fn next_sample(&self) -> usize {
        self.next_sample
    }
    pub(super) fn set_raw_origin(&mut self, absolute_native: usize) {
        self.raw_origin = absolute_native;
    }
    /// Raw samples before this absolute cursor are no longer needed by the
    /// resampler. Keeping one boundary sample also covers fractional positions.
    pub(super) fn consumed_native_before(&self) -> usize {
        ((self.next_sample as u128 * self.rate as u128 / SAMPLE_RATE as u128) as usize)
            .saturating_sub(1)
    }
    /// Trim only retained clean history; a latched onset/promotion pins its
    /// prefix. The returned native cursor is independent of that clean prefix.
    pub(super) fn trim_before(&mut self, absolute16k: usize) -> usize {
        let pinned = self
            .candidate()
            .map(|value| value.pre_roll_start)
            .or(self.promoted_origin);
        let until = absolute16k
            .min(self.next_sample)
            .min(pinned.unwrap_or(usize::MAX))
            .max(self.clean_origin);
        let drop = (until - self.clean_origin).min(self.clean.len());
        self.clean[..drop].fill(0.0);
        self.clean.drain(..drop);
        self.clean_origin += drop;
        while self
            .frame_events
            .front()
            .is_some_and(|frame| frame.absolute_start < self.clean_origin)
        {
            if let Some(mut frame) = self.frame_events.pop_front() {
                frame.samples.fill(0.0);
            }
        }
        self.consumed_native_before()
    }
    pub(super) fn drain_frame_events(&mut self, events: &mut Vec<ProcessedFrame>) {
        events.clear();
        events.extend(self.frame_events.drain(..));
    }
    /// Rolling monitor path: the raw origin and slice are read under the same
    /// append lock, while all AEC/onset work stays outside that lock.
    pub(super) fn pump_shared_window(
        &mut self,
        raw: &Arc<Mutex<Vec<f32>>>,
        base: &AtomicU64,
        flush: bool,
    ) {
        if self.finished || self.rate == 0 {
            return;
        }
        let reference = self.drain_reference();
        let budget = if flush {
            MAX_CLEAN_SAMPLES / FRAME_SAMPLES
        } else {
            4
        };
        for _ in 0..budget {
            let frame = {
                let samples = raw.lock().unwrap();
                self.raw_origin = base.load(Ordering::Acquire) as usize;
                self.read_frame(&samples, flush)
            };
            let Some(frame) = frame else {
                break;
            };
            self.append_frame(&frame, &reference, !flush);
        }
        if flush {
            let samples = raw.lock().unwrap();
            self.raw_origin = base.load(Ordering::Acquire) as usize;
            self.finish_tail(&samples);
        }
    }
    pub(super) fn status(&self) -> Value {
        json!({"mode":"render_reference_aec_v1","sample_rate":SAMPLE_RATE,
            "render_sample_rate":self.render_rate,"processed_samples":self.clean.len(),
            "raw_origin":self.raw_origin,"clean_origin":self.clean_origin,"next_sample":self.next_sample,
            "filter_revision":self.filter_revision,"onset_monitor_armed":self.monitor.is_some() && self.monitor_target_valid,
            "onset_candidate":self.candidate().map(|value|json!({"monitor_token":value.monitor_token,
                "audio_epoch":value.audio_epoch,"pre_roll_start":value.pre_roll_start,
                "confirmed_sample":value.confirmed_sample,"stream_id":value.stream_id,
                "filter_revision":value.filter_revision})),
            "aec_frames":self.counts.cleaned,"bypassed_frames":self.counts.bypassed,
            "idle_silence_frames":self.counts.idle_silence,"stopped_tail_silence_frames":self.counts.stopped_tail_silence,
            "native_render_frames":self.counts.render_frames,
            "native_zero_reference_frames":self.counts.native_zero_frames,
            "capture_timing_missing_frames":self.counts.timing_missing,"capture_timing_missed_callbacks":self.timing.missed.load(Ordering::Relaxed),
            "reference_dropped_samples":self.reference_dropped,"invalid_delay_frames":self.counts.invalid_delay,
            "stale_reference_frames":self.counts.stale_reference,
            "resets":self.counts.resets,"unprocessed_tail_samples":self.counts.partial_tail,
            "last_bypass_reason":self.last_issue,"automatic_interruption":false,"acoustic_quality_verified":false})
    }
    pub(super) fn wipe(&mut self) {
        self.clean.fill(0.0);
        self.clean.clear();
        self.reset_reference();
        for block in &mut self.blocks {
            block.samples.fill(0.0);
        }
        self.blocks.clear();
        if let Some(monitor) = self.monitor.as_mut() {
            monitor.wipe();
        }
        self.monitor = None;
        for frame in &mut self.frame_events {
            frame.samples.fill(0.0);
        }
        self.frame_events.clear();
        self.timing.clear();
        self.finished = true;
    }
}
impl Drop for CaptureProcessing {
    fn drop(&mut self) {
        self.wipe();
    }
}

fn resampled(raw: &[f32], raw_origin: usize, rate: u32, index: usize, flush: bool) -> Option<f32> {
    let position = index as u64 * rate as u64;
    let left = ((position / SAMPLE_RATE as u64) as usize).checked_sub(raw_origin)?;
    let fraction = (position % SAMPLE_RATE as u64) as f32 / SAMPLE_RATE as f32;
    let first = *raw.get(left)?;
    if fraction == 0.0 {
        return Some(first);
    }
    let second = match raw.get(left + 1) {
        Some(value) => *value,
        None if flush => first,
        None => return None,
    };
    Some(first + (second - first) * fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_is_opt_in_and_invalid_explicit_configuration_is_visible() {
        let root = tempfile::tempdir().unwrap();
        assert!(!ProcessingPolicy::load(root.path()).enabled());
        std::fs::create_dir(root.path().join("runtime")).unwrap();
        let policy = root.path().join("runtime/audio-processing-policy.json");
        std::fs::write(
            &policy,
            r#"{"protocol_version":1,"mode":"render_reference_aec_v1"}"#,
        )
        .unwrap();
        assert!(ProcessingPolicy::load(root.path()).enabled());
        assert!(!ProcessingPolicy::load(root.path()).barge_in_enabled());
        std::fs::write(
            &policy,
            r#"{"protocol_version":1,"mode":"off","barge_in":true}"#,
        )
        .unwrap();
        assert_eq!(
            ProcessingPolicy::load(root.path()).status()["phase"],
            "configuration_error"
        );
        std::fs::write(
            &policy,
            r#"{"protocol_version":1,"mode":"render_reference_aec_v1","barge_in":true}"#,
        )
        .unwrap();
        assert!(ProcessingPolicy::load(root.path()).barge_in_enabled());
        std::fs::write(
            &policy,
            r#"{"protocol_version":2,"mode":"render_reference_aec_v1"}"#,
        )
        .unwrap();
        let invalid = ProcessingPolicy::load(root.path());
        assert!(!invalid.enabled());
        assert_eq!(invalid.status()["phase"], "configuration_error");
    }

    #[test]
    fn delay_adds_signed_render_and_capture_terms_before_range_check() {
        let at = Instant::now();
        let ms = |value| at + Duration::from_millis(value);
        assert_eq!(device_delay(ms(20), ms(30), ms(50), ms(25)), Some(15));
        assert_eq!(device_delay(ms(20), ms(30), ms(50), ms(45)), None);
        assert_eq!(device_delay(ms(600), ms(0), ms(50), ms(25)), None);
    }

    #[test]
    fn capture_timing_crosses_contiguous_callbacks_but_missing_callback_is_not_silence() {
        let timing = CaptureTiming::new();
        let at = Instant::now();
        timing.record(0, 80, 16_000, Some(at));
        timing.record(80, 80, 16_000, Some(at + Duration::from_millis(5)));
        assert_eq!(timing.frame_time(0, 16_000, 160), Some(at));
        // A native sample-range gap is not filled with synthetic timing.
        timing.record(320, 160, 16_000, Some(at + Duration::from_millis(20)));
        assert_eq!(timing.frame_time(160, 16_000, 160), None);
        assert_eq!(
            timing.frame_time(320, 16_000, 160),
            Some(at + Duration::from_millis(20))
        );
    }

    #[test]
    fn callback_timing_is_queued_while_consumer_state_is_locked() {
        let timing = CaptureTiming::new();
        let at = Instant::now();
        let held = timing.state.lock().unwrap();
        timing.record(0, 160, 16_000, Some(at));
        assert_eq!(timing.missed.load(Ordering::Relaxed), 0);
        drop(held);
        assert_eq!(timing.frame_time(0, 16_000, 160), Some(at));
    }

    #[test]
    fn real_timing_queue_overflow_counts_loss_and_preserves_the_missing_range() {
        let timing = CaptureTiming::new();
        let at = Instant::now();
        for frame in 0..=TIMING_CAPACITY {
            timing.record(
                frame * 160,
                160,
                16_000,
                Some(at + Duration::from_millis(frame as u64 * 10)),
            );
        }
        assert_eq!(timing.missed.load(Ordering::Relaxed), 1);
        assert_eq!(timing.frame_time(0, 16_000, 160), Some(at));
        assert_eq!(timing.frame_time(TIMING_CAPACITY * 160, 16_000, 160), None);
        let next = TIMING_CAPACITY + 1;
        timing.record(
            next * 160,
            160,
            16_000,
            Some(at + Duration::from_millis(next as u64 * 10)),
        );
        assert_eq!(
            timing.frame_time(next * 160, 16_000, 160),
            Some(at + Duration::from_millis(next as u64 * 10))
        );
        assert_eq!(timing.missed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn bypass_keeps_exact_samples_and_partial_tail_across_live_pump_and_finish() {
        let timing = CaptureTiming::new();
        let mut processing = CaptureProcessing::new(RenderReference::new(true), timing, 16_000);
        let raw = Arc::new(Mutex::new(
            (0..1001).map(|i| i as f32 / 2000.0).collect::<Vec<_>>(),
        ));
        // No capture timing exists: even explicit Idle cannot justify synthetic reference.
        processing.pump_shared(&raw, false);
        assert_eq!(processing.samples().len(), 640);
        processing.pump(&raw.lock().unwrap(), true);
        assert_eq!(processing.samples(), raw.lock().unwrap().as_slice());
        assert_eq!(processing.status()["aec_frames"], 0);
        assert_eq!(processing.status()["unprocessed_tail_samples"], 41);
        processing.wipe();
        assert!(processing.samples().is_empty());
    }

    #[test]
    fn native_resampling_is_chunk_independent_during_reference_bypass() {
        let rate = 44_100;
        let raw = (0..882).map(|i| i as f32 / rate as f32).collect::<Vec<_>>();
        let mut processing =
            CaptureProcessing::new(RenderReference::new(false), CaptureTiming::new(), rate);
        for end in [7, 147, 441, 882] {
            processing.pump(&raw[..end], false);
        }
        processing.pump(&raw, true);
        assert_eq!(processing.samples().len(), 320);
        for (index, value) in processing.samples().iter().enumerate() {
            assert!((*value - index as f32 / 16_000.0).abs() < 0.000001);
        }
    }

    #[test]
    fn actual_native_zero_reference_differs_from_confirmed_idle_and_unavailable() {
        for rate in [44_100, 48_000] {
            let reference = RenderReference::new(true);
            let timing = CaptureTiming::new();
            let (mut tap, _stream) = reference.begin_stream(rate, 7).unwrap();
            let now = Instant::now();
            tap.submit(
                &vec![0.0f32; rate as usize / 100],
                1,
                now,
                Some(now + Duration::from_millis(20)),
            );
            timing.record(0, 160, 16_000, Some(now - Duration::from_millis(10)));
            let mut processing = CaptureProcessing::new(reference, timing, 16_000);
            processing.pump(&[0.02; 160], false);
            assert_eq!(processing.samples().len(), 160);
            assert!(processing.samples().iter().all(|v| v.is_finite()));
            assert_eq!(processing.status()["native_zero_reference_frames"], 1);
            assert_eq!(processing.status()["idle_silence_frames"], 0);
            assert_eq!(processing.status()["aec_frames"], 1);
        }
        for enabled in [false, true] {
            let timing = CaptureTiming::new();
            timing.record(
                0,
                160,
                16_000,
                Some(Instant::now() - Duration::from_millis(10)),
            );
            let mut processing =
                CaptureProcessing::new(RenderReference::new(enabled), timing, 16_000);
            processing.pump(&[0.02; 160], false);
            assert_eq!(processing.status()["native_zero_reference_frames"], 0);
            assert_eq!(
                processing.status()["idle_silence_frames"],
                if enabled { 1 } else { 0 }
            );
            if !enabled {
                assert_eq!(processing.samples(), &[0.02; 160]);
            }
        }
    }

    #[test]
    fn native_reference_tail_is_retained_but_hung_output_cannot_stay_valid_forever() {
        for elapsed in [290, 310] {
            let mut processing =
                CaptureProcessing::new(RenderReference::new(true), CaptureTiming::new(), 16_000);
            let base = Instant::now() - Duration::from_millis(500);
            let mut filter = EchoCanceller16k::new();
            filter.push_render(&[0.0; 160]).unwrap();
            processing.processor = Some(filter);
            // These timings give a valid signed delay even when the source is stale.
            processing.last_render = Some((
                base + Duration::from_millis(90),
                base + Duration::from_millis(80),
            ));
            let end = base + Duration::from_millis(100);
            let state = ReferenceState {
                mode: ReferenceMode::Native,
                stream_active: true,
                stream_id: Some(1),
                speech_epoch: Some(7),
                dropped_samples: 0,
                discontinuity: false,
                last_playback_end: Some(end),
            };
            let output = processing.capture_frame(
                &[0.02; 160],
                Some(end + Duration::from_millis(elapsed)),
                &state,
            );
            if elapsed == 290 {
                assert_eq!(processing.counts.cleaned, 1);
                assert!(output.iter().all(|sample| sample.is_finite()));
            } else {
                assert_eq!(output, [0.02; 160]);
                assert_eq!(processing.status()["last_bypass_reason"], "reference_stale");
                assert_eq!(processing.counts.cleaned, 0);
            }
        }
    }

    #[test]
    fn stopped_reference_advances_silence_through_tail_and_idle_without_erasing_adaptation() {
        let reference = RenderReference::new(true);
        let timing = CaptureTiming::new();
        let (mut tap, mut stream) = reference.begin_stream(48_000, 1).unwrap();
        let base = Instant::now();
        tap.submit(&[0.0f32; 480], 1, base, Some(base));
        let mut processing = CaptureProcessing::new(reference.clone(), timing, 16_000);
        processing.drain_reference();
        stream.finish();
        // Callback timestamps cannot precede this hub's creation. Wait for the
        // 10 ms submitted block, then use two already-captured consecutive frames.
        std::thread::sleep(Duration::from_millis(35));
        let capture_at = Instant::now() - Duration::from_millis(20);
        let tail = processing.drain_reference();
        assert_eq!(tail.mode, ReferenceMode::Native);
        assert!(!tail.stream_active);
        let resets = processing.counts.resets;
        processing.capture_frame(&[0.02; 160], Some(capture_at), &tail);
        assert_eq!(processing.counts.stopped_tail_silence, 1);
        assert_eq!(processing.counts.cleaned, 1);
        let idle =
            processing.drain_reference_at(Instant::now() + ECHO_TAIL + Duration::from_millis(10));
        assert_eq!(idle.mode, ReferenceMode::Idle);
        assert!(!idle.discontinuity);
        processing.capture_frame(
            &[0.02; 160],
            Some(capture_at + Duration::from_millis(10)),
            &idle,
        );
        assert_eq!(processing.counts.idle_silence, 1);
        assert_eq!(processing.counts.cleaned, 2);
        assert_eq!(processing.counts.resets, resets);

        let (mut missing, _stream) = reference.begin_stream(48_000, 2).unwrap();
        missing.submit(&[0.4f32; 480], 1, Instant::now(), None);
        let gap = processing.drain_reference();
        assert!(gap.discontinuity);
        let output = processing.capture_frame(
            &[0.02; 160],
            Some(capture_at + Duration::from_millis(20)),
            &gap,
        );
        assert_eq!(output, [0.02; 160]);
        assert_eq!(processing.counts.cleaned, 2);
        assert!(processing.counts.resets > resets);
    }

    #[test]
    fn rolling_monitor_keeps_absolute_resampling_and_timing_beyond_twenty_seconds() {
        let rate = 44_100;
        let timing = CaptureTiming::new();
        let mut processing =
            CaptureProcessing::new(RenderReference::new(false), timing.clone(), rate);
        processing.enable_monitor(7, 13);
        let raw = Arc::new(Mutex::new(Vec::with_capacity(2 * rate as usize)));
        let base = AtomicU64::new(0);
        let mut native_count = 0;
        for _ in 0..550 {
            {
                let mut raw = raw.lock().unwrap();
                raw.extend(
                    (native_count..native_count + 1764)
                        .map(|index| index as f32 / (rate as f32 * 1000.0)),
                );
                timing.record(native_count, 1764, rate, None);
                native_count += 1764;
            }
            processing.pump_shared_window(&raw, &base, false);
            let safe = processing.trim_before(processing.next_sample().saturating_sub(4800));
            {
                let mut raw = raw.lock().unwrap();
                let old = base.load(Ordering::Acquire) as usize;
                assert!(safe >= old && safe - old <= raw.len());
                raw.drain(..safe - old);
                base.store(safe as u64, Ordering::Release);
                processing.set_raw_origin(safe);
                assert!(raw.len() <= 1765);
            }
            assert!(processing.samples().len() <= 4800);
        }
        assert_eq!(processing.next_sample(), 352_000);
        assert_eq!(processing.samples().len(), 4800);
        for (index, sample) in processing.samples().iter().enumerate() {
            let absolute = processing.clean_origin() + index;
            assert!((*sample - absolute as f32 / 16_000_000.0).abs() < 0.000001);
        }
        assert!(processing.candidate().is_none());
        let mut frames = Vec::new();
        processing.drain_frame_events(&mut frames);
        assert!(frames.len() <= 8 && frames.iter().all(|frame| !frame.reference_valid));
    }

    #[test]
    fn normal_capture_still_has_twenty_second_limit_without_monitoring() {
        let mut processing =
            CaptureProcessing::new(RenderReference::new(false), CaptureTiming::new(), 16_000);
        processing.pump(&vec![0.03; 352_000], true);
        assert_eq!(processing.samples().len(), MAX_CLEAN_SAMPLES);
        assert_eq!(processing.clean_origin(), 0);
        assert_eq!(processing.next_sample(), MAX_CLEAN_SAMPLES);
    }

    #[test]
    fn old_queued_render_can_be_processed_but_cannot_qualify_as_new_reply_audio() {
        let reference = RenderReference::new(true);
        let (mut old, _old_stream) = reference.begin_stream(48_000, 7).unwrap();
        let now = Instant::now();
        old.submit(
            &[0.0f32; 480],
            1,
            now,
            Some(now + Duration::from_millis(20)),
        );
        let (_new, _new_stream) = reference.begin_stream(48_000, 8).unwrap();
        let timing = CaptureTiming::new();
        timing.record(0, 160, 16_000, Some(now - Duration::from_millis(10)));
        let mut processing = CaptureProcessing::new(reference, timing, 16_000);
        processing.enable_monitor(8, 13);
        processing.pump(&[0.02; 160], false);
        let mut frames = Vec::new();
        processing.drain_frame_events(&mut frames);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].aec_applied);
        assert!(!frames[0].reference_valid);
        assert_eq!(frames[0].speech_epoch, Some(8));
        assert!(processing.candidate().is_none());
    }
}

//! Bounded reference from the final native output buffer. This is callback
//! submission evidence, not a loopback recording or measured acoustic output.
use cpal::{FromSample, Sample};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

pub(crate) const BLOCK_FRAMES: usize = 480;
const QUEUE_BLOCKS: usize = 512;
const HISTORY: Duration = Duration::from_secs(1);
pub(crate) const ECHO_TAIL: Duration = Duration::from_millis(300);
const MAX_DEVICE_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub(crate) struct ReferenceBlock {
    pub samples: [f32; BLOCK_FRAMES],
    pub len: usize,
    pub stream_id: u64,
    pub speech_epoch: u64,
    pub first_device_frame: u64,
    pub rate: u32,
    pub predicted_playback_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReferenceMode {
    Idle,
    Native,
    Unavailable,
}

#[derive(Clone, Debug)]
pub(crate) struct ReferenceState {
    pub mode: ReferenceMode,
    pub stream_active: bool,
    pub stream_id: Option<u64>,
    pub speech_epoch: Option<u64>,
    /// Native mono frames lost to reference contention/capacity/history expiry.
    pub dropped_samples: u64,
    pub discontinuity: bool,
    pub last_playback_end: Option<Instant>,
}

struct QueuedBlock {
    block: ReferenceBlock,
    submitted_at: Instant,
}

struct Timeline {
    mode: ReferenceMode,
    stream_id: Option<u64>,
    speech_epoch: Option<u64>,
    progress: Option<Arc<StreamProgress>>,
    active: bool,
    final_frame: Option<u64>,
    last_playback_end: Option<Instant>,
    expires_at: Option<Instant>,
}

#[derive(Default)]
struct StreamProgress {
    frames: AtomicU64,
    // Relative to the hub's monotonic origin; zero is unknown, otherwise ns+1.
    last_playback_end: AtomicU64,
}

pub(crate) struct RenderReference {
    enabled: bool,
    origin: Instant,
    next_stream: AtomicU64,
    current_stream: AtomicU64,
    dropped: AtomicU64,
    submitted_samples: AtomicU64,
    submitted_blocks: AtomicU64,
    dequeued_blocks: AtomicU64,
    sender: SyncSender<QueuedBlock>,
    receiver: Mutex<Receiver<QueuedBlock>>,
    timeline: Mutex<Timeline>,
}

impl RenderReference {
    pub(crate) fn new(enabled: bool) -> Arc<Self> {
        let (sender, receiver) = mpsc::sync_channel(if enabled { QUEUE_BLOCKS } else { 0 });
        Arc::new(Self {
            enabled,
            origin: Instant::now(),
            next_stream: AtomicU64::new(1),
            current_stream: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            submitted_samples: AtomicU64::new(0),
            submitted_blocks: AtomicU64::new(0),
            dequeued_blocks: AtomicU64::new(0),
            sender,
            receiver: Mutex::new(receiver),
            timeline: Mutex::new(Timeline {
                mode: if enabled {
                    ReferenceMode::Idle
                } else {
                    ReferenceMode::Unavailable
                },
                stream_id: None,
                speech_epoch: None,
                progress: None,
                active: false,
                final_frame: None,
                last_playback_end: None,
                expires_at: None,
            }),
        })
    }

    /// The output stream may still play when its format is unsuitable for AEC.
    pub(crate) fn begin_stream(
        self: &Arc<Self>,
        rate: u32,
        epoch: u64,
    ) -> Option<(RenderTap, RenderStream)> {
        if !self.enabled {
            return None;
        }
        if !(24_000..=192_000).contains(&rate) || rate % 100 != 0 {
            self.mark_unavailable();
            return None;
        }
        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
        let progress = Arc::new(StreamProgress::default());
        let mut timeline = self.timeline.lock().unwrap();
        self.expire(&mut timeline, Instant::now());
        // No speech-epoch flush: already submitted sound can still echo.
        timeline.mode = ReferenceMode::Native;
        timeline.stream_id = Some(id);
        timeline.speech_epoch = Some(epoch);
        timeline.progress = Some(progress.clone());
        timeline.active = true;
        timeline.final_frame = None;
        timeline.expires_at = None;
        self.current_stream.store(id, Ordering::Release);
        drop(timeline);
        Some((
            RenderTap {
                reference: self.clone(),
                progress: progress.clone(),
                id,
                rate,
                epoch,
                next_frame: 0,
            },
            RenderStream {
                reference: self.clone(),
                progress,
                id,
                finished: false,
            },
        ))
    }

    /// Called by the speech owner for unreferenced output (for example SAPI).
    pub(crate) fn mark_unavailable(&self) {
        self.current_stream.store(0, Ordering::Release);
        let mut timeline = self.timeline.lock().unwrap();
        self.discard_queue();
        timeline.mode = ReferenceMode::Unavailable;
        timeline.active = false;
        timeline.stream_id = None;
        timeline.speech_epoch = None;
        timeline.progress = None;
        timeline.final_frame = None;
        timeline.expires_at = None;
    }

    pub(crate) fn reader(self: &Arc<Self>) -> ReferenceReader {
        ReferenceReader {
            reference: self.clone(),
            stream_id: None,
            next_frame: None,
            observed_stream: None,
            mode: None,
            dropped: 0,
        }
    }

    fn expire(&self, timeline: &mut Timeline, now: Instant) {
        // A reference block can be dropped. Its latest timing must still
        // revoke an older known endpoint before a consumer invents tail zeros.
        if let Some(progress) = &timeline.progress {
            if progress.frames.load(Ordering::Acquire) > 0 {
                let end = progress.last_playback_end.load(Ordering::Acquire);
                timeline.last_playback_end = if end == 0 {
                    None
                } else {
                    self.origin.checked_add(Duration::from_nanos(end - 1))
                };
            }
        }
        if !timeline.active && timeline.expires_at.is_some_and(|deadline| now >= deadline) {
            self.discard_queue();
            timeline.mode = ReferenceMode::Idle;
            timeline.expires_at = None;
            return;
        }
    }

    fn discard_queue(&self) {
        let receiver = self.receiver.lock().unwrap();
        for _ in 0..QUEUE_BLOCKS {
            let Ok(row) = receiver.try_recv() else {
                break;
            };
            self.dequeued_blocks.fetch_add(1, Ordering::Relaxed);
            self.dropped
                .fetch_add(row.block.len as u64, Ordering::Relaxed);
        }
    }

    fn submitted(&self, samples: usize) {
        self.submitted_samples
            .fetch_add(samples as u64, Ordering::Relaxed);
        self.submitted_blocks.fetch_add(1, Ordering::Release);
    }

    // The normal callback path takes neither our metadata nor receiver mutex.
    // On capacity pressure, at most one oldest block is evicted and one retry
    // is attempted. A busy consumer must never make the callback wait.
    fn try_publish(&self, row: QueuedBlock) {
        let samples = row.block.len;
        match self.sender.try_send(row) {
            Ok(()) => self.submitted(samples),
            Err(TrySendError::Full(row)) => {
                let Ok(receiver) = self.receiver.try_lock() else {
                    self.dropped.fetch_add(samples as u64, Ordering::Release);
                    return;
                };
                if let Ok(oldest) = receiver.try_recv() {
                    self.dequeued_blocks.fetch_add(1, Ordering::Relaxed);
                    self.dropped
                        .fetch_add(oldest.block.len as u64, Ordering::Release);
                }
                drop(receiver);
                if self.sender.try_send(row).is_ok() {
                    self.submitted(samples);
                } else {
                    self.dropped.fetch_add(samples as u64, Ordering::Release);
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(samples as u64, Ordering::Release);
            }
        }
    }

    pub(crate) fn status(&self) -> Value {
        let now = Instant::now();
        let mut timeline = self.timeline.lock().unwrap();
        self.expire(&mut timeline, now);
        let queued = self
            .submitted_blocks
            .load(Ordering::Acquire)
            .saturating_sub(self.dequeued_blocks.load(Ordering::Acquire))
            .min(QUEUE_BLOCKS as u64);
        json!({"enabled":self.enabled,"mode":timeline.mode,"queued_blocks":queued,
            "queue_count_observation":"cumulative_enqueued_minus_dequeued_approximate_during_concurrency",
            "queue_capacity_blocks":if self.enabled {QUEUE_BLOCKS}else{0},"dropped_samples":self.dropped.load(Ordering::Acquire),
            "submitted_samples":self.submitted_samples.load(Ordering::Acquire),
            "submitted_blocks":self.submitted_blocks.load(Ordering::Acquire),
            "dequeued_blocks":self.dequeued_blocks.load(Ordering::Acquire),
            "stream_active":timeline.active,"stream_id":timeline.stream_id,
            "tail_remaining_ms":timeline.expires_at.and_then(|at|at.checked_duration_since(now)).map(|v|v.as_millis()),
            "last_playback_end_age_ms":timeline.last_playback_end.and_then(|at|now.checked_duration_since(at)).map(|v|v.as_millis()),
            "speaker_output_measured":false})
    }
}

/// Moved into the CPAL callback; owns only its sample cursor and preallocated bus.
pub(crate) struct RenderTap {
    reference: Arc<RenderReference>,
    progress: Arc<StreamProgress>,
    id: u64,
    rate: u32,
    epoch: u64,
    next_frame: u64,
}

impl RenderTap {
    /// `data` must be the final interleaved native buffer after all zero-fill
    /// paths. Current PCM output duplicates the same mono sample to all channels.
    pub(crate) fn submit<T: Sample>(
        &mut self,
        data: &[T],
        channels: usize,
        callback_at: Instant,
        predicted_first_sample: Option<Instant>,
    ) where
        f32: FromSample<T>,
    {
        if channels == 0 {
            return;
        }
        // Reject bad device timing for the reference without changing playback
        // or its existing predicted-drain calculation.
        let predicted_first_sample = predicted_first_sample.filter(|at| {
            at.checked_duration_since(callback_at)
                .is_some_and(|delay| delay <= MAX_DEVICE_DELAY)
        });
        let frames = data.len() / channels;
        if frames == 0 {
            return;
        }
        let first = self.next_frame;
        self.next_frame = self.next_frame.saturating_add(frames as u64);
        let end = predicted_first_sample.and_then(|at| {
            at.checked_add(Duration::from_secs_f64(frames as f64 / self.rate as f64))
        });
        let encoded_end = end
            .and_then(|at| at.checked_duration_since(self.reference.origin))
            .map_or(0, |offset| {
                (offset.as_nanos().min(u64::MAX as u128 - 1) as u64) + 1
            });
        self.progress
            .last_playback_end
            .store(encoded_end, Ordering::Release);
        self.progress
            .frames
            .store(self.next_frame, Ordering::Release);
        if self.reference.current_stream.load(Ordering::Acquire) != self.id {
            self.reference
                .dropped
                .fetch_add(frames as u64, Ordering::Release);
            return;
        }
        for at in (0..frames).step_by(BLOCK_FRAMES) {
            let len = (frames - at).min(BLOCK_FRAMES);
            let mut block = ReferenceBlock {
                samples: [0.0; BLOCK_FRAMES],
                len,
                stream_id: self.id,
                speech_epoch: self.epoch,
                first_device_frame: first + at as u64,
                rate: self.rate,
                predicted_playback_at: predicted_first_sample.and_then(|time| {
                    time.checked_add(Duration::from_secs_f64(at as f64 / self.rate as f64))
                }),
            };
            for (target, frame) in block.samples[..len]
                .iter_mut()
                .zip(data[at * channels..(at + len) * channels].chunks_exact(channels))
            {
                *target = frame[0].to_sample::<f32>();
            }
            self.reference.try_publish(QueuedBlock {
                block,
                submitted_at: callback_at,
            });
        }
    }
}

/// Kept by the output owner. Finish only after CPAL stream drop/join.
pub(crate) struct RenderStream {
    reference: Arc<RenderReference>,
    progress: Arc<StreamProgress>,
    id: u64,
    finished: bool,
}

impl RenderStream {
    pub(crate) fn finish(&mut self) {
        self.finish_at(Instant::now());
    }

    fn finish_at(&mut self, now: Instant) {
        if self.finished {
            return;
        }
        self.finished = true;
        let _ = self.reference.current_stream.compare_exchange(
            self.id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let mut timeline = self.reference.timeline.lock().unwrap();
        if timeline.stream_id != Some(self.id) {
            return;
        }
        let frames = self.progress.frames.load(Ordering::Acquire);
        let end = self.progress.last_playback_end.load(Ordering::Acquire);
        timeline.active = false;
        timeline.final_frame = Some(frames);
        if frames > 0 {
            timeline.last_playback_end = if end == 0 {
                None
            } else {
                self.reference
                    .origin
                    .checked_add(Duration::from_nanos(end - 1))
            };
            if timeline.last_playback_end.is_none() {
                timeline.mode = ReferenceMode::Unavailable;
                timeline.expires_at = None;
                return;
            }
        }
        let tail_start = timeline.last_playback_end.map_or(now, |end| end.max(now));
        timeline.expires_at = Some(tail_start + ECHO_TAIL);
        self.reference.expire(&mut timeline, now);
    }
}

impl Drop for RenderStream {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(crate) struct ReferenceReader {
    reference: Arc<RenderReference>,
    stream_id: Option<u64>,
    next_frame: Option<u64>,
    observed_stream: Option<u64>,
    mode: Option<ReferenceMode>,
    dropped: u64,
}

impl ReferenceReader {
    /// One active capture reader; Sonora/resampling and vector allocation occur
    /// outside the output callback. Supplied storage is cleared, then reused.
    pub(crate) fn drain_into(
        &mut self,
        output: &mut Vec<ReferenceBlock>,
        now: Instant,
    ) -> ReferenceState {
        output.clear();
        let mut timeline = self.reference.timeline.lock().unwrap();
        self.reference.expire(&mut timeline, now);
        {
            let receiver = self.reference.receiver.lock().unwrap();
            for _ in 0..QUEUE_BLOCKS {
                let Ok(row) = receiver.try_recv() else {
                    break;
                };
                self.reference
                    .dequeued_blocks
                    .fetch_add(1, Ordering::Relaxed);
                if now
                    .checked_duration_since(row.submitted_at)
                    .is_some_and(|age| age > HISTORY)
                {
                    // Stale, unconsumed references are gaps, never zero audio.
                    self.reference
                        .dropped
                        .fetch_add(row.block.len as u64, Ordering::Relaxed);
                } else {
                    output.push(row.block);
                }
            }
        }
        // Callback progress can advance while this consumer drains the channel.
        self.reference.expire(&mut timeline, now);
        let dropped = self.reference.dropped.load(Ordering::Acquire);
        let mut discontinuity = dropped != self.dropped
            || self.mode.is_some_and(|old| {
                old != timeline.mode
                    && !(old == ReferenceMode::Native && timeline.mode == ReferenceMode::Idle)
            })
            || self
                .observed_stream
                .is_some_and(|old| Some(old) != timeline.stream_id);
        for block in output.iter() {
            if self.stream_id != Some(block.stream_id) {
                discontinuity |= self.stream_id.is_some() || block.first_device_frame != 0;
            } else {
                discontinuity |= self.next_frame != Some(block.first_device_frame);
            }
            discontinuity |= block.predicted_playback_at.is_none();
            self.stream_id = Some(block.stream_id);
            self.next_frame = Some(block.first_device_frame + block.len as u64);
        }
        if !timeline.active
            && timeline.mode != ReferenceMode::Unavailable
            && self.stream_id == timeline.stream_id
        {
            if let Some(end) = timeline.final_frame {
                discontinuity |= self.next_frame != Some(end);
                self.next_frame = Some(end);
            }
        }
        self.observed_stream = timeline.stream_id;
        self.mode = Some(timeline.mode);
        self.dropped = dropped;
        ReferenceState {
            mode: timeline.mode,
            stream_active: timeline.active,
            stream_id: timeline.stream_id,
            speech_epoch: timeline.speech_epoch,
            dropped_samples: dropped,
            discontinuity,
            last_playback_end: timeline.last_playback_end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_native_formats_include_real_zeroes_and_interleaved_channel_frames() {
        let reference = RenderReference::new(true);
        let mut reader = reference.reader();
        let (mut tap, mut stream) = reference.begin_stream(48_000, 7).unwrap();
        let at = Instant::now();
        tap.submit(&[0i16, 0, 16384, 16384, -16384, -16384], 2, at, Some(at));
        tap.submit(&[32768u16, 32768, 49152, 49152], 2, at, Some(at));
        tap.submit(&[0.0f32, 0.0, 0.25, 0.25], 2, at, Some(at));
        let mut blocks = Vec::new();
        let state = reader.drain_into(&mut blocks, at);
        assert!(!state.discontinuity);
        assert_eq!(
            blocks
                .iter()
                .map(|b| b.first_device_frame)
                .collect::<Vec<_>>(),
            [0, 3, 5]
        );
        assert_eq!(&blocks[0].samples[..3], &[0.0, 0.5, -0.5]);
        assert_eq!(&blocks[1].samples[..2], &[0.0, 0.5]);
        assert_eq!(&blocks[2].samples[..2], &[0.0, 0.25]);
        assert!(
            blocks
                .iter()
                .all(|b| b.speech_epoch == 7 && b.rate == 48_000)
        );
        let metadata = reference.status();
        assert_eq!(metadata["queued_blocks"], 0);
        assert_eq!(metadata["submitted_samples"], 7);
        assert_eq!(metadata["submitted_blocks"], 3);
        stream.finish_at(at);
    }

    #[test]
    fn split_callback_preserves_all_frames_and_prediction_offsets() {
        let reference = RenderReference::new(true);
        let (mut tap, _stream) = reference.begin_stream(48_000, 1).unwrap();
        let at = Instant::now();
        let playback = at + Duration::from_millis(20);
        tap.submit(&[0.0f32; 1200], 1, at, Some(playback));
        let mut blocks = Vec::new();
        let state = reference.reader().drain_into(&mut blocks, at);
        assert!(!state.discontinuity);
        assert_eq!(
            blocks.iter().map(|b| b.len).collect::<Vec<_>>(),
            [480, 480, 240]
        );
        assert_eq!(blocks[2].first_device_frame, 960);
        assert_eq!(
            blocks[2].predicted_playback_at,
            Some(playback + Duration::from_millis(20))
        );
        assert_eq!(
            state.last_playback_end,
            Some(playback + Duration::from_millis(25))
        );
    }

    #[test]
    fn metadata_and_receiver_locks_do_not_drop_callback_blocks_while_queue_has_space() {
        let reference = RenderReference::new(true);
        let mut reader = reference.reader();
        let at = Instant::now();
        let (mut tap, mut stream) = reference.begin_stream(48_000, 1).unwrap();
        let lock = reference.timeline.lock().unwrap();
        let receiver_lock = reference.receiver.lock().unwrap();
        tap.submit(&[0.1f32; 480], 1, at, Some(at));
        tap.submit(&[0.2f32; 480], 1, at, Some(at + Duration::from_millis(10)));
        drop(receiver_lock);
        drop(lock);
        let mut blocks = Vec::new();
        let first = reader.drain_into(&mut blocks, at);
        assert!(!first.discontinuity);
        assert_eq!(first.dropped_samples, 0);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].first_device_frame, 0);
        assert_eq!(blocks[1].first_device_frame, 480);
        stream.finish_at(at);
    }

    #[test]
    fn full_queue_eviction_or_final_block_drop_remains_observable() {
        for receiver_busy in [false, true] {
            let reference = RenderReference::new(true);
            let mut reader = reference.reader();
            let at = Instant::now();
            let (mut tap, mut stream) = reference.begin_stream(48_000, 1).unwrap();
            for _ in 0..QUEUE_BLOCKS {
                tap.submit(&[0.0f32; 480], 1, at, Some(at));
            }
            let locked = receiver_busy.then(|| reference.receiver.lock().unwrap());
            tap.submit(&[0.1f32; 480], 1, at, Some(at));
            drop(locked);
            stream.finish_at(at);
            let mut blocks = Vec::new();
            let end = reader.drain_into(&mut blocks, at);
            assert!(end.discontinuity);
            assert_eq!(blocks.len(), QUEUE_BLOCKS);
            assert_eq!(end.dropped_samples, 480);
            assert_eq!(
                blocks[0].first_device_frame,
                if receiver_busy { 0 } else { 480 }
            );
            assert!(!reader.drain_into(&mut blocks, at).discontinuity);
        }
    }

    #[test]
    fn stop_retains_queued_tail_and_a_new_speech_epoch_does_not_flush_it() {
        let reference = RenderReference::new(true);
        let at = Instant::now();
        let mut reader = reference.reader();
        let mut blocks = Vec::new();
        let (mut first, mut stopped) = reference.begin_stream(48_000, 3).unwrap();
        first.submit(&[0.2f32; 480], 1, at, Some(at));
        stopped.finish_at(at);
        let (mut second, mut second_stop) = reference.begin_stream(48_000, 4).unwrap();
        second.submit(&[0.3f32; 480], 1, at, Some(at + Duration::from_millis(10)));
        let active = reader.drain_into(&mut blocks, at);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].speech_epoch, 3);
        assert_eq!(blocks[1].speech_epoch, 4);
        assert!(active.discontinuity);
        second_stop.finish_at(at);
        assert_eq!(
            reader
                .drain_into(&mut blocks, at + Duration::from_millis(200))
                .mode,
            ReferenceMode::Native
        );
        let expired = reader.drain_into(&mut blocks, at + Duration::from_millis(400));
        assert_eq!(expired.mode, ReferenceMode::Idle);
        assert!(expired.last_playback_end.is_some());
    }

    #[test]
    fn unknown_time_or_unreferenced_output_never_becomes_known_native_silence() {
        let reference = RenderReference::new(true);
        let at = Instant::now();
        let (mut tap, _stream) = reference.begin_stream(48_000, 1).unwrap();
        tap.submit(&[0.0f32; 480], 1, at, None);
        let mut blocks = Vec::new();
        let result = reference.reader().drain_into(&mut blocks, at);
        assert!(result.discontinuity);
        assert_eq!(result.mode, ReferenceMode::Native);
        assert!(result.last_playback_end.is_none());
        tap.submit(&[0.0f32; 480], 1, at, Some(at + Duration::from_millis(501)));
        let excessive = reference.reader().drain_into(&mut blocks, at);
        assert!(excessive.discontinuity);
        assert!(blocks[0].predicted_playback_at.is_none());
        assert!(excessive.last_playback_end.is_none());
        reference.mark_unavailable();
        assert_eq!(
            reference.reader().drain_into(&mut blocks, at).mode,
            ReferenceMode::Unavailable
        );
        assert!(reference.begin_stream(22_050, 2).is_none());
        assert_eq!(reference.status()["mode"], "unavailable");
        assert!(
            RenderReference::new(false)
                .begin_stream(48_000, 1)
                .is_none()
        );
    }

    #[test]
    fn expired_undrained_native_reference_is_a_gap_not_idle_silence() {
        let reference = RenderReference::new(true);
        let (mut tap, _stream) = reference.begin_stream(48_000, 1).unwrap();
        let at = Instant::now();
        tap.submit(&[0.2f32; 480], 1, at, Some(at));
        let mut blocks = Vec::new();
        let state = reference
            .reader()
            .drain_into(&mut blocks, at + Duration::from_millis(1100));
        assert!(blocks.is_empty());
        assert_eq!(state.mode, ReferenceMode::Native);
        assert_eq!(state.dropped_samples, 480);
        assert!(state.discontinuity);
        assert_eq!(reference.status()["submitted_samples"], 480);
        assert_eq!(reference.status()["submitted_blocks"], 1);
    }

    #[test]
    fn fully_consumed_stop_tail_becomes_idle_without_resetting_echo_history() {
        let reference = RenderReference::new(true);
        let mut reader = reference.reader();
        let (mut tap, mut stream) = reference.begin_stream(48_000, 1).unwrap();
        let at = Instant::now();
        let mut blocks = Vec::new();
        tap.submit(&[0.2f32; 480], 1, at, Some(at));
        let playing = reader.drain_into(&mut blocks, at);
        assert!(playing.stream_active);
        assert!(!playing.discontinuity);
        stream.finish_at(at + Duration::from_millis(10));
        let tail = reader.drain_into(&mut blocks, at + Duration::from_millis(100));
        assert_eq!(tail.mode, ReferenceMode::Native);
        assert!(!tail.stream_active && !tail.discontinuity);
        let idle = reader.drain_into(&mut blocks, at + Duration::from_millis(400));
        assert_eq!(idle.mode, ReferenceMode::Idle);
        assert!(!idle.stream_active && !idle.discontinuity);
        assert_eq!(idle.dropped_samples, 0);
        assert_eq!(idle.last_playback_end, Some(at + Duration::from_millis(10)));
    }

    #[test]
    fn unread_final_reference_still_reports_a_gap_after_tail_becomes_idle() {
        let reference = RenderReference::new(true);
        let mut reader = reference.reader();
        let (mut tap, mut stream) = reference.begin_stream(48_000, 1).unwrap();
        let at = Instant::now();
        let mut blocks = Vec::new();
        tap.submit(&[0.2f32; 480], 1, at, Some(at));
        assert!(!reader.drain_into(&mut blocks, at).discontinuity);
        tap.submit(&[0.3f32; 480], 1, at, Some(at + Duration::from_millis(10)));
        stream.finish_at(at + Duration::from_millis(20));
        let expired = reader.drain_into(&mut blocks, at + Duration::from_millis(400));
        assert_eq!(expired.mode, ReferenceMode::Idle);
        assert!(!expired.stream_active && expired.discontinuity);
        assert_eq!(expired.dropped_samples, 480);
        assert!(blocks.is_empty());
        assert!(
            !reader
                .drain_into(&mut blocks, at + Duration::from_millis(401))
                .discontinuity
        );
    }

    #[test]
    fn unknown_latest_callback_revokes_a_prior_known_end_even_when_reference_is_dropped() {
        for lose_to_full_queue in [false, true] {
            let reference = RenderReference::new(true);
            let mut reader = reference.reader();
            let (mut tap, mut stream) = reference.begin_stream(48_000, 1).unwrap();
            let at = Instant::now();
            let mut blocks = Vec::new();
            tap.submit(&[0.2f32; 480], 1, at, Some(at));
            assert!(
                reader
                    .drain_into(&mut blocks, at)
                    .last_playback_end
                    .is_some()
            );
            if lose_to_full_queue {
                for _ in 0..QUEUE_BLOCKS {
                    tap.submit(&[0.0f32; 480], 1, at, Some(at));
                }
            }
            let locked = lose_to_full_queue.then(|| reference.receiver.lock().unwrap());
            tap.submit(&[0.3f32; 480], 1, at + Duration::from_millis(10), None);
            drop(locked);
            let untrusted = reader.drain_into(&mut blocks, at + Duration::from_millis(10));
            assert!(untrusted.discontinuity && untrusted.last_playback_end.is_none());
            stream.finish_at(at + Duration::from_millis(20));
            let later = reader.drain_into(&mut blocks, at + Duration::from_millis(1000));
            assert_eq!(later.mode, ReferenceMode::Unavailable);
            assert!(!later.stream_active && later.last_playback_end.is_none());
        }
        // No output differs from submitted output with unknown timing.
        let reference = RenderReference::new(true);
        let (_tap, mut stream) = reference.begin_stream(48_000, 1).unwrap();
        let at = Instant::now();
        stream.finish_at(at);
        assert_eq!(
            reference
                .reader()
                .drain_into(&mut Vec::new(), at + Duration::from_secs(1))
                .mode,
            ReferenceMode::Idle
        );
    }
}

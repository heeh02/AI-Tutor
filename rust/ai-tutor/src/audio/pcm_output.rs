//! Speech-owner-only output device. The callback never waits for producer locks.
//! Callback submission and predicted drain are not a measured speaker receipt.
use super::render_reference::{RenderReference, RenderStream, RenderTap};
use super::streaming_tts::{PACKET_SAMPLES, SAMPLE_RATE};
use anyhow::{Result, anyhow, ensure};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) const RING_SAMPLES: usize = SAMPLE_RATE as usize * 480 / 1000;
const PREBUFFER_SAMPLES: usize = SAMPLE_RATE as usize * 120 / 1000;

#[derive(Clone, Default, Debug, Serialize)]
pub(super) struct PcmStats {
    pub accepted_source_samples: u64,
    pub callback_submitted_frames: u64,
    pub underruns: u64,
    pub callback_lock_misses: u64,
    pub first_callback_submission_ms: Option<f64>,
    pub device_sample_rate: u32,
    pub device_channels: u16,
    pub predicted_output_drained: bool,
    pub speaker_output_measured: bool,
}
struct Buffer {
    samples: VecDeque<i16>,
    phase: f64,
    playing: bool,
    input_done: bool,
    discarded: bool,
    started: Instant,
    last_output_deadline: Option<Instant>,
    stats: PcmStats,
}
impl Buffer {
    fn new(rate: u32, channels: u16) -> Self {
        Self {
            samples: VecDeque::with_capacity(RING_SAMPLES),
            phase: 0.0,
            playing: false,
            input_done: false,
            discarded: false,
            started: Instant::now(),
            last_output_deadline: None,
            stats: PcmStats {
                device_sample_rate: rate,
                device_channels: channels,
                ..Default::default()
            },
        }
    }
    fn push(&mut self, packet: &[i16]) -> bool {
        if self.input_done || self.samples.len() + packet.len() > RING_SAMPLES {
            return false;
        }
        self.samples.extend(packet);
        self.stats.accepted_source_samples += packet.len() as u64;
        true
    }
    fn drained(&self, now: Instant) -> bool {
        !self.discarded
            && self.input_done
            && self.samples.is_empty()
            && self.stats.callback_submitted_frames > 0
            && self
                .last_output_deadline
                .is_some_and(|deadline| now >= deadline)
    }
    fn clear(&mut self) {
        self.discarded |= !self.samples.is_empty() || !self.input_done;
        self.samples.iter_mut().for_each(|sample| *sample = 0);
        self.samples.clear();
        self.playing = false;
    }
    fn render<T: cpal::SizedSample + cpal::FromSample<f32>>(
        &mut self,
        data: &mut [T],
        now: Instant,
        device_delay: Duration,
        valid: impl Fn() -> bool,
    ) {
        data.fill(T::from_sample(0.0));
        if !valid() {
            self.clear();
            return;
        }
        if !self.playing {
            if self.samples.len() >= PREBUFFER_SAMPLES
                || (self.input_done && !self.samples.is_empty())
            {
                self.playing = true
            } else {
                return;
            }
        }
        let rate = self.stats.device_sample_rate;
        let ratio = SAMPLE_RATE as f64 / rate as f64;
        let channels = self.stats.device_channels as usize;
        for (frame_index, frame) in data.chunks_mut(channels).enumerate() {
            if !valid() {
                self.clear();
                break;
            }
            let Some(&first) = self.samples.front() else {
                if !self.input_done {
                    self.stats.underruns += 1;
                    self.playing = false
                }
                break;
            };
            if self.phase > 0.0 && self.samples.len() == 1 && !self.input_done {
                self.stats.underruns += 1;
                self.playing = false;
                break;
            }
            let second = self.samples.get(1).copied().unwrap_or(first);
            let sample = (first as f64 + (second as f64 - first as f64) * self.phase) / 32768.0;
            frame.fill(T::from_sample(sample as f32));
            self.stats.callback_submitted_frames += 1;
            self.stats
                .first_callback_submission_ms
                .get_or_insert(now.duration_since(self.started).as_secs_f64() * 1000.0);
            self.last_output_deadline = Some(
                now + device_delay
                    + Duration::from_secs_f64((frame_index + 1) as f64 / rate as f64),
            );
            self.phase += ratio;
            while self.phase >= 1.0 && !self.samples.is_empty() {
                self.samples.pop_front();
                self.phase -= 1.0
            }
        }
    }
}
pub(super) struct PcmOutput {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Buffer>>,
    halted: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    lock_misses: Arc<AtomicU64>,
    reference_stream: Option<RenderStream>,
    render_reference: Option<Arc<RenderReference>>,
}
impl PcmOutput {
    /// Called only by the dedicated speech owner. Never by the network task.
    pub fn open(
        epoch: Arc<AtomicU64>,
        expected: u64,
        closing: Arc<AtomicBool>,
        render_reference: Option<Arc<RenderReference>>,
    ) -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow!("没有可用的播放设备"))?;
        let supported = device.default_output_config()?;
        let config: cpal::StreamConfig = supported.clone().into();
        ensure!(
            config.channels > 0 && config.sample_rate.0 >= SAMPLE_RATE,
            "播放设备格式不支持本地24kHz语音"
        );
        let buffer = Arc::new(Mutex::new(Buffer::new(
            config.sample_rate.0,
            config.channels,
        )));
        let halted = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let lock_misses = Arc::new(AtomicU64::new(0));
        let (tap, reference_stream) = match render_reference
            .as_ref()
            .and_then(|reference| reference.begin_stream(config.sample_rate.0, expected))
        {
            Some((tap, stream)) => (Some(tap), Some(stream)),
            None => (None, None),
        };
        let make_stream = |format: cpal::SampleFormat| -> Result<cpal::Stream> {
            match format {
                cpal::SampleFormat::F32 => build::<f32>(
                    &device,
                    &config,
                    buffer.clone(),
                    halted.clone(),
                    failed.clone(),
                    lock_misses.clone(),
                    epoch.clone(),
                    expected,
                    closing.clone(),
                    tap,
                ),
                cpal::SampleFormat::I16 => build::<i16>(
                    &device,
                    &config,
                    buffer.clone(),
                    halted.clone(),
                    failed.clone(),
                    lock_misses.clone(),
                    epoch.clone(),
                    expected,
                    closing.clone(),
                    tap,
                ),
                cpal::SampleFormat::U16 => build::<u16>(
                    &device,
                    &config,
                    buffer.clone(),
                    halted.clone(),
                    failed.clone(),
                    lock_misses.clone(),
                    epoch.clone(),
                    expected,
                    closing.clone(),
                    tap,
                ),
                _ => Err(anyhow!("播放设备样本格式不受支持")),
            }
        };
        let stream = make_stream(supported.sample_format())?;
        ensure!(
            epoch.load(Ordering::SeqCst) == expected && !closing.load(Ordering::SeqCst),
            "播放请求已取消"
        );
        stream.play()?;
        Ok(Self {
            stream: Some(stream),
            buffer,
            halted,
            failed,
            lock_misses,
            reference_stream,
            render_reference,
        })
    }
    pub fn push(&self, packet: &[i16]) -> Result<bool> {
        ensure!(
            !packet.is_empty() && packet.len() <= PACKET_SAMPLES,
            "PCM包大小无效"
        );
        Ok(self.buffer.lock().unwrap().push(packet))
    }
    pub fn finish_input(&self) {
        self.buffer.lock().unwrap().input_done = true;
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }
    pub fn drained(&self) -> bool {
        !self.failed() && self.buffer.lock().unwrap().drained(Instant::now())
    }
    pub fn stats(&self) -> PcmStats {
        let buffer = self.buffer.lock().unwrap();
        let mut stats = buffer.stats.clone();
        stats.callback_lock_misses = self.lock_misses.load(Ordering::Relaxed);
        stats.predicted_output_drained = !self.failed() && buffer.drained(Instant::now());
        stats
    }
    /// Join/drop the device stream before inspecting the fallback gate, so an
    /// in-flight callback cannot submit the first PCM after a zero-sample check.
    pub fn stop(&mut self) -> PcmStats {
        self.halted.store(true, Ordering::SeqCst);
        drop(self.stream.take());
        self.finish_reference();
        let stats = self.stats();
        self.buffer.lock().unwrap().clear();
        stats
    }

    fn finish_reference(&mut self) {
        if let Some(mut stream) = self.reference_stream.take() {
            stream.finish();
            if self.failed() {
                if let Some(reference) = &self.render_reference {
                    reference.mark_unavailable();
                }
            }
        }
    }
}
impl Drop for PcmOutput {
    fn drop(&mut self) {
        self.halted.store(true, Ordering::SeqCst);
        drop(self.stream.take());
        self.finish_reference();
    }
}

#[allow(clippy::too_many_arguments)]
fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: Arc<Mutex<Buffer>>,
    halted: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    misses: Arc<AtomicU64>,
    epoch: Arc<AtomicU64>,
    expected: u64,
    closing: Arc<AtomicBool>,
    mut tap: Option<RenderTap>,
) -> Result<cpal::Stream>
where
    f32: cpal::FromSample<T>,
{
    let error = failed.clone();
    let channels = config.channels as usize;
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let now = Instant::now();
            let timestamp = info.timestamp();
            let delay = timestamp.playback.duration_since(&timestamp.callback);
            let valid = || {
                epoch.load(Ordering::SeqCst) == expected
                    && !halted.load(Ordering::SeqCst)
                    && !failed.load(Ordering::SeqCst)
                    && !closing.load(Ordering::SeqCst)
            };
            if let Ok(mut buffer) = buffer.try_lock() {
                buffer.render(data, now, delay.unwrap_or_default(), valid);
            } else {
                data.fill(T::from_sample(0.0));
                misses.fetch_add(1, Ordering::Relaxed);
            }
            // Tap final native values, including every zero-fill path above.
            // Unknown device timestamps remain unknown to the AEC consumer.
            if let Some(tap) = &mut tap {
                tap.submit(
                    data,
                    channels,
                    now,
                    delay.and_then(|delay| now.checked_add(delay)),
                );
            }
        },
        move |_error| {
            error.store(true, Ordering::SeqCst);
        },
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prebuffer_is_bounded_and_short_eof_flushes_without_claiming_audible_completion() {
        let mut buffer = Buffer::new(24000, 1);
        let now = Instant::now();
        assert!(buffer.push(&[1000; PACKET_SAMPLES]));
        let mut output = [1.0f32; PACKET_SAMPLES];
        buffer.render(&mut output, now, Duration::from_millis(10), || true);
        assert!(output.iter().all(|v| *v == 0.0));
        assert_eq!(buffer.stats.callback_submitted_frames, 0);
        buffer.input_done = true;
        buffer.render(&mut output, now, Duration::from_millis(10), || true);
        assert_eq!(buffer.stats.callback_submitted_frames, 480);
        assert!(!buffer.drained(now));
        assert!(buffer.drained(now + Duration::from_millis(31)));
        assert!(!buffer.stats.speaker_output_measured);
        let mut full = Buffer::new(24000, 1);
        assert!(full.push(&vec![0; RING_SAMPLES]));
        assert!(!full.push(&[1]));
    }
    #[test]
    fn device_rate_conversion_preserves_duration_and_replicates_channels() {
        let mut buffer = Buffer::new(48000, 2);
        buffer.push(&[0, 16384]);
        buffer.input_done = true;
        let mut data = [0.0f32; 8];
        buffer.render(&mut data, Instant::now(), Duration::ZERO, || true);
        assert_eq!(data, [0.0, 0.0, 0.25, 0.25, 0.5, 0.5, 0.5, 0.5]);
        assert_eq!(buffer.stats.callback_submitted_frames, 4);
        assert!(buffer.samples.is_empty());
    }
    #[test]
    fn cancellation_silences_queued_pcm_and_zero_pcm_still_counts_as_submitted() {
        let mut buffer = Buffer::new(24000, 1);
        buffer.push(&[0; PACKET_SAMPLES]);
        buffer.input_done = true;
        let mut data = [1.0f32; 80];
        buffer.render(&mut data, Instant::now(), Duration::ZERO, || true);
        assert_eq!(buffer.stats.callback_submitted_frames, 80);
        buffer.render(&mut data, Instant::now(), Duration::ZERO, || false);
        assert!(data.iter().all(|v| *v == 0.0));
        assert!(buffer.samples.is_empty());
        assert_eq!(buffer.stats.callback_submitted_frames, 80);
        assert!(
            !buffer.drained(Instant::now() + Duration::from_secs(1)),
            "Discarded tail is not played completion"
        );
    }
    #[test]
    fn starvation_inserts_silence_and_rebuffers_without_dropping_future_samples() {
        let mut buffer = Buffer::new(24000, 1);
        buffer.push(&vec![1000; PREBUFFER_SAMPLES]);
        let mut data = vec![0.0f32; PREBUFFER_SAMPLES + 480];
        buffer.render(&mut data, Instant::now(), Duration::ZERO, || true);
        assert_eq!(buffer.stats.underruns, 1);
        assert_eq!(
            buffer.stats.callback_submitted_frames,
            PREBUFFER_SAMPLES as u64
        );
        assert!(data[PREBUFFER_SAMPLES..].iter().all(|v| *v == 0.0));
        buffer.push(&[2000; PACKET_SAMPLES]);
        buffer.input_done = true;
        let mut rest = [0.0f32; PACKET_SAMPLES];
        buffer.render(&mut rest, Instant::now(), Duration::ZERO, || true);
        assert!(rest.iter().all(|v| *v == 2000.0 / 32768.0));
    }
}

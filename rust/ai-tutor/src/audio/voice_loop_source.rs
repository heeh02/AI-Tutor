//! Test-build-only fixed PCM source for the real capture loop. It cannot publish
//! an endpoint or a speech receipt. Drop stops and joins its paced producer.
use super::*;
use std::{fs, path::Path, sync::atomic::AtomicUsize, thread::JoinHandle};

const RATE: u32 = 44_100;
const CHUNK: usize = 882;
const TAIL_MS: usize = 4_000;

pub(crate) struct WavSource {
    binding: InputBinding,
    samples: Arc<Vec<f32>>,
    opens: AtomicUsize,
    closes: AtomicUsize,
    active: Arc<AtomicBool>,
    observations: Arc<Mutex<Vec<Value>>>,
}
pub(super) struct Opened {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<usize>>,
    source: Arc<WavSource>,
    failed: Arc<AtomicBool>,
    generation: u64,
    started: Instant,
}
impl Drop for Opened {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let joined = self.thread.take().unwrap().join();
        if joined.is_err() {
            self.failed.store(true, Ordering::SeqCst);
        }
        self.source.active.store(false, Ordering::SeqCst);
        self.source.closes.fetch_add(1, Ordering::SeqCst);
        self.source.observations.lock().unwrap().push(json!({
            "recording_generation":self.generation,"producer_joined":joined.is_ok(),
            "emitted_samples":joined.unwrap_or(0),"sample_rate":RATE,
            "closed_elapsed_ms":self.started.elapsed().as_secs_f64()*1000.0}));
    }
}

impl AudioService {
    pub(crate) fn install_voice_loop_wav(
        &self,
        wav: &Path,
    ) -> Result<(InputBinding, Arc<WavSource>)> {
        anyhow::ensure!(
            self.status()["recording"] != true && !self.asr_busy.load(Ordering::SeqCst),
            "capture must be idle"
        );
        let mut samples = read_fixed_pcm(wav)?;
        // This is an explicit synthetic fixture timeline, not padding missing
        // capture frames. Its finite four-second quiet tail is recorded below.
        samples.extend(std::iter::repeat_n(0.0, 16_000 * TAIL_MS / 1000));
        anyhow::ensure!(
            samples.len() < 16_000 * 20,
            "fixed source exceeds capture cap"
        );
        let binding = self.input_devices.install_fake_binding();
        let source = Arc::new(WavSource {
            binding: binding.clone(),
            samples: Arc::new(resample(&samples, 16_000, RATE)),
            opens: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            active: Arc::new(AtomicBool::new(false)),
            observations: Arc::new(Mutex::new(vec![])),
        });
        self.input_devices.install_voice_loop_source(source.clone());
        Ok((binding, source))
    }
}
impl WavSource {
    pub(crate) fn status(&self) -> Value {
        json!({"opens":self.opens.load(Ordering::SeqCst),"closes":self.closes.load(Ordering::SeqCst),
            "active":self.active.load(Ordering::SeqCst),"sample_rate":RATE,"chunk_ms":20,
            "declared_synthetic_quiet_tail_ms":TAIL_MS,"source_samples":self.samples.len(),
            "closed_streams":self.observations.lock().unwrap().clone(),"physical_microphone":false})
    }
    pub(super) fn matches(&self, selection: &InputSelection) -> bool {
        selection.binding() == Some(&self.binding)
    }
    pub(super) fn open(
        self: &Arc<Self>,
        mode: EndpointMode,
        expected: u64,
        streaming: Option<streaming_asr::CaptureStream>,
        info: &Arc<Mutex<Value>>,
        generation: &Arc<AtomicU64>,
        devices: &input_devices::InputDevices,
    ) -> Result<(Recording, Value)> {
        anyhow::ensure!(
            mode == EndpointMode::VoiceEndpointV1 && streaming.is_some(),
            "real endpoint and ASR required"
        );
        anyhow::ensure!(
            devices.binding_current(Some(&self.binding))
                && generation.load(Ordering::SeqCst) == expected,
            "source binding revoked"
        );
        anyhow::ensure!(
            info.lock().unwrap()["state"] != "speaking",
            "output overlaps capture open"
        );
        anyhow::ensure!(
            !self.active.swap(true, Ordering::SeqCst),
            "two synthetic captures overlap"
        );
        let samples = Arc::new(Mutex::new(Vec::with_capacity(RATE as usize * 20)));
        let failed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let data = samples.clone();
        let pcm = self.samples.clone();
        let cancelled = stop.clone();
        let epoch = generation.clone();
        let thread = match std::thread::Builder::new()
            .name("fixed-wav-source".into())
            .spawn(move || {
                let mut emitted = 0;
                for chunk in pcm.chunks(CHUNK) {
                    // At most one 20ms block per wall-clock period: no accelerated
                    // catch-up after a stalled producer and no unbounded EOF silence.
                    let until = Instant::now() + Duration::from_millis(20);
                    while Instant::now() < until {
                        if cancelled.load(Ordering::SeqCst)
                            || epoch.load(Ordering::SeqCst) != expected
                        {
                            return emitted;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    if cancelled.load(Ordering::SeqCst) || epoch.load(Ordering::SeqCst) != expected
                    {
                        break;
                    }
                    push_samples(&data, chunk.iter().copied(), RATE);
                    emitted += chunk.len();
                }
                emitted
            }) {
            Ok(thread) => thread,
            Err(error) => {
                self.active.store(false, Ordering::SeqCst);
                return Err(error.into());
            }
        };
        self.opens.fetch_add(1, Ordering::SeqCst);
        let stream = Opened {
            stop,
            thread: Some(thread),
            source: self.clone(),
            failed: failed.clone(),
            generation: expected,
            started,
        };
        let recording = Recording {
            stream: Some(InputStream::DiagnosticWav { _source: stream }),
            data: samples,
            rate: RATE,
            started,
            generation: expected,
            failed,
            streaming,
            input_binding: Some(self.binding.clone()),
            endpoint: Some(capture_endpoint::CaptureEndpoint::new(expected)),
            processing: None,
            barge: None,
        };
        let mut state = info.lock().unwrap();
        state["recording"] = json!(true);
        state["state"] = json!("recording");
        state["error"] = Value::Null;
        state["recording_end_reason"] = Value::Null;
        state["recording_endpoint"] = Value::Null;
        state["capture_pending_result"] = json!(false);
        state["endpoint_mode"] = json!(mode);
        Ok((
            recording,
            json!({"sample_rate":RATE,"recording_generation":expected,"endpoint_mode":mode,
            "device_key":self.binding.device_key,"input_topology_revision":self.binding.topology_revision}),
        ))
    }
}

pub(super) fn read_fixed_pcm(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path)?;
    anyhow::ensure!(
        (44..=1_000_000).contains(&bytes.len())
            && &bytes[..4] == b"RIFF"
            && &bytes[8..12] == b"WAVE",
        "invalid fixed WAV"
    );
    let (mut pos, mut format, mut data) = (12, None, None);
    while pos + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into()?) as usize;
        let begin = pos + 8;
        let end = begin
            .checked_add(len)
            .ok_or_else(|| anyhow!("WAV size overflow"))?;
        anyhow::ensure!(end <= bytes.len(), "truncated fixed WAV");
        match &bytes[pos..pos + 4] {
            b"fmt " if len >= 16 => {
                format = Some((
                    u16::from_le_bytes(bytes[begin..begin + 2].try_into()?),
                    u16::from_le_bytes(bytes[begin + 2..begin + 4].try_into()?),
                    u32::from_le_bytes(bytes[begin + 4..begin + 8].try_into()?),
                    u16::from_le_bytes(bytes[begin + 14..begin + 16].try_into()?),
                ))
            }
            b"data" => data = Some((begin, end)),
            _ => {}
        }
        pos = end + len % 2;
    }
    anyhow::ensure!(
        format == Some((1, 1, 16_000, 16)),
        "fixed mono PCM16 16k WAV required"
    );
    let (begin, end) = data.ok_or_else(|| anyhow!("WAV data missing"))?;
    anyhow::ensure!(end > begin && (end - begin) % 2 == 0, "invalid PCM length");
    Ok(bytes[begin..end]
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> Arc<WavSource> {
        Arc::new(WavSource {
            binding: InputBinding {
                device_key: "exact-synthetic-instance".into(),
                topology_revision: 3,
            },
            samples: Arc::new(vec![]),
            opens: AtomicUsize::new(1),
            closes: AtomicUsize::new(0),
            active: Arc::new(AtomicBool::new(true)),
            observations: Arc::new(Mutex::new(vec![])),
        })
    }
    #[test]
    fn source_drop_joins_the_producer_before_publishing_closed() {
        let source = source();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let thread = std::thread::spawn(move || {
            while !cancelled.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(10));
            done.store(true, Ordering::SeqCst);
            123
        });
        let failed = Arc::new(AtomicBool::new(false));
        let opened = Opened {
            stop,
            thread: Some(thread),
            source: source.clone(),
            failed: failed.clone(),
            generation: 4,
            started: Instant::now(),
        };
        assert_eq!(source.status()["closes"], 0);
        drop(opened);
        assert!(finished.load(Ordering::SeqCst));
        assert!(!failed.load(Ordering::SeqCst));
        assert_eq!(source.status()["active"], false);
        assert_eq!(source.status()["closes"], 1);
        assert_eq!(source.status()["closed_streams"][0]["emitted_samples"], 123);
        assert_eq!(
            source.status()["closed_streams"][0]["producer_joined"],
            true
        );
    }
    #[test]
    fn diagnostic_source_rejects_other_binding_or_legacy_instead_of_opening_a_device() {
        let devices = input_devices::InputDevices::new();
        let source = source();
        let exact = InputSelection::Bound(source.binding.clone());
        devices.install_voice_loop_source(source);
        assert!(devices.voice_loop_source(&exact).unwrap().is_some());
        assert!(
            devices
                .voice_loop_source(&InputSelection::Legacy(None))
                .is_err()
        );
        assert!(
            devices
                .voice_loop_source(&InputSelection::Bound(InputBinding {
                    device_key: "different".into(),
                    topology_revision: 3
                }))
                .is_err()
        );
    }
}

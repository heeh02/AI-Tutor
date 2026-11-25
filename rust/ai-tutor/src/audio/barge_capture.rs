//! Capture-owner monitor and same-stream adoption. Onset only latches a
//! candidate; the server detaches its old reply before requesting promotion.
use super::*;

const LEASE: Duration = Duration::from_secs(3);
const ADOPT: Duration = Duration::from_secs(2);
const MONITOR_LIMIT: Duration = Duration::from_secs(120);
const PRE_ROLL: usize = 4_800;

pub(super) struct Window {
    pub(super) base: AtomicU64,
    pub(super) limit: AtomicU64,
    pub(super) overflowed: AtomicBool,
}

pub(super) struct Monitor {
    pub(super) expected_audio_epoch: u64,
    opened: Instant,
    lease_until: Instant,
    candidate_at: Option<Instant>,
    adopted: bool,
    pre_roll_samples: usize,
    pub(super) window: Arc<Window>,
}

impl Monitor {
    fn new(expected_audio_epoch: u64) -> Self {
        let now = Instant::now();
        Self {
            expected_audio_epoch,
            opened: now,
            lease_until: now + LEASE,
            candidate_at: None,
            adopted: false,
            pre_roll_samples: 0,
            window: Arc::new(Window {
                base: AtomicU64::new(0),
                limit: AtomicU64::new(0),
                overflowed: AtomicBool::new(false),
            }),
        }
    }
    pub(super) fn configure_rate(&mut self, rate: u32) {
        self.window.limit.store(rate as u64 * 2, Ordering::Relaxed);
        self.opened_now();
    }
    pub(super) fn opened_now(&mut self) {
        self.opened = Instant::now();
        self.lease_until = self.opened + LEASE;
    }
    pub(super) fn status(
        &self,
        token: u64,
        processing: Option<&capture_processing::CaptureProcessing>,
    ) -> Value {
        let candidate = processing.and_then(|processing| processing.candidate());
        let pre_roll = candidate.map_or(self.pre_roll_samples, |candidate| {
            candidate
                .confirmed_sample
                .saturating_sub(candidate.pre_roll_start)
        });
        json!({"phase":if self.adopted {"adopted"} else if self.candidate_at.is_some() {"candidate"} else {"monitor"},
            "monitoring":!self.adopted,"candidate":self.candidate_at.is_some() && !self.adopted,
            "recording_generation":token,"expected_audio_epoch":self.expected_audio_epoch,
            "pre_roll_samples":pre_roll,"pre_roll_sample_rate":16000,
            "same_input_stream":true,"raw_audio_saved":false})
    }
}

struct Opening<'a> {
    audio: &'a AudioService,
    token: u64,
    armed: bool,
}
impl Drop for Opening<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.audio.discard_recording_expected(self.token);
        }
    }
}

impl AudioService {
    pub(crate) fn barge_in_available(&self) -> bool {
        self.barge_in_enabled
            && !self.closing.load(Ordering::SeqCst)
            && self.info.lock().unwrap()["tts_configured_engine"] == "qwen_pcm"
    }

    pub(crate) async fn start_barge_in_monitor_reserved<F>(
        &self,
        selection: InputSelection,
        expected_audio_epoch: u64,
        on_reserved: F,
    ) -> Result<Value>
    where
        F: FnOnce(u64) -> bool + Send,
    {
        self.validate_endpoint_selection(&selection, EndpointMode::VoiceEndpointV1)?;
        anyhow::ensure!(
            self.barge_in_available(),
            "语音打断需要已启用的本地 Qwen 回声处理"
        );
        anyhow::ensure!(
            self.speech_epoch() == expected_audio_epoch,
            "播报请求已过期"
        );
        let state = self.status();
        anyhow::ensure!(
            state["recording"] != true
                && state["capture_pending_result"] != true
                && state["barge_in"]["monitoring"] != true
                && !self.asr_busy.load(Ordering::SeqCst),
            "输入设备正在使用"
        );
        let token = self
            .record_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let mut opening = Opening {
            audio: self,
            token,
            armed: true,
        };
        self.clear_partial_for_generation(token);
        anyhow::ensure!(
            on_reserved(token) && self.record_generation.load(Ordering::SeqCst) == token,
            "监测请求已取消"
        );
        let (send, receive) = oneshot::channel();
        self.capture
            .try_send(CaptureCommand::Monitor(
                selection,
                token,
                expected_audio_epoch,
                send,
            ))
            .map_err(|_| anyhow!("音频操作繁忙"))?;
        let result = self
            .await_capture_start(token, receive, Duration::from_secs(8))
            .await?;
        anyhow::ensure!(
            self.record_generation.load(Ordering::SeqCst) == token,
            "监测请求已取消"
        );
        opening.armed = false;
        Ok(result)
    }

    pub(crate) fn renew_barge_in_monitor(&self, token: u64) -> Result<()> {
        anyhow::ensure!(
            self.record_generation.load(Ordering::SeqCst) == token,
            "监测请求已过期"
        );
        self.capture
            .try_send(CaptureCommand::RenewMonitor(token))
            .map_err(|_| anyhow!("音频操作繁忙"))
    }

    pub(crate) async fn promote_barge_in_capture(
        &self,
        token: u64,
        expected_audio_epoch: u64,
    ) -> Result<Value> {
        let state = self.status();
        anyhow::ensure!(
            self.barge_in_available()
                && !self.asr_busy.load(Ordering::SeqCst)
                && self.record_generation.load(Ordering::SeqCst) == token
                && self.speech_epoch() == expected_audio_epoch
                && state["barge_in"]["recording_generation"] == token
                && state["barge_in"]["phase"] == "candidate",
            "打断候选已过期"
        );
        // This async caller owns the Tokio context; monitor opening never starts ASR.
        self.reserve_final_asr(token);
        let streaming = streaming_asr::begin(
            self.legacy_asr_descriptor.then_some(self.root.as_path()),
            self.streaming_asr_snapshot(),
            self.info.clone(),
            self.record_generation.clone(),
            token,
        );
        let (send, receive) = oneshot::channel();
        self.capture
            .try_send(CaptureCommand::PromoteMonitor(
                token,
                expected_audio_epoch,
                streaming,
                send,
            ))
            .map_err(|_| anyhow!("音频操作繁忙"))?;
        tokio::time::timeout(Duration::from_secs(3), receive)
            .await
            .map_err(|_| anyhow!("打断接管等待超时"))?
            .map_err(|_| anyhow!("麦克风工作线程已关闭"))?
            .map_err(anyhow::Error::msg)
    }

    pub(crate) async fn stop_barge_in_monitor(&self, token: u64) -> Result<Value> {
        let (send, receive) = oneshot::channel();
        self.capture
            .try_send(CaptureCommand::StopMonitor(token, send))
            .map_err(|_| anyhow!("音频操作繁忙"))?;
        tokio::time::timeout(Duration::from_secs(3), receive)
            .await
            .map_err(|_| anyhow!("监测停止等待超时"))?
            .map_err(|_| anyhow!("麦克风工作线程已关闭"))?
            .map_err(anyhow::Error::msg)
    }
}

pub(super) fn open(
    recording: &mut Option<Recording>,
    selection: InputSelection,
    token: u64,
    audio_epoch: u64,
    info: &Arc<Mutex<Value>>,
    generation: &Arc<AtomicU64>,
    speech: &AtomicU64,
    devices: &input_devices::InputDevices,
    reference: &Option<Arc<render_reference::RenderReference>>,
) -> Result<Value> {
    anyhow::ensure!(
        recording.is_none()
            && generation.load(Ordering::SeqCst) == token
            && speech.load(Ordering::SeqCst) == audio_epoch
            && reference.is_some(),
        "监测请求已过期或输入正在使用"
    );
    let result = open_capture(
        recording,
        selection,
        EndpointMode::VoiceEndpointV1,
        token,
        None,
        info,
        generation,
        devices,
        reference,
        Some(Monitor::new(audio_epoch)),
    )?;
    if generation.load(Ordering::SeqCst) != token || speech.load(Ordering::SeqCst) != audio_epoch {
        discard_input(recording, info);
        bail!("监测请求已取消");
    }
    Ok(result)
}

pub(super) fn renew(recording: &mut Option<Recording>, token: u64) {
    if let Some(monitor) = recording
        .as_mut()
        .filter(|r| r.generation == token)
        .and_then(|r| r.barge.as_mut())
    {
        if !monitor.adopted {
            monitor.lease_until = Instant::now() + LEASE;
        }
    }
}

pub(super) fn stop(
    recording: &mut Option<Recording>,
    token: u64,
    info: &Arc<Mutex<Value>>,
) -> Result<Value> {
    let Some(current) = recording.as_ref().filter(|r| r.generation == token) else {
        return Ok(
            json!({"stopped":false,"stale":true,"onset_pending":false,"recording_generation":token}),
        );
    };
    let monitor = current
        .barge
        .as_ref()
        .ok_or_else(|| anyhow!("输入不是打断监测"))?;
    if monitor.adopted {
        return Ok(
            json!({"stopped":false,"adopted":true,"onset_pending":false,"recording_generation":token}),
        );
    }
    if monitor.candidate_at.is_some() {
        return Ok(json!({"stopped":false,"onset_pending":true,"recording_generation":token}));
    }
    discard_input(recording, info);
    patch(
        info,
        "barge_in",
        json!({"phase":"idle","monitoring":false,"recording_generation":token}),
    );
    Ok(json!({"stopped":true,"onset_pending":false,"recording_generation":token}))
}

pub(super) fn promote(
    recording: &mut Option<Recording>,
    token: u64,
    audio_epoch: u64,
    streaming: Option<streaming_asr::CaptureStream>,
    info: &Arc<Mutex<Value>>,
    generation: &AtomicU64,
    speech: &AtomicU64,
    devices: &input_devices::InputDevices,
    reference: &Option<Arc<render_reference::RenderReference>>,
) -> Result<Value> {
    let r = recording
        .as_mut()
        .filter(|r| r.generation == token)
        .ok_or_else(|| anyhow!("打断输入已过期"))?;
    let monitor = r
        .barge
        .as_mut()
        .ok_or_else(|| anyhow!("输入不是打断监测"))?;
    let now = Instant::now();
    anyhow::ensure!(
        !monitor.adopted
            && monitor.expected_audio_epoch == audio_epoch
            && monitor
                .candidate_at
                .is_some_and(|at| now.duration_since(at) <= ADOPT)
            && now < monitor.lease_until
            && now.duration_since(monitor.opened) < MONITOR_LIMIT
            && generation.load(Ordering::SeqCst) == token
            && devices.binding_current(r.input_binding.as_ref())
            && !r.failed.load(Ordering::SeqCst)
            && !monitor.window.overflowed.load(Ordering::SeqCst),
        "打断候选已过期"
    );
    anyhow::ensure!(
        reference
            .as_ref()
            .is_some_and(|reference| reference.status()["mode"] != "unavailable"),
        "打断参考音频不可用"
    );
    let processing = r
        .processing
        .as_mut()
        .ok_or_else(|| anyhow!("打断回声处理不可用"))?;
    let candidate = processing
        .candidate()
        .ok_or_else(|| anyhow!("打断候选已丢失"))?;
    anyhow::ensure!(
        candidate.monitor_token == token && candidate.audio_epoch == audio_epoch,
        "打断候选归属不符"
    );
    speech
        .compare_exchange(
            audio_epoch,
            audio_epoch.wrapping_add(1),
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .map_err(|_| anyhow!("旧播报已结束或被替换"))?;
    let candidate = processing
        .promote()
        .ok_or_else(|| anyhow!("打断候选已丢失"))?;
    monitor.adopted = true;
    monitor.pre_roll_samples = candidate
        .confirmed_sample
        .saturating_sub(candidate.pre_roll_start);
    r.started = candidate.pre_roll_captured_at;
    r.endpoint = Some(capture_endpoint::CaptureEndpoint::new(token));
    r.streaming = streaming;
    let status = monitor.status(token, Some(processing));
    patch(info, "barge_in", status);
    patch(info, "recording", json!(true));
    patch(info, "state", json!("recording"));
    patch(info, "recording_endpoint", Value::Null);
    patch(info, "capture_pending_result", json!(false));
    let mut result = json!({"adopted":true,"input_continuous":true,"recording_generation":token,
        "sample_rate":16000,"pre_roll_samples":candidate.confirmed_sample.saturating_sub(candidate.pre_roll_start),
        "expected_audio_epoch":audio_epoch,"interrupted_audio_epoch":audio_epoch,"endpoint_mode":EndpointMode::VoiceEndpointV1});
    if let Some(binding) = &r.input_binding {
        result["device_key"] = json!(binding.device_key);
        result["input_topology_revision"] = json!(binding.topology_revision);
    }
    Ok(result)
}

pub(super) fn pump(
    recording: &mut Option<Recording>,
    info: &Arc<Mutex<Value>>,
    speech: &AtomicU64,
) {
    let failure = (|| {
        let r = recording.as_mut()?;
        let monitor = r.barge.as_mut()?;
        let now = Instant::now();
        if monitor.window.overflowed.load(Ordering::SeqCst) {
            return Some("capture_backlog_limit");
        }
        if !monitor.adopted {
            if speech.load(Ordering::SeqCst) != monitor.expected_audio_epoch {
                return Some("speech_epoch_changed");
            }
            if now >= monitor.lease_until {
                return Some("monitor_lease_expired");
            }
            if now.duration_since(monitor.opened) >= MONITOR_LIMIT {
                return Some("monitor_duration_limit");
            }
            if monitor
                .candidate_at
                .is_some_and(|at| now.duration_since(at) > ADOPT)
            {
                return Some("candidate_adoption_timeout");
            }
        }
        let Some(processing) = r.processing.as_mut() else {
            return Some("capture_processing_unavailable");
        };
        processing.set_monitor_target_valid(
            !monitor.adopted && speech.load(Ordering::SeqCst) == monitor.expected_audio_epoch,
        );
        processing.pump_shared_window(&r.data, &monitor.window.base, false);
        if !monitor.adopted && monitor.candidate_at.is_none() && processing.candidate().is_some() {
            monitor.candidate_at = Some(now);
        }
        if !monitor.adopted {
            processing.trim_before(processing.next_sample().saturating_sub(PRE_ROLL));
        }
        let keep = processing.consumed_native_before();
        let mut raw = r.data.lock().unwrap();
        let base = monitor.window.base.load(Ordering::Relaxed) as usize;
        let count = keep.saturating_sub(base).min(raw.len());
        if count > 0 {
            let length = raw.len();
            raw.copy_within(count.., 0);
            raw[length - count..].fill(0.0);
            raw.truncate(length - count);
            monitor
                .window
                .base
                .store((base + count) as u64, Ordering::Release);
            processing.set_raw_origin(base + count);
        }
        drop(raw);
        patch(
            info,
            "barge_in",
            monitor.status(r.generation, Some(processing)),
        );
        patch(info, "capture_processing", processing.status());
        None
    })();
    if let Some(reason) = failure {
        let mut status = recording
            .as_ref()
            .and_then(|r| {
                r.barge
                    .as_ref()
                    .map(|m| m.status(r.generation, r.processing.as_ref()))
            })
            .unwrap_or(json!({}));
        if let Some(current) = recording
            .as_mut()
            .filter(|r| r.barge.as_ref().is_some_and(|m| m.adopted))
        {
            // The ordinary endpoint path closes an adopted stream and publishes
            // its device-error receipt; it must not silently lose a live turn.
            current.failed.store(true, Ordering::SeqCst);
        } else {
            discard_input(recording, info);
        }
        status["phase"] = json!("failed");
        status["monitoring"] = json!(false);
        status["reason"] = json!(reason);
        patch(info, "barge_in", status);
    }
}

pub(super) fn unadopted(recording: &Recording) -> bool {
    recording
        .barge
        .as_ref()
        .is_some_and(|monitor| !monitor.adopted)
}

pub(super) fn sample_clock(recording: &Recording) -> (usize, u32) {
    if recording
        .barge
        .as_ref()
        .is_some_and(|monitor| monitor.adopted)
    {
        (
            recording
                .processing
                .as_ref()
                .map_or(0, |p| p.samples().len()),
            16000,
        )
    } else {
        (recording.data.lock().unwrap().len(), recording.rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropped_monitor_opening_revokes_only_input_and_never_stops_speech_or_starts_asr() {
        let (mut audio, speech) = AudioService::configuration_test_channel(11);
        audio.barge_in_enabled = true;
        patch(&audio.info, "tts_configured_engine", json!("qwen_pcm"));
        let binding = audio.input_devices.install_fake_binding();
        let (commands, received) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        audio.capture = commands;
        let audio = Arc::new(audio);
        let waiting = audio.clone();
        let opening = tokio::spawn(async move {
            waiting
                .start_barge_in_monitor_reserved(InputSelection::Bound(binding), 11, |_| true)
                .await
        });
        let command = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(command) = received.try_recv() {
                    break command;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let CaptureCommand::Monitor(_, token, epoch, response) = command else {
            panic!("monitor command expected");
        };
        assert_eq!(epoch, 11);
        assert_eq!(audio.speech_epoch(), 11);
        assert!(matches!(
            speech.0.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(audio.status()["asr_streaming_state"], "idle");
        opening.abort();
        assert!(opening.await.unwrap_err().is_cancelled());
        assert_eq!(audio.record_generation.load(Ordering::SeqCst), token + 1);
        assert_eq!(audio.speech_epoch(), 11);
        assert!(response.is_closed());
        assert!(matches!(received.try_recv(),Ok(CaptureCommand::Discard(next)) if next==token+1));
    }

    #[test]
    fn completed_reply_stop_preserves_a_candidate_but_renewal_cannot_extend_adoption_forever() {
        let open = Arc::new(AtomicBool::new(true));
        let close = open.clone();
        let raw = Arc::new(Mutex::new(vec![0.2; 4800]));
        let mut monitor = Monitor::new(9);
        monitor.configure_rate(16_000);
        monitor.candidate_at = Some(Instant::now());
        let mut recording = Some(Recording {
            stream: Some(InputStream::Fake {
                _close: TestInputClose(Some(Box::new(move || {
                    close.store(false, Ordering::SeqCst);
                }))),
            }),
            data: raw.clone(),
            rate: 16_000,
            started: Instant::now(),
            generation: 3,
            failed: Arc::new(AtomicBool::new(false)),
            streaming: None,
            input_binding: None,
            endpoint: None,
            processing: None,
            barge: Some(monitor),
        });
        let info = Arc::new(Mutex::new(json!({"recording":false})));
        let pending = stop(&mut recording, 3, &info).unwrap();
        assert_eq!(pending["onset_pending"], true);
        assert!(open.load(Ordering::SeqCst));
        assert_eq!(stop(&mut recording, 2, &info).unwrap()["stale"], true);
        assert!(open.load(Ordering::SeqCst));
        recording
            .as_mut()
            .unwrap()
            .barge
            .as_mut()
            .unwrap()
            .candidate_at = Some(Instant::now() - Duration::from_millis(2100));
        renew(&mut recording, 3);
        pump(&mut recording, &info, &AtomicU64::new(9));
        assert!(recording.is_none() && !open.load(Ordering::SeqCst));
        assert!(raw.lock().unwrap().iter().all(|value| *value == 0.0));
        assert_eq!(
            info.lock().unwrap()["barge_in"]["reason"],
            "candidate_adoption_timeout"
        );
    }
}

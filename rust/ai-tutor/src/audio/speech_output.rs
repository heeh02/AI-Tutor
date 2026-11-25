//! One speech owner serializes SAPI and CPAL; network tasks never own devices.
use super::{
    pcm_output::{PcmOutput, PcmStats},
    streaming_tts::{SynthesisOutcome, TtsClient},
    *,
};
use windows::{
    Win32::{Media::Speech::*, System::Com::*},
    core::PCWSTR,
};
#[cfg(test)]
#[path = "tts_configuration_tests.rs"]
mod configuration_tests;

struct ComGuard;
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}
struct Sapi(ISpVoice);
impl Sapi {
    fn new(info: &Arc<Mutex<Value>>) -> Result<Self> {
        unsafe {
            let voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL)?;
            let tokens = voice_tokens(None)?;
            let mut count = 0;
            tokens.GetCount(&mut count)?;
            let mut names = vec![];
            for index in 0..count {
                let name = tokens.Item(index)?.GetStringValue(PCWSTR::null())?;
                names.push(name.to_string()?);
                CoTaskMemFree(Some(name.0.cast()));
            }
            patch(info, "voices", json!(names));
            Ok(Self(voice))
        }
    }
    fn start(
        &self,
        request: &SpeechRequest,
        generation: &AtomicU64,
        closing: &AtomicBool,
    ) -> Result<bool> {
        unsafe {
            let choices = voice_tokens(Some(if request.language.starts_with("zh") {
                "Language=804"
            } else {
                "Language=409"
            }))?;
            let selected = choices
                .Item(0)
                .map_err(|_| anyhow!("指定语言的本地声音尚未安装"))?;
            self.0.SetVoice(&selected)?;
            self.0.SetRate(request.rate)?;
            let wide: Vec<u16> = request
                .text
                .encode_utf16()
                .filter(|v| *v != 0)
                .chain(Some(0))
                .collect();
            if request.expected != generation.load(Ordering::SeqCst)
                || closing.load(Ordering::SeqCst)
            {
                return Ok(false);
            }
            self.0.Speak(
                PCWSTR(wide.as_ptr()),
                (SPF_ASYNC.0 | SPF_IS_NOT_XML.0 | SPF_PURGEBEFORESPEAK.0) as u32,
                None,
            )?;
        }
        Ok(true)
    }
    fn done(&self) -> Result<bool> {
        unsafe {
            let mut status = SPVOICESTATUS::default();
            self.0.GetStatus(&mut status, std::ptr::null_mut())?;
            Ok(status.dwRunningState == SPRS_DONE.0 as u32)
        }
    }
    fn stop(&self) -> Result<()> {
        unsafe {
            self.0
                .Speak(PCWSTR::null(), SPF_PURGEBEFORESPEAK.0 as u32, None)?;
        }
        Ok(())
    }
}
unsafe fn voice_tokens(filter: Option<&str>) -> Result<IEnumSpObjectTokens> {
    unsafe {
        let category: ISpObjectTokenCategory =
            CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL)?;
        category.SetId(SPCAT_VOICES, false)?;
        let filter = filter.map(|v| v.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
        Ok(category.EnumTokens(
            filter
                .as_ref()
                .map(|v| PCWSTR(v.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            PCWSTR::null(),
        )?)
    }
}
struct AbortTask(tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct PcmActive {
    engine_kind: &'static str,
    output: PcmOutput,
    packets: tokio::sync::mpsc::Receiver<Vec<i16>>,
    completion: oneshot::Receiver<std::result::Result<SynthesisOutcome, String>>,
    _task: AbortTask,
    pending: Option<Vec<i16>>,
    provider_metrics: Option<Value>,
    input_closed: bool,
}
impl PcmActive {
    fn pump(&mut self) -> Result<()> {
        if self.output.failed() {
            bail!("PCM播放设备发生错误")
        }
        if self.provider_metrics.is_none() {
            match self.completion.try_recv() {
                Ok(Ok(SynthesisOutcome::Completed(metrics))) => {
                    self.provider_metrics = Some(metrics)
                }
                Ok(Ok(SynthesisOutcome::Cancelled)) => bail!("PCM合成已取消"),
                Ok(Err(error)) => bail!(error),
                Err(oneshot::error::TryRecvError::Closed) => bail!("PCM合成任务已关闭"),
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        loop {
            if self.pending.is_none() {
                match self.packets.try_recv() {
                    Ok(packet) => self.pending = Some(packet),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        self.input_closed = true;
                        break;
                    }
                }
            }
            if !self.output.push(self.pending.as_ref().unwrap())? {
                break;
            }
            self.pending = None;
        }
        if self.input_closed && self.provider_metrics.is_some() && self.pending.is_none() {
            self.output.finish_input()
        }
        Ok(())
    }
}
enum Engine {
    Sapi,
    Pcm(PcmActive),
}
struct Active {
    request: SpeechRequest,
    engine: Engine,
    began: Instant,
}

fn start_receipt(request: &mut SpeechRequest, engine_kind: &'static str, milestone: &'static str) {
    if let Some(started) = request.started.take() {
        let _ = started.send(SpeechStarted {
            playback_id: request.playback_id.clone(),
            epoch: request.expected,
            engine_kind,
            milestone,
        });
    }
}
fn finish_request(
    request: SpeechRequest,
    state: SpeechEnd,
    engine_kind: &'static str,
    stats: PcmStats,
    reason: Option<String>,
) {
    let _ = request.response.send(Ok(SpeechOutcome {
        playback_id: request.playback_id,
        epoch: request.expected,
        state,
        engine_kind,
        callback_submitted_frames: stats.callback_submitted_frames,
        predicted_output_drained: state == SpeechEnd::Completed && stats.predicted_output_drained,
        speaker_output_measured: false,
        reason,
    }));
}
fn finish_active(
    mut active: Active,
    completed: bool,
    reason: Option<String>,
    sapi: Option<&Sapi>,
    info: &Arc<Mutex<Value>>,
) -> Result<()> {
    let (engine, stats) = match active.engine {
        Engine::Pcm(mut pcm) => {
            let stats = pcm.output.stop();
            if stats.callback_submitted_frames > 0 {
                start_receipt(
                    &mut active.request,
                    pcm.engine_kind,
                    "first_callback_submission",
                )
            }
            patch(info, "pcm_playback", json!(stats));
            (pcm.engine_kind, stats)
        }
        Engine::Sapi => {
            if !completed {
                if let Some(sapi) = sapi {
                    sapi.stop()?;
                }
            }
            ("sapi", PcmStats::default())
        }
    };
    patch(info, "speech_ms", json!(active.began.elapsed().as_millis()));
    patch(info, "state", json!("idle"));
    finish_request(
        active.request,
        if completed {
            SpeechEnd::Completed
        } else {
            SpeechEnd::Interrupted
        },
        engine,
        stats,
        reason,
    );
    Ok(())
}
fn fallback_allowed(submitted: u64, expected: u64, current: u64, closing: bool) -> bool {
    submitted == 0 && expected == current && !closing
}
fn stop_targets_active(stop_epoch: u64, active_epoch: u64, current_epoch: u64) -> bool {
    stop_epoch == current_epoch && active_epoch != stop_epoch
}
pub(super) fn configure_tts_slot(
    client: &mut Option<Arc<TtsClient>>,
    request: TtsConfigurationRequest,
    current_epoch: u64,
    busy: bool,
    closing: bool,
) -> Option<TtsConfigurationReceipt> {
    // A timed-out/cancelled installer must not change a client later in the queue.
    if request.response.is_closed() {
        return None;
    }
    // Cancellation cannot return between this token check and the client swap.
    // The marker lets a later cancellation distinguish an already committed install.
    let mut committed = request
        .preparation
        .as_ref()
        .map(|token| token.committed.lock().unwrap());
    let reason = if closing {
        "closing"
    } else if request
        .preparation
        .as_ref()
        .is_some_and(|token| token.epoch.load(Ordering::SeqCst) != token.expected)
    {
        "preparation_cancelled"
    } else if request.expected != current_epoch {
        "stale_epoch"
    } else if busy {
        "busy"
    } else if request.client.is_some() {
        "installed"
    } else {
        "disabled"
    };
    let applied = matches!(reason, "installed" | "disabled");
    if applied {
        *client = request.client;
        if let (Some(token), Some(committed)) = (&request.preparation, committed.as_mut()) {
            **committed = Some(token.expected);
        }
    }
    drop(committed);
    let receipt = TtsConfigurationReceipt {
        applied,
        reason,
        epoch: current_epoch,
        engine_kind: client.as_ref().map(|c| c.engine_kind()),
    };
    let _ = request.response.send(Ok(receipt.clone()));
    Some(receipt)
}
fn start_sapi(
    mut request: SpeechRequest,
    sapi: Option<&Sapi>,
    info: &Arc<Mutex<Value>>,
    generation: &AtomicU64,
    closing: &AtomicBool,
) -> Option<Active> {
    let result = sapi
        .ok_or_else(|| anyhow!("Windows SAPI声音不可用"))
        .and_then(|sapi| sapi.start(&request, generation, closing));
    match result {
        Ok(true) => {
            start_receipt(&mut request, "sapi", "sapi_speak_accepted");
            patch(info, "tts_provider", json!("Windows SAPI / Rust COM"));
            patch(info, "state", json!("speaking"));
            Some(Active {
                request,
                engine: Engine::Sapi,
                began: Instant::now(),
            })
        }
        Ok(false) => {
            finish_request(
                request,
                SpeechEnd::Interrupted,
                "none",
                PcmStats::default(),
                Some("stale_before_sapi_start".into()),
            );
            patch(info, "state", json!("idle"));
            None
        }
        Err(error) => {
            patch(info, "error", json!(error.to_string()));
            patch(info, "state", json!("idle"));
            let _ = request.response.send(Err(error.to_string()));
            None
        }
    }
}

pub(super) fn run(
    root: PathBuf,
    runtime: Option<tokio::runtime::Handle>,
    receiver: mpsc::Receiver<SpeechCommand>,
    info: Arc<Mutex<Value>>,
    generation: Arc<AtomicU64>,
    closing: Arc<AtomicBool>,
    defer_initial_tts: bool,
    reference: Option<Arc<render_reference::RenderReference>>,
) -> Result<()> {
    // Missing SAPI/voices must not prevent an explicitly configured PCM backend.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
        .ok()
        .map(|_| ComGuard);
    let sapi = if com.is_some() {
        Sapi::new(&info).ok()
    } else {
        None
    };
    let descriptor = root.join("runtime/tts-service.json");
    let mut client = if !defer_initial_tts && descriptor.exists() {
        match TtsClient::from_descriptor(&descriptor) {
            Ok(client) => Some(Arc::new(client)),
            Err(error) => {
                patch(&info, "tts_fallback_reason", json!(error.to_string()));
                None
            }
        }
    } else {
        None
    };
    patch(
        &info,
        "tts_configured_engine",
        json!(client.as_ref().map(|c| c.engine_kind())),
    );
    let mut active: Option<Active> = None;
    loop {
        if closing.load(Ordering::SeqCst) {
            if let Some(old) = active.take() {
                finish_active(old, false, Some("closed".into()), sapi.as_ref(), &info)?;
            }
            break;
        }
        if active.as_ref().is_some_and(|a| {
            a.request.expected != generation.load(Ordering::SeqCst)
                || a.began.elapsed() > Duration::from_secs(90)
        }) {
            finish_active(
                active.take().unwrap(),
                false,
                Some("cancelled_or_timed_out".into()),
                sapi.as_ref(),
                &info,
            )?;
        }
        let mut failed = None;
        let mut completed = false;
        if let Some(current) = active.as_mut() {
            match &mut current.engine {
                Engine::Sapi => match sapi.as_ref().unwrap().done() {
                    Ok(done) => completed = done,
                    Err(error) => failed = Some(error.to_string()),
                },
                Engine::Pcm(pcm) => {
                    if let Err(error) = pcm.pump() {
                        failed = Some(error.to_string())
                    }
                    let stats = pcm.output.stats();
                    if stats.callback_submitted_frames > 0 {
                        start_receipt(
                            &mut current.request,
                            pcm.engine_kind,
                            "first_callback_submission",
                        );
                        patch(&info, "state", json!("speaking"));
                    }
                    if let Some(metrics) = &pcm.provider_metrics {
                        patch(&info, "tts_transport", metrics.clone())
                    }
                    completed = failed.is_none() && pcm.output.drained();
                }
            }
        }
        if let Some(error) = failed {
            let old = active.take().unwrap();
            match old.engine {
                Engine::Pcm(mut pcm) => {
                    // Stop/join first. Checking zero before quiescing would race
                    // the callback and could replay already submitted audio.
                    let stats = pcm.output.stop();
                    let engine_kind = pcm.engine_kind;
                    drop(pcm);
                    patch(&info, "pcm_playback", json!(stats));
                    if fallback_allowed(
                        stats.callback_submitted_frames,
                        old.request.expected,
                        generation.load(Ordering::SeqCst),
                        closing.load(Ordering::SeqCst),
                    ) {
                        patch(&info, "tts_fallback_reason", json!(error));
                        if let Some(reference) = &reference {
                            reference.mark_unavailable();
                        }
                        active =
                            start_sapi(old.request, sapi.as_ref(), &info, &generation, &closing);
                    } else {
                        let mut request = old.request;
                        if stats.callback_submitted_frames > 0 {
                            start_receipt(&mut request, engine_kind, "first_callback_submission")
                        }
                        patch(&info, "state", json!("idle"));
                        patch(&info, "error", json!(error));
                        finish_request(
                            request,
                            SpeechEnd::Interrupted,
                            engine_kind,
                            stats,
                            Some("pcm_failed_after_submission_or_cancelled".into()),
                        );
                    }
                }
                Engine::Sapi => finish_active(
                    Active {
                        request: old.request,
                        engine: Engine::Sapi,
                        began: old.began,
                    },
                    false,
                    Some(error),
                    sapi.as_ref(),
                    &info,
                )?,
            }
        } else if completed {
            let done = active.take().unwrap();
            let valid = done.request.expected == generation.load(Ordering::SeqCst)
                && !closing.load(Ordering::SeqCst);
            finish_active(
                done,
                valid,
                (!valid).then(|| "cancelled_before_completion".into()),
                sapi.as_ref(),
                &info,
            )?;
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(SpeechCommand::Speak(request)) => {
                if request.expected != generation.load(Ordering::SeqCst)
                    || closing.load(Ordering::SeqCst)
                {
                    finish_request(
                        request,
                        SpeechEnd::Interrupted,
                        "none",
                        PcmStats::default(),
                        Some("stale_request".into()),
                    );
                    continue;
                }
                if let Some(old) = active.take() {
                    finish_active(old, false, Some("replaced".into()), sapi.as_ref(), &info)?;
                }
                {
                    let mut status = info.lock().unwrap();
                    status["playback_id"] = json!(request.playback_id);
                    status["playback_epoch"] = json!(request.expected);
                    status["tts_transport"] = Value::Null;
                    status["pcm_playback"] = Value::Null;
                    status["error"] = Value::Null;
                }
                if request.rate == 0 && request.text.chars().count() <= 320 {
                    if let (Some(client), Some(runtime)) = (&client, &runtime) {
                        match PcmOutput::open(
                            generation.clone(),
                            request.expected,
                            closing.clone(),
                            reference.clone(),
                        ) {
                            Ok(output) => {
                                let (send, packets) = tokio::sync::mpsc::channel(6);
                                let (done, completion) = oneshot::channel();
                                let client = client.clone();
                                let engine_kind = client.engine_kind();
                                let provider_name = client.provider_name();
                                let epoch = generation.clone();
                                let expected = request.expected;
                                let text = request.text.clone();
                                let language = request.language.clone();
                                let task = runtime.spawn(async move {
                                    let result = client
                                        .synthesize(
                                            &text, &language, "normal", epoch, expected, send,
                                        )
                                        .await
                                        .map_err(|e| e.to_string());
                                    let _ = done.send(result);
                                });
                                patch(
                                    &info,
                                    "tts_provider",
                                    json!(format!("{provider_name} / Rust CPAL")),
                                );
                                patch(&info, "tts_fallback_reason", Value::Null);
                                patch(&info, "state", json!("synthesizing"));
                                active = Some(Active {
                                    request,
                                    engine: Engine::Pcm(PcmActive {
                                        engine_kind,
                                        output,
                                        packets,
                                        completion,
                                        _task: AbortTask(task),
                                        pending: None,
                                        provider_metrics: None,
                                        input_closed: false,
                                    }),
                                    began: Instant::now(),
                                });
                                continue;
                            }
                            Err(error) => {
                                patch(&info, "tts_fallback_reason", json!(error.to_string()))
                            }
                        }
                    }
                } else if client.is_some() {
                    patch(
                        &info,
                        "tts_fallback_reason",
                        json!(if request.rate != 0 {
                            "speed_requires_sapi"
                        } else {
                            "segment_exceeds_local_pcm_bound"
                        }),
                    );
                }
                // Stop may race device initialization. Recheck before starting fallback.
                if request.expected == generation.load(Ordering::SeqCst)
                    && !closing.load(Ordering::SeqCst)
                {
                    if let Some(reference) = &reference {
                        reference.mark_unavailable();
                    }
                    active = start_sapi(request, sapi.as_ref(), &info, &generation, &closing)
                } else {
                    finish_request(
                        request,
                        SpeechEnd::Interrupted,
                        "none",
                        PcmStats::default(),
                        Some("stale_before_start".into()),
                    )
                }
            }
            Ok(SpeechCommand::Stop(stop_epoch, response)) => {
                if active.as_ref().is_some_and(|a| {
                    stop_targets_active(
                        stop_epoch,
                        a.request.expected,
                        generation.load(Ordering::SeqCst),
                    )
                }) {
                    finish_active(
                        active.take().unwrap(),
                        false,
                        Some("stopped".into()),
                        sapi.as_ref(),
                        &info,
                    )?;
                }
                let _ = response.send(Ok(()));
            }
            Ok(SpeechCommand::ConfigureTts(request)) => {
                if let Some(receipt) = configure_tts_slot(
                    &mut client,
                    request,
                    generation.load(Ordering::SeqCst),
                    active.is_some(),
                    closing.load(Ordering::SeqCst),
                ) {
                    if receipt.applied {
                        patch(&info, "tts_configured_engine", json!(receipt.engine_kind));
                    }
                }
            }
            Ok(SpeechCommand::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(old) = active.take() {
                    finish_active(old, false, Some("closed".into()), sapi.as_ref(), &info)?;
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    drop(sapi);
    drop(com);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_stop_never_purges_a_new_request_in_the_new_epoch() {
        assert!(stop_targets_active(2, 1, 2));
        assert!(!stop_targets_active(2, 2, 2));
        assert!(!stop_targets_active(2, 3, 3));
        assert!(!stop_targets_active(2, 1, 3));
    }
    #[test]
    fn new_speak_queued_before_an_old_stop_survives_the_command_reordering() {
        let (send, receive) = mpsc::sync_channel(2);
        let (response, _) = oneshot::channel();
        send.send(SpeechCommand::Speak(SpeechRequest {
            playback_id: "new".into(),
            text: "synthetic".into(),
            language: "en".into(),
            rate: 0,
            expected: 2,
            started: None,
            response,
        }))
        .unwrap();
        let (response, _) = oneshot::channel();
        send.send(SpeechCommand::Stop(2, response)).unwrap();
        let mut active_epoch = None;
        while let Ok(command) = receive.try_recv() {
            match command {
                SpeechCommand::Speak(request) => active_epoch = Some(request.expected),
                SpeechCommand::Stop(stop, _) => {
                    if active_epoch.is_some_and(|active| stop_targets_active(stop, active, 2)) {
                        active_epoch = None
                    }
                }
                SpeechCommand::Quit | SpeechCommand::ConfigureTts(_) => unreachable!(),
            }
        }
        assert_eq!(active_epoch, Some(2));
    }
    #[test]
    fn partial_or_silent_pcm_submission_and_cancellation_forbid_whole_text_fallback() {
        assert!(fallback_allowed(0, 2, 2, false));
        assert!(!fallback_allowed(1, 2, 2, false));
        assert!(!fallback_allowed(480, 2, 2, false));
        assert!(!fallback_allowed(0, 2, 3, false));
        assert!(!fallback_allowed(0, 2, 2, true));
    }
    #[tokio::test]
    async fn actual_start_receipt_is_once_and_distinguishes_submission_from_sapi_acceptance() {
        let (started, receive) = oneshot::channel();
        let (response, _) = oneshot::channel();
        let mut request = SpeechRequest {
            playback_id: "synthetic".into(),
            text: "fixture".into(),
            language: "en".into(),
            rate: 0,
            expected: 4,
            started: Some(started),
            response,
        };
        start_receipt(&mut request, "qwen_pcm", "first_callback_submission");
        start_receipt(&mut request, "sapi", "sapi_speak_accepted");
        let receipt = receive.await.unwrap();
        assert_eq!(receipt.epoch, 4);
        assert_eq!(receipt.engine_kind, "qwen_pcm");
        assert_eq!(receipt.milestone, "first_callback_submission");
    }
}

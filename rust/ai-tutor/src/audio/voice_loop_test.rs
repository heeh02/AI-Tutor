//! Test-build-only input/output owner. No CPAL, COM, files, or model inference.
//! It exchanges the same bounded commands as the production AudioService.
pub(crate) use super::streaming_asr::tests::Fixture as AsrFixture;
use super::*;
use voice_endpoint::{EndpointCandidate, EndpointReason};

pub(crate) struct VoiceLoopOwner {
    capture: mpsc::Receiver<CaptureCommand>,
    speech: mpsc::Receiver<SpeechCommand>,
    info: Arc<Mutex<Value>>,
    generation: Arc<AtomicU64>,
    speech_generation: Arc<AtomicU64>,
    devices: Arc<input_devices::InputDevices>,
    binding: InputBinding,
    recording: Option<Recording>,
    pending_start: Option<CaptureCommand>,
    pending_speech: Option<SpeechRequest>,
    input_open: Arc<AtomicBool>,
    pub hold_start: bool,
    pub start_requests: usize,
    pub opened: usize,
    pub stops: usize,
    pub speeches: usize,
    pub completed_speeches: usize,
}

impl AudioService {
    pub(crate) fn voice_loop_test_channel(root: PathBuf) -> (Self, VoiceLoopOwner, InputBinding) {
        let (mut audio, speech) = Self::configuration_test_channel(0);
        let (send, receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        audio.root = root;
        audio.capture = send;
        *audio.info.lock().unwrap() = json!({"state":"idle","recording":false,
            "capture_pending_result":false,"recording_endpoint":null,"asr_partial":null,
            "raw_audio_saved":false,"asr_streaming_state":"disabled"});
        let binding = audio.input_devices.install_fake_binding();
        let owner = VoiceLoopOwner {
            capture: receive,
            speech: speech.0,
            info: audio.info.clone(),
            generation: audio.record_generation.clone(),
            speech_generation: audio.speech_generation.clone(),
            devices: audio.input_devices.clone(),
            binding: binding.clone(),
            recording: None,
            pending_start: None,
            pending_speech: None,
            input_open: Arc::new(AtomicBool::new(false)),
            hold_start: false,
            start_requests: 0,
            opened: 0,
            stops: 0,
            speeches: 0,
            completed_speeches: 0,
        };
        (audio, owner, binding)
    }
}

impl VoiceLoopOwner {
    pub(crate) fn pump(&mut self) {
        // The input owner rejects stale tokens before any stream is created.
        discard_stale_input(&mut self.recording, &self.info, &self.generation);
        let speech: Vec<_> = self.speech.try_iter().collect();
        for command in speech {
            match command {
                SpeechCommand::Stop(epoch, reply) => {
                    if self
                        .pending_speech
                        .as_ref()
                        .is_some_and(|request| request.expected < epoch)
                    {
                        self.finish_speech(SpeechEnd::Interrupted);
                    }
                    let _ = reply.send(Ok(()));
                }
                SpeechCommand::Speak(mut request) => {
                    assert!(
                        !self.input_open.load(Ordering::SeqCst),
                        "capture/output overlap"
                    );
                    assert!(self.recording.as_ref().is_none_or(|r| r.stream.is_none()));
                    assert!(self.pending_speech.is_none(), "two output owners overlap");
                    assert_eq!(
                        request.expected,
                        self.speech_generation.load(Ordering::SeqCst)
                    );
                    assert!(!request.text.is_empty());
                    assert_eq!(request.language, "en");
                    assert_eq!(request.rate, 0);
                    self.speeches += 1;
                    patch(&self.info, "state", json!("speaking"));
                    request
                        .started
                        .take()
                        .expect("segments request start receipt")
                        .send(SpeechStarted {
                            playback_id: request.playback_id.clone(),
                            epoch: request.expected,
                            engine_kind: "qwen_pcm",
                            milestone: "first_callback_submission",
                        })
                        .expect("segment consumer remains present");
                    self.pending_speech = Some(request);
                }
                SpeechCommand::Quit => {
                    if self.pending_speech.is_some() {
                        self.finish_speech(SpeechEnd::Interrupted);
                    }
                }
                SpeechCommand::ConfigureTts(_) => {
                    panic!("integration fixture must not configure TTS")
                }
            }
        }
        let capture: Vec<_> = self.capture.try_iter().collect();
        for command in capture {
            match command {
                command @ CaptureCommand::Start(..) => {
                    self.start_requests += 1;
                    if self.hold_start {
                        assert!(self.pending_start.replace(command).is_none());
                    } else {
                        self.open(command);
                    }
                }
                CaptureCommand::Stop(token, reply) => {
                    assert_eq!(token, self.generation.load(Ordering::SeqCst));
                    self.stops += 1;
                    let _ = reply.send(finish_input(&mut self.recording, &self.info));
                }
                CaptureCommand::Discard(token) => {
                    if token == self.generation.load(Ordering::SeqCst) {
                        discard_input(&mut self.recording, &self.info);
                        patch(&self.info, "state", json!("idle"));
                    }
                }
                CaptureCommand::Quit => discard_input(&mut self.recording, &self.info),
                CaptureCommand::Monitor(..)
                | CaptureCommand::RenewMonitor(..)
                | CaptureCommand::PromoteMonitor(..)
                | CaptureCommand::StopMonitor(..) => {
                    panic!("half-duplex fixture does not enable barge-in")
                }
                CaptureCommand::Devices(_) => panic!("no native device enumeration in fixture"),
            }
        }
    }
    fn open(&mut self, command: CaptureCommand) {
        let CaptureCommand::Start(selection, mode, token, streaming, response) = command else {
            unreachable!()
        };
        if token != self.generation.load(Ordering::SeqCst) {
            let _ = response.send(Err("capture cancelled before fake input open".into()));
            return;
        }
        assert_eq!(selection.binding(), Some(&self.binding));
        assert_eq!(mode, EndpointMode::VoiceEndpointV1);
        assert!(
            streaming.is_some(),
            "real HTTP ASR transport must be installed"
        );
        assert!(self.pending_speech.is_none(), "output/capture overlap");
        assert!(self.recording.is_none(), "two captures overlap");
        assert!(!self.input_open.swap(true, Ordering::SeqCst));
        let open = self.input_open.clone();
        self.recording = Some(Recording {
            stream: Some(InputStream::Fake {
                _close: TestInputClose(Some(Box::new(move || {
                    assert!(open.swap(false, Ordering::SeqCst));
                }))),
            }),
            // Synthetic non-silent PCM only tests transport and ownership. The
            // endpoint below is explicit fake input evidence, not VAD accuracy.
            data: Arc::new(Mutex::new(vec![0.2; 8000])),
            rate: 16000,
            started: Instant::now(),
            generation: token,
            failed: Arc::new(AtomicBool::new(false)),
            streaming,
            input_binding: Some(self.binding.clone()),
            endpoint: Some(capture_endpoint::CaptureEndpoint::new(token)),
            processing: None,
            barge: None,
        });
        self.opened += 1;
        patch(&self.info, "recording", json!(true));
        patch(&self.info, "state", json!("recording"));
        let _ = response.send(Ok(json!({"recording_generation":token,"sample_rate":16000,
            "endpoint_mode":mode,"device_key":self.binding.device_key,
            "input_topology_revision":self.binding.topology_revision})));
    }
    pub(crate) fn has_held_start(&self) -> bool {
        self.pending_start.is_some()
    }
    pub(crate) fn release_start(&mut self) {
        let command = self.pending_start.take().expect("fake start is pending");
        self.open(command);
    }
    pub(crate) fn speech_end(&mut self) {
        let recording = self.recording.as_mut().expect("capture was opened");
        let candidate = EndpointCandidate {
            recording_generation: recording.generation,
            reason: EndpointReason::SpeechEnd,
            elapsed_audio_ms: 500,
            speech_confirmed_at_ms: Some(100),
            last_activity_at_ms: Some(350),
        };
        close_endpoint_input(
            recording,
            candidate,
            None,
            &self.info,
            &self.generation,
            &self.devices,
        );
        assert!(!self.input_open.load(Ordering::SeqCst));
        assert_eq!(
            self.info.lock().unwrap()["recording_endpoint"]["input_closed"],
            true
        );
    }
    pub(crate) fn is_speaking(&self) -> bool {
        self.pending_speech.is_some()
    }
    pub(crate) fn input_is_open(&self) -> bool {
        self.input_open.load(Ordering::SeqCst)
    }
    pub(crate) fn complete_speech(&mut self) {
        self.finish_speech(SpeechEnd::Completed);
    }
    fn finish_speech(&mut self, state: SpeechEnd) {
        let request = self.pending_speech.take().expect("output owner is active");
        assert!(
            !self.input_open.load(Ordering::SeqCst),
            "output/capture overlap"
        );
        if state == SpeechEnd::Completed {
            self.completed_speeches += 1;
        }
        patch(&self.info, "state", json!("idle"));
        let _ = request.response.send(Ok(SpeechOutcome {
            playback_id: request.playback_id,
            epoch: request.expected,
            state,
            engine_kind: "qwen_pcm",
            callback_submitted_frames: 480,
            predicted_output_drained: state == SpeechEnd::Completed,
            speaker_output_measured: false,
            reason: None,
        }));
    }
}

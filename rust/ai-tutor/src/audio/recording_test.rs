//! Deterministic fake owner: no microphone, output device, or recognition model.
use super::*;

pub(crate) struct TestRecordingOwner {
    capture: mpsc::Receiver<CaptureCommand>,
    speech: mpsc::Receiver<SpeechCommand>,
    info: Arc<Mutex<Value>>,
    pub starts: usize,
    pub stops: usize,
    pub discards: usize,
    pub hold_stop: bool,
    pub samples: Vec<f32>,
    pending_stop: Option<Response<Captured>>,
}
impl TestRecordingOwner {
    pub(crate) fn pump(&mut self) {
        for command in self.speech.try_iter() {
            match command {
                SpeechCommand::Stop(_, reply) => {
                    let _ = reply.send(Ok(()));
                }
                SpeechCommand::Quit => {}
                _ => panic!("recording fixture must never synthesize or configure speech"),
            }
        }
        for command in self.capture.try_iter() {
            match command {
                CaptureCommand::Start(device, mode, token, streaming, reply) => {
                    assert!(streaming.is_none(), "fixture must not call an ASR service");
                    let InputSelection::Legacy(device) = device else {
                        panic!("fake owner has no native devices")
                    };
                    assert_eq!(mode, EndpointMode::Manual);
                    self.starts += 1;
                    patch(&self.info, "recording", json!(true));
                    let _ = reply.send(Ok(json!({"device_id":device.unwrap_or(0),
                        "sample_rate":16000,"recording_generation":token,"endpoint_mode":mode})));
                }
                CaptureCommand::Stop(_, reply) => {
                    self.stops += 1;
                    patch(&self.info, "recording", json!(false));
                    if self.hold_stop {
                        assert!(self.pending_stop.replace(reply).is_none());
                    } else {
                        let _ = reply.send(Ok(Captured {
                            samples: self.samples.clone(),
                            rate: 16000,
                            streaming: None,
                            input_binding: None,
                            endpoint_receipt: None,
                        }));
                    }
                }
                CaptureCommand::Discard(_) => {
                    self.discards += 1;
                    patch(&self.info, "recording", json!(false));
                }
                CaptureCommand::Monitor(..)
                | CaptureCommand::RenewMonitor(..)
                | CaptureCommand::PromoteMonitor(..)
                | CaptureCommand::StopMonitor(..) => {
                    panic!("half-duplex fixture does not enable barge-in")
                }
                CaptureCommand::Devices(_) => panic!("fixture must not enumerate devices"),
                CaptureCommand::Quit => {}
            }
        }
    }
    pub(crate) fn release_stop(&mut self) {
        self.pending_stop
            .take()
            .expect("a stopped capture is waiting")
            .send(Ok(Captured {
                samples: self.samples.clone(),
                rate: 16000,
                streaming: None,
                input_binding: None,
                endpoint_receipt: None,
            }))
            .ok();
    }
}
impl AudioService {
    pub(crate) fn recording_test_channel(root: PathBuf) -> (Self, TestRecordingOwner) {
        let (mut audio, speech) = Self::configuration_test_channel(0);
        let (send, receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        audio.root = root;
        audio.capture = send;
        patch(&audio.info, "recording", json!(false));
        let owner = TestRecordingOwner {
            capture: receive,
            speech: speech.0,
            info: audio.info.clone(),
            starts: 0,
            stops: 0,
            discards: 0,
            hold_stop: false,
            samples: vec![],
            pending_stop: None,
        };
        (audio, owner)
    }
}

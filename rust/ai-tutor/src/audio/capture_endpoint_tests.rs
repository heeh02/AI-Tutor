//! Fake input Drop and in-memory PCM only; never enumerate/open audio or ASR.
use super::*;
use voice_endpoint::{EndpointCandidate, EndpointReason};

fn fixture() -> (
    Recording,
    Arc<Mutex<Value>>,
    Arc<AtomicU64>,
    Arc<input_devices::InputDevices>,
    Arc<AtomicBool>,
) {
    let info = Arc::new(Mutex::new(
        json!({"recording":true,"state":"recording","recording_endpoint":null}),
    ));
    let generation = Arc::new(AtomicU64::new(17));
    let devices = Arc::new(input_devices::InputDevices::new());
    let closed = Arc::new(AtomicBool::new(false));
    let observed = info.clone();
    let closed_flag = closed.clone();
    let recording = Recording {
        stream: Some(InputStream::Fake {
            _close: TestInputClose(Some(Box::new(move || {
                assert!(
                    observed.lock().unwrap()["recording_endpoint"].is_null(),
                    "published before real input Drop"
                );
                closed_flag.store(true, Ordering::SeqCst);
            }))),
        }),
        data: Arc::new(Mutex::new(vec![0.0; 160_000])),
        rate: 16_000,
        started: Instant::now(),
        generation: 17,
        failed: Arc::new(AtomicBool::new(false)),
        streaming: None,
        input_binding: Some(devices.test_binding()),
        endpoint: Some(capture_endpoint::CaptureEndpoint::new(17)),
        processing: None,
        barge: None,
    };
    (recording, info, generation, devices, closed)
}
fn candidate() -> EndpointCandidate {
    EndpointCandidate {
        recording_generation: 17,
        reason: EndpointReason::SpeechEnd,
        elapsed_audio_ms: 3_000,
        speech_confirmed_at_ms: Some(120),
        last_activity_at_ms: Some(500),
    }
}

#[test]
fn owner_closes_before_terminal_and_keeps_full_pcm_until_stop() {
    let (recording, info, generation, devices, closed) = fixture();
    let pcm = recording.data.clone();
    let mut recording = Some(recording);
    for _ in 0..250 {
        pump_capture_endpoint(&mut recording, &info, &generation, &devices);
    }
    assert!(closed.load(Ordering::SeqCst));
    let endpoint = info.lock().unwrap()["recording_endpoint"].clone();
    assert_eq!(endpoint["reason"], "no_speech");
    assert_eq!(endpoint["input_closed"], true);
    assert_eq!(endpoint["recording_generation"], 17);
    assert_eq!(endpoint["elapsed_audio_ms"], 10_000);
    assert_eq!(endpoint["captured_samples"], 160_000);
    assert_eq!(info.lock().unwrap()["capture_pending_result"], true);
    assert_eq!(info.lock().unwrap()["recording"], false);
    assert_eq!(pcm.lock().unwrap().len(), 160_000);
    pump_capture_endpoint(&mut recording, &info, &generation, &devices);
    assert_eq!(info.lock().unwrap()["recording_endpoint"], endpoint);
    let captured = finish_input(&mut recording, &info).unwrap();
    assert_eq!(captured.samples.len(), 160_000);
    assert_eq!(
        captured.endpoint_receipt.unwrap().candidate.reason,
        EndpointReason::NoSpeech
    );
    assert_eq!(info.lock().unwrap()["capture_pending_result"], false);
    assert_eq!(info.lock().unwrap()["recording_endpoint"], endpoint);
}

#[test]
fn cancellation_during_input_drop_never_publishes_a_terminal() {
    let (mut current, info, generation, devices, _) = fixture();
    let observed = generation.clone();
    current.stream = Some(InputStream::Fake {
        _close: TestInputClose(Some(Box::new(move || {
            observed.store(18, Ordering::SeqCst);
        }))),
    });
    current.started = Instant::now() - Duration::from_secs(21);
    let pcm = current.data.clone();
    let mut recording = Some(current);
    pump_capture_endpoint(&mut recording, &info, &generation, &devices);
    assert!(recording.is_none());
    assert!(info.lock().unwrap()["recording_endpoint"].is_null());
    assert!(pcm.lock().unwrap().iter().all(|sample| *sample == 0.0));
    assert_eq!(generation.load(Ordering::SeqCst), 18);
}

#[test]
fn topology_change_during_close_cannot_publish_speech_end() {
    let (mut recording, info, generation, devices, _) = fixture();
    let changed = devices.clone();
    recording.stream = Some(InputStream::Fake {
        _close: TestInputClose(Some(Box::new(move || {
            changed.topology.changed();
        }))),
    });
    close_endpoint_input(
        &mut recording,
        candidate(),
        None,
        &info,
        &generation,
        &devices,
    );
    let end = info.lock().unwrap()["recording_endpoint"].clone();
    assert_eq!(end["reason"], "device_error");
    assert_eq!(end["cause"], "input_topology_changed");
    assert_eq!(end["input_closed"], true);
    assert_eq!(recording.data.lock().unwrap().len(), 160_000);
}

#[test]
fn later_topology_change_revokes_terminal_instead_of_rewriting_it() {
    let (mut current, info, generation, devices, _) = fixture();
    close_endpoint_input(
        &mut current,
        candidate(),
        None,
        &info,
        &generation,
        &devices,
    );
    assert_eq!(
        info.lock().unwrap()["recording_endpoint"]["reason"],
        "speech_end"
    );
    devices.topology.changed();
    let mut recording = Some(current);
    pump_capture_endpoint(&mut recording, &info, &generation, &devices);
    assert!(recording.is_none());
    assert_eq!(generation.load(Ordering::SeqCst), 18);
    assert!(info.lock().unwrap()["recording_endpoint"].is_null());
}

#[test]
fn wall_and_sample_caps_win_before_backlogged_vad_and_keep_tail_pcm() {
    for sample_cap in [false, true] {
        let (mut current, info, generation, devices, _) = fixture();
        if sample_cap {
            *current.data.lock().unwrap() = vec![0.125; 320_000];
        } else {
            current.started = Instant::now() - Duration::from_secs(21);
        }
        let before = current.data.lock().unwrap().clone();
        let mut recording = Some(current);
        pump_capture_endpoint(&mut recording, &info, &generation, &devices);
        let end = info.lock().unwrap()["recording_endpoint"].clone();
        assert_eq!(end["reason"], "duration_limit");
        assert_eq!(end["elapsed_audio_ms"], 0);
        assert_eq!(
            recording.as_ref().unwrap().data.lock().unwrap().as_slice(),
            before
        );
        assert_eq!(finish_input(&mut recording, &info).unwrap().samples, before);
    }
}

#[test]
fn explicit_device_error_is_closed_once_and_never_returned_as_transcript_audio() {
    let (current, info, generation, devices, _) = fixture();
    *current.data.lock().unwrap() = vec![0.125; 16_000];
    current.failed.store(true, Ordering::SeqCst);
    let pcm = current.data.clone();
    let mut recording = Some(current);
    pump_capture_endpoint(&mut recording, &info, &generation, &devices);
    let end = info.lock().unwrap()["recording_endpoint"].clone();
    assert_eq!(end["reason"], "device_error");
    assert_eq!(end["cause"], "stream_error");
    assert_eq!(pcm.lock().unwrap()[0], 0.125);
    pump_capture_endpoint(&mut recording, &info, &generation, &devices);
    assert_eq!(info.lock().unwrap()["recording_endpoint"], end);
    assert!(finish_input(&mut recording, &info).is_err());
    assert!(pcm.lock().unwrap().iter().all(|sample| *sample == 0.0));
}

#[test]
fn manual_cap_keeps_legacy_pending_recording_without_a_vad_terminal() {
    let (mut current, info, generation, devices, closed) = fixture();
    current.endpoint = None;
    current.started = Instant::now() - Duration::from_secs(21);
    let mut recording = Some(current);
    pump_capture_endpoint(&mut recording, &info, &generation, &devices);
    let current = recording.as_mut().unwrap();
    manual_capture_limit(current);
    assert!(closed.load(Ordering::SeqCst));
    assert_eq!(info.lock().unwrap()["recording"], true);
    assert!(info.lock().unwrap()["recording_endpoint"].is_null());
    assert_eq!(current.data.lock().unwrap().len(), 160_000);
    assert!(
        finish_input(&mut recording, &info)
            .unwrap()
            .endpoint_receipt
            .is_none()
    );
}

#[tokio::test]
async fn explicit_mode_requires_stable_binding_and_pending_result_blocks_start_before_reservation()
{
    let (audio, _queue) = AudioService::configuration_test_channel(0);
    assert!(
        audio
            .start_recording_with_endpoint(
                InputSelection::Legacy(None),
                EndpointMode::VoiceEndpointV1
            )
            .await
            .is_err()
    );
    assert_eq!(audio.record_generation.load(Ordering::SeqCst), 0);
    patch(
        &audio.info,
        "recording_endpoint",
        json!({"recording_generation":0}),
    );
    patch(&audio.info, "capture_pending_result", json!(true));
    assert!(
        audio
            .start_recording_with_endpoint_reserved(
                InputSelection::Legacy(None),
                EndpointMode::Manual,
                |_| panic!("pending capture must not reserve")
            )
            .await
            .is_err()
    );
    assert_eq!(audio.record_generation.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejected_reservation_never_queues_capture_or_overwrites_a_newer_generation() {
    for replace_generation in [false, true] {
        let (mut audio, speech) = AudioService::configuration_test_channel(0);
        let (capture_send, capture_commands) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
        audio.capture = capture_send;
        let before = audio.record_generation.clone();
        let owner = std::thread::spawn(move || {
            let SpeechCommand::Stop(_, response) =
                speech.0.recv_timeout(Duration::from_secs(2)).unwrap()
            else {
                panic!("only stop before reservation")
            };
            response.send(Ok(())).unwrap();
        });
        assert!(
            audio
                .start_recording_with_endpoint_reserved(
                    InputSelection::Legacy(None),
                    EndpointMode::Manual,
                    move |token| {
                        assert_eq!(token, 1);
                        if replace_generation {
                            before.store(9, Ordering::SeqCst);
                        }
                        false
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(
            audio.record_generation.load(Ordering::SeqCst),
            if replace_generation { 9 } else { 2 }
        );
        assert!(matches!(
            capture_commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!audio.status()["capture_pending_result"].as_bool().unwrap());
        owner.join().unwrap();
    }
}

#[tokio::test]
async fn late_start_timeout_cannot_invalidate_or_clear_a_newer_recording() {
    for current in [1, 9] {
        let (audio, _speech) = AudioService::configuration_test_channel(0);
        audio.record_generation.store(current, Ordering::SeqCst);
        patch(
            &audio.info,
            "recording_endpoint",
            json!({"recording_generation":current,"reason":"speech_end"}),
        );
        patch(&audio.info, "capture_pending_result", json!(true));
        let (held_reply, receive) = oneshot::channel();
        let error = audio
            .await_capture_start(1, receive, Duration::from_millis(5))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("启动超时"));
        assert_eq!(
            audio.record_generation.load(Ordering::SeqCst),
            if current == 1 { 2 } else { 9 }
        );
        assert_eq!(audio.status()["capture_pending_result"], current == 9);
        if current == 9 {
            assert_eq!(
                audio.status()["recording_endpoint"]["recording_generation"],
                9
            );
        } else {
            assert!(audio.status()["recording_endpoint"].is_null());
        }
        assert!(
            held_reply
                .send(Ok(json!({"recording_generation":1})))
                .is_err()
        );
    }
}

#[tokio::test]
async fn public_stop_response_preserves_owner_closed_speech_end_receipt() {
    let (mut recording, info, generation, devices, _) = fixture();
    close_endpoint_input(
        &mut recording,
        candidate(),
        None,
        &info,
        &generation,
        &devices,
    );
    let expected = info.lock().unwrap()["recording_endpoint"].clone();
    let (mut audio, _speech) = AudioService::configuration_test_channel(0);
    audio.info = info.clone();
    audio.record_generation = generation;
    audio.input_devices = devices;
    let (send, receive) = mpsc::sync_channel(AUDIO_QUEUE_CAPACITY);
    audio.capture = send;
    // Prepare the owner's result on this thread. The fake IPC worker sends only
    // PCM/receipt, never a native (non-Send) CPAL stream.
    let captured = finish_input(&mut Some(recording), &info).unwrap();
    let owner = std::thread::spawn(move || {
        let CaptureCommand::Stop(17, reply) = receive.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("only the exact Stop is allowed")
        };
        reply.send(Ok(captured)).ok();
    });
    // Quiet PCM deliberately avoids all model calls. Nonempty streaming and
    // Whisper success branches carry the same captured receipt in production.
    let response = audio.stop_recording_expected("zh", 17).await.unwrap();
    assert_eq!(response["recording_endpoint"], expected);
    assert_eq!(response["recording_endpoint"]["reason"], "speech_end");
    assert_eq!(response["recording_endpoint"]["input_closed"], true);
    assert_eq!(response["recording_endpoint"]["recording_generation"], 17);
    assert_eq!(response["text"], "");
    assert_eq!(audio.status()["capture_pending_result"], false);
    owner.join().unwrap();
}

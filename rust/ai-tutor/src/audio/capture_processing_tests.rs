//! Owner-path checks with in-memory PCM and a local fake ASR, never devices.
use super::*;

fn processed_recording(samples: Vec<f32>, rate: u32) -> Recording {
    let reference = render_reference::RenderReference::new(true);
    reference.mark_unavailable(); // SAPI/unknown reference must preserve the input.
    let timing = capture_processing::CaptureTiming::new();
    timing.record(
        0,
        samples.len(),
        rate,
        Some(Instant::now() - Duration::from_secs(20)),
    );
    Recording {
        stream: None,
        data: Arc::new(Mutex::new(samples)),
        rate,
        started: Instant::now(),
        generation: 1,
        failed: Arc::new(AtomicBool::new(false)),
        streaming: None,
        input_binding: None,
        endpoint: Some(capture_endpoint::CaptureEndpoint::new(1)),
        barge: None,
        processing: Some(capture_processing::CaptureProcessing::new(
            reference, timing, rate,
        )),
    }
}

#[tokio::test]
async fn closed_input_tail_reaches_partial_and_final_asr_once_and_raw_is_wiped() {
    let fixture = streaming_asr::tests::Fixture::new().await;
    let info = Arc::new(Mutex::new(json!({"recording":true})));
    let mut recording = processed_recording(vec![0.25; 1440], 48_000);
    recording.endpoint = None;
    let raw = recording.data.clone();
    let timing = capture_processing::CaptureTiming::new();
    timing.record(
        0,
        1440,
        48_000,
        Some(Instant::now() - Duration::from_millis(30)),
    );
    recording.processing = Some(capture_processing::CaptureProcessing::new(
        render_reference::RenderReference::new(true),
        timing,
        48_000,
    ));
    let close_data = raw.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let on_close = closed.clone();
    recording.stream = Some(InputStream::Fake {
        _close: TestInputClose(Some(Box::new(move || {
            // The last native callback completes during stream Drop. Its 73 output
            // samples must survive even though they do not fill a 10 ms AEC frame.
            close_data.lock().unwrap().extend_from_slice(&[0.5; 219]);
            on_close.store(true, Ordering::SeqCst);
        }))),
    });
    recording.streaming = streaming_asr::begin(
        None,
        Some(fixture.managed_client()),
        info.clone(),
        Arc::new(AtomicU64::new(1)),
        1,
    );
    recording
        .processing
        .as_mut()
        .unwrap()
        .pump_shared(&raw, false);
    let processed = recording.processing.as_ref().unwrap().samples().to_vec();
    assert_eq!(processed.len(), 480);
    // Sonora's actual filtered output (including its startup delay) differs
    // from this constant raw input. Both ASR paths must receive that output.
    assert_ne!(processed, vec![0.25; 480]);
    pump_streaming(&mut recording);
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.received_pcm().len() < 640 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let captured = finish_input(&mut Some(recording), &info).unwrap();
    assert!(closed.load(Ordering::SeqCst));
    assert_eq!(captured.rate, 16_000);
    assert_eq!(captured.samples.len(), 553);
    assert_eq!(&captured.samples[..480], processed.as_slice());
    assert_eq!(&captured.samples[480..], &[0.5; 73]);
    assert!(raw.lock().unwrap().iter().all(|sample| *sample == 0.0));
    let expected: Vec<_> = captured
        .samples
        .iter()
        .flat_map(|sample| ((sample * 32767.0).round() as i16).to_le_bytes())
        .collect();
    captured.streaming.unwrap().wait().await.unwrap();
    assert_eq!(fixture.received_pcm(), expected);
    assert_eq!(fixture.request_counts(), (1, 1));
}

#[test]
fn endpoint_waits_for_processed_audio_instead_of_reading_raw_ahead() {
    let mut recording = processed_recording(vec![0.0; 480_000], 48_000);
    let devices = input_devices::InputDevices::new();
    for _ in 0..300 {
        assert!(endpoint_candidate(&mut recording, &devices).is_none());
    }
    let raw = recording.data.clone();
    recording
        .processing
        .as_mut()
        .unwrap()
        .pump_shared(&raw, true);
    let mut terminal = None;
    for _ in 0..300 {
        if let Some(candidate) = endpoint_candidate(&mut recording, &devices) {
            terminal = Some(candidate);
            break;
        }
    }
    assert_eq!(
        terminal.unwrap().0.reason,
        voice_endpoint::EndpointReason::NoSpeech
    );
}

#[test]
fn raw_capture_ceiling_applies_even_when_processed_audio_has_not_advanced() {
    let mut recording = processed_recording(vec![0.0; 960_000], 48_000);
    let devices = input_devices::InputDevices::new();
    assert!(recording.processing.as_ref().unwrap().samples().is_empty());
    let candidate = endpoint_candidate(&mut recording, &devices).unwrap();
    assert_eq!(
        candidate.0.reason,
        voice_endpoint::EndpointReason::DurationLimit
    );
    assert_eq!(recording.data.lock().unwrap().len(), 960_000);
}

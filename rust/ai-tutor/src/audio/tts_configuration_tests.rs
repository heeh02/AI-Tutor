use super::*;

fn client() -> Result<(tempfile::TempDir, Arc<TtsClient>)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("tts.json");
    std::fs::write(
        &path,
        json!({"protocol_version":1,"endpoint":"http://127.0.0.1:9/v1/audio/speech",
        "bearer_token":"synthetic-config-credential","model":"tutor-qwen-tts"})
        .to_string(),
    )?;
    Ok((dir, Arc::new(TtsClient::from_descriptor(&path)?)))
}

#[tokio::test]
async fn configure_public_api_installs_through_a_fake_owner_without_a_speak_command() -> Result<()>
{
    let (_dir, client) = client()?;
    let (speech, receive) = mpsc::sync_channel(4);
    let (capture, _capture) = mpsc::sync_channel(1);
    // No AudioService::new: this fixture cannot initialize COM or audio devices.
    let audio = AudioService {
        root: PathBuf::new(),
        capture,
        speech,
        info: Arc::new(Mutex::new(json!({}))),
        record_generation: Arc::new(AtomicU64::new(0)),
        speech_generation: Arc::new(AtomicU64::new(7)),
        asr_busy: Arc::new(AtomicBool::new(false)),
        closing: Arc::new(AtomicBool::new(false)),
        model: Arc::new(Mutex::new(None)),
        streaming_asr_client: Mutex::new(None),
        final_asr: Mutex::new(FinalAsrSlot::default()),
        legacy_asr_descriptor: false,
        input_devices: Arc::new(input_devices::InputDevices::new()),
        render_reference: render_reference::RenderReference::new(false),
        barge_in_enabled: false,
    };
    let expected = client.clone();
    let owner = std::thread::spawn(move || {
        let mut slot = None;
        let SpeechCommand::ConfigureTts(request) =
            receive.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("configuration must not send Speak");
        };
        let receipt = configure_tts_slot(&mut slot, request, 7, false, false).unwrap();
        assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &expected));
        assert!(receive.try_recv().is_err());
        receipt
    });
    let receipt = audio.configure_tts(Some(client), 7).await?;
    assert!(receipt.applied);
    assert_eq!(receipt.reason, "installed");
    assert_eq!(receipt, owner.join().unwrap());
    Ok(())
}

#[tokio::test]
async fn busy_stale_or_closing_configuration_keeps_the_existing_client() -> Result<()> {
    let (_dir, original) = client()?;
    for (epoch, busy, closing, reason) in [
        (7, true, false, "busy"),
        (8, false, false, "stale_epoch"),
        (7, false, true, "closing"),
    ] {
        let mut slot = Some(original.clone());
        let (response, receive) = oneshot::channel();
        let receipt = configure_tts_slot(
            &mut slot,
            TtsConfigurationRequest {
                client: None,
                expected: 7,
                preparation: None,
                response,
            },
            epoch,
            busy,
            closing,
        )
        .unwrap();
        assert!(!receipt.applied);
        assert_eq!(receipt.reason, reason);
        assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &original));
        assert_eq!(receive.await?.map_err(anyhow::Error::msg)?, receipt);
    }
    let mut slot = Some(original);
    let (response, receive) = oneshot::channel();
    configure_tts_slot(
        &mut slot,
        TtsConfigurationRequest {
            client: None,
            expected: 7,
            preparation: None,
            response,
        },
        7,
        false,
        false,
    );
    assert!(receive.await?.map_err(anyhow::Error::msg)?.applied);
    assert!(slot.is_none());
    Ok(())
}

#[tokio::test]
async fn an_installer_that_abandons_its_receipt_cannot_change_the_client_later() -> Result<()> {
    let (_dir, original) = client()?;
    let mut slot = Some(original.clone());
    let (response, receive) = oneshot::channel();
    drop(receive);
    assert!(
        configure_tts_slot(
            &mut slot,
            TtsConfigurationRequest {
                client: None,
                expected: 7,
                preparation: None,
                response
            },
            7,
            false,
            false
        )
        .is_none()
    );
    assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &original));
    Ok(())
}

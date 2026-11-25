use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    identity: PlaybackIdentity,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("speech.sqlite3"), "simulator")?;
        let learner = store.create_learner("Synthetic receipt fixture", "en", "guided")?;
        let session = store.create_session(s(&learner, "learner_id"))?;
        let sid = s(&session, "session_id");
        let tid = store.add_turn(
            sid,
            "synthetic",
            0,
            "assistant",
            "Already shown text",
            "generated",
            &[],
        )?;
        store.acknowledge(sid, &tid, "shown")?;
        let identity = PlaybackIdentity {
            attempt_id: "attempt-one".into(),
            namespace: "simulator".into(),
            learner_id: s(&learner, "learner_id").to_owned(),
            session_id: sid.to_owned(),
            turn_id: tid,
            generation: 0,
            learner_epoch: 0,
            audio_epoch: 1,
        };
        Ok(Self {
            _dir: dir,
            store,
            identity,
        })
    }
    fn binding(&self) -> PlaybackBinding {
        PlaybackBinding {
            identity: self.identity.clone(),
            started: None,
            task: None,
        }
    }
    fn current(&self) -> Option<(String, String)> {
        Some((
            self.identity.namespace.clone(),
            self.identity.session_id.clone(),
        ))
    }
    fn status(&self) -> Result<String> {
        Ok(s(
            &self.store.session(&self.identity.session_id)?["turns"][0],
            "status",
        )
        .to_owned())
    }
}
fn started(epoch: u64) -> SpeechStarted {
    SpeechStarted {
        playback_id: "audio-one".into(),
        epoch,
        engine_kind: "qwen_pcm",
        milestone: "first_callback_submission",
    }
}
fn outcome(epoch: u64, state: SpeechEnd) -> SpeechOutcome {
    SpeechOutcome {
        playback_id: "audio-one".into(),
        epoch,
        state,
        engine_kind: "qwen_pcm",
        callback_submitted_frames: 480,
        predicted_output_drained: state == SpeechEnd::Completed,
        speaker_output_measured: false,
        reason: None,
    }
}

#[tokio::test]
async fn same_poll_start_and_outcome_are_consumed_in_that_order() -> Result<()> {
    let (send, receive) = oneshot::channel();
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let result = observe_speech(
        async move {
            send.send(started(1)).unwrap();
            Ok(outcome(1, SpeechEnd::Completed))
        },
        receive,
        move |receipt| {
            let events = observed.clone();
            async move {
                events.lock().unwrap().push(receipt.milestone);
            }
        },
    )
    .await?;
    events.lock().unwrap().push("completed");
    assert_eq!(
        *events.lock().unwrap(),
        ["first_callback_submission", "completed"]
    );
    assert!(!result.speaker_output_measured);
    Ok(())
}

#[tokio::test]
async fn queueing_without_a_start_receipt_does_not_change_delivered_state() -> Result<()> {
    let fixture = Fixture::new()?;
    let before = fixture.store.session(&fixture.identity.session_id)?;
    let binding = fixture.binding();
    let (start, receive) = oneshot::channel();
    let (complete, completed) = oneshot::channel();
    let task = tokio::spawn(observe_speech(
        async move { Ok(completed.await?) },
        receive,
        |_| async { panic!("No engine start was sent") },
    ));
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert_eq!(fixture.status()?, "shown");
    drop(start);
    complete.send(outcome(1, SpeechEnd::Interrupted)).unwrap();
    let result = task.await?;
    assert!(terminal_status(&binding, &result).is_none());
    assert_eq!(fixture.store.session(&fixture.identity.session_id)?, before);
    Ok(())
}

#[tokio::test]
async fn playback_invalidated_before_its_task_runs_keeps_the_shown_turn() -> Result<()> {
    let fixture = Fixture::new()?;
    let before = fixture.store.session(&fixture.identity.session_id)?;
    let mut delivery = SpeechDelivery {
        active: Some(fixture.binding()),
        ..Default::default()
    };
    let (start, receive) = oneshot::channel();
    let task = tokio::spawn(observe_speech(
        async move {
            // Cancellation drops the engine receipt without fabricating a start.
            drop(start);
            Ok(outcome(1, SpeechEnd::Interrupted))
        },
        receive,
        |_| async { panic!("cancelled before start") },
    ));
    let cancelled = delivery.take(&fixture.identity).unwrap();
    assert!(cancelled.started.is_none());
    let result = task.await?;
    assert!(!delivery.matches(&fixture.identity));
    assert!(terminal_status(&cancelled, &result).is_none());
    assert_eq!(fixture.status()?, "shown");
    assert_eq!(fixture.store.session(&fixture.identity.session_id)?, before);
    Ok(())
}

#[test]
fn old_start_and_terminal_cannot_take_a_new_attempt_of_the_same_turn() -> Result<()> {
    let fixture = Fixture::new()?;
    let old = fixture.identity.clone();
    let mut replacement = fixture.binding();
    replacement.identity.attempt_id = "attempt-two".into();
    replacement.identity.audio_epoch = 2;
    let mut delivery = SpeechDelivery {
        active: Some(replacement),
        ..Default::default()
    };
    assert!(!delivery.matches(&old));
    assert!(delivery.take(&old).is_none());
    let binding = delivery.active.as_mut().unwrap();
    assert!(!accept_started(binding, started(1)));
    assert!(accept_started(binding, started(2)));
    assert!(
        !accept_started(binding, started(2)),
        "a duplicated start is not a second status update"
    );
    assert!(terminal_status(binding, &Ok(outcome(1, SpeechEnd::Completed))).is_none());
    assert_eq!(
        terminal_status(binding, &Ok(outcome(2, SpeechEnd::Completed))),
        Some("speech_completed")
    );
    assert_eq!(binding.identity.turn_id, old.turn_id);
    Ok(())
}

#[test]
fn receipts_require_owner_session_generation_learner_and_audio_epochs() -> Result<()> {
    let fixture = Fixture::new()?;
    let id = &fixture.identity;
    assert!(identity_is_current(
        &fixture.store,
        &fixture.current(),
        1,
        id
    ));
    for current in [
        None,
        Some(("real".into(), id.session_id.clone())),
        Some((id.namespace.clone(), "other-session".into())),
    ] {
        assert!(!identity_is_current(&fixture.store, &current, 1, id));
    }
    assert!(!identity_is_current(
        &fixture.store,
        &fixture.current(),
        2,
        id
    ));
    let other = fixture
        .store
        .create_learner("Other synthetic owner", "zh", "guided")?;
    let mut wrong_owner = id.clone();
    wrong_owner.learner_id = s(&other, "learner_id").to_owned();
    assert!(!identity_is_current(
        &fixture.store,
        &fixture.current(),
        1,
        &wrong_owner
    ));
    fixture.store.db.execute(
        "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
        [&id.learner_id],
    )?;
    assert!(!identity_is_current(
        &fixture.store,
        &fixture.current(),
        1,
        id
    ));
    let mut current_epoch = id.clone();
    current_epoch.learner_epoch = 1;
    assert!(identity_is_current(
        &fixture.store,
        &fixture.current(),
        1,
        &current_epoch
    ));
    fixture.store.bump(&id.session_id, "idle")?;
    assert!(!identity_is_current(
        &fixture.store,
        &fixture.current(),
        1,
        &current_epoch
    ));
    Ok(())
}

#[test]
fn completion_requires_a_matching_started_playback_and_keeps_engine_milestones() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut binding = fixture.binding();
    assert!(terminal_status(&binding, &Ok(outcome(1, SpeechEnd::Completed))).is_none());
    assert!(accept_started(&mut binding, started(1)));
    let mut wrong = outcome(1, SpeechEnd::Completed);
    wrong.playback_id = "another-audio-request".into();
    assert!(terminal_status(&binding, &Ok(wrong)).is_none());
    assert_eq!(
        terminal_status(&binding, &Err(anyhow!("output failed after submission"))),
        Some("speech_interrupted")
    );
    assert_eq!(
        binding.started.as_ref().unwrap().milestone,
        "first_callback_submission"
    );
    let mut sapi = fixture.binding();
    let receipt = SpeechStarted {
        engine_kind: "sapi",
        milestone: "sapi_speak_accepted",
        ..started(1)
    };
    assert!(accept_started(&mut sapi, receipt));
    let result = SpeechOutcome {
        engine_kind: "sapi",
        callback_submitted_frames: 0,
        predicted_output_drained: false,
        ..outcome(1, SpeechEnd::Completed)
    };
    assert_eq!(
        terminal_status(&sapi, &Ok(result)),
        Some("speech_completed")
    );
    assert_eq!(sapi.started.unwrap().milestone, "sapi_speak_accepted");
    Ok(())
}

#[test]
fn replaying_an_older_turn_does_not_rewind_the_teaching_checkpoint() -> Result<()> {
    let fixture = Fixture::new()?;
    let id = &fixture.identity;
    let newer = fixture.store.add_turn(
        &id.session_id,
        "newer",
        0,
        "assistant",
        "Newest displayed experiment question",
        "generated",
        &[],
    )?;
    fixture.store.acknowledge(&id.session_id, &newer, "shown")?;
    let before = fixture.store.session(&id.session_id)?;
    assert!(
        fixture
            .store
            .acknowledge_speech(&id.session_id, &id.turn_id, "speech_completed")
            .is_err()
    );
    for status in [
        "speech_started",
        "speech_completed",
        "speech_started",
        "speech_interrupted",
    ] {
        fixture
            .store
            .acknowledge_speech(&id.session_id, &id.turn_id, status)?;
        let after = fixture.store.session(&id.session_id)?;
        assert_eq!(after["state"], before["state"]);
        assert_eq!(after["state_revision"], before["state_revision"]);
    }
    let undisplayed = fixture.store.add_turn(
        &id.session_id,
        "pending",
        0,
        "assistant",
        "Not yet shown",
        "generated",
        &[],
    )?;
    assert!(
        fixture
            .store
            .acknowledge_speech(&id.session_id, &undisplayed, "speech_started")
            .is_err()
    );
    fixture.store.bump(&id.session_id, "idle")?;
    assert!(
        fixture
            .store
            .acknowledge_speech(&id.session_id, &id.turn_id, "speech_started")
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn cancelling_the_receipt_waiter_releases_its_foreground_guard() -> Result<()> {
    let worker = MemoryWorker::new();
    let foreground = worker.foreground();
    assert!(!worker.is_idle());
    let (gone, released) = oneshot::channel();
    struct Released(Option<oneshot::Sender<()>>);
    impl Drop for Released {
        fn drop(&mut self) {
            if let Some(send) = self.0.take() {
                let _ = send.send(());
            }
        }
    }
    let task = tokio::spawn(async move {
        let _foreground = foreground;
        let _released = Released(Some(gone));
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    released.await?;
    assert!(worker.is_idle());
    Ok(())
}

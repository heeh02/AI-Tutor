//! Permission/adoption tests use no native devices or remote provider.
use super::*;

fn monitored() -> (Lease, LoopReply) {
    let reply = LoopReply {
        session_id: "session".into(),
        request_id: "request".into(),
        generation: 10,
        audio_epoch: 7,
    };
    let lease = Lease {
        tag: LoopTag {
            client_loop_id: "loop".into(),
            loop_epoch: 1,
        },
        owner: Owner {
            namespace: "simulator".into(),
            learner_id: "learner".into(),
            learner_epoch: 0,
            session_id: "session".into(),
            generation: 10,
            language: "en".into(),
        },
        input: InputBinding {
            device_key: "mic".into(),
            topology_revision: 1,
        },
        deadline: Instant::now() + LEASE,
        phase: Phase::Reply,
        revision: 4,
        recording: Some(12),
        final_text: None,
        reply: Some(reply.clone()),
        cooldown: None,
        pause_reason: None,
        barge_in: true,
        reply_acknowledged: true,
        monitor_pending: false,
        monitoring: true,
        adoption: None,
    };
    (lease, reply)
}

#[test]
fn onset_claim_detaches_old_reply_before_new_capture_can_be_revoked() -> Result<()> {
    let (mut lease, reply) = monitored();
    let tag = lease.tag.clone();
    lease.claim_barge_candidate(&tag, &reply, 12)?;
    assert_eq!(lease.phase, Phase::Starting);
    assert_eq!(lease.recording, Some(12));
    assert!(lease.reply.is_none() && !lease.monitoring);
    assert!(lease.claim_barge_candidate(&tag, &reply, 12).is_err());
    // The existing late-reply revoke match now has no old identity to select.
    assert!(
        !lease
            .reply
            .as_ref()
            .is_some_and(|current| current == &reply)
    );
    Ok(())
}

#[test]
fn onset_cannot_adopt_without_exact_permission_owner_and_token() {
    for case in 0..7 {
        let (mut lease, mut reply) = monitored();
        let mut tag = lease.tag.clone();
        let mut token = 12;
        match case {
            0 => lease.barge_in = false,
            1 => lease.reply_acknowledged = false,
            2 => lease.deadline = Instant::now() - Duration::from_millis(1),
            3 => tag.loop_epoch += 1,
            4 => reply.audio_epoch += 1,
            5 => token += 1,
            _ => lease.owner.generation += 1,
        }
        assert!(lease.claim_barge_candidate(&tag, &reply, token).is_err());
        assert_eq!(lease.phase, Phase::Reply);
        assert!(lease.reply.is_some());
    }
}

#[test]
fn pause_clears_monitor_and_adoption_and_keeps_exact_discard_token() {
    let (mut lease, reply) = monitored();
    lease.adoption = Some(json!({"recording_generation":12}));
    let revoked = lease.revoke("hidden");
    assert_eq!(revoked.0, Some(12));
    assert_eq!(revoked.1, Some(reply));
    assert!(!lease.monitor_pending && !lease.monitoring && lease.adoption.is_none());
    assert_eq!(lease.projection()["rearm_ready"], false);
}

#[tokio::test]
async fn exact_old_reply_cancellation_keeps_other_running_and_pending_requests() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let app = Workstation::new(dir.path().into(), "synthetic-barge-cancel".into())?;
    let (old_cancel, old_receive) = watch::channel(false);
    let (other_cancel, other_receive) = watch::channel(false);
    for (rid, cancellation) in [("old", old_cancel), ("other", other_cancel)] {
        app.running.lock().unwrap().insert(
            rid.into(),
            Running {
                sid: "session".into(),
                cancellation,
                _task: tokio::spawn(async {}),
            },
        );
        app.pending.lock().unwrap().insert(
            format!("turn-{rid}"),
            Pending {
                sid: "session".into(),
                generation: 10,
                epoch: 0,
                reply: json!({"request_id":rid}),
                _foreground: None,
            },
        );
    }
    let reply = LoopReply {
        session_id: "session".into(),
        request_id: "old".into(),
        generation: 10,
        audio_epoch: 7,
    };
    app.cancel_barge_reply_request_locked(&reply);
    assert!(*old_receive.borrow());
    assert!(!*other_receive.borrow());
    assert!(!app.pending.lock().unwrap().contains_key("turn-old"));
    assert!(app.pending.lock().unwrap().contains_key("turn-other"));
    assert_eq!(app.metrics.lock().unwrap()["cancelled"], 1);
    app.cancel_barge_reply_request_locked(&reply);
    assert_eq!(app.metrics.lock().unwrap()["cancelled"], 1);
    // Leave no registered test tasks for application shutdown to contact.
    app.running.lock().unwrap().clear();
    app.close().await;
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn adoption_binds_once_and_late_old_reply_close_cannot_discard_it() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut app = Workstation::new(dir.path().into(), "synthetic-barge-adopt".into())?;
    app.audio.close().await;
    let (audio, mut owner, input) = AudioService::voice_loop_test_channel(dir.path().into());
    audio.discard_recording(); // Reserve a unique synthetic token without opening a device.
    let token = audio.status()["recording_generation"].as_u64().unwrap();
    owner.pump();
    Arc::get_mut(&mut app).unwrap().audio = Arc::new(audio);
    let sid = {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let learner = store.create_learner("Synthetic barge-in", "en", "guided")?;
        s(
            &store.create_session(s(&learner, "learner_id"))?,
            "session_id",
        )
        .to_owned()
    };
    *app.current.lock().unwrap() = Some(("simulator".into(), sid.clone()));
    let old_owner = app.voice_loop_owner(&sid)?;
    let (mut lease, mut reply) = monitored();
    reply.session_id = sid.clone();
    reply.generation = old_owner.generation;
    lease.owner = old_owner.clone();
    lease.input = input.clone();
    lease.recording = Some(token);
    lease.reply = Some(reply.clone());
    let tag = lease.tag.clone();
    lease.claim_barge_candidate(&tag, &reply, token)?;
    app.voice_loop.lock().unwrap().lease = Some(lease);
    let mut receipt = json!({});
    app.adopt_barge_in_recording(&sid, old_owner.generation, token, &input, &mut receipt)?;
    assert_eq!(receipt["session_generation"], old_owner.generation + 1);
    assert_eq!(receipt["recording_generation"], token);
    assert!(
        app.adopt_barge_in_recording(&sid, old_owner.generation, token, &input, &mut json!({}))
            .is_err()
    );
    assert_eq!(
        app.voice_loop_owner(&sid)?.generation,
        old_owner.generation + 1
    );
    app.pause_voice_loop_reply(&sid, &reply.request_id, reply.generation, "provider_failed");
    assert_eq!(app.voice_loop_echo(&tag)?["phase"], "starting");
    assert_eq!(app.audio.status()["recording_generation"], token);
    assert!(app.recording.lock().unwrap().is_some());
    app.pause_voice_loop(Some(&tag), "test_complete");
    owner.pump();
    // The fake owner must be driven while close sends its normal commands.
    let close = app.close();
    tokio::pin!(close);
    std::future::poll_fn(|cx| {
        owner.pump();
        let result = std::future::Future::poll(close.as_mut(), cx);
        if result.is_pending() {
            cx.waker().wake_by_ref();
        }
        result
    })
    .await;
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn failed_postclaim_promotion_revokes_old_media_before_self_abort_and_spares_new_token()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut app = Workstation::new(dir.path().into(), "synthetic-barge-failure".into())?;
    app.audio.close().await;
    let (audio, mut owner, input) = AudioService::voice_loop_test_channel(dir.path().into());
    audio.discard_recording();
    let token = audio.status()["recording_generation"].as_u64().unwrap();
    let audio_epoch = audio.reserve_speech_epoch();
    owner.pump();
    Arc::get_mut(&mut app).unwrap().audio = Arc::new(audio);
    let request = {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let learner = store.create_learner("Synthetic failed promotion", "en", "guided")?;
        let session = store.create_session(s(&learner, "learner_id"))?;
        let sid = s(&session, "session_id");
        let generation = store.bump(sid, "generating")?;
        let rid = uid("barge-failure");
        store.create_tutor_stream_with_speech(
            sid,
            &rid,
            generation,
            0,
            Some(audio_epoch),
            Some("synthetic-barge-failure"),
        )?;
        json!({"session_id":sid,"request_id":rid,"generation":generation,
            "learner_id":learner["learner_id"]})
    };
    let reply = LoopReply {
        session_id: s(&request, "session_id").into(),
        request_id: s(&request, "request_id").into(),
        generation: request["generation"].as_i64().unwrap(),
        audio_epoch,
    };
    *app.current.lock().unwrap() = Some(("simulator".into(), reply.session_id.clone()));
    app.register_segment_speech_locked("simulator", &request, 0, audio_epoch);
    let (cancellation, canceled) = watch::channel(false);
    app.running.lock().unwrap().insert(
        reply.request_id.clone(),
        Running {
            sid: reply.session_id.clone(),
            cancellation,
            _task: tokio::spawn(async {}),
        },
    );
    app.pending.lock().unwrap().insert(
        "old-pending".into(),
        Pending {
            sid: reply.session_id.clone(),
            generation: reply.generation,
            epoch: 0,
            reply: json!({"request_id":reply.request_id}),
            _foreground: None,
        },
    );
    let (mut lease, _) = monitored();
    lease.owner = app.voice_loop_owner(&reply.session_id)?;
    lease.input = input;
    lease.reply = Some(reply.clone());
    lease.recording = Some(token);
    let tag = lease.tag.clone();
    lease.claim_barge_candidate(&tag, &reply, token)?;
    app.voice_loop.lock().unwrap().lease = Some(lease);

    // Exercise the actual self-abort hazard: pause takes this task's handle.
    // The synchronous completion signal after settlement proves that the old
    // watch/gate/token have been revoked before cancellation reaches an await.
    let (start, begin) = tokio::sync::oneshot::channel();
    let (settled, result) = tokio::sync::oneshot::channel();
    let task_app = app.clone();
    let task_reply = reply.clone();
    let task = tokio::spawn(async move {
        begin.await.unwrap();
        let _control = task_app.control.lock().await;
        let failure = task_app.settle_claimed_barge_in_locked(
            &task_reply,
            token,
            Err(anyhow!("synthetic_audio_promotion_failed")),
        );
        let _ = settled.send(failure.is_err());
        tokio::task::yield_now().await;
    });
    app.voice_loop.lock().unwrap().barge_task = Some(task);
    start.send(()).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2), result).await??);
    assert_eq!(app.voice_loop_echo(&tag)?["phase"], "paused");
    assert_eq!(
        app.voice_loop_echo(&tag)?["pause_reason"],
        "barge_in_failed"
    );
    assert!(app.voice_loop.lock().unwrap().barge_task.is_none());
    assert!(*canceled.borrow());
    assert!(!app.pending.lock().unwrap().contains_key("old-pending"));
    assert_eq!(
        app.stores.lock().unwrap()["simulator"]
            .tutor_speech_state(&reply.session_id, &reply.request_id)?["speech_gate"],
        "closed"
    );
    assert_eq!(app.audio.speech_epoch(), audio_epoch + 1);
    assert_eq!(app.audio.status()["recording_generation"], token + 1);
    owner.pump();

    app.audio.discard_recording();
    let new_token = app.audio.status()["recording_generation"].as_u64().unwrap();
    owner.pump();
    {
        let mut state = app.voice_loop.lock().unwrap();
        let lease = state.lease.as_mut().unwrap();
        lease.phase = Phase::Listening;
        lease.deadline = Instant::now() + LEASE;
        lease.recording = Some(new_token);
        lease.pause_reason = None;
    }
    assert!(
        app.settle_claimed_barge_in_locked(&reply, token, Err(anyhow!("late_old_failure")))
            .is_err()
    );
    assert_eq!(app.voice_loop_echo(&tag)?["phase"], "listening");
    assert_eq!(app.audio.status()["recording_generation"], new_token);
    assert_eq!(app.audio.speech_epoch(), audio_epoch + 1);
    app.running.lock().unwrap().clear();
    app.pause_voice_loop(Some(&tag), "test_complete");
    let close = app.close();
    tokio::pin!(close);
    std::future::poll_fn(|cx| {
        owner.pump();
        let result = std::future::Future::poll(close.as_mut(), cx);
        if result.is_pending() {
            cx.waker().wake_by_ref();
        }
        result
    })
    .await;
    Ok(())
}

//! Permission/state and real Store receipt tests. No capture/output/model calls.
use super::*;
use crate::{
    audio::{SpeechEnd, SpeechOutcome, SpeechStarted},
    kimi::tutor_segments::SegmentDecoder,
};

fn tag(epoch: u64) -> LoopTag {
    LoopTag {
        client_loop_id: format!("synthetic-loop-{epoch}"),
        loop_epoch: epoch,
    }
}
fn lease(now: Instant) -> Lease {
    Lease {
        tag: tag(1),
        owner: Owner {
            namespace: "simulator".into(),
            learner_id: "synthetic-learner".into(),
            learner_epoch: 0,
            session_id: "synthetic-session".into(),
            generation: 10,
            language: "en".into(),
        },
        input: InputBinding {
            device_key: "synthetic-unopened-input".into(),
            topology_revision: 1,
        },
        deadline: now + LEASE,
        phase: Phase::Armed,
        revision: 1,
        recording: None,
        final_text: None,
        reply: None,
        cooldown: None,
        pause_reason: None,
        barge_in: false,
        reply_acknowledged: false,
        monitor_pending: false,
        monitoring: false,
        adoption: None,
    }
}
fn transcript(token: u64) -> Value {
    json!({"text":"A complete synthetic answer.","recording_endpoint":{"recording_generation":token,"reason":"speech_end","input_closed":true}})
}

#[test]
fn loop_tags_are_exact_and_expired_permission_cannot_be_renewed_in_place() -> Result<()> {
    assert!(LoopTag::parse(None)?.is_none());
    assert!(LoopTag::parse(Some(&json!({"client_loop_id":"nonce:1","loop_epoch":1})))?.is_some());
    for value in [
        json!({"client_loop_id":"bad id","loop_epoch":1}),
        json!({"client_loop_id":"有效","loop_epoch":1}),
        json!({"client_loop_id":"ok","loop_epoch":0}),
        json!({"client_loop_id":"ok","loop_epoch":1,"extra":true}),
    ] {
        assert!(LoopTag::parse(Some(&value)).is_err());
    }
    let now = Instant::now();
    let mut l = lease(now);
    assert!(l.live(now + LEASE - Duration::from_millis(1)));
    assert!(!l.live(now + LEASE));
    let mut state = VoiceLoopControl {
        epoch: 1,
        lease: Some(l.clone()),
        barge_task: None,
    };
    assert!(state.live_mut(&tag(2)).is_err());
    assert!(state.live_mut(&tag(1)).is_ok());
    l.deadline = now - Duration::from_millis(1);
    state.lease = Some(l);
    assert!(state.live_mut(&tag(1)).is_err());
    state.epoch = 2;
    let mut replacement = lease(now);
    replacement.tag = tag(2);
    state.lease = Some(replacement);
    assert!(state.live_mut(&tag(1)).is_err());
    assert!(state.live_mut(&tag(2)).is_ok());
    Ok(())
}

#[test]
fn generation_allowance_is_phase_bound_and_never_crosses_owner_epoch() {
    for (phase, allowance) in [
        (Phase::Armed, 0),
        (Phase::Starting, 1),
        (Phase::Listening, 0),
        (Phase::Finalizing, 0),
        (Phase::ReadyToSubmit, 0),
        (Phase::Submitting, 2),
        (Phase::Reply, 0),
        (Phase::Cooldown, 0),
    ] {
        let mut l = lease(Instant::now());
        l.phase = phase;
        for delta in -1..=3 {
            let mut owner = l.owner.clone();
            owner.generation += delta;
            assert_eq!(
                l.matches_owner(&owner),
                (0..=allowance).contains(&delta),
                "{phase:?}, delta={delta}"
            );
        }
        let mut changed = l.owner.clone();
        changed.learner_epoch += 1;
        assert!(!l.matches_owner(&changed));
        let mut changed = l.owner.clone();
        changed.namespace = "real".into();
        assert!(!l.matches_owner(&changed));
        let mut changed = l.owner.clone();
        changed.learner_id.push('x');
        assert!(!l.matches_owner(&changed));
        let mut changed = l.owner.clone();
        changed.session_id.push('x');
        assert!(!l.matches_owner(&changed));
        let mut changed = l.owner.clone();
        changed.language = "zh".into();
        assert!(!l.matches_owner(&changed));
    }
}

#[test]
fn tail_requires_five_hundred_continuous_ready_milliseconds_even_after_armed() {
    let now = Instant::now();
    let mut l = lease(now);
    l.phase = Phase::Reply;
    l.recording = Some(11);
    l.final_text = Some("old".into());
    l.output_ready(true, now);
    assert_eq!(l.phase, Phase::Cooldown);
    l.output_ready(true, now + Duration::from_millis(499));
    assert_eq!(l.phase, Phase::Cooldown);
    l.output_ready(false, now + Duration::from_millis(500));
    assert_eq!(l.phase, Phase::Reply);
    assert!(l.cooldown.is_none());
    l.output_ready(true, now + Duration::from_millis(501));
    l.output_ready(true, now + Duration::from_millis(1000));
    assert_eq!(l.phase, Phase::Cooldown);
    l.output_ready(true, now + Duration::from_millis(1001));
    assert_eq!(l.phase, Phase::Armed);
    assert!(l.recording.is_none() && l.final_text.is_none());
    let revision = l.revision;
    l.output_ready(true, now + Duration::from_millis(1002));
    assert_eq!(
        l.revision, revision,
        "same readiness cannot invent transitions"
    );
    l.output_ready(false, now + Duration::from_millis(1003));
    assert_eq!(l.phase, Phase::Reply);
    assert!(l.cooldown.is_none());
    l.output_ready(true, now + Duration::from_millis(1004));
    l.output_ready(true, now + Duration::from_millis(1503));
    assert_eq!(l.phase, Phase::Cooldown);
    l.output_ready(true, now + Duration::from_millis(1504));
    assert_eq!(l.phase, Phase::Armed);
}

#[test]
fn only_exact_bound_final_text_can_be_consumed_once() -> Result<()> {
    let mut original = lease(Instant::now());
    original.phase = Phase::ReadyToSubmit;
    original.recording = Some(8);
    original.final_text = Some("Final text.".into());
    for (sid, token, text, auto, nonce) in [
        ("other", Some(8), "Final text.", true, Some("nonce")),
        (
            "synthetic-session",
            Some(9),
            "Final text.",
            true,
            Some("nonce"),
        ),
        (
            "synthetic-session",
            None,
            "Final text.",
            true,
            Some("nonce"),
        ),
        (
            "synthetic-session",
            Some(8),
            "Edited text.",
            true,
            Some("nonce"),
        ),
        (
            "synthetic-session",
            Some(8),
            "Final text. ",
            true,
            Some("nonce"),
        ),
        (
            "synthetic-session",
            Some(8),
            "Final text.",
            false,
            Some("nonce"),
        ),
        ("synthetic-session", Some(8), "Final text.", true, None),
    ] {
        let mut l = original.clone();
        assert!(l.begin_submit(sid, token, text, auto, nonce).is_err());
        assert_eq!(l.phase, Phase::ReadyToSubmit);
        assert_eq!(l.final_text, original.final_text);
    }
    let mut l = original;
    l.begin_submit(
        "synthetic-session",
        Some(8),
        "Final text.",
        true,
        Some("nonce"),
    )?;
    assert_eq!(l.phase, Phase::Submitting);
    assert!(l.final_text.is_none());
    assert!(
        l.begin_submit(
            "synthetic-session",
            Some(8),
            "Final text.",
            true,
            Some("nonce")
        )
        .is_err()
    );
    Ok(())
}

struct Fixture {
    app: Arc<Workstation>,
    _dir: tempfile::TempDir,
    lid: String,
    sid: String,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "synthetic-voice-loop".into())?;
        let (lid, sid) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let lid = s(
                &store.create_learner("Synthetic voice loop", "en", "guided")?,
                "learner_id",
            )
            .to_owned();
            let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
            (lid, sid)
        };
        *app.current.lock().unwrap() = Some(("simulator".into(), sid.clone()));
        let mut l = lease(Instant::now());
        l.owner = app.voice_loop_owner(&sid)?;
        app.voice_loop.lock().unwrap().lease = Some(l);
        Ok(Self {
            app,
            _dir: dir,
            lid,
            sid,
        })
    }
    fn stream(&self) -> Result<LoopReply> {
        let audio_epoch = self.app.audio.reserve_speech_epoch();
        let request = {
            let stores = self.app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let generation = store.bump(&self.sid, "generating")?;
            let rid = uid("synthetic-loop-reply");
            store.create_tutor_stream_with_speech(
                &self.sid,
                &rid,
                generation,
                0,
                Some(audio_epoch),
                Some("synthetic-loop-nonce"),
            )?;
            let request = json!({"session_id":self.sid,"request_id":rid,"learner_id":self.lid,"generation":generation,"context":{"evidence":[]}});
            let mut decoder = SegmentDecoder::new(&request, generation as u64)?;
            for (sequence, text) in ["First synthetic sentence. ", "Second synthetic sentence."]
                .into_iter()
                .enumerate()
            {
                let line = json!({"schema_version":"1.0","type":"segment","request_id":rid,"sequence":sequence,"text":text,"used_evidence_ids":[]});
                for segment in decoder.push(format!("{line}\n").as_bytes())? {
                    store.append_tutor_segment(&self.sid, &rid, &segment)?;
                }
            }
            request
        };
        self.app
            .register_segment_speech_locked("simulator", &request, 0, audio_epoch);
        Ok(LoopReply {
            session_id: self.sid.clone(),
            request_id: s(&request, "request_id").into(),
            generation: request["generation"].as_i64().unwrap(),
            audio_epoch,
        })
    }
    fn complete_text(&self, reply: &LoopReply) -> Result<()> {
        let stores = self.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        store.mark_tutor_final(&self.sid, &reply.request_id)?;
        for sequence in 0..2 {
            store.ack_tutor_segment(&self.sid, &reply.request_id, reply.generation, sequence)?;
        }
        store.complete_tutor_stream(&self.sid, &reply.request_id, &[])?;
        Ok(())
    }
    fn receipt(&self, reply: &LoopReply, end: SpeechEnd) -> Result<()> {
        let stores = self.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let work = store
            .reserve_next_segment_speech(
                &self.sid,
                &reply.request_id,
                reply.generation,
                0,
                &uid("fake-attempt"),
                reply.audio_epoch,
            )?
            .ok_or_else(|| anyhow!("No fake receipt work"))?;
        let id = format!("synthetic-playback-{}", work.sequence);
        store.record_tutor_speech_started(
            &work,
            &SpeechStarted {
                playback_id: id.clone(),
                epoch: reply.audio_epoch,
                engine_kind: "qwen_pcm",
                milestone: "first_callback_submission",
            },
        )?;
        store.record_tutor_speech_outcome(
            &work,
            &SpeechOutcome {
                playback_id: id,
                epoch: reply.audio_epoch,
                state: end,
                engine_kind: "qwen_pcm",
                callback_submitted_frames: 480,
                predicted_output_drained: end == SpeechEnd::Completed,
                speaker_output_measured: false,
                reason: None,
            },
        )?;
        Ok(())
    }
    fn install_reply_lease(&self, reply: &LoopReply) {
        let mut state = self.app.voice_loop.lock().unwrap();
        let l = state.lease.as_mut().unwrap();
        l.owner.generation = reply.generation;
        l.reply = Some(reply.clone());
        l.phase = Phase::Reply;
    }
}

#[tokio::test]
async fn two_rounds_require_new_final_tokens_and_each_full_reply_receipt() -> Result<()> {
    let f = Fixture::new()?;
    for token in [11, 12] {
        // Device opening is deliberately outside this fixture. Enter only the
        // post-permission Starting state and exercise its precise callback path.
        {
            let mut state = f.app.voice_loop.lock().unwrap();
            let l = state.lease.as_mut().unwrap();
            assert_eq!(l.phase, Phase::Armed);
            l.change(Phase::Starting);
        }
        assert!(f.app.loop_recording_reserved(&tag(1), token));
        f.app.loop_recording_started(&tag(1), token)?;
        f.app.loop_recording_finalizing(token);
        f.app.loop_recording_final(token, &transcript(token));
        {
            let mut state = f.app.voice_loop.lock().unwrap();
            state.live_mut(&tag(1))?.begin_submit(
                &f.sid,
                Some(token),
                "A complete synthetic answer.",
                true,
                Some("nonce"),
            )?;
        }
        let reply = f.stream()?;
        f.app.loop_reply_registered(&tag(1), reply.clone())?;
        assert!(!f.app.voice_loop_reply_output_ready_locked(&reply)?);
        f.complete_text(&reply)?;
        assert!(!f.app.voice_loop_reply_output_ready_locked(&reply)?);
        f.receipt(&reply, SpeechEnd::Completed)?;
        assert!(!f.app.voice_loop_reply_output_ready_locked(&reply)?);
        f.receipt(&reply, SpeechEnd::Completed)?;
        assert!(f.app.voice_loop_reply_output_ready_locked(&reply)?);
        let now = Instant::now();
        {
            let mut state = f.app.voice_loop.lock().unwrap();
            let l = state.live_mut(&tag(1))?;
            l.output_ready(true, now);
            l.output_ready(true, now + OUTPUT_TAIL);
            assert_eq!(l.phase, Phase::Armed);
        }
        f.app.loop_recording_final(token, &transcript(token));
        assert_eq!(
            f.app.voice_loop_echo(&tag(1))?["phase"],
            "armed",
            "late old final cannot become next turn"
        );
    }
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn non_speech_end_partial_error_and_overlong_final_pause_without_submission() -> Result<()> {
    let f = Fixture::new()?;
    let mut invalid = vec![];
    let mut value = transcript(8);
    value["recording_endpoint"]["input_closed"] = json!(false);
    invalid.push(value);
    let mut value = transcript(8);
    value["recording_endpoint"]["reason"] = json!("duration_limit");
    invalid.push(value);
    let mut value = transcript(8);
    value["recording_endpoint"]["recording_generation"] = json!(9);
    invalid.push(value);
    let mut value = transcript(8);
    value["error"] = json!("synthetic ASR failure");
    invalid.push(value);
    let mut value = transcript(8);
    value["text"] = json!(" ");
    invalid.push(value);
    let mut value = transcript(8);
    value["text"] = json!("字".repeat(4001));
    invalid.push(value);
    for value in invalid {
        {
            let mut l = lease(Instant::now());
            l.owner = f.app.voice_loop_owner(&f.sid)?;
            l.phase = Phase::Finalizing;
            l.recording = Some(8);
            f.app.voice_loop.lock().unwrap().lease = Some(l);
        }
        f.app.loop_recording_final(8, &value);
        let state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_ref().unwrap();
        assert_eq!(l.phase, Phase::Paused);
        assert!(l.final_text.is_none());
        assert_eq!(l.pause_reason.as_deref(), Some("transcript_needs_review"));
    }
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn stale_recording_callbacks_do_not_advance_new_lease() -> Result<()> {
    let f = Fixture::new()?;
    {
        let mut state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_mut().unwrap();
        l.tag = tag(2);
        l.phase = Phase::Starting;
        l.recording = Some(12);
    }
    assert!(!f.app.loop_recording_reserved(&tag(1), 11));
    assert!(f.app.loop_recording_started(&tag(1), 11).is_err());
    f.app.loop_recording_started(&tag(2), 12)?;
    f.app.loop_recording_finalizing(11);
    f.app.loop_recording_final(11, &transcript(11));
    assert_eq!(f.app.voice_loop_echo(&tag(2))?["phase"], "listening");
    assert!(
        f.app
            .pause_voice_loop(Some(&tag(1)), "heartbeat_expired")
            .is_none()
    );
    assert_eq!(f.app.voice_loop_echo(&tag(2))?["phase"], "listening");
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn reply_registration_rechecks_real_generation_learner_epoch_and_audio_epoch() -> Result<()> {
    let f = Fixture::new()?;
    f.app
        .voice_loop
        .lock()
        .unwrap()
        .lease
        .as_mut()
        .unwrap()
        .phase = Phase::Submitting;
    let reply = f.stream()?;
    let mut wrong = reply.clone();
    wrong.generation -= 1;
    assert!(f.app.loop_reply_registered(&tag(1), wrong).is_err());
    let mut wrong = reply.clone();
    wrong.audio_epoch = reply.audio_epoch.wrapping_add(1);
    assert!(f.app.loop_reply_registered(&tag(1), wrong).is_err());
    f.app.stores.lock().unwrap()["simulator"].db.execute(
        "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
        [&f.lid],
    )?;
    assert!(f.app.loop_reply_registered(&tag(1), reply).is_err());
    assert_eq!(f.app.voice_loop_echo(&tag(1))?["phase"], "submitting");
    assert!(
        f.app
            .voice_loop
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .unwrap()
            .reply
            .is_none()
    );
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn pause_revokes_precise_output_before_waiting_for_control() -> Result<()> {
    let f = Fixture::new()?;
    let reply = f.stream()?;
    f.install_reply_lease(&reply);
    let guard = f.app.control.lock().await;
    let copied = f
        .app
        .pause_voice_loop(Some(&tag(1)), "heartbeat_expired")
        .unwrap();
    assert_ne!(f.app.audio.speech_epoch(), reply.audio_epoch);
    assert_eq!(copied.request_id, reply.request_id);
    assert_eq!(f.app.voice_loop_echo(&tag(1))?["phase"], "paused");
    drop(guard);
    f.app
        .close_segment_speech(
            &copied.session_id,
            &copied.request_id,
            copied.generation,
            "user",
        )
        .await?;
    assert_eq!(
        f.app.stores.lock().unwrap()["simulator"].tutor_speech_state(&f.sid, &reply.request_id)?["speech_gate"],
        "closed"
    );
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn late_pause_and_close_cannot_touch_new_capture_or_manual_audio_epoch() -> Result<()> {
    let f = Fixture::new()?;
    let reply = f.stream()?;
    f.install_reply_lease(&reply);
    let old_capture = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    f.app
        .voice_loop
        .lock()
        .unwrap()
        .lease
        .as_mut()
        .unwrap()
        .recording = Some(old_capture);
    f.app.audio.discard_recording(); // advances a token, opens no device
    let new_capture = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    let new_audio = f.app.audio.reserve_speech_epoch();
    let copied = f
        .app
        .pause_voice_loop(Some(&tag(1)), "manual_action")
        .unwrap();
    assert_eq!(f.app.audio.status()["recording_generation"], new_capture);
    assert_eq!(f.app.audio.speech_epoch(), new_audio);
    f.app
        .close_segment_speech(
            &copied.session_id,
            &copied.request_id,
            copied.generation,
            "user",
        )
        .await?;
    assert_eq!(f.app.audio.speech_epoch(), new_audio);
    assert_eq!(f.app.audio.status()["recording_generation"], new_capture);
    assert!(
        f.app
            .pause_voice_loop(Some(&tag(1)), "heartbeat_expired")
            .is_none()
    );
    assert_eq!(f.app.audio.speech_epoch(), new_audio);
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn interrupted_or_new_audio_epoch_reply_never_grants_rearm() -> Result<()> {
    for interrupted in [false, true] {
        let f = Fixture::new()?;
        let reply = f.stream()?;
        f.complete_text(&reply)?;
        f.receipt(
            &reply,
            if interrupted {
                SpeechEnd::Interrupted
            } else {
                SpeechEnd::Completed
            },
        )?;
        if !interrupted {
            f.receipt(&reply, SpeechEnd::Completed)?;
            assert!(f.app.voice_loop_reply_output_ready_locked(&reply)?);
            f.app.audio.reserve_speech_epoch();
        }
        assert!(!f.app.voice_loop_reply_output_ready_locked(&reply)?);
        f.app.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn old_round_start_and_send_rejections_preserve_current_listening() -> Result<()> {
    let f = Fixture::new()?;
    f.app.stores.lock().unwrap()["simulator"].bump(&f.sid, "idle")?;
    f.app.audio.discard_recording(); // Reserve a later token without opening input.
    let token = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    let owner = f.app.voice_loop_owner(&f.sid)?;
    let generation = owner.generation;
    let revision = {
        let mut state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_mut().unwrap();
        l.owner = owner;
        l.recording = Some(token);
        l.change(Phase::Listening);
        l.revision
    };
    let last_input = *f.app.last_input.lock().unwrap();
    let loop_tag = json!({"client_loop_id":tag(1).client_loop_id,"loop_epoch":1});
    let requests = [
        (
            "/api/audio/record/start".to_owned(),
            json!({
                "session_id":f.sid,"expected_generation":generation-1,
                "device_key":"synthetic-unopened-input","expected_input_topology_revision":1,
                "endpoint_mode":"voice_endpoint_v1","voice_loop":loop_tag
            }),
        ),
        (
            format!("/api/sessions/{}/turns", f.sid),
            json!({
                "text":"Delayed previous round.","expected_generation":generation-1,
                "recording_generation":token-1,"voice_loop":loop_tag,
                "auto_speak":true,"client_submission_id":"delayed-previous-round"
            }),
        ),
    ];
    for (path, body) in requests {
        let error = dispatch(f.app.clone(), &Method::POST, &path, &body)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("会话已变化"), "{error}");
        let state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_ref().unwrap();
        assert_eq!(l.phase, Phase::Listening);
        assert_eq!(l.recording, Some(token));
        assert_eq!(l.revision, revision);
        assert!(l.pause_reason.is_none());
        drop(state);
        assert_eq!(f.app.audio.status()["recording_generation"], token);
        assert_eq!(
            f.app.stores.lock().unwrap()["simulator"].session(&f.sid)?["generation"],
            generation
        );
        assert_eq!(*f.app.last_input.lock().unwrap(), last_input);
    }
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn precise_old_recording_reply_and_revision_cannot_pause_next_round_in_same_lease()
-> Result<()> {
    let f = Fixture::new()?;
    let old_reply = f.stream()?;
    f.install_reply_lease(&old_reply);
    let old_revision = f
        .app
        .voice_loop
        .lock()
        .unwrap()
        .lease
        .as_ref()
        .unwrap()
        .revision;
    let old_token = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    f.app.audio.discard_recording();
    let next_token = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    let next_epoch = f.app.audio.reserve_speech_epoch();
    let revision = {
        let mut state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_mut().unwrap();
        l.recording = Some(next_token);
        l.reply = None;
        l.change(Phase::Listening);
        l.revision
    };
    f.app
        .pause_voice_loop_recording(old_token, "old_asr_failed");
    f.app.pause_voice_loop_reply(
        &old_reply.session_id,
        &old_reply.request_id,
        old_reply.generation,
        "old_speech_failed",
    );
    assert!(!f.app.renew_loop_heartbeat(
        &tag(1),
        old_revision,
        Err(anyhow!("old context changed"))
    )?);
    {
        let state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_ref().unwrap();
        assert_eq!(l.tag, tag(1), "a lease spans multiple rounds");
        assert_eq!(l.phase, Phase::Listening);
        assert_eq!(l.recording, Some(next_token));
        assert_eq!(l.revision, revision);
        assert!(l.pause_reason.is_none());
    }
    assert_eq!(f.app.audio.status()["recording_generation"], next_token);
    assert_eq!(f.app.audio.speech_epoch(), next_epoch);
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn segment_failure_pauses_reply_but_owned_superseded_transition_does_not() -> Result<()> {
    for reason in ["audio_failed", "receipt_store_failed", "superseded"] {
        let f = Fixture::new()?;
        let reply = f.stream()?;
        f.install_reply_lease(&reply);
        {
            let _control = f.app.control.lock().await;
            f.app
                .fail_segment_speech_locked("simulator", &f.sid, &reply.request_id, reason)
                .await;
        }
        {
            let state = f.app.voice_loop.lock().unwrap();
            let l = state.lease.as_ref().unwrap();
            if reason == "superseded" {
                assert_eq!(l.phase, Phase::Reply);
                assert!(l.pause_reason.is_none());
            } else {
                assert_eq!(l.phase, Phase::Paused);
                assert_eq!(l.pause_reason.as_deref(), Some(reason));
                assert!(l.final_text.is_none());
            }
        }
        assert_ne!(f.app.audio.speech_epoch(), reply.audio_epoch);
        assert!(!f.app.voice_loop_reply_output_ready_locked(&reply)?);
        assert_eq!(
            f.app.stores.lock().unwrap()["simulator"]
                .tutor_speech_state(&f.sid, &reply.request_id)?["speech_gate"],
            "closed"
        );
        f.app.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn heartbeat_validation_finishing_after_expiry_cannot_renew_permission() -> Result<()> {
    let f = Fixture::new()?;
    let (revision, expired_deadline) = {
        let mut state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_mut().unwrap();
        let revision = l.revision;
        // Model a successful context read that returns after its initial lease expired.
        l.deadline = Instant::now() - Duration::from_millis(1);
        (revision, l.deadline)
    };
    assert!(
        f.app
            .renew_loop_heartbeat(&tag(1), revision, Ok(()))
            .is_err()
    );
    {
        let state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_ref().unwrap();
        assert_eq!(l.deadline, expired_deadline);
        assert_eq!(l.revision, revision);
        assert!(!l.live(Instant::now()));
    }
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn pause_revokes_bound_capture_and_output_while_store_is_locked() -> Result<()> {
    let f = Fixture::new()?;
    let reply = f.stream()?;
    f.install_reply_lease(&reply);
    let token = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    f.app
        .voice_loop
        .lock()
        .unwrap()
        .lease
        .as_mut()
        .unwrap()
        .recording = Some(token);
    let stores = f.app.stores.lock().unwrap();
    let app = f.app.clone();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let reply = app.pause_voice_loop(Some(&tag(1)), "heartbeat_expired");
        let _ = send.send(reply);
    });
    let paused = receive.recv_timeout(Duration::from_secs(1));
    // Release the lock before an assertion so a regression cannot strand a thread.
    drop(stores);
    worker.join().expect("fake revocation worker panicked");
    let copied = paused
        .expect("revocation waited for the Store lock")
        .unwrap();
    assert_eq!(copied.request_id, reply.request_id);
    assert_ne!(f.app.audio.status()["recording_generation"], token);
    assert_ne!(f.app.audio.speech_epoch(), reply.audio_epoch);
    assert_eq!(f.app.voice_loop_echo(&tag(1))?["phase"], "paused");
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn begin_recording_detaches_previous_reply_before_awaiting_capture_start() -> Result<()> {
    let f = Fixture::new()?;
    let reply = f.stream()?;
    f.install_reply_lease(&reply);
    f.complete_text(&reply)?;
    for _ in 0..2 {
        f.receipt(&reply, SpeechEnd::Completed)?;
    }
    assert!(f.app.voice_loop_reply_output_ready_locked(&reply)?);
    let token = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    let revision = {
        let mut state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_mut().unwrap();
        let now = Instant::now();
        l.output_ready(true, now);
        l.output_ready(true, now + OUTPUT_TAIL);
        assert_eq!(l.phase, Phase::Armed);
        assert!(l.reply.is_some());
        l.begin_recording()?;
        assert!(l.reply.is_none() && l.cooldown.is_none());
        l.revision
    };
    assert!(f.app.loop_recording_reserved(&tag(1), token));
    dispatch(
        f.app.clone(),
        &Method::POST,
        &format!(
            "/api/sessions/{}/streams/{}/speech/close",
            f.sid, reply.request_id
        ),
        &json!({"generation":reply.generation,"reason":"hidden"}),
    )
    .await?;
    {
        let state = f.app.voice_loop.lock().unwrap();
        let l = state.lease.as_ref().unwrap();
        assert_eq!(l.phase, Phase::Starting);
        assert_eq!(l.recording, Some(token));
        assert_eq!(l.revision, revision);
        assert!(l.pause_reason.is_none());
    }
    assert_eq!(f.app.audio.status()["recording_generation"], token);
    f.app.close().await;
    Ok(())
}

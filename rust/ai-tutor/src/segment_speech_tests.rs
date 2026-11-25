//! Synthetic receipts only: the fake speaker never calls an output device/model.
use super::*;
use crate::kimi::tutor_segments::SegmentDecoder;
use tokio::sync::mpsc;

struct FakeSpeaker {
    calls: mpsc::Sender<FakeCall>,
}
struct FakeCall {
    work: TutorSpeechWork,
    started: Option<oneshot::Sender<SpeechStarted>>,
    outcome: oneshot::Sender<Result<SpeechOutcome>>,
}
impl SegmentSpeaker for FakeSpeaker {
    fn speak<'a>(
        &'a self,
        work: &'a TutorSpeechWork,
        started: oneshot::Sender<SpeechStarted>,
    ) -> BoxFuture<'a, Result<SpeechOutcome>> {
        Box::pin(async move {
            let (send, receive) = oneshot::channel();
            self.calls
                .send(FakeCall {
                    work: work.clone(),
                    started: Some(started),
                    outcome: send,
                })
                .await
                .map_err(|_| anyhow!("fake receiver gone"))?;
            receive.await?
        })
    }
}
impl FakeCall {
    fn start(&mut self) {
        self.started
            .take()
            .unwrap()
            .send(SpeechStarted {
                playback_id: format!("fake-{}", self.work.sequence),
                epoch: self.work.audio_epoch,
                engine_kind: "qwen_pcm",
                milestone: "first_callback_submission",
            })
            .unwrap();
    }
    fn finish(self, state: SpeechEnd) {
        self.outcome
            .send(Ok(SpeechOutcome {
                playback_id: format!("fake-{}", self.work.sequence),
                epoch: self.work.audio_epoch,
                state,
                engine_kind: "qwen_pcm",
                callback_submitted_frames: 480,
                predicted_output_drained: state == SpeechEnd::Completed,
                speaker_output_measured: false,
                reason: None,
            }))
            .unwrap();
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    app: Arc<Workstation>,
    key: AutoSpeechKey,
    speaker: Arc<FakeSpeaker>,
    calls: mpsc::Receiver<FakeCall>,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "synthetic-segment-speech".into())?;
        let audio_epoch = app.audio.reserve_speech_epoch();
        let (key, request) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let learner = store.create_learner("Synthetic audio queue", "en", "guided")?;
            let lid = s(&learner, "learner_id");
            let sid = s(&store.create_session(lid)?, "session_id").to_owned();
            let generation = store.bump(&sid, "generating")?;
            let rid = uid("synthetic-stream");
            store.create_tutor_stream_with_speech(
                &sid,
                &rid,
                generation,
                0,
                Some(audio_epoch),
                Some("synthetic-nonce"),
            )?;
            let request = json!({"request_id":rid,"session_id":sid,"learner_id":lid,"generation":generation,"context":{"evidence":[]}});
            let mut decoder = SegmentDecoder::new(&request, generation as u64)?;
            for (sequence, text) in ["First synthetic sentence. ", "Second synthetic sentence."]
                .iter()
                .enumerate()
            {
                let line = json!({"schema_version":"1.0","type":"segment","request_id":rid,"sequence":sequence,"text":text,"used_evidence_ids":[]});
                for segment in decoder.push(format!("{line}\n").as_bytes())? {
                    store.append_tutor_segment(&sid, &rid, &segment)?;
                }
            }
            (
                AutoSpeechKey {
                    namespace: "simulator".into(),
                    learner_id: lid.into(),
                    session_id: sid,
                    request_id: rid,
                    generation,
                    learner_epoch: 0,
                    audio_epoch,
                },
                request,
            )
        };
        *app.current.lock().unwrap() = Some((key.namespace.clone(), key.session_id.clone()));
        app.register_segment_speech_locked(&key.namespace, &request, 0, audio_epoch);
        let (calls, receive) = mpsc::channel(4);
        Ok(Self {
            _dir: dir,
            app,
            key,
            speaker: Arc::new(FakeSpeaker { calls }),
            calls: receive,
        })
    }
    fn ack(&self, sequence: usize) -> Result<()> {
        self.app.stores.lock().unwrap()[&self.key.namespace].ack_tutor_segment(
            &self.key.session_id,
            &self.key.request_id,
            self.key.generation,
            sequence,
        )
    }
    fn state(&self) -> Result<Value> {
        self.app.stores.lock().unwrap()[&self.key.namespace]
            .tutor_speech_state(&self.key.session_id, &self.key.request_id)
    }
    fn phase(&self) -> Result<String> {
        let snapshot = self.app.stores.lock().unwrap()[&self.key.namespace]
            .tutor_stream_snapshot(&self.key.session_id, &self.key.request_id)?;
        Ok(s(&snapshot, "phase").into())
    }
    async fn kick(&self) {
        let _control = self.app.control.lock().await;
        self.app
            .kick_segment_speech_with_locked(
                &self.key.namespace,
                &self.key.session_id,
                &self.key.request_id,
                self.speaker.clone(),
            )
            .await;
    }
    async fn call(&mut self) -> Result<FakeCall> {
        tokio::time::timeout(Duration::from_secs(2), self.calls.recv())
            .await?
            .ok_or_else(|| anyhow!("fake speaker closed"))
    }
    async fn wait_for(&self, predicate: impl Fn(&Value) -> bool) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if predicate(&self.state()?) {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await?
    }
    async fn idle(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !self.app.memory_worker.is_idle() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        Ok(())
    }
}

#[tokio::test]
async fn final_before_display_ack_still_plays_once_in_order_and_text_complete_is_not_audio_complete()
-> Result<()> {
    let mut f = Fixture::new()?;
    f.app.stores.lock().unwrap()[&f.key.namespace]
        .mark_tutor_final(&f.key.session_id, &f.key.request_id)?;
    f.kick().await;
    f.idle().await?;
    assert!(f.calls.try_recv().is_err());
    f.ack(0)?;
    f.kick().await;
    let mut first = f.call().await?;
    assert_eq!(first.work.sequence, 0);
    f.ack(1)?;
    f.app.stores.lock().unwrap()[&f.key.namespace].complete_tutor_stream(
        &f.key.session_id,
        &f.key.request_id,
        &[],
    )?;
    assert_eq!(f.phase()?, "completed");
    assert_eq!(f.state()?["speech_complete"], false);
    f.kick().await;
    assert!(f.calls.try_recv().is_err(), "no synthesis overlap");
    first.start();
    first.finish(SpeechEnd::Completed);
    let mut second = f.call().await?;
    assert_eq!(second.work.sequence, 1);
    assert_eq!(second.work.audio_epoch, f.key.audio_epoch);
    assert_eq!(f.state()?["speech_completed_through"], 0);
    second.start();
    second.finish(SpeechEnd::Completed);
    f.wait_for(|s| s["speech_complete"] == true).await?;
    f.idle().await?;
    f.ack(1)?;
    f.kick().await;
    f.idle().await?;
    assert!(
        f.calls.try_recv().is_err(),
        "duplicate final ACK must not replay"
    );
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn close_before_first_ack_and_its_retry_cannot_stop_a_later_manual_epoch() -> Result<()> {
    let mut f = Fixture::new()?;
    f.app
        .close_segment_speech(
            &f.key.session_id,
            &f.key.request_id,
            f.key.generation,
            "hidden",
        )
        .await?;
    assert_eq!(f.state()?["speech_gate"], "closed");
    f.ack(0)?;
    f.kick().await;
    assert!(f.calls.try_recv().is_err());
    let manual_epoch = f.app.audio.reserve_speech_epoch();
    f.app
        .close_segment_speech(
            &f.key.session_id,
            &f.key.request_id,
            f.key.generation,
            "hidden",
        )
        .await?;
    assert_eq!(f.app.audio.speech_epoch(), manual_epoch);
    assert_eq!(f.phase()?, "generating", "audio close must not cancel text");
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn bound_close_invalidates_audio_before_waiting_for_the_teaching_lock() -> Result<()> {
    let f = Fixture::new()?;
    let control = f.app.control.lock().await;
    let app = f.app.clone();
    let key = f.key.clone();
    let closing = tokio::spawn(async move {
        app.close_segment_speech(&key.session_id, &key.request_id, key.generation, "user")
            .await
    });
    tokio::task::yield_now().await;
    assert_ne!(f.app.audio.speech_epoch(), f.key.audio_epoch);
    assert!(!closing.is_finished());
    drop(control);
    closing.await??;
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn a_failed_persistent_close_does_not_allow_late_ack_to_renew_the_gate_epoch() -> Result<()> {
    let mut f = Fixture::new()?;
    f.app.stores.lock().unwrap()[&f.key.namespace].db.execute_batch("CREATE TEMP TRIGGER fail_close BEFORE UPDATE OF speech_gate ON tutor_streams WHEN NEW.speech_gate='closed' BEGIN SELECT RAISE(ABORT,'synthetic close failure'); END;")?;
    assert!(
        f.app
            .close_segment_speech(
                &f.key.session_id,
                &f.key.request_id,
                f.key.generation,
                "user"
            )
            .await
            .is_err()
    );
    assert_eq!(f.state()?["speech_gate"], "open");
    assert_ne!(f.app.audio.speech_epoch(), f.key.audio_epoch);
    f.ack(0)?;
    f.kick().await;
    assert!(f.calls.try_recv().is_err());
    f.app.stores.lock().unwrap()[&f.key.namespace]
        .db
        .execute_batch("DROP TRIGGER fail_close;")?;
    f.app
        .close_segment_speech(
            &f.key.session_id,
            &f.key.request_id,
            f.key.generation,
            "user",
        )
        .await?;
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn failed_start_receipt_commit_stops_the_flow_and_never_synthesizes_the_tail() -> Result<()> {
    let mut f = Fixture::new()?;
    f.ack(0)?;
    f.ack(1)?;
    f.app.stores.lock().unwrap()[&f.key.namespace].db.execute_batch("CREATE TEMP TRIGGER fail_started BEFORE UPDATE OF speech_state ON tutor_segments WHEN NEW.speech_state='started' BEGIN SELECT RAISE(ABORT,'synthetic start receipt failure'); END;")?;
    f.kick().await;
    let mut first = f.call().await?;
    first.start();
    f.wait_for(|s| s["speech_gate"] == "closed").await?;
    f.idle().await?;
    assert!(first.outcome.is_closed());
    assert!(f.calls.try_recv().is_err());
    assert_eq!(f.state()?["speech_complete"], false);
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn failed_terminal_receipt_commit_never_advances_to_the_next_segment() -> Result<()> {
    let mut f = Fixture::new()?;
    f.ack(0)?;
    f.ack(1)?;
    f.app.stores.lock().unwrap()[&f.key.namespace].db.execute_batch("CREATE TEMP TRIGGER fail_completed BEFORE UPDATE OF speech_state ON tutor_segments WHEN NEW.speech_state='completed' BEGIN SELECT RAISE(ABORT,'synthetic terminal receipt failure'); END;")?;
    f.kick().await;
    let mut first = f.call().await?;
    first.start();
    first.finish(SpeechEnd::Completed);
    f.wait_for(|s| s["speech_gate"] == "closed").await?;
    f.idle().await?;
    assert!(f.calls.try_recv().is_err());
    assert!(f.state()?["speech_completed_through"].is_null());
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn provider_failure_closes_the_current_segment_and_already_displayed_tail() -> Result<()> {
    let mut f = Fixture::new()?;
    f.ack(0)?;
    f.ack(1)?;
    f.kick().await;
    let first = f.call().await?;
    {
        let _control = f.app.control.lock().await;
        f.app.stores.lock().unwrap()[&f.key.namespace].fail_tutor_stream(
            &f.key.session_id,
            &f.key.request_id,
            "failed",
            "synthetic_provider_failure",
        )?;
        f.app
            .fail_segment_speech_locked(
                &f.key.namespace,
                &f.key.session_id,
                &f.key.request_id,
                "provider_failed",
            )
            .await;
    }
    assert!(first.outcome.is_closed());
    assert!(f.calls.try_recv().is_err());
    assert_eq!(f.state()?["speech_gate"], "closed");
    assert_eq!(f.phase()?, "failed");
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn a_changed_learner_epoch_prevents_a_late_display_from_starting_speech() -> Result<()> {
    let mut f = Fixture::new()?;
    f.ack(0)?;
    f.app.stores.lock().unwrap()[&f.key.namespace].db.execute(
        "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
        [&f.key.learner_id],
    )?;
    f.kick().await;
    assert!(f.calls.try_recv().is_err());
    assert_eq!(f.state()?["speech_gate"], "closed");
    assert_ne!(f.app.audio.speech_epoch(), f.key.audio_epoch);
    f.app.close().await;
    Ok(())
}

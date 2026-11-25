use super::*;
use crate::kimi::tutor_segments::SegmentDecoder;

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    lid: String,
    sid: String,
    rid: String,
    generation: i64,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("speech.sqlite3"), "real")?;
        let lid = s(
            &store.create_learner("synthetic speech", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        let generation = store.bump(&sid, "generating")?;
        let rid = uid("request");
        store.create_tutor_stream_with_speech(
            &sid,
            &rid,
            generation,
            0,
            Some(7),
            Some("synthetic_nonce"),
        )?;
        Ok(Self {
            dir,
            store,
            lid,
            sid,
            rid,
            generation,
        })
    }
    fn append(&self) -> Result<()> {
        let request = json!({"request_id":self.rid,"context":{"evidence":[]}});
        let mut decoder = SegmentDecoder::new(&request, self.generation as u64)?;
        for (sequence, text) in ["first. ", "second."].iter().enumerate() {
            for segment in decoder.push(format!("{}\n",json!({"schema_version":"1.0","type":"segment","request_id":self.rid,"sequence":sequence,"text":text,"used_evidence_ids":[]})).as_bytes())? {
                self.store.append_tutor_segment(&self.sid,&self.rid,&segment)?;
            }
        }
        Ok(())
    }
    fn shown(&self, sequence: usize) -> Result<()> {
        self.store
            .ack_tutor_segment(&self.sid, &self.rid, self.generation, sequence)
    }
    fn reserve(&self, id: &str) -> Result<Option<TutorSpeechWork>> {
        self.store
            .reserve_next_segment_speech(&self.sid, &self.rid, self.generation, 0, id, 7)
    }
    fn summary(&self) -> Result<Value> {
        self.store.tutor_speech_state(&self.sid, &self.rid)
    }
    fn row(&self, sequence: usize) -> Result<Value> {
        Ok(self
            .store
            .one(
                "SELECT * FROM tutor_segments WHERE request_id=? AND sequence=?",
                params![self.rid, sequence as i64],
            )?
            .unwrap())
    }
    fn finish_text(&self) -> Result<()> {
        self.store.mark_tutor_final(&self.sid, &self.rid)?;
        self.store
            .complete_tutor_stream(&self.sid, &self.rid, &[])?;
        self.store.set_status(&self.sid, "idle")
    }
}
fn started(work: &TutorSpeechWork) -> SpeechStarted {
    SpeechStarted {
        playback_id: format!("playback_{}", work.sequence),
        epoch: work.audio_epoch,
        engine_kind: "qwen_pcm",
        milestone: "first_callback_submission",
    }
}
fn outcome(work: &TutorSpeechWork, state: SpeechEnd) -> SpeechOutcome {
    SpeechOutcome {
        playback_id: started(work).playback_id,
        epoch: work.audio_epoch,
        state,
        engine_kind: "qwen_pcm",
        callback_submitted_frames: 2400,
        predicted_output_drained: state == SpeechEnd::Completed,
        speaker_output_measured: false,
        reason: if state == SpeechEnd::Interrupted {
            Some("synthetic_interrupted".into())
        } else {
            None
        },
    }
}

#[test]
fn stop_before_first_ack_durably_closes_only_speech_and_preserves_nonce() -> Result<()> {
    let f = Fixture::new()?;
    let closed = f
        .store
        .close_tutor_speech(&f.sid, &f.rid, f.generation, "user_stop")?;
    ensure!(closed["speech_gate"] == "closed" && closed["speech_complete"] == false);
    f.append()?;
    f.shown(0)?;
    ensure!(f.reserve("late_ack")?.is_none());
    f.shown(1)?;
    f.finish_text()?;
    let snapshot = f.store.tutor_stream_snapshot(&f.sid, &f.rid)?;
    ensure!(snapshot["phase"] == "completed" && snapshot["speech_gate"] == "closed");
    ensure!(
        snapshot["client_submission_id"] == "synthetic_nonce" && snapshot["shown_through"] == 1
    );
    ensure!(f.row(0)?["speech_started_receipt"].is_null());
    Ok(())
}

#[test]
fn one_reserved_segment_real_receipts_and_exact_duplicates_control_prefix() -> Result<()> {
    for engine_kind in ["qwen_pcm", "cosy_pcm"] {
        let started = |work: &TutorSpeechWork| {
            let mut receipt = started(work);
            receipt.engine_kind = engine_kind;
            receipt
        };
        let outcome = |work: &TutorSpeechWork, state: SpeechEnd| {
            let mut receipt = outcome(work, state);
            receipt.engine_kind = engine_kind;
            receipt
        };
        let f = Fixture::new()?;
        f.append()?;
        ensure!(f.reserve("before_display")?.is_none());
        f.shown(0)?;
        f.shown(1)?;
        f.finish_text()?;
        ensure!(f.summary()?["speech_complete"] == false);
        ensure!(
            f.store
                .reserve_next_segment_speech(&f.sid, &f.rid, f.generation, 0, "wrong_epoch", 8)
                .is_err()
        );
        let first = f.reserve("attempt_0")?.unwrap();
        ensure!(first.sequence == 0 && first.text == "first. " && first.audio_epoch == 7);
        ensure!(f.reserve("duplicate_kick")?.is_none());
        ensure!(
            f.store
                .record_tutor_speech_outcome(&first, &outcome(&first, SpeechEnd::Completed))
                .is_err()
        );
        let receipt = started(&first);
        f.store.record_tutor_speech_started(&first, &receipt)?;
        let revision = f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["revision"].clone();
        f.store.record_tutor_speech_started(&first, &receipt)?;
        ensure!(f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["revision"] == revision);
        let mut wrong = first.clone();
        wrong.attempt_id = "wrong_attempt".into();
        ensure!(
            f.store
                .record_tutor_speech_started(&wrong, &receipt)
                .is_err()
        );
        let mut wrong_outcome = outcome(&first, SpeechEnd::Completed);
        wrong_outcome.playback_id = "other_playback".into();
        ensure!(
            f.store
                .record_tutor_speech_outcome(&first, &wrong_outcome)
                .is_err()
        );
        let final_receipt = outcome(&first, SpeechEnd::Completed);
        for (frames, drained) in [(0, true), (2400, false)] {
            let mut incomplete = outcome(&first, SpeechEnd::Completed);
            incomplete.callback_submitted_frames = frames;
            incomplete.predicted_output_drained = drained;
            ensure!(
                f.store
                    .record_tutor_speech_outcome(&first, &incomplete)
                    .is_err()
            );
        }
        f.store
            .record_tutor_speech_outcome(&first, &final_receipt)?;
        let revision = f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["revision"].clone();
        f.store
            .record_tutor_speech_outcome(&first, &final_receipt)?;
        ensure!(f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["revision"] == revision);
        ensure!(
            f.summary()?["speech_completed_through"] == 0
                && f.summary()?["speech_complete"] == false
        );
        let second = f.reserve("attempt_1")?.unwrap();
        ensure!(second.sequence == 1 && second.audio_epoch == first.audio_epoch);
        f.store
            .record_tutor_speech_started(&second, &started(&second))?;
        f.store
            .record_tutor_speech_outcome(&second, &outcome(&second, SpeechEnd::Completed))?;
        ensure!(
            f.summary()?["speech_complete"] == true
                && f.summary()?["speech_completed_through"] == 1
        );
        ensure!(f.reserve("no_more")?.is_none());
        let mut corrupted = outcome(&second, SpeechEnd::Completed);
        corrupted.predicted_output_drained = false;
        f.store.db.execute(
            "UPDATE tutor_segments SET speech_terminal_receipt=? WHERE request_id=? AND sequence=1",
            params![json!(corrupted).to_string(), f.rid],
        )?;
        ensure!(
            f.summary().is_err(),
            "{engine_kind} must validate restored PCM completion"
        );
    }
    Ok(())
}

#[test]
fn owner_interrupted_before_start_does_not_fabricate_start_or_advance() -> Result<()> {
    let f = Fixture::new()?;
    f.append()?;
    f.shown(0)?;
    f.shown(1)?;
    let work = f.reserve("never_started")?.unwrap();
    let mut result = outcome(&work, SpeechEnd::Interrupted);
    result.engine_kind = "none";
    result.callback_submitted_frames = 0;
    f.store.record_tutor_speech_outcome(&work, &result)?;
    ensure!(
        f.summary()?["speech_gate"] == "closed"
            && f.summary()?["speech_completed_through"].is_null()
    );
    let row = f.row(0)?;
    ensure!(row["speech_state"] == "interrupted" && row["speech_started_receipt"].is_null());
    ensure!(
        row["speech_end_origin"] == "owner_receipt" && row["speech_terminal_receipt"].is_string()
    );
    ensure!(f.reserve("cannot_skip")?.is_none());
    ensure!(
        f.store
            .record_tutor_speech_started(&work, &started(&work))
            .is_err()
    );
    ensure!(
        f.store
            .record_tutor_speech_outcome(&work, &outcome(&work, SpeechEnd::Completed))
            .is_err()
    );
    Ok(())
}

#[test]
fn failed_or_closed_text_keeps_real_delivery_but_no_tail_speech() -> Result<()> {
    let f = Fixture::new()?;
    f.append()?;
    f.shown(0)?;
    f.shown(1)?;
    let first = f.reserve("first")?.unwrap();
    f.store
        .record_tutor_speech_started(&first, &started(&first))?;
    f.store
        .record_tutor_speech_outcome(&first, &outcome(&first, SpeechEnd::Completed))?;
    let second = f.reserve("second")?.unwrap();
    f.store
        .record_tutor_speech_started(&second, &started(&second))?;
    f.store
        .fail_tutor_stream(&f.sid, &f.rid, "failed", "final_text_mismatch")?;
    ensure!(
        f.summary()?["speech_gate"] == "closed" && f.summary()?["speech_completed_through"] == 0
    );
    let row = f.row(1)?;
    ensure!(row["speech_state"] == "interrupted" && row["speech_end_origin"] == "gate_closed");
    ensure!(row["speech_started_receipt"].is_string() && row["speech_terminal_receipt"].is_null());
    ensure!(
        f.store
            .record_tutor_speech_outcome(&second, &outcome(&second, SpeechEnd::Completed))
            .is_err()
    );
    ensure!(f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["shown_through"] == 1);
    Ok(())
}

#[test]
fn completed_text_still_closes_on_epoch_or_generation_and_rejects_old_receipts() -> Result<()> {
    for learner_epoch_change in [false, true] {
        let f = Fixture::new()?;
        f.append()?;
        f.shown(0)?;
        f.shown(1)?;
        f.finish_text()?;
        let work = f.reserve("active")?.unwrap();
        f.store
            .record_tutor_speech_started(&work, &started(&work))?;
        if learner_epoch_change {
            f.store.db.execute(
                "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
                [&f.lid],
            )?;
        } else {
            f.store.bump(&f.sid, "idle")?;
        }
        ensure!(f.summary()?["speech_gate"] == "closed");
        ensure!(f.store.tutor_stream_row(&f.sid, &f.rid)?["phase"] == "completed");
        ensure!(
            f.store
                .record_tutor_speech_outcome(&work, &outcome(&work, SpeechEnd::Completed))
                .is_err()
        );
        ensure!(f.row(0)?["speech_terminal_receipt"].is_null());
    }
    Ok(())
}

#[test]
fn restart_never_replays_reserved_or_started_even_when_text_was_completed() -> Result<()> {
    for actually_started in [false, true] {
        let f = Fixture::new()?;
        f.append()?;
        f.shown(0)?;
        f.shown(1)?;
        f.finish_text()?;
        let first = f.reserve("first")?.unwrap();
        f.store
            .record_tutor_speech_started(&first, &started(&first))?;
        f.store
            .record_tutor_speech_outcome(&first, &outcome(&first, SpeechEnd::Completed))?;
        let second = f.reserve("second")?.unwrap();
        if actually_started {
            f.store
                .record_tutor_speech_started(&second, &started(&second))?;
        }
        let reopened = Store::open(f.dir.path().join("speech.sqlite3"), "real")?;
        let summary = reopened.tutor_speech_state(&f.sid, &f.rid)?;
        ensure!(
            summary["speech_gate"] == "closed"
                && summary["speech_close_reason"] == "process_restarted"
        );
        ensure!(summary["speech_completed_through"] == 0 && summary["speech_complete"] == false);
        let row = reopened
            .one(
                "SELECT * FROM tutor_segments WHERE request_id=? AND sequence=1",
                [&f.rid],
            )?
            .unwrap();
        ensure!(row["speech_end_origin"] == "restart" && row["speech_terminal_receipt"].is_null());
        ensure!(row["speech_started_receipt"].is_string() == actually_started);
        ensure!(
            reopened
                .reserve_next_segment_speech(&f.sid, &f.rid, f.generation, 0, "retry", 7)?
                .is_none()
        );
        ensure!(
            reopened
                .record_tutor_speech_outcome(&second, &outcome(&second, SpeechEnd::Completed))
                .is_err()
        );
    }
    Ok(())
}

#[test]
fn persistence_errors_do_not_create_speech_receipts_or_authorize_the_next_segment() -> Result<()> {
    let f = Fixture::new()?;
    f.append()?;
    f.shown(0)?;
    f.shown(1)?;
    let work = f.reserve("active")?.unwrap();
    f.store.db.execute_batch("CREATE TRIGGER reject_speech_started BEFORE UPDATE OF speech_started_receipt ON tutor_segments WHEN NEW.speech_started_receipt IS NOT NULL BEGIN SELECT RAISE(ABORT,'synthetic start failure'); END;")?;
    ensure!(
        f.store
            .record_tutor_speech_started(&work, &started(&work))
            .is_err()
    );
    ensure!(
        f.row(0)?["speech_state"] == "reserved" && f.row(0)?["speech_started_receipt"].is_null()
    );
    ensure!(f.reserve("cannot_advance")?.is_none());
    f.store
        .close_tutor_speech(&f.sid, &f.rid, f.generation, "receipt_store_failed")?;
    ensure!(
        f.row(0)?["speech_terminal_receipt"].is_null()
            && f.row(0)?["speech_end_origin"] == "gate_closed"
    );
    let terminal = Fixture::new()?;
    terminal.append()?;
    terminal.shown(0)?;
    terminal.shown(1)?;
    let work = terminal.reserve("terminal_failure")?.unwrap();
    terminal
        .store
        .record_tutor_speech_started(&work, &started(&work))?;
    terminal.store.db.execute_batch("CREATE TRIGGER reject_speech_terminal BEFORE UPDATE OF speech_terminal_receipt ON tutor_segments WHEN NEW.speech_terminal_receipt IS NOT NULL BEGIN SELECT RAISE(ABORT,'synthetic terminal failure'); END;")?;
    ensure!(
        terminal
            .store
            .record_tutor_speech_outcome(&work, &outcome(&work, SpeechEnd::Completed))
            .is_err()
    );
    ensure!(
        terminal.row(0)?["speech_state"] == "started"
            && terminal.row(0)?["speech_terminal_receipt"].is_null()
    );
    ensure!(terminal.reserve("cannot_advance")?.is_none());
    terminal.store.close_tutor_speech(
        &terminal.sid,
        &terminal.rid,
        terminal.generation,
        "receipt_store_failed",
    )?;
    ensure!(
        terminal.row(0)?["speech_state"] == "interrupted"
            && terminal.row(0)?["speech_started_receipt"].is_string()
    );
    Ok(())
}

#[test]
fn cancelled_unseen_tail_cannot_make_retained_spoken_prefix_look_complete() -> Result<()> {
    for show_first in [false, true] {
        let f = Fixture::new()?;
        f.append()?;
        if show_first {
            f.shown(0)?;
            let work = f.reserve("prefix")?.unwrap();
            f.store
                .record_tutor_speech_started(&work, &started(&work))?;
            f.store
                .record_tutor_speech_outcome(&work, &outcome(&work, SpeechEnd::Completed))?;
        }
        f.store.mark_tutor_final(&f.sid, &f.rid)?;
        f.store.bump(&f.sid, "idle")?;
        let summary = f.summary()?;
        ensure!(summary["speech_gate"] == "closed" && summary["speech_complete"] == false);
        ensure!(
            summary["speech_completed_through"] == if show_first { json!(0) } else { Value::Null }
        );
    }
    Ok(())
}

#[test]
fn invalid_durable_prefix_is_rejected_and_default_wrapper_remains_disabled() -> Result<()> {
    let f = Fixture::new()?;
    f.append()?;
    f.shown(0)?;
    f.shown(1)?;
    f.store.db.execute(
        "UPDATE tutor_segments SET speech_state='completed' WHERE request_id=? AND sequence=1",
        [&f.rid],
    )?;
    ensure!(f.summary().is_err());
    ensure!(f.reserve("bad_prefix").is_err());
    let other = Fixture::new()?;
    other
        .store
        .close_tutor_speech(&other.sid, &other.rid, other.generation, "superseded")?;
    let rid = uid("text_only");
    other
        .store
        .create_tutor_stream(&other.sid, &rid, other.generation, 0)?;
    let snapshot = other.store.tutor_stream_snapshot(&other.sid, &rid)?;
    ensure!(snapshot["speech_gate"] == "disabled" && snapshot["client_submission_id"].is_null());
    ensure!(
        other
            .store
            .reserve_next_segment_speech(&other.sid, &rid, other.generation, 0, "disabled", 7)?
            .is_none()
    );
    Ok(())
}

use super::*;
use crate::kimi::tutor_segments::SegmentDecoder;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    sid: String,
    lid: String,
    rid: String,
    tid: String,
    generation: i64,
    request: Value,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("stream.sqlite3"), "real")?;
        let learner = store.create_learner("synthetic stream", "zh", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        let generation = store.bump(&sid, "generating")?;
        let rid = uid("request");
        let tid = store.create_tutor_stream(&sid, &rid, generation, 0)?;
        let request = json!({"request_id":rid,"generation":generation,"context":{"evidence":[]}});
        Ok(Self {
            _dir: dir,
            store,
            sid,
            lid,
            rid,
            tid,
            generation,
            request,
        })
    }
    fn append(&self, texts: &[&str]) -> Result<()> {
        let mut decoder = SegmentDecoder::new(&self.request, self.generation as u64)?;
        for (sequence, text) in texts.iter().enumerate() {
            let bytes = format!(
                "{}\n",
                json!({"schema_version":"1.0","type":"segment","request_id":self.rid,"sequence":sequence,"text":text,"used_evidence_ids":[]})
            );
            for segment in decoder.push(bytes.as_bytes())? {
                self.store
                    .append_tutor_segment(&self.sid, &self.rid, &segment)?;
            }
        }
        Ok(())
    }
    fn content(&self) -> Result<String> {
        Ok(s(
            &self
                .store
                .one("SELECT content FROM turns WHERE turn_id=?", [&self.tid])?
                .unwrap(),
            "content",
        )
        .to_owned())
    }
}

#[test]
fn validated_text_is_not_displayed_until_ordered_idempotent_ack() -> Result<()> {
    let f = Fixture::new()?;
    f.append(&["first. ", "second."])?;
    ensure!(f.content()?.is_empty());
    ensure!(f.store.session(&f.sid)?["state"]["last_delivered_hint"] == "");
    ensure!(
        f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["segments"]
            .as_array()
            .unwrap()
            .len()
            == 2
    );
    ensure!(
        f.store
            .ack_tutor_segment(&f.sid, &f.rid, f.generation, 1)
            .is_err()
    );
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
    let revision = f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["revision"].clone();
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
    ensure!(f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["revision"] == revision);
    ensure!(f.content()? == "first. ");
    let session = f.store.session(&f.sid)?;
    ensure!(
        session["turns"][0]["delivery_mode"] == "segments"
            && session["tutor_stream"]["shown_through"] == 0
    );
    ensure!(session["state"]["last_delivered_hint"] == "first. ");
    ensure!(f.store.memories(&f.lid)?.is_empty());
    Ok(())
}

#[test]
fn model_failure_hides_unshown_tail_and_accepts_only_current_late_display_receipt() -> Result<()> {
    let f = Fixture::new()?;
    f.append(&["visible prefix ", "previously painted tail"])?;
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
    f.store
        .fail_tutor_stream(&f.sid, &f.rid, "failed", "final_text_mismatch")?;
    let snapshot = f.store.tutor_stream_snapshot(&f.sid, &f.rid)?;
    ensure!(
        snapshot["segments"].as_array().unwrap().len() == 1 && snapshot["final_validated"] == false
    );
    ensure!(!snapshot.to_string().contains("previously painted tail"));
    ensure!(
        f.store
            .ack_tutor_segment(&f.sid, &f.rid, f.generation + 1, 1)
            .is_err()
    );
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 1)?;
    ensure!(f.content()? == "visible prefix previously painted tail");
    ensure!(!f.store.tutor_stream_ready(&f.sid, &f.rid)?);
    ensure!(f.store.complete_tutor_stream(&f.sid, &f.rid, &[]).is_err());
    ensure!(
        f.store.session(&f.sid)?["state"]["goal"] == "" && f.store.memories(&f.lid)?.is_empty()
    );
    Ok(())
}

#[test]
fn generation_and_learner_epoch_invalidate_stream_without_erasing_shown_prefix() -> Result<()> {
    for epoch in [false, true] {
        let f = Fixture::new()?;
        f.append(&["shown", "never shown"])?;
        f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
        if epoch {
            f.store.db.execute(
                "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
                [&f.lid],
            )?;
        } else {
            f.store.bump(&f.sid, "idle")?;
        }
        ensure!(f.store.tutor_stream_snapshot(&f.sid, &f.rid).is_err());
        ensure!(
            f.store
                .ack_tutor_segment(&f.sid, &f.rid, f.generation, 1)
                .is_err()
        );
        ensure!(f.content()? == "shown");
        ensure!(
            f.store
                .rows("SELECT * FROM tutor_segments WHERE request_id=?", [&f.rid])?
                .len()
                == 1
        );
        ensure!(f.store.session(&f.sid)?["tutor_stream"].is_null());
    }
    Ok(())
}

#[test]
fn restart_closes_pending_final_and_retains_only_acknowledged_prefix() -> Result<()> {
    let f = Fixture::new()?;
    f.append(&["confirmed", "unseen"])?;
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
    f.store.mark_tutor_final(&f.sid, &f.rid)?;
    f.store.set_status(&f.sid, "idle")?;
    let path = f._dir.path().join("stream.sqlite3");
    drop(f.store);
    let reopened = Store::open(path, "real")?;
    let snapshot = reopened.tutor_stream_snapshot(&f.sid, &f.rid)?;
    ensure!(
        snapshot["phase"] == "interrupted" && snapshot["segments"].as_array().unwrap().len() == 1
    );
    ensure!(
        reopened
            .ack_tutor_segment(&f.sid, &f.rid, f.generation, 1)
            .is_err()
    );
    ensure!(reopened.session(&f.sid)?["turns"][0]["content"] == "confirmed");
    Ok(())
}

#[test]
fn forgetting_learner_cascades_stream_text_and_final_state() -> Result<()> {
    let f = Fixture::new()?;
    f.append(&["private synthetic body"])?;
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
    f.store.forget(&f.lid, None)?;
    ensure!(f.store.rows("SELECT * FROM tutor_streams", [])?.is_empty());
    ensure!(f.store.rows("SELECT * FROM tutor_segments", [])?.is_empty());
    ensure!(
        f.store.session(&f.sid)?["turns"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    Ok(())
}

#[test]
fn final_completion_is_atomic_and_survives_its_own_accepted_epoch_change() -> Result<()> {
    let f = Fixture::new()?;
    f.append(&["shown"])?;
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
    f.store.mark_tutor_final(&f.sid, &f.rid)?;
    let failed: Result<()> = f.store.tx(|| {
        f.store.complete_tutor_stream(&f.sid, &f.rid, &[])?;
        Err(anyhow!("synthetic transaction failure"))
    });
    ensure!(
        failed.is_err()
            && f.store.tutor_stream_snapshot(&f.sid, &f.rid)?["phase"] == "final_pending"
    );
    f.store.tx(|| {
        f.store.complete_tutor_stream(&f.sid, &f.rid, &[])?;
        f.store.db.execute(
            "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
            [&f.lid],
        )?;
        f.store.bump(&f.sid, "idle")?;
        Ok(())
    })?;
    let turn = &f.store.session(&f.sid)?["turns"][0];
    ensure!(turn["stream_phase"] == "completed" && turn["content"] == "shown");
    Ok(())
}

#[test]
fn history_and_other_session_cannot_lose_stream_phase_or_cross_owner() -> Result<()> {
    let f = Fixture::new()?;
    f.append(&["partial"])?;
    f.store.ack_tutor_segment(&f.sid, &f.rid, f.generation, 0)?;
    f.store
        .fail_tutor_stream(&f.sid, &f.rid, "failed", "invalid_json")?;
    let page = f.store.turn_history_page(&f.lid, &f.sid, None, 20)?;
    ensure!(
        page["items"][0]["delivery_mode"] == "segments"
            && page["items"][0]["stream_phase"] == "failed"
    );
    let other = s(&f.store.create_session(&f.lid)?, "session_id").to_owned();
    ensure!(f.store.tutor_stream_snapshot(&other, &f.rid).is_err());
    ensure!(
        f.store
            .ack_tutor_segment(&other, &f.rid, f.generation, 0)
            .is_err()
    );
    Ok(())
}

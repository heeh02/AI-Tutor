//! Bounded, durable segment delivery. Validation, display and final candidate
//! application are separate facts; this module never applies model candidates.
use super::*;
use crate::kimi::tutor_segments::{MAX_REPLY_CHARS, MAX_SEGMENTS, TutorSegment};

impl Store {
    pub(crate) fn is_tutor_stream_turn(&self, tid: &str) -> Result<bool> {
        Ok(self
            .one("SELECT turn_id FROM tutor_streams WHERE turn_id=?", [tid])?
            .is_some())
    }
    pub(crate) fn delivered_tutor_stream_phase(
        &self,
        lid: &str,
        tid: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .one(
                "SELECT phase FROM tutor_streams WHERE learner_id=? AND turn_id=?",
                params![lid, tid],
            )?
            .map(|row| s(&row, "phase").to_owned()))
    }
    pub(super) fn migrate_tutor_delivery(&self) -> Result<()> {
        self.db.execute_batch("
        CREATE TABLE IF NOT EXISTS tutor_streams(
          request_id TEXT PRIMARY KEY,turn_id TEXT UNIQUE NOT NULL,session_id TEXT NOT NULL,
          learner_id TEXT NOT NULL,generation INTEGER NOT NULL,learner_epoch INTEGER NOT NULL,
          phase TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,
          final_validated INTEGER NOT NULL DEFAULT 0,shown_through INTEGER NOT NULL DEFAULT -1,
          error_code TEXT,created_at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS tutor_segments(
          request_id TEXT NOT NULL,sequence INTEGER NOT NULL,text TEXT NOT NULL,evidence TEXT NOT NULL,
          shown INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(request_id,sequence));
        CREATE INDEX IF NOT EXISTS tutor_stream_session ON tutor_streams(session_id,generation);
        CREATE TRIGGER IF NOT EXISTS tutor_delivery_turn_delete AFTER DELETE ON turns BEGIN
          DELETE FROM tutor_segments WHERE request_id IN (SELECT request_id FROM tutor_streams WHERE turn_id=OLD.turn_id);
          DELETE FROM tutor_streams WHERE turn_id=OLD.turn_id;
        END;
        CREATE TRIGGER IF NOT EXISTS tutor_delivery_generation AFTER UPDATE OF generation ON sessions
        WHEN NEW.generation != OLD.generation BEGIN
          UPDATE tutor_streams SET phase='cancelled',revision=revision+1,error_code='generation_changed'
            WHERE session_id=NEW.session_id AND generation!=NEW.generation AND phase IN('generating','final_pending');
          DELETE FROM tutor_segments WHERE shown=0 AND request_id IN
            (SELECT request_id FROM tutor_streams WHERE session_id=NEW.session_id AND generation!=NEW.generation AND phase!='completed');
          UPDATE turns SET status=CASE WHEN content='' THEN 'cancelled' ELSE 'shown' END
            WHERE turn_id IN(SELECT turn_id FROM tutor_streams WHERE session_id=NEW.session_id AND generation!=NEW.generation AND phase!='completed');
        END;
        CREATE TRIGGER IF NOT EXISTS tutor_delivery_epoch AFTER UPDATE OF epoch ON learners
        WHEN NEW.epoch != OLD.epoch BEGIN
          UPDATE tutor_streams SET phase='cancelled',revision=revision+1,error_code='learner_epoch_changed'
            WHERE learner_id=NEW.learner_id AND learner_epoch!=NEW.epoch AND phase IN('generating','final_pending');
          DELETE FROM tutor_segments WHERE shown=0 AND request_id IN
            (SELECT request_id FROM tutor_streams WHERE learner_id=NEW.learner_id AND learner_epoch!=NEW.epoch AND phase!='completed');
          UPDATE turns SET status=CASE WHEN content='' THEN 'cancelled' ELSE 'shown' END
            WHERE turn_id IN(SELECT turn_id FROM tutor_streams WHERE learner_id=NEW.learner_id AND learner_epoch!=NEW.epoch AND phase!='completed');
        END;
        UPDATE tutor_streams SET phase='interrupted',revision=revision+1,error_code='process_restarted'
          WHERE phase IN('generating','final_pending');
        DELETE FROM tutor_segments WHERE shown=0 AND request_id IN(SELECT request_id FROM tutor_streams WHERE phase='interrupted');
        UPDATE turns SET status=CASE WHEN content='' THEN 'cancelled' ELSE 'shown' END
          WHERE turn_id IN(SELECT turn_id FROM tutor_streams WHERE phase='interrupted');
        ")?;
        self.migrate_tutor_speech()?;
        Ok(())
    }
    pub(super) fn tutor_stream_row(&self, sid: &str, rid: &str) -> Result<Value> {
        self.one(
            "SELECT * FROM tutor_streams WHERE session_id=? AND request_id=?",
            params![sid, rid],
        )?
        .ok_or_else(|| anyhow!("教学流不存在"))
    }
    pub(super) fn current_tutor_stream(&self, sid: &str, rid: &str) -> Result<Value> {
        let stream = self.tutor_stream_row(sid, rid)?;
        let current=self.one("SELECT s.generation,s.status,s.learner_id,l.epoch FROM sessions s JOIN learners l ON l.learner_id=s.learner_id WHERE s.session_id=?",[sid])?
            .ok_or_else(|| anyhow!("教学流会话不存在"))?;
        ensure!(
            current["generation"] == stream["generation"]
                && current["epoch"] == stream["learner_epoch"]
                && current["learner_id"] == stream["learner_id"]
                && current["status"] != "finished",
            "教学流已过期"
        );
        Ok(stream)
    }
    #[cfg(test)]
    pub(crate) fn create_tutor_stream(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        epoch: i64,
    ) -> Result<String> {
        self.create_tutor_stream_with_speech(sid, rid, generation, epoch, None, None)
    }
    pub(crate) fn create_tutor_stream_with_speech(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        epoch: i64,
        speech_gate_audio_epoch: Option<u64>,
        client_submission_id: Option<&str>,
    ) -> Result<String> {
        ensure!(
            client_submission_id.is_none_or(|id| !id.is_empty() && id.len() <= 96 && id.is_ascii()),
            "提交关联标识无效"
        );
        let speech_epoch = speech_gate_audio_epoch
            .map(|value| {
                ensure!(value > 0, "播报代次无效");
                i64::try_from(value).map_err(|_| anyhow!("播报代次超限"))
            })
            .transpose()?;
        self.tx(|| {
            let session=self.session(sid)?;
            let lid=s(&session,"learner_id");
            ensure!(session["generation"] == generation && self.learner(lid)?["epoch"] == epoch && session["status"] != "finished", "教学流已过期");
            let tid=self.add_turn(sid,rid,generation,"assistant","","streaming",&[])?;
            self.db.execute("INSERT INTO tutor_streams(request_id,turn_id,session_id,learner_id,generation,learner_epoch,phase,created_at,speech_gate,speech_gate_audio_epoch,client_submission_id) VALUES(?,?,?,?,?,?,'generating',?,?,?,?)",params![rid,tid,sid,lid,generation,epoch,now(),if speech_epoch.is_some(){"open"}else{"disabled"},speech_epoch,client_submission_id])?;
            Ok(tid)
        })
    }
    pub(crate) fn append_tutor_segment(
        &self,
        sid: &str,
        rid: &str,
        segment: &TutorSegment,
    ) -> Result<()> {
        self.tx(|| {
            let stream = self.current_tutor_stream(sid, rid)?;
            ensure!(
                stream["phase"] == "generating"
                    && segment.binding().request_id == rid
                    && Some(segment.binding().generation) == stream["generation"].as_u64(),
                "教学片段绑定已失效"
            );
            let existing = self.rows(
                "SELECT sequence,text FROM tutor_segments WHERE request_id=? ORDER BY sequence",
                [rid],
            )?;
            ensure!(
                segment.sequence() == existing.len() && existing.len() < MAX_SEGMENTS,
                "教学片段编号不连续"
            );
            let chars: usize = existing
                .iter()
                .map(|row| s(row, "text").chars().count())
                .sum();
            ensure!(
                chars + segment.text().chars().count() <= MAX_REPLY_CHARS,
                "教学片段总长度超限"
            );
            self.db.execute(
                "INSERT INTO tutor_segments(request_id,sequence,text,evidence) VALUES(?,?,?,?)",
                params![
                    rid,
                    segment.sequence() as i64,
                    segment.text(),
                    json!(segment.used_evidence_ids()).to_string()
                ],
            )?;
            self.db.execute(
                "UPDATE tutor_streams SET revision=revision+1 WHERE request_id=?",
                [rid],
            )?;
            Ok(())
        })
    }
    pub(crate) fn tutor_stream_snapshot(&self, sid: &str, rid: &str) -> Result<Value> {
        let stream = self.current_tutor_stream(sid, rid)?;
        self.tutor_snapshot_from(&stream, true)
    }
    fn tutor_snapshot_from(&self, stream: &Value, with_segments: bool) -> Result<Value> {
        let shown = stream["shown_through"].as_i64().unwrap_or(-1);
        let mut head = json!({"protocol_version":1,"session_id":stream["session_id"],"request_id":stream["request_id"],
            "generation":stream["generation"],"turn_id":stream["turn_id"],"revision":stream["revision"],"phase":stream["phase"],
            "final_validated":stream["final_validated"] == 1,"shown_through":if shown<0{Value::Null}else{json!(shown)},"error_code":stream["error_code"]});
        self.annotate_tutor_speech(stream, &mut head)?;
        if with_segments {
            let terminal = ["failed", "cancelled", "interrupted"].contains(&s(stream, "phase"));
            let rows=self.rows("SELECT sequence,text,shown FROM tutor_segments WHERE request_id=? ORDER BY sequence",[s(stream,"request_id")])?;
            let segments: Vec<_> = rows
                .into_iter()
                .filter(|row| !terminal || row["shown"] == 1)
                .map(|row| json!({"sequence":row["sequence"],"text":row["text"]}))
                .collect();
            head["segments"] = json!(segments);
        }
        Ok(head)
    }
    pub(crate) fn ack_tutor_segment(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        sequence: usize,
    ) -> Result<()> {
        self.tx(|| {
            let stream=self.current_tutor_stream(sid,rid)?;
            ensure!(stream["generation"] == generation && sequence<MAX_SEGMENTS,"教学显示回执代次或编号无效");
            ensure!(["generating","final_pending","completed","failed"].contains(&s(&stream,"phase")),"教学流已终止");
            let shown=stream["shown_through"].as_i64().unwrap_or(-1);
            if sequence as i64 <= shown { return Ok(()); }
            ensure!(sequence as i64 == shown+1,"教学显示回执不连续");
            ensure!(self.db.execute("UPDATE tutor_segments SET shown=1 WHERE request_id=? AND sequence=? AND shown=0",params![rid,sequence as i64])? == 1,"教学显示回执没有已校验片段");
            let rows=self.rows("SELECT text,evidence FROM tutor_segments WHERE request_id=? AND shown=1 ORDER BY sequence",[rid])?;
            let prefix:String=rows.iter().map(|row|s(row,"text")).collect();
            let mut ids=vec![];
            for row in &rows { for id in strings(&serde_json::from_str(s(row,"evidence"))?) { if !ids.contains(&id) { ids.push(id); } } }
            self.db.execute("UPDATE turns SET content=?,status='shown',evidence=? WHERE turn_id=?",params![prefix,json!(ids).to_string(),s(&stream,"turn_id")])?;
            self.db.execute("UPDATE tutor_streams SET shown_through=?,revision=revision+1 WHERE request_id=?",params![sequence as i64,rid])?;
            let mut state:Value=serde_json::from_str(s(&self.one("SELECT state FROM sessions WHERE session_id=?",[sid])?.unwrap(),"state"))?;
            state["last_delivered_hint"]=json!(prefix);
            state["last_delivered_turn_id"]=stream["turn_id"].clone();
            self.checkpoint(sid,&state)?;
            Ok(())
        })
    }
    pub(crate) fn mark_tutor_final(&self, sid: &str, rid: &str) -> Result<()> {
        let stream = self.current_tutor_stream(sid, rid)?;
        ensure!(stream["phase"] == "generating", "教学流不能确认final");
        self.db.execute("UPDATE tutor_streams SET phase='final_pending',final_validated=1,revision=revision+1 WHERE request_id=?",[rid])?;
        Ok(())
    }
    pub(crate) fn tutor_stream_ready(&self, sid: &str, rid: &str) -> Result<bool> {
        let stream = self.current_tutor_stream(sid, rid)?;
        let count = self
            .one(
                "SELECT COUNT(*) AS n FROM tutor_segments WHERE request_id=?",
                [rid],
            )?
            .unwrap()["n"]
            .as_i64()
            .unwrap_or(0);
        Ok(stream["phase"] == "final_pending"
            && stream["final_validated"] == 1
            && count > 0
            && stream["shown_through"].as_i64() == Some(count - 1))
    }
    pub(crate) fn complete_tutor_stream(
        &self,
        sid: &str,
        rid: &str,
        final_evidence: &[String],
    ) -> Result<()> {
        self.tx(|| {
            ensure!(self.tutor_stream_ready(sid,rid)?,"教学流尚未全部显示或final未校验");
            ensure!(final_evidence.len()<=20,"完整回复来源超限");
            // Every character is already shown. Only now promote provenance
            // from the displayed segment union to the validated whole reply.
            self.db.execute("UPDATE turns SET evidence=? WHERE turn_id=(SELECT turn_id FROM tutor_streams WHERE request_id=?)",params![json!(final_evidence).to_string(),rid])?;
            self.db.execute("UPDATE tutor_streams SET phase='completed',revision=revision+1 WHERE request_id=?",[rid])?;
            Ok(())
        })
    }
    pub(crate) fn fail_tutor_stream(
        &self,
        sid: &str,
        rid: &str,
        phase: &str,
        error_code: &str,
    ) -> Result<()> {
        ensure!(
            ["failed", "cancelled", "interrupted"].contains(&phase)
                && error_code.len() <= 80
                && error_code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "教学流终态无效"
        );
        self.tx(|| {
            let stream=self.tutor_stream_row(sid,rid)?;
            if ["completed","cancelled","interrupted"].contains(&s(&stream,"phase")) { return Ok(()); }
            self.db.execute("UPDATE tutor_streams SET phase=?,error_code=?,revision=revision+1 WHERE request_id=?",params![phase,error_code,rid])?;
            self.db.execute("UPDATE turns SET status=CASE WHEN content='' THEN 'cancelled' ELSE 'shown' END WHERE turn_id=?",[s(&stream,"turn_id")])?;
            Ok(())
        })
    }
    pub(super) fn annotate_tutor_delivery(&self, session: &mut Value) -> Result<()> {
        let sid = s(session, "session_id").to_owned();
        for turn in session["turns"].as_array_mut().into_iter().flatten() {
            if let Some(stream) = self.one(
                "SELECT * FROM tutor_streams WHERE turn_id=?",
                [s(turn, "turn_id")],
            )? {
                turn["delivery_mode"] = json!("segments");
                turn["stream_phase"] = stream["phase"].clone();
                turn["stream_final_validated"] = json!(stream["final_validated"] == 1);
            }
        }
        let head=self.one("SELECT t.* FROM tutor_streams t JOIN learners l ON l.learner_id=t.learner_id WHERE t.session_id=? AND t.generation=? AND t.learner_epoch=l.epoch ORDER BY t.rowid DESC LIMIT 1",params![sid,session["generation"].as_i64().unwrap_or(-1)])?;
        session["tutor_stream"] = match head {
            Some(stream) => self.tutor_snapshot_from(&stream, false)?,
            None => Value::Null,
        };
        Ok(())
    }
}

#[cfg(test)]
#[path = "tutor_delivery_tests.rs"]
mod tests;

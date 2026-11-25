//! Durable per-segment automatic speech. The audio epoch is fixed for the whole
//! stream; a late display ACK cannot renew an epoch invalidated by Stop.
use super::*;
use crate::audio::{SpeechEnd, SpeechOutcome, SpeechStarted};
use crate::kimi::tutor_segments::{
    DeliveryBinding, DeliveryLedger, DeliveryReceipt, MAX_SEGMENTS, ReceiptKind, SegmentDelivery,
    SpeechGate, SpeechState,
};

#[derive(Clone, Debug)]
pub struct TutorSpeechWork {
    pub namespace: String,
    pub learner_id: String,
    pub session_id: String,
    pub request_id: String,
    pub generation: i64,
    pub learner_epoch: i64,
    pub sequence: usize,
    pub attempt_id: String,
    pub audio_epoch: u64,
    pub text: String,
    pub language: String,
    pub speech_speed: String,
}

fn gate(value: &Value) -> Result<SpeechGate> {
    match s(value, "speech_gate") {
        "disabled" => Ok(SpeechGate::Disabled),
        "open" => Ok(SpeechGate::Open),
        "closed" => Ok(SpeechGate::Closed),
        _ => Err(anyhow!("持久播报开关状态无效")),
    }
}

fn speech_state(value: &Value) -> Result<SpeechState> {
    match s(value, "speech_state") {
        "not_started" => Ok(SpeechState::NotStarted),
        "reserved" => Ok(SpeechState::Reserved),
        "started" => Ok(SpeechState::Started),
        "completed" => Ok(SpeechState::Completed),
        "interrupted" => Ok(SpeechState::Interrupted),
        _ => Err(anyhow!("持久播报片段状态无效")),
    }
}

fn safe_reason(reason: &str) -> Result<()> {
    ensure!(
        !reason.is_empty()
            && reason.len() <= 80
            && reason
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "播报关闭原因无效"
    );
    Ok(())
}

fn positive_epoch(epoch: u64) -> Result<i64> {
    ensure!(epoch > 0, "播报代次无效");
    i64::try_from(epoch).map_err(|_| anyhow!("播报代次超限"))
}

fn persisted_receipt(row: &Value, key: &str) -> Result<Option<Value>> {
    row[key]
        .as_str()
        .map(|raw| {
            ensure!(raw.len() <= 2048, "持久音频回执超限");
            Ok(serde_json::from_str(raw)?)
        })
        .transpose()
}

fn validate_persisted_speech(row: &Value, stream: &Value) -> Result<()> {
    let state = speech_state(row)?;
    let start = persisted_receipt(row, "speech_started_receipt")?;
    let end = persisted_receipt(row, "speech_terminal_receipt")?;
    if state == SpeechState::NotStarted {
        ensure!(
            row["speech_attempt_id"].is_null()
                && row["speech_audio_epoch"].is_null()
                && start.is_none()
                && end.is_none(),
            "未开始片段包含播报回执"
        );
        return Ok(());
    }
    ensure!(
        !s(row, "speech_attempt_id").is_empty()
            && row["speech_audio_epoch"]
                .as_u64()
                .is_some_and(|epoch| epoch > 0)
            && row["speech_audio_epoch"] == stream["speech_gate_audio_epoch"],
        "持久播报尝试绑定无效"
    );
    if let Some(start) = &start {
        ensure!(
            !s(start, "playback_id").is_empty()
                && start["epoch"] == row["speech_audio_epoch"]
                && matches!(
                    (s(start, "engine_kind"), s(start, "milestone")),
                    ("qwen_pcm" | "cosy_pcm", "first_callback_submission")
                        | ("sapi", "sapi_speak_accepted")
                ),
            "持久开始回执无效"
        );
    }
    if let Some(end) = &end {
        ensure!(
            !s(end, "playback_id").is_empty()
                && end["epoch"] == row["speech_audio_epoch"]
                && end["speaker_output_measured"] == false
                && ["qwen_pcm", "cosy_pcm", "sapi", "none"].contains(&s(end, "engine_kind")),
            "持久终态回执无效"
        );
        if let Some(start) = &start {
            ensure!(
                start["playback_id"] == end["playback_id"]
                    && start["engine_kind"] == end["engine_kind"],
                "持久音频回执不是同次播放"
            );
        }
    }
    match state {
        SpeechState::NotStarted => unreachable!(),
        SpeechState::Reserved => ensure!(
            start.is_none() && end.is_none(),
            "预约状态不能冒充已开始或结束"
        ),
        SpeechState::Started => ensure!(
            start.is_some() && end.is_none(),
            "正在播报缺少真实开始或已有终态"
        ),
        SpeechState::Completed => {
            let end = end
                .as_ref()
                .ok_or_else(|| anyhow!("完成片段缺少真实终态"))?;
            ensure!(
                start.is_some()
                    && end["state"] == "completed"
                    && row["speech_end_origin"] == "owner_receipt",
                "完成片段缺少真实开始或owner终态"
            );
            ensure!(
                !["qwen_pcm", "cosy_pcm"].contains(&s(end, "engine_kind"))
                    || (end["predicted_output_drained"] == true
                        && end["callback_submitted_frames"]
                            .as_u64()
                            .is_some_and(|n| n > 0)),
                "PCM完成缺少预计排空证据"
            );
        }
        SpeechState::Interrupted => match s(row, "speech_end_origin") {
            "owner_receipt" => ensure!(
                end.as_ref()
                    .is_some_and(|end| end["state"] == "interrupted"),
                "中断缺少owner回执"
            ),
            "gate_closed" | "restart" => ensure!(
                gate(stream)? == SpeechGate::Closed && end.is_none(),
                "关闭不能伪造音频终态"
            ),
            _ => return Err(anyhow!("中断来源无效")),
        },
    }
    Ok(())
}

impl Store {
    /// Run before legacy startup recovery can bump session generations, so a
    /// crash is recorded as a restart rather than as a fabricated owner receipt.
    pub(super) fn interrupt_tutor_speech_on_open(&self) -> Result<()> {
        if self
            .one(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='tutor_streams'",
                [],
            )?
            .is_some()
            && self
                .rows("PRAGMA table_info(tutor_streams)", [])?
                .iter()
                .any(|row| s(row, "name") == "speech_gate")
        {
            self.db.execute("UPDATE tutor_streams SET speech_gate='closed',speech_close_reason='process_restarted',revision=revision+1 WHERE speech_gate='open'",[])?;
        }
        Ok(())
    }

    pub(super) fn migrate_tutor_speech(&self) -> Result<()> {
        // Additive migration: old streams remain disabled, never auto-enrolled.
        for (table, column, declaration) in [
            (
                "tutor_streams",
                "speech_gate",
                "TEXT NOT NULL DEFAULT 'disabled'",
            ),
            ("tutor_streams", "speech_close_reason", "TEXT"),
            ("tutor_streams", "speech_gate_audio_epoch", "INTEGER"),
            ("tutor_streams", "client_submission_id", "TEXT"),
            (
                "tutor_segments",
                "speech_state",
                "TEXT NOT NULL DEFAULT 'not_started'",
            ),
            ("tutor_segments", "speech_attempt_id", "TEXT"),
            ("tutor_segments", "speech_audio_epoch", "INTEGER"),
            ("tutor_segments", "speech_started_receipt", "TEXT"),
            ("tutor_segments", "speech_terminal_receipt", "TEXT"),
            ("tutor_segments", "speech_end_origin", "TEXT"),
            ("tutor_segments", "speech_end_reason", "TEXT"),
        ] {
            if !self
                .rows(&format!("PRAGMA table_info({table})"), [])?
                .iter()
                .any(|row| s(row, "name") == column)
            {
                self.db.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
                ))?;
            }
        }
        self.db.execute_batch("
            CREATE TRIGGER IF NOT EXISTS tutor_speech_gate_closed AFTER UPDATE OF speech_gate ON tutor_streams
            WHEN OLD.speech_gate='open' AND NEW.speech_gate='closed' BEGIN
                UPDATE tutor_segments SET speech_state='interrupted',
                    speech_end_origin=CASE WHEN NEW.speech_close_reason='process_restarted' THEN 'restart' ELSE 'gate_closed' END,
                    speech_end_reason=NEW.speech_close_reason
                WHERE request_id=NEW.request_id AND speech_state IN('reserved','started');
            END;
            CREATE TRIGGER IF NOT EXISTS tutor_speech_generation AFTER UPDATE OF generation ON sessions
            WHEN NEW.generation!=OLD.generation BEGIN
                UPDATE tutor_streams SET speech_gate='closed',speech_close_reason='generation_changed',revision=revision+1
                WHERE session_id=NEW.session_id AND generation!=NEW.generation AND speech_gate='open';
            END;
            CREATE TRIGGER IF NOT EXISTS tutor_speech_epoch AFTER UPDATE OF epoch ON learners
            WHEN NEW.epoch!=OLD.epoch BEGIN
                UPDATE tutor_streams SET speech_gate='closed',speech_close_reason='learner_epoch_changed',revision=revision+1
                WHERE learner_id=NEW.learner_id AND learner_epoch!=NEW.epoch AND speech_gate='open';
            END;
            CREATE TRIGGER IF NOT EXISTS tutor_speech_text_failed AFTER UPDATE OF phase ON tutor_streams
            WHEN NEW.phase IN('failed','cancelled','interrupted') BEGIN
                UPDATE tutor_streams SET speech_gate='closed',speech_close_reason=COALESCE(NEW.error_code,'text_failed'),revision=revision+1
                WHERE request_id=NEW.request_id AND speech_gate='open';
            END;
            UPDATE tutor_streams SET speech_gate='closed',speech_close_reason='process_restarted',revision=revision+1
                WHERE speech_gate='open';
        ")?;
        Ok(())
    }

    fn speech_ledger(&self, stream: &Value) -> Result<DeliveryLedger> {
        let rows = self.rows(
            "SELECT * FROM tutor_segments WHERE request_id=? ORDER BY sequence",
            [s(stream, "request_id")],
        )?;
        let segments = rows
            .iter()
            .map(|row| {
                validate_persisted_speech(row, stream)?;
                Ok(SegmentDelivery {
                    sequence: row["sequence"]
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| anyhow!("持久片段编号无效"))?,
                    validated: true,
                    displayed: row["shown"] == 1,
                    speech: speech_state(row)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        // Cancelled/failed text can have its unseen tail removed. That retained
        // spoken prefix must never be mistaken for the full validated reply.
        let complete_text = stream["final_validated"] == 1
            && ["final_pending", "completed"].contains(&s(stream, "phase"));
        DeliveryLedger::restore(
            DeliveryBinding {
                request_id: s(stream, "request_id").to_owned(),
                generation: stream["generation"]
                    .as_u64()
                    .ok_or_else(|| anyhow!("教学代次无效"))?,
            },
            segments,
            complete_text,
            gate(stream)?,
        )
    }

    /// Internal precise-stream lookup intentionally survives a generation bump
    /// so a final correction response can report its newly closed speech gate.
    pub(crate) fn tutor_speech_state(&self, sid: &str, rid: &str) -> Result<Value> {
        let stream = self.tutor_stream_row(sid, rid)?;
        let ledger = self.speech_ledger(&stream)?;
        let completed = ledger
            .segments()
            .iter()
            .take_while(|segment| segment.speech == SpeechState::Completed)
            .count();
        Ok(
            json!({"ok":true,"request_id":rid,"generation":stream["generation"],"revision":stream["revision"],
            "speech_gate":stream["speech_gate"],"speech_close_reason":stream["speech_close_reason"],
            "speech_gate_audio_epoch":stream["speech_gate_audio_epoch"],
            "speech_completed_through":if completed == 0 {Value::Null} else {json!(completed-1)},
            "speech_complete":ledger.speech_complete()}),
        )
    }

    pub(super) fn annotate_tutor_speech(&self, stream: &Value, snapshot: &mut Value) -> Result<()> {
        let speech = self.tutor_speech_state(s(stream, "session_id"), s(stream, "request_id"))?;
        for key in [
            "speech_gate",
            "speech_close_reason",
            "speech_gate_audio_epoch",
            "speech_completed_through",
            "speech_complete",
        ] {
            snapshot[key] = speech[key].clone();
        }
        snapshot["client_submission_id"] = stream["client_submission_id"].clone();
        Ok(())
    }

    pub(crate) fn reserve_next_segment_speech(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        learner_epoch: i64,
        attempt_id: &str,
        audio_epoch: u64,
    ) -> Result<Option<TutorSpeechWork>> {
        ensure!(
            !attempt_id.is_empty() && attempt_id.len() <= 96 && attempt_id.is_ascii(),
            "播报尝试标识无效"
        );
        let epoch = positive_epoch(audio_epoch)?;
        self.tx(|| {
            let stream = self.current_tutor_stream(sid,rid)?;
            ensure!(stream["generation"] == generation && stream["learner_epoch"] == learner_epoch,"播报请求绑定已过期");
            if gate(&stream)? != SpeechGate::Open { return Ok(None); }
            ensure!(stream["speech_gate_audio_epoch"] == epoch,"播报开关代次不符");
            ensure!(["generating","final_pending","completed"].contains(&s(&stream,"phase")),"文字流不再允许播报");
            let mut ledger = self.speech_ledger(&stream)?;
            let Some(sequence) = ledger.segments().iter().position(|segment|segment.speech != SpeechState::Completed) else { return Ok(None); };
            if !ledger.segments()[sequence].displayed || ledger.segments()[sequence].speech != SpeechState::NotStarted { return Ok(None); }
            ledger.reserve_speech(sequence)?;
            ensure!(self.one("SELECT sequence FROM tutor_segments WHERE request_id=? AND speech_attempt_id=?",params![rid,attempt_id])?.is_none(),"播报尝试标识不能重复预约");
            ensure!(self.db.execute("UPDATE tutor_segments SET speech_state='reserved',speech_attempt_id=?,speech_audio_epoch=? WHERE request_id=? AND sequence=? AND speech_state='not_started'",params![attempt_id,epoch,rid,sequence as i64])? == 1,"播报片段已经被预约");
            self.db.execute("UPDATE tutor_streams SET revision=revision+1 WHERE request_id=?",[rid])?;
            let segment = self.one("SELECT text FROM tutor_segments WHERE request_id=? AND sequence=?",params![rid,sequence as i64])?.unwrap();
            let learner = self.learner(s(&stream,"learner_id"))?;
            Ok(Some(TutorSpeechWork {
                namespace:self.namespace.clone(), learner_id:s(&stream,"learner_id").to_owned(),session_id:sid.to_owned(),request_id:rid.to_owned(),
                generation,learner_epoch,sequence,attempt_id:attempt_id.to_owned(),audio_epoch,text:s(&segment,"text").to_owned(),
                language:s(&learner,"language").to_owned(),speech_speed:s(&learner,"speech_speed").to_owned(),
            }))
        })
    }

    fn bound_speech_row(&self, work: &TutorSpeechWork) -> Result<(Value, Value)> {
        ensure!(
            work.namespace == self.namespace && work.sequence < MAX_SEGMENTS,
            "播报回执空间或编号无效"
        );
        let stream = self.current_tutor_stream(&work.session_id, &work.request_id)?;
        ensure!(
            stream["generation"] == work.generation
                && stream["learner_epoch"] == work.learner_epoch
                && stream["learner_id"] == work.learner_id
                && stream["speech_gate_audio_epoch"].as_u64() == Some(work.audio_epoch),
            "播报回执绑定已失效"
        );
        let row = self
            .one(
                "SELECT * FROM tutor_segments WHERE request_id=? AND sequence=?",
                params![work.request_id, work.sequence as i64],
            )?
            .ok_or_else(|| anyhow!("播报回执没有片段"))?;
        ensure!(
            row["shown"] == 1
                && row["speech_attempt_id"] == work.attempt_id
                && row["speech_audio_epoch"].as_u64() == Some(work.audio_epoch),
            "播报回执尝试已过期"
        );
        Ok((stream, row))
    }

    pub(crate) fn record_tutor_speech_started(
        &self,
        work: &TutorSpeechWork,
        receipt: &SpeechStarted,
    ) -> Result<()> {
        ensure!(
            receipt.epoch == work.audio_epoch
                && !receipt.playback_id.is_empty()
                && receipt.playback_id.len() <= 96,
            "音频开始回执身份无效"
        );
        ensure!(
            matches!(
                (receipt.engine_kind, receipt.milestone),
                ("qwen_pcm" | "cosy_pcm", "first_callback_submission")
                    | ("sapi", "sapi_speak_accepted")
            ),
            "音频开始里程碑无效"
        );
        self.tx(|| {
            let (stream,row) = self.bound_speech_row(work)?;
            let value = json!(receipt);
            if let Some(previous) = row["speech_started_receipt"].as_str() {
                ensure!(serde_json::from_str::<Value>(previous)? == value,"重复开始回执不一致");
                return Ok(());
            }
            ensure!(gate(&stream)? == SpeechGate::Open && row["speech_state"] == "reserved","播报开始回执已关闭或尚未预约");
            let mut ledger = self.speech_ledger(&stream)?;
            ledger.receipt(DeliveryReceipt { binding:DeliveryBinding {request_id:work.request_id.clone(),generation:work.generation as u64},sequence:work.sequence,kind:ReceiptKind::SpeechStarted })?;
            self.db.execute("UPDATE tutor_segments SET speech_state='started',speech_started_receipt=? WHERE request_id=? AND sequence=?",params![value.to_string(),work.request_id,work.sequence as i64])?;
            self.db.execute("UPDATE tutor_streams SET revision=revision+1 WHERE request_id=?",[&work.request_id])?;
            Ok(())
        })
    }

    pub(crate) fn record_tutor_speech_outcome(
        &self,
        work: &TutorSpeechWork,
        outcome: &SpeechOutcome,
    ) -> Result<()> {
        ensure!(
            outcome.epoch == work.audio_epoch
                && !outcome.playback_id.is_empty()
                && outcome.playback_id.len() <= 96,
            "音频结束回执身份无效"
        );
        ensure!(
            ["qwen_pcm", "cosy_pcm", "sapi", "none"].contains(&outcome.engine_kind)
                && !outcome.speaker_output_measured,
            "音频结束证据类型无效"
        );
        ensure!(
            outcome
                .reason
                .as_ref()
                .is_none_or(|reason| reason.len() <= 256),
            "音频结束原因超限"
        );
        self.tx(|| {
            let (stream,row) = self.bound_speech_row(work)?;
            let value = json!(outcome);
            if let Some(previous) = row["speech_terminal_receipt"].as_str() {
                ensure!(serde_json::from_str::<Value>(previous)? == value,"重复终态回执不一致");
                return Ok(());
            }
            ensure!(gate(&stream)? == SpeechGate::Open,"播报已经关闭，不能接受新的终态");
            let mut ledger = self.speech_ledger(&stream)?;
            if let Some(started) = row["speech_started_receipt"].as_str() {
                let started:Value = serde_json::from_str(started)?;
                ensure!(started["playback_id"] == outcome.playback_id && started["epoch"].as_u64() == Some(outcome.epoch) && started["engine_kind"] == outcome.engine_kind,"音频开始与终态回执不匹配");
                ledger.receipt(DeliveryReceipt {binding:DeliveryBinding {request_id:work.request_id.clone(),generation:work.generation as u64},sequence:work.sequence,
                    kind:if outcome.state == SpeechEnd::Completed {ReceiptKind::SpeechCompleted} else {ReceiptKind::SpeechInterrupted}})?;
            } else {
                ensure!(row["speech_state"] == "reserved" && outcome.state == SpeechEnd::Interrupted,"未收到真实开始不能确认完成");
                ledger.close_speech();
            }
            if outcome.state == SpeechEnd::Completed && ["qwen_pcm", "cosy_pcm"].contains(&outcome.engine_kind) {
                ensure!(outcome.callback_submitted_frames > 0 && outcome.predicted_output_drained,"PCM尚未提交或预计排空，不能确认完成");
            }
            self.db.execute("UPDATE tutor_segments SET speech_state=?,speech_terminal_receipt=?,speech_end_origin='owner_receipt',speech_end_reason=? WHERE request_id=? AND sequence=?",
                params![if outcome.state == SpeechEnd::Completed {"completed"} else {"interrupted"},value.to_string(),outcome.reason,work.request_id,work.sequence as i64])?;
            if outcome.state == SpeechEnd::Interrupted {
                self.close_tutor_speech(&work.session_id,&work.request_id,work.generation,"audio_interrupted")?;
            } else {
                self.db.execute("UPDATE tutor_streams SET revision=revision+1 WHERE request_id=?",[&work.request_id])?;
            }
            Ok(())
        })
    }

    pub(crate) fn close_tutor_speech(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        reason: &str,
    ) -> Result<Value> {
        safe_reason(reason)?;
        self.tx(|| {
            let stream = self.tutor_stream_row(sid,rid)?;
            ensure!(stream["generation"] == generation,"播报关闭请求代次不匹配");
            self.db.execute("UPDATE tutor_streams SET speech_gate='closed',speech_close_reason=?,revision=revision+1 WHERE request_id=? AND speech_gate='open'",params![reason,rid])?;
            self.tutor_speech_state(sid,rid)
        })
    }
}

#[cfg(test)]
#[path = "tutor_speech_tests.rs"]
mod tests;

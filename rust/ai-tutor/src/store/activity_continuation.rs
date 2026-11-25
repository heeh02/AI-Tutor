//! Explicit continuation of a finished activity. The receipt stores identities,
//! not historical text; every displayed summary is resolved from live sources.
use super::*;
use std::collections::HashMap;

impl Store {
    pub(super) fn migrate_activity_continuations(&self) -> Result<()> {
        self.db.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_continuations(
                session_id TEXT PRIMARY KEY, activity_id TEXT NOT NULL,
                learner_id TEXT NOT NULL, namespace TEXT NOT NULL,
                source_session_id TEXT NOT NULL, source_activity_id TEXT NOT NULL,
                client_submission_id TEXT NOT NULL,
                expected_current_session_id TEXT NOT NULL, expected_generation INTEGER NOT NULL,
                selection_event_id TEXT NOT NULL, selection_event_version INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(namespace,learner_id,client_submission_id)
             );
             CREATE TRIGGER IF NOT EXISTS continuation_learner_delete AFTER DELETE ON learners BEGIN
                DELETE FROM session_continuations WHERE learner_id=OLD.learner_id;
             END;",
        )?;
        let wrong: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM session_continuations WHERE namespace!=?",
            [&self.namespace],
            |row| row.get(0),
        )?;
        ensure!(wrong == 0, "续做关联空间不一致");
        Ok(())
    }

    // Retain an invalid receipt after source/session deletion, so retrying its
    // nonce cannot recreate a choice that was forgotten or invalidated.
    fn valid_continuation_link(&self, sid: &str, lid: &str) -> Result<Option<Value>> {
        let row = self.one(
            "SELECT c.*,e.content AS selection_content,e.source_type AS selection_source_type
             FROM session_continuations c
             JOIN sessions n ON n.session_id=c.session_id AND n.learner_id=c.learner_id AND n.activity_id=c.activity_id
             JOIN sessions o ON o.session_id=c.source_session_id AND o.learner_id=c.learner_id AND o.activity_id=c.source_activity_id
             JOIN events e ON e.event_id=c.selection_event_id AND e.session_id=c.session_id AND e.learner_id=c.learner_id
             JOIN education_event_meta m ON m.event_id=e.event_id AND m.session_id=e.session_id AND m.learner_id=e.learner_id
                AND m.namespace=c.namespace AND m.version=c.selection_event_version AND m.status='active'
             WHERE c.session_id=? AND c.learner_id=? AND c.namespace=?
                AND o.status='finished' AND o.rowid<n.rowid AND m.kind='learner_statement'",
            params![sid,lid,self.namespace],
        )?;
        let Some(row) = row else { return Ok(None) };
        let selection: Value = serde_json::from_str(s(&row, "selection_content"))?;
        let expected_source = if self.namespace == "simulator" {
            "simulated"
        } else {
            "operator_confirmed"
        };
        if s(&row, "selection_source_type") != expected_source
            || selection["action"] != "continue_finished_activity"
            || selection["source_session_id"] != row["source_session_id"]
            || selection["source_activity_id"] != row["source_activity_id"]
        {
            return Ok(None);
        }
        Ok(Some(row))
    }

    fn continuation_chain(&self, sid: &str) -> Result<Vec<Value>> {
        let (lid, _) = self.session_memory_activity(sid)?;
        let mut next = sid.to_owned();
        let mut links = Vec::new();
        // The validated source rowid must decrease at every step. No cycles or
        // caller-supplied ancestry, including after a legacy identity rewrite.
        while let Some(link) = self.valid_continuation_link(&next, &lid)? {
            next = s(&link, "source_session_id").to_owned();
            links.push(link);
        }
        Ok(links)
    }

    /// Context identity only: expose the current session and valid explicitly
    /// selected ancestors, never sibling inventory or inferred topic links.
    pub(crate) fn selected_session_depths(&self, sid: &str) -> Result<HashMap<String, usize>> {
        self.tx(|| {
            let mut depths = HashMap::from([(sid.to_owned(), 0)]);
            for (index, link) in self.continuation_chain(sid)?.into_iter().enumerate() {
                depths.insert(s(&link, "source_session_id").to_owned(), index + 1);
            }
            Ok(depths)
        })
    }

    pub(super) fn session_memory_activities(&self, sid: &str) -> Result<(String, HashSet<String>)> {
        let (lid, activity) = self.session_memory_activity(sid)?;
        let mut activities = HashSet::from([activity]);
        for link in self.continuation_chain(sid)? {
            activities.insert(s(&link, "source_activity_id").to_owned());
        }
        Ok((lid, activities))
    }

    fn historical_field(
        &self,
        lid: &str,
        row: &Value,
        field: &str,
        activities: &HashSet<String>,
    ) -> Result<Option<Value>> {
        let state: Value = serde_json::from_str(s(row, "state"))?;
        let text = s(&state, field);
        let ids = strings(&state["field_evidence"][field]["source_event_ids"]);
        if text.trim().is_empty() || text.chars().count() > 1000 || ids.is_empty() || ids.len() > 8
        {
            return Ok(None);
        }
        let sources = self.source_events(lid, &ids)?;
        if sources.len() != ids.iter().collect::<HashSet<_>>().len() {
            return Ok(None);
        }
        let mut versions = json!({});
        for source in &sources {
            let Some(activity) = self.source_memory_activity(lid, s(source, "event_id"))? else {
                return Ok(None);
            };
            if !activities.contains(&activity) {
                return Ok(None);
            }
            if field == "goal"
                && !["learner_statement", "teacher_setting", "legacy_report"]
                    .contains(&s(source, "event_kind"))
            {
                return Ok(None);
            }
            if field == "pending_step" {
                let content = &source["content"];
                let confirmed = if s(source, "event_kind") == "plan" {
                    content["accepted"] == true
                        && (s(content, "text").trim() == text.trim()
                            || s(content, "pending_experiment") == text)
                } else {
                    let raw = if s(content, "report").is_empty() {
                        s(content, "text")
                    } else {
                        s(content, "report")
                    };
                    let trim = |v: &str| {
                        v.trim()
                            .trim_end_matches(['。', '！', '!', '.', '；', ';'])
                            .to_owned()
                    };
                    let future=Regex::new("(?i)下次|下一次|接着|计划|准备|我会|我们会|next time|we will|i will|plan to")?.is_match(raw);
                    let uncertain=Regex::new("(?i)是否|要不要|还没决定|尚未决定|可能|也许|或者|[?？]|don't|do not|maybe|whether")?.is_match(raw);
                    s(content, "pending_experiment") == text
                        || (trim(raw) == trim(text) && future && !uncertain)
                };
                if !confirmed {
                    return Ok(None);
                }
            }
            versions[s(source, "event_id")] = source["event_version"].clone();
        }
        let mut result = json!({"text":text,"source_session_id":row["session_id"],"source_activity_id":row["activity_id"],
            "source_event_ids":ids,"source_event_versions":versions,"historical":true,"hardware_verified":false});
        if field == "pending_step" {
            result["as_of_session_id"] = row["session_id"].clone();
            result["completion_status"] = json!("not_revalidated");
        }
        Ok(Some(result))
    }

    fn older_pending_resolution(
        &self,
        lid: &str,
        row: &Value,
        activities: &HashSet<String>,
    ) -> Result<Option<Value>> {
        let state: Value = serde_json::from_str(s(row, "state"))?;
        let field = if s(&state, "pending_step").is_empty()
            && !strings(&state["field_evidence"]["pending_step"]["source_event_ids"]).is_empty()
        {
            "pending_step"
        } else if state["stage"] == "completed" {
            "stage"
        } else {
            return Ok(None);
        };
        let ids = strings(&state["field_evidence"][field]["source_event_ids"]);
        if ids.is_empty() || ids.len() > 8 {
            return Ok(None);
        }
        let sources = self.source_events(lid, &ids)?;
        if sources.len() != ids.iter().collect::<HashSet<_>>().len() {
            return Ok(None);
        }
        let mut versions = json!({});
        for source in sources {
            if field == "stage" && source["event_kind"] == "plan" {
                return Ok(None);
            }
            if !self
                .source_memory_activity(lid, s(&source, "event_id"))?
                .is_some_and(|v| activities.contains(&v))
            {
                return Ok(None);
            }
            versions[s(&source, "event_id")] = source["event_version"].clone();
        }
        Ok(Some(
            json!({"kind":if field=="stage"{"completed_activity"}else{"cleared_pending_step"},
            "field":field,"value":state[field],"source_session_id":row["session_id"],"source_activity_id":row["activity_id"],
            "source_event_ids":ids,"source_event_versions":versions,"historical":true,"hardware_verified":false,
            "meaning":"historical state-field resolution; not proof that a specific experiment was performed"}),
        ))
    }

    fn historical_summary(&self, sid: &str) -> Result<Value> {
        let (lid, activity) = self.session_memory_activity(sid)?;
        let chain = self.continuation_chain(sid)?;
        let mut activities = HashSet::from([activity]);
        activities.extend(chain.iter().map(|v| s(v, "source_activity_id").to_owned()));
        let mut sessions = vec![sid.to_owned()];
        sessions.extend(chain.iter().map(|v| s(v, "source_session_id").to_owned()));
        let mut result = json!({"goal":null,"pending_step":null,"pending_resolution":null,"ancestor_count":chain.len()});
        let mut pending_closed = false;
        for session in sessions {
            let Some(row)=self.one("SELECT session_id,activity_id,state FROM sessions WHERE session_id=? AND learner_id=?",params![session,lid])? else {continue};
            for field in ["goal", "pending_step"] {
                if field == "pending_step" && pending_closed {
                    continue;
                }
                if result[field].is_null() {
                    if let Some(value) = self.historical_field(&lid, &row, field, &activities)? {
                        result[field] = value;
                    }
                }
            }
            if result["pending_step"].is_null() && !pending_closed {
                if let Some(resolution) = self.older_pending_resolution(&lid, &row, &activities)? {
                    result["pending_resolution"] = resolution;
                    pending_closed = true;
                }
            }
            if !result["goal"].is_null() && (!result["pending_step"].is_null() || pending_closed) {
                break;
            }
        }
        Ok(result)
    }

    pub fn session_continuation(&self, sid: &str) -> Result<Value> {
        let (lid, _) = self.session_memory_activity(sid)?;
        let Some(link) = self.valid_continuation_link(sid, &lid)? else {
            return Ok(Value::Null);
        };
        let summary = self.historical_summary(s(&link, "source_session_id"))?;
        let ancestor_count = summary["ancestor_count"].as_u64().unwrap_or(0) + 1;
        Ok(
            json!({"client_submission_id":link["client_submission_id"],"source_session_id":link["source_session_id"],
            "source_activity_id":link["source_activity_id"],"selection_event_id":link["selection_event_id"],
            "selection_event_version":link["selection_event_version"],"historical":true,"hardware_verified":false,
            "requires_inventory_confirmation":true,"ancestor_count":ancestor_count,"goal":summary["goal"],"pending_step":summary["pending_step"],"pending_resolution":summary["pending_resolution"]}),
        )
    }

    pub(super) fn annotate_activity_continuation(&self, session: &mut Value) -> Result<()> {
        let continuation = self.session_continuation(s(session, "session_id"))?;
        session["continued_from"] = if continuation.is_null() {
            Value::Null
        } else {
            json!({"session_id":continuation["source_session_id"],
            "activity_id":continuation["source_activity_id"],"goal":s(&continuation["goal"],"text")})
        };
        session["continuation"] = continuation;
        Ok(())
    }

    pub fn session_catalog(&self, lid: &str, before: Option<i64>, limit: usize) -> Result<Value> {
        ensure!((1..=50).contains(&limit), "活动目录页大小必须为1至50");
        ensure!(before.is_none_or(|v| v > 0), "活动目录游标无效");
        self.learner(lid)?;
        let mut rows=match before {
            Some(cursor)=>self.rows("SELECT rowid AS cursor,session_id,activity_id,created_at,status FROM sessions WHERE learner_id=? AND rowid<? ORDER BY rowid DESC LIMIT ?",params![lid,cursor,(limit+1)as i64])?,
            None=>self.rows("SELECT rowid AS cursor,session_id,activity_id,created_at,status FROM sessions WHERE learner_id=? ORDER BY rowid DESC LIMIT ?",params![lid,(limit+1)as i64])?,
        };
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = if more {
            rows.last().and_then(|r| r["cursor"].as_i64())
        } else {
            None
        };
        for row in &mut rows {
            let summary = self.historical_summary(s(row, "session_id"))?;
            row["goal"] = json!(s(&summary["goal"], "text"));
            self.annotate_activity_continuation(row)?;
            row.as_object_mut().unwrap().remove("cursor");
            row.as_object_mut().unwrap().remove("continuation");
        }
        Ok(json!({"learner_id":lid,"namespace":self.namespace,"items":rows,"next_before":next}))
    }

    fn validate_continuation_payload(
        &self,
        lid: &str,
        source_sid: &str,
        nonce: &str,
        current_sid: &str,
        generation: i64,
    ) -> Result<()> {
        self.learner(lid)?;
        ensure!(
            !nonce.is_empty()
                && nonce.len() <= 96
                && nonce
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "续做请求编号无效"
        );
        ensure!(
            !source_sid.is_empty()
                && !current_sid.is_empty()
                && generation >= 0
                && generation.checked_add(1).is_some(),
            "续做活动身份或代次无效"
        );
        let source = self
            .one(
                "SELECT status,activity_id FROM sessions WHERE session_id=? AND learner_id=?",
                params![source_sid, lid],
            )?
            .ok_or_else(|| anyhow!("来源活动不存在或不属于当前档案"))?;
        ensure!(
            source["status"] == "finished" && !s(&source, "activity_id").is_empty(),
            "只能明确续做已结束活动"
        );
        Ok(())
    }

    fn validate_continuation_current(&self, lid: &str, sid: &str, generation: i64) -> Result<()> {
        let row = self
            .one(
                "SELECT generation FROM sessions WHERE session_id=? AND learner_id=?",
                params![sid, lid],
            )?
            .ok_or_else(|| anyhow!("当前活动归属已失效"))?;
        ensure!(row["generation"] == generation, "当前活动代次已变化");
        if let Some(active)=self.one("SELECT session_id FROM sessions WHERE learner_id=? AND status!='finished' ORDER BY rowid DESC LIMIT 1",[lid])? {
            ensure!(active["session_id"]==sid,"当前活动已切换");
        }
        Ok(())
    }

    fn continuation_receipt(
        &self,
        lid: &str,
        source_sid: &str,
        nonce: &str,
        current_sid: &str,
        generation: i64,
    ) -> Result<Option<Value>> {
        let Some(receipt)=self.one("SELECT * FROM session_continuations WHERE learner_id=? AND namespace=? AND client_submission_id=?",params![lid,self.namespace,nonce])? else {return Ok(None)};
        ensure!(
            receipt["source_session_id"] == source_sid
                && receipt["expected_current_session_id"] == current_sid
                && receipt["expected_generation"] == generation,
            "续做编号已用于不同请求"
        );
        let sid = s(&receipt, "session_id");
        ensure!(
            self.valid_continuation_link(sid, lid)?.is_some(),
            "续做选择来源已失效"
        );
        let active=self.one("SELECT session_id FROM sessions WHERE learner_id=? AND status!='finished' ORDER BY rowid DESC LIMIT 1",[lid])?;
        ensure!(
            active.as_ref().is_none_or(|v| v["session_id"] == sid),
            "续做操作所属活动已离开"
        );
        self.continuation_response(sid, nonce, true).map(Some)
    }

    fn continuation_response(&self, sid: &str, nonce: &str, replayed: bool) -> Result<Value> {
        let session = self.session(sid)?;
        Ok(
            json!({"learner_id":session["learner_id"],"session_id":session["session_id"],"activity_id":session["activity_id"],
            "client_submission_id":nonce,"replayed":replayed,"session":session}),
        )
    }

    /// Preflight is read-only. A returned replay must additionally match the
    /// Workstation's current session before the server returns without cancelling.
    pub fn continuation_replay(
        &self,
        lid: &str,
        source_sid: &str,
        nonce: &str,
        expected_current_sid: &str,
        expected_generation: i64,
    ) -> Result<Option<Value>> {
        self.validate_continuation_payload(
            lid,
            source_sid,
            nonce,
            expected_current_sid,
            expected_generation,
        )?;
        if let Some(value) = self.continuation_receipt(
            lid,
            source_sid,
            nonce,
            expected_current_sid,
            expected_generation,
        )? {
            return Ok(Some(value));
        }
        self.validate_continuation_current(lid, expected_current_sid, expected_generation)?;
        Ok(None)
    }

    /// The server holds its control lock, preflights, cancels once (bumping the
    /// old generation), and passes that exact new generation into this transaction.
    pub fn continue_finished_session(
        &self,
        lid: &str,
        source_sid: &str,
        nonce: &str,
        expected_current_sid: &str,
        expected_generation: i64,
        current_generation_after_cancel: i64,
    ) -> Result<Value> {
        self.tx(||{
            self.validate_continuation_payload(lid,source_sid,nonce,expected_current_sid,expected_generation)?;
            if let Some(value)=self.continuation_receipt(lid,source_sid,nonce,expected_current_sid,expected_generation)? {return Ok(value);}
            ensure!(expected_generation.checked_add(1)==Some(current_generation_after_cancel),"续做取消代次不匹配");
            self.validate_continuation_current(lid,expected_current_sid,current_generation_after_cancel)?;
            let (_,source_activity)=self.session_memory_activity(source_sid)?;
            let session=self.create_session(lid)?;
            let sid=s(&session,"session_id");
            let event=self.record_education_event(lid,sid,"operator_confirmed","learner_statement",&json!({"action":"continue_finished_activity","source_session_id":source_sid,"source_activity_id":source_activity,"hardware_verified":false}))?;
            self.db.execute("INSERT INTO session_continuations VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",params![sid,s(&session,"activity_id"),lid,self.namespace,source_sid,source_activity,nonce,expected_current_sid,expected_generation,s(&event,"event_id"),event["event_version"].as_i64().unwrap(),now()])?;
            self.continuation_response(sid,nonce,false)
        })
    }
}

//! Native transactional SQLite state, provenance, isolation and real deletion.
use anyhow::{Result, anyhow, ensure};
use chrono::Utc;
use regex::Regex;
use rusqlite::{Connection, Params, params, types::ValueRef};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};
use uuid::Uuid;
mod activity_continuation;
mod consolidation;
mod education;
mod embeddings;
mod experiments;
mod help_preference;
mod history;
mod memory_activity;
mod memory_comparison;
mod memory_discovery;
mod memory_policy;
mod memory_review;
mod tutor_delivery;
mod tutor_speech;
pub use consolidation::MemoryJobLimits;
pub use embeddings::{
    CandidateRanking, EligibleMemoryEmbedding, EmbeddingCoverage, EmbeddingExclusion,
    MemoryEmbeddingSnapshot, QueryEmbedding, RecallCandidate,
};
pub(crate) use tutor_speech::TutorSpeechWork;

pub fn uid(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}
pub fn now() -> String {
    Utc::now().to_rfc3339()
}
pub fn empty_state() -> Value {
    json!({"goal":"","stage":"goal_setting","reported_result":"","pending_step":"","modules":null,"modules_source":"unknown","latest_input":"","last_delivered_hint":"","attempts":[],"session_preferences":{},"field_evidence":{},"requires_inventory_confirmation":true})
}
pub fn s<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub struct Store {
    pub db: Connection,
    pub namespace: String,
}
impl Store {
    pub fn open(path: impl AsRef<Path>, namespace: &str) -> Result<Self> {
        ensure!(["real", "simulator"].contains(&namespace), "无效数据空间");
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA secure_delete=ON;")?;
        let store = Self {
            db,
            namespace: namespace.to_owned(),
        };
        store.tx(|| {
        store.interrupt_tutor_speech_on_open()?;
        store.db.execute_batch("
        CREATE TABLE IF NOT EXISTS learners(learner_id TEXT PRIMARY KEY,name TEXT,language TEXT,help_style TEXT,epoch INTEGER DEFAULT 0);
        CREATE TABLE IF NOT EXISTS sessions(session_id TEXT PRIMARY KEY,learner_id TEXT,activity_id TEXT,generation INTEGER DEFAULT 0,status TEXT,state TEXT,created_at TEXT);
        CREATE TABLE IF NOT EXISTS events(event_id TEXT PRIMARY KEY,learner_id TEXT,session_id TEXT,source_type TEXT,content TEXT,created_at TEXT);
        CREATE TABLE IF NOT EXISTS turns(turn_id TEXT PRIMARY KEY,learner_id TEXT,session_id TEXT,request_id TEXT,generation INTEGER,role TEXT,content TEXT,status TEXT,evidence TEXT,created_at TEXT);
        CREATE TABLE IF NOT EXISTS memories(memory_id TEXT PRIMARY KEY,learner_id TEXT,activity_scope TEXT,kind TEXT,content TEXT,source_event_ids TEXT,source_type TEXT,scope TEXT,created_at TEXT,updated_at TEXT,status TEXT,supersedes TEXT,epoch INTEGER);
        CREATE TABLE IF NOT EXISTS tombstones(id TEXT PRIMARY KEY,learner_id TEXT,epoch INTEGER,created_at TEXT);
        CREATE TABLE IF NOT EXISTS provider_runs(request_id TEXT PRIMARY KEY,learner_id TEXT,session_id TEXT,status TEXT,created_at TEXT,latency_ms INTEGER,payload_chars INTEGER,reported_tokens INTEGER);
        CREATE INDEX IF NOT EXISTS memory_owner ON memories(learner_id,status);
        CREATE INDEX IF NOT EXISTS events_owner ON events(learner_id,session_id);
        CREATE INDEX IF NOT EXISTS sessions_owner ON sessions(learner_id);
        CREATE INDEX IF NOT EXISTS turns_session ON turns(session_id);
        CREATE INDEX IF NOT EXISTS events_session ON events(session_id);
        UPDATE turns SET status='cancelled' WHERE status IN('generated','generating');
        UPDATE turns SET status='speech_interrupted' WHERE status='speech_started';
        UPDATE sessions SET status='idle',generation=generation+1 WHERE status='generating';
        UPDATE provider_runs SET status='interrupted' WHERE status='running';")?;
        store.migrate_education()?;
        store.migrate_memory_activity()?;
        store.migrate_activity_continuations()?;
        store.migrate_learner_preferences()?;
        store.migrate_memory_jobs()?;
        store.migrate_memory_embeddings()?;
        store.migrate_tutor_delivery()?;
        store.migrate_experiments()?;
        store.migrate_help_preference_receipts()?;
        Ok(())
        })?;
        Ok(store)
    }
    pub fn rows(&self, sql: &str, args: impl Params) -> Result<Vec<Value>> {
        let mut statement = self.db.prepare(sql)?;
        let names: Vec<_> = statement
            .column_names()
            .iter()
            .map(|x| x.to_string())
            .collect();
        let result = statement
            .query_map(args, |row| {
                let mut object = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let value = match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(n) => json!(n),
                        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        ValueRef::Blob(_) => Value::Null,
                    };
                    object.insert(name.clone(), value);
                }
                Ok(Value::Object(object))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(result)
    }
    pub fn one(&self, sql: &str, args: impl Params) -> Result<Option<Value>> {
        Ok(self.rows(sql, args)?.into_iter().next())
    }
    pub(crate) fn tx<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let outer = self.db.is_autocommit();
        if outer {
            self.db.execute_batch("BEGIN IMMEDIATE")?;
        }
        match action() {
            Ok(value) => {
                if outer {
                    if let Err(error) = self.db.execute_batch("COMMIT") {
                        let _ = self.db.execute_batch("ROLLBACK");
                        return Err(error.into());
                    }
                }
                Ok(value)
            }
            Err(error) => {
                if outer {
                    let _ = self.db.execute_batch("ROLLBACK");
                }
                Err(error)
            }
        }
    }
    pub fn learners(&self) -> Result<Vec<Value>> {
        let mut rows = self.rows(
            "SELECT learner_id,name,language,help_style,epoch FROM learners ORDER BY rowid",
            [],
        )?;
        for row in &mut rows {
            row["namespace"] = json!(self.namespace);
            let preferences = self.learner_preferences(s(row, "learner_id"))?;
            row["reading_level"] = preferences["reading_level"].clone();
            row["speech_speed"] = preferences["speech_speed"].clone();
        }
        Ok(rows)
    }
    pub fn learner(&self, id: &str) -> Result<Value> {
        let mut row = self
            .one("SELECT * FROM learners WHERE learner_id=?", [id])?
            .ok_or_else(|| anyhow!("学习者档案不存在"))?;
        row["namespace"] = json!(self.namespace);
        let preferences = self.learner_preferences(id)?;
        row["reading_level"] = preferences["reading_level"].clone();
        row["speech_speed"] = preferences["speech_speed"].clone();
        Ok(row)
    }
    pub fn create_learner(&self, name: &str, language: &str, help_style: &str) -> Result<Value> {
        ensure!(
            !name.trim().is_empty() && name.chars().count() <= 48,
            "档案名称无效"
        );
        let id = uid(if self.namespace == "simulator" {
            "sim"
        } else {
            "learner"
        });
        self.tx(|| {
            self.db.execute(
                "INSERT INTO learners(learner_id,name,language,help_style) VALUES(?,?,?,?)",
                params![id, name, language, help_style],
            )?;
            self.db.execute(
                "INSERT INTO learner_preferences(learner_id,namespace) VALUES(?,?)",
                params![id, self.namespace],
            )?;
            self.learner(&id)
        })
    }
    pub fn create_session(&self, lid: &str) -> Result<Value> {
        self.tx(|| {
            self.learner(lid)?;
            // Starting again is an explicit activity boundary, including legacy
            // databases that may contain more than one unfinished activity.
            for row in self.rows(
                "SELECT session_id FROM sessions WHERE learner_id=? AND status!='finished' ORDER BY rowid",
                [lid],
            )? {
                self.consolidate_and_finish(s(&row, "session_id"))?;
            }
            let id = uid("session");
            self.db.execute(
                "INSERT INTO sessions VALUES(?,?,?,0,'idle',?,?)",
                params![id, lid, uid("activity"), empty_state().to_string(), now()],
            )?;
            self.session(&id)
        })
    }
    pub fn latest_session(&self, lid: &str) -> Result<Option<Value>> {
        match self.one("SELECT session_id FROM sessions WHERE learner_id=? AND status!='finished' ORDER BY rowid DESC LIMIT 1",[lid])? { Some(v)=>Ok(Some(self.session(s(&v,"session_id"))?)),None=>Ok(None) }
    }
    pub fn session(&self, id: &str) -> Result<Value> {
        let mut result = self
            .one("SELECT * FROM sessions WHERE session_id=?", [id])?
            .ok_or_else(|| anyhow!("会话不存在"))?;
        result["state"] = serde_json::from_str(s(&result, "state"))?;
        // Read metadata stays outside state: writers clone state and checkpoint it.
        result["pending_step_needs_confirmation"] =
            json!(self.pending_step_needs_confirmation(&result)?);
        result["namespace"] = json!(self.namespace);
        result["state_revision"] = json!(self.state_revision(id)?);
        let mut turns = self.rows(
            "SELECT rowid AS history_cursor,* FROM turns WHERE session_id=? ORDER BY rowid DESC LIMIT 24",
            [id],
        )?;
        result["history_before"] = turns
            .last()
            .map(|turn| turn["history_cursor"].clone())
            .unwrap_or(Value::Null);
        for turn in &mut turns {
            turn.as_object_mut().unwrap().remove("history_cursor");
            turn["evidence"] = serde_json::from_str(s(turn, "evidence"))?;
        }
        turns.reverse();
        result["turns"] = json!(turns);
        self.annotate_tutor_delivery(&mut result)?;
        self.annotate_activity_continuation(&mut result)?;
        let goal = s(&result["state"], "goal");
        result["recalled_memories"] = json!(self.recall_for_session(
            id,
            if goal.is_empty() {
                "小车 停车 墙 距离 灯"
            } else {
                goal
            },
            5
        )?);
        Ok(result)
    }
    pub(crate) fn pending_step_needs_confirmation(&self, session: &Value) -> Result<bool> {
        let state = &session["state"];
        let Some(value) = state.get("pending_step") else {
            return Ok(false);
        };
        // An empty default or previously cleared field has no step to project.
        // This is not permission to write a clearing update.
        if value.as_str() == Some("") {
            return Ok(false);
        }
        let ids = &state["field_evidence"]["pending_step"]["source_event_ids"];
        if !ids.as_array().is_some_and(|ids| {
            (1..=8).contains(&ids.len())
                && ids
                    .iter()
                    .all(|id| id.as_str().is_some_and(|id| !id.is_empty()))
        }) {
            return Ok(true);
        }
        let lid = s(session, "learner_id");
        let sources = self.source_events(lid, &strings(ids))?;
        Ok(!crate::pending_step::supported_current_read(
            value,
            ids,
            &sources,
            lid,
            s(session, "session_id"),
        ))
    }
    pub fn checkpoint(&self, sid: &str, state: &Value) -> Result<()> {
        self.db.execute(
            "UPDATE sessions SET state=? WHERE session_id=?",
            params![state.to_string(), sid],
        )?;
        Ok(())
    }
    pub fn bump(&self, sid: &str, status: &str) -> Result<i64> {
        self.tx(||{self.db.execute("UPDATE sessions SET generation=generation+1,status=CASE WHEN status='finished' THEN 'finished' ELSE ? END WHERE session_id=?",params![status,sid])?;self.db.execute("UPDATE turns SET status='cancelled' WHERE session_id=? AND status IN('generated','generating')",[sid])?;self.db.execute("UPDATE turns SET status='speech_interrupted' WHERE session_id=? AND status='speech_started'",[sid])?;Ok(self.session(sid)?["generation"].as_i64().unwrap_or_default())})
    }
    pub fn set_status(&self, sid: &str, status: &str) -> Result<()> {
        self.db.execute(
            "UPDATE sessions SET status=CASE WHEN status='finished' THEN 'finished' ELSE ? END WHERE session_id=?",
            params![status, sid],
        )?;
        Ok(())
    }
    pub fn set_session_status(&self, sid: &str, status: &str) -> Result<()> {
        self.set_status(sid, status)
    }
    pub fn finish_session(&self, sid: &str) -> Result<()> {
        self.consolidate_and_finish(sid)?;
        Ok(())
    }
    /// Close an activity without a remote summary call. Only current, valid
    /// report sources are copied verbatim; neither hardware nor success is inferred.
    pub fn consolidate_and_finish(&self, sid: &str) -> Result<Value> {
        self.tx(|| {
            let session = self.session(sid)?;
            let mut state = session["state"].clone();
            if s(&session, "status") == "finished" {
                return Ok(json!({"ok":true,"already_finished":true,"consolidated_memory_ids":state.get("finish_memory_ids").cloned().unwrap_or(json!([]))}));
            }
            let lid = s(&session, "learner_id");
            let epoch = self.learner(lid)?["epoch"].as_i64().unwrap_or_default();
            let mut saved = Vec::<String>::new();
            let mut seen_sources = HashSet::new();
            let mut omitted = 0usize;
            for (field, kind) in [("reported_result", "reported_progress"), ("pending_step", "pending_experiment")] {
                let value = s(&state, field);
                let provenance = &state["field_evidence"][field];
                if value.trim().is_empty() || provenance["historical"] == true || s(&state, &format!("{field}_source")) == "historical" { continue; }
                let ids = strings(&provenance["source_event_ids"]);
                let sources = self.source_events(lid, &ids)?;
                if ids.is_empty() || ids.len()>8 || sources.len()!=ids.iter().collect::<HashSet<_>>().len() { omitted += 1; continue; }
                // A mixed reference set is not permission to promote its plan
                // portion as observed or accepted work. Legacy untyped reports
                // retain their existing verbatim/future-intent validation.
                if sources.iter().any(|source| s(source,"event_kind")=="plan"
                    && (field=="reported_result" || source["content"]["accepted"]!=true)) {
                    omitted += 1;
                    continue;
                }
                for source in sources.iter().rev().filter(|row| s(row,"session_id")==sid) {
                    let raw = if !s(&source["content"], "report").trim().is_empty() { s(&source["content"], "report") } else { s(&source["content"], "text") };
                    if raw.trim().is_empty() { continue; }
                    // A model's next-step suggestion is not an accepted experiment.
                    // Persist it only when the source itself states that exact step.
                    if field=="pending_step" {
                        let trim = |text:&str| text.trim().trim_end_matches(['。','！','!','.','；',';']).to_owned();
                        let explicit_future=Regex::new("(?i)下次|下一次|留到|计划|准备|我会|我们会|next time|we will|i will|plan to")?.is_match(raw);
                        let uncertain=Regex::new("(?i)是否|要不要|还没决定|尚未决定|建议|也许|可能|[?？]|don't|do not|maybe|whether")?.is_match(raw);
                        let source_states_step=trim(raw)==trim(value)&&explicit_future&&!uncertain;
                        let structured_confirmation=s(&source["content"],"pending_experiment")==value;
                        if !source_states_step&&!structured_confirmation { omitted += 1; continue; }
                    }
                    let key = (s(source,"event_id").to_owned(),kind);
                    if !seen_sources.insert(key) { continue; }
                    let chars: Vec<char> = if field=="pending_step" { value.chars().collect() } else { raw.chars().collect() };
                    let chunks: Vec<String> = chars.chunks(700).map(|part| part.iter().collect()).collect();
                    if saved.len()+chunks.len()>8 { omitted += 1; continue; }
                    for (index, chunk) in chunks.iter().enumerate() {
                        let label = if field=="pending_step" { "来源明确陈述的待做步骤原文" } else { "结束时报告原文" };
                        let content = format!("小车活动{label}（{}/{}，仅报告，未硬件验证）：{chunk}", index+1, chunks.len());
                        let candidate=json!({"kind":kind,"scope":"current_activity","content":content,"source_event_ids":[source["event_id"]]});
                        // This exact-source gate above has already authorized
                        // the verbatim text; its server-generated label/chunks
                        // are not an untrusted foreground summary.
                        if let Some(id)=self.add_memory_inner(lid,&candidate,epoch,true)? { if !saved.contains(&id) { saved.push(id); } }
                    }
                }
            }
            state["finish_memory_ids"]=json!(saved);
            state["finish_consolidation"]=json!({"method":"verbatim_current_sources","omitted_sources":omitted,"max_records":8,"hardware_verified":false});
            state["finished_at"]=json!(now());
            self.checkpoint(sid,&state)?;
            self.bump(sid,"finished")?;
            Ok(json!({"ok":true,"already_finished":false,"consolidated_memory_ids":saved,"consolidation":state["finish_consolidation"]}))
        })
    }
    pub fn event(&self, lid: &str, sid: &str, source: &str, content: &Value) -> Result<String> {
        ensure!(
            s(&self.session(sid)?, "learner_id") == lid,
            "事件与学习者不匹配"
        );
        ensure!(
            ["learner_report", "operator_confirmed", "simulated"].contains(&source),
            "来源类型无效"
        );
        ensure!(
            self.namespace == "simulator" || source != "simulated",
            "模拟事件只能写入模拟档案"
        );
        let source = if self.namespace == "simulator" {
            "simulated"
        } else {
            source
        };
        let id = uid("event");
        self.db.execute(
            "INSERT INTO events VALUES(?,?,?,?,?,?)",
            params![id, lid, sid, source, content.to_string(), now()],
        )?;
        Ok(id)
    }
    pub fn source_events(&self, lid: &str, ids: &[String]) -> Result<Vec<Value>> {
        ensure!(ids.len() <= 100, "来源数量超出上限");
        let mut result = vec![];
        let mut seen = HashSet::new();
        for id in ids {
            if seen.insert(id) {
                if let Some(mut row) = self.one(
                    "SELECT e.*,es.activity_id,m.kind AS event_kind,m.version AS event_version,m.status AS event_status FROM events e JOIN sessions es ON es.session_id=e.session_id AND es.learner_id=e.learner_id JOIN education_event_meta m ON m.event_id=e.event_id AND m.learner_id=e.learner_id AND m.session_id=e.session_id WHERE e.learner_id=? AND e.event_id=? AND m.status='active' AND m.namespace=?",
                    params![lid, id, self.namespace],
                )? {
                    row["content"] = serde_json::from_str(s(&row, "content"))?;
                    result.push(row);
                }
            }
        }
        Ok(result)
    }
    pub fn recent_evidence(&self, sid: &str) -> Result<Vec<Value>> {
        let mut result = self.rows(
            "SELECT e.*,es.activity_id,m.kind AS event_kind,m.version AS event_version,m.status AS event_status FROM events e JOIN sessions es ON es.session_id=e.session_id AND es.learner_id=e.learner_id JOIN education_event_meta m ON m.event_id=e.event_id AND m.learner_id=e.learner_id AND m.session_id=e.session_id WHERE e.session_id=? AND m.status='active' AND m.namespace=? ORDER BY e.rowid DESC LIMIT 12",
            params![sid,self.namespace],
        )?;
        for row in &mut result {
            row["content"] = serde_json::from_str(s(row, "content"))?;
        }
        result.reverse();
        Ok(result)
    }
    pub fn add_turn(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        role: &str,
        content: &str,
        status: &str,
        evidence: &[String],
    ) -> Result<String> {
        let session = self.session(sid)?;
        let id = uid("turn");
        self.db.execute(
            "INSERT INTO turns VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                s(&session, "learner_id"),
                sid,
                rid,
                generation,
                role,
                content,
                status,
                json!(evidence).to_string(),
                now()
            ],
        )?;
        Ok(id)
    }
    pub fn acknowledge(&self, sid: &str, tid: &str, status: &str) -> Result<()> {
        self.tx(|| {
            let session = self.session(sid)?;
            let turn = self
                .one(
                    "SELECT * FROM turns WHERE session_id=? AND turn_id=?",
                    params![sid, tid],
                )?
                .ok_or_else(|| anyhow!("回复不存在"))?;
            ensure!(
                turn["generation"] == session["generation"]
                    && s(&turn, "role") == "assistant"
                    && s(&turn, "status") != "cancelled",
                "回复已过期"
            );
            ensure!(
                [
                    "shown",
                    "speech_started",
                    "speech_completed",
                    "speech_interrupted",
                    "cancelled"
                ]
                .contains(&status),
                "投递状态无效"
            );
            if status == "shown"
                && [
                    "shown",
                    "speech_started",
                    "speech_completed",
                    "speech_interrupted",
                ]
                .contains(&s(&turn, "status"))
            {
                return Ok(());
            }
            self.db.execute(
                "UPDATE turns SET status=? WHERE turn_id=?",
                params![status, tid],
            )?;
            if status != "cancelled" {
                let mut state = session["state"].clone();
                state["last_delivered_hint"] = turn["content"].clone();
                state["last_delivered_turn_id"] = json!(tid);
                self.checkpoint(sid, &state)?;
            }
            Ok(())
        })
    }
    /// Audio receipt status for a turn already delivered as text. Replaying an
    /// older reply must not replace the teaching checkpoint or its revision.
    pub fn acknowledge_speech(&self, sid: &str, tid: &str, status: &str) -> Result<()> {
        ensure!(
            ["speech_started", "speech_completed", "speech_interrupted"].contains(&status),
            "播报状态无效"
        );
        self.tx(|| {
            let session = self.session(sid)?;
            let turn = self
                .one(
                    "SELECT * FROM turns WHERE session_id=? AND turn_id=?",
                    params![sid, tid],
                )?
                .ok_or_else(|| anyhow!("回复不存在"))?;
            ensure!(
                turn["generation"] == session["generation"] && s(&turn, "role") == "assistant",
                "回复已过期"
            );
            let previous = s(&turn, "status");
            ensure!(
                [
                    "shown",
                    "speech_started",
                    "speech_completed",
                    "speech_interrupted"
                ]
                .contains(&previous),
                "回复尚未显示或已过期"
            );
            ensure!(
                status == "speech_started" || previous == "speech_started" || previous == status,
                "播报尚未开始"
            );
            self.db.execute(
                "UPDATE turns SET status=? WHERE turn_id=?",
                params![status, tid],
            )?;
            Ok(())
        })
    }
    /// Compatibility view for the existing UI. Business decisions must not use
    /// this first page as if it were the learner's complete memory collection.
    pub fn memories(&self, lid: &str) -> Result<Vec<Value>> {
        Ok(self.memory_page(lid, None, 100)?.0)
    }
    /// Stable newest-first pagination. A cursor is scoped by the learner filter;
    /// new inserts between requests cannot shift or duplicate older pages.
    pub fn memory_page(
        &self,
        lid: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<(Vec<Value>, Option<i64>)> {
        ensure!((1..=100).contains(&limit), "记忆分页数量必须为1至100");
        self.learner(lid)?;
        let mut rows = match before {
            Some(cursor) => self.rows(
                "SELECT rowid AS page_cursor,* FROM memories WHERE learner_id=? AND status='active' AND rowid<? ORDER BY rowid DESC LIMIT ?",
                params![lid, cursor, (limit + 1) as i64],
            )?,
            None => self.rows(
                "SELECT rowid AS page_cursor,* FROM memories WHERE learner_id=? AND status='active' ORDER BY rowid DESC LIMIT ?",
                params![lid, (limit + 1) as i64],
            )?,
        };
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next = if has_more {
            rows.last().and_then(|row| row["page_cursor"].as_i64())
        } else {
            None
        };
        for row in &mut rows {
            row.as_object_mut().unwrap().remove("page_cursor");
        }
        Ok((Self::decode_memories(rows)?, next))
    }
    fn active_memories(&self, lid: &str) -> Result<Vec<Value>> {
        self.learner(lid)?;
        // Intentionally independent of the UI page size. Retrieval, duplicate
        // detection and source correction must still see record 101 and beyond.
        let memories = Self::decode_memories(self.rows(
            "SELECT m.* FROM memories m WHERE m.learner_id=? AND m.status='active'
             AND EXISTS(SELECT 1 FROM memory_source_links l WHERE l.memory_id=m.memory_id)
             AND NOT EXISTS(SELECT 1 FROM memory_source_links l WHERE l.memory_id=m.memory_id AND
                (l.learner_id!=m.learner_id OR l.namespace!=? OR l.status!='active' OR NOT EXISTS(
                    SELECT 1 FROM education_event_meta em JOIN events e ON e.event_id=em.event_id
                    AND e.learner_id=em.learner_id AND e.session_id=em.session_id
                    JOIN sessions es ON es.session_id=e.session_id AND es.learner_id=e.learner_id
                    WHERE em.event_id=l.event_id AND em.learner_id=l.learner_id AND em.namespace=l.namespace
                    AND em.version=l.event_version AND em.status='active')))
             ORDER BY m.rowid DESC",
            params![lid,self.namespace],
        )?)?;
        let mut eligible = Vec::new();
        for memory in memories {
            let sources = self.source_events(lid, &strings(&memory["source_event_ids"]))?;
            if self.comparison_context_for_sources(lid, &sources)?["status"] != "unavailable" {
                eligible.push(memory);
            }
        }
        Ok(eligible)
    }
    fn decode_memories(mut rows: Vec<Value>) -> Result<Vec<Value>> {
        for row in &mut rows {
            for key in ["source_event_ids", "supersedes"] {
                row[key] = serde_json::from_str(s(row, key))?;
            }
        }
        Ok(rows)
    }
    /// Broad historical/lab search. Teaching context must use recall_for_session.
    pub fn recall(&self, lid: &str, query: &str, top_k: usize) -> Result<Vec<Value>> {
        self.recall_in_activity(lid, query, top_k, None)
    }
    /// Activity filtering happens before ranking and the whole-record budget.
    pub fn recall_for_session(&self, sid: &str, query: &str, top_k: usize) -> Result<Vec<Value>> {
        self.tx(|| {
            let (lid, activities) = self.session_memory_activities(sid)?;
            self.recall_in_activity(&lid, query, top_k, Some(&activities))
        })
    }
    fn recall_in_activity(
        &self,
        lid: &str,
        query: &str,
        top_k: usize,
        activity: Option<&HashSet<String>>,
    ) -> Result<Vec<Value>> {
        fn grams(text: &str) -> HashSet<String> {
            let chars: Vec<_> = text
                .to_lowercase()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            chars.windows(2).map(|w| w.iter().collect()).collect()
        }
        // Activity relevance must not depend on a learner's wording containing
        // the activity name. Match specific names, not generic "活动" or loose
        // English bigrams, and keep the original memory content untouched.
        let activity_query = query.to_lowercase();
        let activity_match = activity_query.contains("小车")
            || Regex::new(r"(?i)\b(?:modular[\s_-]+rover|rover)\b")?.is_match(&activity_query);
        let query = grams(query);
        let mut scored = vec![];
        for memory in self.active_memories(lid)? {
            if s(&memory, "activity_scope") != "modular_rover" {
                continue;
            }
            if let Some(activity) = activity {
                if !self.memory_in_activity(lid, &memory, activity)? {
                    continue;
                }
            }
            let overlap = query.intersection(&grams(s(&memory, "content"))).count();
            if s(&memory, "kind") == "help_preference" || overlap > 0 || activity_match {
                scored.push((
                    overlap
                        + if s(&memory, "kind") == "pending_experiment" {
                            2
                        } else {
                            0
                        },
                    s(&memory, "updated_at").to_owned(),
                    memory,
                ));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        let mut result = vec![];
        let mut budget = 2400usize;
        for (_, _, memory) in scored {
            if result.len() >= top_k.min(5) {
                break;
            }
            let size = s(&memory, "content").chars().count();
            if size <= budget {
                budget -= size;
                result.push(memory);
            }
        }
        Ok(result)
    }
    pub fn add_memory(&self, lid: &str, candidate: &Value, epoch: i64) -> Result<Option<String>> {
        self.add_memory_inner(lid, candidate, epoch, false)
    }
    // Only consolidate_and_finish may request the already-validated derived
    // pending form. No candidate field, prefix or public parameter enables it.
    fn add_memory_inner(
        &self,
        lid: &str,
        candidate: &Value,
        epoch: i64,
        validated_finish: bool,
    ) -> Result<Option<String>> {
        self.tx(||{
        if self.learner(lid)?["epoch"].as_i64()!=Some(epoch){return Ok(None)}
        let mut ids=strings(&candidate["source_event_ids"]);if ids.is_empty()||ids.len()>8{return Ok(None)}
        let mut record_epoch=epoch;
        let sources=self.source_events(lid,&ids)?;let unique:HashSet<_>=ids.iter().collect();if sources.len()!=unique.len(){return Ok(None)}
        if !self.candidate_versions_match(candidate, &sources)? { return Ok(None); }
        if s(candidate,"kind")=="reported_progress" && sources.iter().any(|row|s(row,"event_kind")=="plan") {return Ok(None)}
        if s(candidate,"kind")=="pending_experiment" && sources.iter().any(|row|s(row,"event_kind")=="plan" && row["content"]["accepted"]!=true) {return Ok(None)}
        if sources.iter().any(|r|!["learner_report","operator_confirmed","simulated"].contains(&s(r,"source_type"))||(self.namespace=="real"&&s(r,"source_type")=="simulated")){return Ok(None)}
        if s(candidate,"scope")=="current_session"{return Ok(None)}
        if s(candidate,"scope")=="current_activity" && s(candidate,"kind")!="correction"
            && !self.sources_share_activity(lid,&ids)? { return Ok(None); }
        ensure!(["reported_progress","help_preference","pending_experiment","correction"].contains(&s(candidate,"kind")),"记忆类型无效");
        ensure!(s(candidate,"content").chars().count()<=800&&!s(candidate,"content").is_empty(),"记忆内容无效");
        if s(candidate,"kind")=="pending_experiment" && (s(candidate,"scope")!="current_activity"
            || (!validated_finish && !crate::pending_step::supported_memory(&candidate["content"],&sources))) {return Ok(None)}
        if s(candidate,"kind")=="help_preference" {
            if s(candidate,"scope")!="cross_session_explicit" || !self.explicit_persistent_preference_sources(&sources)? { return Ok(None) }
        }
        for existing in self.active_memories(lid)? {
            if s(&existing,"content")==s(candidate,"content") && s(&existing,"kind")==s(candidate,"kind") && s(&existing,"scope")==s(candidate,"scope") && strings(&existing["source_event_ids"]).into_iter().collect::<HashSet<_>>()==ids.iter().cloned().collect::<HashSet<_>>() {return Ok(Some(s(&existing,"memory_id").to_owned()))}
        }
        let mut supersedes=vec![];
        if s(candidate,"kind")=="correction" {
            let mut ordered=vec![];for id in &ids {if let Some(row)=self.one("SELECT rowid AS source_order,* FROM events WHERE learner_id=? AND event_id=?",params![lid,id])? {ordered.push(row);}}
            if ordered.len()<2{return Ok(None)}ordered.sort_by_key(|r|r["source_order"].as_i64().unwrap_or_default());let current=ordered.last().unwrap();
            let content:Value=serde_json::from_str(s(current,"content"))?;let raw=format!("{} {}",s(&content,"text"),s(&content,"report"));
            let explicit=Regex::new(r"(?i)刚才.{0,12}说错|我.{0,8}说错|更正[:：]|纠正[:：]|更正一下|纠正一下|correction\s*:|i was wrong|i misspoke|that was incorrect")?.is_match(&raw)||(s(current,"source_type")=="operator_confirmed"&&!s(&content,"correction").is_empty());
            let latest=self.one("SELECT event_id FROM events WHERE learner_id=? AND session_id=? ORDER BY rowid DESC LIMIT 1",params![lid,s(current,"session_id")])?;
            if !explicit||latest.as_ref().map(|r|s(r,"event_id"))!=Some(s(current,"event_id")){return Ok(None)}
            let older:HashSet<_>=ordered[..ordered.len()-1].iter().map(|r|s(r,"event_id").to_owned()).collect();
            for memory in self.active_memories(lid)? {if s(&memory,"activity_scope")=="modular_rover"&&strings(&memory["source_event_ids"]).iter().any(|id|older.contains(id)){supersedes.push(s(&memory,"memory_id").to_owned());}}
            if supersedes.is_empty(){return Ok(None)}
            if !self.memories_share_activity(lid,&supersedes)? { return Ok(None); }
            let current_id=s(current,"event_id").to_owned();
            let (new_epoch,affected_memories)=self.invalidate_correction_sources(lid,&older.into_iter().collect::<Vec<_>>(),&[current_id.clone()])?;
            supersedes=affected_memories;
            record_epoch=new_epoch;
            // The obsolete evidence is invalidated, while the explicit current
            // correction remains independently retrievable and source-graded.
            ids=vec![current_id];
        } else if s(candidate,"kind")=="help_preference" {for row in self.rows("SELECT memory_id FROM memories WHERE learner_id=? AND status='active' AND kind='help_preference'",[lid])?{supersedes.push(s(&row,"memory_id").to_owned());}}
        for old in &supersedes {self.db.execute("UPDATE memories SET status='superseded' WHERE learner_id=? AND memory_id=?",params![lid,old])?;}
        let source=if self.namespace=="simulator"{"simulated"}else if sources.iter().any(|r|ids.iter().any(|id|id==s(r,"event_id"))&&s(r,"source_type")=="learner_report"){"learner_report"}else{"operator_confirmed"};
        let id=uid("memory");let timestamp=now();self.db.execute("INSERT INTO memories VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",params![id,lid,"modular_rover",s(candidate,"kind"),s(candidate,"content"),json!(ids).to_string(),source,s(candidate,"scope"),timestamp,timestamp,"active",json!(supersedes).to_string(),record_epoch])?;self.link_memory_sources(&id)?;self.link_memory_activity(&id)?;Ok(Some(id))})
    }
    pub fn forget(&self, lid: &str, mid: Option<&str>) -> Result<i64> {
        let epoch=self.tx(||{
            if let Some(mid)=mid {
                let memory=self.one("SELECT source_event_ids FROM memories WHERE learner_id=? AND memory_id=?",params![lid,mid])?
                    .ok_or_else(||anyhow!("记忆不存在"))?;
                let sources=strings(&serde_json::from_str::<Value>(s(&memory,"source_event_ids"))?);
                // Unsummarized events are still independent learner records.
                // Invalidate only the selected provenance closure, then physically
                // delete its memory text rather than retaining superseded rows.
                let (epoch,mut affected)=self.invalidate_sources(lid,&sources,&[],"deleted")?;
                affected.push(mid.to_owned());
                for id in affected {self.db.execute("DELETE FROM memories WHERE learner_id=? AND memory_id=?",params![lid,id])?;}
                return Ok(epoch);
            }
            let epoch=self.learner(lid)?["epoch"].as_i64().unwrap_or_default()+1;
            self.db.execute("DELETE FROM memories WHERE learner_id=?",[lid])?;
            self.db.execute("UPDATE learners SET epoch=? WHERE learner_id=?",params![epoch,lid])?;self.db.execute("INSERT INTO tombstones VALUES(?,?,?,?)",params![uid("deleted"),lid,epoch,now()])?;
            self.db.execute("DELETE FROM turns WHERE learner_id=?",[lid])?;
            self.db.execute("DELETE FROM events WHERE learner_id=?",[lid])?;
            self.db.execute("DELETE FROM provider_runs WHERE learner_id=?",[lid])?;
            self.db.execute("UPDATE sessions SET state=?,generation=generation+1,status=CASE WHEN status='finished' THEN 'finished' ELSE 'idle' END WHERE learner_id=?",params![empty_state().to_string(),lid])?;
            self.db.execute("DELETE FROM education_event_meta WHERE learner_id=? AND event_id NOT IN (SELECT event_id FROM events WHERE learner_id=?)",params![lid,lid])?;
            Ok(epoch)})?;
        if self.db.is_autocommit() {
            self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        }
        Ok(epoch)
    }
    fn invalidate_correction_sources(
        &self,
        lid: &str,
        initial: &[String],
        protected: &[String],
    ) -> Result<(i64, Vec<String>)> {
        self.invalidate_sources(lid, initial, protected, "corrected")
    }
    fn invalidate_sources(
        &self,
        lid: &str,
        initial: &[String],
        protected: &[String],
        tombstone_prefix: &str,
    ) -> Result<(i64, Vec<String>)> {
        self.tx(|| {
            let all=self.rows("SELECT * FROM memories WHERE learner_id=?",[lid])?;
            let mut affected:HashSet<String>=initial.iter().filter(|id|!protected.contains(id)).cloned().collect();
            // Invalidate the provenance closure, not the learner's whole activity.
            loop {
                let before=affected.len();
                for memory in &all {
                    let ids=strings(&serde_json::from_str::<Value>(s(memory,"source_event_ids"))?);
                    if ids.iter().any(|id|affected.contains(id)) { affected.extend(ids.into_iter().filter(|id|!protected.contains(id))); }
                }
                if before==affected.len(){break}
            }
            let mut superseded=vec![];
            for memory in &all {
                let ids=strings(&serde_json::from_str::<Value>(s(memory,"source_event_ids"))?);
                if ids.iter().any(|id|affected.contains(id)) {
                    self.db.execute("UPDATE memories SET status='superseded' WHERE learner_id=? AND memory_id=?",params![lid,s(memory,"memory_id")])?;
                    superseded.push(s(memory,"memory_id").to_owned());
                }
            }
            let epoch=self.learner(lid)?["epoch"].as_i64().unwrap_or_default()+1;
            self.db.execute("UPDATE learners SET epoch=? WHERE learner_id=?",params![epoch,lid])?;
            self.db.execute("INSERT INTO tombstones VALUES(?,?,?,?)",params![uid(tombstone_prefix),lid,epoch,now()])?;
            let mut invalid_events=vec![];
            for ids in affected.iter().cloned().collect::<Vec<_>>().chunks(100) {invalid_events.extend(self.source_events(lid,ids)?);}
            let invalid_texts:HashSet<String>=invalid_events.iter().flat_map(|row|[s(&row["content"],"text").to_owned(),s(&row["content"],"report").to_owned()]).filter(|text|!text.is_empty()).collect();
            let mut invalid_turns=HashSet::new();
            let mut corrected_turns=vec![];
            for turn in self.rows("SELECT * FROM turns WHERE learner_id=?",[lid])? {
                let ids=strings(&serde_json::from_str::<Value>(s(&turn,"evidence"))?);
                if ids.iter().any(|id|affected.contains(id)) {
                    if ids.iter().any(|id|protected.contains(id)) {
                        let retained:Vec<_>=ids.iter().filter(|id|!affected.contains(*id)).collect();
                        self.db.execute("UPDATE turns SET evidence=? WHERE learner_id=? AND turn_id=?",params![json!(retained).to_string(),lid,s(&turn,"turn_id")])?;
                        corrected_turns.push(s(&turn,"turn_id").to_owned());
                    } else {
                        invalid_turns.insert(s(&turn,"turn_id").to_owned());
                        self.db.execute("DELETE FROM turns WHERE learner_id=? AND turn_id=?",params![lid,s(&turn,"turn_id")])?;
                    }
                } else if ids.iter().any(|id|protected.contains(id)) {
                    corrected_turns.push(s(&turn,"turn_id").to_owned());
                }
            }
            for row in self.rows("SELECT session_id,state FROM sessions WHERE learner_id=?",[lid])? {
                let sid=s(&row,"session_id");
                let mut state:Value=serde_json::from_str(s(&row,"state"))?;
                let defaults=empty_state();
                for field in ["goal","stage","reported_result","pending_step","modules"] {
                    if strings(&state["field_evidence"][field]["source_event_ids"]).iter().any(|id|affected.contains(id)) {
                        state[field]=defaults[field].clone();
                        state.as_object_mut().unwrap().remove(&format!("{field}_source"));
                        state["field_evidence"].as_object_mut().map(|fields|fields.remove(field));
                        if field=="modules" {state["modules_source"]=json!("unknown");state["requires_inventory_confirmation"]=json!(true);}
                    }
                }
                if let Some(attempts)=state.get_mut("attempts").and_then(Value::as_array_mut) {attempts.retain(|attempt|!strings(&attempt["source_event_ids"]).iter().any(|id|affected.contains(id)));}
                if invalid_texts.contains(s(&state,"latest_input")) {state["latest_input"]=json!("");}
                if invalid_turns.contains(s(&state,"last_delivered_turn_id")) || self.one("SELECT turn_id FROM turns WHERE learner_id=? AND session_id=? AND role='assistant' AND content=? AND status IN('shown','speech_started','speech_completed','speech_interrupted') LIMIT 1",params![lid,sid,s(&state,"last_delivered_hint")])?.is_none() {
                    state["last_delivered_hint"]=json!("");
                    state.as_object_mut().unwrap().remove("last_delivered_turn_id");
                }
                if strings(&state["session_preferences"]["source_event_ids"]).iter().any(|id|affected.contains(id)) {state["session_preferences"]=json!({});}
                if let Some(ids)=state.get_mut("finish_memory_ids").and_then(Value::as_array_mut) {ids.retain(|id|!superseded.iter().any(|old|id.as_str()==Some(old.as_str())));}
                self.checkpoint(sid,&state)?;
            }
            for event in &affected {self.db.execute("DELETE FROM events WHERE learner_id=? AND event_id=?",params![lid,event])?;}
            self.db.execute("UPDATE sessions SET generation=generation+1,status=CASE WHEN status='finished' THEN 'finished' ELSE 'idle' END WHERE learner_id=?",[lid])?;
            for tid in corrected_turns {self.db.execute("UPDATE turns SET generation=(SELECT generation FROM sessions WHERE sessions.session_id=turns.session_id) WHERE learner_id=? AND turn_id=?",params![lid,tid])?;}
            self.db.execute("UPDATE turns SET status='cancelled' WHERE learner_id=? AND status IN('generated','generating')",[lid])?;
            self.db.execute("UPDATE turns SET status='speech_interrupted' WHERE learner_id=? AND status='speech_started'",[lid])?;
            self.db.execute("UPDATE provider_runs SET status='cancelled' WHERE learner_id=? AND status='running'",[lid])?;
            Ok((epoch,superseded))
        })
    }
    pub fn correct_memory(&self, lid: &str, mid: &str, content: &str) -> Result<String> {
        let id = self.tx(|| {
            ensure!(
                !content.trim().is_empty() && content.chars().count() <= 800,
                "更正内容长度无效"
            );
            let original = self
                .one(
                    "SELECT * FROM memories WHERE learner_id=? AND memory_id=? AND status='active'",
                    params![lid, mid],
                )?
                .ok_or_else(|| anyhow!("记忆不存在"))?;
            let ids = strings(&serde_json::from_str::<Value>(s(
                &original,
                "source_event_ids",
            ))?);
            let (epoch, superseded) = self.invalidate_correction_sources(lid, &ids, &[])?;
            // A memory edit is not a new learning activity. Reuse the newest
            // existing activity even if it has already ended.
            let session = match self.one(
                "SELECT session_id FROM sessions WHERE learner_id=? ORDER BY rowid DESC LIMIT 1",
                [lid],
            )? {
                Some(row) => row,
                None => self.create_session(lid)?,
            };
            let preference = s(&original, "kind") == "help_preference"
                && s(&original, "scope") == "cross_session_explicit";
            let mut corrected =
                json!({"correction":content,"corrected_memory_id":mid,"set_preference":preference});
            if preference {
                corrected["explicit_preference"] = json!(true);
                corrected["preference_scope"] = json!("persistent");
            }
            let event = self.event(
                lid,
                s(&session, "session_id"),
                "operator_confirmed",
                &corrected,
            )?;
            if preference {
                self.db.execute(
                    "UPDATE education_event_meta SET kind='teacher_setting' WHERE event_id=?",
                    [&event],
                )?;
            }
            let id = uid("memory");
            let timestamp = now();
            let source = if self.namespace == "simulator" {
                "simulated"
            } else {
                "operator_confirmed"
            };
            self.db.execute(
                "INSERT INTO memories VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    id,
                    lid,
                    "modular_rover",
                    s(&original, "kind"),
                    content,
                    json!([event]).to_string(),
                    source,
                    s(&original, "scope"),
                    timestamp,
                    timestamp,
                    "active",
                    json!(superseded).to_string(),
                    epoch
                ],
            )?;
            self.link_memory_sources(&id)?;
            self.link_memory_activity(&id)?;
            Ok(id)
        })?;
        self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(id)
    }
    pub fn settings(&self, lid: &str, language: &str, style: &str) -> Result<()> {
        self.learner(lid)?;
        self.db.execute(
            "UPDATE learners SET language=?,help_style=? WHERE learner_id=?",
            params![language, style, lid],
        )?;
        Ok(())
    }
    pub fn start_run(&self, rid: &str, lid: &str, sid: &str, chars: usize) -> Result<()> {
        self.db.execute(
            "INSERT INTO provider_runs VALUES(?,?,?,'running',?,NULL,?,NULL)",
            params![rid, lid, sid, now(), chars as i64],
        )?;
        Ok(())
    }
    pub fn end_run(&self, rid: &str, status: &str, latency_ms: u64) -> Result<()> {
        self.db.execute(
            "UPDATE provider_runs SET status=?,latency_ms=? WHERE request_id=?",
            params![status, latency_ms as i64, rid],
        )?;
        Ok(())
    }
    pub fn set_run_payload_chars(&self, rid: &str, chars: usize) -> Result<()> {
        ensure!(
            self.db.execute(
                "UPDATE provider_runs SET payload_chars=? WHERE request_id=? AND status='running'",
                params![chars as i64, rid],
            )? == 1,
            "教学请求已失效"
        );
        Ok(())
    }
    pub fn run_metrics(&self) -> Result<Value> {
        let mut result = json!({});
        for row in self.rows(
            "SELECT status,COUNT(*) AS count FROM provider_runs GROUP BY status",
            [],
        )? {
            result[s(&row, "status")] = row["count"].clone();
        }
        Ok(result)
    }
    pub fn checkpoint_wal(&self) -> Result<()> {
        self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(content: &str, ids: &[String], kind: &str) -> Value {
        json!({"kind":kind,"content":content,"scope":"current_activity","source_event_ids":ids})
    }
    #[test]
    fn failed_commit_rolls_back_and_leaves_connection_usable() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("commit-error.db"), "real")?;
        store.db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(parent_id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);")?;
        ensure!(
            store
                .tx(|| {
                    store.db.execute("INSERT INTO child VALUES(123)", [])?;
                    Ok(())
                })
                .is_err()
        );
        ensure!(store.db.is_autocommit());
        ensure!(
            store
                .one("SELECT COUNT(*) AS count FROM child", [])?
                .unwrap()["count"]
                == 0
        );
        store.tx(|| {
            store.db.execute("INSERT INTO parent VALUES(123)", [])?;
            store.db.execute("INSERT INTO child VALUES(123)", [])?;
            Ok(())
        })?;
        ensure!(
            store
                .one("SELECT COUNT(*) AS count FROM child", [])?
                .unwrap()["count"]
                == 1
        );
        Ok(())
    }
    #[test]
    fn session_history_queries_use_indexes_without_sorting_all_history() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("indexed.db"), "real")?;
        for (sql, index) in [
            (
                "SELECT * FROM turns WHERE session_id=? ORDER BY rowid DESC LIMIT 24",
                "turns_session",
            ),
            (
                "SELECT * FROM events WHERE session_id=? ORDER BY rowid DESC LIMIT 12",
                "events_session",
            ),
            (
                "SELECT session_id FROM sessions WHERE learner_id=? AND status!='finished' ORDER BY rowid DESC LIMIT 1",
                "sessions_owner",
            ),
        ] {
            let plan = store.rows(&format!("EXPLAIN QUERY PLAN {sql}"), ["synthetic-id"])?;
            ensure!(
                plan.iter().any(|row| s(row, "detail").contains(index)),
                "missing indexed search for {sql}: {plan:?}"
            );
            ensure!(
                plan.iter()
                    .all(|row| !s(row, "detail").contains("TEMP B-TREE"))
            );
        }
        Ok(())
    }
    #[test]
    fn isolation_deletion_correction_and_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("real.db");
        let store = Store::open(&path, "real")?;
        let a = store.create_learner("A", "zh", "one_step")?;
        let b = store.create_learner("B", "zh", "one_step")?;
        let lid = s(&a, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let old = store.event(lid, sid, "learner_report", &json!({"text":"停车成功"}))?;
        let mid = store
            .add_memory(
                lid,
                &candidate("据报告停车成功", &[old.clone()], "reported_progress"),
                0,
            )?
            .unwrap();
        ensure!(
            store
                .add_memory(
                    s(&b, "learner_id"),
                    &candidate("不得串记忆", &[old.clone()], "reported_progress"),
                    0
                )?
                .is_none()
        );
        let other = store.event(
            lid,
            sid,
            "learner_report",
            &json!({"text":"下次移动墙测试停车距离"}),
        )?;
        let keep = store
            .add_memory(
                lid,
                &candidate("下次移动墙测试停车距离", &[other], "pending_experiment"),
                0,
            )?
            .unwrap();
        let correction = store.event(
            lid,
            sid,
            "learner_report",
            &json!({"text":"刚才说错了，停车尚未成功"}),
        )?;
        let new = store
            .add_memory(
                lid,
                &candidate("学习者更正停车尚未成功", &[old, correction], "correction"),
                0,
            )?
            .unwrap();
        ensure!(
            store
                .memories(lid)?
                .iter()
                .all(|m| s(m, "memory_id") != mid)
        );
        ensure!(
            store
                .memories(lid)?
                .iter()
                .any(|m| s(m, "memory_id") == keep)
        );
        store.forget(lid, Some(&new))?;
        ensure!(store.memories(lid)?.len() == 1);
        ensure!(
            store
                .add_memory(
                    lid,
                    &candidate("旧整理不得写回", &[], "reported_progress"),
                    0
                )?
                .is_none()
        );
        drop(store);
        let restored = Store::open(&path, "real")?;
        ensure!(restored.recall(lid, "移动墙停车", 5)?.len() == 1);
        ensure!(restored.memories(s(&b, "learner_id"))?.is_empty());
        Ok(())
    }
    #[test]
    fn simulation_and_preferences_keep_scope() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("sim.db"), "simulator")?;
        let learner = store.create_learner("sim", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let event = store.event(
            lid,
            s(&session, "session_id"),
            "operator_confirmed",
            &json!({"text":"这次一步一步"}),
        )?;
        let mut value = candidate("一次一步", &[event.clone()], "help_preference");
        value["scope"] = json!("cross_session_explicit");
        ensure!(store.add_memory(lid, &value, 0)?.is_none());
        let mid = store
            .add_memory(
                lid,
                &candidate("模拟停车成功", &[event], "reported_progress"),
                0,
            )?
            .unwrap();
        ensure!(store.memories(lid)?[0]["source_type"] == "simulated");
        ensure!(!mid.is_empty());
        Ok(())
    }
    #[test]
    fn speech_interruption_and_failed_correction_preserve_delivered_state() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.db");
        let store = Store::open(&path, "real")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let tid = store.add_turn(
            sid,
            "r",
            0,
            "assistant",
            "actually displayed",
            "generated",
            &[],
        )?;
        store.acknowledge(sid, &tid, "shown")?;
        store.acknowledge(sid, &tid, "speech_started")?;
        store.bump(sid, "idle")?;
        ensure!(store.session(sid)?["turns"][0]["status"] == "speech_interrupted");
        ensure!(store.session(sid)?["state"]["last_delivered_hint"] == "actually displayed");
        let eid = store.event(lid, sid, "learner_report", &json!({"text":"停车成功"}))?;
        let mid = store
            .add_memory(lid, &candidate("停车成功", &[eid], "reported_progress"), 0)?
            .unwrap();
        ensure!(store.correct_memory(lid, &mid, &"x".repeat(801)).is_err());
        ensure!(
            store.learner(lid)?["epoch"] == 0
                && store
                    .memories(lid)?
                    .iter()
                    .any(|m| s(m, "memory_id") == mid)
        );
        Ok(())
    }
    #[test]
    fn finish_keeps_full_unicode_report_and_does_not_promote_suggestions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("finish.db"), "real")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let raw = format!("{}第二次据报告成功，尚未移动墙🧱", "中英🙂".repeat(240));
        let eid = store.event(lid, sid, "operator_confirmed", &json!({"report":raw}))?;
        let mut state = empty_state();
        state["reported_result"] = json!("模型压缩后的进展不能替换报告原文");
        state["field_evidence"]["reported_result"] = json!({"source_event_ids":[eid]});
        state["pending_step"] = json!("把停车距离改成20厘米");
        state["field_evidence"]["pending_step"] = json!({"source_event_ids":[eid]});
        store.checkpoint(sid, &state)?;
        let result = store.consolidate_and_finish(sid)?;
        let memories = store.memories(lid)?;
        ensure!(memories.len() == 2);
        let mut ordered = memories.clone();
        ordered.sort_by_key(|m| {
            if s(m, "content").contains("（1/") {
                0
            } else {
                1
            }
        });
        let reconstructed: String = ordered
            .iter()
            .map(|memory| s(memory, "content").split_once('：').unwrap().1)
            .collect();
        ensure!(
            reconstructed == raw,
            "whole Unicode report must survive without truncation"
        );
        ensure!(memories.iter().all(|m| s(m, "kind") == "reported_progress"
            && m["source_type"] == "operator_confirmed"
            && s(m, "content").chars().count() <= 800));
        let generation = store.session(sid)?["generation"].clone();
        ensure!(store.consolidate_and_finish(sid)?["already_finished"] == true);
        ensure!(store.session(sid)?["generation"] == generation && store.memories(lid)?.len() == 2);
        ensure!(result["consolidation"]["omitted_sources"] == 1);
        store.bump(sid, "idle")?;
        store.set_status(sid, "error")?;
        ensure!(store.session(sid)?["status"] == "finished");
        Ok(())
    }
    #[test]
    fn finish_rejects_historical_sources_and_accepts_explicit_pending_step() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("historical.db"), "simulator")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let old = store.create_session(lid)?;
        let old_eid = store.event(
            lid,
            s(&old, "session_id"),
            "learner_report",
            &json!({"text":"过去停车成功"}),
        )?;
        let current = store.create_session(lid)?;
        let sid = s(&current, "session_id");
        let eid = store.event(
            lid,
            sid,
            "learner_report",
            &json!({"text":"下次移动墙后再测试"}),
        )?;
        let mut state = empty_state();
        state["reported_result"] = json!("过去停车成功");
        state["field_evidence"]["reported_result"] = json!({"source_event_ids":[old_eid]});
        state["pending_step"] = json!("下次移动墙后再测试");
        state["field_evidence"]["pending_step"] = json!({"source_event_ids":[eid]});
        state["modules"] = json!(["distance"]);
        state["field_evidence"]["modules"] = json!({"source_event_ids":[old_eid]});
        store.checkpoint(sid, &state)?;
        store.consolidate_and_finish(sid)?;
        let memories = store.memories(lid)?;
        ensure!(
            memories.len() == 1
                && memories[0]["kind"] == "pending_experiment"
                && memories[0]["source_type"] == "simulated"
        );
        let next = store.create_session(lid)?;
        ensure!(next["state"]["modules"].is_null());
        ensure!(store.latest_session(lid)?.unwrap()["session_id"] == next["session_id"]);
        Ok(())
    }
    #[test]
    fn correction_invalidates_only_source_linked_fast_state_and_stale_writes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("correction.db"), "real")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let old = store.event(lid, sid, "learner_report", &json!({"text":"停车成功"}))?;
        let valid = store.event(
            lid,
            sid,
            "operator_confirmed",
            &json!({"modules":["control","motor"],"text":"今天要先测试电机"}),
        )?;
        let mid = store
            .add_memory(
                lid,
                &candidate("旧停车报告", &[old.clone()], "reported_progress"),
                0,
            )?
            .unwrap();
        let derived = store
            .add_memory(
                lid,
                &candidate("旧报告衍生摘要", &[old.clone()], "reported_progress"),
                0,
            )?
            .unwrap();
        let mut state = empty_state();
        state["modules"] = json!(["control", "motor"]);
        state["modules_source"] = json!("operator_confirmed");
        state["requires_inventory_confirmation"] = json!(false);
        state["field_evidence"]["modules"] = json!({"source_event_ids":[valid]});
        state["goal"] = json!("今天要先测试电机");
        state["field_evidence"]["goal"] = json!({"source_event_ids":[valid]});
        state["reported_result"] = json!("停车成功");
        state["field_evidence"]["reported_result"] = json!({"source_event_ids":[old]});
        state["attempts"] = json!([{"report":"停车成功","source_event_ids":[old]}]);
        store.checkpoint(sid, &state)?;
        let old_turn = store.add_turn(
            sid,
            "r",
            0,
            "assistant",
            "旧事实提示",
            "shown",
            &[old.clone()],
        )?;
        store.acknowledge(sid, &old_turn, "speech_completed")?;
        store.correct_memory(lid, &mid, "教师更正：停车尚未成功")?;
        let session = store.session(sid)?;
        ensure!(
            session["state"]["modules"] == json!(["control", "motor"])
                && session["state"]["goal"] == "今天要先测试电机"
        );
        ensure!(
            session["state"]["reported_result"] == ""
                && session["state"]["attempts"] == json!([])
                && session["state"]["last_delivered_hint"] == ""
        );
        ensure!(
            store.source_events(lid, &[old.clone()])?.is_empty()
                && store.source_events(lid, &[valid])?.len() == 1
        );
        ensure!(
            store
                .memories(lid)?
                .iter()
                .all(|m| s(m, "memory_id") != mid && s(m, "memory_id") != derived)
        );
        ensure!(
            store
                .add_memory(lid, &candidate("不得写回", &[old], "reported_progress"), 0)?
                .is_none()
        );
        ensure!(session["turns"].as_array().unwrap().is_empty());
        Ok(())
    }
    #[test]
    fn activity_name_recalls_chinese_correction_without_rewriting_original_content() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("scope-recall.db"), "simulator")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let event = store.event(
            lid,
            s(&session, "session_id"),
            "learner_report",
            &json!({"report":"上次第二次试验的间隙很大"}),
        )?;
        let old = store
            .add_memory(
                lid,
                &candidate("上次第二次试验的间隙很大", &[event], "reported_progress"),
                0,
            )?
            .unwrap();
        let raw = "教师更正：上次第二次试验的间隙记录有误；移动墙实验仍未做。";
        let corrected = store.correct_memory(lid, &old, raw)?;
        for query in ["小车活动", "小车 停车 墙 距离 灯", "modular rover"] {
            let recalled = store.recall(lid, query, 5)?;
            ensure!(
                recalled
                    .iter()
                    .any(|memory| memory["memory_id"] == corrected
                        && memory["content"] == raw
                        && memory["source_type"] == "simulated")
            );
        }
        ensure!(
            store.session(s(&session, "session_id"))?["recalled_memories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|memory| memory["memory_id"] == corrected)
        );
        Ok(())
    }
    #[test]
    fn activity_recall_does_not_match_unrelated_topic_owner_or_scope() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("scope-isolation.db"), "real")?;
        let a = store.create_learner("A", "zh", "one_step")?;
        let b = store.create_learner("B", "zh", "one_step")?;
        let lid = s(&a, "learner_id");
        let session = store.create_session(lid)?;
        let event = store.event(
            lid,
            s(&session, "session_id"),
            "operator_confirmed",
            &json!({"report":"第二次试验间隙记录有误"}),
        )?;
        store.add_memory(
            lid,
            &candidate(
                "第二次试验间隙记录有误",
                &[event.clone()],
                "reported_progress",
            ),
            0,
        )?;
        let foreign_scope = store
            .add_memory(
                lid,
                &candidate("其他课程的进展", &[event], "reported_progress"),
                0,
            )?
            .unwrap();
        store.db.execute(
            "UPDATE memories SET activity_scope='other_activity' WHERE memory_id=?",
            [foreign_scope],
        )?;
        ensure!(store.recall(lid, "介绍火星上的火山", 5)?.is_empty());
        ensure!(
            store.recall(lid, "story", 5)?.is_empty(),
            "English fragments must not match rover"
        );
        ensure!(
            store.recall(lid, "今天的活动", 5)?.is_empty(),
            "generic activity is not a rover query"
        );
        ensure!(store.recall(s(&b, "learner_id"), "小车活动", 5)?.is_empty());
        ensure!(store.recall(lid, "小车活动", 5)?.len() == 1);
        ensure!(store.recall(lid, "其他课程的进展", 5)?.is_empty());
        Ok(())
    }
}

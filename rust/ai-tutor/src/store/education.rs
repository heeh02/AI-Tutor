//! Additive provenance and compare-and-swap support for educational memory.
//! Legacy event text and memory text keep their original single storage owner.
use super::*;

impl Store {
    pub(super) fn migrate_education(&self) -> Result<()> {
        self.db.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS education_event_meta(
                event_id TEXT PRIMARY KEY, learner_id TEXT NOT NULL, session_id TEXT NOT NULL,
                namespace TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'legacy_report',
                version INTEGER NOT NULL DEFAULT 1 CHECK(version>=1),
                status TEXT NOT NULL DEFAULT 'active' CHECK(status IN('active','invalidated')),
                supersedes_event_id TEXT
            );
            CREATE TABLE IF NOT EXISTS memory_source_links(
                memory_id TEXT NOT NULL, event_id TEXT NOT NULL, learner_id TEXT NOT NULL,
                namespace TEXT NOT NULL, event_version INTEGER NOT NULL,
                status TEXT NOT NULL CHECK(status IN('active','invalidated')),
                PRIMARY KEY(memory_id,event_id)
            );
            CREATE INDEX IF NOT EXISTS source_link_event ON memory_source_links(event_id,status);
            CREATE TABLE IF NOT EXISTS session_state_versions(
                session_id TEXT PRIMARY KEY, learner_id TEXT NOT NULL,
                namespace TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 0
            );
        ",
        )?;
        for table in [
            "education_event_meta",
            "memory_source_links",
            "session_state_versions",
        ] {
            let wrong: i64 = self.db.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE namespace!=?"),
                [&self.namespace],
                |row| row.get(0),
            )?;
            ensure!(wrong == 0, "数据库空间与来源元数据不一致");
        }
        self.db.execute(
            "INSERT OR IGNORE INTO education_event_meta(event_id,learner_id,session_id,namespace) SELECT event_id,learner_id,session_id,? FROM events",
            [&self.namespace],
        )?;
        self.db.execute(
            "INSERT OR IGNORE INTO session_state_versions(session_id,learner_id,namespace) SELECT session_id,learner_id,? FROM sessions",
            [&self.namespace],
        )?;
        // The namespace has already passed the real/simulator allow-list. These
        // triggers also cover legacy direct UPDATE statements such as forgetting.
        self.db.execute_batch(&format!("
            CREATE TRIGGER IF NOT EXISTS education_event_insert AFTER INSERT ON events BEGIN
                INSERT INTO education_event_meta(event_id,learner_id,session_id,namespace)
                VALUES(NEW.event_id,NEW.learner_id,NEW.session_id,'{}');
            END;
            CREATE TRIGGER IF NOT EXISTS education_event_immutable
            BEFORE UPDATE OF learner_id,session_id,source_type,content ON events BEGIN
                SELECT RAISE(ABORT,'event evidence is immutable; record an explicit correction');
            END;
            CREATE TRIGGER IF NOT EXISTS education_event_delete AFTER DELETE ON events BEGIN
                UPDATE education_event_meta SET status='invalidated' WHERE event_id=OLD.event_id;
                UPDATE memories SET status='invalidated' WHERE status='active' AND memory_id IN
                    (SELECT memory_id FROM memory_source_links WHERE event_id=OLD.event_id);
                UPDATE memory_source_links SET status='invalidated' WHERE event_id=OLD.event_id;
            END;
            CREATE TRIGGER IF NOT EXISTS education_memory_delete AFTER DELETE ON memories BEGIN
                DELETE FROM memory_source_links WHERE memory_id=OLD.memory_id;
            END;
            CREATE TRIGGER IF NOT EXISTS education_memory_status AFTER UPDATE OF status ON memories
            WHEN NEW.status!='active' BEGIN
                UPDATE memory_source_links SET status='invalidated' WHERE memory_id=NEW.memory_id;
            END;
            CREATE TRIGGER IF NOT EXISTS education_session_insert AFTER INSERT ON sessions BEGIN
                INSERT INTO session_state_versions(session_id,learner_id,namespace,revision)
                VALUES(NEW.session_id,NEW.learner_id,'{}',0);
            END;
            CREATE TRIGGER IF NOT EXISTS education_state_update AFTER UPDATE OF state ON sessions BEGIN
                UPDATE session_state_versions SET revision=revision+1 WHERE session_id=NEW.session_id;
            END;
            CREATE TRIGGER IF NOT EXISTS education_session_delete AFTER DELETE ON sessions BEGIN
                DELETE FROM session_state_versions WHERE session_id=OLD.session_id;
            END;
        ", self.namespace, self.namespace))?;
        // INSERT OR IGNORE is deliberate: reopening must never reactivate a link
        // previously invalidated by a correction or deletion.
        for memory in self.rows("SELECT memory_id FROM memories", [])? {
            self.link_memory_sources(s(&memory, "memory_id"))?;
        }
        Ok(())
    }

    pub(super) fn link_memory_sources(&self, mid: &str) -> Result<()> {
        let memory = self
            .one("SELECT * FROM memories WHERE memory_id=?", [mid])?
            .ok_or_else(|| anyhow!("记忆不存在"))?;
        let lid = s(&memory, "learner_id");
        for event_id in strings(&serde_json::from_str::<Value>(s(
            &memory,
            "source_event_ids",
        ))?) {
            let metadata = self.one(
                "SELECT m.*,CASE WHEN es.session_id IS NOT NULL THEN e.source_type END AS source_type FROM education_event_meta m LEFT JOIN events e ON e.event_id=m.event_id AND e.learner_id=m.learner_id AND e.session_id=m.session_id LEFT JOIN sessions es ON es.session_id=e.session_id AND es.learner_id=e.learner_id WHERE m.event_id=? AND m.learner_id=? AND m.namespace=?",
                params![event_id, lid, self.namespace],
            )?;
            let valid = metadata.as_ref().is_some_and(|row| {
                s(&memory, "status") == "active"
                    && s(row, "status") == "active"
                    && ["learner_report", "operator_confirmed", "simulated"]
                        .contains(&s(row, "source_type"))
                    && (self.namespace == "simulator" || s(row, "source_type") != "simulated")
            });
            let version = metadata
                .as_ref()
                .and_then(|row| row["version"].as_i64())
                .unwrap_or(1);
            self.db.execute(
                "INSERT OR IGNORE INTO memory_source_links VALUES(?,?,?,?,?,?)",
                params![
                    mid,
                    event_id,
                    lid,
                    self.namespace,
                    version,
                    if valid { "active" } else { "invalidated" }
                ],
            )?;
        }
        Ok(())
    }

    pub(super) fn candidate_versions_match(
        &self,
        candidate: &Value,
        sources: &[Value],
    ) -> Result<bool> {
        let Some(value) = candidate.get("source_event_versions") else {
            return Ok(true);
        };
        let versions = value
            .as_object()
            .ok_or_else(|| anyhow!("来源版本必须为对象"))?;
        Ok(versions.len() == sources.len()
            && sources.iter().all(|source| {
                versions.get(s(source, "event_id")).and_then(Value::as_i64)
                    == source["event_version"].as_i64()
                    && source["event_status"] == "active"
            }))
    }

    /// The caller supplies a known semantic type; legacy events are not
    /// retroactively assigned cognitive or personality labels.
    pub fn record_education_event(
        &self,
        lid: &str,
        sid: &str,
        source: &str,
        kind: &str,
        content: &Value,
    ) -> Result<Value> {
        ensure!(
            [
                "learner_statement",
                "learner_answer",
                "observation",
                "plan",
                "teacher_setting",
                "correction",
            ]
            .contains(&kind),
            "教育事件类型无效"
        );
        ensure!(content.is_object(), "教育事件内容必须为对象");
        ensure!(
            kind != "teacher_setting" || source == "operator_confirmed",
            "教学设置必须由教师确认"
        );
        self.tx(|| {
            let session = self.session(sid)?;
            ensure!(
                s(&session, "learner_id") == lid && s(&session, "status") != "finished",
                "教育事件活动已失效"
            );
            let event_id = self.event(lid, sid, source, content)?;
            self.db.execute(
                "UPDATE education_event_meta SET kind=? WHERE event_id=?",
                params![kind, event_id],
            )?;
            self.source_events(lid, &[event_id])?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("教育事件来源无效"))
        })
    }

    /// A correction creates a new event version/ID and invalidates the old
    /// source closure. It does not silently turn a plan into a completed fact.
    pub fn revise_education_event(
        &self,
        lid: &str,
        sid: &str,
        old_event: &str,
        source: &str,
        content: &Value,
    ) -> Result<Value> {
        self.tx(|| {
            let old = self
                .source_events(lid, &[old_event.to_owned()])?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("被更正来源已失效"))?;
            let new = self.record_education_event(lid, sid, source, "correction", content)?;
            let id = s(&new, "event_id").to_owned();
            self.db.execute(
                "UPDATE education_event_meta SET version=?,supersedes_event_id=? WHERE event_id=?",
                params![
                    old["event_version"].as_i64().unwrap_or(1) + 1,
                    old_event,
                    id
                ],
            )?;
            self.invalidate_correction_sources(lid, &[old_event.to_owned()], &[id.clone()])?;
            self.source_events(lid, &[id])?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("更正来源无效"))
        })
    }

    /// Resolve every source from live evidence, not from a copied summary.
    pub fn memory_evidence(&self, lid: &str, mid: &str) -> Result<Value> {
        self.learner(lid)?;
        let memory = self
            .one(
                "SELECT * FROM memories WHERE learner_id=? AND memory_id=? AND status='active'",
                params![lid, mid],
            )?
            .ok_or_else(|| anyhow!("记忆不存在或已失效"))?;
        let ids = strings(&serde_json::from_str::<Value>(s(
            &memory,
            "source_event_ids",
        ))?);
        let unique: HashSet<_> = ids.iter().collect();
        ensure!(!unique.is_empty(), "记忆缺少来源");
        let links = self.rows(
            "SELECT l.event_id,l.event_version FROM memory_source_links l JOIN education_event_meta m ON m.event_id=l.event_id AND m.learner_id=l.learner_id AND m.namespace=l.namespace AND m.version=l.event_version JOIN events e ON e.event_id=m.event_id AND e.learner_id=m.learner_id AND e.session_id=m.session_id JOIN sessions es ON es.session_id=e.session_id AND es.learner_id=e.learner_id WHERE l.memory_id=? AND l.learner_id=? AND l.namespace=? AND l.status='active' AND m.status='active'",
            params![mid,lid,self.namespace],
        )?;
        let linked: HashSet<_> = links
            .iter()
            .map(|row| s(row, "event_id").to_owned())
            .collect();
        ensure!(
            linked == ids.iter().cloned().collect(),
            "记忆来源版本已失效"
        );
        let sources = self.source_events(lid, &ids)?;
        ensure!(sources.len() == unique.len(), "记忆来源不完整");
        ensure!(
            sources
                .iter()
                .all(|row| ["learner_report", "operator_confirmed", "simulated"]
                    .contains(&s(row, "source_type"))
                    && (self.namespace == "simulator" || s(row, "source_type") != "simulated")),
            "记忆来源类型不符合空间"
        );
        let decoded = Self::decode_memories(vec![memory])?.remove(0);
        let comparison_context = self.comparison_context_for_sources(lid, &sources)?;
        Ok(
            json!({"memory":decoded,"sources":sources,"source_links":links,"namespace":self.namespace,"education_metadata":self.education_memory_metadata(lid,mid)?,"activity_binding":self.memory_activity_binding(lid,mid)?,"comparison_context":comparison_context}),
        )
    }

    pub fn state_revision(&self, sid: &str) -> Result<i64> {
        let row = self.one(
            "SELECT v.revision FROM session_state_versions v JOIN sessions s ON s.session_id=v.session_id AND s.learner_id=v.learner_id WHERE v.session_id=? AND v.namespace=?",
            params![sid,self.namespace],
        )?.ok_or_else(|| anyhow!("活动状态版本不存在"))?;
        row["revision"]
            .as_i64()
            .ok_or_else(|| anyhow!("活动状态版本无效"))
    }

    /// Atomic optimistic update for future asynchronous extraction workers.
    /// The existing synchronous checkpoint API remains compatible.
    pub fn checkpoint_if_current(
        &self,
        lid: &str,
        sid: &str,
        generation: i64,
        revision: i64,
        epoch: i64,
        state: &Value,
    ) -> Result<i64> {
        self.tx(|| {
            let session = self.session(sid)?;
            ensure!(
                s(&session, "learner_id") == lid && s(&session, "status") != "finished",
                "活动归属或状态已失效"
            );
            ensure!(
                session["generation"] == generation
                    && self.state_revision(sid)? == revision
                    && self.learner(lid)?["epoch"] == epoch,
                "排队状态更新已过期"
            );
            ensure!(state.is_object(), "快状态必须为对象");
            ensure!(
                ["goal", "stage", "reported_result", "pending_step"]
                    .iter()
                    .all(|field| state[*field].is_string())
                    && (state["modules"].is_null() || state["modules"].is_array())
                    && state["field_evidence"].is_object()
                    && state["attempts"].is_array()
                    && state["session_preferences"].is_object(),
                "快状态结构不完整"
            );
            let mut ids = vec![];
            for field in [
                "goal",
                "stage",
                "reported_result",
                "pending_step",
                "modules",
            ] {
                let field_ids = strings(&state["field_evidence"][field]["source_event_ids"]);
                let nonempty = !state[field].is_null()
                    && state[field] != ""
                    && state[field] != empty_state()[field];
                ensure!(
                    !nonempty || !field_ids.is_empty(),
                    "快状态字段 {field} 缺少来源"
                );
                ids.extend(field_ids);
            }
            ids.extend(strings(&state["session_preferences"]["source_event_ids"]));
            for attempt in state["attempts"].as_array().into_iter().flatten() {
                let evidence = strings(&attempt["source_event_ids"]);
                ensure!(!evidence.is_empty(), "尝试记录缺少来源");
                ids.extend(evidence);
            }
            ids.sort();
            ids.dedup();
            for batch in ids.chunks(100) {
                ensure!(
                    self.source_events(lid, batch)?.len() == batch.len(),
                    "快状态引用了无效或其他档案来源"
                );
            }
            for field in ["reported_result", "stage"] {
                if state[field] != session["state"][field]
                    && (field == "reported_result" || state[field] == "completed")
                {
                    let sources = self.source_events(
                        lid,
                        &strings(&state["field_evidence"][field]["source_event_ids"]),
                    )?;
                    ensure!(
                        sources.iter().all(|row| s(row, "event_kind") != "plan"),
                        "计划不能作为已完成事实"
                    );
                }
            }
            if !state["modules"].is_null() {
                let sources = self.source_events(
                    lid,
                    &strings(&state["field_evidence"]["modules"]["source_event_ids"]),
                )?;
                ensure!(
                    sources
                        .iter()
                        .all(|row| s(row, "session_id") == sid && s(row, "event_kind") != "plan"),
                    "历史配置或计划不能作为当前模块"
                );
            }
            self.checkpoint(sid, state)?;
            self.state_revision(sid)
        })
    }

    pub(super) fn migrate_learner_preferences(&self) -> Result<()> {
        self.db.execute_batch("CREATE TABLE IF NOT EXISTS learner_preferences(
            learner_id TEXT PRIMARY KEY,
            namespace TEXT NOT NULL,
            reading_level TEXT NOT NULL DEFAULT 'standard' CHECK(reading_level IN('plain','standard','technical')),
            speech_speed TEXT NOT NULL DEFAULT 'normal' CHECK(speech_speed IN('slow','normal','fast'))
        );")?;
        let wrong: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM learner_preferences WHERE namespace!=?",
            [&self.namespace],
            |row| row.get(0),
        )?;
        ensure!(wrong == 0, "学习者设置空间不一致");
        self.db.execute("INSERT OR IGNORE INTO learner_preferences(learner_id,namespace) SELECT learner_id,? FROM learners", [&self.namespace])?;
        Ok(())
    }

    pub(super) fn learner_preferences(&self, lid: &str) -> Result<Value> {
        Ok(self.one("SELECT reading_level,speech_speed FROM learner_preferences WHERE learner_id=? AND namespace=?", params![lid,self.namespace])?
            .unwrap_or_else(||json!({"reading_level":"standard","speech_speed":"normal"})))
    }

    /// Partial preference updates preserve unspecified values and legacy fields.
    pub fn update_learner_preferences(
        &self,
        lid: &str,
        reading_level: Option<&str>,
        speech_speed: Option<&str>,
    ) -> Result<()> {
        ensure!(
            reading_level.is_none_or(|v| ["plain", "standard", "technical"].contains(&v)),
            "阅读复杂度无效"
        );
        ensure!(
            speech_speed.is_none_or(|v| ["slow", "normal", "fast"].contains(&v)),
            "播报速度无效"
        );
        self.tx(|| {
            self.learner(lid)?;
            self.db.execute("INSERT OR IGNORE INTO learner_preferences(learner_id,namespace) VALUES(?,?)",params![lid,self.namespace])?;
            let changed = self.db.execute("UPDATE learner_preferences SET reading_level=COALESCE(?,reading_level),speech_speed=COALESCE(?,speech_speed) WHERE learner_id=? AND namespace=?",params![reading_level,speech_speed,lid,self.namespace])?;
            ensure!(changed==1,"学习者设置归属不一致");
            Ok(())
        })
    }
}

//! Activity identity comes from persisted source lineage, never topic words.
//! This side table leaves the legacy positional memories schema unchanged.
use super::*;

impl Store {
    pub(super) fn migrate_memory_activity(&self) -> Result<()> {
        self.db.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_activity_bindings(
                memory_id TEXT PRIMARY KEY, learner_id TEXT NOT NULL,
                namespace TEXT NOT NULL, activity_id TEXT
             );
             CREATE TRIGGER IF NOT EXISTS memory_activity_delete AFTER DELETE ON memories BEGIN
                DELETE FROM memory_activity_bindings WHERE memory_id=OLD.memory_id;
             END;
             CREATE TRIGGER IF NOT EXISTS memory_activity_source_changed
             AFTER UPDATE OF learner_id,activity_scope,kind,scope,source_event_ids,supersedes ON memories
             WHEN NEW.learner_id IS NOT OLD.learner_id OR NEW.activity_scope IS NOT OLD.activity_scope
                OR NEW.kind IS NOT OLD.kind OR NEW.scope IS NOT OLD.scope
                OR NEW.source_event_ids IS NOT OLD.source_event_ids OR NEW.supersedes IS NOT OLD.supersedes BEGIN
                UPDATE memory_activity_bindings SET activity_id=NULL WHERE memory_id=NEW.memory_id;
             END;
             CREATE TRIGGER IF NOT EXISTS memory_activity_session_changed
             AFTER UPDATE OF learner_id,activity_id ON sessions
             WHEN NEW.learner_id IS NOT OLD.learner_id OR NEW.activity_id IS NOT OLD.activity_id BEGIN
                UPDATE memory_activity_bindings SET activity_id=NULL
                WHERE learner_id=OLD.learner_id AND activity_id=OLD.activity_id;
             END;",
        )?;
        let wrong: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM memory_activity_bindings WHERE namespace!=?",
            [&self.namespace],
            |row| row.get(0),
        )?;
        ensure!(wrong == 0, "记忆活动归属空间不一致");
        // Corrections refer to earlier rows. Include inactive originals because
        // their identity is provenance, not permission to recall their content.
        for row in self.rows(
            "SELECT m.memory_id FROM memories m WHERE NOT EXISTS(
                SELECT 1 FROM memory_activity_bindings b WHERE b.memory_id=m.memory_id)
             ORDER BY m.rowid",
            [],
        )? {
            self.link_memory_activity(s(&row, "memory_id"))?;
        }
        Ok(())
    }

    pub(super) fn session_memory_activity(&self, sid: &str) -> Result<(String, String)> {
        let row = self
            .one(
                "SELECT learner_id,activity_id FROM sessions WHERE session_id=?",
                [sid],
            )?
            .ok_or_else(|| anyhow!("活动会话不存在"))?;
        let lid = s(&row, "learner_id");
        let activity = s(&row, "activity_id");
        self.learner(lid)?;
        ensure!(!activity.is_empty(), "会话缺少真实活动归属");
        Ok((lid.to_owned(), activity.to_owned()))
    }

    fn bound_memory_activity(&self, lid: &str, mid: &str) -> Result<Option<String>> {
        Ok(self.one(
            "SELECT b.activity_id FROM memory_activity_bindings b
             JOIN memories m ON m.memory_id=b.memory_id AND m.learner_id=b.learner_id
             WHERE b.memory_id=? AND b.learner_id=? AND b.namespace=?
             AND EXISTS(SELECT 1 FROM sessions s WHERE s.learner_id=b.learner_id AND s.activity_id=b.activity_id)",
            params![mid,lid,self.namespace],
        )?.and_then(|row| row["activity_id"].as_str().filter(|v| !v.is_empty()).map(str::to_owned)))
    }

    // Explicit correction lineage retains the target's activity. Without an
    // explicit target this binds only the statement's own recorded activity.
    pub(super) fn source_memory_activity(&self, lid: &str, event: &str) -> Result<Option<String>> {
        let mut current = event.to_owned();
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            let Some(row) = self.one(
                "SELECT e.content,s.activity_id,m.kind,m.supersedes_event_id
                 FROM education_event_meta m JOIN sessions s ON s.session_id=m.session_id AND s.learner_id=m.learner_id
                 LEFT JOIN events e ON e.event_id=m.event_id AND e.learner_id=m.learner_id AND e.session_id=m.session_id
                 WHERE m.event_id=? AND m.learner_id=? AND m.namespace=?",
                params![current,lid,self.namespace],
            )? else { return Ok(None) };
            // Deleted sources retain identity metadata for correction lineage;
            // memory_evidence separately requires every live source/version.
            let content: Value = if row["content"].is_null() {
                Value::Null
            } else {
                serde_json::from_str(s(&row, "content"))?
            };
            let corrected = s(&content, "corrected_memory_id");
            if !corrected.is_empty() {
                return self.bound_memory_activity(lid, corrected);
            }
            let previous = s(&row, "supersedes_event_id");
            if !previous.is_empty() {
                current = previous.to_owned();
                continue;
            }
            return Ok(row["activity_id"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned));
        }
        Ok(None)
    }

    pub(super) fn link_memory_activity(&self, mid: &str) -> Result<()> {
        if self
            .one(
                "SELECT memory_id FROM memory_activity_bindings WHERE memory_id=?",
                [mid],
            )?
            .is_some()
        {
            return Ok(());
        }
        let memory = self
            .one("SELECT * FROM memories WHERE memory_id=?", [mid])?
            .ok_or_else(|| anyhow!("记忆不存在"))?;
        let lid = s(&memory, "learner_id");
        let supersedes = strings(&serde_json::from_str::<Value>(s(&memory, "supersedes"))?);
        let ids = strings(&serde_json::from_str::<Value>(s(
            &memory,
            "source_event_ids",
        ))?);
        let mut explicit_target = false;
        for event in &ids {
            if let Some(row) = self.one(
                "SELECT content FROM events WHERE event_id=? AND learner_id=?",
                params![event, lid],
            )? {
                let content: Value = serde_json::from_str(s(&row, "content"))?;
                explicit_target |= !s(&content, "corrected_memory_id").is_empty();
            }
        }
        let mut activities = HashSet::new();
        let mut complete = true;
        if !explicit_target && s(&memory, "kind") != "help_preference" && !supersedes.is_empty() {
            for original in &supersedes {
                match self.bound_memory_activity(lid, original)? {
                    Some(activity) => {
                        activities.insert(activity);
                    }
                    None => complete = false,
                }
            }
        } else {
            complete = !ids.is_empty();
            for event in &ids {
                match self.source_memory_activity(lid, event)? {
                    Some(activity) => {
                        activities.insert(activity);
                    }
                    None => complete = false,
                }
            }
        }
        let activity = if complete && activities.len() == 1 {
            activities.into_iter().next()
        } else {
            None
        };
        // Persist unresolved too: reopening must not guess a different origin.
        self.db.execute(
            "INSERT INTO memory_activity_bindings(memory_id,learner_id,namespace,activity_id) VALUES(?,?,?,?)",
            params![mid,lid,self.namespace,activity],
        )?;
        Ok(())
    }

    pub(super) fn sources_share_activity(&self, lid: &str, ids: &[String]) -> Result<bool> {
        let mut activity = None;
        for event in ids {
            let Some(origin) = self.source_memory_activity(lid, event)? else {
                return Ok(false);
            };
            if activity.as_ref().is_some_and(|a| a != &origin) {
                return Ok(false);
            }
            activity = Some(origin);
        }
        Ok(activity.is_some())
    }

    pub(super) fn memories_share_activity(&self, lid: &str, ids: &[String]) -> Result<bool> {
        let mut activity = None;
        for mid in ids {
            let Some(origin) = self.bound_memory_activity(lid, mid)? else {
                return Ok(false);
            };
            if activity.as_ref().is_some_and(|a| a != &origin) {
                return Ok(false);
            }
            activity = Some(origin);
        }
        Ok(activity.is_some())
    }

    pub(super) fn memory_activity_binding(&self, lid: &str, mid: &str) -> Result<Value> {
        let activity = self.bound_memory_activity(lid, mid)?;
        Ok(
            json!({"activity_id":activity,"status":if activity.is_some() {"bound"} else {"unresolved"}}),
        )
    }

    pub(super) fn explicit_persistent_preference_sources(&self, sources: &[Value]) -> Result<bool> {
        let explicit = Regex::new("(?i)以后|每次|一直|今后|always|from now|every time")?;
        Ok(!sources.is_empty()
            && sources.iter().all(|row| {
                if s(row, "event_kind") != "legacy_report" {
                    ["learner_statement", "teacher_setting"].contains(&s(row, "event_kind"))
                        && row["content"]["explicit_preference"] == true
                        && row["content"]["preference_scope"] == "persistent"
                } else {
                    row["content"]["preference_scope"] != "temporary"
                        && (explicit.is_match(&row["content"].to_string())
                            || (s(row, "source_type") == "operator_confirmed"
                                && row["content"]["set_preference"] == true))
                }
            }))
    }

    pub(super) fn memory_in_activity(
        &self,
        lid: &str,
        memory: &Value,
        activities: &HashSet<String>,
    ) -> Result<bool> {
        if s(memory, "learner_id") != lid || s(memory, "activity_scope") != "modular_rover" {
            return Ok(false);
        }
        if s(memory, "scope") == "cross_session_explicit" && s(memory, "kind") == "help_preference"
        {
            let ids = strings(&memory["source_event_ids"]);
            let sources = self.source_events(lid, &ids)?;
            return Ok(sources.len() == ids.iter().collect::<HashSet<_>>().len()
                && self.explicit_persistent_preference_sources(&sources)?);
        }
        Ok(s(memory, "scope") == "current_activity"
            && self
                .bound_memory_activity(lid, s(memory, "memory_id"))?
                .is_some_and(|activity| activities.contains(&activity)))
    }
}

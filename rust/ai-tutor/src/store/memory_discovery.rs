//! A small dirty-session index for memory discovery. Ticks inspect changed
//! sessions only; they never repeatedly scan all historical sessions/events.
use super::*;

impl Store {
    pub(super) fn migrate_memory_schedule(&self) -> Result<()> {
        let exists = self
            .one(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='memory_schedule_due'",
                [],
            )?
            .is_some();
        self.db.execute_batch("
            CREATE TABLE IF NOT EXISTS memory_schedule_due(
                session_id TEXT PRIMARY KEY,learner_id TEXT NOT NULL,namespace TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS memory_schedule_namespace ON memory_schedule_due(namespace);
            CREATE INDEX IF NOT EXISTS memory_discovery_snapshot ON memory_jobs(
                namespace,session_id,learner_id,generation,state_revision,epoch,prompt_version
            );
            CREATE TRIGGER IF NOT EXISTS memory_schedule_event_insert AFTER INSERT ON education_event_meta
            WHEN NEW.status='active' AND NEW.kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction') BEGIN
                INSERT OR IGNORE INTO memory_schedule_due VALUES(NEW.session_id,NEW.learner_id,NEW.namespace);
            END;
            CREATE TRIGGER IF NOT EXISTS memory_schedule_event_update AFTER UPDATE OF status,kind,version ON education_event_meta
            WHEN NEW.kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction')
                OR OLD.kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction') BEGIN
                INSERT OR IGNORE INTO memory_schedule_due VALUES(NEW.session_id,NEW.learner_id,NEW.namespace);
            END;
            CREATE TRIGGER IF NOT EXISTS memory_schedule_revision AFTER UPDATE OF revision ON session_state_versions
            WHEN NEW.revision!=OLD.revision BEGIN
                INSERT OR IGNORE INTO memory_schedule_due SELECT NEW.session_id,NEW.learner_id,NEW.namespace
                WHERE EXISTS(SELECT 1 FROM education_event_meta WHERE session_id=NEW.session_id AND status='active'
                    AND kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction'));
            END;
            CREATE TRIGGER IF NOT EXISTS memory_schedule_generation AFTER UPDATE OF generation ON sessions
            WHEN NEW.generation!=OLD.generation BEGIN
                INSERT OR IGNORE INTO memory_schedule_due
                SELECT NEW.session_id,NEW.learner_id,v.namespace FROM session_state_versions v WHERE v.session_id=NEW.session_id
                AND EXISTS(SELECT 1 FROM education_event_meta WHERE session_id=NEW.session_id AND status='active'
                    AND kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction'));
            END;
            CREATE TRIGGER IF NOT EXISTS memory_schedule_session_delete AFTER DELETE ON sessions BEGIN
                DELETE FROM memory_schedule_due WHERE session_id=OLD.session_id;
            END;
            CREATE TRIGGER IF NOT EXISTS memory_schedule_learner_delete AFTER DELETE ON learners BEGIN
                DELETE FROM memory_schedule_due WHERE learner_id=OLD.learner_id;
            END;
        ")?;
        // One-time migration backfill. A normal reopen must not re-enqueue every
        // reviewed historical session or revive deliberately cleared due rows.
        if !exists {
            self.db.execute("INSERT OR IGNORE INTO memory_schedule_due SELECT DISTINCT m.session_id,m.learner_id,m.namespace
                FROM education_event_meta m JOIN sessions s ON s.session_id=m.session_id AND s.learner_id=m.learner_id
                JOIN learners l ON l.learner_id=m.learner_id WHERE m.namespace=? AND m.status='active'
                AND m.kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction')",[&self.namespace])?;
        }
        let wrong: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM memory_schedule_due WHERE namespace!=?",
            [&self.namespace],
            |row| row.get(0),
        )?;
        ensure!(wrong == 0, "记忆待发现活动空间不一致");
        Ok(())
    }

    /// At most four dirty sessions and twelve uncovered typed sources per job.
    /// Existing current/versioned job snapshots are coverage, not proof of learning.
    /// A thirteenth row keeps the session dirty for another bounded batch.
    /// Selection, enqueue and dirty-marker rotation share one write transaction.
    pub fn discover_memory_jobs(
        &self,
        prompt_version: &str,
        limits: &MemoryJobLimits,
        synthetic_only: bool,
        now_ms: i64,
    ) -> Result<Value> {
        limits.validate()?;
        ensure!(
            !synthetic_only || self.namespace == "simulator",
            "合成发现必须使用simulator空间"
        );
        self.tx(||{
            let due=self.rows("SELECT d.session_id,d.learner_id FROM memory_schedule_due d
                JOIN sessions s ON s.session_id=d.session_id AND s.learner_id=d.learner_id
                JOIN learners l ON l.learner_id=d.learner_id
                WHERE d.namespace=? AND s.status!='generating' ORDER BY d.rowid LIMIT 4",[&self.namespace])?;
            let mut inspected=0;let mut discovered=0;let mut skipped=0;let mut full=false;
            for row in due {
                let queued:i64=self.db.query_row("SELECT COUNT(*) FROM memory_jobs WHERE namespace=? AND status IN('pending','leased','shadow_ready')",[&self.namespace],|row|row.get(0))?;
                if queued>=limits.max_pending as i64 {full=true;break;}
                inspected+=1;
                let sid=s(&row,"session_id");let lid=s(&row,"learner_id");
                let session=self.session(sid)?;
                let epoch=self.learner(lid)?["epoch"].as_i64();
                let sources=self.rows("SELECT e.event_id FROM events e JOIN education_event_meta m
                    ON m.event_id=e.event_id AND m.learner_id=e.learner_id AND m.session_id=e.session_id
                    WHERE e.session_id=? AND e.learner_id=? AND m.namespace=? AND m.status='active'
                    AND m.kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction')
                    AND e.source_type IN('learner_report','operator_confirmed','simulated')
                    AND (? OR e.source_type!='simulated') AND (NOT ? OR json_extract(e.content,'$.synthetic_fixture')=1)
                    AND NOT EXISTS(SELECT 1 FROM memory_jobs j,json_each(j.source_snapshot) binding
                        WHERE j.namespace=m.namespace AND j.learner_id=e.learner_id AND j.session_id=e.session_id
                        AND j.generation=? AND j.state_revision=? AND j.epoch=? AND j.prompt_version=?
                        AND j.status!='invalidated'
                        AND json_extract(binding.value,'$.event_id')=e.event_id
                        AND json_extract(binding.value,'$.event_version')=m.version)
                    ORDER BY e.rowid ASC LIMIT 13",params![sid,lid,self.namespace,self.namespace=="simulator",synthetic_only,
                        session["generation"].as_i64(),session["state_revision"].as_i64(),epoch,prompt_version])?;
                let more=sources.len()>12;
                if sources.is_empty(){skipped+=1;}else{
                    let ids:Vec<_>=sources.iter().take(12).map(|source|s(source,"event_id").to_owned()).collect();
                    self.enqueue_memory_job(lid,sid,&ids,prompt_version,limits,now_ms)?;
                    discovered+=1;
                }
                self.db.execute("DELETE FROM memory_schedule_due WHERE session_id=? AND learner_id=? AND namespace=?",params![sid,lid,self.namespace])?;
                if more {
                    // Rotate to the tail so one large session does not monopolize
                    // the next four-session tick. Queue-full/error never gets here.
                    self.db.execute("INSERT INTO memory_schedule_due VALUES(?,?,?)",params![sid,lid,self.namespace])?;
                }
            }
            Ok(json!({"inspected":inspected,"enqueued_or_existing":discovered,"skipped":skipped,"queue_full":full}))
        })
    }
}

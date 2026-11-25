//! Persistent, bounded background consolidation; the application owns scheduling
//! and provider calls. This module stores receipts and validates source snapshots.
use super::*;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryJobLimits {
    pub max_pending: usize,
    pub max_attempts: usize,
    pub daily_reservations: usize,
    pub session_reservations: usize,
    pub lease_seconds: u64,
}
impl Default for MemoryJobLimits {
    fn default() -> Self {
        Self {
            max_pending: 16,
            max_attempts: 3,
            daily_reservations: 20,
            session_reservations: 6,
            lease_seconds: 120,
        }
    }
}
impl MemoryJobLimits {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=128).contains(&self.max_pending)
                && (1..=5).contains(&self.max_attempts)
                && (1..=1000).contains(&self.daily_reservations)
                && (1..=100).contains(&self.session_reservations)
                && (5..=300).contains(&self.lease_seconds),
            "记忆整理限额无效"
        );
        Ok(())
    }
}

impl Store {
    pub(super) fn migrate_memory_jobs(&self) -> Result<()> {
        self.db.execute_batch("
            CREATE TABLE IF NOT EXISTS memory_jobs(
                job_id TEXT PRIMARY KEY,idempotency_key TEXT NOT NULL UNIQUE,
                namespace TEXT NOT NULL,learner_id TEXT NOT NULL,session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,epoch INTEGER NOT NULL,state_revision INTEGER NOT NULL,
                source_snapshot TEXT NOT NULL,prompt_version TEXT NOT NULL,limits_json TEXT NOT NULL,
                status TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,
                lease_owner TEXT,lease_token TEXT,lease_until_ms INTEGER,
                created_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS memory_job_queue ON memory_jobs(namespace,status,created_at_ms);
            CREATE TABLE IF NOT EXISTS memory_job_attempts(
                job_id TEXT NOT NULL,attempt_no INTEGER NOT NULL,namespace TEXT NOT NULL,
                learner_id TEXT NOT NULL,session_id TEXT NOT NULL,day_key TEXT NOT NULL,
                lease_token TEXT NOT NULL,claimed_at_ms INTEGER NOT NULL,started_at_ms INTEGER,
                status TEXT NOT NULL,PRIMARY KEY(job_id,attempt_no)
            );
            CREATE INDEX IF NOT EXISTS memory_job_day ON memory_job_attempts(namespace,day_key);
            CREATE INDEX IF NOT EXISTS memory_job_session ON memory_job_attempts(namespace,session_id);
            CREATE TABLE IF NOT EXISTS education_memory_candidates(
                candidate_id TEXT PRIMARY KEY,job_id TEXT NOT NULL,namespace TEXT NOT NULL,
                learner_id TEXT NOT NULL,kind TEXT NOT NULL,payload TEXT,status TEXT NOT NULL,
                memory_id TEXT,created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS education_candidate_job ON education_memory_candidates(job_id,status);
            CREATE TRIGGER IF NOT EXISTS memory_job_invalidate_payload AFTER UPDATE OF status ON memory_jobs
            WHEN NEW.status='invalidated' BEGIN
                UPDATE education_memory_candidates SET status='invalidated',payload=NULL WHERE job_id=NEW.job_id;
                UPDATE memory_job_attempts SET status='invalidated' WHERE job_id=NEW.job_id AND status IN('reserved','started');
            END;
            CREATE TRIGGER IF NOT EXISTS memory_job_source_invalidated AFTER UPDATE OF status,version ON education_event_meta
            WHEN NEW.status!='active' OR NEW.version!=OLD.version BEGIN
                UPDATE memory_jobs SET status='invalidated',source_snapshot='[]',lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL
                WHERE status!='invalidated' AND learner_id=NEW.learner_id AND namespace=NEW.namespace
                AND EXISTS(SELECT 1 FROM json_each(source_snapshot) WHERE json_extract(value,'$.event_id')=NEW.event_id);
            END;
            CREATE TRIGGER IF NOT EXISTS memory_job_source_deleted AFTER DELETE ON education_event_meta BEGIN
                UPDATE memory_jobs SET status='invalidated',source_snapshot='[]',lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL
                WHERE status!='invalidated' AND learner_id=OLD.learner_id AND namespace=OLD.namespace
                AND EXISTS(SELECT 1 FROM json_each(source_snapshot) WHERE json_extract(value,'$.event_id')=OLD.event_id);
            END;
            CREATE TRIGGER IF NOT EXISTS memory_job_revision_changed AFTER UPDATE OF revision ON session_state_versions
            WHEN NEW.revision!=OLD.revision BEGIN
                UPDATE memory_jobs SET status='invalidated',source_snapshot='[]',lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL
                WHERE session_id=NEW.session_id AND namespace=NEW.namespace AND state_revision!=NEW.revision AND status IN('pending','leased','shadow_ready');
            END;
            CREATE TRIGGER IF NOT EXISTS memory_job_generation_changed AFTER UPDATE OF generation ON sessions
            WHEN NEW.generation!=OLD.generation BEGIN
                UPDATE memory_jobs SET status='invalidated',source_snapshot='[]',lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL
                WHERE session_id=NEW.session_id AND generation!=NEW.generation AND status IN('pending','leased','shadow_ready');
            END;
            CREATE TRIGGER IF NOT EXISTS memory_job_epoch_changed AFTER UPDATE OF epoch ON learners
            WHEN NEW.epoch!=OLD.epoch BEGIN
                UPDATE memory_jobs SET status='invalidated',source_snapshot='[]',lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL
                WHERE learner_id=NEW.learner_id AND epoch!=NEW.epoch AND status!='invalidated';
            END;
        ")?;
        for table in [
            "memory_jobs",
            "memory_job_attempts",
            "education_memory_candidates",
        ] {
            let wrong: i64 = self.db.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE namespace!=?"),
                [&self.namespace],
                |row| row.get(0),
            )?;
            ensure!(wrong == 0, "记忆整理空间不一致");
        }
        self.migrate_memory_schedule()?;
        Ok(())
    }

    pub(super) fn memory_job(&self, id: &str) -> Result<Value> {
        let mut row = self
            .one(
                "SELECT * FROM memory_jobs WHERE job_id=? AND namespace=?",
                params![id, self.namespace],
            )?
            .ok_or_else(|| anyhow!("记忆整理任务不存在"))?;
        row["source_snapshot"] = serde_json::from_str(s(&row, "source_snapshot"))?;
        row["limits"] = serde_json::from_str(s(&row, "limits_json"))?;
        row.as_object_mut().unwrap().remove("limits_json");
        Ok(row)
    }

    pub(super) fn memory_job_sources(&self, job: &Value) -> Result<Vec<Value>> {
        ensure!(job["namespace"] == self.namespace, "记忆任务空间不一致");
        let lid = s(job, "learner_id");
        let session = self.session(s(job, "session_id"))?;
        ensure!(
            s(&session, "learner_id") == lid
                && session["generation"] == job["generation"]
                && session["state_revision"] == job["state_revision"]
                && self.learner(lid)?["epoch"] == job["epoch"],
            "记忆整理快照已过期"
        );
        let snapshot = job["source_snapshot"]
            .as_array()
            .ok_or_else(|| anyhow!("记忆来源快照无效"))?;
        ensure!(
            !snapshot.is_empty() && snapshot.len() <= 32,
            "记忆来源数量无效"
        );
        let ids: Vec<_> = snapshot
            .iter()
            .map(|row| s(row, "event_id").to_owned())
            .collect();
        let sources = self.source_events(lid, &ids)?;
        ensure!(
            sources
                .iter()
                .all(|row| row["session_id"] == job["session_id"])
                && sources.len() == snapshot.len()
                && snapshot.iter().all(|binding| sources
                    .iter()
                    .any(|row| row["event_id"] == binding["event_id"]
                        && row["event_version"] == binding["event_version"]
                        && row["event_status"] == "active")),
            "记忆整理来源已失效"
        );
        Ok(sources)
    }

    pub fn enqueue_memory_job(
        &self,
        lid: &str,
        sid: &str,
        source_ids: &[String],
        prompt_version: &str,
        limits: &MemoryJobLimits,
        now_ms: i64,
    ) -> Result<Value> {
        limits.validate()?;
        ensure!(
            !prompt_version.is_empty() && prompt_version.len() <= 80,
            "记忆提示版本无效"
        );
        ensure!(
            !source_ids.is_empty() && source_ids.len() <= 32,
            "记忆整理来源数量无效"
        );
        self.tx(|| {
            let session=self.session(sid)?;
            ensure!(s(&session,"learner_id")==lid,"记忆任务活动归属不一致");
            let mut ids=source_ids.to_vec();ids.sort();ids.dedup();
            let sources=self.source_events(lid,&ids)?;
            ensure!(sources.len()==ids.len() && sources.iter().all(|row|s(row,"session_id")==sid),"记忆任务包含无效来源或其他活动来源");
            let snapshot:Vec<_>=ids.iter().map(|id| {
                let row=sources.iter().find(|row|s(row,"event_id")==id).unwrap();
                json!({"event_id":id,"event_version":row["event_version"]})
            }).collect();
            let key_data=json!([self.namespace,lid,sid,session["generation"],session["state_revision"],self.learner(lid)?["epoch"],prompt_version,snapshot]);
            let key=format!("{:x}",Sha256::digest(key_data.to_string().as_bytes()));
            if let Some(existing)=self.one("SELECT job_id FROM memory_jobs WHERE idempotency_key=?",[&key])? {
                return self.memory_job_status(lid,s(&existing,"job_id"));
            }
            let queued:i64=self.db.query_row("SELECT COUNT(*) FROM memory_jobs WHERE namespace=? AND status IN('pending','leased','shadow_ready')",[&self.namespace],|row|row.get(0))?;
            ensure!(queued<limits.max_pending as i64,"后台记忆队列已满");
            let id=uid("memory_job");
            self.db.execute("INSERT INTO memory_jobs(job_id,idempotency_key,namespace,learner_id,session_id,generation,epoch,state_revision,source_snapshot,prompt_version,limits_json,status,created_at_ms,updated_at_ms) VALUES(?,?,?,?,?,?,?,?,?,?,?,'pending',?,?)",
                params![id,key,self.namespace,lid,sid,session["generation"].as_i64(),self.learner(lid)?["epoch"].as_i64(),session["state_revision"].as_i64(),json!(snapshot).to_string(),prompt_version,serde_json::to_string(limits)?,now_ms,now_ms])?;
            self.memory_job_status(lid,&id)
        })
    }

    /// A reservation bounds cost even if a worker disappears before reporting
    /// whether it actually started a model call. It is not a success metric.
    pub fn claim_memory_job(
        &self,
        worker: &str,
        limits: &MemoryJobLimits,
        now_ms: i64,
    ) -> Result<Option<Value>> {
        self.claim_memory_job_selected(worker, limits, now_ms, None)
    }

    /// An explicit review request never claims another learner's queued work.
    pub fn claim_memory_job_for(
        &self,
        worker: &str,
        limits: &MemoryJobLimits,
        now_ms: i64,
        job_id: &str,
    ) -> Result<Option<Value>> {
        ensure!(!job_id.is_empty(), "记忆整理任务编号缺失");
        self.claim_memory_job_selected(worker, limits, now_ms, Some(job_id))
    }

    fn claim_memory_job_selected(
        &self,
        worker: &str,
        limits: &MemoryJobLimits,
        now_ms: i64,
        job_id: Option<&str>,
    ) -> Result<Option<Value>> {
        limits.validate()?;
        ensure!(
            !worker.is_empty() && worker.len() <= 100,
            "记忆工作者编号无效"
        );
        let instant = DateTime::<Utc>::from_timestamp_millis(now_ms)
            .ok_or_else(|| anyhow!("记忆时钟无效"))?;
        let day = instant
            .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
            .format("%Y-%m-%d")
            .to_string();
        self.tx(|| {
            let previous:Option<i64>=self.db.query_row("SELECT MAX(claimed_at_ms) FROM memory_job_attempts WHERE namespace=?",[&self.namespace],|row|row.get(0))?;
            ensure!(previous.is_none_or(|previous|now_ms>=previous),"记忆预算时钟回退，暂不开始新任务");
            self.reclaim_memory_jobs(now_ms, false, job_id)?;
            for row in self.rows("SELECT job_id FROM memory_jobs WHERE namespace=? AND status='pending' AND (? IS NULL OR job_id=?) ORDER BY created_at_ms,rowid",params![self.namespace,job_id,job_id])? {
                let id=s(&row,"job_id");
                let job=self.memory_job(id)?;
                let sources=match self.memory_job_sources(&job) {Ok(sources)=>sources,Err(_)=>{self.invalidate_memory_job(id)?;continue;}};
                let original:MemoryJobLimits=serde_json::from_value(job["limits"].clone())?;
                let attempts=job["attempts"].as_u64().unwrap_or(0) as usize;
                if attempts>=limits.max_attempts.min(original.max_attempts) {
                    self.db.execute("UPDATE memory_jobs SET status='exhausted' WHERE job_id=?",[id])?;continue;
                }
                let daily:i64=self.db.query_row("SELECT COUNT(*) FROM memory_job_attempts WHERE namespace=? AND day_key=?",params![self.namespace,day],|row|row.get(0))?;
                let per_session:i64=self.db.query_row("SELECT COUNT(*) FROM memory_job_attempts WHERE namespace=? AND session_id=?",params![self.namespace,s(&job,"session_id")],|row|row.get(0))?;
                if daily>=limits.daily_reservations.min(original.daily_reservations) as i64 {continue;}
                if per_session>=limits.session_reservations.min(original.session_reservations) as i64 {continue;}
                let token=uid("memory_lease");
                let until=now_ms+(limits.lease_seconds.min(original.lease_seconds)*1000) as i64;
                self.db.execute("UPDATE memory_jobs SET status='leased',attempts=attempts+1,lease_owner=?,lease_token=?,lease_until_ms=?,updated_at_ms=? WHERE job_id=?",params![worker,token,until,now_ms,id])?;
                self.db.execute("INSERT INTO memory_job_attempts VALUES(?,?,?,?,?,?,?,?,NULL,'reserved')",params![id,(attempts+1) as i64,self.namespace,s(&job,"learner_id"),s(&job,"session_id"),day,token,now_ms])?;
                let receipt=json!({"job_id":id,"lease_owner":worker,"lease_token":token,"attempt_no":attempts+1,"lease_until_ms":until});
                return Ok(Some(json!({"job":self.memory_job_status(s(&job,"learner_id"),id)?,"receipt":receipt,"sources":sources})));
            }
            Ok(None)
        })
    }

    fn leased_memory_job(&self, receipt: &Value, now_ms: i64) -> Result<Value> {
        let job = self.memory_job(s(receipt, "job_id"))?;
        ensure!(
            job["status"] == "leased"
                && job["lease_owner"] == receipt["lease_owner"]
                && job["lease_token"] == receipt["lease_token"]
                && job["attempts"] == receipt["attempt_no"]
                && job["lease_until_ms"]
                    .as_i64()
                    .is_some_and(|until| until > now_ms)
                && job["updated_at_ms"]
                    .as_i64()
                    .is_some_and(|start| now_ms >= start),
            "记忆租约已过期或不属于该工作者"
        );
        self.memory_job_sources(&job)?;
        Ok(job)
    }

    pub fn mark_memory_job_started(&self, receipt: &Value, now_ms: i64) -> Result<()> {
        self.tx(|| {
            let job=self.leased_memory_job(receipt,now_ms)?;
            self.db.execute("UPDATE memory_job_attempts SET started_at_ms=COALESCE(started_at_ms,?),status='started' WHERE job_id=? AND attempt_no=? AND status IN('reserved','started')",params![now_ms,s(&job,"job_id"),job["attempts"].as_i64()])?;
            Ok(())
        })
    }

    fn release_memory_job(&self, receipt: &Value, now_ms: i64, outcome: &str) -> Result<()> {
        self.tx(|| {
            let job=self.leased_memory_job(receipt,now_ms)?;
            let limits:MemoryJobLimits=serde_json::from_value(job["limits"].clone())?;
            let status=if job["attempts"].as_u64().unwrap_or(0)>=limits.max_attempts as u64 {"exhausted"} else {"pending"};
            self.db.execute("UPDATE memory_job_attempts SET status=? WHERE job_id=? AND attempt_no=?",params![outcome,s(&job,"job_id"),job["attempts"].as_i64()])?;
            self.db.execute("UPDATE memory_jobs SET status=?,lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL,updated_at_ms=? WHERE job_id=?",params![status,now_ms,s(&job,"job_id")])?;
            Ok(())
        })
    }
    pub fn preempt_memory_job(&self, receipt: &Value, now_ms: i64) -> Result<()> {
        self.release_memory_job(receipt, now_ms, "preempted")
    }
    pub fn fail_memory_job(&self, receipt: &Value, now_ms: i64) -> Result<()> {
        self.release_memory_job(receipt, now_ms, "failed")
    }

    fn invalidate_memory_job(&self, id: &str) -> Result<()> {
        self.db.execute("UPDATE memory_jobs SET status='invalidated',source_snapshot='[]',lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL WHERE job_id=?",[id])?;
        Ok(())
    }
    fn reclaim_memory_jobs(
        &self,
        now_ms: i64,
        restart: bool,
        job_id: Option<&str>,
    ) -> Result<usize> {
        self.tx(|| {
            let rows=self.rows("SELECT job_id FROM memory_jobs WHERE namespace=? AND status='leased' AND (? OR lease_until_ms<=?) AND (? IS NULL OR job_id=?)",params![self.namespace,restart,now_ms,job_id,job_id])?;
            for row in &rows {
                let id=s(row,"job_id");let job=self.memory_job(id)?;
                if self.memory_job_sources(&job).is_err() {self.invalidate_memory_job(id)?;continue;}
                let limits:MemoryJobLimits=serde_json::from_value(job["limits"].clone())?;
                let status=if job["attempts"].as_u64().unwrap_or(0)>=limits.max_attempts as u64 {"exhausted"}else{"pending"};
                self.db.execute("UPDATE memory_job_attempts SET status=? WHERE job_id=? AND attempt_no=?",params![if restart{"restart_recovered"}else{"lease_expired"},id,job["attempts"].as_i64()])?;
                self.db.execute("UPDATE memory_jobs SET status=?,lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL,updated_at_ms=? WHERE job_id=?",params![status,now_ms,id])?;
            }
            Ok(rows.len())
        })
    }
    pub fn reap_memory_jobs(&self, now_ms: i64) -> Result<usize> {
        self.reclaim_memory_jobs(now_ms, false, None)
    }
    /// Only the scheduler that owns the application's exclusive startup lock
    /// calls this. Merely opening another SQLite connection never steals leases.
    pub fn recover_memory_jobs(&self, now_ms: i64) -> Result<usize> {
        self.reclaim_memory_jobs(now_ms, true, None)
    }

    pub fn stage_memory_candidates(
        &self,
        receipt: &Value,
        candidates: &[Value],
        now_ms: i64,
    ) -> Result<Vec<Value>> {
        ensure!(candidates.len() <= 5, "记忆候选数量超出上限");
        self.tx(|| {
            let job=self.leased_memory_job(receipt,now_ms)?;
            let sources=self.memory_job_sources(&job)?;
            let normalized=candidates.iter().map(|candidate|self.validate_education_candidate(candidate,&sources)).collect::<Result<Vec<_>>>()?;
            let mut saved=vec![];
            for payload in normalized {
                let id=uid("memory_candidate");
                self.db.execute("INSERT INTO education_memory_candidates VALUES(?,?,?,?,?,?,'shadow',NULL,?)",params![id,s(&job,"job_id"),self.namespace,s(&job,"learner_id"),s(&payload,"kind"),payload.to_string(),now_ms])?;
                saved.push(json!({"candidate_id":id,"status":"shadow","payload":payload}));
            }
            self.db.execute("UPDATE memory_job_attempts SET status='response_received' WHERE job_id=? AND attempt_no=?",params![s(&job,"job_id"),job["attempts"].as_i64()])?;
            self.db.execute("UPDATE memory_jobs SET status=?,lease_owner=NULL,lease_token=NULL,lease_until_ms=NULL,updated_at_ms=? WHERE job_id=?",params![if saved.is_empty(){"completed"}else{"shadow_ready"},now_ms,s(&job,"job_id")])?;
            Ok(saved)
        })
    }

    pub fn memory_job_status(&self, lid: &str, id: &str) -> Result<Value> {
        self.learner(lid)?;
        let mut job = self.memory_job(id)?;
        ensure!(s(&job, "learner_id") == lid, "记忆任务不属于该档案");
        let object = job.as_object_mut().unwrap();
        object.remove("lease_owner");
        object.remove("lease_token");
        job["attempt_receipts"]=json!(self.rows("SELECT attempt_no,day_key,claimed_at_ms,started_at_ms,status FROM memory_job_attempts WHERE job_id=? ORDER BY attempt_no",[id])?);
        let mut candidates=self.rows("SELECT candidate_id,kind,payload,status,memory_id FROM education_memory_candidates WHERE job_id=? AND namespace=? AND learner_id=?",params![id,self.namespace,lid])?;
        for candidate in &mut candidates {
            if candidate["payload"].is_string() {
                candidate["payload"] = serde_json::from_str(s(candidate, "payload"))?;
            }
        }
        job["candidates"] = json!(candidates);
        Ok(job)
    }

    pub fn apply_memory_candidate(
        &self,
        lid: &str,
        candidate_id: &str,
        enabled_kinds: &[&str],
        now_ms: i64,
    ) -> Result<Value> {
        self.tx(|| {
            let candidate=self.one("SELECT * FROM education_memory_candidates WHERE candidate_id=? AND learner_id=? AND namespace=?",params![candidate_id,lid,self.namespace])?.ok_or_else(||anyhow!("教育记忆候选不存在"))?;
            ensure!(enabled_kinds.contains(&s(&candidate,"kind")),"该记忆类型尚未启用，候选保留为shadow");
            let job=self.memory_job(s(&candidate,"job_id"))?;
            let sources=self.memory_job_sources(&job)?;
            if candidate["status"]=="applied" {
                return self.memory_evidence(lid,s(&candidate,"memory_id"));
            }
            ensure!(candidate["status"]=="shadow" && job["status"]=="shadow_ready","教育记忆候选已失效");
            let payload:Value=serde_json::from_str(s(&candidate,"payload"))?;
            let payload=self.validate_education_candidate(&payload,&sources)?;
            let mid=self.persist_education_candidate(lid,&payload,&sources,job["epoch"].as_i64().unwrap_or(-1))?;
            self.db.execute("UPDATE education_memory_candidates SET status='applied',memory_id=? WHERE candidate_id=?",params![mid,candidate_id])?;
            let remaining:i64=self.db.query_row("SELECT COUNT(*) FROM education_memory_candidates WHERE job_id=? AND status='shadow'",[s(&job,"job_id")],|row|row.get(0))?;
            if remaining==0 {self.db.execute("UPDATE memory_jobs SET status='applied',updated_at_ms=? WHERE job_id=?",params![now_ms,s(&job,"job_id")])?;}
            self.memory_evidence(lid,&mid)
        })
    }
}

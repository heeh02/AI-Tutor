//! Explicit, source-checked review. Preparing work never calls a provider, and
//! staging a candidate never implies that the user chose to retain it.
use super::*;

const REVIEW_KINDS: &[&str] = &[
    "learning_goal",
    "context_constraint",
    "trial_observation",
    "concept_evidence",
    "revisable_misconception",
    "help_preference",
    "accepted_pending_experiment",
    "correction",
];

impl Store {
    fn memory_review_context(&self, lid: &str, sid: &str) -> Result<Value> {
        let learner = self.learner(lid)?;
        let session = self.session(sid)?;
        ensure!(
            s(&session, "learner_id") == lid && session["namespace"] == self.namespace,
            "记忆审阅会话不属于此档案或数据空间"
        );
        Ok(
            json!({"learner_id":lid,"session_id":sid,"namespace":self.namespace,
            "generation":session["generation"],"state_revision":session["state_revision"],
            "epoch":learner["epoch"],"session_status":session["status"]}),
        )
    }

    /// Same source/version/context coverage rule as bounded discovery. Explicit
    /// simulator use is allowed here; synthetic_fixture is a diagnostic gate.
    fn uncovered_review_sources(&self, context: &Value) -> Result<Vec<Value>> {
        self.rows("SELECT e.event_id FROM events e JOIN education_event_meta m
            ON m.event_id=e.event_id AND m.learner_id=e.learner_id AND m.session_id=e.session_id
            WHERE e.session_id=? AND e.learner_id=? AND m.namespace=? AND m.status='active'
            AND m.kind IN('learner_statement','learner_answer','observation','plan','teacher_setting','correction')
            AND e.source_type IN('learner_report','operator_confirmed','simulated')
            AND (? OR e.source_type!='simulated')
            AND (COALESCE(json_extract(e.content,'$.confirmation'),'')!='explicit_save_action'
                OR NOT EXISTS(SELECT 1 FROM memories saved JOIN memory_source_links link ON link.memory_id=saved.memory_id
                    WHERE saved.learner_id=e.learner_id AND saved.status='active' AND saved.kind='help_preference'
                    AND saved.scope='cross_session_explicit' AND link.event_id=e.event_id AND link.learner_id=e.learner_id
                    AND link.namespace=m.namespace AND link.status='active' AND link.event_version=m.version))
            AND NOT EXISTS(SELECT 1 FROM memory_jobs j,json_each(j.source_snapshot) binding
                WHERE j.namespace=m.namespace AND j.learner_id=e.learner_id AND j.session_id=e.session_id
                AND j.generation=? AND j.state_revision=? AND j.epoch=? AND j.prompt_version=?
                AND j.status!='invalidated'
                AND json_extract(binding.value,'$.event_id')=e.event_id
                AND json_extract(binding.value,'$.event_version')=m.version)
            ORDER BY e.rowid ASC LIMIT 13",
            params![s(context,"session_id"),s(context,"learner_id"),self.namespace,self.namespace=="simulator",
                context["generation"].as_i64(),context["state_revision"].as_i64(),context["epoch"].as_i64(),
                crate::kimi::memory_contract::PROMPT_VERSION])
    }

    fn current_review_job(&self, context: &Value) -> Result<Option<Value>> {
        let row = self.one(
            "SELECT job_id FROM memory_jobs WHERE learner_id=? AND session_id=? AND namespace=?
            AND generation=? AND state_revision=? AND epoch=? AND prompt_version=?
            ORDER BY CASE WHEN status IN('pending','leased','shadow_ready') THEN 0 ELSE 1 END,
                CASE WHEN status IN('pending','leased','shadow_ready') THEN created_at_ms END ASC,
                CASE WHEN status IN('pending','leased','shadow_ready') THEN rowid END ASC,
                created_at_ms DESC,rowid DESC LIMIT 1",
            params![
                s(context, "learner_id"),
                s(context, "session_id"),
                self.namespace,
                context["generation"].as_i64(),
                context["state_revision"].as_i64(),
                context["epoch"].as_i64(),
                crate::kimi::memory_contract::PROMPT_VERSION
            ],
        )?;
        row.map(|row| self.memory_job(s(&row, "job_id")))
            .transpose()
    }

    /// Current context only. The job projection intentionally omits leases,
    /// worker tokens, request snapshots, and raw provider output.
    pub fn memory_review_snapshot(&self, lid: &str, sid: &str) -> Result<Value> {
        self.tx(|| {
            let mut context = self.memory_review_context(lid,sid)?;
            context.as_object_mut().unwrap().remove("session_status");
            let remaining = self.uncovered_review_sources(&context)?;
            let mut candidates = Vec::new();
            let job = self.current_review_job(&context)?;
            if let Some(job) = &job {
                if job["status"] == "shadow_ready" {
                    let available = self.memory_job_sources(job)?;
                    for candidate in self.rows("SELECT candidate_id,kind,payload FROM education_memory_candidates
                        WHERE job_id=? AND learner_id=? AND namespace=? AND status='shadow' ORDER BY rowid",
                        params![s(job,"job_id"),lid,self.namespace])? {
                        let input: Value = serde_json::from_str(s(&candidate,"payload"))?;
                        let payload = self.validate_education_candidate(&input,&available)?;
                        ensure!(payload["kind"] == candidate["kind"], "记忆候选类别绑定已失效");
                        let ids = strings(&payload["source_event_ids"]);
                        let sources: Vec<_> = available.iter().filter(|source|
                            ids.iter().any(|id| id == s(source,"event_id"))).cloned().collect();
                        let admission = self.validate_education_promotion(lid,&payload,&available,
                            context["epoch"].as_i64().unwrap_or(-1));
                        let blocked_reason = admission.err().map(|error| error.to_string());
                        candidates.push(json!({"candidate_id":candidate["candidate_id"],"job_id":job["job_id"],
                            "kind":payload["kind"],"payload":payload,"sources":sources,
                            "can_retain":blocked_reason.is_none(),"blocked_reason":blocked_reason}));
                    }
                }
            }
            context["status"] = json!(job.as_ref().map(|job| s(job,"status"))
                .unwrap_or(if remaining.is_empty() { "no_sources" } else { "ready" }));
            context["job"] = job.map(|job| json!({"job_id":job["job_id"],"status":job["status"],"attempts":job["attempts"]}))
                .unwrap_or(Value::Null);
            context["candidates"] = json!(candidates);
            context["has_more_sources"] = json!(!remaining.is_empty());
            Ok(context)
        })
    }

    /// An explicit click prepares at most one batch. Merely viewing candidates
    /// is read-only; repeated clicks reuse current work rather than reserving
    /// another model attempt or recreating dismissed candidates.
    pub fn prepare_memory_review(
        &self,
        lid: &str,
        sid: &str,
        limits: &MemoryJobLimits,
        now_ms: i64,
    ) -> Result<Value> {
        limits.validate()?;
        self.tx(|| {
            let context = self.memory_review_context(lid, sid)?;
            ensure!(
                context["session_status"] != "generating",
                "教学正在进行，请稍后整理记忆"
            );
            if self
                .current_review_job(&context)?
                .as_ref()
                .is_some_and(|job| {
                    matches!(s(job, "status"), "pending" | "leased" | "shadow_ready")
                })
            {
                return self.memory_review_snapshot(lid, sid);
            }
            let remaining = self.uncovered_review_sources(&context)?;
            if !remaining.is_empty() {
                let ids: Vec<_> = remaining
                    .iter()
                    .take(12)
                    .map(|row| s(row, "event_id").to_owned())
                    .collect();
                self.enqueue_memory_job(
                    lid,
                    sid,
                    &ids,
                    crate::kimi::memory_contract::PROMPT_VERSION,
                    limits,
                    now_ms,
                )?;
            }
            self.memory_review_snapshot(lid, sid)
        })
    }

    pub fn review_memory_candidate(
        &self,
        lid: &str,
        sid: &str,
        cid: &str,
        decision: &str,
        now_ms: i64,
    ) -> Result<Value> {
        ensure!(
            ["retain", "dismiss"].contains(&decision),
            "记忆审阅决定无效"
        );
        self.tx(|| {
            self.memory_review_context(lid,sid)?;
            let candidate = self.one("SELECT * FROM education_memory_candidates WHERE candidate_id=? AND learner_id=? AND namespace=?",
                params![cid,lid,self.namespace])?.ok_or_else(||anyhow!("记忆候选不存在或不属于此档案"))?;
            let job = self.memory_job(s(&candidate,"job_id"))?;
            ensure!(job["session_id"] == sid && job["learner_id"] == lid
                && job["prompt_version"] == crate::kimi::memory_contract::PROMPT_VERSION, "记忆候选会话或版本已失效");
            self.memory_job_sources(&job)?;
            let previous = s(&candidate,"status");
            if previous == "applied" && decision == "retain" {
                self.memory_evidence(lid,s(&candidate,"memory_id"))?;
                return Ok(json!({"candidate_id":cid,"job_id":job["job_id"],"decision":decision,"status":"applied",
                    "memory_id":candidate["memory_id"],"replayed":true}));
            }
            if previous == "dismissed" && decision == "dismiss" {
                return Ok(json!({"candidate_id":cid,"job_id":job["job_id"],"decision":decision,"status":"dismissed",
                    "memory_id":null,"replayed":true}));
            }
            ensure!(previous == "shadow" && job["status"] == "shadow_ready", "记忆候选已经处理或失效");
            if decision == "retain" {
                let evidence = self.apply_memory_candidate(lid,cid,REVIEW_KINDS,now_ms)?;
                return Ok(json!({"candidate_id":cid,"job_id":job["job_id"],"decision":decision,"status":"applied",
                    "memory_id":evidence["memory"]["memory_id"],"replayed":false}));
            }
            self.db.execute("UPDATE education_memory_candidates SET status='dismissed',payload=NULL WHERE candidate_id=?",[cid])?;
            let remaining: i64 = self.db.query_row("SELECT COUNT(*) FROM education_memory_candidates WHERE job_id=? AND status='shadow'",
                [s(&job,"job_id")],|row|row.get(0))?;
            if remaining == 0 {
                self.db.execute("UPDATE memory_jobs SET status='completed',updated_at_ms=? WHERE job_id=?",params![now_ms,s(&job,"job_id")])?;
            }
            Ok(json!({"candidate_id":cid,"job_id":job["job_id"],"decision":decision,"status":"dismissed",
                "memory_id":null,"replayed":false}))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Result<(tempfile::TempDir, Store, String, String)> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("review.sqlite3"), "simulator")?;
        let learner = store.create_learner("Synthetic review", "zh", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        Ok((directory, store, lid, s(&session, "session_id").to_owned()))
    }
    fn source(store: &Store, lid: &str, sid: &str, kind: &str, text: &str) -> Result<Value> {
        store.record_education_event(lid, sid, "simulated", kind, &json!({"text":text}))
    }
    fn payload(source: &Value, kind: &str, scope: &str) -> Value {
        let claim = match kind {
            "learning_goal" => "stated_goal",
            "context_constraint" => "condition_limit",
            "trial_observation" => "reported_observation",
            "concept_evidence" => "task_specific_response",
            "revisable_misconception" => "tentative_interpretation",
            "help_preference" => "explicit_preference",
            "accepted_pending_experiment" => "accepted_plan",
            "correction" => "explicit_correction",
            _ => unreachable!(),
        };
        let id = s(source, "event_id");
        json!({"kind":kind,"content":source["content"]["text"],"scope":scope,"claim_type":claim,
            "source_event_ids":[id],"source_event_versions":{id:source["event_version"]},
            "source_quotes":[{"event_id":id,"quote":source["content"]["text"]}],"support_level":"unknown",
            "importance":{"goal_relevance":1,"continuity":1,"novel_change":1,"explicit_request":1}})
    }
    fn stage(
        store: &Store,
        lid: &str,
        sid: &str,
        candidates: &[Value],
        at: i64,
    ) -> Result<Vec<Value>> {
        let limits = MemoryJobLimits::default();
        let prepared = store.prepare_memory_review(lid, sid, &limits, at)?;
        let job_id = s(&prepared["job"], "job_id");
        let claim = store
            .claim_memory_job_for("review-fixture", &limits, at + 1, job_id)?
            .unwrap();
        store.stage_memory_candidates(&claim["receipt"], candidates, at + 2)
    }

    #[test]
    fn review_is_explicit_source_graded_and_each_decision_is_idempotent() -> Result<()> {
        let (_directory, store, lid, sid) = fixture()?;
        let a = source(&store, &lid, &sid, "observation", "第一次仍碰到墙")?;
        let b = source(&store, &lid, &sid, "observation", "第二次提前停下")?;
        let saved = stage(
            &store,
            &lid,
            &sid,
            &[
                payload(&a, "trial_observation", "current_activity"),
                payload(&b, "trial_observation", "current_activity"),
            ],
            1000,
        )?;
        ensure!(store.memories(&lid)?.is_empty());
        let review = store.memory_review_snapshot(&lid, &sid)?;
        ensure!(review["candidates"].as_array().unwrap().len() == 2);
        ensure!(review["job"].as_object().unwrap().len() == 3);
        ensure!(review["candidates"][0]["can_retain"] == true);
        ensure!(review["candidates"][0]["sources"][0]["source_type"] == "simulated");
        ensure!(review["candidates"][0]["payload"]["source_grade"] == "simulated");
        let before = store.session(&sid)?;
        let retained = store.review_memory_candidate(
            &lid,
            &sid,
            s(&saved[0], "candidate_id"),
            "retain",
            1003,
        )?;
        ensure!(retained["status"] == "applied" && retained["replayed"] == false);
        ensure!(
            store.review_memory_candidate(
                &lid,
                &sid,
                s(&saved[0], "candidate_id"),
                "retain",
                1004
            )?["replayed"]
                == true
        );
        ensure!(
            store
                .review_memory_candidate(&lid, &sid, s(&saved[0], "candidate_id"), "dismiss", 1004)
                .is_err()
        );
        let evidence = store.memory_evidence(&lid, s(&retained, "memory_id"))?;
        ensure!(
            evidence["memory"]["kind"] == "reported_progress"
                && evidence["memory"]["source_type"] == "simulated"
        );
        ensure!(store.session(&sid)?["state_revision"] == before["state_revision"]);
        let cid = s(&saved[1], "candidate_id");
        ensure!(
            store.review_memory_candidate(&lid, &sid, cid, "dismiss", 1005)?["status"]
                == "dismissed"
        );
        ensure!(
            store.review_memory_candidate(&lid, &sid, cid, "dismiss", 1006)?["replayed"] == true
        );
        ensure!(
            store
                .review_memory_candidate(&lid, &sid, cid, "retain", 1006)
                .is_err()
        );
        let again = store.prepare_memory_review(&lid, &sid, &MemoryJobLimits::default(), 1007)?;
        ensure!(
            again["job"]["status"] == "completed"
                && again["candidates"] == json!([])
                && again["has_more_sources"] == false
        );
        ensure!(store.rows("SELECT job_id FROM memory_jobs", [])?.len() == 1);
        ensure!(
            store
                .rows("SELECT job_id FROM memory_job_attempts", [])?
                .len()
                == 1
        );
        Ok(())
    }

    #[test]
    fn temporary_unaccepted_and_wrong_type_sources_are_visible_but_not_retainable() -> Result<()> {
        let (_directory, store, lid, sid) = fixture()?;
        let statement = source(&store, &lid, &sid, "learner_statement", "这次只讲一步")?;
        let plan = store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "plan",
            &json!({"text":"也许下次再试","accepted":false}),
        )?;
        let staged = stage(
            &store,
            &lid,
            &sid,
            &[
                payload(&statement, "help_preference", "current_session"),
                payload(&plan, "accepted_pending_experiment", "current_activity"),
                payload(&statement, "concept_evidence", "current_activity"),
            ],
            2000,
        )?;
        let snapshot = store.memory_review_snapshot(&lid, &sid)?;
        for row in snapshot["candidates"].as_array().unwrap() {
            ensure!(row["can_retain"] == false && row["blocked_reason"].is_string());
        }
        for row in staged {
            ensure!(
                store
                    .review_memory_candidate(&lid, &sid, s(&row, "candidate_id"), "retain", 2003)
                    .is_err()
            );
            store.review_memory_candidate(&lid, &sid, s(&row, "candidate_id"), "dismiss", 2004)?;
        }
        ensure!(store.memories(&lid)?.is_empty());
        ensure!(store.session(&sid)?["state"]["reported_result"] == "");
        Ok(())
    }

    #[test]
    fn all_eight_existing_categories_use_the_same_strict_promotion_gate() -> Result<()> {
        for (kind, event_kind, scope) in [
            ("learning_goal", "learner_statement", "current_activity"),
            (
                "context_constraint",
                "learner_statement",
                "current_activity",
            ),
            ("trial_observation", "observation", "current_activity"),
            ("concept_evidence", "learner_answer", "current_activity"),
            (
                "revisable_misconception",
                "learner_answer",
                "current_activity",
            ),
            (
                "help_preference",
                "learner_statement",
                "cross_session_explicit",
            ),
            ("accepted_pending_experiment", "plan", "current_activity"),
            ("correction", "correction", "current_activity"),
        ] {
            let (_directory, store, lid, sid) = fixture()?;
            let event = store.record_education_event(&lid,&sid,"simulated",event_kind,
                &json!({"text":"明确的合成来源原文","accepted":true,"explicit_preference":true,"preference_scope":"persistent"}))?;
            let staged = stage(&store, &lid, &sid, &[payload(&event, kind, scope)], 3000)?;
            ensure!(
                store.memory_review_snapshot(&lid, &sid)?["candidates"][0]["can_retain"] == true,
                "{kind}"
            );
            store.review_memory_candidate(
                &lid,
                &sid,
                s(&staged[0], "candidate_id"),
                "retain",
                3003,
            )?;
            ensure!(store.memories(&lid)?.len() == 1, "{kind}");
        }
        Ok(())
    }

    #[test]
    fn discovery_keeps_oldest_twelve_and_followup_batch_without_new_budget_on_repeated_clicks()
    -> Result<()> {
        let (_directory, store, lid, sid) = fixture()?;
        let limits = MemoryJobLimits::default();
        let mut ids = Vec::new();
        for i in 0..14 {
            ids.push(
                s(
                    &source(&store, &lid, &sid, "learner_statement", &format!("来源{i}"))?,
                    "event_id",
                )
                .to_owned(),
            );
        }
        let first = store.prepare_memory_review(&lid, &sid, &limits, 4000)?;
        let first_id = s(&first["job"], "job_id");
        let job = store.memory_job(first_id)?;
        let first_sources: HashSet<_> = job["source_snapshot"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| s(v, "event_id").to_owned())
            .collect();
        ensure!(
            first_sources == ids[..12].iter().cloned().collect()
                && first["has_more_sources"] == true
        );
        ensure!(
            store.prepare_memory_review(&lid, &sid, &limits, 4001)?["job"]["job_id"] == first_id
        );
        let claim = store
            .claim_memory_job_for("batch-one", &limits, 4002, first_id)?
            .unwrap();
        store.stage_memory_candidates(&claim["receipt"], &[], 4003)?;
        let second = store.prepare_memory_review(&lid, &sid, &limits, 4004)?;
        ensure!(second["job"]["job_id"] != first_id && second["has_more_sources"] == false);
        let second_id = s(&second["job"], "job_id");
        ensure!(
            store.memory_job(second_id)?["source_snapshot"]
                .as_array()
                .unwrap()
                .len()
                == 2
        );
        let claim = store
            .claim_memory_job_for("batch-two", &limits, 4005, second_id)?
            .unwrap();
        store.stage_memory_candidates(&claim["receipt"], &[], 4006)?;
        ensure!(
            store.prepare_memory_review(&lid, &sid, &limits, 4007)?["job"]["job_id"] == second_id
        );
        ensure!(store.rows("SELECT job_id FROM memory_jobs", [])?.len() == 2);
        ensure!(
            store
                .rows("SELECT job_id FROM memory_job_attempts", [])?
                .len()
                == 2
        );
        Ok(())
    }

    #[test]
    fn target_claim_never_claims_or_reaps_another_profile() -> Result<()> {
        let (_directory, store, lid, sid) = fixture()?;
        let other = store.create_learner("Other review", "en", "guided")?;
        let other_lid = s(&other, "learner_id");
        let other_session = store.create_session(other_lid)?;
        let other_sid = s(&other_session, "session_id");
        source(&store, &lid, &sid, "observation", "当前档案来源")?;
        source(&store, other_lid, other_sid, "observation", "其他档案来源")?;
        let limits = MemoryJobLimits::default();
        let foreign = store.prepare_memory_review(other_lid, other_sid, &limits, 5000)?;
        let current = store.prepare_memory_review(&lid, &sid, &limits, 5001)?;
        let foreign_id = s(&foreign["job"], "job_id");
        let current_id = s(&current["job"], "job_id");
        let claim = store
            .claim_memory_job_for("selected", &limits, 5002, current_id)?
            .unwrap();
        ensure!(claim["job"]["job_id"] == current_id && claim["job"]["learner_id"] == lid);
        ensure!(
            store.memory_job(foreign_id)?["status"] == "pending"
                && store.memory_job(foreign_id)?["attempts"] == 0
        );
        ensure!(
            store
                .claim_memory_job_for("missing", &limits, 5003, "does-not-exist")?
                .is_none()
        );
        ensure!(
            store
                .claim_memory_job("background", &limits, 5004)?
                .unwrap()["job"]["job_id"]
                == foreign_id
        );
        ensure!(store.memory_review_snapshot(other_lid, &sid).is_err());
        let real_directory = tempfile::tempdir()?;
        let real = Store::open(real_directory.path().join("real.sqlite3"), "real")?;
        ensure!(real.memory_review_snapshot(&lid, &sid).is_err());
        ensure!(
            real.prepare_memory_review(&lid, &sid, &limits, 5005)
                .is_err()
        );
        ensure!(
            real.claim_memory_job_for("wrong-namespace", &limits, 5006, current_id)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn repeated_preparation_and_new_context_cannot_reset_reserved_budget() -> Result<()> {
        let (_directory, store, lid, sid) = fixture()?;
        source(&store, &lid, &sid, "observation", "有来源的观察")?;
        let limits = MemoryJobLimits {
            max_attempts: 1,
            daily_reservations: 1,
            session_reservations: 1,
            ..Default::default()
        };
        let first = store.prepare_memory_review(&lid, &sid, &limits, 5500)?;
        let job_id = s(&first["job"], "job_id");
        let claimed = store
            .claim_memory_job_for("only-reservation", &limits, 5501, job_id)?
            .unwrap();
        store.fail_memory_job(&claimed["receipt"], 5502)?;
        let retry = store.prepare_memory_review(&lid, &sid, &limits, 5503)?;
        ensure!(retry["job"]["job_id"] == job_id && retry["job"]["status"] == "exhausted");
        ensure!(
            store
                .claim_memory_job_for("no-retry", &limits, 5504, job_id)?
                .is_none()
        );
        // A new teaching context may reinterpret still-valid evidence; it does
        // not obtain a new session/day reservation merely by getting a new ID.
        store.bump(&sid, "idle")?;
        let next = store.prepare_memory_review(&lid, &sid, &limits, 5505)?;
        ensure!(next["job"]["job_id"] != job_id);
        ensure!(
            store
                .claim_memory_job_for("still-budgeted", &limits, 5506, s(&next["job"], "job_id"))?
                .is_none()
        );
        ensure!(
            store
                .rows("SELECT job_id FROM memory_job_attempts", [])?
                .len()
                == 1
        );
        Ok(())
    }

    #[test]
    fn newer_scheduled_batch_does_not_hide_the_manual_batch_before_review_finishes() -> Result<()> {
        let (_directory, store, lid, sid) = fixture()?;
        let limits = MemoryJobLimits::default();
        let mut events = Vec::new();
        for i in 0..13 {
            events.push(source(
                &store,
                &lid,
                &sid,
                "observation",
                &format!("第{i}条观察"),
            )?);
        }
        let first = store.prepare_memory_review(&lid, &sid, &limits, 5600)?;
        let first_id = s(&first["job"], "job_id");
        ensure!(first["job"]["status"] == "pending");
        let claim = store
            .claim_memory_job_for("manual-first", &limits, 5601, first_id)?
            .unwrap();
        // An automatic discovery tick that passed its idle gate earlier may
        // enqueue the next uncovered batch while this manual job is running.
        store.discover_memory_jobs(
            crate::kimi::memory_contract::PROMPT_VERSION,
            &limits,
            false,
            5602,
        )?;
        let second = store
            .one(
                "SELECT job_id FROM memory_jobs WHERE session_id=? AND job_id!=?",
                params![sid, first_id],
            )?
            .unwrap();
        let second_id = s(&second, "job_id");
        ensure!(store.memory_job(second_id)?["status"] == "pending");
        let during = store.memory_review_snapshot(&lid, &sid)?;
        ensure!(during["job"]["job_id"] == first_id && during["job"]["status"] == "leased");
        ensure!(
            store.prepare_memory_review(&lid, &sid, &limits, 5603)?["job"]["job_id"] == first_id
        );
        let staged = store.stage_memory_candidates(
            &claim["receipt"],
            &[payload(&events[0], "trial_observation", "current_activity")],
            5604,
        )?;
        let ready = store.memory_review_snapshot(&lid, &sid)?;
        ensure!(ready["job"]["job_id"] == first_id && ready["job"]["status"] == "shadow_ready");
        store.review_memory_candidate(
            &lid,
            &sid,
            s(&staged[0], "candidate_id"),
            "dismiss",
            5605,
        )?;
        ensure!(store.memory_job(first_id)?["status"] == "completed");
        ensure!(store.memory_review_snapshot(&lid, &sid)?["job"]["job_id"] == second_id);
        let claim = store
            .claim_memory_job_for("next-batch", &limits, 5606, second_id)?
            .unwrap();
        store.stage_memory_candidates(&claim["receipt"], &[], 5607)?;
        let finished = store.memory_review_snapshot(&lid, &sid)?;
        ensure!(finished["job"]["job_id"] == second_id && finished["job"]["status"] == "completed");
        Ok(())
    }

    #[test]
    fn changed_sources_epochs_and_contexts_reject_old_candidates_and_dont_leak_across_profiles()
    -> Result<()> {
        for change in [
            "source_version",
            "source_delete",
            "epoch",
            "generation",
            "revision",
            "forget",
        ] {
            let (_directory, store, lid, sid) = fixture()?;
            let event = source(&store, &lid, &sid, "observation", "一次来源明确的观察")?;
            let rows = stage(
                &store,
                &lid,
                &sid,
                &[payload(&event, "trial_observation", "current_activity")],
                6000,
            )?;
            let cid = s(&rows[0], "candidate_id");
            let other = store.create_learner("Other owner", "zh", "guided")?;
            let other_sid = s(
                &store.create_session(s(&other, "learner_id"))?,
                "session_id",
            )
            .to_owned();
            ensure!(
                store
                    .review_memory_candidate(
                        s(&other, "learner_id"),
                        &other_sid,
                        cid,
                        "retain",
                        6003
                    )
                    .is_err()
            );
            match change {
                "source_version" => {
                    store.db.execute(
                        "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
                        [s(&event, "event_id")],
                    )?;
                }
                "source_delete" => {
                    store.db.execute(
                        "DELETE FROM events WHERE event_id=?",
                        [s(&event, "event_id")],
                    )?;
                }
                "epoch" => {
                    store.db.execute(
                        "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
                        [&lid],
                    )?;
                }
                "generation" => {
                    store.bump(&sid, "idle")?;
                }
                "revision" => {
                    store.checkpoint(&sid, &store.session(&sid)?["state"])?;
                }
                "forget" => {
                    store.forget(&lid, None)?;
                }
                _ => unreachable!(),
            }
            ensure!(
                store
                    .review_memory_candidate(&lid, &sid, cid, "retain", 6004)
                    .is_err(),
                "{change}"
            );
            ensure!(
                store
                    .review_memory_candidate(&lid, &sid, cid, "dismiss", 6004)
                    .is_err(),
                "{change}"
            );
            ensure!(
                store.memory_review_snapshot(&lid, &sid)?["candidates"] == json!([]),
                "{change}"
            );
            ensure!(store.memories(&lid)?.is_empty(), "{change}");
        }
        Ok(())
    }

    #[test]
    fn explicit_saved_preference_is_not_reorganized_or_persisted_as_a_duplicate() -> Result<()> {
        let (_directory, store, lid, sid) = fixture()?;
        let saved = store.save_help_preference(
            &lid,
            &sid,
            &json!({"request_id":"saved-preference","content":"以后先给提示",
            "expected_epoch":0,"expected_generation":0}),
        )?;
        let limits = MemoryJobLimits::default();
        ensure!(store.prepare_memory_review(&lid, &sid, &limits, 7000)?["job"].is_null());
        let event = store
            .source_events(&lid, &[s(&saved, "event_id").to_owned()])?
            .remove(0);
        // An existing background job may predate the manual source filter.
        let job = store.enqueue_memory_job(
            &lid,
            &sid,
            &[s(&event, "event_id").to_owned()],
            crate::kimi::memory_contract::PROMPT_VERSION,
            &limits,
            7001,
        )?;
        let claim = store
            .claim_memory_job_for("existing-background", &limits, 7002, s(&job, "job_id"))?
            .unwrap();
        let staged = store.stage_memory_candidates(
            &claim["receipt"],
            &[payload(&event, "help_preference", "cross_session_explicit")],
            7003,
        )?;
        ensure!(store.memory_review_snapshot(&lid, &sid)?["candidates"][0]["can_retain"] == false);
        ensure!(
            store
                .review_memory_candidate(&lid, &sid, s(&staged[0], "candidate_id"), "retain", 7004)
                .is_err()
        );
        ensure!(
            store.memories(&lid)?.len() == 1
                && store.memories(&lid)?[0]["memory_id"] == saved["memory_id"]
        );
        Ok(())
    }
}

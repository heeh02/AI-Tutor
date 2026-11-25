//! Educational candidate semantics. Importance affects usefulness, never source
//! credibility; promotion is explicitly enabled by kind, not by an LLM score.
use super::*;

const KINDS: &[&str] = &[
    "learning_goal",
    "context_constraint",
    "trial_observation",
    "concept_evidence",
    "revisable_misconception",
    "help_preference",
    "accepted_pending_experiment",
    "correction",
];
const IMPORTANCE: &[&str] = &[
    "goal_relevance",
    "continuity",
    "novel_change",
    "explicit_request",
];

impl Store {
    pub(super) fn validate_education_candidate(
        &self,
        input: &Value,
        available: &[Value],
    ) -> Result<Value> {
        let object = input
            .as_object()
            .ok_or_else(|| anyhow!("教育记忆候选必须为对象"))?;
        ensure!(
            object.keys().all(|key| [
                "kind",
                "content",
                "scope",
                "claim_type",
                "source_event_ids",
                "source_event_versions",
                "source_quotes",
                "support_level",
                "importance",
                "source_grade",
                "source_count"
            ]
            .contains(&key.as_str())),
            "教育记忆候选含未知字段"
        );
        let kind = s(input, "kind");
        ensure!(KINDS.contains(&kind), "教育记忆类型无效");
        ensure!(
            !s(input, "content").trim().is_empty() && s(input, "content").chars().count() <= 600,
            "教育记忆文本长度无效"
        );
        ensure!(
            [
                "current_session",
                "current_activity",
                "cross_session_explicit"
            ]
            .contains(&s(input, "scope")),
            "教育记忆范围无效"
        );
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
        ensure!(s(input, "claim_type") == claim, "记忆类型与陈述等级不符");
        ensure!(
            ["unknown", "independent", "hinted", "scaffolded"].contains(&s(input, "support_level")),
            "提示程度无效"
        );
        let ids = strings(&input["source_event_ids"]);
        let unique: HashSet<_> = ids.iter().collect();
        ensure!(
            !ids.is_empty() && ids.len() <= 8 && ids.len() == unique.len(),
            "来源必须唯一且数量有界"
        );
        let sources: Vec<_> = available
            .iter()
            .filter(|row| ids.iter().any(|id| id == s(row, "event_id")))
            .cloned()
            .collect();
        ensure!(
            sources.len() == ids.len() && self.candidate_versions_match(input, &sources)?,
            "教育记忆来源或版本不匹配"
        );
        ensure!(
            input["source_event_versions"].is_object(),
            "教育记忆必须绑定来源版本"
        );
        ensure!(
            sources.iter().all(|row| s(row, "event_status") == "active"
                && !["assistant_suggestion", "model_summary", "memory_summary"]
                    .contains(&s(row, "event_kind"))
                && ["learner_report", "operator_confirmed", "simulated"]
                    .contains(&s(row, "source_type"))
                && (self.namespace == "simulator" || s(row, "source_type") != "simulated")),
            "助手说明或摘要不能作为学习事实"
        );
        if kind == "context_constraint" {
            ensure!(
                s(input, "scope") == "current_activity"
                    && s(input, "support_level") == "unknown"
                    && sources
                        .iter()
                        .all(|row| s(row, "event_kind") == "learner_statement"),
                "条件与推断边界只接受活动内的学习者表述，不能推断作答支持程度"
            );
        }
        let quotes = input["source_quotes"]
            .as_array()
            .ok_or_else(|| anyhow!("教育记忆缺少引用原句"))?;
        ensure!(quotes.len() == ids.len(), "每个来源必须提供一条原句");
        let mut quoted = HashSet::new();
        for quote in quotes {
            let eid = s(quote, "event_id");
            ensure!(quoted.insert(eid), "重复引用不能增加证据");
            let source = sources
                .iter()
                .find(|row| s(row, "event_id") == eid)
                .ok_or_else(|| anyhow!("引用来源不在快照中"))?;
            let text = s(quote, "quote");
            ensure!(
                !text.is_empty()
                    && text.chars().count() <= 800
                    && ["text", "report", "correction"].iter().any(|field| s(
                        &source["content"],
                        field
                    )
                    .contains(text)),
                "引用原句与证据不符"
            );
        }
        if matches!(
            kind,
            "trial_observation" | "concept_evidence" | "revisable_misconception"
        ) {
            ensure!(
                sources.iter().all(|row| s(row, "event_kind") != "plan"),
                "计划不能转成试验结果或理解证据"
            );
        }
        if s(input, "support_level") != "unknown" {
            ensure!(
                sources
                    .iter()
                    .all(|row| s(&row["content"], "support_level") == s(input, "support_level")),
                "提示程度必须由全部引用来源明确一致支持，否则应拆分候选或使用 unknown"
            );
        }
        let importance = input["importance"]
            .as_object()
            .ok_or_else(|| anyhow!("教育重要性分项无效"))?;
        ensure!(
            importance.len() == IMPORTANCE.len()
                && IMPORTANCE.iter().all(|key| importance
                    .get(*key)
                    .and_then(Value::as_u64)
                    .is_some_and(|score| score <= 3)),
            "教育重要性必须为四项0至3分"
        );
        let mut value = input.clone();
        // Repeated sources, model scores and retrieval frequency never upgrade
        // a learner report to teacher observation or hardware verification.
        value["source_grade"] = json!(if self.namespace == "simulator" {
            "simulated"
        } else if sources
            .iter()
            .any(|row| s(row, "source_type") == "learner_report")
        {
            "learner_report"
        } else {
            "operator_confirmed"
        });
        value["source_count"] = json!(ids.len());
        Ok(value)
    }

    /// Shared, read-only admission for the review UI and the actual write.
    pub(super) fn validate_education_promotion(
        &self,
        lid: &str,
        payload: &Value,
        available: &[Value],
        epoch: i64,
    ) -> Result<()> {
        ensure!(self.learner(lid)?["epoch"] == epoch, "教育记忆代次已失效");
        let ids = strings(&payload["source_event_ids"]);
        let sources: Vec<_> = available
            .iter()
            .filter(|row| ids.iter().any(|id| id == s(row, "event_id")))
            .collect();
        let kind = s(payload, "kind");
        let scope = s(payload, "scope");
        ensure!(
            scope != "current_session",
            "临时偏好或本次记忆不能升格为长期记忆"
        );
        if kind != "help_preference" {
            ensure!(scope == "current_activity", "该教育记忆必须限定活动范围");
            ensure!(
                self.sources_share_activity(lid, &ids)?,
                "教育记忆来源活动缺失或不一致"
            );
        }
        let valid_types = match kind {
            "learning_goal" => sources.iter().all(|row| {
                ["learner_statement", "teacher_setting"].contains(&s(row, "event_kind"))
            }),
            "context_constraint" => sources
                .iter()
                .all(|row| s(row, "event_kind") == "learner_statement"),
            "trial_observation" => sources
                .iter()
                .all(|row| s(row, "event_kind") == "observation"),
            "concept_evidence" | "revisable_misconception" => sources
                .iter()
                .all(|row| s(row, "event_kind") == "learner_answer"),
            "accepted_pending_experiment" => sources
                .iter()
                .all(|row| s(row, "event_kind") == "plan" && row["content"]["accepted"] == true),
            "help_preference" => {
                scope == "cross_session_explicit"
                    && sources.iter().all(|row| {
                        ["learner_statement", "teacher_setting"].contains(&s(row, "event_kind"))
                            && row["content"]["explicit_preference"] == true
                            && row["content"]["preference_scope"] == "persistent"
                    })
            }
            "correction" => sources
                .iter()
                .all(|row| s(row, "event_kind") == "correction"),
            _ => false,
        };
        ensure!(
            valid_types,
            "来源尚不足以升格此类教育记忆，候选保留为shadow"
        );
        if kind == "help_preference"
            && sources
                .iter()
                .any(|source| source["content"]["confirmation"] == "explicit_save_action")
        {
            let source_ids: HashSet<_> = ids.iter().cloned().collect();
            ensure!(
                !self
                    .active_memories(lid)?
                    .iter()
                    .any(|memory| memory["kind"] == "help_preference"
                        && memory["scope"] == "cross_session_explicit"
                        && strings(&memory["source_event_ids"])
                            .iter()
                            .any(|id| source_ids.contains(id))),
                "这条持续讲解偏好已经明确保存，无需再次保留"
            );
        }
        Ok(())
    }

    pub(super) fn persist_education_candidate(
        &self,
        lid: &str,
        payload: &Value,
        available: &[Value],
        epoch: i64,
    ) -> Result<String> {
        self.validate_education_promotion(lid, payload, available, epoch)?;
        let ids = strings(&payload["source_event_ids"]);
        let kind = s(payload, "kind");
        let scope = s(payload, "scope");
        let memory_kind = match kind {
            "trial_observation" => "reported_progress",
            "accepted_pending_experiment" => "pending_experiment",
            other => other,
        };
        let prefix = match kind {
            "learning_goal" => "明确表达的学习目标",
            "context_constraint" => "特定任务的条件与推断边界",
            "trial_observation" => "据报告的尝试与观察",
            "concept_evidence" => "本次回答证据，不代表已掌握",
            "revisable_misconception" => "待确认、可修订的理解线索",
            "help_preference" => "明确要求的持续帮助偏好",
            "accepted_pending_experiment" => "已接受、尚待完成的实验",
            "correction" => "明确更正",
            _ => unreachable!(),
        };
        let content = format!("{prefix}：{}", s(payload, "content"));
        for previous in self.active_memories(lid)? {
            if s(&previous, "kind") == memory_kind
                && s(&previous, "scope") == scope
                && s(&previous, "content") == content
                && strings(&previous["source_event_ids"])
                    .into_iter()
                    .collect::<HashSet<_>>()
                    == ids.iter().cloned().collect()
            {
                return Ok(s(&previous, "memory_id").to_owned());
            }
        }
        let mut supersedes = vec![];
        if kind == "help_preference" {
            for old in self.rows("SELECT memory_id FROM memories WHERE learner_id=? AND status='active' AND kind='help_preference'",[lid])? {
                supersedes.push(s(&old,"memory_id").to_owned());
            }
            for old in &supersedes {
                self.db.execute(
                    "UPDATE memories SET status='superseded' WHERE memory_id=?",
                    [old],
                )?;
            }
        }
        let id = uid("memory");
        let timestamp = now();
        self.db.execute(
            "INSERT INTO memories VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                lid,
                "modular_rover",
                memory_kind,
                content,
                json!(ids).to_string(),
                s(payload, "source_grade"),
                scope,
                timestamp,
                timestamp,
                "active",
                json!(supersedes).to_string(),
                epoch
            ],
        )?;
        self.link_memory_sources(&id)?;
        self.link_memory_activity(&id)?;
        Ok(id)
    }

    pub(super) fn education_memory_metadata(&self, lid: &str, mid: &str) -> Result<Option<Value>> {
        let Some(row)=self.one("SELECT payload FROM education_memory_candidates WHERE learner_id=? AND namespace=? AND memory_id=? AND status='applied' AND payload IS NOT NULL ORDER BY rowid DESC LIMIT 1",params![lid,self.namespace,mid])? else{return Ok(None);};
        let payload: Value = serde_json::from_str(s(&row, "payload"))?;
        let mut metadata = json!({});
        for key in [
            "kind",
            "claim_type",
            "support_level",
            "importance",
            "source_grade",
            "source_count",
        ] {
            metadata[key] = payload[key].clone();
        }
        Ok(Some(metadata))
    }
}

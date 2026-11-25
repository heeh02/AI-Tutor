//! Bounded A/B comparison receipts. Source events own every business text;
//! receipts retain only identity, a payload digest, and source/version bindings.
use super::*;
use sha2::{Digest, Sha256};

const SOURCE_COLUMNS: [(&str, &str, &str); 3] = [
    ("observation_id", "observation_version", "observation"),
    ("untested_id", "untested_version", "plan"),
    ("pending_id", "pending_version", "plan"),
];

fn keys(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("比较记录必须为对象"))?;
    ensure!(
        object.keys().all(|key| allowed.contains(&key.as_str())),
        "比较记录含未知字段"
    );
    Ok(())
}

fn bounded_text<'a>(value: &'a Value, field: &str, max: usize) -> Result<&'a str> {
    let text = value[field]
        .as_str()
        .ok_or_else(|| anyhow!("比较记录字段 {field} 必须为文本"))?;
    ensure!(
        !text.trim().is_empty() && text.chars().count() <= max,
        "比较记录字段 {field} 长度无效"
    );
    Ok(text)
}

fn normalized_payload(data: &Value, namespace: &str) -> Result<Value> {
    keys(
        data,
        &[
            "question",
            "changed_variable",
            "conditions",
            "trials",
            "pending_plan",
            "source_type",
            "submission_id",
        ],
    )?;
    let question = bounded_text(data, "question", 200)?;
    let changed_variable = bounded_text(data, "changed_variable", 200)?;
    let submission_id = bounded_text(data, "submission_id", 96)?;
    let source = bounded_text(data, "source_type", 30)?;
    ensure!(
        ["learner_report", "operator_confirmed", "simulated"].contains(&source),
        "比较记录来源无效"
    );
    ensure!(
        namespace == "simulator" || source != "simulated",
        "模拟记录只能写入模拟档案"
    );
    let conditions = data["conditions"]
        .as_array()
        .ok_or_else(|| anyhow!("共同条件必须为列表"))?;
    ensure!(
        (1..=8).contains(&conditions.len()),
        "共同条件数量必须为1至8项"
    );
    let mut clean_conditions = Vec::new();
    for condition in conditions {
        keys(condition, &["name", "status"])?;
        let name = bounded_text(condition, "name", 200)?;
        let status = s(condition, "status");
        ensure!(
            ["reported_same", "not_confirmed"].contains(&status),
            "共同条件状态无效"
        );
        clean_conditions.push(json!({"name":name,"status":status}));
    }
    let trials = data["trials"]
        .as_array()
        .ok_or_else(|| anyhow!("试验必须为列表"))?;
    ensure!(trials.len() == 2, "比较必须包含A和B两项试验");
    let mut clean_trials = Vec::new();
    for label in ["A", "B"] {
        let matching: Vec<_> = trials
            .iter()
            .filter(|trial| s(trial, "label") == label)
            .collect();
        ensure!(matching.len() == 1, "试验标签必须唯一且为A/B");
        let trial = matching[0];
        keys(
            trial,
            &["label", "setting", "status", "result", "trial_count"],
        )?;
        let setting = bounded_text(trial, "setting", 200)?;
        let status = s(trial, "status");
        ensure!(["reported", "not_tested"].contains(&status), "试验状态无效");
        let result = if status == "reported" {
            json!(bounded_text(trial, "result", 1000)?)
        } else {
            ensure!(
                trial["result"].is_null() && trial["trial_count"].is_null(),
                "尚未测试的试验不能填写结果或次数"
            );
            Value::Null
        };
        let count = if trial["trial_count"].is_null() {
            Value::Null
        } else {
            let count = trial["trial_count"]
                .as_u64()
                .ok_or_else(|| anyhow!("试验次数必须为整数"))?;
            ensure!((1..=10000).contains(&count), "试验次数必须为1至10000或未知");
            json!(count)
        };
        clean_trials.push(json!({"label":label,"setting":setting,"status":status,"result":result,"trial_count":count}));
    }
    let plan = if data["pending_plan"].is_null() {
        Value::Null
    } else {
        let plan = &data["pending_plan"];
        keys(plan, &["text", "accepted"])?;
        let text = bounded_text(plan, "text", 1000)?;
        let accepted = match plan.get("accepted") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| anyhow!("接受计划必须为布尔值"))?,
        };
        json!({"text":text,"accepted":accepted})
    };
    // Preserve requested source in the digest. In simulator the persisted
    // effective source is forced separately; changing the request still clashes.
    Ok(
        json!({"question":question,"changed_variable":changed_variable,"conditions":clean_conditions,
        "trials":clean_trials,"pending_plan":plan,"source_type":source,"submission_id":submission_id}),
    )
}

fn payload_digest(payload: &Value) -> String {
    format!("{:x}", Sha256::digest(payload.to_string().as_bytes()))
}

fn trial_text(context: &Value, trials: &[Value], reported: bool) -> String {
    let conditions = context["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|condition| {
            format!(
                "{}（{}）",
                s(condition, "name"),
                if condition["status"] == "reported_same" {
                    "据报告保持一致"
                } else {
                    "尚未确认"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    let mut text = format!(
        "比较问题：{}\n改变的变量：{}\n共同条件：{}\n",
        s(context, "question"),
        s(context, "changed_variable"),
        conditions
    );
    for trial in trials {
        if reported {
            let count = trial["trial_count"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "未知".to_owned());
            text.push_str(&format!(
                "{}：设置={}；据报告结果={}；报告次数={}。\n",
                s(trial, "label"),
                s(trial, "setting"),
                s(trial, "result"),
                count
            ));
        } else {
            text.push_str(&format!(
                "{}：计划设置={}；尚未测试，无结果、无已做次数。\n",
                s(trial, "label"),
                s(trial, "setting")
            ));
        }
    }
    text.push_str("仅记录报告或待测内容，未核验硬件，也未判定活动完成。");
    text
}

impl Store {
    pub(super) fn migrate_experiments(&self) -> Result<()> {
        self.db.execute_batch("
            CREATE TABLE IF NOT EXISTS experiment_comparisons(
                comparison_id TEXT PRIMARY KEY, learner_id TEXT NOT NULL, session_id TEXT NOT NULL,
                namespace TEXT NOT NULL, submission_id TEXT NOT NULL, payload_digest TEXT NOT NULL,
                source_type TEXT NOT NULL, created_at TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1,
                observation_id TEXT, observation_version INTEGER, untested_id TEXT, untested_version INTEGER,
                pending_id TEXT, pending_version INTEGER,
                UNIQUE(namespace,session_id,submission_id)
            );
            CREATE INDEX IF NOT EXISTS comparisons_session ON experiment_comparisons(learner_id,session_id,active);
            CREATE INDEX IF NOT EXISTS comparison_observation_source ON experiment_comparisons(observation_id);
            CREATE INDEX IF NOT EXISTS comparison_untested_source ON experiment_comparisons(untested_id);
            CREATE INDEX IF NOT EXISTS comparison_pending_source ON experiment_comparisons(pending_id);
            CREATE TRIGGER IF NOT EXISTS comparison_event_delete AFTER DELETE ON events BEGIN
                UPDATE experiment_comparisons SET active=0 WHERE OLD.event_id IN(observation_id,untested_id,pending_id);
            END;
            CREATE TRIGGER IF NOT EXISTS comparison_source_invalidated AFTER UPDATE OF status,version ON education_event_meta
            WHEN NEW.status!='active' OR NEW.version!=OLD.version BEGIN
                UPDATE experiment_comparisons SET active=0 WHERE NEW.event_id IN(observation_id,untested_id,pending_id);
            END;
            CREATE TRIGGER IF NOT EXISTS comparison_source_delete AFTER DELETE ON education_event_meta BEGIN
                UPDATE experiment_comparisons SET active=0 WHERE OLD.event_id IN(observation_id,untested_id,pending_id);
            END;
            CREATE TRIGGER IF NOT EXISTS comparison_session_delete AFTER DELETE ON sessions BEGIN
                DELETE FROM experiment_comparisons WHERE session_id=OLD.session_id;
            END;
            CREATE TRIGGER IF NOT EXISTS comparison_learner_delete AFTER DELETE ON learners BEGIN
                DELETE FROM experiment_comparisons WHERE learner_id=OLD.learner_id;
            END;
        ")?;
        let wrong: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM experiment_comparisons WHERE namespace!=?",
            [&self.namespace],
            |row| row.get(0),
        )?;
        ensure!(wrong == 0, "比较记录数据空间不一致");
        Ok(())
    }

    fn comparison_scope(&self, lid: &str, sid: &str, writable: bool) -> Result<()> {
        self.learner(lid)?;
        let row = self
            .one(
                "SELECT learner_id,status FROM sessions WHERE session_id=?",
                [sid],
            )?
            .ok_or_else(|| anyhow!("比较记录会话不存在"))?;
        ensure!(s(&row, "learner_id") == lid, "比较记录与学习者不匹配");
        ensure!(
            !writable || s(&row, "status") != "finished",
            "已结束会话不能新增比较记录"
        );
        Ok(())
    }

    pub(super) fn project_comparison(&self, receipt: &Value) -> Result<Value> {
        ensure!(
            receipt["active"] == 1 && s(receipt, "namespace") == self.namespace,
            "比较记录来源已失效"
        );
        let lid = s(receipt, "learner_id");
        let sid = s(receipt, "session_id");
        let ids: Vec<_> = SOURCE_COLUMNS
            .iter()
            .filter_map(|(id, _, _)| receipt[*id].as_str().map(str::to_owned))
            .collect();
        let sources = self.source_events(lid, &ids)?;
        ensure!(
            !ids.is_empty() && sources.len() == ids.len(),
            "比较记录来源已失效"
        );
        let mut context = Value::Null;
        let mut trials = Vec::new();
        let mut pending = Value::Null;
        for (id_column, version_column, kind) in SOURCE_COLUMNS {
            let Some(id) = receipt[id_column].as_str() else {
                continue;
            };
            let source = sources
                .iter()
                .find(|source| s(source, "event_id") == id)
                .ok_or_else(|| anyhow!("比较记录来源缺失"))?;
            ensure!(
                s(source, "session_id") == sid
                    && source["event_version"] == receipt[version_column]
                    && s(source, "event_kind") == kind
                    && source["source_type"] == receipt["source_type"],
                "比较记录来源绑定已失效"
            );
            let content = &source["content"];
            ensure!(
                content["comparison_id"] == receipt["comparison_id"],
                "比较记录来源标识不符"
            );
            if id_column == "pending_id" {
                pending = content["pending_plan"].clone();
            } else {
                let source_context = &content["comparison_context"];
                if context.is_null() {
                    context = source_context.clone();
                }
                ensure!(context == *source_context, "比较记录条件来源不一致");
                trials.extend(
                    content["trials"]
                        .as_array()
                        .ok_or_else(|| anyhow!("比较记录来源格式无效"))?
                        .iter()
                        .cloned(),
                );
            }
        }
        trials.sort_by(|a, b| s(a, "label").cmp(s(b, "label")));
        ensure!(
            trials.len() == 2 && trials[0]["label"] == "A" && trials[1]["label"] == "B",
            "比较记录试验来源不完整"
        );
        Ok(
            json!({"comparison_id":receipt["comparison_id"],"created_at":receipt["created_at"],
            "source_type":receipt["source_type"],"hardware_verified":false,"submission_id":receipt["submission_id"],
            "question":context["question"],"changed_variable":context["changed_variable"],"conditions":context["conditions"],
            "trials":trials,"pending_plan":pending}),
        )
    }

    fn replay_validated_comparison(
        &self,
        lid: &str,
        sid: &str,
        payload: &Value,
    ) -> Result<Option<Value>> {
        let Some(receipt) = self.one("SELECT * FROM experiment_comparisons WHERE learner_id=? AND session_id=? AND namespace=? AND submission_id=?", params![lid,sid,self.namespace,s(payload,"submission_id")])? else { return Ok(None); };
        ensure!(
            s(&receipt, "payload_digest") == payload_digest(payload),
            "相同提交标识不能用于不同的比较内容"
        );
        let item = self.project_comparison(&receipt)?;
        Ok(Some(json!({"learner_id":lid,"session_id":sid,"item":item})))
    }

    /// Read-only preflight for the server before cancelling foreground work.
    /// Even a first submission validates scope and the entire payload here.
    pub fn comparison_replay(&self, lid: &str, sid: &str, data: &Value) -> Result<Option<Value>> {
        self.comparison_scope(lid, sid, true)?;
        let payload = normalized_payload(data, &self.namespace)?;
        self.replay_validated_comparison(lid, sid, &payload)
    }

    pub fn comparisons(&self, lid: &str, sid: &str) -> Result<Value> {
        let page = self.comparisons_page(lid, sid, None, 10)?;
        Ok(json!({"learner_id":lid,"session_id":sid,"items":page["items"]}))
    }

    /// Invalid sources are filtered before paging, and every returned receipt
    /// is projected from live sources. The cursor never selects an activity.
    pub fn comparisons_page(
        &self,
        lid: &str,
        sid: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Value> {
        ensure!((1..=20).contains(&limit), "比较记录页大小必须为1至20");
        ensure!(before.is_none_or(|cursor| cursor > 0), "比较记录游标无效");
        self.comparison_scope(lid, sid, false)?;
        let mut sql = String::from(
            "SELECT r.rowid AS cursor,r.* FROM experiment_comparisons r WHERE r.learner_id=? AND r.session_id=? AND r.namespace=? AND r.active=1 AND (? IS NULL OR r.rowid<?)",
        );
        for (id, version, kind) in SOURCE_COLUMNS {
            sql.push_str(&format!(" AND (r.{id} IS NULL OR EXISTS(SELECT 1 FROM events e JOIN education_event_meta m ON m.event_id=e.event_id AND m.learner_id=e.learner_id AND m.session_id=e.session_id WHERE e.event_id=r.{id} AND e.learner_id=r.learner_id AND e.session_id=r.session_id AND e.source_type=r.source_type AND m.namespace=r.namespace AND m.status='active' AND m.version=r.{version} AND m.kind='{kind}'))"));
        }
        sql.push_str(" ORDER BY r.rowid DESC LIMIT ?");
        let mut rows = self.rows(
            &sql,
            params![lid, sid, self.namespace, before, before, (limit + 1) as i64],
        )?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = if more {
            rows.last().and_then(|row| row["cursor"].as_i64())
        } else {
            None
        };
        let items = rows
            .iter()
            .map(|receipt| self.project_comparison(receipt))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"learner_id":lid,"session_id":sid,"namespace":self.namespace,
            "items":items,"next_before":next}))
    }

    /// Atomic source creation + receipt + current reported/accepted state.
    /// Caller owns foreground cancellation; this never invokes a model or device.
    pub fn save_comparison(&self, lid: &str, sid: &str, data: &Value) -> Result<Value> {
        self.tx(|| {
            self.comparison_scope(lid,sid,true)?;
            let payload = normalized_payload(data,&self.namespace)?;
            if let Some(replay) = self.replay_validated_comparison(lid,sid,&payload)? { return Ok(replay); }
            let comparison_id = uid("comparison");
            let source = if self.namespace == "simulator" { "simulated" } else { s(&payload,"source_type") };
            let context = json!({"question":payload["question"],"changed_variable":payload["changed_variable"],"conditions":payload["conditions"]});
            let trials = payload["trials"].as_array().unwrap();
            let reported: Vec<_> = trials.iter().filter(|trial| trial["status"] == "reported").cloned().collect();
            let untested: Vec<_> = trials.iter().filter(|trial| trial["status"] == "not_tested").cloned().collect();
            let mut observation = None;
            let mut untested_source = None;
            if !reported.is_empty() {
                let text = trial_text(&context,&reported,true);
                observation = Some(self.record_education_event(lid,sid,source,"observation",&json!({"comparison_id":comparison_id,"comparison_context":context,"trials":reported,"text":text,"hardware_verified":false}))?);
            }
            if !untested.is_empty() {
                let text = trial_text(&context,&untested,false);
                untested_source = Some(self.record_education_event(lid,sid,source,"plan",&json!({"comparison_id":comparison_id,"comparison_context":context,"trials":untested,"text":text,"accepted":false,"hardware_verified":false}))?);
            }
            let mut pending = None;
            if !payload["pending_plan"].is_null() {
                let plan = &payload["pending_plan"];
                let mut content = json!({"comparison_id":comparison_id,"pending_plan":plan,"text":plan["text"],"accepted":plan["accepted"],"hardware_verified":false});
                if plan["accepted"] == true { content["pending_experiment"] = plan["text"].clone(); }
                pending = Some(self.record_education_event(lid,sid,source,"plan",&content)?);
            }
            let event_id = |event: &Option<Value>| event.as_ref().map(|event| s(event,"event_id").to_owned());
            let version = |event: &Option<Value>| event.as_ref().and_then(|event| event["event_version"].as_i64());
            self.db.execute("INSERT INTO experiment_comparisons(comparison_id,learner_id,session_id,namespace,submission_id,payload_digest,source_type,created_at,observation_id,observation_version,untested_id,untested_version,pending_id,pending_version) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![comparison_id,lid,sid,self.namespace,s(&payload,"submission_id"),payload_digest(&payload),source,now(),event_id(&observation),version(&observation),event_id(&untested_source),version(&untested_source),event_id(&pending),version(&pending)])?;
            let mut state = self.session(sid)?["state"].clone();
            let mut changed = false;
            if let Some(event) = &observation {
                let text = s(&event["content"],"text");
                state["reported_result"] = json!(text);
                state["reported_result_source"] = json!(source);
                state["field_evidence"]["reported_result"] = json!({"source_event_ids":[event["event_id"]],"hardware_verified":false});
                let attempts = state["attempts"].as_array_mut().ok_or_else(|| anyhow!("活动观察列表格式不正确"))?;
                attempts.push(json!({"report":text,"source_event_ids":[event["event_id"]],"hardware_verified":false,"historical":false,"original_source_type":source}));
                if attempts.len() > 12 { attempts.drain(..attempts.len()-12); }
                changed = true;
            }
            if let Some(event) = pending.as_ref().filter(|event| event["content"]["accepted"] == true) {
                state["pending_step"] = event["content"]["text"].clone();
                state["pending_step_source"] = json!(source);
                state["field_evidence"]["pending_step"] = json!({"source_event_ids":[event["event_id"]],"hardware_verified":false});
                changed = true;
            }
            if changed { self.checkpoint(sid,&state)?; }
            self.replay_validated_comparison(lid,sid,&payload)?.ok_or_else(|| anyhow!("比较记录保存后缺失"))
        })
    }
}

#[cfg(test)]
#[path = "experiments_tests.rs"]
mod tests;

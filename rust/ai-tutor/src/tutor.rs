//! Strict business schema and source-aware candidate application; never commands.
use crate::store::{Store, s, strings};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;
pub(crate) mod citation;
use crate::pending_step;
use citation::{CitationSite, check_citations};
pub const STAGES: &[&str] = &[
    "goal_setting",
    "inventory_check",
    "capability_gap",
    "build_guidance",
    "test_preparation",
    "observation",
    "comparison",
    "reflection",
    "paused",
    "completed",
];
fn keys(value: &Value, required: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("模型回复字段必须为对象"))?;
    ensure!(
        object.len() == required.len() && required.iter().all(|key| object.contains_key(*key)),
        "模型回复包含未允许或缺少的字段"
    );
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str, min: usize, max: usize) -> Result<&'a str> {
    let text = value[key]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("模型回复文本字段类型无效"))?;
    ensure!(
        (min..=max).contains(&text.chars().count()),
        "模型回复文本长度无效"
    );
    Ok(text)
}
pub fn validate_reply(reply: &Value, request: &Value) -> Result<()> {
    keys(
        reply,
        &[
            "schema_version",
            "request_id",
            "reply_text",
            "teaching_move",
            "task_update_candidates",
            "memory_candidates",
            "used_evidence_ids",
            "needs_clarification",
        ],
    )?;
    ensure!(reply["schema_version"] == "1.0", "模型业务协议版本无效");
    ensure!(
        text(reply, "request_id", 1, 128)? == s(request, "request_id"),
        "模型回复请求编号不匹配"
    );
    text(reply, "reply_text", 1, 1600)?;
    ensure!(
        [
            "clarify_goal",
            "ask_inventory",
            "explain_module",
            "give_hint",
            "ask_observation",
            "compare_trials",
            "reflect",
            "resume",
            "pause",
            "answer"
        ]
        .contains(&s(reply, "teaching_move")),
        "教学动作无效"
    );
    ensure!(reply["needs_clarification"].is_boolean(), "澄清字段无效");
    let valid: HashSet<_> = request["context"]["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["event_id"].as_str())
        .map(str::to_owned)
        .collect();
    check_citations(
        &reply["used_evidence_ids"],
        &valid,
        request,
        CitationSite::UsedEvidence,
        0,
        20,
    )?;
    for key in ["task_update_candidates", "memory_candidates"] {
        let array = reply[key]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("模型候选列表无效"))?;
        ensure!(array.len() <= 5, "模型候选数量超出上限");
        for (index, candidate) in array.iter().enumerate() {
            if key == "task_update_candidates" {
                keys(candidate, &["field", "value", "source_event_ids"])?;
                ensure!(
                    ["goal", "stage", "reported_result", "pending_step"]
                        .contains(&s(candidate, "field")),
                    "任务字段未允许"
                );
                text(candidate, "value", 0, 800)?;
                ensure!(
                    s(candidate, "field") != "stage" || STAGES.contains(&s(candidate, "value")),
                    "教学阶段未允许"
                );
            } else {
                keys(candidate, &["kind", "content", "scope", "source_event_ids"])?;
                ensure!(
                    [
                        "reported_progress",
                        "help_preference",
                        "pending_experiment",
                        "correction"
                    ]
                    .contains(&s(candidate, "kind")),
                    "记忆类型未允许"
                );
                ensure!(
                    [
                        "current_session",
                        "current_activity",
                        "cross_session_explicit"
                    ]
                    .contains(&s(candidate, "scope")),
                    "记忆作用域无效"
                );
                text(candidate, "content", 1, 800)?;
            }
            let site = if key == "task_update_candidates" {
                CitationSite::TaskCandidate(index)
            } else {
                CitationSite::MemoryCandidate(index)
            };
            check_citations(&candidate["source_event_ids"], &valid, request, site, 1, 8)?;
        }
    }
    Ok(())
}
pub fn apply_reply(store: &Store, sid: &str, reply: &Value, epoch: i64) -> Result<Value> {
    // A correction may invalidate sources before later candidates are applied.
    // Keep all effects together so a storage error cannot leave a partial reply.
    store.tx(|| apply_reply_inner(store, sid, reply, epoch))
}

// History can support an answer or a source-bound memory, but it cannot become
// a fact or temporary preference of the newly selected session. Checking every
// source also prevents a current question from laundering historical evidence.
fn sources_match_session(sources: &[Value], ids: &[String], sid: &str) -> bool {
    !sources.is_empty()
        && sources.len() == ids.iter().collect::<HashSet<_>>().len()
        && sources.iter().all(|source| s(source, "session_id") == sid)
}

fn apply_reply_inner(store: &Store, sid: &str, reply: &Value, epoch: i64) -> Result<Value> {
    let session = store.session(sid)?;
    ensure!(
        s(&session, "status") != "finished",
        "会话已结束，请开始新活动"
    );
    let lid = s(&session, "learner_id");
    ensure!(
        store.learner(lid)?["epoch"].as_i64() == Some(epoch),
        "记忆代次已失效"
    );
    let mut applied_tasks = 0;
    let mut rejected = 0;
    let mut memories = vec![];
    let mut application_epoch = epoch;
    // Corrections invalidate provenance and the associated fast facts first.
    // Re-read state afterwards so a pre-correction clone cannot restore them.
    for candidate in reply["memory_candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|candidate| {
            s(candidate, "kind") == "correction" && s(candidate, "scope") != "current_session"
        })
    {
        match store.add_memory(lid, candidate, application_epoch)? {
            Some(id) => {
                memories.push(id);
                application_epoch = store.learner(lid)?["epoch"]
                    .as_i64()
                    .unwrap_or(application_epoch);
            }
            None => rejected += 1,
        }
    }
    let mut state = store.session(sid)?["state"].clone();
    for candidate in reply["task_update_candidates"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let field = s(candidate, "field");
        if !["goal", "stage", "reported_result", "pending_step"].contains(&field) {
            bail!("未允许的任务字段")
        }
        if field == "stage" && !STAGES.contains(&s(candidate, "value")) {
            rejected += 1;
            continue;
        }
        let ids = strings(&candidate["source_event_ids"]);
        let sources = store.source_events(lid, &ids)?;
        if !sources_match_session(&sources, &ids, sid) {
            rejected += 1;
            continue;
        }
        let observes_work =
            field == "reported_result" || (field == "stage" && candidate["value"] == "completed");
        if sources.iter().any(|source| {
            s(source, "event_kind") == "plan"
                && (observes_work
                    || (field == "pending_step" && source["content"]["accepted"] == false))
        }) {
            rejected += 1;
            continue;
        }
        if field == "pending_step"
            && !pending_step::supported_current(
                &candidate["value"],
                &candidate["source_event_ids"],
                &sources,
                lid,
                sid,
            )
        {
            // A valid source ID does not ground a newly invented method. Keep
            // the old step and deliver the reply; reject only this candidate.
            rejected += 1;
            continue;
        }
        state[field] = candidate["value"].clone();
        let original_source = if store.namespace == "simulator" {
            "simulated"
        } else if sources
            .iter()
            .any(|row| s(row, "source_type") == "learner_report")
        {
            "learner_report"
        } else {
            "operator_confirmed"
        };
        state[format!("{field}_source")] = json!(original_source);
        state["field_evidence"][field] = json!({"source_event_ids":ids,"status":"model_interpreted_report","hardware_verified":false,"historical":false,"original_source_type":original_source});
        if field == "reported_result" {
            state["attempts"].as_array_mut().unwrap().push(json!({"report":candidate["value"],"source_event_ids":ids,"hardware_verified":false,"historical":false,"original_source_type":original_source}));
            let attempts = state["attempts"].as_array_mut().unwrap();
            if attempts.len() > 12 {
                attempts.drain(..attempts.len() - 12);
            }
        }
        applied_tasks += 1;
    }
    for candidate in reply["memory_candidates"].as_array().into_iter().flatten() {
        if s(candidate, "kind") == "correction" && s(candidate, "scope") != "current_session" {
            continue;
        }
        if s(candidate, "scope") == "current_session" {
            if s(candidate, "kind") == "help_preference" {
                let ids = strings(&candidate["source_event_ids"]);
                let sources = store.source_events(lid, &ids)?;
                if !sources_match_session(&sources, &ids, sid) {
                    rejected += 1;
                    continue;
                }
                state["session_preferences"]["explicit_request"] = candidate["content"].clone();
                state["session_preferences"]["source_event_ids"] =
                    candidate["source_event_ids"].clone();
            }
        } else {
            match store.add_memory(lid, candidate, application_epoch)? {
                Some(id) => memories.push(id),
                None => rejected += 1,
            }
        }
    }
    store.checkpoint(sid, &state)?;
    Ok(
        json!({"task_updates":applied_tasks,"applied_memory_ids":memories,"rejected_candidates":rejected}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply() -> Value {
        json!({"schema_version":"1.0","request_id":"r","reply_text":"距离模块测量距离。","teaching_move":"explain_module","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":["e"],"needs_clarification":false})
    }
    #[test]
    fn storage_failure_rolls_back_correction_sources_epoch_and_candidates() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("atomic-correction.db"), "real")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let old = store.event(lid, sid, "learner_report", &json!({"text":"停车成功"}))?;
        let memory = store.add_memory(lid, &json!({"kind":"reported_progress","content":"停车成功","scope":"current_activity","source_event_ids":[old]}), 0)?.unwrap();
        let current = store.event(
            lid,
            sid,
            "learner_report",
            &json!({"text":"刚才说错了，停车还没有成功"}),
        )?;
        let mut value = reply();
        value["memory_candidates"] = json!([
            {"kind":"correction","content":"停车尚未成功","scope":"current_activity","source_event_ids":[old,current]},
            {"kind":"reported_progress","content":"故障注入","scope":"current_activity","source_event_ids":[current]}
        ]);
        store.db.execute_batch("CREATE TEMP TRIGGER fail_candidate BEFORE INSERT ON memories WHEN NEW.content='故障注入' BEGIN SELECT RAISE(ABORT,'synthetic storage failure'); END;")?;
        ensure!(apply_reply(&store, sid, &value, 0).is_err());
        ensure!(store.db.is_autocommit());
        ensure!(store.learner(lid)?["epoch"] == 0);
        ensure!(store.source_events(lid, &[old.clone()])?.len() == 1);
        ensure!(store.session(sid)?["generation"] == session["generation"]);
        ensure!(store.memories(lid)?.len() == 1 && store.memories(lid)?[0]["memory_id"] == memory);
        store.db.execute_batch("DROP TRIGGER fail_candidate;")?;
        apply_reply(&store, sid, &value, 0)?;
        ensure!(store.learner(lid)?["epoch"] == 1);
        ensure!(store.source_events(lid, &[old])?.is_empty());
        ensure!(store.memories(lid)?.len() == 2);
        Ok(())
    }
    #[test]
    fn rejects_unknown_actuator_or_foreign_evidence() {
        let request = json!({"request_id":"r","context":{"evidence":[{"event_id":"e"}]}});
        let mut value = reply();
        assert!(validate_reply(&value, &request).is_ok());
        value["shell"] = json!("forbidden");
        assert!(validate_reply(&value, &request).is_err());
        let mut value = reply();
        value["used_evidence_ids"] = json!(["foreign"]);
        assert!(validate_reply(&value, &request).is_err());
    }
    #[test]
    fn historical_task_candidate_cannot_replace_current_fast_result() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("source.db"), "simulator")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let first = store.create_session(lid)?;
        let old = store.event(
            lid,
            s(&first, "session_id"),
            "learner_report",
            &json!({"text":"据模拟报告成功停车"}),
        )?;
        let second = store.create_session(lid)?;
        let sid = s(&second, "session_id");
        let mut value = reply();
        value["task_update_candidates"] = json!([{"field":"reported_result","value":"上次模拟报告成功停车","source_event_ids":[old]}]);
        let applied = apply_reply(&store, sid, &value, 0)?;
        let state = store.session(sid)?["state"].clone();
        ensure!(
            applied["task_updates"] == 0
                && applied["rejected_candidates"] == 1
                && state["reported_result"] == ""
                && state["field_evidence"].get("reported_result").is_none()
        );
        ensure!(state["modules"].is_null());
        Ok(())
    }
    #[test]
    fn displayed_memory_only_correction_cannot_revive_old_fact_on_finish() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("correct-finish.db"), "real")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let old_text = "它已经成功停车并亮绿灯";
        let a = store.event(lid, sid, "learner_report", &json!({"text":old_text}))?;
        let old_memory=store.add_memory(lid,&json!({"kind":"reported_progress","content":old_text,"scope":"current_activity","source_event_ids":[a]}),0)?.unwrap();
        let c = store.event(
            lid,
            sid,
            "operator_confirmed",
            &json!({"modules":["control","motor"],"text":"今天只观察电机"}),
        )?;
        let mut state = crate::store::empty_state();
        state["reported_result"] = json!(old_text);
        state["field_evidence"]["reported_result"] = json!({"source_event_ids":[a]});
        state["attempts"] = json!([{"report":old_text,"source_event_ids":[a]}]);
        state["modules"] = json!(["control", "motor"]);
        state["modules_source"] = json!("operator_confirmed");
        state["requires_inventory_confirmation"] = json!(false);
        state["field_evidence"]["modules"] = json!({"source_event_ids":[c]});
        state["goal"] = json!("今天只观察电机");
        state["field_evidence"]["goal"] = json!({"source_event_ids":[c]});
        store.checkpoint(sid, &state)?;
        let old_turn = store.add_turn(
            sid,
            "old",
            0,
            "assistant",
            "以前的成功陈述",
            "shown",
            &[a.clone()],
        )?;
        let b = store.event(
            lid,
            sid,
            "learner_report",
            &json!({"text":"刚才说错了，停车还没有成功"}),
        )?;
        let corrected_text = "已更正：停车尚未成功。今天的模块报告仍保留。";
        let tid = store.add_turn(
            sid,
            "r",
            0,
            "assistant",
            corrected_text,
            "generated",
            &[a.clone(), b.clone()],
        )?;
        store.acknowledge(sid, &tid, "shown")?;
        let mut value = reply();
        value["reply_text"] = json!(corrected_text);
        value["used_evidence_ids"] = json!([a, b]);
        value["memory_candidates"] = json!([{"kind":"correction","content":"学习者更正：停车尚未成功","scope":"current_activity","source_event_ids":[a,b]}]);
        apply_reply(&store, sid, &value, 0)?;
        let current = store.session(sid)?;
        ensure!(
            current["state"]["reported_result"] == "" && current["state"]["attempts"] == json!([])
        );
        ensure!(
            current["state"]["modules"] == json!(["control", "motor"])
                && current["state"]["goal"] == "今天只观察电机"
        );
        let turns = current["turns"].as_array().unwrap();
        ensure!(turns.iter().all(|turn| turn["turn_id"] != old_turn));
        let displayed = turns.iter().find(|turn| turn["turn_id"] == tid).unwrap();
        ensure!(
            displayed["content"] == corrected_text
                && displayed["status"] == "shown"
                && displayed["evidence"] == json!([b])
                && displayed["generation"] == current["generation"]
        );
        store.acknowledge(sid, &tid, "speech_started")?;
        ensure!(store.source_events(lid, &[a.clone()])?.is_empty());
        let epoch = store.learner(lid)?["epoch"].as_i64().unwrap();
        ensure!(epoch == 1);
        let stale = json!({"kind":"reported_progress","content":old_text,"scope":"current_activity","source_event_ids":[a]});
        ensure!(
            store.add_memory(lid, &stale, 0)?.is_none()
                && store.add_memory(lid, &stale, epoch)?.is_none()
        );
        store.consolidate_and_finish(sid)?;
        let memories = store.memories(lid)?;
        ensure!(
            memories
                .iter()
                .all(|memory| memory["memory_id"] != old_memory
                    && !s(memory, "content").contains(old_text))
        );
        ensure!(memories.iter().any(|memory| memory["kind"] == "correction"
            && memory["source_event_ids"] == json!([b])
            && memory["source_type"] == "learner_report"));
        Ok(())
    }
    #[test]
    fn correction_rejects_invalidated_task_sources_but_keeps_same_reply_valid_candidates()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("correct-candidates.db"), "simulator")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let a = store.event(lid, sid, "learner_report", &json!({"text":"停车成功"}))?;
        store.add_memory(lid,&json!({"kind":"reported_progress","content":"停车成功","scope":"current_activity","source_event_ids":[a]}),0)?;
        let b = store.event(
            lid,
            sid,
            "learner_report",
            &json!({"text":"刚才说错了，停车尚未成功"}),
        )?;
        let mut value = reply();
        value["task_update_candidates"] = json!([
            {"field":"reported_result","value":"错误的旧成功","source_event_ids":[a]},
            {"field":"goal","value":"接下来检查停车原因","source_event_ids":[b]}
        ]);
        value["memory_candidates"] = json!([
            {"kind":"correction","content":"学习者更正停车尚未成功","scope":"current_activity","source_event_ids":[a,b]},
            {"kind":"reported_progress","content":"当前报告尚未停车成功","scope":"current_activity","source_event_ids":[b]}
        ]);
        let applied = apply_reply(&store, sid, &value, 0)?;
        ensure!(applied["task_updates"] == 1 && applied["rejected_candidates"] == 1);
        ensure!(applied["applied_memory_ids"].as_array().unwrap().len() == 2);
        ensure!(
            store.session(sid)?["state"]["reported_result"] == ""
                && store.session(sid)?["state"]["goal"] == "接下来检查停车原因"
        );
        ensure!(
            store
                .memories(lid)?
                .iter()
                .all(|memory| memory["epoch"] == 1 && memory["source_type"] == "simulated")
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "tutor_historical_tests.rs"]
mod historical_task_tests;

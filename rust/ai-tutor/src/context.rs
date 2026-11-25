//! Whole-record context selection with exact current input and source ownership.
#[path = "context_comparison.rs"]
mod comparison;
use crate::store::{CandidateRanking, QueryEmbedding, Store, s, strings};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
pub const MAX_CONTEXT_CHARS: usize = 18_000;

pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return arg.to_owned();
    }
    let mut result = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            result.push_str(&"\\".repeat(slashes * 2 + 1));
            result.push('"');
        } else {
            result.push_str(&"\\".repeat(slashes));
            result.push(c);
        }
        slashes = 0;
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    result
}
fn fits(context: &Value) -> bool {
    let text = context.to_string();
    let extra = comparison::policy(context);
    let extra_chars = extra.map_or(0, |policy| policy.chars().count() + 2);
    let extra_quoted = extra.map_or(0, |policy| {
        quote_windows_arg(policy).encode_utf16().count() + 2
    });
    text.chars().count() + extra_chars <= MAX_CONTEXT_CHARS - 256
        && quote_windows_arg(&text).encode_utf16().count() + extra_quoted <= 21_000 - 256
}

pub(crate) fn comparison_policy(request: &Value) -> Option<&'static str> {
    comparison::policy(&request["context"])
}
fn fetch(store: &Store, lid: &str, ids: &[String]) -> Result<HashMap<String, Value>> {
    let mut result = HashMap::new();
    for batch in ids.chunks(20) {
        for row in store.source_events(lid, batch)? {
            if s(&row, "learner_id") == lid {
                result.insert(s(&row, "event_id").to_owned(), row);
            }
        }
    }
    Ok(result)
}
fn source(
    row: &Value,
    reference: Option<Value>,
    historical: bool,
    session_depths: &HashMap<String, usize>,
) -> Value {
    let mut result = json!({"event_id":row["event_id"],"source_type":row["source_type"],"event_kind":row["event_kind"],"event_version":row["event_version"],"event_status":row["event_status"],"created_at":row["created_at"],"historical":historical,"original_content_included":reference.is_none()});
    result["session_id"] = row["session_id"].clone();
    result["activity_id"] = row["activity_id"].clone();
    let depth = session_depths.get(s(row, "session_id")).copied();
    result["session_relation"] = json!(match depth {
        Some(0) => "current",
        Some(_) => "selected_ancestor",
        None => "outside_selected_chain",
    });
    result["ancestor_depth"] = json!(depth);
    if let Some(reference) = reference {
        result["content_reference"] = reference
    } else {
        result["content"] = row["content"].clone()
    }
    result
}
fn append_evidence(context: &mut Value, event: Value) {
    let id = s(&event, "event_id").to_owned();
    if !context["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| s(row, "event_id") == id)
    {
        context["evidence"].as_array_mut().unwrap().push(event);
    }
}
fn count(context: &mut Value, key: &str, number: usize) {
    context["context_selection"][key] = json!(number);
}

/// Select retrieval text independently of the teaching goal. Concrete input is
/// kept verbatim; only a complete continuation command borrows the goal. Any
/// future query encoder must consume this same selection, not append the goal.
/// The activity fallback preserves explicit resume without a current goal.
pub fn memory_recall_query<'a>(text: &'a str, goal: &'a str) -> Option<&'a str> {
    if text.trim().is_empty() {
        return None;
    }
    let command = text
        .trim()
        .trim_end_matches(|c| matches!(c, '.' | '!' | '?' | '。' | '！' | '？' | '…'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if ["继续", "上次", "接着", "resume", "continue", "last time"].contains(&command.as_str())
    {
        Some(if goal.trim().is_empty() {
            "小车活动"
        } else {
            goal
        })
    } else {
        Some(text)
    }
}

pub fn build_context(
    store: &Store,
    session: &Value,
    learner: &Value,
    text: &str,
    eid: &str,
    vision: &Value,
) -> Result<Value> {
    build_context_inner(store, session, learner, text, eid, vision, None)
}

/// Optional local semantic selection. The caller computes the vector before
/// taking Store locks; the text must be exactly the production recall query.
/// Candidate scores stay outside the model context and never imply confidence.
#[allow(clippy::too_many_arguments)]
pub fn build_context_with_semantic(
    store: &Store,
    session: &Value,
    learner: &Value,
    text: &str,
    eid: &str,
    vision: &Value,
    query_text: &str,
    embedding: QueryEmbedding<'_>,
    ranking: CandidateRanking,
) -> Result<Value> {
    build_context_inner(
        store,
        session,
        learner,
        text,
        eid,
        vision,
        Some((query_text, embedding, ranking)),
    )
}

fn build_context_inner(
    store: &Store,
    session: &Value,
    learner: &Value,
    text: &str,
    eid: &str,
    vision: &Value,
    semantic: Option<(&str, QueryEmbedding<'_>, CandidateRanking)>,
) -> Result<Value> {
    let query = memory_recall_query(text, s(&session["state"], "goal"))
        .ok_or_else(|| anyhow::anyhow!("请输入内容"))?;
    if let Some((query_text, _, _)) = &semantic {
        ensure!(
            *query_text == query,
            "Semantic query text differs from the production recall query"
        );
    }
    let lid = s(learner, "learner_id");
    let sid = s(session, "session_id");
    ensure!(
        s(session, "learner_id") == lid,
        "教学上下文学习者归属不匹配"
    );
    let current = fetch(store, lid, &[eid.to_owned()])?;
    let event = current
        .get(eid)
        .ok_or_else(|| anyhow::anyhow!("当前输入缺少有效来源"))?;
    ensure!(
        s(event, "session_id") == sid && s(&event["content"], "text") == text,
        "当前输入与来源不一致"
    );
    // Resolve once from live selection receipts. A summary's historical flag
    // does not establish ancestry (same-session slow memories are summaries too).
    let session_depths = store.selected_session_depths(sid)?;
    let state = &session["state"];
    let mut task = json!({});
    for key in [
        "goal",
        "stage",
        "reported_result",
        "pending_step",
        "modules",
        "modules_source",
        "requires_inventory_confirmation",
        "session_preferences",
        "last_delivered_hint",
    ] {
        if let Some(value) = state.get(key) {
            task[key] = value.clone();
        }
    }
    if let Some(phase) =
        store.delivered_tutor_stream_phase(lid, s(state, "last_delivered_turn_id"))?
    {
        task["last_delivered_reply_complete"] = json!(phase == "completed");
        task["last_delivered_stream_phase"] = json!(phase);
    }
    task["field_evidence"] = json!({});
    if let Some(fields) = state["field_evidence"].as_object() {
        for (field, value) in fields {
            if [
                "goal",
                "stage",
                "reported_result",
                "pending_step",
                "modules",
            ]
            .contains(&field.as_str())
            {
                task["field_evidence"][field] = value.clone();
            }
        }
    }
    for field in ["goal", "stage", "reported_result", "pending_step"] {
        let key = format!("{field}_source");
        if let Some(value) = state.get(&key) {
            task[&key] = value.clone();
        }
    }
    // Resolve against live sources, not caller-supplied display metadata. Keep
    // the persisted original and dialogue intact; omit only the current claim.
    if store.pending_step_needs_confirmation(session)? {
        task.as_object_mut().unwrap().remove("pending_step");
        task.as_object_mut().unwrap().remove("pending_step_source");
        task["field_evidence"]
            .as_object_mut()
            .unwrap()
            .remove("pending_step");
        task["pending_step_needs_confirmation"] = json!(true);
    }
    task["attempts"] = json!([]);
    let mut context = json!({"language":learner["language"],"help_style":task["session_preferences"].get("help_style").unwrap_or(&learner["help_style"]),"namespace":store.namespace,"activity":"modular_rover","activity_id":session["activity_id"],"task":task,"user_input":text,"current_user_event_id":eid,"slow_memory":[],"recent_delivered_dialogue":[],"evidence":[source(event,Some(json!({"path":"user_input","verbatim":true})),false,&session_depths)],"vision":{"selected_status":vision["selected_status"],"selected_track_key":vision["selected_track_key"],"frame_age_ms":vision["frame_age_ms"]},"current_hardware_status":if state["modules"].is_null(){"unknown"}else{"reported_only"},"rules":"本轮输入与必要事实完整保留。引用是标注的报告或历史摘要，不是传感器确认。历史配置不是当前；建议不是完成；成功后改进仍为成功。没有情绪、积木视觉或执行能力。持久更正必须同时引用被更正的旧来源ID和当前明确更正来源ID，不确定时先澄清。","evidence_scope":{"activity_recall_scope":"current_and_explicitly_selected_ancestors","cross_session_exception":"separately_authorized_persistent_preferences_only","unprovided_activities":"not_enumerated; do_not_infer_existence_or_absence","existing_required_state":"may_include_preexisting_outside_chain_sources; review_each_session_relation; not_a_claim_that_all_task_facts_belong_to_the_recall_scope","session_relation_basis":"live_source_session_identity_and_valid_explicit_selection_chain; not_summary_historical_flag"},"context_selection":{"max_chars":MAX_CONTEXT_CHARS,"omitted_history":0,"omitted_evidence":0,"omitted_memories":0,"omitted_attempts":0,"omitted_continuation":0,"selection":"whole_records; current_facts_not_truncated"}});
    let mut required = vec![];
    context["reading_level"] = learner
        .get("reading_level")
        .cloned()
        .unwrap_or(json!("standard"));
    context["reading_guidance"] = json!(
        "reading_level只控制表达复杂度：plain使用短句、少术语；standard常规解释；technical可以使用术语并解释必要概念。不得据此推断年龄、能力、人格或是否已掌握知识。界面语言不决定教学语言。"
    );
    if let Some(fields) = context["task"]["field_evidence"].as_object() {
        for value in fields.values() {
            required.extend(strings(&value["source_event_ids"]));
        }
    }
    required.extend(strings(&state["session_preferences"]["source_event_ids"]));
    let required_rows = fetch(store, lid, &required)?;
    ensure!(
        required.iter().all(|id| required_rows.contains_key(id)),
        "活动状态引用了无效来源"
    );
    let required_comparisons = store.comparison_context_for_sources(
        lid,
        &required_rows.values().cloned().collect::<Vec<_>>(),
    )?;
    for (id, row) in required_rows {
        let fields: Vec<_> = context["task"]["field_evidence"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, value)| strings(&value["source_event_ids"]).contains(&id))
            .map(|(field, _)| field.clone())
            .collect();
        append_evidence(
            &mut context,
            source(
                &row,
                Some(
                    json!({"task_fields":fields,"verbatim":false,"meaning":"model-interpreted report; source retained locally"}),
                ),
                s(&row, "session_id") != sid,
                &session_depths,
            ),
        );
    }
    let binding = comparison::attach(
        store,
        lid,
        sid,
        &mut context,
        &required_comparisons,
        &session_depths,
    )?;
    comparison::bind(&mut context["task"], &binding);
    ensure!(
        fits(&context),
        "完整输入与必要事实超过本机上下文上限；未截断事实，请缩短输入或开始新活动"
    );
    // The explicit choice is current; the selected goal and plan remain history.
    // Resolve again from Store rather than trusting a caller-supplied snapshot.
    let continuation = store.session_continuation(sid)?;
    if !continuation.is_null() {
        let mut trial = context.clone();
        trial["continuation"] = continuation.clone();
        let mut ids = vec![s(&continuation, "selection_event_id").to_owned()];
        for field in ["goal", "pending_step", "pending_resolution"] {
            ids.extend(strings(&continuation[field]["source_event_ids"]));
        }
        ids.sort();
        ids.dedup();
        let rows = fetch(store, lid, &ids)?;
        ensure!(rows.len() == ids.len(), "续做历史来源已失效");
        let resolution_ids: HashSet<_> =
            strings(&continuation["pending_resolution"]["source_event_ids"])
                .into_iter()
                .collect();
        for row in rows.values() {
            let reference = if resolution_ids.contains(s(row, "event_id")) {
                None
            } else {
                Some(json!({"path":"continuation","verbatim":false,
                "meaning":"explicit selection and sourced historical goal/plan; not completed work or current hardware"}))
            };
            append_evidence(
                &mut trial,
                source(row, reference, s(row, "session_id") != sid, &session_depths),
            );
        }
        let projection = store
            .comparison_context_for_sources(lid, &rows.values().cloned().collect::<Vec<_>>())?;
        let binding =
            comparison::attach(store, lid, sid, &mut trial, &projection, &session_depths)?;
        comparison::bind(&mut trial["continuation"], &binding);
        if fits(&trial) {
            context = trial;
        } else {
            // Keep the exact selected identity visible even if optional historical
            // summaries cannot fit beside required current facts.
            let mut compact = context.clone();
            compact["continuation"] = json!({"client_submission_id":continuation["client_submission_id"],
                "source_session_id":continuation["source_session_id"],"source_activity_id":continuation["source_activity_id"],
                "historical":true,"requires_inventory_confirmation":true,"summaries_omitted_for_budget":true});
            if fits(&compact) {
                context = compact;
            }
            count(&mut context, "omitted_continuation", 1);
        }
    }
    let recalled = match semantic {
        Some((_, embedding, ranking)) => {
            // Explicit teaching preferences are instructions from the learner,
            // not topical facts to discard for a low cosine score. Keep their
            // live-source authorization and the original shared whole-record budget.
            let preferences = store.explicit_help_preferences_for_session(sid)?;
            let dense =
                store.recall_candidates_for_session_ranked(sid, query, embedding, 5, ranking)?;
            let mut selected = Vec::new();
            let mut seen = HashSet::new();
            let mut remaining = 2400;
            for memory in preferences
                .into_iter()
                .chain(dense.into_iter().map(|candidate| candidate.memory))
            {
                if selected.len() == 5 {
                    break;
                }
                let size = s(&memory, "content").chars().count();
                let id = s(&memory, "memory_id").to_owned();
                if size <= remaining && seen.insert(id) {
                    remaining -= size;
                    selected.push(memory);
                }
            }
            selected
        }
        None => store.recall_for_session(sid, query, 5)?,
    };
    let mut omitted_memories = 0;
    for memory in recalled {
        ensure!(s(&memory, "learner_id") == lid, "召回包含其他学习者记录");
        let evidence = match store.memory_evidence(lid, s(&memory, "memory_id")) {
            Ok(value) => value,
            Err(_) => {
                omitted_memories += 1;
                continue;
            }
        };
        if evidence["comparison_context"]["status"] == "unavailable" {
            omitted_memories += 1;
            continue;
        }
        let mut compact = json!({"historical":true});
        for key in [
            "memory_id",
            "kind",
            "content",
            "source_type",
            "source_event_ids",
            "scope",
            "status",
            "activity_scope",
        ] {
            compact[key] = memory[key].clone();
        }
        compact["source_event_versions"] = json!({});
        compact["activity_binding"] = evidence["activity_binding"].clone();
        if evidence["education_metadata"].is_object() {
            compact["education_metadata"] = evidence["education_metadata"].clone();
        }
        for link in evidence["source_links"].as_array().into_iter().flatten() {
            compact["source_event_versions"][s(link, "event_id")] = link["event_version"].clone();
        }
        let ids = strings(&compact["source_event_ids"]);
        let rows = fetch(store, lid, &ids)?;
        if rows.len() != ids.iter().collect::<HashSet<_>>().len() {
            omitted_memories += 1;
            continue;
        }
        let mut trial = context.clone();
        let binding = comparison::attach(
            store,
            lid,
            sid,
            &mut trial,
            &evidence["comparison_context"],
            &session_depths,
        )?;
        comparison::bind(&mut compact, &binding);
        trial["slow_memory"]
            .as_array_mut()
            .unwrap()
            .push(compact.clone());
        for row in rows.values() {
            append_evidence(
                &mut trial,
                source(
                    row,
                    Some(
                        json!({"memory_id":compact["memory_id"],"verbatim":false,"meaning":"historical summary, not current hardware"}),
                    ),
                    true,
                    &session_depths,
                ),
            );
        }
        if fits(&trial) {
            context = trial
        } else {
            omitted_memories += 1
        }
    }
    count(&mut context, "omitted_memories", omitted_memories);
    let attempts = state["attempts"].as_array().cloned().unwrap_or_default();
    for attempt in attempts.iter().rev().take(3) {
        let ids = strings(&attempt["source_event_ids"]);
        let rows = fetch(store, lid, &ids)?;
        if rows.len() != ids.iter().collect::<HashSet<_>>().len() {
            continue;
        }
        let mut trial = context.clone();
        let mut value = attempt.clone();
        let projection = store
            .comparison_context_for_sources(lid, &rows.values().cloned().collect::<Vec<_>>())?;
        if projection["status"] == "unavailable" {
            continue;
        }
        let binding =
            comparison::attach(store, lid, sid, &mut trial, &projection, &session_depths)?;
        comparison::bind(&mut value, &binding);
        if value["report"] == state["reported_result"] {
            value.as_object_mut().unwrap().remove("report");
            value["report_reference"] = json!("task.reported_result");
        }
        trial["task"]["attempts"]
            .as_array_mut()
            .unwrap()
            .insert(0, value);
        for row in rows.values() {
            append_evidence(
                &mut trial,
                source(
                    row,
                    Some(json!({"attempt_source_ids":ids,"verbatim":false})),
                    s(row, "session_id") != sid,
                    &session_depths,
                ),
            );
        }
        if fits(&trial) { context = trial } else { break }
    }
    let attempts_included = context["task"]["attempts"].as_array().unwrap().len();
    count(
        &mut context,
        "omitted_attempts",
        attempts.len() - attempts_included,
    );
    let turns: Vec<_> = session["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|turn| {
            [
                "shown",
                "speech_started",
                "speech_completed",
                "speech_interrupted",
            ]
            .contains(&s(turn, "status"))
                && s(turn, "learner_id") == lid
                && !(s(turn, "role") == "user"
                    && strings(&turn["evidence"]).contains(&eid.to_owned()))
        })
        .cloned()
        .collect();
    for turn in turns.iter().rev() {
        let mut value =
            json!({"turn_id":turn["turn_id"],"role":turn["role"],"delivery":turn["status"]});
        if turn["delivery_mode"] == "segments" {
            value["stream_phase"] = turn["stream_phase"].clone();
            value["reply_complete"] = json!(turn["stream_phase"] == "completed");
            value["delivery_scope"] =
                json!("only_acknowledged_displayed_prefix; not_hardware_or_comprehension_evidence");
        }
        if s(turn, "role") == "assistant" && turn["content"] == state["last_delivered_hint"] {
            value["text_reference"] = json!("task.last_delivered_hint")
        } else {
            value["text"] = turn["content"].clone()
        }
        let mut trial = context.clone();
        trial["recent_delivered_dialogue"]
            .as_array_mut()
            .unwrap()
            .insert(0, value);
        if s(turn, "role") == "user" {
            for row in fetch(store, lid, &strings(&turn["evidence"]))?.values() {
                append_evidence(
                    &mut trial,
                    source(
                        row,
                        Some(
                            json!({"turn_id":turn["turn_id"],"verbatim":row["content"]["text"]==turn["content"]}),
                        ),
                        s(row, "session_id") != sid,
                        &session_depths,
                    ),
                );
            }
        }
        if trial["recent_delivered_dialogue"].as_array().unwrap().len() > 8 || !fits(&trial) {
            break;
        }
        context = trial;
    }
    let history_included = context["recent_delivered_dialogue"]
        .as_array()
        .unwrap()
        .len();
    count(
        &mut context,
        "omitted_history",
        turns.len() - history_included,
    );
    let recent = store.recent_evidence(sid)?;
    for row in recent.iter().rev() {
        if s(row, "learner_id") != lid
            || context["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["event_id"] == row["event_id"])
        {
            continue;
        }
        let mut trial = context.clone();
        append_evidence(&mut trial, source(row, None, false, &session_depths));
        if fits(&trial) {
            context = trial
        }
    }
    let included: HashSet<_> = context["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| s(r, "event_id").to_owned())
        .collect();
    count(
        &mut context,
        "omitted_evidence",
        recent
            .iter()
            .filter(|r| s(r, "learner_id") == lid && !included.contains(s(r, "event_id")))
            .count(),
    );
    let chars = context.to_string().chars().count();
    count(&mut context, "chars", chars);
    ensure!(fits(&context), "上下文元数据超过上限；未截断事实");
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserve_long_unicode_and_exact_provenance() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("context.db"), "real")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let session = store.create_session(s(&learner, "learner_id"))?;
        let text = "测距\"😀".repeat(800);
        let event = store.event(
            s(&learner, "learner_id"),
            s(&session, "session_id"),
            "learner_report",
            &json!({"text":text}),
        )?;
        let context = build_context(&store, &session, &learner, &text, &event, &json!({}))?;
        ensure!(context["user_input"] == text);
        ensure!(context.to_string().chars().count() <= 18_000);
        ensure!(
            build_context(&store, &session, &learner, "different", &event, &json!({})).is_err()
        );
        Ok(())
    }
    #[test]
    fn excludes_undelivered_and_other_learner() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("context.db"), "real")?;
        let a = store.create_learner("A", "zh", "one_step")?;
        let session = store.create_session(s(&a, "learner_id"))?;
        let sid = s(&session, "session_id");
        let eid = store.event(
            s(&a, "learner_id"),
            sid,
            "learner_report",
            &json!({"text":"停车"}),
        )?;
        store.add_turn(
            sid,
            "r",
            0,
            "assistant",
            "UNDELIVERED_SECRET",
            "generated",
            &[],
        )?;
        let context = build_context(&store, &store.session(sid)?, &a, "停车", &eid, &json!({}))?;
        ensure!(!context.to_string().contains("UNDELIVERED_SECRET"));
        Ok(())
    }
    #[test]
    fn complete_windows_payload_fits_four_thousand_quotes_and_emoji() -> Result<()> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let policy = std::fs::read_to_string(root.join("prompts/tutor_core.md"))?;
        let schema:Value=serde_json::from_slice(&std::fs::read(root.join("implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"))?)?;
        for text in ["\"".repeat(4000), "😀".repeat(4000)] {
            let dir = tempfile::tempdir()?;
            let store = Store::open(dir.path().join("context.db"), "real")?;
            let learner = store.create_learner("A", "zh", "one_step")?;
            let session = store.create_session(s(&learner, "learner_id"))?;
            let event = store.event(
                s(&learner, "learner_id"),
                s(&session, "session_id"),
                "learner_report",
                &json!({"text":text}),
            )?;
            let context = build_context(&store, &session, &learner, &text, &event, &json!({}))?;
            let request = json!({"request_id":"request_native_budget","learner_id":learner["learner_id"],"session_id":session["session_id"],"generation":1,"context":context});
            let prompt = crate::kimi::build_prompt(&policy, &schema, &request)?;
            crate::kimi::validate_command_budget(&[
                r"C:\Users\RBMS\.kimi-code\bin\kimi.exe".into(),
                "--agent-file".into(),
                root.join("prompts/tutor-agent.md")
                    .to_string_lossy()
                    .into_owned(),
                "--skills-dir".into(),
                root.join("runtime/kimi/run-rust-placeholder/empty-skills")
                    .to_string_lossy()
                    .into_owned(),
                "--output-format".into(),
                "stream-json".into(),
                "-p".into(),
                prompt,
            ])?;
        }
        Ok(())
    }
    #[test]
    fn continue_does_not_implicitly_select_a_finished_activity() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("continue.db"), "real")?;
        let learner = store.create_learner("A", "zh", "one_step")?;
        let lid = s(&learner, "learner_id");
        let first = store.create_session(lid)?;
        let sid = s(&first, "session_id");
        let eid = store.event(
            lid,
            sid,
            "operator_confirmed",
            &json!({"report":"绿灯正常，下次移动障碍物再测试"}),
        )?;
        let mut state = crate::store::empty_state();
        state["reported_result"] = json!("绿灯正常，下次移动障碍物再测试");
        state["modules"] = json!(["distance", "light"]);
        state["field_evidence"]["reported_result"] = json!({"source_event_ids":[eid]});
        store.checkpoint(sid, &state)?;
        let next = store.create_session(lid)?;
        let current = store.event(
            lid,
            s(&next, "session_id"),
            "learner_report",
            &json!({"text":"继续"}),
        )?;
        let context = build_context(&store, &next, &learner, "继续", &current, &json!({}))?;
        ensure!(context["slow_memory"].as_array().unwrap().is_empty());
        ensure!(
            !store.recall(lid, "绿灯正常", 5)?.is_empty(),
            "history remains available without implicit recall"
        );
        ensure!(
            context["task"]["modules"].is_null() && context["current_hardware_status"] == "unknown"
        );
        Ok(())
    }
}

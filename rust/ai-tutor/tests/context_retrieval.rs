//! Query/goal separation through real temporary Store provenance. No model,
//! device or challenge-label changes; fake embeddings are not needed here.
use ai_tutor::{
    context::{build_context, memory_recall_query},
    store::{Store, s},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

fn source_type(store: &Store) -> &'static str {
    if store.namespace == "simulator" {
        "simulated"
    } else {
        "learner_report"
    }
}

fn remember(
    store: &Store,
    learner: &Value,
    session: &Value,
    text: &str,
) -> Result<(String, String)> {
    let lid = s(learner, "learner_id");
    let event = store.record_education_event(
        lid,
        s(session, "session_id"),
        source_type(store),
        "observation",
        &json!({"text":text,"synthetic_fixture":true}),
    )?;
    let mut candidate = json!({"kind":"reported_progress","scope":"current_activity",
        "content":text,"source_event_ids":[event["event_id"]],"source_event_versions":{}});
    candidate["source_event_versions"][s(&event, "event_id")] = event["event_version"].clone();
    let mid = store
        .add_memory(
            lid,
            &candidate,
            store.learner(lid)?["epoch"].as_i64().unwrap(),
        )?
        .expect("synthetic observation must be supported");
    Ok((s(&event, "event_id").into(), mid))
}

fn goal(store: &Store, learner: &Value, session: &Value, text: &str) -> Result<()> {
    let sid = s(session, "session_id");
    let event = store.record_education_event(
        s(learner, "learner_id"),
        sid,
        source_type(store),
        "learner_statement",
        &json!({"text":text}),
    )?;
    let mut state = store.session(sid)?["state"].clone();
    state["goal"] = json!(text);
    state["field_evidence"]["goal"] = json!({"source_event_ids":[event["event_id"]]});
    store.checkpoint(sid, &state)
}

fn context(store: &Store, learner: &Value, session: &Value, text: &str) -> Result<Value> {
    let sid = s(session, "session_id");
    let event = store.record_education_event(
        s(learner, "learner_id"),
        sid,
        source_type(store),
        "learner_statement",
        &json!({"text":text}),
    )?;
    build_context(
        store,
        &store.session(sid)?,
        learner,
        text,
        s(&event, "event_id"),
        &json!({}),
    )
}

fn memory_ids(context: &Value) -> Vec<String> {
    context["slow_memory"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| s(row, "memory_id").to_owned())
        .collect()
}

#[test]
fn concrete_questions_are_verbatim_even_when_they_contain_resume_words() {
    for text in [
        "为什么这次结果不同？",
        "  Keep the original wording and spaces.  ",
        "继续解释为什么结果不同",
        "我不想继续这个任务",
        "Do not continue the previous activity.",
        "What does continue mean?",
        "discontinue",
        "What changed last time?",
    ] {
        for goal in ["", "上一个任务的长期目标", "A previous teaching goal"] {
            assert_eq!(memory_recall_query(text, goal), Some(text));
        }
    }
}

#[test]
fn only_complete_continuation_commands_borrow_goal_or_existing_activity_fallback() {
    for text in [
        "继续",
        "上次。",
        "接着！",
        " resume? ",
        "CONTINUE!",
        "Last   time...",
    ] {
        assert_eq!(memory_recall_query(text, "原目标"), Some("原目标"));
        assert_eq!(memory_recall_query(text, " \t "), Some("小车活动"));
    }
    for text in ["", " \t\n ", "　"] {
        assert_eq!(memory_recall_query(text, "不能隐式借此目标搜旧记忆"), None);
    }
}

#[test]
fn current_evidence_survives_goal_crowding_and_goal_stays_in_teaching_context() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("crowding.db"), "real")?;
    let learner = store.create_learner("synthetic", "zh", "guided")?;
    let history = store.create_session(s(&learner, "learner_id"))?;
    let (_, wanted) = remember(&store, &learner, &history, "水晶")?;
    let old_goal = "铅笔书签便笺橡皮";
    for suffix in ["甲", "乙", "丙", "丁", "戊", "己"] {
        remember(&store, &learner, &history, &format!("{old_goal}{suffix}"))?;
    }
    // Prove that the previous concatenation loses the only current-query match.
    let old_rows = store.recall(s(&learner, "learner_id"), &format!("水晶 {old_goal} "), 5)?;
    ensure!(old_rows.len() == 5 && old_rows.iter().all(|row| row["memory_id"] != wanted));
    // Query selection is tested within one real activity; cross-activity
    // selection now requires a separate explicit resume flow.
    let active = history.clone();
    for task_goal in [old_goal, "另一项已经报告的教学目标"] {
        goal(&store, &learner, &active, task_goal)?;
        let ctx = context(&store, &learner, &active, "水晶")?;
        ensure!(memory_ids(&ctx) == vec![wanted.clone()]);
        ensure!(ctx["task"]["goal"] == task_goal && ctx["user_input"] == "水晶");
        ensure!(
            ctx["task"]["field_evidence"]["goal"]["source_event_ids"]
                .as_array()
                .unwrap()
                .len()
                == 1
        );
        // The opt-in candidate API's no-embedding path receives the same query;
        // this does not enable or claim quality for semantic/RRF retrieval.
        let rows = store.recall_candidates_for_session(
            s(&active, "session_id"),
            memory_recall_query("水晶", task_goal).unwrap(),
            None,
            5,
        )?;
        ensure!(rows.len() == 1 && rows[0].memory["memory_id"] == wanted);
    }
    Ok(())
}

#[test]
fn continuation_uses_goal_but_empty_input_cannot_recall_it() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("continue.db"), "real")?;
    let learner = store.create_learner("synthetic", "en", "guided")?;
    let history = store.create_session(s(&learner, "learner_id"))?;
    let (_, wanted) = remember(&store, &learner, &history, "静电胶带")?;
    // Query selection is tested within one real activity; cross-activity
    // selection now requires a separate explicit resume flow.
    let active = history.clone();
    goal(&store, &learner, &active, "静电胶带")?;
    let ctx = context(&store, &learner, &active, "CONTINUE!")?;
    ensure!(memory_ids(&ctx) == vec![wanted]);
    ensure!(ctx["user_input"] == "CONTINUE!" && ctx["task"]["goal"] == "静电胶带");
    for text in ["", " \t\n "] {
        let error = build_context(
            &store,
            &store.session(s(&active, "session_id"))?,
            &learner,
            text,
            "no-event",
            &json!({}),
        )
        .unwrap_err();
        ensure!(error.to_string() == "请输入内容");
    }
    Ok(())
}

#[test]
fn separated_query_keeps_nonempty_owners_namespaces_and_live_source_versions_isolated() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let real = Store::open(dir.path().join("real.db"), "real")?;
    let sim = Store::open(dir.path().join("sim.db"), "simulator")?;
    let a = real.create_learner("A", "zh", "guided")?;
    let b = real.create_learner("B", "zh", "guided")?;
    let c = sim.create_learner("C", "zh", "guided")?;
    let sa = real.create_session(s(&a, "learner_id"))?;
    let sb = real.create_session(s(&b, "learner_id"))?;
    let sc = sim.create_session(s(&c, "learner_id"))?;
    let text = "镜面反射";
    let (ae, am) = remember(&real, &a, &sa, text)?;
    let (_, bm) = remember(&real, &b, &sb, text)?;
    let (_, cm) = remember(&sim, &c, &sc, text)?;
    for (store, learner, session, mid) in [
        (&real, &a, &sa, &am),
        (&real, &b, &sb, &bm),
        (&sim, &c, &sc, &cm),
    ] {
        let ctx = context(store, learner, session, text)?;
        ensure!(memory_ids(&ctx) == vec![mid.clone()]);
        ensure!(ctx["namespace"] == store.namespace);
        ensure!(
            ctx["slow_memory"][0]["source_event_versions"]
                .as_object()
                .unwrap()
                .values()
                .all(|version| version.as_i64() == Some(1))
        );
    }
    real.revise_education_event(
        s(&a, "learner_id"),
        s(&sa, "session_id"),
        &ae,
        "learner_report",
        &json!({"text":"更正：旧观察尚未确认"}),
    )?;
    ensure!(memory_ids(&context(&real, &a, &sa, text)?).is_empty());
    ensure!(memory_ids(&context(&real, &b, &sb, text)?) == vec![bm]);
    ensure!(memory_ids(&context(&sim, &c, &sc, text)?) == vec![cm]);
    ensure!(build_context(&real, &sb, &a, text, &ae, &json!({})).is_err());
    Ok(())
}

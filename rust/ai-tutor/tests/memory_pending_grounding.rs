//! Pending memory write grounding and long accepted-plan reads, SQLite only.
use ai_tutor::{
    context::build_context,
    store::{MemoryJobLimits, Store, s},
    tutor::{apply_reply, validate_reply},
};
use anyhow::Result;
use serde_json::{Value, json};

const STOP: &str = "先让小车停下来。";
// Frozen observed state counterexample; no original database is opened here.
const EXPANDED: &str = "先在不靠近运动部件的情况下，使用设备设计的停止按钮或电源开关停车；若做不到，请现场熟悉设备的人协助。";

struct Fixture {
    store: Store,
    lid: String,
    sid: String,
    _dir: tempfile::TempDir,
}
impl Fixture {
    fn new(namespace: &str) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("pending.sqlite3"), namespace)?;
        let lid = s(
            &store.create_learner("SYNTHETIC_PENDING_MEMORY", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        Ok(Self {
            store,
            lid,
            sid,
            _dir: dir,
        })
    }
    fn event(&self, kind: &str, content: Value) -> Result<Value> {
        let source = if self.store.namespace == "simulator" {
            "simulated"
        } else {
            "learner_report"
        };
        if kind == "legacy_report" {
            let id = self.store.event(&self.lid, &self.sid, source, &content)?;
            return Ok(self.store.source_events(&self.lid, &[id])?.remove(0));
        }
        self.store
            .record_education_event(&self.lid, &self.sid, source, kind, &content)
    }
    fn candidate(&self, content: &str, ids: &[String]) -> Value {
        json!({"kind":"pending_experiment","content":content,"scope":"current_activity","source_event_ids":ids})
    }
    fn add(&self, content: &str, ids: &[String]) -> Result<Option<String>> {
        self.store
            .add_memory(&self.lid, &self.candidate(content, ids), 0)
    }
    fn raw_state(&self) -> Result<String> {
        Ok(self.store.db.query_row(
            "SELECT state FROM sessions WHERE session_id=?",
            [&self.sid],
            |row| row.get(0),
        )?)
    }
}
fn reply(tasks: Value, memories: Value, ids: &[String]) -> Value {
    json!({"schema_version":"1.0","request_id":"pending-fixture","reply_text":EXPANDED,
        "teaching_move":"give_hint","task_update_candidates":tasks,"memory_candidates":memories,
        "used_evidence_ids":ids,"needs_clarification":false})
}

#[test]
fn expanded_teacher_method_cannot_enter_persistent_pending_memory_but_current_intent_survives()
-> Result<()> {
    for namespace in ["real", "simulator"] {
        let f = Fixture::new(namespace)?;
        let event = f.event("learner_statement", json!({"text":STOP}))?;
        let ids = [s(&event, "event_id").to_owned()];
        let value = reply(
            json!([
                {"field":"pending_step","value":STOP,"source_event_ids":ids},
                {"field":"goal","value":"让小车先停止移动","source_event_ids":ids}
            ]),
            json!([f.candidate(EXPANDED, &ids)]),
            &ids,
        );
        let request = json!({"request_id":"pending-fixture","learner_id":f.lid,"session_id":f.sid,
            "context":{"evidence":[event]}});
        validate_reply(&value, &request)?;
        let result = apply_reply(&f.store, &f.sid, &value, 0)?;
        assert_eq!(result["task_updates"], 2);
        assert_eq!(result["rejected_candidates"], 1);
        assert_eq!(result["applied_memory_ids"], json!([]));
        let state = f.store.session(&f.sid)?["state"].clone();
        assert_eq!(state["pending_step"], STOP);
        assert_eq!(state["goal"], "让小车先停止移动");
        assert!(f.store.memories(&f.lid)?.is_empty());
        assert_eq!(
            f.store.source_events(&f.lid, &ids)?[0]["content"]["text"],
            STOP
        );
        assert_eq!(value["reply_text"], EXPANDED);
        let mut forged = f.candidate(EXPANDED, &ids);
        forged["validated_finish"] = json!(true);
        assert!(f.store.add_memory(&f.lid, &forged, 0)?.is_none());
        let wrapped =
            format!("小车活动来源明确陈述的待做步骤原文（1/1，仅报告，未硬件验证）：{EXPANDED}");
        assert!(f.add(&wrapped, &ids)?.is_none());
    }
    Ok(())
}

#[test]
fn public_pending_write_requires_authorized_whole_text_without_changing_summary_promotion()
-> Result<()> {
    for (kind, content, value, expected) in [
        (
            "plan",
            json!({"text":"Measure 10 cm.","accepted":true}),
            "Measure 10 cm.",
            true,
        ),
        (
            "plan",
            json!({"text":"Next time we will test the light."}),
            "Next time we will test the light.",
            false,
        ),
        (
            "plan",
            json!({"text":"Measure 10 cm.","accepted":false}),
            "Measure 10 cm.",
            false,
        ),
        (
            "plan",
            json!({"text":"Measure 10 cm.","accepted":null}),
            "Measure 10 cm.",
            false,
        ),
        (
            "plan",
            json!({"text":"Measure 10 cm.","accepted":true}),
            "Measure 20 cm.",
            false,
        ),
        (
            "plan",
            json!({"text":"Measure 10 cm.","accepted":true}),
            "Check a 10 cm gap.",
            false,
        ),
        (
            "learner_statement",
            json!({"text":"Next time we will test the wall distance."}),
            "Next time we will test the wall distance.",
            true,
        ),
        (
            "legacy_report",
            json!({"text":"下次移动墙测试停车距离"}),
            "下次移动墙测试停车距离",
            true,
        ),
        (
            "learner_statement",
            json!({"text":"Maybe next time we will test it."}),
            "Maybe next time we will test it.",
            false,
        ),
        (
            "learner_statement",
            json!({"text":"Should we test it next time?"}),
            "Should we test it next time?",
            false,
        ),
        (
            "observation",
            json!({"text":"The rover stopped."}),
            "Next time we will change the threshold.",
            false,
        ),
        (
            "learner_statement",
            json!({"text":"I accept this plan.","pending_experiment":"Compare one fold and two folds."}),
            "Compare one fold and two folds.",
            true,
        ),
    ] {
        let f = Fixture::new("real")?;
        let event = f.event(kind, content)?;
        let found = f.add(value, &[s(&event, "event_id").to_owned()])?;
        assert_eq!(found.is_some(), expected, "{kind}: {value}");
    }
    Ok(())
}

#[test]
fn every_pending_source_must_support_the_same_method_and_old_activity_sources_remain_valid()
-> Result<()> {
    let f = Fixture::new("real")?;
    let one = f.event("plan", json!({"text":"Measure 10 cm.","accepted":true}))?;
    let other = f.event("plan", json!({"text":"Use red paper.","accepted":true}))?;
    assert!(
        f.add(
            "Measure 10 cm.",
            &[s(&one, "event_id").into(), s(&other, "event_id").into()]
        )?
        .is_none()
    );
    let same = f.event("plan", json!({"text":" Measure 10 cm. ","accepted":true}))?;
    assert!(
        f.add(
            "Measure 10 cm.",
            &[s(&one, "event_id").into(), s(&same, "event_id").into()]
        )?
        .is_some()
    );
    let next = f.store.create_session(&f.lid)?;
    // Persistent memory belongs to its original activity; it must not invent a
    // latest-session requirement just because fast-state validation uses one.
    let old_memory = f
        .add("Measure 10 cm.", &[s(&one, "event_id").into()])?
        .unwrap();
    assert_eq!(
        f.store.memory_evidence(&f.lid, &old_memory)?["sources"][0]["session_id"],
        f.sid
    );
    assert!(
        f.store
            .recall_for_session(s(&next, "session_id"), "rover", 5)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn accepted_800_801_and_1000_character_plans_read_faithfully_and_finish_without_losing_chunks()
-> Result<()> {
    for size in [800, 801, 1000] {
        let f = Fixture::new("real")?;
        let body = "下次保持同一张纸和10厘米跨度，只改变折痕数量并比较硬币数量。"
            .repeat(40)
            .chars()
            .take(size - 1)
            .collect::<String>()
            + "。";
        assert_eq!(body.chars().count(), size);
        f.store.save_comparison(&f.lid,&f.sid,&json!({
            "submission_id":"long-accepted-plan","source_type":"learner_report","question":"Compare paper bridges.",
            "changed_variable":"folds","conditions":[{"name":"same paper","status":"reported_same"}],
            "trials":[{"label":"A","setting":"one fold","status":"not_tested","result":null,"trial_count":null},
                      {"label":"B","setting":"two folds","status":"not_tested","result":null,"trial_count":null}],
            "pending_plan":{"text":body,"accepted":true}
        }))?;
        let accepted = f
            .store
            .recent_evidence(&f.sid)?
            .into_iter()
            .find(|v| v["content"]["accepted"] == true)
            .unwrap();
        let query = f.event(
            "learner_statement",
            json!({"text":"What is the accepted plan?"}),
        )?;
        let before = f.raw_state()?;
        let session = f.store.session(&f.sid)?;
        assert_eq!(session["pending_step_needs_confirmation"], false);
        let context = build_context(
            &f.store,
            &session,
            &f.store.learner(&f.lid)?,
            "What is the accepted plan?",
            s(&query, "event_id"),
            &json!({}),
        )?;
        assert_eq!(context["task"]["pending_step"], body);
        assert_eq!(f.raw_state()?, before);
        if size > 800 {
            let ids = [s(&accepted, "event_id").to_owned()];
            let candidate = reply(
                json!([{"field":"pending_step","value":body,"source_event_ids":ids}]),
                json!([]),
                &ids,
            );
            let request = json!({"request_id":"pending-fixture","context":context});
            assert!(validate_reply(&candidate, &request).is_err());
            let applied = apply_reply(&f.store, &f.sid, &candidate, 0)?;
            assert_eq!(applied["task_updates"], 0);
            assert_eq!(applied["rejected_candidates"], 1);
            assert_eq!(f.raw_state()?, before);
        }
        let finished = f.store.consolidate_and_finish(&f.sid)?;
        let memories = f.store.memories(&f.lid)?;
        assert_eq!(memories.len(), 2);
        assert_eq!(
            finished["consolidated_memory_ids"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let mut restored = String::new();
        for index in 1..=2 {
            let marker = format!("（{index}/2，");
            let memory = memories
                .iter()
                .find(|m| s(m, "content").contains(&marker))
                .unwrap();
            assert_eq!(memory["kind"], "pending_experiment");
            assert_eq!(memory["source_event_ids"], json!([accepted["event_id"]]));
            assert!(s(memory, "content").chars().count() <= 800);
            restored.push_str(s(memory, "content").split_once('：').unwrap().1);
        }
        assert_eq!(restored, body);
        assert_eq!(
            f.store
                .source_events(&f.lid, &[s(&accepted, "event_id").into()])?[0]["content"]["text"],
            body
        );
        f.store.consolidate_and_finish(&f.sid)?;
        assert_eq!(f.store.memories(&f.lid)?.len(), 2);
    }
    Ok(())
}

#[test]
fn explicitly_promoted_educational_plan_summary_keeps_its_legitimate_paraphrase() -> Result<()> {
    let f = Fixture::new("real")?;
    let original = "Next time we will test the wall distance.";
    let summary = "Test the wall distance next time.";
    let event = f.event("plan", json!({"text":original,"accepted":true}))?;
    let eid = s(&event, "event_id");
    assert!(f.add(summary, &[eid.into()])?.is_none());
    let limits = MemoryJobLimits::default();
    let now = chrono::Utc::now().timestamp_millis();
    f.store.enqueue_memory_job(
        &f.lid,
        &f.sid,
        &[eid.into()],
        "pending-grounding-fixture",
        &limits,
        now,
    )?;
    let claim = f
        .store
        .claim_memory_job("synthetic-worker", &limits, now)?
        .unwrap();
    let mut payload = json!({"kind":"accepted_pending_experiment","claim_type":"accepted_plan","content":summary,"scope":"current_activity",
        "source_event_ids":[eid],"source_event_versions":{},"source_quotes":[{"event_id":eid,"quote":original}],
        "support_level":"unknown","importance":{"goal_relevance":3,"continuity":3,"novel_change":1,"explicit_request":3}});
    payload["source_event_versions"][eid] = event["event_version"].clone();
    let candidates = f
        .store
        .stage_memory_candidates(&claim["receipt"], &[payload], now + 1)?;
    assert_eq!(candidates[0]["status"], "shadow");
    assert!(f.store.memories(&f.lid)?.is_empty());
    let applied = f.store.apply_memory_candidate(
        &f.lid,
        s(&candidates[0], "candidate_id"),
        &["accepted_pending_experiment"],
        now + 2,
    )?;
    assert_eq!(applied["memory"]["kind"], "pending_experiment");
    assert!(s(&applied["memory"], "content").ends_with(summary));
    assert_eq!(
        f.store
            .recall_for_session(&f.sid, "wall distance", 5)?
            .len(),
        1
    );
    assert_eq!(f.store.session(&f.sid)?["state"]["pending_step"], "");
    Ok(())
}

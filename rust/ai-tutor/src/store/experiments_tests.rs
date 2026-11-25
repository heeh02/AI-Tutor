use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    lid: String,
    sid: String,
}

impl Fixture {
    fn new(namespace: &str) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("comparison.sqlite3"), namespace)?;
        let lid = s(
            &store.create_learner("synthetic comparison", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        Ok(Self {
            _dir: dir,
            store,
            lid,
            sid,
        })
    }

    fn payload(&self, submission: &str) -> Value {
        json!({"submission_id":submission,"source_type":"learner_report","question":"停车距离改变后发生了什么？",
            "changed_variable":"停车距离","conditions":[{"name":"同一面墙","status":"reported_same"},{"name":"电量","status":"not_confirmed"}],
            "trials":[{"label":"A","setting":"10厘米","status":"reported","result":"报告原文：小车停在墙前。","trial_count":2},
                      {"label":"B","setting":"20厘米尚待尝试","status":"not_tested","result":null,"trial_count":null}],
            "pending_plan":{"text":"下一次只改变停车距离，再观察。","accepted":false}})
    }

    fn count(&self, table: &str) -> Result<i64> {
        Ok(self
            .store
            .db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?)
    }
}

#[test]
fn observed_untested_and_unaccepted_plan_have_separate_live_sources() -> Result<()> {
    let f = Fixture::new("real")?;
    let before = f.store.session(&f.sid)?["state"].clone();
    let saved = f
        .store
        .save_comparison(&f.lid, &f.sid, &f.payload("separate"))?;
    ensure!(saved["item"]["hardware_verified"] == false);
    let evidence = f.store.recent_evidence(&f.sid)?;
    ensure!(evidence.len() == 3 && f.count("memories")? == 0);
    let observed = evidence
        .iter()
        .find(|event| event["event_kind"] == "observation")
        .unwrap();
    ensure!(observed["content"]["trials"].as_array().unwrap().len() == 1);
    ensure!(observed["content"]["trials"][0]["label"] == "A");
    ensure!(!s(&observed["content"], "text").contains("20厘米尚待尝试"));
    for event in evidence
        .iter()
        .filter(|event| event["event_kind"] == "plan")
    {
        ensure!(event["content"]["accepted"] == false);
        ensure!(!s(&event["content"], "text").contains("小车停在墙前"));
    }
    let state = &f.store.session(&f.sid)?["state"];
    ensure!(
        state["pending_step"] == before["pending_step"] && state["modules"] == before["modules"]
    );
    ensure!(state["stage"] == before["stage"] && state["requires_inventory_confirmation"] == true);
    ensure!(state["reported_result"] == observed["content"]["text"]);
    ensure!(
        state["field_evidence"]["reported_result"]["source_event_ids"]
            == json!([observed["event_id"]])
    );
    let listed = f.store.comparisons(&f.lid, &f.sid)?;
    ensure!(listed["items"] == json!([saved["item"]]));
    Ok(())
}

#[test]
fn accepted_pending_step_uses_only_its_own_plan_and_never_marks_test_complete() -> Result<()> {
    let f = Fixture::new("real")?;
    let mut data = f.payload("accepted");
    data["trials"][0]["status"] = json!("not_tested");
    data["trials"][0]["result"] = Value::Null;
    data["trials"][0]["trial_count"] = Value::Null;
    data["pending_plan"]["accepted"] = json!(true);
    f.store.save_comparison(&f.lid, &f.sid, &data)?;
    let sources = f.store.recent_evidence(&f.sid)?;
    ensure!(sources.len() == 2 && sources.iter().all(|event| event["event_kind"] == "plan"));
    let accepted = sources
        .iter()
        .find(|event| event["content"]["accepted"] == true)
        .unwrap();
    let untested = sources
        .iter()
        .find(|event| event["content"]["accepted"] == false)
        .unwrap();
    ensure!(untested["content"]["trials"].as_array().unwrap().len() == 2);
    ensure!(accepted["content"]["trials"].is_null());
    let state = &f.store.session(&f.sid)?["state"];
    ensure!(state["reported_result"] == "" && state["attempts"] == json!([]));
    ensure!(state["pending_step"] == data["pending_plan"]["text"]);
    ensure!(
        state["field_evidence"]["pending_step"]["source_event_ids"]
            == json!([accepted["event_id"]])
    );
    ensure!(state["stage"] == "goal_setting" && f.count("memories")? == 0);
    Ok(())
}

#[test]
fn same_submission_replays_without_new_evidence_or_state_and_conflicts_reject() -> Result<()> {
    let f = Fixture::new("real")?;
    let mut data = f.payload("idempotent");
    data["pending_plan"]
        .as_object_mut()
        .unwrap()
        .remove("accepted");
    ensure!(f.store.comparison_replay(&f.lid, &f.sid, &data)?.is_none());
    let saved = f.store.save_comparison(&f.lid, &f.sid, &data)?;
    let revision = f.store.state_revision(&f.sid)?;
    data["pending_plan"]["accepted"] = json!(false);
    ensure!(f.store.comparison_replay(&f.lid, &f.sid, &data)? == Some(saved.clone()));
    ensure!(f.store.save_comparison(&f.lid, &f.sid, &data)? == saved);
    ensure!(f.count("events")? == 3 && f.count("experiment_comparisons")? == 1);
    ensure!(f.store.state_revision(&f.sid)? == revision);
    data["trials"][0]["result"] = json!("不同原文");
    ensure!(f.store.comparison_replay(&f.lid, &f.sid, &data).is_err());
    ensure!(f.store.save_comparison(&f.lid, &f.sid, &data).is_err());
    ensure!(f.count("events")? == 3);
    Ok(())
}

#[test]
fn preflight_rejects_invalid_first_payload_without_writes() -> Result<()> {
    let f = Fixture::new("real")?;
    let data = f.payload("invalid");
    let mut bad = Vec::new();
    let mut value = data.clone();
    value["hardware_verified"] = json!(true);
    bad.push(value);
    let mut value = data.clone();
    value["trials"][0]["result"] = json!(" ");
    bad.push(value);
    let mut value = data.clone();
    value["trials"][1]["result"] = json!("尚未测试但已有结果");
    bad.push(value);
    let mut value = data.clone();
    value["trials"][1]["trial_count"] = json!(1);
    bad.push(value);
    let mut value = data.clone();
    value["trials"][1]["label"] = json!("A");
    bad.push(value);
    let mut value = data.clone();
    value["trials"][0]["trial_count"] = json!(1.5);
    bad.push(value);
    let mut value = data.clone();
    value["trials"][0]["trial_count"] = json!(10001);
    bad.push(value);
    let mut value = data.clone();
    value["conditions"] = json!([]);
    bad.push(value);
    let mut value = data.clone();
    value["question"] = json!("字".repeat(201));
    bad.push(value);
    let mut value = data.clone();
    value["pending_plan"]["accepted"] = json!("true");
    bad.push(value);
    let mut value = data.clone();
    value["source_type"] = json!("hardware_verified");
    bad.push(value);
    for value in bad {
        ensure!(f.store.comparison_replay(&f.lid, &f.sid, &value).is_err());
        ensure!(f.store.save_comparison(&f.lid, &f.sid, &value).is_err());
    }
    ensure!(f.count("events")? == 0 && f.count("experiment_comparisons")? == 0);
    Ok(())
}

#[test]
fn owner_finished_session_and_namespace_are_checked_on_reads_and_replays() -> Result<()> {
    let f = Fixture::new("real")?;
    let data = f.payload("scope");
    let other = s(
        &f.store.create_learner("other", "en", "guided")?,
        "learner_id",
    )
    .to_owned();
    ensure!(f.store.comparisons(&other, &f.sid).is_err());
    ensure!(f.store.comparison_replay(&other, &f.sid, &data).is_err());
    let mut simulated = data.clone();
    simulated["source_type"] = json!("simulated");
    ensure!(f.store.save_comparison(&f.lid, &f.sid, &simulated).is_err());
    let saved = f.store.save_comparison(&f.lid, &f.sid, &data)?;
    f.store.finish_session(&f.sid)?;
    ensure!(f.store.comparisons(&f.lid, &f.sid)?["items"] == json!([saved["item"]]));
    ensure!(f.store.comparison_replay(&f.lid, &f.sid, &data).is_err());
    ensure!(f.store.save_comparison(&f.lid, &f.sid, &data).is_err());
    let sim = Fixture::new("simulator")?;
    let mut input = sim.payload("sim");
    input["source_type"] = json!("operator_confirmed");
    let saved = sim.store.save_comparison(&sim.lid, &sim.sid, &input)?;
    ensure!(
        saved["item"]["source_type"] == "simulated" && saved["item"]["hardware_verified"] == false
    );
    ensure!(
        sim.store
            .recent_evidence(&sim.sid)?
            .iter()
            .all(|event| event["source_type"] == "simulated")
    );
    Ok(())
}

#[test]
fn source_invalidation_and_forgetting_never_recreate_comparison_on_replay() -> Result<()> {
    let f = Fixture::new("real")?;
    let data = f.payload("invalid-source");
    f.store.save_comparison(&f.lid, &f.sid, &data)?;
    let event = f.store.recent_evidence(&f.sid)?[0]["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.store.db.execute(
        "UPDATE education_event_meta SET status='invalidated' WHERE event_id=?",
        [&event],
    )?;
    ensure!(f.store.comparisons(&f.lid, &f.sid)?["items"] == json!([]));
    f.store.db.execute(
        "UPDATE education_event_meta SET status='active' WHERE event_id=?",
        [&event],
    )?;
    ensure!(f.store.comparisons(&f.lid, &f.sid)?["items"] == json!([]));
    ensure!(f.store.comparison_replay(&f.lid, &f.sid, &data).is_err());
    ensure!(f.store.save_comparison(&f.lid, &f.sid, &data).is_err());
    let second = f.payload("forget-source");
    f.store.save_comparison(&f.lid, &f.sid, &second)?;
    f.store.forget(&f.lid, None)?;
    ensure!(f.count("events")? == 0);
    ensure!(f.store.save_comparison(&f.lid, &f.sid, &second).is_err());
    let receipts = f.store.rows("SELECT * FROM experiment_comparisons", [])?;
    ensure!(receipts.len() == 2 && receipts.iter().all(|row| row["active"] == 0));
    ensure!(!json!(receipts).to_string().contains("小车停在墙前"));
    let reopened = Store::open(f._dir.path().join("comparison.sqlite3"), "real")?;
    ensure!(reopened.comparisons(&f.lid, &f.sid)?["items"] == json!([]));
    ensure!(reopened.save_comparison(&f.lid, &f.sid, &second).is_err());
    Ok(())
}

#[test]
fn source_versions_and_deletion_remove_projection_without_losing_older_latest_ten() -> Result<()> {
    let f = Fixture::new("real")?;
    for i in 0..12 {
        let mut data = f.payload(&format!("row-{i}"));
        data["question"] = json!(format!("第{i}条比较"));
        f.store.save_comparison(&f.lid, &f.sid, &data)?;
    }
    let listed = f.store.comparisons(&f.lid, &f.sid)?;
    ensure!(
        listed["items"].as_array().unwrap().len() == 10
            && listed["items"][0]["submission_id"] == "row-11"
    );
    let receipt = f
        .store
        .one(
            "SELECT * FROM experiment_comparisons WHERE submission_id='row-11'",
            [],
        )?
        .unwrap();
    f.store.db.execute(
        "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
        [s(&receipt, "observation_id")],
    )?;
    let receipt = f
        .store
        .one(
            "SELECT * FROM experiment_comparisons WHERE submission_id='row-10'",
            [],
        )?
        .unwrap();
    f.store.db.execute(
        "DELETE FROM events WHERE event_id=?",
        [s(&receipt, "pending_id")],
    )?;
    let listed = f.store.comparisons(&f.lid, &f.sid)?;
    ensure!(
        listed["items"].as_array().unwrap().len() == 10
            && listed["items"][0]["submission_id"] == "row-9"
    );
    ensure!(listed["items"][9]["submission_id"] == "row-0");
    Ok(())
}

#[test]
fn comparison_history_pages_all_live_receipts_without_duplicates_or_newer_insertions() -> Result<()> {
    let f = Fixture::new("simulator")?;
    for i in 0..25 {
        f.store
            .save_comparison(&f.lid, &f.sid, &f.payload(&format!("page-{i}")))?;
    }
    let revised = f.store.one(
        "SELECT observation_id FROM experiment_comparisons WHERE submission_id='page-23'",
        [],
    )?.unwrap();
    f.store.db.execute(
        "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
        [s(&revised, "observation_id")],
    )?;
    let deleted = f.store.one(
        "SELECT pending_id FROM experiment_comparisons WHERE submission_id='page-12'",
        [],
    )?.unwrap();
    f.store.db.execute("DELETE FROM events WHERE event_id=?", [s(&deleted, "pending_id")])?;

    let mut cursor = None;
    let mut seen = Vec::new();
    let mut lengths = Vec::new();
    loop {
        let page = f.store.comparisons_page(&f.lid, &f.sid, cursor, 10)?;
        ensure!(page["learner_id"] == f.lid && page["session_id"] == f.sid);
        ensure!(page["namespace"] == "simulator");
        let items = page["items"].as_array().unwrap();
        lengths.push(items.len());
        ensure!(items.iter().all(|item| item.get("cursor").is_none()
            && item["source_type"] == "simulated" && item["hardware_verified"] == false));
        seen.extend(items.iter().map(|item| s(item, "submission_id").to_owned()));
        if cursor.is_none() {
            let legacy = f.store.comparisons(&f.lid, &f.sid)?;
            ensure!(legacy["items"] == page["items"] && legacy.as_object().unwrap().len() == 3);
            f.store.save_comparison(&f.lid, &f.sid, &f.payload("newer-after-first-page"))?;
        }
        let Some(next) = page["next_before"].as_i64() else { break };
        ensure!(next > 0 && cursor.is_none_or(|previous| next < previous));
        cursor = Some(next);
    }
    let expected: Vec<_> = (0..25).rev().filter(|i| ![23, 12].contains(i))
        .map(|i| format!("page-{i}")).collect();
    ensure!(seen == expected && lengths == vec![10, 10, 3]);
    f.store.finish_session(&f.sid)?;
    ensure!(f.store.comparisons_page(&f.lid, &f.sid, None, 20)?["items"]
        .as_array().unwrap().len() == 20);
    f.store.forget(&f.lid, None)?;
    let forgotten = f.store.comparisons_page(&f.lid, &f.sid, None, 10)?;
    ensure!(forgotten["items"] == json!([]) && forgotten["next_before"].is_null());
    Ok(())
}

#[test]
fn comparison_history_rejects_wrong_owner_and_invalid_page_bounds() -> Result<()> {
    let f = Fixture::new("real")?;
    let other = f.store.create_learner("other comparison reader", "en", "guided")?;
    for limit in [0, 21, usize::MAX] {
        ensure!(f.store.comparisons_page(&f.lid, &f.sid, None, limit).is_err());
    }
    for cursor in [0, -1] {
        ensure!(f.store.comparisons_page(&f.lid, &f.sid, Some(cursor), 10).is_err());
    }
    ensure!(f.store.comparisons_page(s(&other, "learner_id"), &f.sid, None, 10).is_err());
    ensure!(f.store.comparisons_page(&f.lid, "missing-session", None, 10).is_err());
    let empty = f.store.comparisons_page(&f.lid, &f.sid, Some(1), 1)?;
    ensure!(empty["namespace"] == "real" && empty["items"] == json!([]));
    ensure!(empty["next_before"].is_null());
    Ok(())
}

#[test]
fn forgetting_one_comparison_plan_preserves_eleven_unsummarized_records_and_other_activity() -> Result<()> {
    let f = Fixture::new("simulator")?;
    for index in 1..=12 {
        let mut input = f.payload(&format!("selective-{index}"));
        input["pending_plan"]["accepted"] = json!(true);
        f.store.save_comparison(&f.lid, &f.sid, &input)?;
    }
    f.store.finish_session(&f.sid)?;
    let memories = f.store.memories(&f.lid)?;
    ensure!(memories.len() == 2);
    let target = memories.iter().find(|m| m["kind"] == "pending_experiment").unwrap();
    let target_id = s(target, "memory_id");
    let target_sources = strings(&target["source_event_ids"]);
    ensure!(target_sources.len() == 1);
    let raw_plan = f.store.source_events(&f.lid, &target_sources)?[0]["content"]["text"].clone();
    let derived_id = f.store.add_memory(&f.lid, &json!({"kind":"pending_experiment",
        "scope":"current_activity","content":raw_plan,"source_event_ids":target_sources}), 0)?.unwrap();
    ensure!(derived_id != target_id);
    let observation_id = s(memories.iter().find(|m| m["kind"] == "reported_progress").unwrap(), "memory_id");
    let affected_comparison = f.store.comparisons(&f.lid, &f.sid)?["items"][0]["comparison_id"].clone();
    let sibling_sources = f.store.recent_evidence(&f.sid)?.into_iter()
        .filter(|event| event["content"]["comparison_id"] == affected_comparison)
        .filter(|event| !target_sources.contains(&s(event, "event_id").to_owned()))
        .map(|event| s(&event, "event_id").to_owned()).collect::<Vec<_>>();
    ensure!(sibling_sources.len() == 2);

    let other = f.store.create_session(&f.lid)?;
    let other_sid = s(&other, "session_id");
    let other_record = f.store.save_comparison(&f.lid, other_sid, &f.payload("other-activity"))?;
    let event = f.store.record_education_event(&f.lid, other_sid, "simulated", "learner_statement",
        &json!({"text":"Keep the independent activity goal."}))?;
    let eid = s(&event, "event_id").to_owned();
    let generation = f.store.session(other_sid)?["generation"].as_i64().unwrap();
    f.store.add_turn(other_sid, "selective-forget-run", generation, "user",
        "Keep the independent activity goal.", "shown", &[eid.clone()])?;
    let turn = f.store.add_turn(other_sid, "selective-forget-run", generation, "assistant",
        "The independent goal is still a plan.", "shown", &[eid.clone()])?;
    let mut state = f.store.session(other_sid)?["state"].clone();
    state["goal"] = json!("Keep the independent activity goal.");
    state["goal_source"] = json!("simulated");
    state["field_evidence"]["goal"] = json!({"source_event_ids":[eid],"hardware_verified":false});
    state["latest_input"] = json!("Keep the independent activity goal.");
    state["last_delivered_hint"] = json!("The independent goal is still a plan.");
    state["last_delivered_turn_id"] = json!(turn);
    f.store.checkpoint(other_sid, &state)?;
    let revision = f.store.state_revision(other_sid)?;
    let turns = f.store.session(other_sid)?["turns"].clone();
    let other_sources = f.store.recent_evidence(other_sid)?;
    let events_before = f.count("events")?;
    let review = f.store.prepare_memory_review(&f.lid, other_sid, &MemoryJobLimits::default(), 1000)?;
    let job_id = s(&review["job"], "job_id");
    ensure!(!job_id.is_empty());
    f.store.start_run("selective-forget-run", &f.lid, other_sid, 1)?;
    const MODEL: &str = "3804e3b06d6f86abe39d18a439e34e8fe451cdae3d0e9e4ebb33709cfc8d1dda";
    let snapshot = f.store.embedding_work(&f.lid, MODEL, 8)?.into_iter()
        .find(|row| row.memory_id == target_id).unwrap();
    let mut vector = vec![0.0; crate::memory_embedding::DIM];
    vector[0] = 1.0;
    ensure!(f.store.commit_memory_embedding(&snapshot, MODEL, &vector)?);

    let epoch = f.store.forget(&f.lid, Some(target_id))?;
    ensure!(epoch == 1 && f.count("events")? == events_before - 1);
    let first = f.store.comparisons_page(&f.lid, &f.sid, None, 10)?;
    let last = f.store.comparisons_page(&f.lid, &f.sid, first["next_before"].as_i64(), 10)?;
    ensure!(first["items"].as_array().unwrap().len() == 10
        && first["items"][0]["submission_id"] == "selective-11"
        && last["items"].as_array().unwrap().len() == 1
        && last["items"][0]["submission_id"] == "selective-1"
        && last["next_before"].is_null());
    ensure!(f.store.comparisons(&f.lid, other_sid)?["items"] == json!([other_record["item"]]));
    let after_state = f.store.session(other_sid)?["state"].clone();
    ensure!(after_state == state,
        "unrelated fast state changed: before={} after={}", state, after_state);
    ensure!(f.store.session(other_sid)?["turns"] == turns && f.store.recent_evidence(other_sid)? == other_sources);
    ensure!(f.store.source_events(&f.lid, &sibling_sources)?.len() == 2);
    ensure!(f.store.memory_evidence(&f.lid, observation_id)?["comparison_context"]["status"] == "unavailable");
    ensure!(f.store.memory_evidence(&f.lid, target_id).is_err());
    ensure!(f.store.one("SELECT memory_id FROM memories WHERE memory_id=?", [target_id])?.is_none());
    ensure!(f.store.one("SELECT memory_id FROM memories WHERE memory_id=?", [&derived_id])?.is_none());
    ensure!(f.store.rows("SELECT event_id FROM memory_source_links WHERE memory_id=?", [target_id])?.is_empty());
    ensure!(f.store.rows("SELECT memory_id FROM memory_embeddings WHERE learner_id=?", [&f.lid])?.is_empty());
    ensure!(!f.store.commit_memory_embedding(&snapshot, MODEL, &vector)?);
    ensure!(f.store.one("SELECT status,source_snapshot FROM memory_jobs WHERE job_id=?", [job_id])?
        .is_some_and(|job| job["status"] == "invalidated" && job["source_snapshot"] == "[]"));
    ensure!(f.store.one("SELECT status FROM provider_runs WHERE request_id='selective-forget-run'", [])?
        .is_some_and(|run| run["status"] == "cancelled"));
    ensure!(f.store.checkpoint_if_current(&f.lid, other_sid, generation, revision, 0, &state).is_err());
    ensure!(f.store.add_memory(&f.lid, &json!({"kind":"pending_experiment","scope":"current_activity",
        "content":"Next time repeat the forgotten plan.","source_event_ids":target_sources}), epoch)?.is_none());
    let reopened = Store::open(f._dir.path().join("comparison.sqlite3"), "simulator")?;
    ensure!(reopened.comparisons_page(&f.lid, &f.sid, None, 20)?["items"].as_array().unwrap().len() == 11);
    ensure!(reopened.comparisons(&f.lid, other_sid)?["items"] == json!([other_record["item"]]));
    ensure!(reopened.memory_evidence(&f.lid, target_id).is_err());
    Ok(())
}

#[test]
fn receipt_failure_rolls_back_all_new_sources_and_fast_state() -> Result<()> {
    let f = Fixture::new("real")?;
    let before = f.store.session(&f.sid)?["state"].clone();
    f.store.db.execute_batch("CREATE TRIGGER reject_comparison BEFORE INSERT ON experiment_comparisons BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;")?;
    ensure!(
        f.store
            .save_comparison(&f.lid, &f.sid, &f.payload("rollback"))
            .is_err()
    );
    ensure!(
        f.count("events")? == 0
            && f.count("education_event_meta")? == 0
            && f.count("experiment_comparisons")? == 0
    );
    ensure!(f.store.session(&f.sid)?["state"] == before);
    Ok(())
}

#[test]
fn untested_comparison_context_cannot_promote_model_candidates_to_facts_or_acceptance() -> Result<()>
{
    let f = Fixture::new("real")?;
    let mut data = f.payload("not-done");
    data["trials"][0]["status"] = json!("not_tested");
    data["trials"][0]["result"] = Value::Null;
    data["trials"][0]["trial_count"] = Value::Null;
    f.store.save_comparison(&f.lid, &f.sid, &data)?;
    let plans = f.store.recent_evidence(&f.sid)?;
    let ids: Vec<_> = plans
        .iter()
        .map(|source| s(source, "event_id").to_owned())
        .collect();
    let input = f.store.record_education_event(
        &f.lid,
        &f.sid,
        "learner_report",
        "learner_statement",
        &json!({"text":"帮我看看这份比较计划。"}),
    )?;
    let context = crate::context::build_context(
        &f.store,
        &f.store.session(&f.sid)?,
        &f.store.learner(&f.lid)?,
        "帮我看看这份比较计划。",
        s(&input, "event_id"),
        &Value::Null,
    )?;
    for id in &ids {
        let source = context["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| s(source, "event_id") == id)
            .unwrap();
        ensure!(source["event_kind"] == "plan" && source["content"]["accepted"] == false);
    }
    let reply = json!({"task_update_candidates":[
        {"field":"reported_result","value":"A/B都已完成","source_event_ids":ids},
        {"field":"stage","value":"completed","source_event_ids":ids},
        {"field":"pending_step","value":data["pending_plan"]["text"],"source_event_ids":ids}],
        "memory_candidates":[
        {"kind":"reported_progress","content":"A/B都已完成","scope":"current_activity","source_event_ids":ids},
        {"kind":"pending_experiment","content":data["pending_plan"]["text"],"scope":"current_activity","source_event_ids":ids}]});
    let applied = crate::tutor::apply_reply(&f.store, &f.sid, &reply, 0)?;
    ensure!(applied["task_updates"] == 0 && applied["rejected_candidates"] == 5);
    let state = &f.store.session(&f.sid)?["state"];
    ensure!(
        state["reported_result"] == ""
            && state["pending_step"] == ""
            && state["stage"] == "goal_setting"
    );
    f.store.finish_session(&f.sid)?;
    ensure!(f.count("memories")? == 0);
    Ok(())
}

#[test]
fn mixed_observation_and_plan_citations_reject_but_precise_valid_sources_still_apply() -> Result<()>
{
    let f = Fixture::new("real")?;
    f.store
        .save_comparison(&f.lid, &f.sid, &f.payload("mixed-sources"))?;
    let sources = f.store.recent_evidence(&f.sid)?;
    let observed = sources
        .iter()
        .find(|source| source["event_kind"] == "observation")
        .unwrap();
    let plan = sources
        .iter()
        .find(|source| source["event_kind"] == "plan")
        .unwrap();
    let ids = json!([observed["event_id"], plan["event_id"]]);
    let original = f.store.session(&f.sid)?["state"].clone();
    let applied = crate::tutor::apply_reply(
        &f.store,
        &f.sid,
        &json!({"task_update_candidates":[
        {"field":"reported_result","value":"B已测试","source_event_ids":ids},
        {"field":"stage","value":"completed","source_event_ids":ids},
        {"field":"pending_step","value":"已接受B设置","source_event_ids":ids}],
        "memory_candidates":[{"kind":"pending_experiment","content":"已接受B设置","scope":"current_activity","source_event_ids":ids}]}),
        0,
    )?;
    ensure!(applied["task_updates"] == 0 && applied["rejected_candidates"] == 4);
    ensure!(f.store.session(&f.sid)?["state"] == original && f.count("memories")? == 0);
    let applied = crate::tutor::apply_reply(
        &f.store,
        &f.sid,
        &json!({"task_update_candidates":[{"field":"reported_result","value":"A据报告停在墙前","source_event_ids":[observed["event_id"]]}],"memory_candidates":[]}),
        0,
    )?;
    ensure!(applied["task_updates"] == 1);
    let mut data = f.payload("explicitly-accepted");
    data["pending_plan"]["accepted"] = json!(true);
    f.store.save_comparison(&f.lid, &f.sid, &data)?;
    let accepted = f
        .store
        .recent_evidence(&f.sid)?
        .into_iter()
        .find(|source| source["content"]["accepted"] == true)
        .unwrap();
    let applied = crate::tutor::apply_reply(
        &f.store,
        &f.sid,
        &json!({"task_update_candidates":[{"field":"pending_step","value":data["pending_plan"]["text"],"source_event_ids":[accepted["event_id"]]}],
        "memory_candidates":[{"kind":"pending_experiment","content":data["pending_plan"]["text"],"scope":"current_activity","source_event_ids":[accepted["event_id"]]}]}),
        0,
    )?;
    ensure!(
        applied["task_updates"] == 1
            && applied["applied_memory_ids"].as_array().unwrap().len() == 1
    );
    Ok(())
}

#[test]
fn finishing_legacy_fast_state_cannot_promote_mixed_or_explicitly_unaccepted_plan_sources()
-> Result<()> {
    let f = Fixture::new("real")?;
    f.store
        .save_comparison(&f.lid, &f.sid, &f.payload("finish-guard"))?;
    let sources = f.store.recent_evidence(&f.sid)?;
    let observed = sources
        .iter()
        .find(|source| source["event_kind"] == "observation")
        .unwrap();
    let plan = sources
        .iter()
        .find(|source| source["content"]["pending_plan"].is_object())
        .unwrap();
    let mut state = f.store.session(&f.sid)?["state"].clone();
    // Simulate a pre-upgrade fast-state candidate already referring to both
    // an observation and an unaccepted plan. Finish must not launder that set.
    let mixed = json!({"source_event_ids":[observed["event_id"],plan["event_id"]]});
    state["field_evidence"]["reported_result"] = mixed.clone();
    state["pending_step"] = plan["content"]["text"].clone();
    state["field_evidence"]["pending_step"] = mixed;
    f.store.checkpoint(&f.sid, &state)?;
    let finished = f.store.consolidate_and_finish(&f.sid)?;
    ensure!(finished["consolidated_memory_ids"] == json!([]));
    ensure!(finished["consolidation"]["omitted_sources"] == 2 && f.count("memories")? == 0);
    Ok(())
}

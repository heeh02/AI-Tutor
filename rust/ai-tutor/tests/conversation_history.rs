//! Temporary, author-written conversations only; no production data or devices.
use ai_tutor::store::{Store, s};
use anyhow::{Result, ensure};
use serde_json::Value;

fn fill(store: &Store, sid: &str, start: usize, end: usize) -> Result<()> {
    for index in start..end {
        store.add_turn(
            sid,
            "synthetic-request",
            0,
            "user",
            &format!("synthetic message {index}"),
            "shown",
            &[],
        )?;
    }
    Ok(())
}
fn texts(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| s(turn, "content").to_owned())
        .collect()
}

#[test]
fn cursor_pages_are_stable_across_new_messages_and_do_not_mix_owners() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(&temp.path().join("history.sqlite3"), "real")?;
    let learner = store.create_learner("synthetic A", "zh", "guided")?;
    let other = store.create_learner("synthetic B", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let other_session = store.create_session(s(&other, "learner_id"))?;
    fill(&store, sid, 0, 123)?;
    fill(&store, s(&other_session, "session_id"), 900, 905)?;
    let first = store.turn_history_page(lid, sid, None, 50)?;
    ensure!(
        texts(&first)
            == (73..123)
                .map(|i| format!("synthetic message {i}"))
                .collect::<Vec<_>>()
    );
    let cursor = first["next_before"].as_i64().unwrap();
    fill(&store, sid, 123, 128)?;
    let second = store.turn_history_page(lid, sid, Some(cursor), 50)?;
    ensure!(
        texts(&second)
            == (23..73)
                .map(|i| format!("synthetic message {i}"))
                .collect::<Vec<_>>()
    );
    let last = store.turn_history_page(lid, sid, second["next_before"].as_i64(), 50)?;
    ensure!(
        texts(&last)
            == (0..23)
                .map(|i| format!("synthetic message {i}"))
                .collect::<Vec<_>>()
    );
    ensure!(last["next_before"].is_null());
    ensure!(
        store
            .turn_history_page(s(&other, "learner_id"), sid, None, 50)
            .is_err()
    );
    ensure!(store.turn_history_page(lid, "missing", None, 50).is_err());
    for turn in first["items"].as_array().unwrap() {
        ensure!(turn.as_object().unwrap().len() == 5);
        ensure!(turn.get("request_id").is_none() && turn.get("evidence").is_none());
    }
    Ok(())
}

#[test]
fn history_reads_finished_sessions_without_mutation_and_enforces_bounds() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(&temp.path().join("finished.sqlite3"), "simulator")?;
    let learner = store.create_learner("synthetic finished", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    ensure!(
        store.turn_history_page(lid, sid, None, 50)?["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fill(&store, sid, 0, 55)?;
    store.add_turn(
        sid,
        "synthetic-undelivered",
        0,
        "assistant",
        "Not yet displayed",
        "generated",
        &[],
    )?;
    store.db.execute(
        "UPDATE sessions SET status='finished' WHERE session_id=?",
        [sid],
    )?;
    let before = store.session(sid)?;
    let page = store.turn_history_page(lid, sid, None, 50)?;
    ensure!(page["items"].as_array().unwrap().len() == 50);
    ensure!(store.session(sid)? == before);
    for limit in [0, 51] {
        ensure!(store.turn_history_page(lid, sid, None, limit).is_err());
    }
    for cursor in [0, -1] {
        ensure!(store.turn_history_page(lid, sid, Some(cursor), 50).is_err());
    }
    Ok(())
}

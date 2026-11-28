use super::*;

const CHILD_NAME_MAX_CHARS: usize = 48;
const ACTIVITY_TITLE_MAX_CHARS: usize = 240;
const CHILD_WORDS_MAX_CHARS: usize = 500;

fn bounded_text(value: &Value, key: &str, max_chars: usize) -> String {
    s(value, key).chars().take(max_chars).collect()
}

pub(super) fn empty_digest() -> Value {
    json!({
        "schema_version": 1,
        "child_name": "",
        "activity_title": "",
        "activity_finished": false,
        "recent_child_words": [],
        "memory_count": 0,
        "has_records": false,
    })
}

pub(super) fn project(store: &Store, session: &Value) -> Result<Value> {
    let learner_id = s(session, "learner_id");
    let learner = store.learner(learner_id)?;
    let child_name = bounded_text(&learner, "name", CHILD_NAME_MAX_CHARS);
    let activity_title = bounded_text(&session["state"], "goal", ACTIVITY_TITLE_MAX_CHARS);
    let activity_finished = s(session, "status") == "finished";
    let recent_child_words = store
        .rows(
            "SELECT content FROM turns WHERE session_id=? AND learner_id=? AND role='user' AND trim(content)<>'' ORDER BY rowid DESC LIMIT 3",
            rusqlite::params![s(session, "session_id"), learner_id],
        )?
        .iter()
        .map(|turn| bounded_text(turn, "content", CHILD_WORDS_MAX_CHARS))
        .collect::<Vec<_>>();
    let memory_count = store
        .one(
            "SELECT COUNT(*) AS count FROM memories WHERE learner_id=? AND status='active'",
            [learner_id],
        )?
        .and_then(|row| row["count"].as_u64())
        .unwrap_or(0);
    let has_records = activity_finished
        || !activity_title.is_empty()
        || !recent_child_words.is_empty()
        || memory_count > 0;

    Ok(json!({
        "schema_version": 1,
        "child_name": child_name,
        "activity_title": activity_title,
        "activity_finished": activity_finished,
        "recent_child_words": recent_child_words,
        "memory_count": memory_count,
        "has_records": has_records,
    }))
}

impl Workstation {
    pub(super) fn parent_digest(&self) -> Result<Value> {
        let current = self.current.lock().unwrap().clone();
        let Some((namespace, session_id)) = current else {
            return Ok(empty_digest());
        };
        let stores = self.stores.lock().unwrap();
        let store = &stores[&namespace];
        let session = store.session(&session_id)?;
        project(store, &session)
    }
}

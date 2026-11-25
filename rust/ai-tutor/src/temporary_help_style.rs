//! Narrow, whole-input temporary help requests. This is not intent extraction.
//! Everything else stays verbatim in latest_input/context for normal teaching.
use serde_json::{Value, json};

fn explicit_one_step(text: &str) -> bool {
    let text = text.trim().trim_end_matches(['.', '!', '。', '！']).trim();
    matches!(
        text.to_ascii_lowercase().as_str(),
        "this time, please give one step at a time"
            | "this time please give one step at a time"
            | "this time, please explain step by step"
            | "this time please explain step by step"
    ) || matches!(
        text,
        "这次请只讲一步"
            | "这次只讲一步"
            | "本次请只讲一步"
            | "本次只讲一步"
            | "这次请一步一步讲"
            | "本次请一步一步讲"
    )
}

pub(super) fn apply(state: &mut Value, text: &str, event_id: &str) {
    if explicit_one_step(text) {
        state["session_preferences"]["help_style"] = json!("one_step");
        state["session_preferences"]["source_event_ids"] = json!([event_id]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::build_context,
        store::{Store, s},
    };
    use anyhow::Result;

    #[test]
    fn whole_positive_requests_preserve_their_source_and_only_override_this_session() -> Result<()>
    {
        for text in [
            "This time, please give one step at a time.",
            "THIS TIME PLEASE EXPLAIN STEP BY STEP!",
            "这次请只讲一步。",
            "本次请一步一步讲！",
        ] {
            let dir = tempfile::tempdir()?;
            let store = Store::open(dir.path().join("synthetic-real.sqlite3"), "real")?;
            let learner = store.create_learner("synthetic temporary request", "en", "guided")?;
            let lid = s(&learner, "learner_id");
            let session = store.create_session(lid)?;
            let sid = s(&session, "session_id");
            let source = store.record_education_event(
                lid,
                sid,
                "learner_report",
                "learner_statement",
                &json!({"text":text}),
            )?;
            let eid = s(&source, "event_id");
            let mut state = session["state"].clone();
            state["latest_input"] = json!(text);
            apply(&mut state, text, eid);
            store.checkpoint(sid, &state)?;
            let saved = store.session(sid)?;
            let context = build_context(&store, &saved, &learner, text, eid, &json!({}))?;
            assert_eq!(context["help_style"], "one_step");
            assert_eq!(
                context["task"]["session_preferences"]["source_event_ids"],
                json!([eid])
            );
            assert_eq!(context["user_input"], text);
            assert_eq!(
                store.source_events(lid, &[eid.to_owned()])?[0]["content"],
                json!({"text":text})
            );
            assert_eq!(store.learner(lid)?["help_style"], "guided");
            assert!(store.memories(lid)?.is_empty());
            assert_eq!(
                store.create_session(lid)?["state"]["session_preferences"],
                json!({})
            );
        }
        Ok(())
    }

    #[test]
    fn negation_questions_quotes_and_durable_requests_keep_the_explicit_input_without_override()
    -> Result<()> {
        for text in [
            "this time, please do not explain step by step.",
            "This time, don't give one step at a time.",
            "This time, should we explain step by step?",
            "This time, please explain step by step?",
            "This time, please explain step by step, unless I say otherwise.",
            "\"This time, please explain step by step.\"",
            "From now on, please give one step at a time.",
            "这次不要一步一步讲。",
            "本次请不要只讲一步。",
            "这次请一步一步讲吗？",
            "以后请只讲一步。",
            "This time the rover took one step.",
        ] {
            let dir = tempfile::tempdir()?;
            let store = Store::open(dir.path().join("synthetic-real.sqlite3"), "real")?;
            let learner = store.create_learner("synthetic unchanged request", "en", "guided")?;
            let lid = s(&learner, "learner_id");
            let session = store.create_session(lid)?;
            let sid = s(&session, "session_id");
            let source = store.record_education_event(
                lid,
                sid,
                "learner_report",
                "learner_statement",
                &json!({"text":text}),
            )?;
            let eid = s(&source, "event_id");
            let mut state = session["state"].clone();
            state["latest_input"] = json!(text);
            let before = state["session_preferences"].clone();
            apply(&mut state, text, eid);
            assert_eq!(state["session_preferences"], before);
            store.checkpoint(sid, &state)?;
            let context = build_context(
                &store,
                &store.session(sid)?,
                &learner,
                text,
                eid,
                &json!({}),
            )?;
            assert_eq!(context["help_style"], "guided");
            assert_eq!(context["user_input"], text);
            assert_eq!(store.session(sid)?["state"]["latest_input"], text);
            assert_eq!(
                store.source_events(lid, &[eid.to_owned()])?[0]["content"],
                json!({"text":text})
            );
            assert!(store.memories(lid)?.is_empty());
        }
        let mut existing =
            json!({"session_preferences":{"help_style":"direct","source_event_ids":["earlier"]}});
        let before = existing.clone();
        apply(
            &mut existing,
            "this time, please do not explain step by step.",
            "now",
        );
        assert_eq!(existing, before);
        Ok(())
    }
}

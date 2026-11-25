//! Current pending-step values are source-exact, not generated advice.
//! Shared by candidate writes and read-only current-state projections.
//! Historical future-intent rules remain separate from current requests.
use regex::Regex;
use serde_json::Value;
use std::{collections::HashSet, sync::LazyLock};

const MAX_VALUE_CHARS: usize = 800;
const MAX_STORED_VALUE_CHARS: usize = 1000;
const MAX_SOURCE_BYTES: usize = 16 * 1024;

fn whole_text_matches(source: &str, value: &str) -> bool {
    // Only surrounding whitespace is ignored. Punctuation, negation, case,
    // numbers, units and internal spacing remain part of the full statement.
    source.len() <= MAX_SOURCE_BYTES && !source.trim().is_empty() && source.trim() == value.trim()
}

fn source_supports(source: &Value, value: &str) -> bool {
    let content = &source["content"];
    let kind = source["event_kind"].as_str().unwrap_or("");
    if content["accepted"] == false || (kind == "plan" && content["accepted"] != true) {
        return false;
    }
    if let Some(declared) = content.get("pending_experiment").and_then(Value::as_str) {
        // This existing structured declaration may explicitly clear a step.
        // Missing/null fields never become empty strings through an accessor.
        return if value.is_empty() {
            declared.is_empty()
        } else {
            whole_text_matches(declared, value)
        };
    }
    if value.is_empty() {
        return false;
    }
    if kind == "plan" {
        return content["text"]
            .as_str()
            .is_some_and(|text| whole_text_matches(text, value));
    }
    if !["learner_statement", "legacy_report"].contains(&kind) {
        return false;
    }
    let report = content["report"].as_str();
    if report.is_some_and(|text| text.len() > MAX_SOURCE_BYTES) {
        return false;
    }
    let raw = report
        .filter(|text| !text.trim().is_empty())
        .or_else(|| content["text"].as_str());
    raw.is_some_and(|text| whole_text_matches(text, value))
}

fn supported(value: &Value, sources: &[Value]) -> bool {
    supported_with_limit(value, sources, MAX_VALUE_CHARS)
}

fn supported_with_limit(value: &Value, sources: &[Value], limit: usize) -> bool {
    let Some(value) = value.as_str() else {
        return false;
    };
    if value.len() > limit * 4
        || value.chars().take(limit + 1).count() > limit
        || (!value.is_empty() && value.trim().is_empty())
        || sources.is_empty()
        || sources.len() > 8
    {
        return false;
    }
    // No source may launder a different or unaccepted plan through a matching one.
    sources.iter().all(|source| source_supports(source, value))
}

pub(crate) fn supported_current(
    value: &Value,
    source_ids: &Value,
    sources: &[Value],
    learner_id: &str,
    session_id: &str,
) -> bool {
    supported_current_with_limit(
        value,
        source_ids,
        sources,
        learner_id,
        session_id,
        MAX_VALUE_CHARS,
    )
}

/// Explicit comparison plans may contain 1000 characters. This read projection
/// does not widen the 800-character model candidate contract.
pub(crate) fn supported_current_read(
    value: &Value,
    source_ids: &Value,
    sources: &[Value],
    learner_id: &str,
    session_id: &str,
) -> bool {
    supported_current_with_limit(
        value,
        source_ids,
        sources,
        learner_id,
        session_id,
        MAX_STORED_VALUE_CHARS,
    )
}

fn supported_current_with_limit(
    value: &Value,
    source_ids: &Value,
    sources: &[Value],
    learner_id: &str,
    session_id: &str,
    limit: usize,
) -> bool {
    let Some(ids) = source_ids.as_array() else {
        return false;
    };
    if !(1..=8).contains(&ids.len()) || learner_id.is_empty() || session_id.is_empty() {
        return false;
    }
    let Some(ids) = ids
        .iter()
        .map(Value::as_str)
        .collect::<Option<HashSet<_>>>()
    else {
        return false;
    };
    if ids.contains("") {
        return false;
    }
    let actual: HashSet<_> = sources
        .iter()
        .filter_map(|v| v["event_id"].as_str())
        .collect();
    sources.len() == ids.len()
        && actual == ids
        && sources.iter().all(|source| {
            source["learner_id"].as_str() == Some(learner_id)
                && source["session_id"].as_str() == Some(session_id)
                && source["event_status"] == "active"
        })
        && supported_with_limit(value, sources, limit)
}

/// The public foreground memory boundary accepts an authorized whole plan,
/// never a generated method merely accompanied by a valid source ID. This is
/// activity/source scoped; it deliberately does not require the latest session.
pub(crate) fn supported_memory(value: &Value, sources: &[Value]) -> bool {
    static FUTURE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new("(?i)下次|下一次|留到|计划|准备|我会|我们会|next time|we will|i will|plan to")
            .unwrap()
    });
    static UNCERTAIN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            "(?i)是否|要不要|还没决定|尚未决定|建议|也许|可能|[?？]|don't|do not|maybe|whether",
        )
        .unwrap()
    });
    value.as_str().is_some_and(|v| !v.trim().is_empty())
        && supported(value, sources)
        && sources.iter().all(|source| {
            let content = &source["content"];
            // source_supports already requires accepted=true for every plan and
            // exact text for a declared pending_experiment.
            if source["event_kind"] == "plan"
                || content["pending_experiment"]
                    .as_str()
                    .is_some_and(|v| !v.is_empty())
            {
                return true;
            }
            // Preserve the established explicit-future distinction used when
            // finishing an activity; do not apply it to current fast requests.
            let raw = content["report"]
                .as_str()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| content["text"].as_str())
                .unwrap_or("");
            FUTURE.is_match(raw) && !UNCERTAIN.is_match(raw)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comparisons_are_whole_statement_bounded_and_do_not_erase_punctuation() {
        let source = |text: &str| json!({"event_kind":"learner_statement","content":{"text":text}});
        assert!(supported(
            &json!("先让小车停下来。"),
            &[source(" \n先让小车停下来。\t")]
        ));
        for (raw, invented) in [
            ("Calculate 5!", "Calculate 5"),
            ("Stop?", "Stop"),
            ("Do not move.", "Do move."),
            ("Set m to 10 cm.", "Set M to 10 cm."),
            ("Set m to 10 cm.", "Set m to 10 m."),
        ] {
            assert!(!supported(&json!(invented), &[source(raw)]));
        }
        let limit = "字".repeat(800);
        assert!(supported(&json!(limit), &[source(&limit)]));
        let too_long = "字".repeat(801);
        assert!(!supported(&json!(too_long), &[source(&too_long)]));
        assert!(!supported(
            &json!("step"),
            &[source(&format!("{}step", " ".repeat(MAX_SOURCE_BYTES)))]
        ));
        assert!(!supported(&json!("step"), &[]));
        assert!(!supported(&json!("step"), &vec![source("step"); 9]));
        assert!(!supported(&Value::Null, &[source("step")]));
    }

    #[test]
    fn clearing_requires_an_explicit_empty_structured_string() {
        for content in [
            json!({}),
            json!({"pending_experiment":null}),
            json!({"pending_experiment":false}),
            json!({"pending_experiment":" "}),
            json!({"text":""}),
            json!({"text":"Clear the plan."}),
        ] {
            assert!(!supported(
                &json!(""),
                &[json!({"event_kind":"learner_statement","content":content})]
            ));
        }
        assert!(supported(
            &json!(""),
            &[json!({"event_kind":"learner_statement","content":{"pending_experiment":""}})]
        ));
        assert!(!supported(
            &json!(" "),
            &[json!({"event_kind":"learner_statement","content":{"pending_experiment":""}})]
        ));
        assert!(!supported(
            &json!("step"),
            &[
                json!({"event_kind":"learner_statement","content":{"text":"step","pending_experiment":""}})
            ]
        ));
    }
}

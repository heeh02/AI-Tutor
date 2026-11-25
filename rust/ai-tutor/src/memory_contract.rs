//! Small wire contract shared by the tool-free provider and shadow worker.
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};

// Prompt semantics and the accepted type vocabulary each have explicit versions.
pub const PROMPT_VERSION: &str = "educational-memory-prompt-v4";
pub const SCHEMA_VERSION: &str = "educational-memory-v2";
pub const POLICY: &str = include_str!("../../../prompts/memory_consolidation.md");

pub fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["schema_version","candidates"],"properties":{
        "schema_version":{"const":SCHEMA_VERSION},
        "candidates":{"type":"array","maxItems":5,"items":{"type":"object","additionalProperties":false,
            "required":["kind","claim_type","content","scope","source_event_ids","source_event_versions","source_quotes","support_level","importance"],
            "properties":{
                "kind":{"enum":["learning_goal","context_constraint","trial_observation","concept_evidence","revisable_misconception","help_preference","accepted_pending_experiment","correction"]},
                "claim_type":{"enum":["stated_goal","condition_limit","reported_observation","task_specific_response","tentative_interpretation","explicit_preference","accepted_plan","explicit_correction"]},
                "content":{"type":"string","minLength":1,"maxLength":600},
                "scope":{"enum":["current_session","current_activity","cross_session_explicit"]},
                "source_event_ids":{"type":"array","minItems":1,"maxItems":8,"uniqueItems":true,"items":{"type":"string"}},
                "source_event_versions":{"type":"object","minProperties":1,"maxProperties":8,"additionalProperties":{"type":"integer","minimum":1}},
                "source_quotes":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"object","additionalProperties":false,"required":["event_id","quote"],"properties":{"event_id":{"type":"string"},"quote":{"type":"string","minLength":1,"maxLength":800}}}},
                "support_level":{"enum":["unknown","independent","hinted","scaffolded"]},
                "importance":{"type":"object","additionalProperties":false,"required":["goal_relevance","continuity","novel_change","explicit_request"],"properties":{
                    "goal_relevance":{"type":"integer","minimum":0,"maximum":3},"continuity":{"type":"integer","minimum":0,"maximum":3},
                    "novel_change":{"type":"integer","minimum":0,"maximum":3},"explicit_request":{"type":"integer","minimum":0,"maximum":3}
                }}
            }
        }}
    }})
}

/// The Store remains the authority for candidate semantics and source versions.
/// This checks the outer protocol without accepting conversational text as data.
pub fn validate_response(value: &Value) -> Result<&[Value]> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("记忆整理响应不是对象"))?;
    ensure!(
        object.len() == 2 && value["schema_version"] == SCHEMA_VERSION,
        "记忆整理响应版本或字段无效"
    );
    let candidates = value["candidates"]
        .as_array()
        .ok_or_else(|| anyhow!("记忆整理候选不是数组"))?;
    ensure!(
        candidates.len() <= 5 && candidates.iter().all(Value::is_object),
        "记忆整理候选数量或类型无效"
    );
    Ok(candidates)
}

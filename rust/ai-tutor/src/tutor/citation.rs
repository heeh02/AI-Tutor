//! Bounded citation diagnostics. Never retain model strings or request content.
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Copy)]
pub(crate) enum CitationSite {
    UsedEvidence,
    TaskCandidate(usize),
    MemoryCandidate(usize),
    Segment(usize),
}

#[derive(Clone, Debug, Serialize)]
struct InvalidReference {
    reference_index: usize,
    id_shape: &'static str,
    matches_request_non_event_identity: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CitationFailure {
    code: &'static str,
    field: &'static str,
    candidate_index: Option<usize>,
    segment_sequence: Option<usize>,
    count_scope: &'static str,
    reference_count: usize,
    invalid_reference_count: usize,
    allowed_event_count: usize,
    invalid_references: Vec<InvalidReference>,
}
impl std::fmt::Display for CitationFailure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(if self.code == "citation_id_type" {
            "模型来源ID无效"
        } else {
            "模型引用了本轮不可用的来源"
        })
    }
}
impl std::error::Error for CitationFailure {}

/// Only a typed cause can be exported, including through anyhow/contract wrappers.
pub(crate) fn diagnostic(error: &anyhow::Error) -> Option<Value> {
    typed_failure(error).and_then(|failure| serde_json::to_value(failure).ok())
}
pub(crate) fn typed_failure(error: &anyhow::Error) -> Option<CitationFailure> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<CitationFailure>().cloned())
}

fn id_shape(value: &Value) -> &'static str {
    let Some(text) = value.as_str() else {
        return "non_string";
    };
    if text.is_empty() {
        return "empty_string";
    }
    let Some((prefix, suffix)) = text.split_once('_') else {
        return "other_string";
    };
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return "other_string";
    }
    match prefix {
        "event" => "event_id",
        "memory" => "memory_id",
        "session" => "session_id",
        "activity" => "activity_id",
        "learner" => "learner_id",
        "turn" => "turn_id",
        "request" => "request_id",
        "choice" => "choice_id",
        _ => "other_string",
    }
}

// Check identity fields only: matching arbitrary prose does not make it an ID.
// No matched value or path is returned. Traversal is of the existing bounded request.
fn matches_identity(value: &Value, text: &str) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (matches!(
                key.as_str(),
                "request_id"
                    | "learner_id"
                    | "session_id"
                    | "activity_id"
                    | "memory_id"
                    | "turn_id"
                    | "source_session_id"
                    | "source_activity_id"
                    | "as_of_session_id"
                    | "client_submission_id"
            ) && value.as_str() == Some(text))
                || matches_identity(value, text)
        }),
        Value::Array(values) => values.iter().any(|value| matches_identity(value, text)),
        _ => false,
    }
}

/// Preserves first-error ordering and the existing array bounds. Counts cover
/// only the failing field, not unvalidated later candidates or the whole reply.
pub(crate) fn check_citations(
    value: &Value,
    valid: &HashSet<String>,
    request: &Value,
    site: CitationSite,
    min: usize,
    max: usize,
) -> Result<()> {
    let ids = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("模型来源字段无效"))?;
    ensure!((min..=max).contains(&ids.len()), "模型来源数量无效");
    // Whole TutorReply already rejects empty strings. Segments historically
    // require set membership only; do not silently change that contract here.
    let invalid = |value: &Value| {
        value.as_str().is_none_or(|id| {
            (!matches!(site, CitationSite::Segment(_)) && id.is_empty()) || !valid.contains(id)
        })
    };
    let Some(first) = ids.iter().find(|value| invalid(value)) else {
        return Ok(());
    };
    let invalid_references: Vec<_> = ids
        .iter()
        .enumerate()
        .filter(|(_, value)| invalid(value))
        .map(|(index, value)| InvalidReference {
            reference_index: index,
            id_shape: id_shape(value),
            matches_request_non_event_identity: value
                .as_str()
                .filter(|id| !id.is_empty())
                .is_some_and(|id| matches_identity(request, id)),
        })
        .collect();
    let (field, candidate_index, segment_sequence) = match site {
        CitationSite::UsedEvidence => ("used_evidence_ids", None, None),
        CitationSite::TaskCandidate(index) => {
            ("task_update_candidates.source_event_ids", Some(index), None)
        }
        CitationSite::MemoryCandidate(index) => {
            ("memory_candidates.source_event_ids", Some(index), None)
        }
        CitationSite::Segment(sequence) => ("segment.used_evidence_ids", None, Some(sequence)),
    };
    Err(CitationFailure {
        code: if first.is_string() {
            "citation_unavailable"
        } else {
            "citation_id_type"
        },
        field,
        candidate_index,
        segment_sequence,
        count_scope: "failing_field_only",
        reference_count: ids.len(),
        invalid_reference_count: invalid_references.len(),
        allowed_event_count: valid.len(),
        invalid_references,
    }
    .into())
}

#[cfg(test)]
mod tests;

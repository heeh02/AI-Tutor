//! Bounded text delivery state. A snapshot is transport input, not a display receipt.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamKey {
    pub session: String,
    pub request: String,
    pub generation: u64,
    pub turn: String,
}
impl StreamKey {
    pub fn from_head(session: &str, head: &Value) -> Option<Self> {
        let request = head["request_id"].as_str()?;
        let turn = head["turn_id"].as_str()?;
        let generation = head["generation"].as_u64()?;
        if session.is_empty() || request.is_empty() || turn.is_empty() || generation == 0 {
            return None;
        }
        Some(Self {
            session: session.into(),
            request: request.into(),
            generation,
            turn: turn.into(),
        })
    }
    pub fn path(&self) -> String {
        format!("/api/sessions/{}/streams/{}", self.session, self.request)
    }
}

pub fn incomplete(phase: &str) -> bool {
    matches!(phase, "failed" | "cancelled" | "interrupted")
}
pub fn terminal(phase: &str) -> bool {
    phase == "completed" || incomplete(phase)
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    revision: u64,
    phase: String,
    confirmed: usize,
    segments: Vec<String>,
}
impl Snapshot {
    fn parse(key: &StreamKey, value: &Value) -> Result<Self, &'static str> {
        if value["protocol_version"] != 1
            || value["session_id"] != key.session
            || value["request_id"] != key.request
            || value["generation"].as_u64() != Some(key.generation)
            || value["turn_id"] != key.turn
        {
            return Err("stream binding");
        }
        let revision = value["revision"].as_u64().ok_or("stream revision")?;
        let phase = value["phase"].as_str().ok_or("stream phase")?;
        if !matches!(phase, "generating" | "final_pending" | "completed") && !incomplete(phase) {
            return Err("stream phase");
        }
        let items = value["segments"].as_array().ok_or("stream segments")?;
        if items.len() > 12 {
            return Err("stream segment limit");
        }
        let mut characters = 0;
        let mut segments = Vec::with_capacity(items.len());
        for (sequence, item) in items.iter().enumerate() {
            let text = item["text"].as_str().ok_or("stream text")?;
            let length = text.chars().count();
            if item["sequence"].as_u64() != Some(sequence as u64)
                || text.trim().is_empty()
                || length > 320
            {
                return Err("stream sequence or text");
            }
            characters += length;
            segments.push(text.to_owned());
        }
        if characters > 1600 {
            return Err("stream reply limit");
        }
        let confirmed = if value["shown_through"].is_null() {
            0
        } else {
            value["shown_through"]
                .as_u64()
                .and_then(|sequence| usize::try_from(sequence).ok())
                .and_then(|sequence| sequence.checked_add(1))
                .filter(|count| *count <= segments.len())
                .ok_or("stream shown prefix")?
        };
        if matches!(phase, "final_pending" | "completed")
            && (value["final_validated"] != true || segments.is_empty())
        {
            return Err("stream final");
        }
        if phase == "completed" && confirmed != segments.len() {
            return Err("stream final display");
        }
        Ok(Self {
            revision,
            phase: phase.into(),
            confirmed,
            segments,
        })
    }
}

pub struct StreamState {
    pub key: StreamKey,
    snapshot: Option<Snapshot>,
    /// Includes at most one rendered but not yet acknowledged segment.
    segments: Vec<String>,
    committed: usize,
}
impl StreamState {
    pub fn new(key: StreamKey) -> Self {
        Self {
            key,
            snapshot: None,
            segments: vec![],
            committed: 0,
        }
    }
    pub fn phase(&self) -> &str {
        self.snapshot.as_ref().map_or("generating", |s| &s.phase)
    }
    pub fn confirmed(&self) -> usize {
        self.snapshot.as_ref().map_or(0, |s| s.confirmed)
    }
    pub fn display_count(&self) -> usize {
        if incomplete(self.phase()) {
            self.committed
                .max(self.confirmed())
                .min(self.segments.len())
        } else {
            (self.confirmed() + 1).min(self.segments.len())
        }
    }
    pub fn visible_segments(&self) -> &[String] {
        &self.segments[..self.display_count()]
    }
    pub fn next_receipt(&self) -> Option<usize> {
        // Final can arrive after the last display receipt. Repeating that
        // receipt retries the final transaction without inventing new display.
        if self.phase() == "final_pending"
            && self.confirmed() == self.segments.len()
            && !self.segments.is_empty()
        {
            return Some(self.segments.len() - 1);
        }
        let new_display = !terminal(self.phase()) && self.confirmed() < self.display_count();
        // A lost response may be retried after failure only for the exact
        // segment already committed locally; no failed tail becomes visible.
        let retry_display = self.phase() == "failed" && self.confirmed() < self.committed;
        (new_display || retry_display).then_some(self.confirmed())
    }
    /// Called only after DOM commit and a visible animation-frame opportunity.
    pub fn commit(&mut self, sequence: usize) -> bool {
        if self.next_receipt() != Some(sequence) {
            return false;
        }
        self.committed = self.committed.max(sequence + 1);
        true
    }
    pub fn apply(&mut self, value: &Value) -> Result<bool, &'static str> {
        let next = Snapshot::parse(&self.key, value)?;
        if let Some(previous) = &self.snapshot {
            if next.revision < previous.revision {
                return Ok(false);
            }
            if next.revision == previous.revision {
                return if &next == previous {
                    Ok(false)
                } else {
                    Err("stream changed revision")
                };
            }
            if next.confirmed < previous.confirmed
                || (terminal(&previous.phase) && next.phase != previous.phase)
                || (!incomplete(&next.phase) && next.segments.len() < self.segments.len())
            {
                return Err("stream regressed");
            }
        }
        if self
            .segments
            .iter()
            .zip(&next.segments)
            .any(|(old, new)| old != new)
        {
            return Err("stream revised segment");
        }
        if incomplete(&next.phase) {
            // A failed response may contain only the server-confirmed prefix.
            // Keep the one already displayed locally while its receipt finishes;
            // never introduce an unshown tail from a terminal response.
            let keep = self.committed.max(next.confirmed);
            if next.segments.len() > self.segments.len() {
                self.segments = next.segments[..next.confirmed].to_vec();
            }
            self.segments.truncate(keep);
        } else {
            self.segments = next.segments.clone();
        }
        self.snapshot = Some(next);
        Ok(true)
    }
}

/// Whole-reply delivery must never acknowledge or automatically read a stream turn.
pub fn is_stream_turn(turn: &Value) -> bool {
    turn["delivery_mode"] == "segments"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn key() -> StreamKey {
        StreamKey {
            session: "s".into(),
            request: "r".into(),
            generation: 3,
            turn: "t".into(),
        }
    }
    fn snapshot(revision: u64, phase: &str, shown: Option<usize>, parts: &[&str]) -> Value {
        json!({"protocol_version":1,"session_id":"s","request_id":"r","generation":3,"turn_id":"t",
            "revision":revision,"phase":phase,"final_validated":matches!(phase,"final_pending"|"completed"),
            "shown_through":shown,"segments":parts.iter().enumerate().map(|(sequence,text)|json!({"sequence":sequence,"text":text})).collect::<Vec<_>>()})
    }
    #[test]
    fn full_snapshots_deduplicate_and_render_only_one_unconfirmed_segment() {
        let mut state = StreamState::new(key());
        let first = snapshot(1, "generating", None, &["先观察。", "Then compare."]);
        assert!(state.apply(&first).unwrap());
        assert_eq!(state.visible_segments(), &["先观察。"]);
        assert!(!state.apply(&first).unwrap());
        assert!(!state.commit(1));
        assert!(state.commit(0));
        assert!(
            state
                .apply(&snapshot(
                    2,
                    "generating",
                    Some(0),
                    &["先观察。", "Then compare."]
                ))
                .unwrap()
        );
        assert_eq!(state.next_receipt(), Some(1));
        assert_eq!(state.visible_segments().len(), 2);
        assert!(!state.apply(&first).unwrap());
    }
    #[test]
    fn gaps_rewrites_and_wrong_owner_cannot_replace_current_text() {
        let mut state = StreamState::new(key());
        state
            .apply(&snapshot(1, "generating", None, &["original"]))
            .unwrap();
        let mut gap = snapshot(2, "generating", None, &["original", "later"]);
        gap["segments"][1]["sequence"] = json!(2);
        assert!(state.apply(&gap).is_err());
        assert!(
            state
                .apply(&snapshot(2, "generating", None, &["revised"]))
                .is_err()
        );
        let mut old = snapshot(2, "generating", None, &["original"]);
        old["request_id"] = json!("old");
        assert!(state.apply(&old).is_err());
        old["request_id"] = json!("r");
        old["generation"] = json!(2);
        assert!(state.apply(&old).is_err());
        assert_eq!(state.visible_segments(), &["original"]);
    }
    #[test]
    fn failure_keeps_displayed_prefix_without_revealing_buffered_tail() {
        let mut state = StreamState::new(key());
        state
            .apply(&snapshot(1, "generating", None, &["visible", "buffered"]))
            .unwrap();
        state.commit(0);
        state.apply(&snapshot(2, "failed", None, &[])).unwrap();
        assert_eq!(state.visible_segments(), &["visible"]);
        assert_eq!(state.next_receipt(), Some(0));
        // A receipt already sent before failure may still finish successfully.
        state
            .apply(&snapshot(3, "failed", Some(0), &["visible"]))
            .unwrap();
        assert_eq!(state.confirmed(), 1);
        assert_eq!(state.visible_segments(), &["visible"]);
        assert_eq!(state.next_receipt(), None);
    }
    #[test]
    fn final_upgrades_the_same_prefix_only_after_every_display_receipt() {
        let mut state = StreamState::new(key());
        state
            .apply(&snapshot(1, "final_pending", None, &["A", "B"]))
            .unwrap();
        assert!(
            state
                .apply(&snapshot(2, "completed", Some(0), &["A", "B"]))
                .is_err()
        );
        state.commit(0);
        state
            .apply(&snapshot(2, "final_pending", Some(0), &["A", "B"]))
            .unwrap();
        state.commit(1);
        state
            .apply(&snapshot(3, "completed", Some(1), &["A", "B"]))
            .unwrap();
        assert_eq!(state.visible_segments().concat(), "AB");
        assert_eq!(state.next_receipt(), None);
        assert_eq!(state.key.turn, "t");
        assert!(is_stream_turn(
            &json!({"delivery_mode":"segments","status":"generated"})
        ));
        assert!(!is_stream_turn(&json!({"status":"generated"})));
    }
    #[test]
    fn final_arriving_after_all_receipts_retries_only_the_last_receipt() {
        let mut state = StreamState::new(key());
        state
            .apply(&snapshot(1, "generating", Some(1), &["A", "B"]))
            .unwrap();
        assert_eq!(state.next_receipt(), None);
        state
            .apply(&snapshot(2, "final_pending", Some(1), &["A", "B"]))
            .unwrap();
        assert_eq!(state.next_receipt(), Some(1));
        assert!(!state.commit(0));
        assert!(state.commit(1));
        state
            .apply(&snapshot(3, "completed", Some(1), &["A", "B"]))
            .unwrap();
        assert_eq!(state.next_receipt(), None);
    }
    #[test]
    fn failure_without_dom_commit_and_cancellation_never_invent_receipts() {
        let mut state = StreamState::new(key());
        state
            .apply(&snapshot(1, "generating", None, &["unshown"]))
            .unwrap();
        state.apply(&snapshot(2, "failed", None, &[])).unwrap();
        assert!(state.visible_segments().is_empty());
        assert_eq!(state.next_receipt(), None);
        let mut cancelled = StreamState::new(key());
        cancelled
            .apply(&snapshot(1, "generating", None, &["shown locally"]))
            .unwrap();
        cancelled.commit(0);
        cancelled
            .apply(&snapshot(2, "cancelled", None, &[]))
            .unwrap();
        assert_eq!(cancelled.visible_segments(), &["shown locally"]);
        assert_eq!(cancelled.next_receipt(), None);
    }
    #[test]
    fn snapshot_limits_and_same_revision_changes_are_rejected() {
        let mut state = StreamState::new(key());
        state
            .apply(&snapshot(1, "generating", None, &["original"]))
            .unwrap();
        assert!(
            state
                .apply(&snapshot(1, "generating", None, &["changed"]))
                .is_err()
        );
        assert!(
            state
                .apply(&snapshot(2, "generating", None, &["x"; 13]))
                .is_err()
        );
        let too_long = "好".repeat(321);
        assert!(
            state
                .apply(&snapshot(2, "generating", None, &[&too_long]))
                .is_err()
        );
        let part = "x".repeat(320);
        assert!(
            state
                .apply(&snapshot(2, "generating", None, &[part.as_str(); 6]))
                .is_err()
        );
        assert_eq!(state.visible_segments(), &["original"]);
    }
}

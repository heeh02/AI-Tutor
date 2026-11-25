//! Bound recording results and two explicit, bounded text-review slots.
use crate::model::{AudioTicket, restore_failed_input};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingTicket {
    pub session_id: String,
    pub session_generation: u64,
    pub language: String,
    pub recording_generation: u64,
}
impl RecordingTicket {
    pub fn parse(value: &Value) -> Option<Self> {
        let session_id = value["session_id"].as_str()?.to_owned();
        let language = value["language"].as_str()?.to_owned();
        let recording_generation = value["recording_generation"].as_u64()?;
        if session_id.is_empty()
            || !matches!(language.as_str(), "zh" | "zh-CN" | "en" | "en-US")
            || recording_generation == 0
        {
            return None;
        }
        Some(Self {
            session_id,
            session_generation: value["session_generation"].as_u64()?,
            language,
            recording_generation,
        })
    }
    pub fn payload(&self) -> Value {
        json!({"session_id":self.session_id,"session_generation":self.session_generation,
            "language":self.language,"recording_generation":self.recording_generation})
    }
}
#[derive(Clone)]
struct RecordingStart {
    intent: AudioTicket,
    revision: u64,
    was_empty: bool,
}
#[derive(Clone)]
pub struct SubmittedDraft {
    pub text: String,
    revision: u64,
    session: String,
    owner_revision: u64,
}
#[derive(Clone, Copy, PartialEq)]
pub enum ReviewKind {
    Transcript,
    Submission,
}
#[derive(Debug, PartialEq)]
pub enum FinalText {
    Fill(String),
    Held,
    Stale,
}

#[derive(Default)]
pub struct RecordingDraft {
    revision: u64,
    owner_revision: u64,
    session: String,
    generation: u64,
    language: String,
    start: Option<RecordingStart>,
    binding: Option<RecordingTicket>,
    transcript: Option<String>,
    uncertain_submission: Option<String>,
}
impl RecordingDraft {
    pub fn edited(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
    /// State can observe our own cancel/bump before the start ACK. Only that
    /// ACK identifies the new recording; a higher generation alone cannot
    /// cancel an as-yet-unbound start.
    pub fn observe(&mut self, session: &str, generation: u64, language: &str) -> bool {
        if self.session != session {
            let active = self.start.is_some();
            *self = Self {
                revision: self.revision.wrapping_add(1),
                owner_revision: self.owner_revision.wrapping_add(1),
                session: session.into(),
                generation,
                language: language.into(),
                ..Self::default()
            };
            return active;
        }
        if generation >= self.generation {
            self.generation = generation;
            self.language = language.into();
        }
        let stale = self.binding.as_ref().is_some_and(|binding| {
            binding.session_generation < self.generation
                || (binding.session_generation == self.generation
                    && binding.language != self.language)
        });
        if stale {
            let active = self.start.is_some();
            self.cancel_recording();
            return active;
        }
        false
    }
    pub fn expected_generation(&self, session: &str, snapshot: u64) -> u64 {
        if session == self.session {
            self.generation.max(snapshot)
        } else {
            snapshot
        }
    }
    pub fn begin(&mut self, intent: AudioTicket, current: &str) -> bool {
        if self.start.is_some() || self.transcript.is_some() || intent.session_id != self.session {
            return false;
        }
        self.start = Some(RecordingStart {
            intent,
            revision: self.revision,
            was_empty: current.is_empty(),
        });
        self.binding = None;
        true
    }
    pub fn is_start(&self, intent: &AudioTicket) -> bool {
        self.start
            .as_ref()
            .is_some_and(|start| &start.intent == intent)
    }
    pub fn bind(&mut self, intent: &AudioTicket, binding: RecordingTicket) -> bool {
        if !self.is_start(intent)
            || binding.session_id != self.session
            || binding.session_generation < self.generation
            || (binding.session_generation == self.generation && binding.language != self.language)
        {
            return false;
        }
        self.generation = binding.session_generation;
        self.language = binding.language.clone();
        self.binding = Some(binding);
        true
    }
    pub fn active_binding(&self) -> Option<RecordingTicket> {
        self.start.as_ref()?;
        self.binding.clone()
    }
    pub fn matches(&self, intent: &AudioTicket, binding: &RecordingTicket) -> bool {
        self.is_start(intent) && self.binding.as_ref() == Some(binding)
    }
    pub fn cancel_recording(&mut self) {
        self.start = None;
        self.binding = None;
        // Already accepted text is a user-owned draft, not an active capture.
        // Preserve both review slots until an explicit action or owner change.
    }
    pub fn finish(
        &mut self,
        intent: &AudioTicket,
        binding: &RecordingTicket,
        text: String,
        current: &str,
    ) -> FinalText {
        if !self.matches(intent, binding) {
            return FinalText::Stale;
        }
        let start = self.start.take().unwrap();
        self.binding = None;
        if start.was_empty && start.revision == self.revision && current.is_empty() {
            FinalText::Fill(text)
        } else {
            self.transcript = Some(text);
            FinalText::Held
        }
    }
    pub fn submitted(&self, text: String) -> SubmittedDraft {
        SubmittedDraft {
            text,
            revision: self.revision,
            session: self.session.clone(),
            owner_revision: self.owner_revision,
        }
    }
    pub fn failed_submission(&mut self, submitted: SubmittedDraft, current: &mut String) -> bool {
        if submitted.session != self.session || submitted.owner_revision != self.owner_revision {
            return false;
        }
        if current.is_empty() && self.revision == submitted.revision {
            restore_failed_input(current, submitted.text.clone());
            self.edited();
        }
        if self.uncertain_submission.is_none() {
            self.uncertain_submission = Some(submitted.text);
        }
        true
    }
    pub fn review(&self, kind: ReviewKind) -> Option<&str> {
        match kind {
            ReviewKind::Transcript => self.transcript.as_deref(),
            ReviewKind::Submission => self.uncertain_submission.as_deref(),
        }
    }
    pub fn has_reviews(&self) -> bool {
        self.transcript.is_some() || self.uncertain_submission.is_some()
    }
    pub fn blocks_navigation(&self, current: &str) -> bool {
        !current.is_empty() || self.has_reviews()
    }
    pub fn dismiss(&mut self, kind: ReviewKind) {
        match kind {
            ReviewKind::Transcript => self.transcript = None,
            ReviewKind::Submission => self.uncertain_submission = None,
        }
    }
    /// Adoption changes only the editable composer and never submits a turn.
    pub fn adopt(
        &mut self,
        kind: ReviewKind,
        append: bool,
        current: &str,
    ) -> Result<Option<String>, ()> {
        let Some(text) = self.review(kind) else {
            return Ok(None);
        };
        let value = if append && !current.is_empty() {
            format!("{current}\n{text}")
        } else {
            text.to_owned()
        };
        if value.chars().count() > 4000 {
            return Err(());
        }
        self.dismiss(kind);
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn intent(epoch: u64) -> AudioTicket {
        AudioTicket {
            epoch,
            session_id: "A".into(),
        }
    }
    fn binding(generation: u64, recording: u64) -> RecordingTicket {
        RecordingTicket {
            session_id: "A".into(),
            session_generation: generation,
            language: "zh".into(),
            recording_generation: recording,
        }
    }
    fn draft(current: &str) -> RecordingDraft {
        let mut state = RecordingDraft::default();
        state.observe("A", 4, "zh");
        assert!(state.begin(intent(1), current));
        assert!(state.bind(&intent(1), binding(5, 10)));
        state
    }
    #[test]
    fn final_fills_only_an_untouched_initially_empty_composer() {
        let mut existing = draft("原有草稿");
        assert_eq!(
            existing.finish(&intent(1), &binding(5, 10), "识别结果".into(), "原有草稿"),
            FinalText::Held
        );
        let mut state = draft("");
        assert_eq!(
            state.finish(&intent(1), &binding(5, 10), "识别结果".into(), ""),
            FinalText::Fill("识别结果".into())
        );
        for original in ["已有草稿", ""] {
            let mut state = draft(original);
            state.edited();
            state.edited(); // Edited, then cleared again.
            assert_eq!(
                state.finish(&intent(1), &binding(5, 10), "识别结果".into(), ""),
                FinalText::Held
            );
            assert_eq!(state.review(ReviewKind::Transcript), Some("识别结果"));
        }
    }
    #[test]
    fn own_generation_poll_before_start_ack_does_not_revoke_the_recording() {
        let mut state = RecordingDraft::default();
        state.observe("A", 4, "zh");
        state.begin(intent(1), "");
        assert!(!state.observe("A", 5, "zh"));
        assert!(state.bind(&intent(1), binding(5, 10)));
        assert!(!state.observe("A", 4, "zh")); // Late pre-start state cannot roll back the ACK.
        assert_eq!(state.expected_generation("A", 4), 5);
        assert!(state.matches(&intent(1), &binding(5, 10)));
    }
    #[test]
    fn later_generation_language_or_owner_rejects_old_ack_and_final() {
        let mut state = RecordingDraft::default();
        state.observe("A", 4, "zh");
        state.begin(intent(1), "");
        state.observe("A", 6, "en");
        assert!(!state.bind(&intent(1), binding(5, 10)));
        let mut state = draft("");
        assert!(state.observe("A", 6, "en"));
        assert_eq!(
            state.finish(&intent(1), &binding(5, 10), "迟到".into(), ""),
            FinalText::Stale
        );
        let mut state = draft("");
        assert!(state.observe("B", 5, "zh"));
        state.observe("A", 5, "zh");
        assert!(!state.bind(&intent(1), binding(5, 10)));
    }
    #[test]
    fn old_final_cannot_consume_a_new_capture_and_bound_payload_is_complete() {
        let mut state = draft("");
        state.cancel_recording();
        assert!(state.begin(intent(2), ""));
        assert!(state.bind(&intent(2), binding(6, 11)));
        assert_eq!(
            state.finish(&intent(1), &binding(5, 10), "旧结果".into(), ""),
            FinalText::Stale
        );
        assert!(state.matches(&intent(2), &binding(6, 11)));
        assert_eq!(
            RecordingTicket::parse(&binding(6, 11).payload()),
            Some(binding(6, 11))
        );
        assert!(
            RecordingTicket::parse(&json!({"session_id":"A","recording_generation":11})).is_none()
        );
    }
    #[test]
    fn transcript_and_uncertain_submission_have_independent_review_slots() {
        let mut state = draft("已有草稿");
        state.finish(&intent(1), &binding(5, 10), "转写".into(), "已有草稿");
        let submitted = state.submitted("发送原文".into());
        state.edited();
        let mut current = "新编辑".to_owned();
        state.failed_submission(submitted, &mut current);
        assert_eq!(current, "新编辑");
        assert_eq!(state.review(ReviewKind::Transcript), Some("转写"));
        assert_eq!(state.review(ReviewKind::Submission), Some("发送原文"));
        assert!(!state.begin(intent(2), &current));
        assert_eq!(
            state.adopt(ReviewKind::Transcript, true, &current),
            Ok(Some("新编辑\n转写".into()))
        );
        assert_eq!(state.review(ReviewKind::Submission), Some("发送原文"));
        assert!(state.has_reviews());
        state.dismiss(ReviewKind::Submission);
        assert!(!state.has_reviews());
    }
    #[test]
    fn an_edited_then_cleared_draft_is_not_restored_over_after_uncertain_send() {
        let mut state = RecordingDraft::default();
        let submitted = state.submitted("原提交".into());
        state.edited();
        state.edited();
        let mut current = String::new();
        state.failed_submission(submitted, &mut current);
        assert!(current.is_empty());
        assert_eq!(state.review(ReviewKind::Submission), Some("原提交"));
        state.observe("another", 0, "en");
        assert!(!state.has_reviews());
    }
    #[test]
    fn adoption_never_truncates_an_existing_draft_to_fit_the_message_limit() {
        let mut state = draft("x");
        state.finish(&intent(1), &binding(5, 10), "转写".into(), "x");
        assert_eq!(
            state.adopt(ReviewKind::Transcript, true, &"x".repeat(4000)),
            Err(())
        );
        assert_eq!(state.review(ReviewKind::Transcript), Some("转写"));
    }
    #[test]
    fn language_aliases_remain_exact_in_every_bound_request() {
        for language in ["zh", "zh-CN", "en", "en-US"] {
            let mut value = binding(5, 10).payload();
            value["language"] = json!(language);
            let ticket = RecordingTicket::parse(&value).unwrap();
            assert_eq!(ticket.language, language);
            assert_eq!(ticket.payload(), value);
        }
    }
    #[test]
    fn cancelling_before_start_ack_keeps_the_close_intent_after_visibility_returns() {
        let mut state = RecordingDraft::default();
        state.observe("A", 4, "zh");
        state.begin(intent(1), "");
        state.cancel_recording();
        state.observe("A", 5, "zh");
        assert!(!state.bind(&intent(1), binding(5, 10)));
        assert!(state.active_binding().is_none());
        assert!(state.begin(intent(2), ""));
        assert!(state.bind(&intent(2), binding(6, 11)));
        assert!(!state.matches(&intent(1), &binding(5, 10)));
    }
    #[test]
    fn even_an_untouched_restored_submission_requires_explicit_review_before_resending() {
        let mut state = RecordingDraft::default();
        state.observe("A", 4, "zh");
        let submitted = state.submitted("发送原文".into());
        let mut current = String::new();
        state.failed_submission(submitted, &mut current);
        assert_eq!(current, "发送原文");
        assert_eq!(state.review(ReviewKind::Submission), Some("发送原文"));
        assert!(state.blocks_navigation(""));
        state.failed_submission(state.submitted("迟到的另一错误".into()), &mut current);
        assert_eq!(state.review(ReviewKind::Submission), Some("发送原文"));
        state.dismiss(ReviewKind::Submission);
        assert!(state.blocks_navigation(&current));
        assert!(!state.blocks_navigation(""));
    }
    #[test]
    fn send_then_stop_then_transport_error_preserves_new_edits_and_submitted_text() {
        let mut state = RecordingDraft::default();
        state.observe("A", 4, "zh");
        let mut audio = crate::model::IntentGuard::default();
        audio.set_session("A");
        let original_audio = audio.capture();
        let submitted = state.submitted("已提交的原文".into());
        audio.invalidate();
        state.cancel_recording(); // Stop supersedes audio/action ownership.
        state.edited();
        let mut current = "停止后新写的草稿".to_owned();
        assert!(!audio.valid(&original_audio));
        assert!(state.failed_submission(submitted, &mut current));
        assert_eq!(current, "停止后新写的草稿");
        assert_eq!(state.review(ReviewKind::Submission), Some("已提交的原文"));
    }
    #[test]
    fn late_send_error_cannot_return_text_after_leaving_and_reentering_the_same_session() {
        let mut state = RecordingDraft::default();
        state.observe("A", 4, "zh");
        let submitted = state.submitted("旧会话原文".into());
        state.observe("B", 0, "en");
        state.observe("A", 5, "zh");
        let mut current = "当前草稿".to_owned();
        assert!(!state.failed_submission(submitted, &mut current));
        assert_eq!(current, "当前草稿");
        assert!(!state.has_reviews());
    }
}

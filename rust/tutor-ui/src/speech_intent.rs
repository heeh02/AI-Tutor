//! Browser intent for one automatic speech stream. The server owns all audio.
//! A submission nonce correlates a lost HTTP response with its persisted head;
//! it is neither authorization nor a server idempotency key.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechKey {
    pub session: String,
    pub request: String,
    pub generation: u64,
}
impl SpeechKey {
    pub fn from_head(session: &str, head: &Value) -> Option<Self> {
        let request = head["request_id"].as_str()?;
        let generation = head["generation"].as_u64()?;
        (!session.is_empty() && !request.is_empty() && generation > 0).then(|| Self {
            session: session.into(),
            request: request.into(),
            generation,
        })
    }
    pub fn close_path(&self) -> String {
        format!(
            "/api/sessions/{}/streams/{}/speech/close",
            self.session, self.request
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseSpeech {
    pub key: SpeechKey,
    pub reason: &'static str,
}
struct Submission {
    nonce: String,
    close_reason: Option<&'static str>,
}
struct Current {
    key: SpeechKey,
    gate: String,
    close_reason: Option<&'static str>,
    confirmed_closed: bool,
    close_failed: bool,
    complete: bool,
    completed_through: Option<u64>,
}
#[derive(Default)]
pub struct SpeechIntent {
    session: String,
    submission: Option<Submission>,
    current: Option<Current>,
}
impl SpeechIntent {
    pub fn bind_session(&mut self, session: &str) {
        if self.session != session {
            *self = Self {
                session: session.into(),
                ..Self::default()
            };
        }
    }
    pub fn begin(&mut self, session: &str, nonce: String) {
        self.bind_session(session);
        // Every new Send replaces only the unresolved submission intention.
        // Already-issued close requests retain their exact request binding.
        self.submission = Some(Submission {
            nonce,
            close_reason: None,
        });
    }
    pub fn close(&mut self, reason: &'static str) {
        if let Some(submission) = &mut self.submission {
            submission.close_reason.get_or_insert(reason);
        }
        if let Some(current) = &mut self.current {
            current.close_reason.get_or_insert(reason);
        }
    }
    pub fn observe(&mut self, session: &str, head: &Value) {
        self.bind_session(session);
        // A transient missing head must not erase an unconfirmed close latch.
        let Some(key) = SpeechKey::from_head(session, head) else {
            return;
        };
        // A state request may have started before the submit response. Its
        // older head cannot erase the new request's pending close intention.
        if self.current.as_ref().is_some_and(|current| {
            key.generation < current.key.generation
                || (key.generation == current.key.generation && key.request != current.key.request)
        }) {
            return;
        }
        let gate = head["speech_gate"].as_str().unwrap_or("disabled");
        if self
            .current
            .as_ref()
            .is_none_or(|current| current.key != key)
        {
            self.current = Some(Current {
                key,
                gate: gate.into(),
                close_reason: None,
                confirmed_closed: gate != "open",
                close_failed: false,
                complete: false,
                completed_through: None,
            });
        }
        let current = self.current.as_mut().unwrap();
        if matches!(gate, "closed" | "disabled") {
            current.confirmed_closed = true;
            current.close_failed = false;
            current.gate = gate.into();
        } else if !current.confirmed_closed {
            current.gate = "open".into();
        }
        current.complete |= head["speech_complete"] == true;
        current.completed_through = current
            .completed_through
            .max(head["speech_completed_through"].as_u64());
        if self.submission.as_ref().is_some_and(|pending| {
            head["client_submission_id"].as_str() == Some(pending.nonce.as_str())
        }) {
            if let Some(reason) = self.submission.take().unwrap().close_reason {
                current.close_reason.get_or_insert(reason);
            }
        }
    }
    pub fn submitted(&mut self, session: &str, reply: &Value) {
        if self.session != session
            || !self.submission.as_ref().is_some_and(|pending| {
                reply["client_submission_id"].as_str() == Some(pending.nonce.as_str())
            })
        {
            return;
        }
        if reply["delivery_mode"] == "segments" && SpeechKey::from_head(session, reply).is_some() {
            self.observe(session, reply);
        } else if reply["request_id"].is_null() || reply["delivery_mode"] == "whole_reply" {
            self.submission = None;
        }
    }
    pub fn close_job(&self) -> Option<CloseSpeech> {
        let current = self.current.as_ref()?;
        if current.confirmed_closed {
            return None;
        }
        Some(CloseSpeech {
            key: current.key.clone(),
            reason: current.close_reason?,
        })
    }
    pub fn closed(&mut self, key: &SpeechKey, result: Option<&Value>) -> bool {
        let Some(current) = self.current.as_mut().filter(|current| current.key == *key) else {
            return false;
        };
        let confirmed = result.is_some_and(|value| {
            value["ok"] == true
                && value["request_id"] == key.request
                && value["generation"].as_u64() == Some(key.generation)
                && matches!(value["speech_gate"].as_str(), Some("closed" | "disabled"))
        });
        current.close_failed = !confirmed;
        if confirmed {
            current.confirmed_closed = true;
            current.gate = "closed".into();
        }
        confirmed
    }
    pub fn allows_ack(&self, key: &SpeechKey) -> bool {
        self.current.as_ref().is_none_or(|current| {
            current.key != *key || current.close_reason.is_none() || current.confirmed_closed
        })
    }
    pub fn can_close(&self, key: &SpeechKey) -> bool {
        self.current.as_ref().is_some_and(|current| {
            current.key == *key && !current.confirmed_closed && current.close_reason.is_none()
        })
    }
    pub fn status(&self, key: &SpeechKey, language: &str) -> Option<String> {
        let current = self
            .current
            .as_ref()
            .filter(|current| current.key == *key)?;
        let (zh, en) = if current.close_reason.is_some() && !current.confirmed_closed {
            if current.close_failed {
                (
                    "关闭播报尚未确认；保持关闭意图，连接恢复后重试。",
                    "Stopping speech is not confirmed; it will be retried when the connection recovers.",
                )
            } else {
                (
                    "正在关闭本条自动播报…",
                    "Stopping automatic speech for this reply…",
                )
            }
        } else if current.gate == "closed" {
            (
                "本条自动播报已关闭；重新勾选只影响下次发送。",
                "Automatic speech is off for this reply. Re-enabling it applies to the next send.",
            )
        } else if current.complete {
            (
                "本条分句播报已完成。",
                "Segment speech completed for this reply.",
            )
        } else if current.gate == "open" {
            (
                "本条已启用分句朗读；在显示确认后依次播报。",
                "Segment speech is enabled for this reply and follows display confirmation.",
            )
        } else {
            (
                "本条仅显示文字，不自动朗读。",
                "This reply is text only; automatic speech is disabled.",
            )
        };
        let mut status = if language == "en" { en } else { zh }.to_owned();
        if let Some(through) = current.completed_through.filter(|through| *through < 12) {
            status.push_str(&if language == "en" {
                format!(" Completed segments: {}.", through + 1)
            } else {
                format!(" 已完成 {} 段。", through + 1)
            });
        }
        Some(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn head(nonce: &str, request: &str, generation: u64) -> Value {
        json!({"client_submission_id":nonce,"request_id":request,"generation":generation,"delivery_mode":"segments","speech_gate":"open","phase":"generating"})
    }
    fn confirmed(key: &SpeechKey) -> Value {
        json!({"ok":true,"request_id":key.request,"generation":key.generation,"speech_gate":"closed"})
    }
    #[test]
    fn lost_submit_response_binds_close_by_nonce_before_display() {
        let mut state = SpeechIntent::default();
        state.begin("s", "nonce1".into());
        state.close("hidden");
        let h = head("nonce1", "r1", 2);
        state.observe("s", &h);
        let job = state.close_job().unwrap();
        assert_eq!(job.reason, "hidden");
        assert!(!state.allows_ack(&job.key));
        assert!(state.closed(&job.key, Some(&confirmed(&job.key))));
        assert!(state.allows_ack(&job.key));
        assert!(state.close_job().is_none());
    }
    #[test]
    fn old_unbound_nonce_never_closes_the_next_identical_input_request() {
        let mut state = SpeechIntent::default();
        state.begin("s", "old".into());
        state.close("auto_speak_off");
        state.begin("s", "new".into());
        state.observe("s", &head("old", "old-request", 2));
        assert!(state.close_job().is_none());
        state.observe("s", &head("new", "new-request", 4));
        assert!(state.close_job().is_none());
        state.submitted("s", &head("old", "old-request", 2));
        assert!(state.close_job().is_none());
    }
    #[test]
    fn unrelated_head_does_not_consume_a_pending_close() {
        let mut state = SpeechIntent::default();
        state.begin("s", "mine".into());
        state.close("history");
        state.observe("s", &head("another", "other", 3));
        assert!(state.close_job().is_none());
        state.observe("s", &head("mine", "mine", 4));
        assert_eq!(state.close_job().unwrap().reason, "history");
    }
    #[test]
    fn closed_latch_survives_stale_open_head_and_duplicate_submit_receipt() {
        let mut state = SpeechIntent::default();
        state.begin("s", "n".into());
        let h = head("n", "r", 2);
        state.submitted("s", &h);
        state.close("user");
        let job = state.close_job().unwrap();
        state.closed(&job.key, Some(&confirmed(&job.key)));
        state.observe("s", &h);
        state.submitted("s", &h);
        assert!(state.close_job().is_none());
        assert!(state.status(&job.key, "en").unwrap().contains("off"));
    }
    #[test]
    fn failed_close_holds_ack_but_does_not_change_text_final() {
        let mut state = SpeechIntent::default();
        let mut h = head("n", "r", 2);
        h["phase"] = json!("completed");
        state.observe("s", &h);
        state.close("auto_speak_off");
        let job = state.close_job().unwrap();
        assert!(!state.closed(&job.key, None));
        assert_eq!(state.close_job().unwrap(), job);
        assert!(!state.allows_ack(&job.key));
        assert_eq!(h["phase"], "completed");
        let mut wrong = confirmed(&job.key);
        wrong["generation"] = json!(4);
        assert!(!state.closed(&job.key, Some(&wrong)));
    }
    #[test]
    fn late_close_and_profile_switch_cannot_close_a_new_owner() {
        let mut state = SpeechIntent::default();
        state.observe("s", &head("a", "a", 2));
        state.close("user");
        let job = state.close_job().unwrap();
        state.observe("s", &head("b", "b", 4));
        assert!(!state.closed(&job.key, Some(&confirmed(&job.key))));
        assert!(state.close_job().is_none());
        state.begin("s", "pending".into());
        state.close("hidden");
        state.observe("other", &head("pending", "new", 2));
        assert!(state.close_job().is_none());
    }
    #[test]
    fn disabled_legacy_heads_never_request_audio_or_block_display() {
        let mut state = SpeechIntent::default();
        let mut h = head("n", "r", 2);
        h.as_object_mut().unwrap().remove("speech_gate");
        state.observe("s", &h);
        state.close("hidden");
        assert!(state.close_job().is_none());
        assert!(state.allows_ack(&SpeechKey::from_head("s", &h).unwrap()));
    }
    #[test]
    fn temporarily_missing_head_preserves_an_unconfirmed_close() {
        let mut state = SpeechIntent::default();
        let h = head("n", "r", 2);
        state.observe("s", &h);
        state.close("user");
        let job = state.close_job().unwrap();
        state.observe("s", &Value::Null);
        state.observe("s", &h);
        assert_eq!(state.close_job().unwrap(), job);
        assert!(!state.allows_ack(&job.key));
    }
    #[test]
    fn server_closed_gate_and_audio_complete_never_infer_text_completion() {
        let mut state = SpeechIntent::default();
        let mut h = head("n", "r", 2);
        h["speech_gate"] = json!("closed");
        state.observe("s", &h);
        let key = SpeechKey::from_head("s", &h).unwrap();
        assert!(state.allows_ack(&key));
        assert!(!state.can_close(&key));
        assert_eq!(h["phase"], "generating");
        h["speech_gate"] = json!("open");
        state.observe("s", &h);
        assert!(!state.can_close(&key));
        assert!(state.status(&key, "en").unwrap().contains("off"));
    }
    #[test]
    fn state_started_before_submit_cannot_erase_its_unconfirmed_close() {
        let mut state = SpeechIntent::default();
        state.begin("s", "new".into());
        let new = head("new", "new", 4);
        state.submitted("s", &new);
        state.close("user");
        let job = state.close_job().unwrap();
        state.observe("s", &head("old", "old", 2));
        state.observe("s", &new);
        assert!(!state.closed(&job.key, None));
        assert_eq!(state.close_job().unwrap(), job);
        assert!(!state.allows_ack(&job.key));
    }
}

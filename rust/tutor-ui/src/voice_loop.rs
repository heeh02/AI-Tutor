//! Page-local permission for an experimental half-duplex conversation.
//! Only a matching server heartbeat may authorize another capture. Text or
//! playback UI completion never supplies that permission.
use crate::recording_draft::RecordingTicket;
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub session: String,
    pub learner: String,
    pub language: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub key: String,
    pub revision: u64,
}
impl Device {
    pub fn parse(value: &Value) -> Option<Self> {
        if value["stable_binding_available"] != true {
            return None;
        }
        let key = value["device_key"].as_str()?.to_owned();
        (!key.is_empty()).then_some(Self {
            key,
            revision: value["input_topology_revision"].as_u64()?,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopKey {
    pub session: String,
    pub client_id: String,
    pub epoch: u64,
}
impl LoopKey {
    pub fn path(&self, action: &str) -> String {
        format!(
            "/api/sessions/{}/voice-loop/{}/{action}",
            self.session, self.client_id
        )
    }
    pub fn payload(&self) -> Value {
        json!({"client_loop_id":self.client_id,"loop_epoch":self.epoch})
    }
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Off,
    Enabling,
    Armed,
    Starting,
    Listening,
    Finalizing,
    ReadyToSubmit,
    Submitting,
    Reply,
    Paused,
}
#[derive(Clone, Debug)]
struct Intent {
    owner: Owner,
    device: Device,
    client_id: String,
    generation: u64,
    key: Option<LoopKey>,
    revision: u64,
    recording: Option<u64>,
    speech_ended: bool,
    submission: Option<String>,
    barge_in: bool,
    reply_request: Option<String>,
    reply_audio_epoch: Option<u64>,
    reply_ack_sent: bool,
    adoption: Option<RecordingTicket>,
}
#[derive(Default, Clone, Debug)]
pub struct VoiceLoop {
    pub phase: Phase,
    pub pause_reason: String,
    intent: Option<Intent>,
    pub monitoring: bool,
}
impl VoiceLoop {
    pub fn active(&self) -> bool {
        !matches!(self.phase, Phase::Off | Phase::Paused)
    }
    pub fn key(&self) -> Option<LoopKey> {
        self.intent.as_ref()?.key.clone()
    }
    pub fn begin(
        &mut self,
        owner: Owner,
        device: Device,
        generation: u64,
        client_id: String,
        barge_in: bool,
    ) -> Value {
        let mut payload = json!({"client_loop_id":client_id,"expected_generation":generation,
            "language":owner.language,"device_key":device.key,
            "expected_input_topology_revision":device.revision});
        if barge_in {
            payload["barge_in"] = json!(true);
        }
        self.intent = Some(Intent {
            owner,
            device,
            generation,
            client_id,
            key: None,
            revision: 0,
            recording: None,
            speech_ended: false,
            submission: None,
            barge_in,
            reply_request: None,
            reply_audio_epoch: None,
            reply_ack_sent: false,
            adoption: None,
        });
        self.phase = Phase::Enabling;
        self.pause_reason.clear();
        self.monitoring = false;
        payload
    }
    /// Accept only the exact intent, including a late enable after local pause.
    /// The latter supplies a key to close, but cannot arm a recording.
    pub fn enabled(&mut self, client_id: &str, value: &Value) -> bool {
        let Some(intent) = self.intent.as_mut() else {
            return false;
        };
        if intent.client_id != client_id
            || !identity_matches(intent, value)
            || value["session_generation"].as_u64() != Some(intent.generation)
            || value["lease_ms"].as_u64() != Some(3000)
            || value["barge_in"].as_bool().unwrap_or(false) != intent.barge_in
        {
            return false;
        }
        let Some(epoch) = value["loop_epoch"].as_u64().filter(|epoch| *epoch > 0) else {
            return false;
        };
        let Some(revision) = value["revision"].as_u64() else {
            return false;
        };
        intent.key = Some(LoopKey {
            session: intent.owner.session.clone(),
            client_id: client_id.into(),
            epoch,
        });
        intent.revision = revision;
        if self.phase == Phase::Enabling {
            if value["phase"] != "armed" || value["rearm_ready"] != true {
                return false;
            }
            self.phase = Phase::Armed;
        }
        true
    }
    pub fn pause(&mut self, reason: &str) -> Option<LoopKey> {
        if !self.active() {
            return None;
        }
        self.phase = Phase::Paused;
        self.monitoring = false;
        self.pause_reason = reason.into();
        self.key()
    }
    pub fn owner_changed(&self, owner: &Owner, generation: u64) -> bool {
        self.active()
            && self.intent.as_ref().is_some_and(|intent| {
                &intent.owner != owner
                    || generation
                        > intent.generation.saturating_add(match self.phase {
                            Phase::Starting => 1,
                            Phase::Submitting => 2,
                            _ => 0,
                        })
            })
    }
    pub fn interruption_enabled(&self) -> bool {
        self.intent.as_ref().is_some_and(|intent| intent.barge_in)
    }
    pub fn needs_reply_ack(&self) -> bool {
        self.phase == Phase::Reply
            && self
                .intent
                .as_ref()
                .is_some_and(|intent| intent.barge_in && !intent.reply_ack_sent)
    }
    pub fn heartbeat_payload(&mut self) -> Option<Value> {
        let intent = self.intent.as_mut()?;
        let key = intent.key.as_ref()?;
        let mut value = json!({"loop_epoch":key.epoch});
        if self.phase == Phase::Reply && intent.barge_in {
            value["reply_ack"] = json!({"request_id":intent.reply_request.as_ref()?,
                "generation":intent.generation,"audio_epoch":intent.reply_audio_epoch?});
            intent.reply_ack_sent = true;
        }
        Some(value)
    }

    /// Only this explicit, reply-bound receipt can adopt an already-open input.
    /// A generic generation increase or Listening heartbeat never authorizes it.
    pub fn adopt_barge_in(
        &mut self,
        key: &LoopKey,
        value: &Value,
    ) -> Result<Option<RecordingTicket>, ()> {
        let Some(adoption) = value.get("barge_in_adoption").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        if !self.active() {
            return Ok(None);
        }
        let intent = self.intent.as_mut().ok_or(())?;
        if intent.key.as_ref() != Some(key)
            || !identity_matches(intent, value)
            || value["loop_epoch"].as_u64() != Some(key.epoch)
        {
            return Err(());
        }
        let revision = value["revision"].as_u64().ok_or(())?;
        if revision < intent.revision {
            return Ok(None);
        }
        let ticket = RecordingTicket::parse(adoption).ok_or(())?;
        if intent.adoption.as_ref() == Some(&ticket) {
            return Ok(None);
        }
        if !intent.barge_in
            || self.phase != Phase::Reply
            || value["phase"] != "listening"
            || value["barge_in"] != true
            || adoption["kind"] != "speech_barge_in_v1"
            || adoption["from_request_id"].as_str() != intent.reply_request.as_deref()
            || adoption["from_generation"].as_u64() != Some(intent.generation)
            || adoption["from_audio_epoch"].as_u64() != intent.reply_audio_epoch
            || ticket.session_id != intent.owner.session
            || ticket.language != intent.owner.language
            || ticket.session_generation != intent.generation.saturating_add(1)
            || value["session_generation"].as_u64() != Some(ticket.session_generation)
            || ticket.recording_generation <= intent.recording.unwrap_or(0)
            || adoption["device_key"] != intent.device.key
            || adoption["input_topology_revision"].as_u64() != Some(intent.device.revision)
            || adoption["sample_rate"] != 16000
            || !adoption["pre_roll_samples"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= 4800)
        {
            return Err(());
        }
        intent.generation = ticket.session_generation;
        intent.recording = Some(ticket.recording_generation);
        intent.revision = revision;
        intent.speech_ended = false;
        intent.submission = None;
        intent.reply_request = None;
        intent.reply_audio_epoch = None;
        intent.adoption = Some(ticket.clone());
        self.monitoring = false;
        self.phase = Phase::Listening;
        Ok(Some(ticket))
    }
    /// Returns false for a stale/foreign heartbeat. A matching paused server
    /// response is still valid; its caller must close the page-local intent.
    pub fn heartbeat(&mut self, key: &LoopKey, value: &Value) -> bool {
        let Some(intent) = self.intent.as_mut() else {
            return false;
        };
        if intent.key.as_ref() != Some(key)
            || !identity_matches(intent, value)
            || value["loop_epoch"].as_u64() != Some(key.epoch)
            || value["lease_ms"].as_u64() != Some(3000)
        {
            return false;
        }
        let (Some(revision), Some(generation)) = (
            value["revision"].as_u64(),
            value["session_generation"].as_u64(),
        ) else {
            return false;
        };
        if revision < intent.revision || generation < intent.generation {
            // A heartbeat begun before the Start/Send receipt may arrive later.
            // It renews no local permission and cannot reverse a newer phase.
            return true;
        }
        intent.revision = revision;
        self.monitoring =
            self.phase == Phase::Reply && intent.barge_in && value["monitoring"] == true;
        // Only Start/Send receipts commit their new generation locally.
        if generation
            > intent.generation.saturating_add(match self.phase {
                Phase::Starting => 1,
                Phase::Submitting => 2,
                _ => 0,
            })
        {
            return false;
        }
        if self.phase == Phase::Reply && value["phase"] == "armed" && value["rearm_ready"] == true {
            self.phase = Phase::Armed;
        } else if self.phase == Phase::Armed
            && (value["phase"] != "armed" || value["rearm_ready"] != true)
        {
            self.phase = Phase::Reply;
        }
        true
    }
    pub fn start_payload(&mut self) -> Option<Value> {
        if self.phase != Phase::Armed {
            return None;
        }
        let intent = self.intent.as_mut()?;
        let key = intent.key.as_ref()?;
        let value = json!({"session_id":intent.owner.session,"expected_generation":intent.generation,
            "device_key":intent.device.key,"expected_input_topology_revision":intent.device.revision,
            "endpoint_mode":"voice_endpoint_v1","voice_loop":key.payload()});
        intent.recording = None;
        intent.speech_ended = false;
        intent.submission = None;
        intent.reply_request = None;
        intent.reply_audio_epoch = None;
        self.phase = Phase::Starting;
        Some(value)
    }
    pub fn started(&mut self, ticket: &RecordingTicket) -> bool {
        let Some(intent) = self.intent.as_mut() else {
            return false;
        };
        if self.phase != Phase::Starting
            || ticket.session_id != intent.owner.session
            || ticket.language != intent.owner.language
            || ticket.session_generation != intent.generation.saturating_add(1)
        {
            return false;
        }
        intent.generation = ticket.session_generation;
        intent.recording = Some(ticket.recording_generation);
        self.phase = Phase::Listening;
        true
    }
    pub fn start_echo_matches(&self, value: &Value) -> bool {
        let Some(intent) = self.intent.as_ref() else {
            return false;
        };
        let Some(key) = intent.key.as_ref() else {
            return false;
        };
        let echo = &value["voice_loop"];
        identity_matches(intent, echo)
            && echo["loop_epoch"].as_u64() == Some(key.epoch)
            && echo["session_generation"] == value["session_generation"]
            && value["device_key"] == intent.device.key
            && value["input_topology_revision"].as_u64() == Some(intent.device.revision)
            && value["endpoint_mode"] == "voice_endpoint_v1"
    }
    pub fn endpoint(&mut self, ticket: &RecordingTicket, value: &Value) -> Option<String> {
        let intent = self.intent.as_mut()?;
        if self.phase != Phase::Listening
            || intent.recording != Some(ticket.recording_generation)
            || ticket.session_id != intent.owner.session
            || ticket.session_generation != intent.generation
            || ticket.language != intent.owner.language
            || value["input_closed"] != true
            || value["method_id"] != "voice_endpoint_v1"
            || value["protocol_version"] != 1
            || value["recording_generation"].as_u64() != intent.recording
        {
            return None;
        }
        let reason = value["reason"].as_str()?;
        if !matches!(
            reason,
            "speech_end" | "no_speech" | "duration_limit" | "device_error"
        ) {
            return None;
        }
        self.phase = Phase::Finalizing;
        intent.speech_ended = reason == "speech_end";
        Some(reason.into())
    }
    pub fn final_ready(&mut self, ticket: &RecordingTicket) -> bool {
        if self.phase != Phase::Finalizing
            || !self.intent.as_ref().is_some_and(|intent| {
                intent.speech_ended
                    && intent.recording == Some(ticket.recording_generation)
                    && intent.generation == ticket.session_generation
                    && intent.owner.session == ticket.session_id
                    && intent.owner.language == ticket.language
            })
        {
            return false;
        }
        self.phase = Phase::ReadyToSubmit;
        true
    }
    pub fn submission_payload(&mut self, nonce: &str) -> Option<Value> {
        if self.phase != Phase::ReadyToSubmit {
            return None;
        }
        let intent = self.intent.as_mut()?;
        let value = json!({"voice_loop":intent.key.as_ref()?.payload(),
            "recording_generation":intent.recording?,"expected_generation":intent.generation});
        intent.submission = Some(nonce.into());
        self.phase = Phase::Submitting;
        Some(value)
    }
    pub fn submitted(&mut self, value: &Value) -> bool {
        let Some(intent) = self.intent.as_mut() else {
            return false;
        };
        if self.phase != Phase::Submitting
            || value["session_id"] != intent.owner.session
            || value["client_submission_id"].as_str() != intent.submission.as_deref()
            || value["request_id"].as_str().is_none_or(str::is_empty)
            || value["delivery_mode"] != "segments"
        {
            return false;
        }
        let Some(generation) = value["generation"]
            .as_u64()
            .filter(|generation| *generation == intent.generation.saturating_add(2))
        else {
            return false;
        };
        let echo = &value["voice_loop"];
        if !identity_matches(intent, echo)
            || echo["loop_epoch"].as_u64() != intent.key.as_ref().map(|key| key.epoch)
            || echo["session_generation"].as_u64() != Some(generation)
        {
            return false;
        }
        if intent.barge_in
            && (echo["barge_in"] != true
                || echo["reply_request_id"] != value["request_id"]
                || echo["reply_audio_epoch"]
                    .as_u64()
                    .is_none_or(|epoch| epoch == 0))
        {
            return false;
        }
        let Some(revision) = echo["revision"].as_u64() else {
            return false;
        };
        intent.generation = generation;
        intent.revision = intent.revision.max(revision);
        intent.reply_request = value["request_id"].as_str().map(str::to_owned);
        intent.reply_audio_epoch = echo["reply_audio_epoch"].as_u64();
        intent.reply_ack_sent = false;
        self.monitoring = false;
        self.phase = Phase::Reply;
        true
    }
}
fn identity_matches(intent: &Intent, value: &Value) -> bool {
    value["client_loop_id"] == intent.client_id
        && value["session_id"] == intent.owner.session
        && value["language"] == intent.owner.language
        && value["device_key"] == intent.device.key
        && value["input_topology_revision"].as_u64() == Some(intent.device.revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (VoiceLoop, Value, RecordingTicket) {
        let mut state = VoiceLoop::default();
        state.begin(
            Owner {
                session: "s".into(),
                learner: "l".into(),
                language: "en-US".into(),
            },
            Device {
                key: "mic".into(),
                revision: 4,
            },
            2,
            "n".into(),
            false,
        );
        let echo = json!({"client_loop_id":"n","session_id":"s","language":"en-US",
            "session_generation":2,"device_key":"mic","input_topology_revision":4,
            "loop_epoch":9,"revision":1,"phase":"armed","rearm_ready":true,"lease_ms":3000});
        let ticket = RecordingTicket {
            session_id: "s".into(),
            session_generation: 3,
            language: "en-US".into(),
            recording_generation: 10,
        };
        (state, echo, ticket)
    }

    fn barge_reply() -> (VoiceLoop, Value, RecordingTicket) {
        let (mut state, mut echo, ticket) = setup();
        let intent = state.intent.as_ref().unwrap().clone();
        let request = state.begin(intent.owner, intent.device, 2, "n".into(), true);
        assert_eq!(request["barge_in"], true);
        echo["barge_in"] = json!(true);
        assert!(state.enabled("n", &echo));
        assert!(state
            .heartbeat_payload()
            .unwrap()
            .get("reply_ack")
            .is_none());
        state.start_payload();
        assert!(state.started(&ticket));
        assert_eq!(
            state
                .endpoint(
                    &ticket,
                    &json!({"protocol_version":1,"method_id":"voice_endpoint_v1",
            "input_closed":true,"reason":"speech_end","recording_generation":10})
                )
                .as_deref(),
            Some("speech_end")
        );
        assert!(state.final_ready(&ticket));
        state.submission_payload("send");
        assert!(state
            .heartbeat_payload()
            .unwrap()
            .get("reply_ack")
            .is_none());
        echo["session_generation"] = json!(5);
        echo["revision"] = json!(3);
        echo["phase"] = json!("reply");
        echo["rearm_ready"] = json!(false);
        echo["reply_request_id"] = json!("r");
        echo["reply_audio_epoch"] = json!(12);
        assert!(
            state.submitted(&json!({"session_id":"s","client_submission_id":"send",
            "request_id":"r","generation":5,"delivery_mode":"segments","voice_loop":echo}))
        );
        assert!(state.needs_reply_ack());
        assert_eq!(
            state.heartbeat_payload().unwrap()["reply_ack"],
            json!({"request_id":"r","generation":5,"audio_epoch":12})
        );
        assert!(!state.needs_reply_ack());
        let adopted = RecordingTicket {
            session_id: "s".into(),
            session_generation: 6,
            language: "en-US".into(),
            recording_generation: 11,
        };
        echo["session_generation"] = json!(6);
        echo["revision"] = json!(4);
        echo["phase"] = json!("listening");
        let mut receipt = adopted.payload();
        for (key,value) in json!({"kind":"speech_barge_in_v1","from_request_id":"r","from_generation":5,
            "from_audio_epoch":12,"device_key":"mic","input_topology_revision":4,"sample_rate":16000,"pre_roll_samples":4800}).as_object().unwrap() {
            receipt[key]=value.clone();
        }
        echo["barge_in_adoption"] = receipt;
        (state, echo, adopted)
    }

    #[test]
    fn explicit_barge_receipt_adopts_once_and_reuses_the_normal_final_submission_path() {
        let (mut state, echo, ticket) = barge_reply();
        let key = state.key().unwrap();
        assert_eq!(state.adopt_barge_in(&key, &echo), Ok(Some(ticket.clone())));
        assert_eq!(state.phase, Phase::Listening);
        assert!(state.start_payload().is_none());
        assert_eq!(state.adopt_barge_in(&key, &echo), Ok(None));
        assert!(state.heartbeat(&key, &echo));
        state.endpoint(
            &ticket,
            &json!({"protocol_version":1,"method_id":"voice_endpoint_v1",
            "input_closed":true,"reason":"speech_end","recording_generation":11}),
        );
        assert!(state.final_ready(&ticket));
        assert_eq!(
            state.submission_payload("next").unwrap()["recording_generation"],
            11
        );
        assert_eq!(state.adopt_barge_in(&key, &echo), Ok(None));
        assert_eq!(state.phase, Phase::Submitting);
    }

    #[test]
    fn generation_increase_without_a_matching_barge_receipt_never_authorizes_capture() {
        let (state, echo, _) = barge_reply();
        let key = state.key().unwrap();
        let mut missing = echo.clone();
        missing.as_object_mut().unwrap().remove("barge_in_adoption");
        let mut copy = state.clone();
        assert_eq!(copy.adopt_barge_in(&key, &missing), Ok(None));
        assert!(!copy.heartbeat(&key, &missing));
        assert_eq!(copy.phase, Phase::Reply);
        for (field, value) in [
            ("kind", json!("other")),
            ("from_request_id", json!("old")),
            ("from_generation", json!(4)),
            ("from_audio_epoch", json!(13)),
            ("recording_generation", json!(10)),
            ("device_key", json!("other")),
            ("language", json!("zh")),
            ("pre_roll_samples", json!(0)),
        ] {
            let mut wrong = echo.clone();
            wrong["barge_in_adoption"][field] = value;
            let mut copy = state.clone();
            assert!(copy.adopt_barge_in(&key, &wrong).is_err(), "{field}");
            assert_eq!(copy.phase, Phase::Reply);
        }
    }

    #[test]
    fn paused_or_stale_barge_notifications_cannot_reopen_listening() {
        let (mut state, echo, _) = barge_reply();
        let key = state.key().unwrap();
        state.pause("hidden");
        assert_eq!(state.adopt_barge_in(&key, &echo), Ok(None));
        assert_eq!(state.phase, Phase::Paused);
        let (mut state, mut echo, _) = barge_reply();
        echo["revision"] = json!(2);
        assert_eq!(state.adopt_barge_in(&key, &echo), Ok(None));
        assert_eq!(state.phase, Phase::Reply);
    }
    #[test]
    fn late_enable_only_provides_a_key_to_stop() {
        let (mut state, echo, _) = setup();
        state.pause("hidden");
        assert!(state.enabled("n", &echo));
        assert!(state.key().is_some());
        assert!(state.start_payload().is_none());
    }
    #[test]
    fn entire_cycle_needs_successful_submit_and_new_server_rearm() {
        let (mut state, mut echo, ticket) = setup();
        assert!(state.enabled("n", &echo));
        assert!(state.start_payload().is_some());
        assert!(state.start_payload().is_none());
        assert!(state.started(&ticket));
        let endpoint = json!({"protocol_version":1,"method_id":"voice_endpoint_v1","input_closed":true,"reason":"speech_end","recording_generation":10});
        assert_eq!(
            state.endpoint(&ticket, &endpoint).as_deref(),
            Some("speech_end")
        );
        assert!(state.endpoint(&ticket, &endpoint).is_none());
        assert!(state.final_ready(&ticket));
        assert!(state.submission_payload("send").is_some());
        assert!(state.submission_payload("send").is_none());
        echo["session_generation"] = json!(3);
        assert!(state.heartbeat(&state.key().unwrap(), &echo));
        assert_eq!(state.phase, Phase::Submitting);
        echo["session_generation"] = json!(5);
        assert!(
            state.submitted(&json!({"session_id":"s","client_submission_id":"send",
            "request_id":"r","generation":5,"delivery_mode":"segments","voice_loop":echo}))
        );
        echo["revision"] = json!(2);
        echo["rearm_ready"] = json!(false);
        assert!(state.heartbeat(&state.key().unwrap(), &echo));
        assert_eq!(state.phase, Phase::Reply);
        echo["rearm_ready"] = json!(true);
        echo["revision"] = json!(3);
        assert!(state.heartbeat(&state.key().unwrap(), &echo));
        assert!(state.start_payload().is_some());
    }
    #[test]
    fn edit_pause_cannot_be_undone_by_final_or_heartbeat() {
        let (mut state, mut echo, ticket) = setup();
        state.enabled("n", &echo);
        state.start_payload();
        state.started(&ticket);
        state.pause("edited");
        echo["session_generation"] = json!(3);
        assert!(state.heartbeat(&state.key().unwrap(), &echo));
        assert!(!state.final_ready(&ticket));
        assert!(state.start_payload().is_none());
    }
    #[test]
    fn device_and_request_identity_do_not_fall_back_to_names() {
        assert!(Device::parse(&json!({"device_id":0,"name":"mic"})).is_none());
        let (mut state, mut echo, _) = setup();
        echo["device_key"] = json!("other");
        assert!(!state.enabled("n", &echo));
        assert!(state.start_payload().is_none());
        echo["device_key"] = json!("mic");
        assert!(!state.enabled("old", &echo));
    }
    #[test]
    fn own_start_generation_waits_for_ticket_but_owner_change_stops() {
        let (mut state, echo, _) = setup();
        state.enabled("n", &echo);
        state.start_payload();
        let mut owner = Owner {
            session: "s".into(),
            learner: "l".into(),
            language: "en-US".into(),
        };
        assert!(!state.owner_changed(&owner, 3));
        owner.language = "zh".into();
        assert!(state.owner_changed(&owner, 3));
    }
    #[test]
    fn stale_endpoint_and_stale_heartbeat_cannot_rearm() {
        let (mut state, mut echo, ticket) = setup();
        state.enabled("n", &echo);
        state.start_payload();
        state.started(&ticket);
        assert!(state
            .endpoint(
                &ticket,
                &json!({"protocol_version":1,"method_id":"voice_endpoint_v1","input_closed":true,
            "reason":"speech_end","recording_generation":9})
            )
            .is_none());
        echo["revision"] = json!(0);
        assert!(state.heartbeat(&state.key().unwrap(), &echo));
        assert_eq!(state.phase, Phase::Listening);
    }
    #[test]
    fn duration_limit_never_authorizes_a_send_even_with_nonempty_final() {
        let (mut state, echo, ticket) = setup();
        state.enabled("n", &echo);
        state.start_payload();
        state.started(&ticket);
        assert_eq!(
            state
                .endpoint(
                    &ticket,
                    &json!({"protocol_version":1,"method_id":"voice_endpoint_v1",
            "input_closed":true,"reason":"duration_limit","recording_generation":10})
                )
                .as_deref(),
            Some("duration_limit")
        );
        assert!(!state.final_ready(&ticket));
        assert!(state.submission_payload("send").is_none());
    }
    #[test]
    fn unknown_submission_is_not_recovered_from_a_later_armed_head() {
        let (mut state, mut echo, ticket) = setup();
        state.enabled("n", &echo);
        state.start_payload();
        state.started(&ticket);
        state.endpoint(
            &ticket,
            &json!({"protocol_version":1,"method_id":"voice_endpoint_v1",
            "input_closed":true,"reason":"speech_end","recording_generation":10}),
        );
        state.final_ready(&ticket);
        state.submission_payload("send");
        state.pause("submission_unknown");
        echo["session_generation"] = json!(5);
        echo["revision"] = json!(9);
        assert!(!state.heartbeat(&state.key().unwrap(), &echo));
        assert!(state.start_payload().is_none());
        assert!(!state.submitted(
            &json!({"session_id":"s","client_submission_id":"send","request_id":"r",
            "generation":5,"delivery_mode":"segments","voice_loop":echo})
        ));
    }
    #[test]
    fn start_echo_binds_the_selected_device_and_permission() {
        let (mut state, mut echo, ticket) = setup();
        state.enabled("n", &echo);
        state.start_payload();
        echo["session_generation"] = json!(ticket.session_generation);
        let mut receipt = json!({"session_generation":ticket.session_generation,"voice_loop":echo,
            "device_key":"mic","input_topology_revision":4,"endpoint_mode":"voice_endpoint_v1"});
        assert!(state.start_echo_matches(&receipt));
        receipt["voice_loop"]["loop_epoch"] = json!(8);
        assert!(!state.start_echo_matches(&receipt));
        receipt["voice_loop"]["loop_epoch"] = json!(9);
        receipt["device_key"] = json!("replacement");
        assert!(!state.start_echo_matches(&receipt));
    }
    #[test]
    fn newer_heartbeat_revokes_an_unused_rearm_and_old_echo_cannot_restore_it() {
        let (mut state, mut echo, _) = setup();
        state.enabled("n", &echo);
        let old = echo.clone();
        echo["revision"] = json!(2);
        echo["phase"] = json!("reply");
        echo["rearm_ready"] = json!(false);
        assert!(state.heartbeat(&state.key().unwrap(), &echo));
        assert_eq!(state.phase, Phase::Reply);
        assert!(state.start_payload().is_none());
        assert!(state.heartbeat(&state.key().unwrap(), &old));
        assert!(state.start_payload().is_none());
        echo["revision"] = json!(3);
        echo["phase"] = json!("armed");
        echo["rearm_ready"] = json!(true);
        assert!(state.heartbeat(&state.key().unwrap(), &echo));
        assert!(state.start_payload().is_some());
    }
}

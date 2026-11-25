use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};

/// An explicit return to current messages is a retry opportunity, not another
/// delivery. Successful and in-flight receipts retain their existing ownership.
pub fn retry_unshown_turns(
    turns: &Value,
    shown: &VecDeque<String>,
    pending: &HashSet<String>,
    attempts: &mut HashMap<String, u8>,
    blocked: &mut HashSet<String>,
) {
    for turn in turns.as_array().into_iter().flatten() {
        let Some(id) = turn["turn_id"].as_str() else {
            continue;
        };
        if turn["role"] == "assistant"
            && turn["status"] == "generated"
            && turn["content"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
            && !shown.iter().any(|shown| shown == id)
            && !pending.contains(id)
        {
            attempts.remove(id);
            blocked.remove(id);
        }
    }
}

pub const MODULES: [(&str, &str); 4] = [
    ("control", "主控"),
    ("motor", "电机"),
    ("distance", "测距"),
    ("light", "灯"),
];

pub fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(v) => v.clone(),
        Value::Number(v) => v.to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Object(_) => ["value", "content", "text", "report"]
            .iter()
            .find_map(|key| value.get(*key).filter(|v| !v.is_null()).map(text))
            .unwrap_or_default(),
        Value::Array(values) => values
            .iter()
            .map(text)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("；"),
    }
}

/// A report is independent of inventory confirmation. Untouched checkboxes
/// must never turn a report-only submission into an empty hardware inventory.
#[derive(Default, Debug)]
pub struct ModuleDraft {
    session_id: String,
    pub modules: Vec<String>,
    pub modules_dirty: bool,
    pub report: String,
}
impl ModuleDraft {
    pub fn sync(&mut self, session_id: &str, inventory: &Value) {
        if self.session_id != session_id {
            *self = Self {
                session_id: session_id.to_owned(),
                ..Self::default()
            };
        }
        if !self.modules_dirty {
            let inventory = inventory.get("value").unwrap_or(inventory);
            self.modules = MODULES
                .iter()
                .filter(|(id, _)| {
                    inventory
                        .as_array()
                        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(*id)))
                })
                .map(|(id, _)| (*id).to_owned())
                .collect();
        }
    }
    pub fn set_module(&mut self, id: &str, checked: bool) {
        if !MODULES.iter().any(|(known, _)| *known == id) {
            return;
        }
        self.modules.retain(|module| module != id);
        if checked {
            self.modules.push(id.to_owned());
        }
        self.modules_dirty = true;
    }
    pub fn payload(&self, source: &str) -> Option<Value> {
        if !self.modules_dirty && self.report.trim().is_empty() {
            return None;
        }
        let mut payload = json!({"source_type": source});
        if self.modules_dirty {
            payload["modules"] = json!(self.modules);
        }
        if !self.report.trim().is_empty() {
            payload["report"] = json!(self.report.trim());
        }
        Some(payload)
    }
    pub fn saved(&mut self, session_id: &str, payload: &Value) {
        if self.session_id != session_id {
            return;
        }
        if payload
            .get("modules")
            .is_some_and(|modules| *modules == json!(self.modules))
        {
            self.modules_dirty = false;
        }
        if payload["report"].as_str() == Some(self.report.trim()) {
            self.report.clear();
        }
    }
}

pub fn preferences(value: &Value) -> String {
    let mut values = Vec::new();
    if let Some(style) = value["help_style"].as_str() {
        values.push(match style {
            "one_step" => "一次一步".to_owned(),
            "direct" => "直接解释".to_owned(),
            "guided" => "适量提示".to_owned(),
            _ => String::new(),
        });
    }
    let explicit = text(&value["explicit_request"]);
    if !explicit.is_empty() {
        values.push(explicit);
    }
    let values = values
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        "使用档案设置".into()
    } else {
        values.join("；")
    }
}

pub fn fallback(value: &Value, default: &str) -> String {
    let value = text(value);
    if value.is_empty() {
        default.to_owned()
    } else {
        value
    }
}

pub fn list(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}
/// Recover an editable question after an asynchronous provider failure.
/// This only reads the current session and never sends or retries a request.
pub fn recoverable_input(session: &Value) -> Option<String> {
    if session["status"] != "error" {
        return None;
    }
    session["turns"]
        .as_array()?
        .iter()
        .rev()
        .find(|turn| turn["role"] == "user")
        .and_then(|turn| turn["content"].as_str())
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

/// A failed request may arrive after the learner has begun a new question.
pub fn restore_failed_input(current: &mut String, submitted: String) {
    if current.is_empty() {
        *current = submitted;
    }
}

/// The newest request owns a list even when the user leaves and returns to
/// the same learner while an earlier response is still in flight.
#[derive(Default)]
pub struct RequestVersion(u64);
impl RequestVersion {
    pub fn advance(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
    pub fn current(&self, version: u64) -> bool {
        self.0 == version
    }
}

/// Conversation windows can roll over at a fixed length. Follow their newest
/// turn ID, while retaining the user's decision to read earlier messages.
pub struct ChatViewport {
    session: String,
    last_turn: String,
    at_bottom: bool,
    pub unseen: bool,
    scroll_requested: bool,
    history: bool,
    display_revision: u64,
}
impl Default for ChatViewport {
    fn default() -> Self {
        Self {
            session: String::new(),
            last_turn: String::new(),
            at_bottom: true,
            unseen: false,
            scroll_requested: false,
            history: false,
            display_revision: 0,
        }
    }
}
impl ChatViewport {
    /// Streaming extends an existing turn; its stable ID alone cannot signal
    /// new content. Keep the same scroll policy as an additional live turn.
    pub fn content_changed(&mut self) {
        if self.at_bottom && !self.history {
            self.scroll_requested = true;
        } else {
            self.unseen = true;
        }
    }
    pub fn observe(&mut self, session: &str, turns: &Value) {
        let last = turns
            .as_array()
            .and_then(|turns| turns.last())
            .and_then(|turn| turn["turn_id"].as_str())
            .unwrap_or("");
        if self.session != session {
            *self = Self {
                session: session.into(),
                last_turn: last.into(),
                scroll_requested: true,
                ..Self::default()
            };
        } else if self.last_turn != last {
            self.last_turn = last.into();
            if last.is_empty() {
                self.unseen = false;
            } else if self.at_bottom && !self.history {
                self.scroll_requested = true;
            } else {
                self.unseen = true;
            }
        }
    }
    /// Repaint both the new-message affordance and the streaming display gate.
    pub fn scrolled(&mut self, scroll_top: i32, scroll_height: i32, client_height: i32) -> bool {
        if self.history {
            return false;
        }
        let was_unseen = self.unseen;
        let was_at_bottom = self.at_bottom;
        self.at_bottom =
            i64::from(scroll_height) - i64::from(client_height) - i64::from(scroll_top.max(0))
                <= 48;
        if self.at_bottom {
            self.unseen = false;
        } else {
            // A user scroll between the state update and render wins.
            self.scroll_requested = false;
        }
        was_unseen != self.unseen || was_at_bottom != self.at_bottom
    }
    pub fn stream_following(&self) -> bool {
        self.at_bottom && !self.history
    }
    pub fn history_open(&self) -> bool {
        self.history
    }
    pub fn set_history(&mut self, open: bool) {
        if self.history == open {
            return;
        }
        self.history = open;
        if open {
            self.scroll_requested = false;
            self.display_revision = self.display_revision.wrapping_add(1);
        }
    }
    pub fn display_revision(&self) -> u64 {
        self.display_revision
    }
    pub fn automatic_read_current(&self, revision: u64) -> bool {
        !self.history && revision == self.display_revision
    }
    pub fn show_latest(&mut self) {
        self.history = false;
        self.at_bottom = true;
        self.unseen = false;
        self.scroll_requested = true;
    }
    pub fn take_scroll_request(&mut self) -> bool {
        std::mem::take(&mut self.scroll_requested)
    }
}

pub fn milliseconds(value: &Value) -> String {
    value
        .as_f64()
        .map(|v| format!("{v:.0} ms"))
        .unwrap_or_else(|| "未知".into())
}
pub fn track_label(track: &Value) -> String {
    track["label"]
        .as_str()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or("临时目标")
        .to_owned()
}
pub fn selected_track_label(tracks: &Value, selected: &str) -> String {
    if selected.is_empty() {
        return "等待教师选择".into();
    }
    tracks
        .as_array()
        .and_then(|tracks| {
            tracks
                .iter()
                .find(|track| track["track_key"].as_str() == Some(selected))
        })
        .map(track_label)
        .unwrap_or_else(|| "已选目标 · 暂时丢失".into())
}
pub fn frame_box(bounds: &Value, mirrored: bool) -> Option<String> {
    let xy = bounds.as_array()?;
    if xy.len() != 4 {
        return None;
    }
    let x1 = xy[0].as_f64()?;
    let y1 = xy[1].as_f64()?;
    let x2 = xy[2].as_f64()?;
    let y2 = xy[3].as_f64()?;
    if [x1, y1, x2, y2]
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        || x2 < x1
        || y2 < y1
    {
        return None;
    }
    Some(format!(
        "left:{:.4}%;top:{:.4}%;width:{:.4}%;height:{:.4}%",
        (if mirrored { 1.0 - x2 } else { x1 }) * 100.0,
        y1 * 100.0,
        (x2 - x1) * 100.0,
        (y2 - y1) * 100.0
    ))
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioTicket {
    pub epoch: u64,
    pub session_id: String,
}
#[derive(Default, Debug)]
pub struct IntentGuard {
    epoch: u64,
    session_id: String,
}
impl IntentGuard {
    pub fn set_session(&mut self, value: &str) {
        if self.session_id != value {
            self.session_id = value.into();
            self.invalidate();
        }
    }
    pub fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }
    pub fn capture(&self) -> AudioTicket {
        AudioTicket {
            epoch: self.epoch,
            session_id: self.session_id.clone(),
        }
    }
    pub fn valid(&self, ticket: &AudioTicket) -> bool {
        ticket.epoch == self.epoch && ticket.session_id == self.session_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn turn_window(first: u32, last: u32) -> Value {
        json!(
            (first..=last)
                .map(|id| json!({"turn_id":format!("turn-{id}"),"content":"观察"}))
                .collect::<Vec<_>>()
        )
    }
    #[test]
    fn rolling_twenty_four_turn_window_follows_the_newest_id() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        assert!(viewport.take_scroll_request());
        viewport.observe("a", &turn_window(2, 25));
        assert!(
            viewport.take_scroll_request(),
            "equal length must not hide a new turn"
        );
        viewport.observe("a", &turn_window(2, 25));
        assert!(
            !viewport.take_scroll_request(),
            "state polling must not force scrolling"
        );
    }
    #[test]
    fn reading_older_turns_exposes_an_entry_without_forcing_scroll() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        viewport.take_scroll_request();
        viewport.scrolled(200, 1000, 300);
        viewport.observe("a", &turn_window(2, 25));
        assert!(viewport.unseen);
        assert!(!viewport.take_scroll_request());
        viewport.observe("a", &turn_window(3, 26));
        assert!(!viewport.take_scroll_request());
        viewport.show_latest();
        assert!(!viewport.unseen);
        assert!(viewport.take_scroll_request());
        viewport.observe("a", &turn_window(4, 27));
        assert!(viewport.take_scroll_request());
    }
    #[test]
    fn streaming_same_turn_growth_respects_scroll_and_history() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        viewport.take_scroll_request();
        viewport.content_changed();
        assert!(viewport.take_scroll_request());
        viewport.scrolled(200, 1000, 300);
        viewport.content_changed();
        assert!(viewport.unseen);
        assert!(!viewport.take_scroll_request());
        viewport.show_latest();
        viewport.take_scroll_request();
        viewport.set_history(true);
        viewport.content_changed();
        assert!(viewport.unseen);
        assert!(!viewport.take_scroll_request());
    }
    #[test]
    fn stream_follow_gate_repaints_on_scroll_and_survives_same_turn_growth() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        viewport.take_scroll_request();
        assert!(viewport.stream_following());
        // DOM growth requests another bottom alignment but does not rewrite
        // the follow intent just because its new height has not painted yet.
        viewport.content_changed();
        assert!(viewport.stream_following());
        assert!(viewport.take_scroll_request());
        viewport.scrolled(500, 800, 300);
        assert!(viewport.stream_following());
        assert!(
            viewport.scrolled(200, 800, 300),
            "gate change must repaint even with no unseen badge"
        );
        assert!(!viewport.stream_following());
        assert!(!viewport.unseen);
        viewport.content_changed();
        assert!(viewport.unseen);
        assert!(!viewport.stream_following());
        assert!(!viewport.take_scroll_request());
        viewport.show_latest();
        assert!(viewport.stream_following());
        assert!(viewport.take_scroll_request());
        viewport.content_changed();
        assert!(viewport.stream_following());
        assert!(viewport.take_scroll_request());
        viewport.scrolled(1000, 1300, 300);
        assert!(viewport.stream_following());
        viewport.set_history(true);
        assert!(!viewport.stream_following());
    }
    #[test]
    fn history_browsing_keeps_new_replies_without_forcing_the_live_window_down() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        viewport.take_scroll_request();
        viewport.set_history(true);
        viewport.observe("a", &turn_window(2, 25));
        assert!(viewport.unseen);
        assert!(!viewport.take_scroll_request());
        // Hiding the live DOM must not look like the user scrolled to bottom.
        viewport.scrolled(0, 0, 0);
        assert!(viewport.unseen);
        viewport.observe("a", &turn_window(30, 53));
        viewport.set_history(false);
        assert!(viewport.unseen);
        assert!(!viewport.take_scroll_request());
        viewport.show_latest();
        assert!(!viewport.history_open() && !viewport.unseen);
        assert!(viewport.take_scroll_request());
    }
    #[test]
    fn a_display_receipt_from_before_history_cannot_restart_automatic_speech() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        let previous = viewport.display_revision();
        assert!(viewport.automatic_read_current(previous));
        viewport.set_history(true);
        assert!(!viewport.automatic_read_current(previous));
        viewport.set_history(false);
        assert!(!viewport.automatic_read_current(previous));
        assert!(viewport.automatic_read_current(viewport.display_revision()));
        viewport.set_history(true);
        viewport.observe("b", &turn_window(1, 24));
        assert!(!viewport.history_open());
    }
    #[test]
    fn explicit_return_retries_only_current_unshown_nonpending_assistant_turns() {
        let turns = json!([
            {"turn_id":"retry","role":"assistant","status":"generated","content":"New reply"},
            {"turn_id":"shown","role":"assistant","status":"generated","content":"Already shown, poll is stale"},
            {"turn_id":"pending","role":"assistant","status":"generated","content":"Receipt in flight"},
            {"turn_id":"cancelled","role":"assistant","status":"cancelled","content":"Obsolete"},
            {"turn_id":"user","role":"user","status":"generated","content":"Learner text"}
        ]);
        let mut shown = VecDeque::from(["shown".into()]);
        let pending = HashSet::from(["pending".into()]);
        let mut attempts: HashMap<String, u8> = [
            "retry",
            "shown",
            "pending",
            "cancelled",
            "user",
            "another-session",
        ]
        .into_iter()
        .map(|id| (id.into(), 2))
        .collect();
        let mut blocked: HashSet<String> = attempts.keys().cloned().collect();
        retry_unshown_turns(&turns, &shown, &pending, &mut attempts, &mut blocked);
        assert!(!attempts.contains_key("retry") && !blocked.contains("retry"));
        for id in ["shown", "pending", "cancelled", "user", "another-session"] {
            assert_eq!(attempts.get(id), Some(&2));
            assert!(blocked.contains(id));
        }
        // Once that retried acknowledgement succeeds, another return does not
        // reopen it even if /state still briefly says generated.
        shown.push_back("retry".into());
        attempts.insert("retry".into(), 1);
        retry_unshown_turns(&turns, &shown, &pending, &mut attempts, &mut blocked);
        assert_eq!(attempts.get("retry"), Some(&1));
        assert!(pending.contains("pending"));
    }
    #[test]
    fn manually_returning_to_bottom_clears_unseen_and_session_switch_resets_it() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        viewport.take_scroll_request();
        viewport.scrolled(0, 1000, 300);
        viewport.observe("a", &turn_window(2, 25));
        assert!(viewport.scrolled(700, 1000, 300));
        assert!(!viewport.unseen);
        viewport.scrolled(0, 1000, 300);
        viewport.observe("a", &turn_window(3, 26));
        viewport.observe("b", &turn_window(1, 24));
        assert!(!viewport.unseen);
        assert!(viewport.take_scroll_request());
    }
    #[test]
    fn user_scroll_before_render_cancels_queued_follow() {
        let mut viewport = ChatViewport::default();
        viewport.observe("a", &turn_window(1, 24));
        viewport.scrolled(50, 1000, 300);
        assert!(!viewport.take_scroll_request());
        viewport.observe("a", &json!([]));
        assert!(!viewport.unseen);
    }
    #[test]
    fn failed_send_does_not_replace_a_newer_draft() {
        let mut input = "下一次观察".to_owned();
        restore_failed_input(&mut input, "旧问题".into());
        assert_eq!(input, "下一次观察");
        input.clear();
        restore_failed_input(&mut input, "旧问题".into());
        assert_eq!(input, "旧问题");
    }
    #[test]
    fn late_memory_lists_cannot_replace_a_refresh_or_a_returned_profile() {
        let mut requests = RequestVersion::default();
        let before_correction = requests.advance();
        let after_correction = requests.advance();
        assert!(!requests.current(before_correction));
        assert!(requests.current(after_correction));
        requests.advance(); // Changing profiles invalidates outstanding reads.
        assert!(!requests.current(after_correction));
        let returned_profile = requests.advance();
        assert!(!requests.current(before_correction));
        assert!(requests.current(returned_profile));
    }
    #[test]
    fn provider_failure_recovers_only_the_latest_question_in_the_current_session() {
        let mut session = json!({"status":"error","turns":[
            {"role":"user","content":"第一次的问题"},
            {"role":"assistant","content":"旧的教学提示"},
            {"role":"user","content":"第二次停在哪里？"}
        ]});
        assert_eq!(
            recoverable_input(&session).as_deref(),
            Some("第二次停在哪里？")
        );
        session["status"] = json!("finished");
        assert!(recoverable_input(&session).is_none());
        assert!(recoverable_input(&json!({"status":"error","turns":[]})).is_none());
    }
    #[test]
    fn restored_inventory_is_not_replaced_by_report_only_submission() {
        let mut draft = ModuleDraft::default();
        draft.sync(
            "session-a",
            &json!(["control", "motor", "distance", "light"]),
        );
        assert_eq!(draft.modules.len(), 4);
        draft.report = "第二次已提前停，间隙更大".into();
        let payload = draft.payload("operator_confirmed").unwrap();
        assert!(payload.get("modules").is_none());
        assert_eq!(payload["report"], "第二次已提前停，间隙更大");
    }
    #[test]
    fn failed_save_and_background_poll_keep_edits_until_success() {
        let mut draft = ModuleDraft::default();
        draft.sync("a", &json!(["control", "motor"]));
        draft.set_module("distance", true);
        draft.report = "已接上测距模块，还没试跑".into();
        let attempted = draft.payload("learner_report").unwrap();
        // A failed request makes no acknowledgement; polling must not reset it.
        draft.sync("a", &json!(["control", "motor"]));
        assert_eq!(draft.payload("learner_report"), Some(attempted.clone()));
        draft.saved("a", &attempted);
        assert!(!draft.modules_dirty);
        assert!(draft.report.is_empty());
        assert!(draft.payload("learner_report").is_none());
    }
    #[test]
    fn explicit_empty_inventory_is_distinct_from_unchanged_inventory() {
        let mut draft = ModuleDraft::default();
        draft.sync("a", &json!(["motor"]));
        draft.set_module("motor", false);
        assert_eq!(
            draft.payload("operator_confirmed").unwrap()["modules"],
            json!([])
        );
        draft.sync("a", &json!(["motor"]));
        assert!(draft.modules.is_empty());
    }
    #[test]
    fn a_late_save_never_clears_newer_edits_or_another_learners_draft() {
        let mut draft = ModuleDraft::default();
        draft.sync("a", &Value::Null);
        draft.set_module("motor", true);
        draft.report = "第一次停车太近".into();
        let attempted = draft.payload("learner_report").unwrap();
        draft.set_module("distance", true);
        draft.report = "第二次停车间隙更大".into();
        draft.saved("a", &attempted);
        assert!(draft.modules_dirty);
        assert_eq!(draft.report, "第二次停车间隙更大");
        draft.sync("b", &json!({"value":["light"]}));
        draft.report = "B的新观察".into();
        draft.saved("a", &attempted);
        assert_eq!(draft.modules, vec!["light"]);
        assert!(!draft.modules_dirty);
        assert_eq!(draft.report, "B的新观察");
    }
    #[test]
    fn structured_experiments_and_preferences_show_readable_content() {
        assert_eq!(
            text(
                &json!({"value":[{"report":"移动墙后重测"},"只改停车距离"],"source_type":"learner_report"})
            ),
            "移动墙后重测；只改停车距离"
        );
        assert_eq!(
            preferences(
                &json!({"help_style":"one_step","explicit_request":{"content":"这次请解释原因"}})
            ),
            "一次一步；这次请解释原因"
        );
        assert_eq!(preferences(&json!({})), "使用档案设置");
    }
    #[test]
    fn stale_asr_never_survives_profile_switch_or_return() {
        let mut g = IntentGuard::default();
        g.set_session("A");
        let t = g.capture();
        g.set_session("B");
        assert!(!g.valid(&t));
        g.set_session("A");
        assert!(!g.valid(&t));
    }
    #[test]
    fn stop_invalidates_queued_audio() {
        let mut g = IntentGuard::default();
        g.set_session("A");
        let t = g.capture();
        g.invalidate();
        assert!(!g.valid(&t));
        assert!(g.valid(&g.capture()));
    }
    #[test]
    fn polling_does_not_cancel_live_recording() {
        let mut g = IntentGuard::default();
        g.set_session("A");
        let t = g.capture();
        g.set_session("A");
        assert!(g.valid(&t));
    }
    #[test]
    fn mirror_uses_original_normalized_coordinates() {
        assert_eq!(
            frame_box(&json!([0.1, 0.2, 0.4, 0.6]), false).unwrap(),
            "left:10.0000%;top:20.0000%;width:30.0000%;height:40.0000%"
        );
        let mirrored = frame_box(&json!([0.1, 0.2, 0.4, 0.6]), true).unwrap();
        assert!(mirrored.starts_with("left:60.0000%"));
        assert!(frame_box(&json!([0.4, 0.2, 0.1, 0.6]), false).is_none());
        assert!(frame_box(&json!([-0.1, 0.2, 0.4, 0.6]), false).is_none());
    }
    #[test]
    fn reports_keep_original_wording_and_missing_latency_stays_unknown() {
        assert_eq!(
            text(&json!({"report":"成功停车","hardware_verified":false})),
            "成功停车"
        );
        assert_eq!(milliseconds(&Value::Null), "未知");
    }
    #[test]
    fn backend_short_label_is_used_without_exposing_opaque_uuid() {
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let previous = json!({"track_key":format!("previous:{uuid}"),"label":"Track 1"});
        let current = json!({"track_key":format!("current:{uuid}"),"label":"Track 7"});
        assert_eq!(track_label(&current), "Track 7");
        assert_eq!(
            selected_track_label(&json!([previous, current]), &format!("current:{uuid}")),
            "Track 7"
        );
    }
    #[test]
    fn missing_label_and_lost_selection_never_fall_back_to_uuid() {
        let key = "camera-session:550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(track_label(&json!({"track_key":key})), "临时目标");
        assert_eq!(selected_track_label(&json!([]), key), "已选目标 · 暂时丢失");
        assert_eq!(selected_track_label(&json!([]), ""), "等待教师选择");
    }
}

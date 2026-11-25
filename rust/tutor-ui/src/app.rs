use crate::{
    activity_history_state::{ContinuationIntent, ContinueRequest, HistoricalActivity},
    api::{
        activity_transition_calls, api, calls, continue_activity, ActivityTransition, ApiResult,
        Call, ContinueResult,
    },
    comparison_state::{ComparisonGuard, ComparisonOwner},
    model::{
        list, recoverable_input, retry_unshown_turns, text, AudioTicket, ChatViewport, IntentGuard,
        ModuleDraft, RequestVersion,
    },
    preview_browser::{hidden, VisibilityListener},
    recording_draft::{FinalText, RecordingDraft, RecordingTicket, ReviewKind, SubmittedDraft},
    speech_intent::{SpeechIntent, SpeechKey},
    voice_loop::{
        Device as LoopDevice, LoopKey, Owner as LoopOwner, Phase as LoopPhase, VoiceLoop,
    },
};
use gloo_timers::callback::Timeout;
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
};
use yew::prelude::*;

#[derive(Clone)]
pub enum Field {
    Input,
    Camera,
    Microphone,
    LoopMicrophone,
    Name,
    Namespace,
    ModuleSource,
    Report,
    Language,
    UiLanguage,
    ReadingLevel,
    SpeechSpeed,
    HelpStyle,
    EditMemory,
}
#[derive(Clone)]
pub enum Action {
    Camera(bool),
    Track(String),
    ClearTrack,
    Send,
    VoiceSend,
    RecoverInput,
    Switch(String),
    Create,
    Modules,
    ConfirmInventory,
    NewActivity,
    ContinueActivity(HistoricalActivity),
    Finish,
    Settings(Value),
    UiSettings,
    Read(String),
    SpeechStop,
    Stop,
    SaveMemory,
    ConfirmDelete,
    Discover,
    PtzEnable(Value),
    PtzStop,
}
impl Action {
    fn permitted_while_busy(&self) -> bool {
        matches!(
            self,
            Self::Stop | Self::SpeechStop | Self::PtzStop | Self::Discover
        )
    }
}
pub enum Msg {
    FetchState(bool),
    StateResult(u64, ApiResult),
    Discover,
    Devices(bool, ApiResult),
    LoadMemories,
    Memories(u64, String, ApiResult),
    Field(Field, String),
    ToggleSpeak,
    PageVisibility(bool),
    CloseSegmentSpeech,
    SpeechClosed(u64, SpeechKey, ApiResult),
    ToggleTeacher,
    OpenModuleConfirmation,
    FocusTodayPlan,
    Module(String, bool),
    FormDone(u64, AudioTicket, String, Option<Value>, ApiResult),
    Action(Action),
    Done(
        u64,
        String,
        Option<AudioTicket>,
        Option<SubmittedDraft>,
        bool,
        ApiResult,
    ),
    ClearError,
    ConversationScrolled(i32, i32, i32),
    ShowLatest,
    OpenHistory,
    CloseHistory,
    ViewActivity(HistoricalActivity),
    RetryContinuation,
    ContinuationDone(u64, ContinueRequest, ContinueResult),
    VoiceBegin,
    VoiceEnabled(String, ApiResult),
    VoiceHeartbeat,
    VoiceHeartbeatResult(LoopKey, ApiResult),
    VoiceDrive,
    VoicePause,
    ToggleBargeIn,
    VoiceRecordStart,
    VoiceAudioStatus(RecordingTicket, ApiResult),
    RecordStart,
    RecordStarted(AudioTicket, ApiResult),
    RecordEnd,
    RecordTimeout(AudioTicket, RecordingTicket),
    Transcript(AudioTicket, RecordingTicket, ApiResult),
    AdoptRecordingText(ReviewKind, bool),
    DismissRecordingText(ReviewKind),
    ClearInputDraft,
    Delivered(AudioTicket, String, u64, u64, String, ApiResult),
    StreamProgress(String, String),
    StreamSync(String, String),
    ComparisonChanged(ComparisonGuard),
    ComparisonSettled(String, String),
    Edit(Value),
    Delete(Value),
    CancelDelete,
    CancelEdit,
}

pub struct App {
    pub data: Value,
    pub cameras: Vec<Value>,
    pub microphones: Vec<Value>,
    pub memories: Vec<Value>,
    pub online: bool,
    pub error: String,
    pub busy: String,
    pub input: String,
    pub camera_id: String,
    pub microphone_id: String,
    pub auto_speak: bool,
    pub teacher: bool,
    pub name: String,
    pub namespace: String,
    pub inventory: ModuleDraft,
    pub module_source: String,
    pub report_notice: String,
    pub language: String,
    pub ui_language: String,
    pub reading_level: String,
    pub speech_speed: String,
    pub help_style: String,
    pub edit_memory: Option<String>,
    pub edit_text: String,
    pub delete_target: Option<Value>,
    pub recording: bool,
    pub record_start_pending: bool,
    pub transcribing: bool,
    pub conversation: NodeRef,
    pub composer_input: NodeRef,
    pub module_confirmation: NodeRef,
    pub historical_records: NodeRef,
    focus_request: Option<FocusTarget>,
    pub chat_viewport: ChatViewport,
    pub history_before: Option<i64>,
    pub historical_activity: Option<HistoricalActivity>,
    pub continuation: ContinuationIntent,
    continuation_seq: Option<u64>,
    pub comparison_focus: u64,
    pub recording_draft: RecordingDraft,
    pub voice_loop: VoiceLoop,
    pub loop_microphone: Option<LoopDevice>,
    pub barge_in_requested: bool,
    loop_timer: Option<Timeout>,
    loop_enable_pending: Option<String>,
    loop_heartbeat_pending: Option<LoopKey>,
    loop_start_ticket: Option<AudioTicket>,
    loop_submit_seq: Option<u64>,
    comparison_guard: Option<ComparisonGuard>,
    comparison_refresh_after: Option<u64>,
    history_scroll: Option<i32>,
    guard: Rc<RefCell<IntentGuard>>,
    record_ticket: Option<AudioTicket>,
    release_pending: bool,
    state_seq: u64,
    state_loading: bool,
    action_seq: u64,
    submission_pending: Option<u64>,
    refresh_after_action: Option<u64>,
    memory_requests: RequestVersion,
    state_timer: Option<Timeout>,
    record_timer: Option<Timeout>,
    shown: VecDeque<String>,
    delivery_pending: HashSet<String>,
    delivery_attempts: HashMap<String, u8>,
    delivery_retry_blocked: HashSet<String>,
    pending_read: Option<(AudioTicket, u64, String)>,
    stream_blocked_request: Option<String>,
    speech_intent: SpeechIntent,
    speech_close_pending: Option<(u64, SpeechKey)>,
    speech_close_seq: u64,
    _visibility: Option<VisibilityListener>,
}

enum FocusTarget {
    Modules,
    TodayPlan,
    HistoricalRecords,
}

impl App {
    fn loop_owner(&self) -> LoopOwner {
        LoopOwner {
            session: self.session_id(),
            learner: text(&self.data["session"]["learner_id"]),
            language: text(&self.learner()["language"]),
        }
    }
    fn loop_device(&self) -> Option<LoopDevice> {
        let selected = self.loop_microphone.as_ref()?;
        if self.data["audio"]["input_topology_revision"].as_u64() != Some(selected.revision) {
            return None;
        }
        self.microphones
            .iter()
            .filter_map(LoopDevice::parse)
            .find(|device| device == selected)
    }
    pub fn voice_loop_available(&self) -> bool {
        self.data["voice_loop"]["available"] == true && self.segment_speech_enabled()
    }
    pub fn barge_in_available(&self) -> bool {
        self.data["voice_loop"]["barge_in_available"] == true && self.speech_speed == "normal"
    }
    pub fn can_begin_voice_loop(&self) -> bool {
        self.voice_loop_available()
            && (!self.barge_in_requested || self.barge_in_available())
            && !self.voice_loop.active()
            && self.loop_device().is_some()
            && self.online
            && self.has_session()
            && self.data["session"]["status"] != "finished"
            && self.auto_speak
            && self.busy.is_empty()
            && !self.activity_transition_pending()
            && !self.comparison_saving()
            && !self.recording
            && !self.record_start_pending
            && !self.transcribing
            && !self.sending_request()
            && !self.recording_draft.blocks_navigation(&self.input)
            && self.loop_enable_pending.is_none()
            && self.loop_heartbeat_pending.is_none()
            && self.chat_viewport.stream_following()
            && !hidden()
    }

    fn observe_barge_in_adoption(
        &mut self,
        ctx: &Context<Self>,
        key: &LoopKey,
        value: &Value,
    ) -> bool {
        let mut next = self.voice_loop.clone();
        let binding = match next.adopt_barge_in(key, value) {
            Ok(Some(binding)) => binding,
            Ok(None) => return true,
            Err(()) => return false,
        };
        if hidden()
            || !self.auto_speak
            || !self.chat_viewport.stream_following()
            || self.recording
            || self.record_start_pending
            || self.transcribing
            || self.activity_transition_pending()
            || self.comparison_saving()
            || self.recording_draft.blocks_navigation(&self.input)
            || self.session_id() != binding.session_id
        {
            return false;
        }
        self.recording_draft.observe(
            &binding.session_id,
            binding.session_generation,
            &binding.language,
        );
        self.guard.borrow_mut().invalidate();
        let ticket = self.guard.borrow().capture();
        if !self.recording_draft.begin(ticket.clone(), &self.input) {
            return false;
        }
        if !self.recording_draft.bind(&ticket, binding.clone()) {
            self.recording_draft.cancel_recording();
            return false;
        }
        self.stream_blocked_request = value["barge_in_adoption"]["from_request_id"]
            .as_str()
            .map(str::to_owned);
        self.voice_loop = next;
        self.record_ticket = Some(ticket.clone());
        self.recording = true;
        self.record_start_pending = false;
        self.release_pending = false;
        self.loop_start_ticket = None;
        let link = ctx.link().clone();
        self.record_timer = Some(Timeout::new(22_000, move || {
            link.send_message(Msg::RecordTimeout(ticket, binding))
        }));
        ctx.link().send_message(Msg::FetchState(true));
        true
    }
    fn stop_loop_key(key: LoopKey, reason: String) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api(
                &key.path("stop"),
                "POST",
                Some(json!({"loop_epoch":key.epoch,"reason":reason})),
            )
            .await;
        });
    }
    fn pause_voice_loop(&mut self, ctx: &Context<Self>, reason: &str) {
        if !self.voice_loop.active() {
            return;
        }
        let key = self.voice_loop.pause(reason);
        self.loop_timer = None;
        self.close_segment_speech(
            ctx,
            match reason {
                "hidden" => "hidden",
                "history" => "history",
                _ => "user",
            },
        );
        if self.recording || self.record_start_pending {
            let binding = self.recording_draft.active_binding();
            self.guard.borrow_mut().invalidate();
            self.clear_recording();
            if let Some(binding) = binding {
                Self::discard_bound_recording(binding);
            }
        }
        if let Some(key) = key {
            Self::stop_loop_key(key, reason.into());
        }
    }
    fn schedule_voice_heartbeat(&mut self, ctx: &Context<Self>) {
        if self.voice_loop.active() && self.voice_loop.key().is_some() {
            let link = ctx.link().clone();
            self.loop_timer = Some(Timeout::new(1000, move || {
                link.send_message(Msg::VoiceHeartbeat)
            }));
        }
    }
    pub fn session_id(&self) -> String {
        text(&self.data["session"]["session_id"])
    }
    pub fn learner(&self) -> Value {
        let id = &self.data["session"]["learner_id"];
        list(&self.data["learners"])
            .into_iter()
            .find(|l| &l["learner_id"] == id)
            .unwrap_or(Value::Null)
    }
    pub fn simulator(&self) -> bool {
        self.learner()["namespace"] == "simulator"
    }
    pub fn has_session(&self) -> bool {
        !self.session_id().is_empty()
    }
    pub fn comparison_owner(&self) -> ComparisonOwner {
        ComparisonOwner {
            learner: text(&self.data["session"]["learner_id"]),
            session: self.session_id(),
            simulated: self.simulator(),
        }
    }
    pub fn comparison_saving(&self) -> bool {
        self.comparison_refresh_after.is_some()
            || self
                .comparison_guard
                .as_ref()
                .is_some_and(|guard| guard.owner == self.comparison_owner() && guard.saving)
    }
    pub fn caption_ticket(&self) -> Option<RecordingTicket> {
        self.recording_draft
            .active_binding()
            .filter(|_| self.recording || self.transcribing)
    }
    pub fn sending_request(&self) -> bool {
        self.submission_pending.is_some()
    }
    fn set_input(&mut self, value: String) {
        self.recording_draft.edited();
        self.input = value;
    }
    fn expected_generation(&self) -> u64 {
        self.recording_draft.expected_generation(
            &self.session_id(),
            self.data["session"]["generation"].as_u64().unwrap_or(0),
        )
    }
    fn discard_bound_recording(binding: RecordingTicket) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api("/api/audio/record/discard", "POST", Some(binding.payload())).await;
        });
    }
    pub fn segment_speech_enabled(&self) -> bool {
        self.data["tutor_delivery"]["mode"] == "segments"
            && self.data["tutor_delivery"]["segment_speech"] == "after_display"
    }
    pub fn stream_ack_enabled(&self) -> bool {
        SpeechKey::from_head(&self.session_id(), &self.data["session"]["tutor_stream"])
            .is_none_or(|key| self.speech_intent.allows_ack(&key))
    }
    pub fn stream_speech_status(&self) -> Option<String> {
        let key = SpeechKey::from_head(&self.session_id(), &self.data["session"]["tutor_stream"])?;
        self.speech_intent.status(&key, &self.ui_language)
    }
    pub fn stream_speech_close_available(&self) -> bool {
        SpeechKey::from_head(&self.session_id(), &self.data["session"]["tutor_stream"])
            .is_some_and(|key| self.speech_intent.can_close(&key))
    }
    fn drive_speech_close(&mut self, ctx: &Context<Self>) {
        if self.speech_close_pending.is_some() {
            return;
        }
        let Some(job) = self.speech_intent.close_job() else {
            return;
        };
        self.speech_close_seq = self.speech_close_seq.wrapping_add(1);
        let seq = self.speech_close_seq;
        self.speech_close_pending = Some((seq, job.key.clone()));
        ctx.link().send_future(async move {
            let result = api(
                &job.key.close_path(),
                "POST",
                Some(json!({"generation":job.key.generation,"reason":job.reason})),
            )
            .await;
            Msg::SpeechClosed(seq, job.key, result)
        });
    }
    fn close_segment_speech(&mut self, ctx: &Context<Self>, reason: &'static str) {
        self.speech_intent.close(reason);
        self.drive_speech_close(ctx);
    }
    fn sync_speech_intent(&mut self, ctx: &Context<Self>) {
        self.speech_intent
            .observe(&self.session_id(), &self.data["session"]["tutor_stream"]);
        if hidden() {
            self.speech_intent.close("hidden");
        } else if self.chat_viewport.history_open() {
            self.speech_intent.close("history");
        } else if !self.auto_speak {
            self.speech_intent.close("auto_speak_off");
        }
        self.drive_speech_close(ctx);
    }
    pub fn stream_display_enabled(&self) -> bool {
        let head = &self.data["session"]["tutor_stream"];
        self.chat_viewport.stream_following()
            && !self.comparison_saving()
            && !self.activity_transition_pending()
            && head["generation"] == self.data["session"]["generation"]
            && head["request_id"]
                .as_str()
                .is_some_and(|request| self.stream_blocked_request.as_deref() != Some(request))
    }
    fn automatic_speech_enabled(&self) -> bool {
        self.auto_speak
            && !self.comparison_saving()
            && self.data["tutor_delivery"]["automatic_speech"] != false
    }
    fn retry_current_display(&mut self) {
        retry_unshown_turns(
            &self.data["session"]["turns"],
            &self.shown,
            &self.delivery_pending,
            &mut self.delivery_attempts,
            &mut self.delivery_retry_blocked,
        );
    }
    pub fn input_disabled(&self) -> bool {
        !self.has_session()
            || self.data["session"]["status"] == "finished"
            || self.activity_transition_pending()
            || self.comparison_saving()
            || (!self.voice_loop.active()
                && (self.recording || self.record_start_pending || self.transcribing))
    }
    pub fn activity_transition_pending(&self) -> bool {
        self.continuation.request.is_some()
            || matches!(
                self.busy.as_str(),
                "结束活动" | "新建活动" | "切换档案" | "创建档案" | "继续活动"
            )
    }
    fn launch_continuation(&mut self, ctx: &Context<Self>, request: ContinueRequest) {
        self.action_seq = self.action_seq.wrapping_add(1);
        let seq = self.action_seq;
        self.continuation_seq = Some(seq);
        self.refresh_after_action = None;
        self.busy = "继续活动".into();
        self.error.clear();
        ctx.link().send_future(async move {
            let result = continue_activity(&request.path(), request.payload()).await;
            Msg::ContinuationDone(seq, request, result)
        });
    }
    fn invalidate_audio(&mut self) {
        let recording = self.recording_draft.active_binding();
        // The same actions also cancel/replace the current teaching request.
        // Revoke display receipts immediately, before the server call returns.
        self.stream_blocked_request = self.data["session"]["tutor_stream"]["request_id"]
            .as_str()
            .map(str::to_owned);
        self.guard.borrow_mut().invalidate();
        self.clear_recording();
        self.pending_read = None;
        if let Some(binding) = recording {
            Self::discard_bound_recording(binding);
        }
    }
    fn clear_recording(&mut self) {
        self.recording = false;
        self.transcribing = false;
        self.record_timer = None;
        self.record_ticket = None;
        self.recording_draft.cancel_recording();
    }
    fn launch(
        &mut self,
        ctx: &Context<Self>,
        label: &str,
        items: Vec<Call>,
        ticket: Option<AudioTicket>,
        restore: Option<SubmittedDraft>,
        refresh_memory: bool,
    ) {
        self.action_seq += 1;
        self.refresh_after_action = None;
        let seq = self.action_seq;
        self.busy = label.into();
        self.error.clear();
        let label = label.to_owned();
        ctx.link().send_future(async move {
            Msg::Done(
                seq,
                label,
                ticket,
                restore,
                refresh_memory,
                calls(items).await,
            )
        });
    }
    fn form_payload(&self) -> Option<Value> {
        self.inventory.payload(if self.simulator() {
            "simulated"
        } else {
            &self.module_source
        })
    }
    fn append_report_call(&self, items: &mut Vec<Call>, payload: &Option<Value>) {
        if let Some(payload) = payload {
            items.push(Call::post(
                format!(
                    "/api/activities/{}/events",
                    text(&self.data["session"]["activity_id"])
                ),
                payload.clone(),
            ));
        }
    }
    fn launch_form(
        &mut self,
        ctx: &Context<Self>,
        label: &str,
        items: Vec<Call>,
        payload: Option<Value>,
    ) {
        self.action_seq += 1;
        self.refresh_after_action = None;
        let seq = self.action_seq;
        let ticket = self.guard.borrow().capture();
        self.busy = label.into();
        self.error.clear();
        self.report_notice.clear();
        let label = label.to_owned();
        ctx.link().send_future(async move {
            Msg::FormDone(seq, ticket, label, payload, calls(items).await)
        });
    }
    fn perform(&mut self, ctx: &Context<Self>, action: Action) {
        // A later click must not replace the result owner of an in-flight save
        // or profile switch. Stop controls remain available at all times.
        if (!self.busy.is_empty() || self.comparison_saving()) && !action.permitted_while_busy() {
            return;
        }
        if self.continuation.request.is_some() && !action.permitted_while_busy() {
            return;
        }
        if matches!(
            &action,
            Action::Finish
                | Action::NewActivity
                | Action::Switch(_)
                | Action::Create
                | Action::ContinueActivity(_)
        ) && self.sending_request()
        {
            self.error = "请先等待本次发送结果，再切换档案或活动。".into();
            return;
        }
        if matches!(
            &action,
            Action::Finish
                | Action::NewActivity
                | Action::Switch(_)
                | Action::Create
                | Action::ContinueActivity(_)
        ) && self.recording_draft.blocks_navigation(&self.input)
        {
            self.error = "请先处理输入草稿和未采用文字，再切换档案或活动。".into();
            return;
        }
        if matches!(
            &action,
            Action::Finish
                | Action::NewActivity
                | Action::Switch(_)
                | Action::Create
                | Action::ContinueActivity(_)
        ) && self
            .comparison_guard
            .as_ref()
            .is_some_and(|guard| guard.blocks_navigation(&self.comparison_owner()))
        {
            self.comparison_focus = self.comparison_focus.wrapping_add(1);
            self.error = "请先保存或清空比较草稿，再切换档案或活动。".into();
            return;
        }
        if matches!(&action, Action::SaveMemory | Action::ConfirmDelete) {
            self.chat_viewport.set_history(false);
            self.history_scroll = None;
            self.history_before = None;
        }
        if matches!(&action, Action::ContinueActivity(_)) {
            if self.recording || self.record_start_pending || self.transcribing {
                self.error = "请先结束录音并处理转写，再继续过去的活动。".into();
                return;
            }
            if self.form_payload().is_some() {
                self.teacher = true;
                self.error = "请先保存当前模块或观察报告，再继续过去的活动。".into();
                return;
            }
        }
        if matches!(
            &action,
            Action::Send
                | Action::RecoverInput
                | Action::Switch(_)
                | Action::Create
                | Action::Modules
                | Action::ConfirmInventory
                | Action::NewActivity
                | Action::ContinueActivity(_)
                | Action::Finish
                | Action::Settings(_)
                | Action::Read(_)
                | Action::SpeechStop
                | Action::Stop
                | Action::SaveMemory
                | Action::ConfirmDelete
        ) {
            self.pause_voice_loop(ctx, "user");
        }
        let sid = self.session_id();
        let learner = self.learner();
        let lid = text(&learner["learner_id"]);
        let cancel = || Call::post(format!("/api/sessions/{sid}/cancel"), json!({}));
        match action {
            Action::ContinueActivity(source) => {
                if source.learner != lid || !source.finished || sid.is_empty() {
                    return;
                }
                let request = ContinueRequest {
                    source,
                    current_session: sid.clone(),
                    generation: self.expected_generation(),
                    nonce: format!(
                        "continue-{:x}-{:x}",
                        js_sys::Date::now() as u64,
                        (js_sys::Math::random() * u64::MAX as f64) as u64
                    ),
                };
                if !self.continuation.begin(request.clone()) {
                    return;
                }
                self.invalidate_audio();
                self.launch_continuation(ctx, request);
            }
            Action::PtzEnable(payload) => self.launch(
                ctx,
                "启用物理跟随",
                vec![Call::post("/api/ptz/enable", payload)],
                None,
                None,
                false,
            ),
            Action::PtzStop => self.launch(
                ctx,
                "云台停止",
                vec![Call::post("/api/ptz/stop", json!({}))],
                None,
                None,
                false,
            ),
            Action::Discover => ctx.link().send_message(Msg::Discover),
            Action::Camera(open) => {
                self.launch(
                    ctx,
                    if open { "关闭相机" } else { "连接相机" },
                    vec![Call::post(
                        if open {
                            "/api/camera/close"
                        } else {
                            "/api/camera/open"
                        },
                        if self.camera_id.is_empty() {
                            json!({})
                        } else {
                            json!({"device_id":self.camera_id})
                        },
                    )],
                    None,
                    None,
                    false,
                );
            }
            Action::Track(key) => self.launch(
                ctx,
                "选择视觉目标",
                vec![Call::post("/api/tracking/select", json!({"track_key":key}))],
                None,
                None,
                false,
            ),
            Action::ClearTrack => self.launch(
                ctx,
                "清除目标",
                vec![Call::post("/api/tracking/clear", json!({}))],
                None,
                None,
                false,
            ),
            Action::RecoverInput => {
                if self.input.trim().is_empty() && !self.input_disabled() {
                    if let Some(input) = recoverable_input(&self.data["session"]) {
                        self.set_input(input);
                    }
                }
            }
            action @ (Action::Send | Action::VoiceSend) => {
                let automatic = matches!(action, Action::VoiceSend);
                if automatic
                    && (hidden()
                        || !self.auto_speak
                        || !self.online
                        || !self.chat_viewport.stream_following()
                        || !self.segment_speech_enabled())
                {
                    self.pause_voice_loop(ctx, "submission_blocked");
                    return;
                }
                if self.input_disabled()
                    || self.sending_request()
                    || self.input.trim().is_empty()
                    || self
                        .recording_draft
                        .review(ReviewKind::Submission)
                        .is_some()
                {
                    return;
                }
                let expected_generation = self.expected_generation();
                self.invalidate_audio();
                let ticket = self.guard.borrow().capture();
                let input = self.input.trim().to_owned();
                let nonce = format!(
                    "speech-{}-{:016x}",
                    js_sys::Date::now() as u64,
                    (js_sys::Math::random() * u64::MAX as f64) as u64
                );
                let mut payload = json!({"text":input,"auto_speak":self.auto_speak,
                    "client_submission_id":nonce,"expected_generation":expected_generation});
                if automatic {
                    let Some(extra) = self.voice_loop.submission_payload(&nonce) else {
                        return;
                    };
                    for (key, value) in extra.as_object().expect("voice submission object") {
                        payload[key] = value.clone();
                    }
                    payload["auto_speak"] = json!(true);
                }
                if self.data["tutor_delivery"]["mode"] == "segments" {
                    self.speech_intent.begin(&sid, nonce.clone());
                    if hidden() {
                        self.speech_intent.close("hidden");
                    } else if self.chat_viewport.history_open() {
                        self.speech_intent.close("history");
                    }
                }
                self.set_input(String::new());
                let submitted = self.recording_draft.submitted(input.clone());
                self.launch(
                    ctx,
                    "发送中",
                    vec![Call::post(format!("/api/sessions/{sid}/turns"), payload)],
                    Some(ticket),
                    Some(submitted),
                    true,
                );
                self.submission_pending = Some(self.action_seq);
                if automatic {
                    self.loop_submit_seq = Some(self.action_seq);
                }
            }
            Action::Switch(id) => {
                if id.is_empty() {
                    return;
                }
                self.invalidate_audio();
                self.launch(
                    ctx,
                    "切换档案",
                    vec![
                        Call::post("/api/audio/playback/stop", json!({})),
                        Call::post("/api/sessions", json!({"learner_id":id,"resume":true})),
                    ],
                    None,
                    None,
                    true,
                );
            }
            Action::Create => {
                if self.name.trim().is_empty() {
                    return;
                }
                self.invalidate_audio();
                self.action_seq += 1;
                self.refresh_after_action = None;
                let seq = self.action_seq;
                self.busy = "创建档案".into();
                self.error.clear();
                let payload = json!({"name":self.name.trim(),"language":"zh","help_style":"one_step","namespace":self.namespace});
                ctx.link().send_future(async move {
                    let result = async {
                        let learner = api("/api/learners", "POST", Some(payload)).await?;
                        let id = text(&learner["learner_id"]);
                        if id.is_empty() {
                            return Err("服务未返回有效的档案编号".into());
                        }
                        api(
                            "/api/sessions",
                            "POST",
                            Some(json!({"learner_id":id,"resume":true})),
                        )
                        .await
                    }
                    .await;
                    Msg::Done(seq, "创建档案".into(), None, None, true, result)
                });
            }
            Action::ConfirmInventory => {
                self.inventory.modules_dirty = true;
                self.perform(ctx, Action::Modules);
            }
            Action::Modules => {
                if sid.is_empty() || self.data["session"]["status"] == "finished" {
                    return;
                }
                let payload = self.form_payload();
                if payload.is_none() {
                    return;
                }
                self.invalidate_audio();
                let mut items = vec![cancel()];
                self.append_report_call(&mut items, &payload);
                self.launch_form(ctx, "保存活动报告", items, payload);
            }
            Action::NewActivity => {
                if sid.is_empty() {
                    return;
                }
                self.invalidate_audio();
                let finished = self.data["session"]["status"] == "finished";
                let payload = if finished { None } else { self.form_payload() };
                let items = activity_transition_calls(
                    &sid,
                    &text(&self.data["session"]["activity_id"]),
                    &lid,
                    finished,
                    payload.clone(),
                    ActivityTransition::NewActivity,
                );
                self.launch_form(ctx, "新建活动", items, payload);
            }
            Action::Finish => {
                if sid.is_empty() || self.data["session"]["status"] == "finished" {
                    return;
                }
                self.invalidate_audio();
                let payload = self.form_payload();
                let items = activity_transition_calls(
                    &sid,
                    &text(&self.data["session"]["activity_id"]),
                    &lid,
                    false,
                    payload.clone(),
                    ActivityTransition::Finish,
                );
                self.launch_form(ctx, "结束活动", items, payload);
            }
            Action::Settings(payload) => {
                if sid.is_empty() {
                    return;
                }
                if payload.get("speech_speed").is_none() {
                    self.invalidate_audio();
                }
                self.launch(
                    ctx,
                    "更新教学设置",
                    vec![Call::post(format!("/api/sessions/{sid}/settings"), payload)],
                    None,
                    None,
                    false,
                );
            }
            Action::UiSettings => {
                self.launch(
                    ctx,
                    "更新界面语言",
                    vec![Call::post(
                        "/api/ui/preferences",
                        json!({"ui_language":self.ui_language}),
                    )],
                    None,
                    None,
                    false,
                );
            }
            Action::Read(tid) => {
                if sid.is_empty() {
                    return;
                }
                if list(&self.data["session"]["turns"]).iter().any(|turn| {
                    turn["turn_id"] == tid
                        && crate::tutor_stream_state::is_stream_turn(turn)
                        && turn["stream_phase"] != "completed"
                }) {
                    return;
                }
                let ticket = self.guard.borrow().capture();
                self.launch(
                    ctx,
                    "本地播报",
                    vec![Call::post(
                        "/api/audio/playback",
                        json!({"session_id":sid,"turn_id":tid}),
                    )],
                    Some(ticket),
                    None,
                    false,
                );
            }
            Action::SpeechStop => {
                self.close_segment_speech(ctx, "user");
                // A speech-only stop must not orphan a live microphone/ASR ticket.
                if !self.recording && !self.record_start_pending && !self.transcribing {
                    self.guard.borrow_mut().invalidate();
                }
                self.launch(
                    ctx,
                    "停止播报",
                    vec![Call::post("/api/audio/playback/stop", json!({}))],
                    None,
                    None,
                    false,
                );
            }
            Action::Stop => {
                self.close_segment_speech(ctx, "user");
                self.invalidate_audio();
                let mut items = vec![Call::post("/api/audio/playback/stop", json!({}))];
                if !sid.is_empty() {
                    items.push(cancel());
                }
                self.launch(ctx, "停止中", items, None, None, false);
            }
            Action::SaveMemory => {
                if let Some(mid) = self.edit_memory.clone() {
                    if self.edit_text.trim().is_empty() {
                        return;
                    }
                    self.invalidate_audio();
                    self.launch(
                        ctx,
                        "更正记忆",
                        vec![Call::patch(
                            format!("/api/learners/{lid}/memories/{mid}"),
                            json!({"content":self.edit_text.trim()}),
                        )],
                        None,
                        None,
                        true,
                    );
                }
            }
            Action::ConfirmDelete => {
                if let Some(target) = self.delete_target.clone() {
                    self.invalidate_audio();
                    let path = if target["all"] == true {
                        format!("/api/learners/{lid}/data")
                    } else {
                        format!(
                            "/api/learners/{lid}/memories/{}",
                            text(&target["memory_id"])
                        )
                    };
                    self.launch(ctx, "清理记录", vec![Call::delete(path)], None, None, true);
                }
            }
        }
    }
}

impl Component for App {
    type Message = Msg;
    type Properties = ();
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::FetchState(true));
        ctx.link().send_message(Msg::Discover);
        Self {
            data: json!({"learners":[],"session":null,"camera":{},"kimi":{},"audio":{},"metrics":{}}),
            cameras: vec![],
            microphones: vec![],
            memories: vec![],
            online: false,
            error: String::new(),
            busy: String::new(),
            input: String::new(),
            camera_id: String::new(),
            microphone_id: String::new(),
            auto_speak: true,
            teacher: false,
            name: String::new(),
            namespace: "real".into(),
            inventory: ModuleDraft::default(),
            module_source: "operator_confirmed".into(),
            report_notice: String::new(),
            language: "zh".into(),
            ui_language: "zh".into(),
            reading_level: "standard".into(),
            speech_speed: "normal".into(),
            help_style: "one_step".into(),
            edit_memory: None,
            edit_text: String::new(),
            delete_target: None,
            recording: false,
            record_start_pending: false,
            transcribing: false,
            conversation: NodeRef::default(),
            composer_input: NodeRef::default(),
            module_confirmation: NodeRef::default(),
            historical_records: NodeRef::default(),
            focus_request: None,
            chat_viewport: ChatViewport::default(),
            history_before: None,
            historical_activity: None,
            continuation: ContinuationIntent::default(),
            continuation_seq: None,
            comparison_focus: 0,
            recording_draft: RecordingDraft::default(),
            voice_loop: VoiceLoop::default(),
            loop_microphone: None,
            barge_in_requested: false,
            loop_timer: None,
            loop_enable_pending: None,
            loop_heartbeat_pending: None,
            loop_start_ticket: None,
            loop_submit_seq: None,
            comparison_guard: None,
            comparison_refresh_after: None,
            history_scroll: None,
            guard: Rc::new(RefCell::new(IntentGuard::default())),
            record_ticket: None,
            release_pending: false,
            state_seq: 0,
            state_loading: false,
            action_seq: 0,
            submission_pending: None,
            refresh_after_action: None,
            memory_requests: RequestVersion::default(),
            state_timer: None,
            record_timer: None,
            shown: VecDeque::new(),
            delivery_pending: HashSet::new(),
            delivery_attempts: HashMap::new(),
            delivery_retry_blocked: HashSet::new(),
            pending_read: None,
            stream_blocked_request: None,
            speech_intent: SpeechIntent::default(),
            speech_close_pending: None,
            speech_close_seq: 0,
            _visibility: VisibilityListener::new(ctx.link().callback(Msg::PageVisibility)),
        }
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::VoiceBegin => {
                if !self.can_begin_voice_loop() {
                    return false;
                }
                let Some(device) = self.loop_device() else {
                    return false;
                };
                let client_id = format!(
                    "loop-{}-{:016x}",
                    js_sys::Date::now() as u64,
                    (js_sys::Math::random() * u64::MAX as f64) as u64
                );
                let sid = self.session_id();
                let payload = self.voice_loop.begin(
                    self.loop_owner(),
                    device,
                    self.expected_generation(),
                    client_id.clone(),
                    self.barge_in_requested,
                );
                self.loop_enable_pending = Some(client_id.clone());
                ctx.link().send_future(async move {
                    Msg::VoiceEnabled(
                        client_id,
                        api(
                            &format!("/api/sessions/{sid}/voice-loop/enable"),
                            "POST",
                            Some(payload),
                        )
                        .await,
                    )
                });
            }
            Msg::VoiceEnabled(client_id, result) => {
                if self.loop_enable_pending.as_ref() != Some(&client_id) {
                    return false;
                }
                self.loop_enable_pending = None;
                match result {
                    Ok(value) if self.voice_loop.enabled(&client_id, &value) => {
                        if !self.voice_loop.active() {
                            if let Some(key) = self.voice_loop.key() {
                                Self::stop_loop_key(key, "cancelled".into());
                            }
                        } else {
                            self.schedule_voice_heartbeat(ctx);
                            ctx.link().send_message(Msg::VoiceDrive);
                        }
                    }
                    Err(error) => {
                        self.pause_voice_loop(ctx, "enable_failed");
                        self.error = error;
                    }
                    Ok(_) => self.pause_voice_loop(ctx, "enable_failed"),
                }
            }
            Msg::VoicePause => self.pause_voice_loop(ctx, "user"),
            Msg::ToggleBargeIn => {
                if self.voice_loop.active() {
                    return false;
                }
                if self.barge_in_requested {
                    self.barge_in_requested = false;
                } else if self.barge_in_available() {
                    self.barge_in_requested = true;
                }
            }
            Msg::VoiceHeartbeat => {
                self.loop_timer = None;
                if !self.voice_loop.active() || self.loop_heartbeat_pending.is_some() {
                    return false;
                }
                let Some(key) = self.voice_loop.key() else {
                    return false;
                };
                let Some(payload) = self.voice_loop.heartbeat_payload() else {
                    return false;
                };
                self.loop_heartbeat_pending = Some(key.clone());
                ctx.link().send_future(async move {
                    let result = api(&key.path("heartbeat"), "POST", Some(payload)).await;
                    Msg::VoiceHeartbeatResult(key, result)
                });
                return false;
            }
            Msg::VoiceHeartbeatResult(key, result) => {
                if self.loop_heartbeat_pending.as_ref() != Some(&key) {
                    return false;
                }
                self.loop_heartbeat_pending = None;
                if !self.voice_loop.active() || self.voice_loop.key().as_ref() != Some(&key) {
                    return true;
                }
                let previous = self.voice_loop.phase;
                let was_monitoring = self.voice_loop.monitoring;
                match result {
                    Ok(value) => {
                        if !self.observe_barge_in_adoption(ctx, &key, &value)
                            || !self.voice_loop.heartbeat(&key, &value)
                            || value["phase"] == "paused"
                        {
                            self.pause_voice_loop(ctx, "permission_lost");
                        } else {
                            if self.voice_loop.needs_reply_ack() {
                                ctx.link().send_message(Msg::VoiceHeartbeat);
                            } else {
                                self.schedule_voice_heartbeat(ctx);
                            }
                            if self.voice_loop.phase == LoopPhase::Armed {
                                ctx.link().send_message(Msg::VoiceDrive);
                            }
                        }
                    }
                    _ => self.pause_voice_loop(ctx, "permission_lost"),
                }
                return previous != self.voice_loop.phase
                    || was_monitoring != self.voice_loop.monitoring;
            }
            Msg::VoiceDrive => {
                if self.voice_loop.phase != LoopPhase::Armed {
                    return false;
                }
                if hidden()
                    || !self.auto_speak
                    || !self.chat_viewport.stream_following()
                    || self.recording_draft.blocks_navigation(&self.input)
                    || self.comparison_saving()
                {
                    self.pause_voice_loop(ctx, "user");
                } else if self.busy.is_empty()
                    && !self.sending_request()
                    && !self.record_start_pending
                    && !self.recording
                    && !self.transcribing
                {
                    ctx.link().send_message(Msg::VoiceRecordStart);
                }
            }
            Msg::VoiceAudioStatus(binding, result) => {
                if !self.voice_loop.active()
                    || self.recording_draft.active_binding().as_ref() != Some(&binding)
                {
                    return false;
                }
                match result {
                    Ok(value) => {
                        if let Some(reason) = self
                            .voice_loop
                            .endpoint(&binding, &value["recording_endpoint"])
                        {
                            if reason == "speech_end" {
                                // Finish under this already checked ticket, before
                                // another UI event can replace the recording.
                                <Self as Component>::update(self, ctx, Msg::RecordEnd);
                            } else if reason == "duration_limit" {
                                // Preserve the full captured result for explicit review;
                                // a time limit is never a speech-end send permission.
                                <Self as Component>::update(self, ctx, Msg::RecordEnd);
                                self.recording_draft.edited();
                                // Keep the lease only while finishing the captured
                                // result. Pausing the server now would discard ASR.
                                // The unchanged-text gate is already revoked above.
                            } else {
                                self.pause_voice_loop(ctx, &reason);
                            }
                            return true;
                        }
                        if value["recording_generation"].as_u64()
                            != Some(binding.recording_generation)
                        {
                            self.pause_voice_loop(ctx, "recording_changed");
                            return true;
                        }
                    }
                    Err(_) => {
                        self.pause_voice_loop(ctx, "network");
                        return true;
                    }
                }
                return false;
            }
            Msg::FetchState(force) => {
                if self.state_loading && !force {
                    return false;
                }
                self.state_timer = None;
                self.state_loading = true;
                self.state_seq += 1;
                let seq = self.state_seq;
                ctx.link().send_future(async move {
                    Msg::StateResult(seq, api("/api/state", "GET", None).await)
                });
                return false;
            }
            Msg::StateResult(seq, result) => {
                if seq != self.state_seq {
                    return false;
                }
                self.state_loading = false;
                match result {
                    Ok(data) => {
                        if self
                            .comparison_refresh_after
                            .is_some_and(|required| seq >= required)
                        {
                            self.comparison_refresh_after = None;
                        }
                        let old_sid = self.session_id();
                        let old_learner = self.learner();
                        let old_generation = self.data["session"]["generation"].clone();
                        let old_stream = self.data["session"]["tutor_stream"].clone();
                        let had_continuation = self.continuation.request.is_some();
                        self.continuation.observe(&data["session"]);
                        if had_continuation && self.continuation.request.is_none() {
                            self.continuation_seq = None;
                            if self.busy == "继续活动" {
                                self.busy.clear();
                                self.refresh_after_action = None;
                            }
                        }
                        self.data = data;
                        let refreshed_learner = self.learner();
                        if old_learner["learner_id"] == refreshed_learner["learner_id"]
                            && old_learner["epoch"] != refreshed_learner["epoch"]
                        {
                            if let Some(activity) = &mut self.historical_activity {
                                activity.invalidate_cached_title();
                            }
                        }
                        if let Some(key) = self.voice_loop.key() {
                            let echo = self.data["voice_loop"].clone();
                            if !self.observe_barge_in_adoption(ctx, &key, &echo) {
                                self.pause_voice_loop(ctx, "permission_lost");
                            }
                        }
                        self.chat_viewport
                            .observe(&self.session_id(), &self.data["session"]["turns"]);
                        self.sync_speech_intent(ctx);
                        let stream = &self.data["session"]["tutor_stream"];
                        if old_sid == self.session_id()
                            && !self.chat_viewport.stream_following()
                            && stream["request_id"].is_string()
                            && stream["request_id"] == old_stream["request_id"]
                            && stream["revision"].as_u64() > old_stream["revision"].as_u64()
                        {
                            // Hidden/offscreen streaming does not poll locally.
                            // A changed head offers a return entry without scroll.
                            self.chat_viewport.content_changed();
                        }
                        self.delivery_retry_blocked.clear();
                        let current_turns: HashSet<String> = list(&self.data["session"]["turns"])
                            .iter()
                            .map(|turn| text(&turn["turn_id"]))
                            .collect();
                        self.delivery_attempts
                            .retain(|id, _| current_turns.contains(id));
                        self.online = true;
                        let sid = self.session_id();
                        self.guard.borrow_mut().set_session(&sid);
                        let generation = self.data["session"]["generation"].as_u64().unwrap_or(0);
                        let language = text(&self.learner()["language"]);
                        if self
                            .voice_loop
                            .owner_changed(&self.loop_owner(), generation)
                            || (self.voice_loop.active()
                                && (!self.segment_speech_enabled()
                                    || self.data["voice_loop"]["available"] != true))
                        {
                            self.pause_voice_loop(ctx, "owner_changed");
                        }
                        let previous_recording = self.recording_draft.active_binding();
                        if self.recording_draft.observe(&sid, generation, &language) {
                            self.guard.borrow_mut().invalidate();
                            self.clear_recording();
                            if let Some(binding) = previous_recording {
                                Self::discard_bound_recording(binding);
                            }
                        }
                        self.inventory
                            .sync(&sid, &self.data["session"]["state"]["modules"]);
                        if sid != old_sid {
                            self.barge_in_requested = false;
                            self.comparison_guard = None;
                            self.comparison_refresh_after = None;
                            self.memory_requests.advance();
                            self.set_input(String::new());
                            self.report_notice.clear();
                            self.recording = false;
                            self.transcribing = false;
                            self.record_timer = None;
                            self.clear_recording();
                            self.module_source = if self.simulator() {
                                "simulated"
                            } else {
                                "operator_confirmed"
                            }
                            .into();
                            self.memories.clear();
                            self.history_before = None;
                            self.history_scroll = None;
                            self.historical_activity = None;
                            self.edit_memory = None;
                            self.delete_target = None;
                        }
                        if self.busy != "更新教学设置" || sid != old_sid {
                            let learner = self.learner();
                            self.language = if text(&learner["language"]).starts_with("en") {
                                "en"
                            } else {
                                "zh"
                            }
                            .into();
                            self.help_style =
                                learner["help_style"].as_str().unwrap_or("one_step").into();
                            self.reading_level =
                                crate::i18n::reading_level(&learner["reading_level"]).into();
                            self.speech_speed =
                                crate::i18n::speech_speed(&learner["speech_speed"]).into();
                        }
                        if self.busy != "更新界面语言" {
                            self.ui_language = crate::i18n::ui_language(
                                &self.data["ui_preferences"]["ui_language"],
                            )
                            .into();
                        }
                        if sid != old_sid || old_generation != self.data["session"]["generation"] {
                            ctx.link().send_message(Msg::LoadMemories);
                        }
                    }
                    Err(error) => {
                        self.online = false;
                        self.pause_voice_loop(ctx, "network");
                        self.error = error;
                    }
                }
                if self
                    .refresh_after_action
                    .is_some_and(|required| seq >= required)
                {
                    self.busy.clear();
                    self.refresh_after_action = None;
                    if let Some((ticket, generation, tid)) = self.pending_read.take() {
                        if self.automatic_speech_enabled()
                            && !self.chat_viewport.history_open()
                            && self.online
                            && self.guard.borrow().valid(&ticket)
                            && self.data["session"]["generation"].as_u64() == Some(generation)
                        {
                            self.perform(ctx, Action::Read(tid));
                        }
                    }
                }
                let link = ctx.link().clone();
                self.state_timer = Some(Timeout::new(1000, move || {
                    link.send_message(Msg::FetchState(false))
                }));
            }
            Msg::Discover => {
                ctx.link().send_future(async {
                    Msg::Devices(true, api("/api/devices/cameras", "GET", None).await)
                });
                ctx.link().send_future(async {
                    Msg::Devices(false, api("/api/audio/devices", "GET", None).await)
                });
                return false;
            }
            Msg::Devices(camera, result) => match result {
                Ok(value) => {
                    let values = if value.is_array() {
                        list(&value)
                    } else {
                        list(&value["devices"])
                    };
                    if camera {
                        self.cameras = values;
                    } else {
                        self.microphones = values;
                    }
                }
                Err(error) => self.error = error,
            },
            Msg::LoadMemories => {
                let seq = self.memory_requests.advance();
                let id = text(&self.learner()["learner_id"]);
                if id.is_empty() {
                    self.memories.clear();
                } else {
                    ctx.link().send_future(async move {
                        let result =
                            api(&format!("/api/learners/{id}/memories"), "GET", None).await;
                        Msg::Memories(seq, id, result)
                    });
                }
                return false;
            }
            Msg::Memories(seq, id, result) => {
                if !self.memory_requests.current(seq) || id != text(&self.learner()["learner_id"]) {
                    return false;
                }
                match result {
                    Ok(value) => self.memories = list(&value),
                    Err(error) => self.error = error,
                }
            }
            Msg::Field(field, value) => match field {
                Field::Input => {
                    self.pause_voice_loop(ctx, "edited");
                    self.set_input(value);
                }
                Field::LoopMicrophone => {
                    self.pause_voice_loop(ctx, "device_changed");
                    self.loop_microphone = self
                        .microphones
                        .iter()
                        .filter_map(LoopDevice::parse)
                        .find(|device| device.key == value);
                }
                Field::Camera => self.camera_id = value,
                Field::Microphone => {
                    self.pause_voice_loop(ctx, "device_changed");
                    self.microphone_id = value;
                }
                Field::Name => self.name = value,
                Field::Namespace => self.namespace = value,
                Field::ModuleSource => self.module_source = value,
                Field::Report => {
                    self.inventory.report = value;
                    self.report_notice.clear();
                }
                Field::EditMemory => self.edit_text = value,
                Field::UiLanguage => {
                    if !self.busy.is_empty() {
                        return false;
                    }
                    self.ui_language = value;
                    self.perform(ctx, Action::UiSettings);
                }
                Field::ReadingLevel => {
                    if !self.busy.is_empty() {
                        return false;
                    }
                    self.reading_level = value;
                    self.perform(
                        ctx,
                        Action::Settings(json!({"reading_level":self.reading_level})),
                    );
                }
                Field::SpeechSpeed => {
                    if !self.busy.is_empty() {
                        return false;
                    }
                    self.speech_speed = value;
                    self.perform(
                        ctx,
                        Action::Settings(json!({"speech_speed":self.speech_speed})),
                    );
                }
                Field::Language => {
                    if !self.busy.is_empty() {
                        return false;
                    }
                    self.language = value;
                    self.perform(ctx, Action::Settings(json!({"language":self.language})));
                }
                Field::HelpStyle => {
                    if !self.busy.is_empty() {
                        return false;
                    }
                    self.help_style = value;
                    self.perform(ctx, Action::Settings(json!({"help_style":self.help_style})));
                }
            },
            Msg::ToggleSpeak => {
                self.auto_speak = !self.auto_speak;
                if !self.auto_speak {
                    self.pause_voice_loop(ctx, "auto_speak_off");
                    self.pending_read = None;
                    self.close_segment_speech(ctx, "auto_speak_off");
                }
                // Re-enabling is a preference for the next submit only.
            }
            Msg::PageVisibility(page_hidden) => {
                if page_hidden {
                    self.pause_voice_loop(ctx, "hidden");
                    self.close_segment_speech(ctx, "hidden");
                    if self.recording || self.transcribing || self.record_start_pending {
                        let binding = self.recording_draft.active_binding();
                        self.guard.borrow_mut().invalidate();
                        self.clear_recording();
                        if let Some(binding) = binding {
                            Self::discard_bound_recording(binding);
                        }
                    }
                } else {
                    self.drive_speech_close(ctx);
                    ctx.link().send_message(Msg::FetchState(true));
                }
            }
            Msg::CloseSegmentSpeech => {
                self.pause_voice_loop(ctx, "user");
                self.close_segment_speech(ctx, "user");
            }
            Msg::SpeechClosed(seq, key, result) => {
                if self.speech_close_pending.as_ref() != Some(&(seq, key.clone())) {
                    return false;
                }
                self.speech_close_pending = None;
                self.speech_intent.closed(&key, result.as_ref().ok());
                // Retry the same failed close on the ordinary state poll, not
                // a tight error loop. A newer owner can use the released slot.
                if self
                    .speech_intent
                    .close_job()
                    .is_some_and(|job| job.key != key)
                {
                    self.drive_speech_close(ctx);
                }
            }
            Msg::ToggleTeacher => self.teacher = !self.teacher,
            Msg::OpenModuleConfirmation => {
                self.teacher = true;
                self.focus_request = Some(FocusTarget::Modules);
            }
            Msg::FocusTodayPlan => {
                self.focus_request = Some(FocusTarget::TodayPlan);
            }
            Msg::Module(id, checked) => {
                self.inventory.set_module(&id, checked);
                self.report_notice.clear();
            }
            Msg::FormDone(seq, ticket, label, payload, result) => {
                if seq == self.action_seq {
                    self.refresh_after_action = Some(self.state_seq + 1);
                }
                ctx.link().send_message(Msg::FetchState(true));
                ctx.link().send_message(Msg::LoadMemories);
                if !self.guard.borrow().valid(&ticket) {
                    return false;
                }
                match result {
                    Ok(_) => {
                        if let Some(payload) = payload {
                            self.inventory.saved(&ticket.session_id, &payload);
                            self.report_notice = if payload.get("modules").is_some() {
                                "模块确认已保存；观察报告按填写内容记录。"
                            } else {
                                "本次观察已保存，当前模块保持原有报告。"
                            }
                            .into();
                        }
                    }
                    Err(error) => self.error = format!("{label}未完成，草稿已保留：{error}"),
                }
            }
            Msg::Action(action) => self.perform(ctx, action),
            Msg::RetryContinuation => {
                if self.recording_draft.blocks_navigation(&self.input)
                    || self.form_payload().is_some()
                    || self
                        .comparison_guard
                        .as_ref()
                        .is_some_and(|guard| guard.blocks_navigation(&self.comparison_owner()))
                {
                    self.error = "请先处理输入草稿和未采用文字，再切换档案或活动。".into();
                    return true;
                }
                let Some(request) = self.continuation.retry() else {
                    return false;
                };
                self.launch_continuation(ctx, request);
            }
            Msg::ContinuationDone(seq, request, result) => {
                if self.continuation_seq != Some(seq)
                    || self.continuation.request.as_ref() != Some(&request)
                {
                    return false;
                }
                self.continuation_seq = None;
                match result {
                    ContinueResult::Applied(value)
                        if value["learner_id"] == request.source.learner
                            && value["client_submission_id"] == request.nonce
                            && value["session_id"] == value["session"]["session_id"]
                            && value["activity_id"] == value["session"]["activity_id"]
                            && request.recognizes(&value["session"]) =>
                    {
                        self.continuation.clear(&request);
                    }
                    ContinueResult::Rejected(_detail) => {
                        self.continuation.clear(&request);
                        self.error = "请求未被接受，请核对当前活动。".into();
                    }
                    ContinueResult::Unknown(error) => {
                        self.continuation.unknown(&request);
                        self.error = error;
                    }
                    ContinueResult::Applied(_) => {
                        self.continuation.unknown(&request);
                    }
                }
                if self.action_seq == seq {
                    self.refresh_after_action = Some(self.state_seq.wrapping_add(1));
                }
                ctx.link().send_message(Msg::FetchState(true));
            }
            Msg::Done(seq, label, ticket, restore, refresh_memory, mut result) => {
                if label == "发送中" {
                    if self.loop_submit_seq == Some(seq) {
                        self.loop_submit_seq = None;
                        if self.voice_loop.active()
                            && !result
                                .as_ref()
                                .is_ok_and(|value| self.voice_loop.submitted(value))
                        {
                            self.pause_voice_loop(ctx, "submission_unknown");
                            if result.is_ok() {
                                result =
                                    Err("发送回执无法与本轮核对，请检查对话记录后再决定是否发送。"
                                        .into());
                            }
                        }
                    }
                    if self.submission_pending == Some(seq) {
                        self.submission_pending = None;
                    }
                    if self.voice_loop.needs_reply_ack() {
                        ctx.link().send_message(Msg::VoiceHeartbeat);
                    }
                    // Stop can replace the busy action without revoking the
                    // user's submitted text. Owner revision still rejects ABA.
                    if result.is_err() {
                        if let Some(submitted) = restore {
                            self.recording_draft
                                .failed_submission(submitted, &mut self.input);
                        }
                    }
                    if let (Some(ticket), Ok(value)) = (&ticket, &result) {
                        self.speech_intent.submitted(&ticket.session_id, value);
                        self.drive_speech_close(ctx);
                    }
                }
                if seq != self.action_seq {
                    return label == "发送中";
                }
                self.refresh_after_action = Some(self.state_seq + 1);
                let current = ticket
                    .as_ref()
                    .map(|t| self.guard.borrow().valid(t))
                    .unwrap_or(true);
                if current {
                    match result {
                        Ok(value) => {
                            if label == "创建档案" {
                                self.name.clear();
                            }
                            if label == "更正记忆" {
                                self.edit_memory = None;
                            }
                            if label == "清理记录" {
                                self.delete_target = None;
                            }
                            if let Some(message) = value["local_action"].as_str() {
                                self.error = message.into();
                            }
                        }
                        Err(error) => {
                            self.error = error;
                        }
                    }
                }
                ctx.link().send_message(Msg::FetchState(true));
                if refresh_memory {
                    ctx.link().send_message(Msg::LoadMemories);
                }
            }
            Msg::ClearError => self.error.clear(),
            Msg::AdoptRecordingText(kind, append) => {
                self.pause_voice_loop(ctx, "edited");
                if self.input_disabled() || !self.busy.is_empty() {
                    return false;
                }
                match self.recording_draft.adopt(kind, append, &self.input) {
                    Ok(Some(text)) => self.set_input(text),
                    Ok(None) => return false,
                    Err(()) => {
                        self.error = "合并后超过4000字，请先精简草稿；未采用文字仍已保留。".into()
                    }
                }
            }
            Msg::DismissRecordingText(kind) => {
                self.recording_draft.dismiss(kind);
            }
            Msg::ClearInputDraft => {
                self.pause_voice_loop(ctx, "edited");
                self.set_input(String::new());
            }
            Msg::ComparisonChanged(guard) => {
                if guard.owner != self.comparison_owner() {
                    return false;
                }
                if guard.saving {
                    self.pause_voice_loop(ctx, "user");
                }
                if guard.saving
                    && !self
                        .comparison_guard
                        .as_ref()
                        .is_some_and(|previous| previous.saving)
                {
                    self.guard.borrow_mut().invalidate();
                    self.pending_read = None;
                }
                self.comparison_guard = Some(guard);
            }
            Msg::ComparisonSettled(learner, session) => {
                if session != self.session_id() || self.data["session"]["learner_id"] != learner {
                    return false;
                }
                // A failed transport may have saved server-side. Keep teaching
                // receipts gated until the subsequent current state is known.
                self.comparison_refresh_after = Some(self.state_seq.wrapping_add(1));
                ctx.link().send_message(Msg::FetchState(true));
            }
            Msg::StreamProgress(sid, tid) => {
                if sid != self.session_id()
                    || self.data["session"]["tutor_stream"]["turn_id"] != tid
                {
                    return false;
                }
                self.chat_viewport.content_changed();
            }
            Msg::StreamSync(sid, request) => {
                if sid == self.session_id()
                    && self.data["session"]["tutor_stream"]["request_id"] == request
                {
                    ctx.link().send_message(Msg::FetchState(true));
                }
                return false;
            }
            Msg::ConversationScrolled(top, height, client) => {
                let changed = self.chat_viewport.scrolled(top, height, client);
                if self.voice_loop.active() && !self.chat_viewport.stream_following() {
                    self.pause_voice_loop(ctx, "history");
                    return true;
                }
                return changed;
            }
            Msg::ShowLatest => {
                self.history_scroll = None;
                self.retry_current_display();
                self.chat_viewport.show_latest();
            }
            Msg::OpenHistory => {
                if !self.busy.is_empty() || self.chat_viewport.history_open() {
                    return false;
                }
                let Some(before) = self.data["session"]["history_before"].as_i64() else {
                    return false;
                };
                self.history_before = Some(before);
                self.historical_activity = None;
                self.history_scroll = self
                    .conversation
                    .cast::<web_sys::HtmlElement>()
                    .map(|node| node.scroll_top());
                self.chat_viewport.set_history(true);
                self.close_segment_speech(ctx, "history");
                self.pending_read = None;
                // Stop audio only; retain the activity, composer and any active
                // recording ticket. Hidden new replies remain undelivered.
                self.perform(ctx, Action::SpeechStop);
            }
            Msg::ViewActivity(activity) => {
                if activity.learner != text(&self.learner()["learner_id"]) || !self.busy.is_empty()
                {
                    return false;
                }
                self.history_before = None;
                self.historical_activity = Some(activity);
                self.history_scroll = self
                    .conversation
                    .cast::<web_sys::HtmlElement>()
                    .map(|node| node.scroll_top());
                self.chat_viewport.set_history(true);
                self.close_segment_speech(ctx, "history");
                self.pending_read = None;
                self.perform(ctx, Action::SpeechStop);
                self.focus_request = Some(FocusTarget::HistoricalRecords);
            }
            Msg::CloseHistory => {
                self.retry_current_display();
                self.chat_viewport.set_history(false);
                self.historical_activity = None;
            }
            msg @ (Msg::RecordStart | Msg::VoiceRecordStart) => {
                let automatic = matches!(msg, Msg::VoiceRecordStart);
                if !automatic {
                    self.pause_voice_loop(ctx, "user");
                }
                if !self.has_session()
                    || !self.busy.is_empty()
                    || self.comparison_saving()
                    || self.activity_transition_pending()
                    || self.record_start_pending
                    || self.recording
                    || self.transcribing
                    || hidden()
                    || self
                        .recording_draft
                        .review(ReviewKind::Transcript)
                        .is_some()
                    || self.data["session"]["status"] == "finished"
                {
                    return false;
                }
                let automatic_payload = if automatic {
                    let Some(payload) = self.voice_loop.start_payload() else {
                        return false;
                    };
                    Some(payload)
                } else {
                    None
                };
                self.guard.borrow_mut().invalidate();
                let ticket = self.guard.borrow().capture();
                if !self.recording_draft.begin(ticket.clone(), &self.input) {
                    if automatic {
                        self.pause_voice_loop(ctx, "edited");
                    }
                    return false;
                }
                if automatic {
                    self.loop_start_ticket = Some(ticket.clone());
                }
                self.record_ticket = Some(ticket.clone());
                self.record_start_pending = true;
                self.release_pending = false;
                self.error.clear();
                let guard = self.guard.clone();
                let microphone = self.microphone_id.parse::<u32>().ok();
                let expected_generation = self.expected_generation();
                ctx.link().send_future(async move {
                    let result = async {
                        if !automatic { api("/api/audio/playback/stop", "POST", Some(json!({}))).await?; }
                        if !guard.borrow().valid(&ticket) {
                            return Err(String::new());
                        }
                        let mut payload = automatic_payload.unwrap_or_else(|| json!({"session_id":ticket.session_id,"expected_generation":expected_generation}));
                        if let Some(id) = microphone.filter(|_| !automatic) {
                            payload["device_id"] = json!(id);
                        }
                        api("/api/audio/record/start", "POST", Some(payload)).await
                    }
                    .await;
                    Msg::RecordStarted(ticket, result)
                });
            }
            Msg::RecordStarted(ticket, result) => {
                let automatic = self.loop_start_ticket.as_ref() == Some(&ticket);
                if automatic {
                    self.loop_start_ticket = None;
                }
                self.record_start_pending = false;
                if !self.guard.borrow().valid(&ticket)
                    || !self.recording_draft.is_start(&ticket)
                    || hidden()
                {
                    if let Ok(value) = result {
                        if let Some(binding) = RecordingTicket::parse(&value) {
                            Self::discard_bound_recording(binding);
                        }
                    }
                    if self.recording_draft.is_start(&ticket) {
                        self.clear_recording();
                    }
                    if automatic {
                        self.pause_voice_loop(ctx, "start_failed");
                    }
                    return true;
                }
                match result {
                    Ok(value) => {
                        let Some(binding) = RecordingTicket::parse(&value) else {
                            self.error = "录音响应缺少有效编号，请停止后重新录音。".into();
                            self.pause_voice_loop(ctx, "start_failed");
                            self.clear_recording();
                            return true;
                        };
                        if !self.recording_draft.bind(&ticket, binding.clone()) {
                            Self::discard_bound_recording(binding);
                            self.clear_recording();
                            self.error = "录音请求已过期，请重新录音。".into();
                            self.pause_voice_loop(ctx, "start_failed");
                            return true;
                        }
                        if automatic
                            && (!self.voice_loop.start_echo_matches(&value)
                                || !self.voice_loop.started(&binding))
                        {
                            Self::discard_bound_recording(binding);
                            self.clear_recording();
                            self.pause_voice_loop(ctx, "start_failed");
                            return true;
                        }
                        self.recording = true;
                        let link = ctx.link().clone();
                        self.record_timer = Some(Timeout::new(
                            if automatic { 22_000 } else { 20_000 },
                            move || link.send_message(Msg::RecordTimeout(ticket, binding)),
                        ));
                        ctx.link().send_message(Msg::FetchState(true));
                        if self.release_pending {
                            ctx.link().send_message(Msg::RecordEnd);
                        }
                    }
                    Err(error) => {
                        self.clear_recording();
                        if !error.is_empty() {
                            self.error = error;
                        }
                    }
                }
                if automatic && self.voice_loop.phase == LoopPhase::Starting {
                    self.pause_voice_loop(ctx, "start_failed");
                }
            }
            Msg::RecordTimeout(ticket, binding) => {
                if self.voice_loop.active()
                    && self.guard.borrow().valid(&ticket)
                    && self.recording_draft.matches(&ticket, &binding)
                {
                    // The owner publishes the authoritative terminal after dropping
                    // its input. A UI timer alone must never authorize auto-send.
                    self.pause_voice_loop(ctx, "duration_limit");
                    return true;
                }
                if self.guard.borrow().valid(&ticket)
                    && self.recording_draft.matches(&ticket, &binding)
                {
                    return <Self as Component>::update(self, ctx, Msg::RecordEnd);
                }
                return false;
            }
            Msg::RecordEnd => {
                if self.voice_loop.active() && self.voice_loop.phase != LoopPhase::Finalizing {
                    self.pause_voice_loop(ctx, "user");
                    return true;
                }
                if !self.recording {
                    if self.record_start_pending {
                        self.release_pending = true;
                    }
                    return false;
                }
                let Some(ticket) = self.record_ticket.clone() else {
                    return false;
                };
                let Some(binding) = self.recording_draft.active_binding() else {
                    return false;
                };
                if !self.guard.borrow().valid(&ticket) {
                    Self::discard_bound_recording(binding);
                    self.clear_recording();
                    return true;
                }
                self.recording = false;
                self.transcribing = true;
                self.record_timer = None;
                ctx.link().send_future(async move {
                    let payload = binding.payload();
                    Msg::Transcript(
                        ticket,
                        binding,
                        api("/api/audio/record/stop", "POST", Some(payload)).await,
                    )
                });
            }
            Msg::Transcript(ticket, binding, result) => {
                if !self.guard.borrow().valid(&ticket)
                    || !self.recording_draft.matches(&ticket, &binding)
                {
                    return false;
                }
                self.transcribing = false;
                match result {
                    Ok(value) => {
                        if RecordingTicket::parse(&value).as_ref() != Some(&binding) {
                            self.error = "录音请求已过期，请重新录音。".into();
                            self.clear_recording();
                        } else if let Some(error) = value["error"].as_str() {
                            if !error.is_empty() {
                                self.error = error.into();
                            }
                            self.clear_recording();
                        } else if let Some(text) =
                            value["text"].as_str().filter(|v| !v.trim().is_empty())
                        {
                            let final_text = self.recording_draft.finish(
                                &ticket,
                                &binding,
                                text.into(),
                                &self.input,
                            );
                            self.record_ticket = None;
                            if let FinalText::Fill(text) = final_text {
                                self.set_input(text);
                                if self.voice_loop.final_ready(&binding) {
                                    self.perform(ctx, Action::VoiceSend);
                                    if self.voice_loop.phase != LoopPhase::Submitting {
                                        self.pause_voice_loop(ctx, "submission_blocked");
                                    }
                                }
                            } else {
                                self.pause_voice_loop(ctx, "edited");
                            }
                        } else {
                            self.error = "没有识别到清楚的语音，请重录或输入文字。".into();
                            self.clear_recording();
                        }
                    }
                    Err(error) => {
                        self.error = error;
                        self.clear_recording();
                    }
                }
                if self.voice_loop.phase == LoopPhase::Finalizing {
                    self.pause_voice_loop(ctx, "transcript_unavailable");
                }
            }
            Msg::Delivered(ticket, sid, generation, display_revision, tid, result) => {
                self.delivery_pending.remove(&tid);
                match result {
                    Ok(_) => {
                        self.shown.push_back(tid.clone());
                        while self.shown.len() > 256 {
                            self.shown.pop_front();
                        }
                        if self.automatic_speech_enabled()
                            && self.chat_viewport.automatic_read_current(display_revision)
                            && self.guard.borrow().valid(&ticket)
                            && sid == self.session_id()
                            && self.data["session"]["generation"].as_u64() == Some(generation)
                        {
                            if self.busy.is_empty() {
                                self.perform(ctx, Action::Read(tid));
                            } else {
                                self.pending_read = Some((ticket, generation, tid));
                            }
                        }
                    }
                    Err(error) => {
                        self.delivery_retry_blocked.insert(tid.clone());
                        if self.guard.borrow().valid(&ticket) {
                            self.error = error;
                        }
                    }
                }
            }
            Msg::Edit(memory) => {
                self.edit_memory = Some(text(&memory["memory_id"]));
                self.edit_text = text(&memory["content"]);
            }
            Msg::Delete(memory) => self.delete_target = Some(memory),
            Msg::CancelDelete => self.delete_target = None,
            Msg::CancelEdit => self.edit_memory = None,
        }
        true
    }
    fn rendered(&mut self, ctx: &Context<Self>, _first: bool) {
        // These explicit navigation requests reveal existing controls only;
        // they do not change drafts, current facts, devices or lesson requests.
        if let Some(target) = self.focus_request.take() {
            let reference = match target {
                FocusTarget::Modules => &self.module_confirmation,
                FocusTarget::TodayPlan => &self.composer_input,
                FocusTarget::HistoricalRecords => &self.historical_records,
            };
            if let Some(node) = reference.cast::<web_sys::HtmlElement>() {
                node.scroll_into_view();
                let _ = node.focus();
            }
        }
        if self.chat_viewport.history_open()
            || self.comparison_saving()
            || self.activity_transition_pending()
        {
            return;
        }
        if let Some(top) = self.history_scroll.take() {
            if let Some(conversation) = self.conversation.cast::<web_sys::HtmlElement>() {
                conversation.set_scroll_top(top);
            }
        }
        let sid = self.session_id();
        let generation = self.data["session"]["generation"].as_u64().unwrap_or(0);
        let turns = list(&self.data["session"]["turns"]);
        if self.chat_viewport.take_scroll_request() {
            if let Some(conversation) = self.conversation.cast::<web_sys::HtmlElement>() {
                conversation.set_scroll_top(conversation.scroll_height());
            }
        }
        for turn in turns {
            let tid = text(&turn["turn_id"]);
            if turn["role"] != "assistant"
                || crate::tutor_stream_state::is_stream_turn(&turn)
                || turn["status"] != "generated"
                || text(&turn["content"]).is_empty()
                || self.shown.contains(&tid)
                || self.delivery_pending.contains(&tid)
                || self.delivery_retry_blocked.contains(&tid)
                || self.delivery_attempts.get(&tid).copied().unwrap_or(0) >= 2
            {
                continue;
            }
            self.delivery_pending.insert(tid.clone());
            *self.delivery_attempts.entry(tid.clone()).or_default() += 1;
            let ticket = self.guard.borrow().capture();
            let sid = sid.clone();
            let display_revision = self.chat_viewport.display_revision();
            ctx.link().send_future(async move {
                let result = api(
                    &format!("/api/sessions/{sid}/delivery"),
                    "POST",
                    Some(json!({"turn_id":tid,"status":"shown"})),
                )
                .await;
                Msg::Delivered(ticket, sid, generation, display_revision, tid, result)
            });
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        self.render_app(ctx)
    }
}

#[cfg(test)]
mod ui_control_tests {
    use super::*;
    #[test]
    fn stop_controls_remain_available_while_conflicting_mutations_wait() {
        for action in [
            Action::Stop,
            Action::SpeechStop,
            Action::PtzStop,
            Action::Discover,
        ] {
            assert!(action.permitted_while_busy());
        }
        for action in [
            Action::Send,
            Action::Switch("b".into()),
            Action::Create,
            Action::Settings(json!({"language":"en"})),
            Action::UiSettings,
            Action::NewActivity,
            Action::ContinueActivity(HistoricalActivity {
                learner: "l".into(),
                session: "past".into(),
                activity: "past-activity".into(),
                goal: "A past goal".into(),
                title_stale: false,
                created_at: String::new(),
                finished: true,
                continued_from: Value::Null,
            }),
            Action::Finish,
            Action::Modules,
            Action::SaveMemory,
            Action::ConfirmDelete,
            Action::Read("turn".into()),
        ] {
            assert!(!action.permitted_while_busy());
        }
    }
}

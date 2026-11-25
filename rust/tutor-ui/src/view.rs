use crate::{
    activity_history::{ActivityHistory, ContinuationSource},
    activity_summary::{pending_needs_confirmation, ActivitySummary},
    app::{Action, App, Field, Msg},
    camera::CameraPreview,
    confirm_forget::ConfirmForget,
    conversation_history::ConversationHistory,
    evidence_labels::source_label,
    experiment_comparison::ExperimentComparison,
    help_preference::HelpPreference,
    historical_comparisons::HistoricalComparisons,
    i18n::t,
    live_transcript::LiveTranscript,
    local_asr_status::LocalAsrStatus,
    local_memory_status::LocalMemoryStatus,
    local_speech_status::LocalSpeechStatus,
    memory_evidence::{kind_label, scope_label, MemoryEvidence},
    memory_review::MemoryReview,
    model::{
        fallback, list, milliseconds, preferences as preference_text, recoverable_input, text,
    },
    module_inventory::ModuleInventory,
    operation_notice::OperationNotice,
    recording_draft::ReviewKind,
    recording_review::RecordingReview,
    robot_companion::RobotCompanion,
    tutor_stream::TutorStream,
    tutor_stream_state::{is_stream_turn, StreamKey},
    voice_loop_controls::VoiceLoopControls,
};

#[derive(Properties, PartialEq)]
pub(crate) struct SelectProps {
    pub value: AttrValue,
    pub onchange: Callback<Event>,
    #[prop_or_default]
    pub label: AttrValue,
    #[prop_or_default]
    pub disabled: bool,
    pub children: Children,
}

/// HTML select has a value property, but no reflected value attribute.
#[function_component(SelectControl)]
pub(crate) fn select_control(props: &SelectProps) -> Html {
    let element = use_node_ref();
    {
        let element = element.clone();
        let value = props.value.clone();
        use_effect(move || {
            if let Some(select) = element.cast::<HtmlSelectElement>() {
                select.set_value(&value);
            }
        });
    }
    html! { <select ref={element} aria-label={props.label.clone()} disabled={props.disabled} onchange={props.onchange.clone()}>{for props.children.iter()}</select> }
}
use serde_json::{json, Value};
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

pub(crate) fn icon(name: &str) -> Html {
    let path = match name {
        "camera" => {
            "M14 4h-4L8 7H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-4z M16 14a4 4 0 1 1-8 0 4 4 0 0 1 8 0"
        }
        "mic" => {
            "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3 M5 10v2a7 7 0 0 0 14 0v-2 M12 19v3 M8 22h8"
        }
        "light" => "M9 18h6 M9 22h6 M9 15a6 6 0 1 1 6 0v3H9z",
        "settings" => "M4 5h16 M4 12h16 M4 19h16 M8 2v6 M16 9v6 M9 16v6",
        "memory" => "M5 4h14v16H5z M9 8h6 M9 12h6 M9 16h4 M2 8h3 M2 16h3 M19 8h3 M19 16h3",
        "shield" => "M12 2l8 4v6c0 5-8 10-8 10S4 17 4 12V6z M8 12l3 3 5-6",
        "trash" => "M3 6h18 M9 6V3h6v3 M6 6l1 15h10l1-15 M10 10v7 M14 10v7",
        "arrow" => "M4 12h16 M14 6l6 6-6 6",
        "chevron" => "M6 9l6 6 6-6",
        "plus" => "M12 4v16 M4 12h16",
        "close" => "M6 6l12 12 M18 6L6 18",
        "stop" => "M6 6h12v12H6z",
        "send" => "M22 2L9 15 M22 2l-7 20-6-7-7-6z",
        "refresh" => "M20 8a8 8 0 1 0 0 8 M20 3v5h-5",
        "focus" => "M8 3H3v5 M16 3h5v5 M21 16v5h-5 M8 21H3v-5 M8 8h8v8H8z",
        "volume" => "M11 5L6 9H2v6h4l5 4z M15 8a6 6 0 0 1 0 8 M18 5a10 10 0 0 1 0 14",
        "check" => "M4 12l5 5L20 6",
        "history" => "M3 12a9 9 0 1 0 3-7 M3 3v5h5 M12 7v5l4 2",
        "sparkle" => "M12 3l2.4 6.6L21 12l-6.6 2.4L12 21l-2.4-6.6L3 12l6.6-2.4z M20 2v4 M18 4h4",
        "book" => "M12 5v16 M12 5C9 3 5 3 2 4v15c3-1 7-1 10 1 3-2 7-2 10-1V4c-3-1-7-1-10 1z",
        "activity" => "M2 12h4l3-9 5 18 3-9h5",
        "flag" => "M4 22V3 M4 3c5-4 9 4 16 0v11c-7 4-11-4-16 0",
        "radio" => {
            "M9 8a6 6 0 0 0 0 8 M15 8a6 6 0 0 1 0 8 M5 4a12 12 0 0 0 0 16 M19 4a12 12 0 0 1 0 16 M11 11h2v2h-2z"
        }
        _ => {
            "M5 5h14v14H5z M9 9h6v6H9z M2 8h3 M2 16h3 M19 8h3 M19 16h3 M8 2v3 M16 2v3 M8 19v3 M16 19v3"
        }
    };
    html! {<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={path}/></svg>}
}
fn click(ctx: &Context<App>, action: Action) -> Callback<MouseEvent> {
    ctx.link().callback(move |_| Msg::Action(action.clone()))
}
fn input(ctx: &Context<App>, field: Field) -> Callback<InputEvent> {
    ctx.link().callback(move |event: InputEvent| {
        Msg::Field(
            field.clone(),
            event.target_unchecked_into::<HtmlInputElement>().value(),
        )
    })
}
fn textarea(ctx: &Context<App>, field: Field) -> Callback<InputEvent> {
    ctx.link().callback(move |event: InputEvent| {
        Msg::Field(
            field.clone(),
            event.target_unchecked_into::<HtmlTextAreaElement>().value(),
        )
    })
}
fn select(ctx: &Context<App>, field: Field) -> Callback<Event> {
    ctx.link().callback(move |event: Event| {
        Msg::Field(
            field.clone(),
            event.target_unchecked_into::<HtmlSelectElement>().value(),
        )
    })
}

impl App {
    fn teaching_label(&self) -> String {
        match self.data["teaching"]["provider"].as_str() {
            Some("deepseek_flash") => if self.ui_language == "en" {
                "DeepSeek Flash · Remote teaching"
            } else {
                "DeepSeek Flash · 远程教学"
            }
            .into(),
            Some("invalid") => if self.ui_language == "en" {
                "Teaching provider unavailable"
            } else {
                "教学模型暂不可用"
            }
            .into(),
            _ => t(&self.ui_language, "Kimi · 远程推理").into(),
        }
    }
    fn teaching_input_notice(&self) -> String {
        match self.data["teaching"]["provider"].as_str() {
            Some("deepseek_flash") => if self.ui_language == "en" {
                "Review and edit the transcript before sending. Audio stays local; teaching text is sent to DeepSeek."
            } else { "转写后可修改再发送。语音在本机处理，教学文字会发送到 DeepSeek。" }.into(),
            Some("invalid") => if self.ui_language == "en" {
                "You can still type and record locally. Correct the teaching provider configuration before requesting a reply."
            } else { "仍可输入和在本机录音。请求教学回复前，请先修正教学模型配置。" }.into(),
            _ => t(&self.ui_language, "转写后可修改再发送。语音在本机处理，教学文字会发送到 Kimi。").into(),
        }
    }
    fn teaching_privacy_notice(&self) -> String {
        if matches!(
            self.data["teaching"]["provider"].as_str(),
            Some("deepseek_flash" | "invalid")
        ) {
            if self.ui_language == "en" {
                "Raw video, face crops and recordings are not saved by default. Teaching text uses the selected remote teaching service. When background memory is enabled, its source text is processed by Kimi. Teachers can correct memories or clear this profile's data."
            } else { "默认不保存原始视频、脸部裁剪或录音。教学文本使用当前选择的远程教学服务；启用后台记忆时，其来源文本通过 Kimi 处理。教师可纠正记忆或清除此档案的数据。" }.into()
        } else {
            t(&self.ui_language, "默认不保存原始视频、脸部裁剪或录音。教学文本通过 Kimi 远程处理。教师可纠正记忆或清除此档案的数据。").into()
        }
    }
    pub fn render_app(&self, ctx: &Context<Self>) -> Html {
        let learner = self.learner();
        let has_learner = !learner.is_null();
        let simulator = self.simulator();
        let session = &self.data["session"];
        let state = &session["state"];
        let name = fallback(&learner["name"], t(&self.ui_language, "尚未选择"));
        let avatar = name.chars().next().unwrap_or('?').to_string();
        let en = self.ui_language == "en";
        let goal = text(&state["goal"]);
        let escape = ctx.link().batch_callback(|event: KeyboardEvent| {
            if event.key() == "Escape" {
                event.prevent_default();
                Some(Msg::Action(Action::Stop))
            } else {
                None
            }
        });
        html! {<div class={classes!("app-shell","demo-shell",session["continued_from"].is_object().then_some("has-continuation"))} onkeydown={escape}>
            <a class="skip-link" href="#learning-conversation">{if en{"Skip to conversation"}else{"跳到学习对话"}}</a>
            <header class="topbar">
                <a class="brand" href="#learning-space" aria-label={t(&self.ui_language,"AI Tutor 首页")}>
                    <span class="brand-mark"><svg width="34" height="34" viewBox="0 0 40 40" fill="none" aria-hidden="true"><path d="M20 6v5" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"/><circle cx="20" cy="5" r="2.5" fill="#F3987C"/><rect x="6" y="12" width="28" height="23" rx="10" fill="currentColor"/><rect x="10" y="17" width="20" height="12" rx="6" fill="#252B45"/><path d="M15 21v3m10-3v3" stroke="#C9BFFF" stroke-width="2.8" stroke-linecap="round"/><path d="M17 31h6" stroke="#C9BFFF" stroke-width="2" stroke-linecap="round"/></svg></span>
                    <span><strong>{"AI Tutor"}<span class="brand-dot">{"✦"}</span></strong><small>{if en{"GROW WITH CURIOSITY"}else{"与好奇心一起长大"}}</small></span>
                </a>
                <nav class="learning-nav" aria-label={if en{"Learning navigation"}else{"学习导航"}}><a href="#learning-space">{icon("sparkle")}{if en{"Explore"}else{"一起探索"}}</a><a href="#learning-records">{icon("book")}{if en{"Learning journal"}else{"学习手记"}}</a></nav>
                <div class="top-actions"><span class={classes!("connection",self.online.then_some("connected"))}><i/>{if self.online{if en{"Connected locally"}else{"本机已连接"}}else{if en{"Connecting…"}else{"正在连接…"}}}</span><label class="ui-language-picker"><span class="visually-hidden">{"界面 / Interface"}</span><SelectControl label="Interface language / 界面语言" value={self.ui_language.clone()} disabled={!self.busy.is_empty()} onchange={select(ctx,Field::UiLanguage)}><option value="zh">{"中文"}</option><option value="en">{"English"}</option></SelectControl></label><button class={classes!("teacher-toggle",self.teacher.then_some("active"))} aria-label={if en{"Settings"}else{"设置"}} aria-expanded={self.teacher.to_string()} aria-controls="teacher-settings" onclick={ctx.link().callback(|_|Msg::ToggleTeacher)}>{icon("settings")}<span>{if en{"Settings"}else{"设置"}}</span>{icon("chevron")}</button></div>
            </header>
            <main>
            <div class="intro"><div><span class="eyebrow">{if en{"A LITTLE IDEA. A NEW DISCOVERY."}else{"一点好奇，一起发现"}}</span><h1>{if en{"What shall we discover today?"}else{"今天，我们一起探索什么？"}}</h1><p>{if en{"Think it through, try it out, and find your next little discovery."}else{"从一个小想法出发，动手试一试，让发现慢慢发生。"}}</p></div><div class="profile-summary"><span class="profile-avatar">{avatar}</span><div><small>{t(&self.ui_language,"当前学习者档案")}</small><strong>{name}</strong><span>{if has_learner{if simulator{t(&self.ui_language,"独立模拟空间")}else{t(&self.ui_language,"真实学习记录")}}else{t(&self.ui_language,"请在教师工作台创建或选择")}}</span></div>{if has_learner{html!{<span class={classes!("pill",simulator.then_some("simulation"))}>{if simulator{t(&self.ui_language,"模拟")}else{t(&self.ui_language,"匿名档案")}}</span>}}else{Html::default()}}</div></div>
            {if !self.error.is_empty(){html!{<OperationNotice ui_language={self.ui_language.clone()} detail={self.error.clone()} on_dismiss={ctx.link().callback(|_|Msg::ClearError)}/>}}else{Html::default()}}
            {if !has_learner&&!self.teacher{html!{<div class="onboard"><span><strong>{t(&self.ui_language,"从一个匿名档案开始。")}</strong>{t(&self.ui_language,"教师选择档案后，Tutor 才会记录和召回这位学习者的进展。")}</span><button class="primary" onclick={ctx.link().callback(|_|Msg::ToggleTeacher)}>{t(&self.ui_language,"设置学习者")}{icon("arrow")}</button></div>}}else{Html::default()}}
            {if simulator{html!{<div class="sim-banner">{icon("radio")}{t(&self.ui_language,"当前为独立模拟空间。模块、活动和记忆均标记为模拟，不作为真实硬件验证。")}</div>}}else{Html::default()}}
            {if self.teacher{html!{<div id="teacher-settings">{self.render_teacher(ctx)}</div>}}else{Html::default()}}
            <section class="workspace-grid" id="learning-space" aria-label={if en{"Learning space"}else{"学习空间"}}>
                <div class="companion-column">
                    <div class="companion-stage">
                        <div class="companion-heading"><div><span class="companion-kicker">{if en{"YOUR LEARNING COMPANION"}else{"你的学习伙伴"}}</span><h2>{if en{"Hi, I'm Xiao Yu."}else{"嗨，我是小宇。"}}<span aria-hidden="true">{"✦"}</span></h2><p>{if en{"Let's turn your ideas into discoveries."}else{"陪你把每一个“为什么”，变成新的发现。"}}</p></div><span class="companion-badge">{"AI"}<span>{if en{"COMPANION"}else{"学习搭子"}}</span></span></div>
                        <RobotCompanion ptz={self.data["ptz"].clone()} camera={self.data["camera"].clone()} audio={self.data["audio"].clone()} ui_language={self.ui_language.clone()}/>
                    </div>
                    <div class="stage-utilities">
                        <section class="current-mission"><span class="mission-icon">{icon("flag")}</span><div><small>{if session["status"]=="finished"{if en{"THIS EXPLORATION HAS ENDED"}else{"这次探索已结束"}}else{if en{"OUR CURRENT EXPLORATION"}else{"正在探索的小任务"}}}</small><h3>{if goal.is_empty(){if !session["continued_from"].is_null(){if en{"Confirm today's goal and setup.".to_owned()}else{"先核对今天的目标与搭建。".to_owned()}}else{if en{"Every discovery starts with a question.".to_owned()}else{"每一个发现，都从一个问题开始。".to_owned()}}}else{goal}}</h3>
                            <ContinuationSource session={session.clone()} ui_language={self.ui_language.clone()} on_view={ctx.link().callback(Msg::ViewActivity)}/>
                            {if self.has_session()&&session["status"]!="finished"{html!{<div class="mission-preparation">
                                {if state["requires_inventory_confirmation"]==true{html!{<p class="mission-readiness">{if en{"Today's modules and setup still need confirmation."}else{"今天的模块与设置还需要重新确认。"}}</p>}}else{Html::default()}}
                                <div class="mission-actions"><button class="secondary" disabled={!self.busy.is_empty()||self.activity_transition_pending()} onclick={ctx.link().callback(|_|Msg::OpenModuleConfirmation)}>{icon("circuit")}{if state["requires_inventory_confirmation"]==true{if en{"Confirm today's modules"}else{"确认今天的模块"}}else{if en{"View current modules"}else{"查看当前模块"}}}</button><button class="text-button" disabled={self.input_disabled()||!self.busy.is_empty()} onclick={ctx.link().callback(|_|Msg::FocusTodayPlan)}>{icon("book")}{if en{"Write today's plan"}else{"输入今天的计划"}}</button></div>
                            </div>}}else{Html::default()}}
                            <a href="#learning-records">{if en{"View the learning journal"}else{"查看学习手记"}}{icon("arrow")}</a></div></section>
                        <div class="camera-dock"><CameraPreview compact={true} ui_language={self.ui_language.clone()} camera={self.data["camera"].clone()} ptz={self.data["ptz"].clone()} cameras={self.cameras.clone()} camera_id={self.camera_id.clone()} busy={!self.busy.is_empty()} on_action={ctx.link().callback(Msg::Action)} on_camera={ctx.link().callback(|id|Msg::Field(Field::Camera,id))}/></div>
                    </div>
                </div>
                {self.render_dialogue(ctx)}
            </section>
            <section class="learning-records" id="learning-records" aria-label={if en{"Learning journal"}else{"学习手记"}}><div class="records-heading"><span>{icon("book")}<h2>{if en{"A little more from our learning"}else{"把每一次发现，留作下一步"}}</h2></span><p>{if en{"Your activity, observations and memories — ready when you need them."}else{"活动、观察与记忆，需要时随时翻开。"}}</p></div><details class="panel activity-disclosure"><summary><span>{icon("history")}<strong>{if en{"Current activity & past explorations"}else{"当前活动与探索记录"}}</strong><small>{if en{"Goals · experiments · history"}else{"目标 · 实验 · 历史"}}</small></span>{icon("chevron")}</summary>{self.render_activity(ctx,state)}</details><div class="lower-grid">{self.render_memories(ctx,state)}{self.render_diagnostics()}</div></section>
            <footer><span>{"AI Tutor"}<span class="footer-sparkle">{"✦"}</span>{if en{"Made for curious minds"}else{"为每一份好奇心而来"}}</span><span>{if en{"Local camera & voice · You control your learning records"}else{"本机画面与语音 · 学习记录由你管理"}}</span></footer></main>
            {if !self.busy.is_empty(){html!{<div class="busy-toast" role="status"><span class="spin">{icon("refresh")}</span>{t(&self.ui_language,&self.busy).to_owned()}</div>}}else{Html::default()}}
            {self.render_delete(ctx)}
        </div>}
    }

    fn render_dialogue(&self, ctx: &Context<Self>) -> Html {
        let session = &self.data["session"];
        let turns = list(&session["turns"]);
        let learner = self.learner();
        let finished = session["status"] == "finished";
        let stream_mode = self.data["tutor_delivery"]["mode"] == "segments";
        let stream_text_only = stream_mode && !self.segment_speech_enabled();
        let recalled = list(&session["recalled_memories"]);
        let name = fallback(&learner["name"], "学习者");
        let generating = session["status"] == "generating" || session["status"] == "running";
        let status = if finished {
            "本次活动已结束"
        } else if self.record_start_pending {
            "正在准备本机录音…"
        } else if self.recording {
            "正在本机录音 · 松开结束"
        } else if self.transcribing {
            "正在本机转写…"
        } else if generating {
            "正在生成 · 可停止或提交新问题"
        } else {
            "文字输入 / 按住说话"
        };
        let keydown = ctx.link().batch_callback(|event: KeyboardEvent| {
            if event.key() == "Enter" && !event.shift_key() && !event.is_composing() {
                event.prevent_default();
                Some(Msg::Action(Action::Send))
            } else {
                None
            }
        });
        let pointer_down = ctx.link().callback(|event: PointerEvent| {
            if let Some(target) = event
                .current_target()
                .and_then(|v| v.dyn_into::<web_sys::Element>().ok())
            {
                let _ = target.set_pointer_capture(event.pointer_id());
            }
            Msg::RecordStart
        });
        let ptt_key_down = ctx.link().batch_callback(|event: KeyboardEvent| {
            if (event.key() == " " || event.key() == "Enter") && !event.repeat() {
                event.prevent_default();
                Some(Msg::RecordStart)
            } else {
                None
            }
        });
        let ptt_key_up = ctx.link().batch_callback(|event: KeyboardEvent| {
            if event.key() == " " || event.key() == "Enter" {
                event.prevent_default();
                Some(Msg::RecordEnd)
            } else {
                None
            }
        });
        html! {<div class="dialogue-column" id="learning-conversation"><article class="panel dialogue-panel"><div class="panel-title"><div><span class="dialogue-title-icon">{icon("sparkle")}</span><h2>{if self.ui_language=="en"{"Let's think together"}else{"和小宇聊一聊"}}</h2></div><span class="model-label">{self.teaching_label()}</span></div>
            {if finished{html!{<div class="session-complete" role="status"><div><strong>{t(&self.ui_language,"本次活动已结束")}</strong><p>{if self.ui_language=="en"{"Your records are saved. Continue this exploration or begin a new one; today's setup will need confirmation."}else{"学习记录已保存。可以续做这次探索，也可以换一个新目标；今天的搭建仍需重新确认。"}}</p></div><div class="session-complete-actions">{crate::activity_history_state::HistoricalActivity::from_finished_session(session).map(|source|html!{<button class="primary" disabled={!self.busy.is_empty()||self.activity_transition_pending()||self.comparison_saving()} onclick={click(ctx,Action::ContinueActivity(source))}>{if self.ui_language=="en"{"Continue this exploration"}else{"继续这次探索"}}{icon("arrow")}</button>}).unwrap_or_default()}<button class="secondary" disabled={!self.busy.is_empty()||self.activity_transition_pending()||self.comparison_saving()} onclick={click(ctx,Action::NewActivity)}>{if self.ui_language=="en"{"Start a new exploration"}else{"开始新的探索"}}</button></div></div>}}else{Html::default()}}
            <div class="history-switch">
                {if self.chat_viewport.history_open() {html!{<button class="text-button" onclick={ctx.link().callback(|_|Msg::CloseHistory)}>{if self.ui_language=="en" {"Return to current messages"} else {"返回当前消息"}}</button>}}
                else {html!{<><span>{if self.ui_language=="en" {"Current session · Latest messages"} else {"当前活动 · 最新消息"}}</span><button class="text-button" disabled={!self.busy.is_empty()||session["history_before"].as_i64().is_none()} onclick={ctx.link().callback(|_|Msg::OpenHistory)}>{if self.ui_language=="en" {"View earlier messages"} else {"查看更早消息"}}</button></>}}}
            </div>
            {if self.chat_viewport.history_open() {html!{
                <div class="historical-learning-content" ref={self.historical_records.clone()} tabindex="-1">
                {if let Some(activity)=&self.historical_activity {html!{<div class="historical-activity-caption"><strong>{crate::activity_history::title(activity,&self.ui_language)}</strong><p>{if self.ui_language=="en"{"Viewing a past activity. Your current activity has not changed."}else{"正在查看过去的活动。当前活动尚未切换。"}}</p><button class="secondary" disabled={!activity.finished||!self.busy.is_empty()||self.continuation.request.is_some()} onclick={click(ctx,Action::ContinueActivity(activity.clone()))}>{if self.ui_language=="en"{"Continue this activity"}else{"继续这次活动"}}</button></div>}}else{Html::default()}}
                {if let Some(activity)=&self.historical_activity {html!{<HistoricalComparisons learner_id={activity.learner.clone()} namespace={if self.simulator(){"simulator".to_owned()}else{"real".to_owned()}} session_id={activity.session.clone()} learner_epoch={learner["epoch"].as_i64().unwrap_or(-1)} ui_language={self.ui_language.clone()} enabled={self.online&&self.busy.is_empty()&&!self.activity_transition_pending()}/>}}else{Html::default()}}
                <ConversationHistory learner_id={text(&learner["learner_id"])} session_id={self.historical_activity.as_ref().map(|a|a.session.clone()).unwrap_or_else(||self.session_id())} learner_epoch={learner["epoch"].as_i64().unwrap_or(-1)} before={self.history_before} ui_language={self.ui_language.clone()} other_activity={self.historical_activity.is_some()}/>
                </div>
            }} else {Html::default()}}
            <div class="conversation" style={if self.chat_viewport.history_open(){"display:none"}else{""}} ref={self.conversation.clone()} onscroll={ctx.link().callback(|event:Event|{let element=event.target_unchecked_into::<HtmlElement>();Msg::ConversationScrolled(element.scroll_top(),element.scroll_height(),element.client_height())})} aria-live="polite" aria-relevant="additions text">
            {if turns.is_empty()&&!recalled.is_empty(){html!{<div class="resume-card"><strong>{t(&self.ui_language,"相关学习记忆")}</strong><ul>{for recalled.iter().take(2).map(|memory|html!{<li>{text(&memory["content"])}</li>})}</ul><p>{t(&self.ui_language,"这些是历史报告。先确认今天的搭建，再继续实验或提出新想法。")}</p></div>}}else{Html::default()}}
            {if turns.is_empty(){html!{<div class="chat-empty"><span class="tutor-emblem">{icon("sparkle")}</span><h3>{if self.ui_language=="en"{"Big discoveries start small."}else{"小小的想法，也值得认真探索。"}}</h3><p>{if self.ui_language=="en"{"Tell me what you're wondering, building or trying. We'll work out the next step together."}else{"说说你想做什么，或刚刚遇到了什么。我们一起想一想，再动手试一试。"}}</p><div class="prompt-suggestions"><small>{if self.ui_language=="en"{"Need a starting point?"}else{"还没想好？从这里开始"}}</small>{for [
                ("我想让小车遇到障碍物停下来。", "I'd like my rover to stop when it meets an obstacle.", "让小车学会停下来", "Help a rover stop"),
                ("电机不转，我可以先检查什么？", "My motor won't turn. What should I check first?", "一起找找电机的问题", "Investigate a motor"),
                ("帮我设计一次只改变一个条件的小实验。", "Help me design a small experiment that changes only one condition.", "设计一个小实验", "Plan a small experiment")
            ].iter().map(|(zh,en,zh_label,en_label)|{
                let prompt=if self.ui_language=="en"{*en}else{*zh}.to_owned();
                html!{<button class="prompt-chip" disabled={self.input_disabled()||!self.busy.is_empty()||!self.input.trim().is_empty()} onclick={ctx.link().callback(move |_|Msg::Field(Field::Input,prompt.clone()))}>{if self.ui_language=="en"{*en_label}else{*zh_label}}{icon("arrow")}</button>}
            })}</div></div>}}else{html!{for turns.iter().map(|turn| {
                let assistant = turn["role"] == "assistant";
                let tid = text(&turn["turn_id"]);
                let streamed = is_stream_turn(turn);
                let binding = StreamKey::from_head(&self.session_id(), &session["tutor_stream"])
                    .filter(|binding| streamed && binding.turn == tid);
                let message_status = if streamed {
                    if matches!(turn["stream_phase"].as_str(), Some("failed" | "cancelled" | "interrupted")) {
                        if self.ui_language == "en" { "Reply incomplete" } else { "回复未完成" }
                    } else if turn["stream_phase"] == "completed" {
                        if self.ui_language == "en" { "Teaching guidance" } else { "教学指导" }
                    } else {
                        if self.ui_language == "en" { "Text reply" } else { "文字回复" }
                    }
                } else {
                    match turn["status"].as_str().unwrap_or("") {
                        "cancelled" => t(&self.ui_language,"已取消"), "generated" => t(&self.ui_language,"待投递"),
                        "speech_interrupted" => t(&self.ui_language,"朗读已打断"),
                        _ => if assistant { t(&self.ui_language,"教学指导") } else { source_label(&self.ui_language,"learner_report") }
                    }
                };
                html!{<div class={classes!("message",if assistant{"tutor-message"}else{"learner-message"},(turn["status"]=="cancelled").then_some("cancelled"))} key={tid.clone()}>
                    <div class="message-meta"><span>{if assistant{if self.ui_language=="en"{"Xiao Yu · AI Tutor".to_owned()}else{"小宇 · AI Tutor".to_owned()}}else{name.clone()}}</span><small>{message_status}</small></div>
                    {if let Some(binding) = binding {
                        let sid = binding.session.clone();
                        let sync_sid = sid.clone();
                        let request = binding.request.clone();
                        html!{<TutorStream binding={binding} enabled={self.stream_display_enabled()} ack_enabled={self.stream_ack_enabled()} viewport={self.conversation.clone()}
                            head_revision={session["tutor_stream"]["revision"].as_u64().unwrap_or(0)}
                            head_phase={text(&session["tutor_stream"]["phase"])} persisted_text={text(&turn["content"])}
                            ui_language={self.ui_language.clone()}
                            on_progress={ctx.link().callback(move |tid| Msg::StreamProgress(sid.clone(),tid))}
                            on_sync={ctx.link().callback(move |_| Msg::StreamSync(sync_sid.clone(),request.clone()))}/>}
                    } else { html!{<div class="message-bubble">{text(&turn["content"])}</div>} }}
                    {if assistant&&session["tutor_stream"]["turn_id"]==turn["turn_id"] {self.stream_speech_status().map(|status|html!{<div class="segment-speech-status"><small role="status">{status}</small>{if self.stream_speech_close_available(){html!{<button class="text-button" onclick={ctx.link().callback(|_|Msg::CloseSegmentSpeech)}>{if self.ui_language=="en"{"Turn off speech for this reply"}else{"关闭本条自动朗读"}}</button>}}else{Html::default()}}</div>}).unwrap_or_default()}else{Html::default()}}
                    {if assistant {html!{<button class="read-button" disabled={!self.busy.is_empty()||(!turn["generation"].is_null()&&turn["generation"]!=session["generation"])||(streamed&&turn["stream_phase"]!="completed")} title={t(&self.ui_language,"本地语音播报（仅当前有效轮次）")} onclick={click(ctx,Action::Read(tid))}>{icon("volume")}{t(&self.ui_language,"朗读")}</button>}} else {Html::default()}}
                </div>}
            })}}}
            {if generating{html!{<div class="thinking"><span class="spin">{icon("refresh")}</span>{t(&self.ui_language,"正在结合当前活动与相关记忆思考…")}</div>}}else{Html::default()}}
            {if session["status"]=="error"{let errors=list(&self.data["errors"]);html!{<div class="turn-error" role="alert">{icon("shield")}<div class="turn-error-content">{t(&self.ui_language,"本轮回复未完成。可检查详情，或取回问题后再发送。")}<details><summary>{t(&self.ui_language,"查看服务详情（原文）")}</summary><p>{errors.last().map(|e|text(&e["message"])).filter(|s|!s.is_empty()).unwrap_or_else(||t(&self.ui_language,"请检查后端状态后重试。").into())}</p></details>{if recoverable_input(session).is_some(){html!{<button class="text-button" disabled={!self.input.trim().is_empty()||self.input_disabled()} onclick={click(ctx,Action::RecoverInput)}>{t(&self.ui_language,"取回上次问题，检查后再发送")}</button>}}else{Html::default()}}</div></div>}}else{Html::default()}}</div>
            {if self.chat_viewport.unseen || (session["tutor_stream"].is_object() && !self.chat_viewport.stream_following()) {html!{<div class="new-reply-row"><button class="new-reply-button" onclick={ctx.link().callback(|_|Msg::ShowLatest)}>{if self.chat_viewport.unseen {if self.ui_language=="en"{"New reply · Jump to latest"}else{"有新回复 · 查看最新"}} else {if self.ui_language=="en"{"Return to latest messages"}else{"返回最新消息"}}}{icon("arrow")}</button></div>}}else{Html::default()}}
            <div class="composer"><div class="composer-heading"><span><i class={if self.recording{"record-indicator"}else{""}}/>{t(&self.ui_language,status)}</span><label title={if stream_text_only {if self.ui_language=="en" {"Automatic segment speech is disabled in this workstation"} else {"此工作台未启用自动分句朗读"}} else if stream_mode {if self.ui_language=="en"{"Captured when you send. Turning it off also stops this reply; turning it on does not resume it."}else{"发送时固定本次选择。取消勾选也会关闭本条；重新勾选不会恢复本条。"}}else{"仅播报已通过校验的回复"}}><input type="checkbox" disabled={stream_text_only} checked={self.auto_speak&&!stream_text_only} onchange={ctx.link().callback(|_|Msg::ToggleSpeak)}/>{if stream_text_only {if self.ui_language=="en" {"Text only · no auto-read"} else {"仅文字 · 不自动朗读"}} else if stream_mode {if self.ui_language=="en"{"Auto-read next send"}else{"下次发送自动朗读"}}else{t(&self.ui_language,"自动朗读")}}</label></div>
            <LiveTranscript recording={self.caption_ticket()} ui_language={self.ui_language.clone()} automatic={self.voice_loop.active()}
                on_status={ctx.link().callback(|(ticket,result)|Msg::VoiceAudioStatus(ticket,result))}/>
            <RecordingReview transcript={self.recording_draft.review(ReviewKind::Transcript).map(str::to_owned)} submission={self.recording_draft.review(ReviewKind::Submission).map(str::to_owned)}
                ui_language={self.ui_language.clone()} enabled={!self.input_disabled()&&self.busy.is_empty()}
                on_adopt={ctx.link().callback(|(kind,append)|Msg::AdoptRecordingText(kind,append))} on_dismiss={ctx.link().callback(Msg::DismissRecordingText)}/>
            {if !self.input.is_empty(){html!{<button class="text-button clear-input-draft" onclick={ctx.link().callback(|_|Msg::ClearInputDraft)}>{if self.ui_language=="en"{"Clear input draft"}else{"清空输入草稿"}}</button>}}else{Html::default()}}
            <textarea ref={self.composer_input.clone()} aria-label={t(&self.ui_language,"给 Tutor 的问题或观察")} placeholder={if finished{t(&self.ui_language,"本次活动已结束，请开始下一次活动")}else if self.has_session(){t(&self.ui_language,"描述你的目标、搭建或观察结果…")}else{t(&self.ui_language,"先选择一个学习者档案…")}} value={self.input.clone()} disabled={self.input_disabled()} maxlength="4000" oninput={textarea(ctx,Field::Input)} onkeydown={keydown}/>
            <div class="composer-actions"><button class={classes!("ptt",self.recording.then_some("recording"))} disabled={self.voice_loop.active()||!self.has_session()||session["status"]=="finished"||self.transcribing||!self.busy.is_empty()||self.recording_draft.review(ReviewKind::Transcript).is_some()} onpointerdown={pointer_down} onpointerup={ctx.link().callback(|_|Msg::RecordEnd)} onpointercancel={ctx.link().callback(|_|Msg::RecordEnd)} onblur={ctx.link().callback(|_|Msg::RecordEnd)} onkeydown={ptt_key_down} onkeyup={ptt_key_up}>{icon("mic")}{if self.recording{t(&self.ui_language,"松开结束")}else{t(&self.ui_language,"按住说话")}}</button><button class="stop-button" title={t(&self.ui_language,"停止生成和播报")} onclick={click(ctx,Action::Stop)}>{icon("stop")}{t(&self.ui_language,"停止")}</button><button class="send-button" disabled={self.input_disabled()||self.sending_request()||!self.busy.is_empty()||self.input.trim().is_empty()||self.recording_draft.review(ReviewKind::Submission).is_some()} onclick={click(ctx,Action::Send)}><span>{t(&self.ui_language,"发送")}</span>{icon("send")}</button></div><p class="composer-note">{self.teaching_input_notice()}</p></div>
        </article>
        <details class="panel voice-panel" open={self.voice_loop.active()} aria-label={if self.ui_language=="en"{"Voice and transcription"}else{"语音与转写"}}>
            <summary><span>{icon("mic")}<strong>{if self.ui_language=="en"{"Voice & listening"}else{"语音与聆听"}}</strong><small>{if self.voice_loop.active(){if self.ui_language=="en"{"Voice conversation is active"}else{"语音轮换进行中"}}else{if self.ui_language=="en"{"Conversation and local services"}else{"连续对话与本机服务"}}}</small></span>{icon("chevron")}</summary>
            <div class="voice-panel-content">
            <VoiceLoopControls language={self.ui_language.clone()} phase={self.voice_loop.phase}
                barge_in_available={self.barge_in_available()} barge_in_requested={self.barge_in_requested}
                barge_in_active={self.voice_loop.interruption_enabled()} monitoring={self.voice_loop.monitoring}
                on_barge_in={ctx.link().callback(|_|Msg::ToggleBargeIn)}
                pause_reason={self.voice_loop.pause_reason.clone()}
                available={self.voice_loop_available()} can_begin={self.can_begin_voice_loop()}
                selected={self.loop_microphone.as_ref().map(|device|device.key.clone()).unwrap_or_default()} devices={self.microphones.clone()}
                on_device={ctx.link().callback(|id|Msg::Field(Field::LoopMicrophone,id))}
                on_begin={ctx.link().callback(|_|Msg::VoiceBegin)} on_pause={ctx.link().callback(|_|Msg::VoicePause)}/>
            <details class="voice-services"><summary>{if self.ui_language=="en"{"Local voice and transcription services"}else{"本地语音与转写服务"}}</summary>
                <LocalSpeechStatus status={self.data["local_speech"].clone()} ui_language={self.ui_language.clone()} enabled={self.online&&self.busy.is_empty()} on_refresh={ctx.link().callback(|_|Msg::FetchState(true))}/>
                <LocalAsrStatus status={self.data["local_asr"].clone()} ui_language={self.ui_language.clone()} enabled={self.online&&self.busy.is_empty()} on_refresh={ctx.link().callback(|_|Msg::FetchState(true))}/>
                <LocalAsrStatus precise={true} status={self.data["final_asr"].clone()} ui_language={self.ui_language.clone()} enabled={self.online&&self.busy.is_empty()} on_refresh={ctx.link().callback(|_|Msg::FetchState(true))}/>
            </details>
            </div>
        </details></div>}
    }

    fn render_activity(&self, ctx: &Context<Self>, state: &Value) -> Html {
        let finished = self.data["session"]["status"] == "finished";
        html! {<article class="panel activity-panel">
            {if self.continuation.uncertain {html!{<div class="continuation-review" role="status"><strong>{if self.ui_language=="en"{"Continuation result needs checking"}else{"续做结果待核对"}}</strong><p>{if self.ui_language=="en"{"The request may have completed. Refresh to check, or retry this same request. No second activity will be requested automatically."}else{"请求可能已经完成。可以刷新核对，或重试同一次请求；不会自动另建一次活动。"}}</p><div class="history-controls"><button class="text-button" onclick={ctx.link().callback(|_|Msg::FetchState(true))}>{if self.ui_language=="en"{"Refresh current activity"}else{"刷新核对当前活动"}}</button><button class="secondary" disabled={self.continuation.in_flight||!self.busy.is_empty()} onclick={ctx.link().callback(|_|Msg::RetryContinuation)}>{if self.ui_language=="en"{"Check this request again"}else{"核对同一次续做"}}</button></div></div>}}else{Html::default()}}
            <ActivitySummary state={state.clone()} ui_language={self.ui_language.clone()} finished={finished} pending_step_needs_confirmation={pending_needs_confirmation(&self.data["session"])}/>
            <ExperimentComparison owner={self.comparison_owner()} ui_language={self.ui_language.clone()} reading_level={self.reading_level.clone()}
                writable={!finished} enabled={self.busy.is_empty()&&!self.activity_transition_pending()&&!self.recording&&!self.record_start_pending&&!self.transcribing}
                save_enabled={!self.comparison_saving()} generation={self.data["session"]["generation"].as_u64().unwrap_or(0)} focus_revision={self.comparison_focus}
                on_guard={ctx.link().callback(Msg::ComparisonChanged)} on_settled={ctx.link().callback(|(learner,session)|Msg::ComparisonSettled(learner,session))}/>
            <div class="activity-footer">{icon("memory")}<span>{if finished{t(&self.ui_language,"本次学习记录已保存")}else{t(&self.ui_language,"当前活动会自动保存")}}<br/><small>{t(&self.ui_language,if finished{"下次从相关记忆继续，并重新确认搭建。"}else{"完成尝试后，可结束并保存本次活动。"})}</small></span>{if self.has_session()&&!finished{html!{<button class="text-button" disabled={!self.busy.is_empty()||self.comparison_saving()} onclick={click(ctx,Action::Finish)}>{t(&self.ui_language,"结束并保存")}</button>}}else{Html::default()}}</div>
            <ActivityHistory learner_id={text(&self.learner()["learner_id"])} learner_epoch={self.learner()["epoch"].as_i64().unwrap_or(-1)} current_session={self.session_id()} current_finished={finished} ui_language={self.ui_language.clone()} busy={!self.busy.is_empty()||self.continuation.request.is_some()} on_view={ctx.link().callback(Msg::ViewActivity)} on_continue={ctx.link().callback(|activity|Msg::Action(Action::ContinueActivity(activity)))}/>
        </article>}
    }

    fn render_memories(&self, ctx: &Context<Self>, state: &Value) -> Html {
        let recall = list(&self.data["session"]["recalled_memories"]);
        let preferences = &state["session_preferences"];
        let language = &self.ui_language;
        let learner_id = text(&self.learner()["learner_id"]);
        let namespace = if self.simulator() {
            "simulator"
        } else {
            "real"
        };
        // Any edit/delete dialog or mutation revokes already fetched details,
        // including requests which finish while a profile switch is pending.
        let evidence_enabled =
            self.busy.is_empty() && self.edit_memory.is_none() && self.delete_target.is_none();
        let count = if language == "en" {
            format!("{} records to review", self.memories.len())
        } else {
            format!("{} 条可检查记录", self.memories.len())
        };
        html! {
            <details class="panel memory-panel">
                <summary><span>{icon("memory")}<strong>{t(language,"学习记忆")}</strong><small>{count}</small></span>{icon("chevron")}</summary>
                <div class="details-content">
                    <div class="memory-explanation"><span>{if language=="en" {"Memories retain sourced reports, explicit preferences and unfinished experiments."} else {"慢记忆保存有出处的报告、明确偏好与未完成实验。"}}</span><button class="text-button" disabled={!self.has_session() || !self.busy.is_empty()} onclick={ctx.link().callback(|_|Msg::LoadMemories)}>{t(language,"刷新")}</button></div>
                    <LocalMemoryStatus status={self.data["local_memory"].clone()} learner_id={learner_id.clone()}
                        learner_epoch={self.learner()["epoch"].as_i64().unwrap_or(-1)} namespace={namespace.to_owned()}
                        session_id={self.session_id()} ui_language={language.clone()} enabled={self.online&&evidence_enabled&&!self.activity_transition_pending()}
                        on_refresh={ctx.link().callback(|_|Msg::FetchState(false))}/>
                    <HelpPreference learner_id={learner_id.clone()} namespace={namespace.to_owned()}
                        session_id={self.session_id()} epoch={self.learner()["epoch"].as_i64().unwrap_or(-1)}
                        generation={self.data["session"]["generation"].as_i64().unwrap_or(-1)} ui_language={language.clone()}
                        enabled={self.online&&evidence_enabled&&!self.input_disabled()&&!self.activity_transition_pending()&&!self.voice_loop.active()}
                        on_saved={ctx.link().batch_callback(|_|vec![Msg::FetchState(false),Msg::LoadMemories])}/>
                    <MemoryReview learner_id={learner_id.clone()} namespace={namespace.to_owned()}
                        session_id={self.session_id()} epoch={self.learner()["epoch"].as_i64().unwrap_or(-1)}
                        generation={self.data["session"]["generation"].as_i64().unwrap_or(-1)}
                        state_revision={self.data["session"]["state_revision"].as_i64().unwrap_or(-1)} ui_language={language.clone()}
                        enabled={self.online&&evidence_enabled&&!self.activity_transition_pending()}
                        on_retained={ctx.link().batch_callback(|_|vec![Msg::FetchState(false),Msg::LoadMemories])}/>
                    {if self.memories.is_empty() {html! {
                        <p class="empty-note">{if language=="en" {"No memories are available for this profile. Records appear after their sources have been checked."} else {"当前档案还没有可用的慢记忆。来源校验通过后，相关记录会出现在这里。"}}</p>
                    }} else {html! {for self.memories.iter().map(|memory| {
                        let id=text(&memory["memory_id"]);
                        let edit=memory.clone();
                        let delete=memory.clone();
                        let source=if namespace=="simulator" {"simulated"} else {memory["source_type"].as_str().unwrap_or("")};
                        let sources=list(&memory["source_event_ids"]).len();
                        let scope=scope_label(language,memory["scope"].as_str().unwrap_or(""));
                        let summary=if language=="en" {format!("{scope} · {sources} sources")} else {format!("{scope} · {sources} 条来源证据")};
                        html! {
                            <div class="memory-row" key={format!("{learner_id}:{id}")}>
                                <div>
                                    <span class="memory-kind">{kind_label(language,memory["kind"].as_str().unwrap_or(""))}</span>
                                    <span class={classes!("source", (source=="simulated").then_some("simulation"))}>{source_label(language,source)}</span>
                                    {if self.edit_memory.as_ref()==Some(&id) {html! {
                                        <div class="memory-edit">
                                            <textarea disabled={!self.busy.is_empty()} aria-label={if language=="en" {"Correct memory content"} else {"更正记忆内容"}} value={self.edit_text.clone()} maxlength="800" oninput={textarea(ctx,Field::EditMemory)}/>
                                            <button class="primary" disabled={self.edit_text.trim().is_empty()||!self.busy.is_empty()} onclick={click(ctx,Action::SaveMemory)}>{t(language,"保存更正")}</button>
                                            <button class="text-button" disabled={!self.busy.is_empty()} onclick={ctx.link().callback(|_|Msg::CancelEdit)}>{t(language,"取消")}</button>
                                        </div>
                                    }} else {html! {<p>{text(&memory["content"])}</p>}}}
                                    <small>{summary}</small>
                                    <MemoryEvidence learner_id={learner_id.clone()} namespace={namespace.to_owned()}
                                        learner_epoch={self.learner()["epoch"].as_i64().unwrap_or(-1)}
                                        memory={memory.clone()} ui_language={language.clone()} reading_level={self.reading_level.clone()} enabled={evidence_enabled}/>
                                </div>
                                <div class="memory-actions">
                                    <button class="text-button" disabled={!self.busy.is_empty()} onclick={ctx.link().callback(move |_|Msg::Edit(edit.clone()))}>{t(language,"更正")}</button>
                                    <button class="icon-button danger" aria-label={if language=="en" {"Forget this memory"} else {"忘记这条记忆"}} disabled={!self.busy.is_empty()} onclick={ctx.link().callback(move |_|Msg::Delete(delete.clone()))}>{icon("trash")}</button>
                                </div>
                            </div>
                        }
                    })}}}
                    <details class="snapshot"><summary>{"本轮快记忆与召回条目"}</summary><dl><dt>{"上一条已投递提示"}</dt><dd>{fallback(&state["last_delivered_hint"],"暂无")}</dd><dt>{"本轮偏好"}</dt><dd>{preference_text(preferences)}</dd><dt>{"本轮召回"}</dt><dd>{if recall.is_empty(){"尚无相关历史".into()}else{recall.iter().map(|m|text(&m["content"])).collect::<Vec<_>>().join("；")}}</dd></dl></details>
                </div>
            </details>
        }
    }

    fn render_diagnostics(&self) -> Html {
        let metrics = &self.data["metrics"];
        let audio = &self.data["audio"];
        let kimi = &self.data["kimi"];
        let teaching = &self.data["teaching"];
        let en = self.ui_language == "en";
        let teaching_detail = match teaching["provider"].as_str() {
            Some("deepseek_flash") => format!(
                "DeepSeek Flash · {} · {}",
                fallback(&teaching["thinking"], "unknown"),
                if teaching["available"] == true {
                    if en {
                        "Locally prepared; remote availability is checked by actual requests".into()
                    } else {
                        "本地已准备；远端可用性由实际请求确认".into()
                    }
                } else {
                    fallback(
                        &teaching["error"],
                        if en { "Unavailable" } else { "暂不可用" },
                    )
                }
            ),
            Some("invalid") => fallback(
                &teaching["error"],
                if en {
                    "Invalid teaching provider configuration"
                } else {
                    "教学模型配置无效"
                },
            ),
            _ => format!(
                "Kimi Code · {}",
                if kimi["available"] == true {
                    if en {
                        "Verified tool-free provider".into()
                    } else {
                        "无工具后端已核验".into()
                    }
                } else {
                    fallback(&kimi["error"], if en { "Unavailable" } else { "暂不可用" })
                }
            ),
        };
        let voice_ready = !list(&audio["voices"]).is_empty();
        let values = vec![
            (
                "检测处理帧率",
                self.data["camera"]["fps"]
                    .as_f64()
                    .map(|v| format!("{v:.1} fps"))
                    .unwrap_or_else(|| "未知".into()),
            ),
            (
                "服务端帧龄",
                milliseconds(&self.data["camera"]["frame_age_ms"]),
            ),
            ("教学请求数", fallback(&metrics["requests"], "未知")),
            ("模型调用耗时", milliseconds(&metrics["last_latency_ms"])),
            (
                "供应商 token 用量",
                fallback(&metrics["reported_tokens"], "未知"),
            ),
            (
                "本地 ASR 耗时",
                milliseconds(audio.get("asr_ms").unwrap_or(&audio["last_asr_ms"])),
            ),
        ];
        html! {<details class="panel diagnostics-panel"><summary><span>{icon("activity")}<strong>{t(&self.ui_language,"运行状态")}</strong><small>{if en {"Capabilities and latency"} else {"实际能力与延迟"}}</small></span>{icon("chevron")}</summary><div class="details-content"><div class="metrics-grid">{for values.into_iter().map(|(label,value)|html!{<div><small>{label}</small><strong>{value}</strong></div>})}</div><dl class="capabilities"><dt>{if en {"Teaching model"} else {"教学模型"}}</dt><dd>{teaching_detail}</dd><dt>{"Kimi Code"}</dt><dd>{format!("{} · {}",fallback(&kimi["version"],"版本待探测"),if kimi["available"]==true{"无工具后端可用".into()}else{fallback(&kimi["error"],"后端尚未可用")})}</dd><dt>{"本地语音"}</dt><dd>{format!("{} / {}",if audio["asr_ready"]==true{fallback(&audio["asr_provider"],"本地 ASR 可用")}else{"本地 ASR 模型待准备".into()},if voice_ready{fallback(&audio["tts_provider"],"本地 TTS 可用")}else{"本地声音待探测".into()})}</dd><dt>{"情绪模型"}</dt><dd>{"本轮关闭"}</dd><dt>{"积木自动执行"}</dt><dd>{"未启用"}</dd><dt>{"界面运行时"}</dt><dd>{"Rust · WebAssembly"}</dd></dl></div></details>}
    }

    fn render_session_preferences(&self, ctx: &Context<Self>) -> Html {
        let Some(notice) = crate::i18n::session_preference_text(
            &self.ui_language,
            &self.data["session"]["state"]["session_preferences"],
        ) else {
            return Html::default();
        };
        let learner = self.learner();
        let restore = json!({"help_style":learner["help_style"].as_str().unwrap_or("one_step")});
        html! {
            <aside class="session-preference-notice" aria-live="polite">
                <strong>{t(&self.ui_language,"本次活动偏好")}</strong>
                <p class="session-preference-content">{notice}</p>
                <p>{t(&self.ui_language,"本次偏好会影响当前指导。修改提示方式或恢复档案设置可清除本次偏好。")}</p>
                <button class="text-button"
                    disabled={!self.has_session() || self.data["session"]["status"] == "finished" || !self.busy.is_empty()}
                    onclick={click(ctx,Action::Settings(restore))}>
                    {t(&self.ui_language,"恢复档案提示方式")}
                </button>
            </aside>
        }
    }

    fn render_teacher(&self, ctx: &Context<Self>) -> Html {
        let learner = self.learner();
        let lid = text(&learner["learner_id"]);
        let simulator = self.simulator();
        let no_session = !self.has_session();
        let inactive = no_session
            || self.data["session"]["status"] == "finished"
            || self.activity_transition_pending();
        let learner_name = fallback(&learner["name"], "当前档案");
        html! {<section class="panel teacher-panel"><div class="teacher-header"><div>{icon("settings")}<h2>{t(&self.ui_language,"教师工作台")}</h2></div><button class="icon-button" aria-label="收起教师工作台" onclick={ctx.link().callback(|_|Msg::ToggleTeacher)}>{icon("close")}</button></div><div class="teacher-grid">
            <div><h3>{t(&self.ui_language,"学习者档案")}</h3><label class="field-label">{t(&self.ui_language,"选择档案")}<SelectControl value={lid} disabled={!self.busy.is_empty()} onchange={ctx.link().callback(|event:Event|Msg::Action(Action::Switch(event.target_unchecked_into::<HtmlSelectElement>().value())))}><option value="">{t(&self.ui_language,"选择匿名档案")}</option>{for list(&self.data["learners"]).iter().map(|l|html!{<option value={text(&l["learner_id"])}>{format!("{}{}",text(&l["name"]),if l["namespace"]=="simulator"{if self.ui_language=="en"{" · Simulation"}else{" · 模拟"}}else{""})}</option>})}</SelectControl></label><div class="create-profile"><input aria-label={t(&self.ui_language,"新匿名档案名称")} placeholder={t(&self.ui_language,"新档案名称，如 学习者 A")} value={self.name.clone()} maxlength="40" oninput={input(ctx,Field::Name)}/><SelectControl label={t(&self.ui_language,"档案数据空间")} value={self.namespace.clone()} onchange={select(ctx,Field::Namespace)}><option value="real">{t(&self.ui_language,"真实记录")}</option><option value="simulator">{t(&self.ui_language,"独立模拟")}</option></SelectControl><button class="secondary" disabled={self.name.trim().is_empty()||!self.busy.is_empty()} onclick={click(ctx,Action::Create)}>{icon("plus")}{t(&self.ui_language,"创建")}</button></div><p class="fine-print">{t(&self.ui_language,"只使用匿名名称。视觉目标由教师另行选择。")}</p>{if !no_session{html!{<div class="session-actions"><button class="text-button" disabled={!self.busy.is_empty()} onclick={click(ctx,Action::NewActivity)}>{t(&self.ui_language,"开启新的学习活动")}</button>{if self.data["session"]["status"]!="finished"{html!{<button class="text-button" disabled={!self.busy.is_empty()} onclick={click(ctx,Action::Finish)}>{t(&self.ui_language,"结束本次活动并保存")}</button>}}else{Html::default()}}</div>}}else{Html::default()}}</div>
            <div class="module-confirmation" ref={self.module_confirmation.clone()} tabindex="-1"><ModuleInventory ui_language={self.ui_language.clone()} modules={self.inventory.modules.clone()} modules_dirty={self.inventory.modules_dirty}
                report={self.inventory.report.clone()} notice={self.report_notice.clone()} source={self.module_source.clone()} simulated={simulator} inactive={inactive} busy={!self.busy.is_empty()}
                can_save={self.inventory.payload(&self.module_source).is_some()} on_module={ctx.link().callback(|(id,value)|Msg::Module(id,value))}
                on_report={ctx.link().callback(|value|Msg::Field(Field::Report,value))} on_source={ctx.link().callback(|value|Msg::Field(Field::ModuleSource,value))}
                on_confirm={click(ctx,Action::ConfirmInventory)} on_save={click(ctx,Action::Modules)}/></div>
            <div><h3>{t(&self.ui_language,"教学与本地音频")}</h3><label class="field-label">{t(&self.ui_language,"教学语言")}<SelectControl value={self.language.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::Language)}><option value="zh">{"中文"}</option><option value="en">{"English"}</option></SelectControl></label><label class="field-label">{t(&self.ui_language,"档案提示方式")}<SelectControl value={self.help_style.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::HelpStyle)}><option value="one_step">{t(&self.ui_language,"一次一步")}</option><option value="direct">{t(&self.ui_language,"直接解释")}</option><option value="guided">{t(&self.ui_language,"适量提示")}</option></SelectControl></label>{self.render_session_preferences(ctx)}<label class="field-label">{t(&self.ui_language,"阅读表达")}<SelectControl value={self.reading_level.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::ReadingLevel)}><option value="plain">{t(&self.ui_language,"短句、少术语")}</option><option value="standard">{t(&self.ui_language,"常规说明")}</option><option value="technical">{t(&self.ui_language,"术语与完整解释")}</option></SelectControl></label><label class="field-label">{t(&self.ui_language,"朗读语速")}<SelectControl value={self.speech_speed.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::SpeechSpeed)}><option value="slow">{t(&self.ui_language,"较慢")}</option><option value="normal">{t(&self.ui_language,"正常")}</option><option value="fast">{t(&self.ui_language,"较快")}</option></SelectControl></label><p class="fine-print">{t(&self.ui_language,"按本次需要选择表达方式，不根据年龄推断能力。")}</p><label class="field-label">{t(&self.ui_language,"麦克风")}<SelectControl value={self.microphone_id.clone()} onchange={select(ctx,Field::Microphone)}><option value="">{t(&self.ui_language,"系统默认输入")}</option>{for self.microphones.iter().filter(|d|d["input_channels"].as_u64()!=Some(0)).map(|d|{let id=d.get("device_id").or_else(||d.get("index")).map(text).unwrap_or_default();html!{<option value={id}>{fallback(&d["name"],"麦克风")}</option>}})}</SelectControl></label><button class="text-button" onclick={click(ctx,Action::SpeechStop)}>{icon("volume")}{t(&self.ui_language,"停止本地播报")}</button></div>
            <div class="privacy-card">{icon("shield")}<h3>{t(&self.ui_language,"可检查，也可忘记")}</h3><p>{self.teaching_privacy_notice()}</p><button class="danger-link" disabled={no_session} onclick={ctx.link().callback(move |_|Msg::Delete(json!({"all":true,"content":learner_name})))}>{icon("trash")}{t(&self.ui_language,"清除当前档案学习数据")}</button></div>
        </div></section>}
    }

    fn render_delete(&self, ctx: &Context<Self>) -> Html {
        let Some(target) = &self.delete_target else {
            return Html::default();
        };
        html! {<ConfirmForget all={target["all"]==true} content={text(&target["content"])} ui_language={self.ui_language.clone()} busy={!self.busy.is_empty()}
        on_cancel={ctx.link().callback(|_|Msg::CancelDelete)} on_confirm={click(ctx,Action::ConfirmDelete)}/>}
    }
}

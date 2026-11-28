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
    home::{ActivityGallery, RobotHome},
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
    navigation::AppView,
    operation_notice::OperationNotice,
    parent_center::ParentCenter,
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
            "M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3 M5 11v1a7 7 0 0 0 14 0v-1 M12 19v3 M9 22h6"
        }
        "home" => "M3 10.8 12 3l9 7.8 M5.5 9.5V21h13V9.5 M9.2 21v-6.5h5.6V21",
        "grid" => "M5 3h4a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2 M15 3h4a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2h-4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2 M5 13h4a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2 M15 13h4a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2h-4a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2",
        "compass" => "M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0 M15.5 8.5l-2 5-5 2 2-5z",
        "hand" => "M18 11V6a2 2 0 0 0-4 0 M14 10V4a2 2 0 0 0-4 0v2 M10 10.5V6a2 2 0 0 0-4 0v8 M18 8a2 2 0 1 1 4 0v6a8 8 0 0 1-8 8h-2c-2.8 0-4.5-.86-5.97-2.34L3.2 16.3a2 2 0 0 1 2.6-3L8 16",
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
        "study" => "M4 20l4-1L20 7l-3-3L5 16z M14 6l3 3 M12 20h8",
        "palette" => "M12 3a9 9 0 1 0 0 18h1.5a2 2 0 0 0 0-4H12a1.5 1.5 0 0 1 0-3h4a5 5 0 0 0 5-5c0-3.5-4-6-9-6z M7 8h.01 M11 6h.01 M15.5 7.5h.01 M6.5 12h.01",
        "chat" => "M20 11.5a7.5 7.5 0 0 1-8 7.5 9 9 0 0 1-4-.9L3 20l1.8-4.2A7.5 7.5 0 1 1 20 11.5z M8 11.5h.01 M12 11.5h.01 M16 11.5h.01",
        "heart" => "M20.5 5.5a5 5 0 0 0-7.1 0L12 6.9l-1.4-1.4a5 5 0 0 0-7.1 7.1L12 21l8.5-8.4a5 5 0 0 0 0-7.1z",
        "activity" => "M2 12h4l3-9 5 18 3-9h5",
        "flag" => "M4 22V3 M4 3c5-4 9 4 16 0v11c-7 4-11-4-16 0",
        "radio" => {
            "M9 8a6 6 0 0 0 0 8 M15 8a6 6 0 0 1 0 8 M5 4a12 12 0 0 0 0 16 M19 4a12 12 0 0 1 0 16 M11 11h2v2h-2z"
        }
        _ => {
            "M5 5h14v14H5z M9 9h6v6H9z M2 8h3 M2 16h3 M19 8h3 M19 16h3 M8 2v3 M16 2v3 M8 19v3 M16 19v3"
        }
    };
    html! {<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={path}/></svg>}
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

fn navigate_click(ctx: &Context<App>, target: AppView) -> Callback<MouseEvent> {
    ctx.link().batch_callback(move |event: MouseEvent| {
        event.prevent_default();
        vec![Msg::Navigate(target)]
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
            Some("qwen_omni") => if self.ui_language == "en" {
                "Qwen Omni · Local multimodal"
            } else {
                "Qwen Omni · 本地多模态"
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
            Some("qwen_omni") => if self.ui_language == "en" {
                "Review and edit the transcript before sending. With local Omni, teaching text, explicitly submitted images and recordings stay on this computer."
            } else { "转写后可修改再发送。使用本机 Omni 时，教学文字、显式提交的图片与录音仅在本机处理。" }.into(),
            Some("invalid") => if self.ui_language == "en" {
                "You can still type and record locally. Correct the teaching provider configuration before requesting a reply."
            } else { "仍可输入和在本机录音。请求教学回复前，请先修正教学模型配置。" }.into(),
            _ => t(&self.ui_language, "转写后可修改再发送。语音在本机处理，教学文字会发送到 Kimi。").into(),
        }
    }
    fn teaching_privacy_notice(&self) -> String {
        if self.data["teaching"]["provider"].as_str() == Some("qwen_omni") {
            if self.ui_language == "en" {
                "Raw video, face crops and recordings are not saved by default. With local Omni, teaching text, explicitly submitted images and recordings are processed on this computer. When background memory is enabled, its source text is processed by Kimi. Teachers can correct memories or clear this profile's data."
            } else {
                "默认不保存原始视频、脸部裁剪或录音。使用本机 Omni 时，教学文字、显式提交的图片与录音在本机处理；启用后台记忆时，其来源文本仍通过 Kimi 处理。教师可纠正记忆或清除此档案的数据。"
            }.into()
        } else if matches!(
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
        let camera_open = matches!(
            self.data["camera"]["state"].as_str(),
            Some("running" | "opening" | "connecting" | "reconnecting")
        ) || self.data["ptz"]["external_yolo_enabled"] == true;
        let en = self.ui_language == "en";
        let delete_open = self.delete_target.is_some();
        let edit_open = self.edit_memory.is_some() && self.busy.is_empty();
        let teacher_open = self.teacher;
        let parent_clear_name = fallback(&learner["name"], t(&self.ui_language, "当前档案"));
        let mut parent_digest = self.data["parent_digest"].clone();
        if simulator {
            parent_digest["child_name"] = json!(if en { "Child" } else { "孩子" });
        }
        let escape = ctx.link().batch_callback(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                event.prevent_default();
                if delete_open {
                    Some(Msg::CancelDelete)
                } else if edit_open {
                    Some(Msg::CancelEdit)
                } else if teacher_open {
                    Some(Msg::ToggleTeacher)
                } else {
                    Some(Msg::Action(Action::Stop))
                }
            } else {
                None
            }
        });
        html! {<div class={classes!("app-shell","demo-shell",has_learner.then_some("has-learner"),self.has_session().then_some("has-session"),self.teacher.then_some("has-teacher"),session["continued_from"].is_object().then_some("has-continuation"))} onkeydown={escape}>
            <a class="skip-link" href="#/voice" onclick={navigate_click(ctx,AppView::Voice)}>{if en{"Skip to voice companion"}else{"跳到语音对话"}}</a>
            <div class="ambient-scene" aria-hidden="true"><i class="ambient-orb orb-blue"/><i class="ambient-orb orb-gold"/><i class="ambient-orb orb-green"/></div>
            <header class={classes!("topbar",(self.view==AppView::Home).then_some("home-topbar"))}>
                <a class="brand" href="#/home" aria-label={t(&self.ui_language,"AI Tutor 首页")} onclick={navigate_click(ctx,AppView::Home)}>
                    <span class="brand-mark"><svg width="36" height="36" viewBox="0 0 40 40" fill="none" aria-hidden="true"><path d="M20 6v4" stroke="#ADC5D6" stroke-width="1.8" stroke-linecap="round"/><circle cx="20" cy="5" r="2.1" fill="#D9AE5F"/><rect x="6.5" y="10.5" width="27" height="23" rx="9" fill="#FFFCF3"/><rect x="9.5" y="14.5" width="21" height="14" rx="6" fill="#8FB2C9"/><rect x="10.5" y="15.5" width="19" height="12" rx="5" fill="#17384F"/><path d="M15.5 19.5v3m9-3v3" stroke="#A9F1ED" stroke-width="2.6" stroke-linecap="round"/><path d="M18 30.5q2 1.5 4 0" stroke="#7398B2" stroke-width="1.5" stroke-linecap="round"/></svg></span>
                    <span><strong>{"AI Tutor"}<span class="brand-dot"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 1.5 9.7 6.3 14.5 8 9.7 9.7 8 14.5 6.3 9.7 1.5 8 6.3 6.3Z" fill="currentColor"/></svg></span></strong><small>{if en{"LEARNING AND EVERYDAY COMPANIONSHIP"}else{"儿童学习与日常陪伴"}}</small></span>
                </a>
                <div class="top-actions"><span class={classes!("connection",self.online.then_some("connected"))}><i/>{if self.online{if en{"Connected locally"}else{"本机已连接"}}else{if en{"Connecting…"}else{"正在连接…"}}}</span><label class="ui-language-picker"><span class="visually-hidden">{"界面 / Interface"}</span><SelectControl label="Interface language / 界面语言" value={self.ui_language.clone()} disabled={!self.busy.is_empty()} onchange={select(ctx,Field::UiLanguage)}><option value="zh">{"中文"}</option><option value="en">{"English"}</option></SelectControl></label><button ref={self.teacher_toggle.clone()} class={classes!("teacher-toggle",self.teacher.then_some("active"))} aria-label={if en{"Parent center"}else{"家长中心"}} aria-expanded={self.teacher.to_string()} aria-controls="teacher-settings" onclick={ctx.link().callback(|_|Msg::ToggleTeacher)}>{icon("heart")}<span>{if en{"Parents"}else{"家长"}}</span>{icon("chevron")}</button></div>
            </header>
            <main class={match self.view{AppView::Home=>"app-main home-view",AppView::Voice=>"app-main voice-view",AppView::Activities=>"app-main activities-view",AppView::Conversation=>"app-main conversation-view",AppView::Records=>"app-main records-view"}}>
            {if !self.error.is_empty(){html!{<OperationNotice ui_language={self.ui_language.clone()} detail={self.error.clone()} on_dismiss={ctx.link().callback(|_|Msg::ClearError)}/>}}else{Html::default()}}
            {if !has_learner&&!self.teacher{html!{<div class="onboard"><span><strong>{t(&self.ui_language,"从一个匿名档案开始。")}</strong>{t(&self.ui_language,"教师选择档案后，Tutor 才会记录和召回这位学习者的进展。")}</span><button class="primary" onclick={ctx.link().callback(|_|Msg::ToggleTeacher)}>{t(&self.ui_language,"设置学习者")}{icon("arrow")}</button></div>}}else{Html::default()}}
            {if self.teacher{html!{<div id="teacher-settings" class="settings-anchor parent-center-anchor">
                <ParentCenter parent_digest={parent_digest} ui_language={self.ui_language.clone()}
                    help_style={self.help_style.clone()} reading_level={self.reading_level.clone()} speech_speed={self.speech_speed.clone()}
                    settings_disabled={!self.has_session()||!self.busy.is_empty()} clear_disabled={!self.has_session()}
                    privacy_notice={self.teaching_privacy_notice()}
                    on_close={ctx.link().callback(|_|Msg::ToggleTeacher)}
                    on_help_style={select(ctx,Field::HelpStyle)} on_reading_level={select(ctx,Field::ReadingLevel)} on_speech_speed={select(ctx,Field::SpeechSpeed)}
                    on_clear_data={ctx.link().callback(move |_|Msg::Delete(json!({"all":true,"content":parent_clear_name})))}
                    on_records={ctx.link().batch_callback(|event: MouseEvent| {
                        event.prevent_default();
                        vec![Msg::ToggleTeacher, Msg::Navigate(AppView::Records)]
                    })}/>
                <details class="parent-teacher-tools">
                    <summary>{icon("study")}<span><strong>{if en{"Teacher tools"}else{"教师工具"}}</strong><small>{if en{"A separate workspace for teaching preparation and profile management"}else{"用于教学准备与档案管理的独立工作区"}}</small></span>{icon("chevron")}</summary>
                    <div class="parent-teacher-tools-content">{self.render_teacher(ctx)}</div>
                </details>
            </div>}}else{Html::default()}}
            {match self.view {
                AppView::Home => html!{<>
                    <RobotHome ptz={self.data["ptz"].clone()} camera={self.data["camera"].clone()} audio={self.data["audio"].clone()} ui_language={self.ui_language.clone()} generating={matches!(session["status"].as_str(),Some("generating"|"running"))} has_session={self.has_session()} learner_name={name} simulator={simulator} on_chat={ctx.link().callback(|_|Msg::FocusVoice)} on_activities={navigate_click(ctx,AppView::Activities)} on_profile={ctx.link().callback(|_|Msg::ToggleTeacher)}/>
                    {if camera_open{html!{<aside class="home-active-camera" aria-label={if en{"Active camera controls"}else{"使用中的摄像头控制"}}>{self.render_camera_dock(ctx,true)}</aside>}}else{Html::default()}}
                </>},
                AppView::Voice => self.render_voice_companion(ctx,camera_open),
                AppView::Activities => html!{<ActivityGallery ui_language={self.ui_language.clone()} disabled={self.input_disabled()||!self.busy.is_empty()||!self.input.trim().is_empty()} has_draft={!self.input.trim().is_empty()} selected={crate::companion_activity::selected_activity(&self.input,en)} on_select={ctx.link().callback(Msg::ChooseActivity)}/>},
                AppView::Conversation => self.render_conversation_workspace(ctx,camera_open),
                AppView::Records => self.render_records_page(ctx,state),
            }}
            <footer><span>{"AI Tutor"}<span class="footer-sparkle">{"✦"}</span>{if en{"Learning and everyday companion for children"}else{"儿童学习与日常陪伴"}}</span><span>{if en{"Local camera & voice · You control your learning records"}else{"本机画面与语音 · 学习记录由你管理"}}</span></footer></main>
            {if !self.busy.is_empty(){html!{<div class="busy-toast" role="status"><span class="spin">{icon("refresh")}</span>{t(&self.ui_language,&self.busy).to_owned()}</div>}}else{Html::default()}}
            {self.render_delete(ctx)}
        </div>}
    }

    fn render_page_header(
        &self,
        title_zh: &'static str,
        title_en: &'static str,
        subtitle_zh: &'static str,
        subtitle_en: &'static str,
    ) -> Html {
        let en = self.ui_language == "en";
        let name = fallback(&self.learner()["name"], t(&self.ui_language, "尚未选择"));
        html! {<div class="page-heading"><div><h1>{if en{title_en}else{title_zh}}</h1><p>{if en{subtitle_en}else{subtitle_zh}}</p></div><div class="page-learner"><span>{name.chars().next().unwrap_or('?')}</span><strong>{name}</strong></div></div>}
    }

    fn render_conversation_workspace(&self, ctx: &Context<Self>, camera_open: bool) -> Html {
        let session = &self.data["session"];
        let state = &session["state"];
        let goal = text(&state["goal"]);
        let en = self.ui_language == "en";
        html! {<>
            {self.render_page_header("对话工作台","Conversation","当前活动与消息。","Current activity and messages.")}
            <section class="workspace-grid conversation-workspace" id="conversation-workspace" aria-label={if en{"Companion workspace"}else{"陪伴工作台"}}>
                {self.render_dialogue(ctx)}
                <aside class="companion-column" aria-label={if en{"This activity and devices"}else{"本次陪伴与设备"}}>
                    <div class="stage-utilities">
                        <section class="current-mission" id="current-mission"><span class="mission-icon">{icon("flag")}</span><div><small>{if session["status"]=="finished"{if en{"ACTIVITY FINISHED"}else{"本次活动已结束"}}else{if en{"CURRENT ACTIVITY"}else{"当前活动"}}}</small><h3>{if goal.is_empty(){if !session["continued_from"].is_null(){if en{"Review the previous activity and choose how to continue.".to_owned()}else{"查看上次活动并选择继续方式。".to_owned()}}else{if en{"Choose an activity, or enter what you want to say.".to_owned()}else{"选择一项活动，或输入你想说的内容。".to_owned()}}}else{goal}}</h3>
                            <ContinuationSource session={session.clone()} ui_language={self.ui_language.clone()} on_view={ctx.link().callback(Msg::ViewActivity)}/>
                            {if self.has_session()&&session["status"]!="finished"{html!{<details class="mission-modules" open={state["requires_inventory_confirmation"]==true}><summary>{icon("circuit")}<span>{if en{"Experiments and building"}else{"实验与搭建"}}</span>{icon("chevron")}</summary><div class="mission-preparation">
                                {if state["requires_inventory_confirmation"]==true{html!{<p class="mission-readiness">{if en{"Today's modules and setup still need confirmation."}else{"今天的模块与设置还需要重新确认。"}}</p>}}else{Html::default()}}
                                <div class="mission-actions"><button class="secondary" disabled={!self.busy.is_empty()||self.activity_transition_pending()} onclick={ctx.link().callback(|_|Msg::OpenModuleConfirmation)}>{icon("circuit")}{if state["requires_inventory_confirmation"]==true{if en{"Confirm today's modules"}else{"确认今天的模块"}}else{if en{"View current modules"}else{"查看当前模块"}}}</button><button class="text-button" disabled={self.input_disabled()||!self.busy.is_empty()} onclick={ctx.link().callback(|_|Msg::FocusTodayPlan)}>{icon("book")}{if en{"Write today's plan"}else{"输入今天的计划"}}</button></div>
                            </div></details>}}else{Html::default()}}
                            <a href="#/records" onclick={navigate_click(ctx,AppView::Records)}>{if en{"View activity records"}else{"查看活动记录"}}{icon("arrow")}</a></div></section>
                        {self.render_camera_dock(ctx,camera_open)}
                    </div>
                </aside>
            </section>
        </>}
    }

    fn render_camera_dock(&self, ctx: &Context<Self>, open: bool) -> Html {
        let en = self.ui_language == "en";
        html! {<details class="camera-dock" open={open}><summary><span class="dock-icon">{icon("camera")}</span><span><strong>{if en{"A little window into your world"}else{"看见你的世界"}}</strong><small>{if open{if en{"Local camera · In use"}else{"本机摄像头 · 使用中"}}else{if en{"Local camera · Off until you need it"}else{"本机摄像头 · 需要时再开启"}}}</small></span>{icon("chevron")}</summary><CameraPreview compact={true} ui_language={self.ui_language.clone()} camera={self.data["camera"].clone()} ptz={self.data["ptz"].clone()} cameras={self.cameras.clone()} camera_id={self.camera_id.clone()} busy={!self.busy.is_empty()} on_action={ctx.link().callback(Msg::Action)} on_camera={ctx.link().callback(|id|Msg::Field(Field::Camera,id))}/></details>}
    }

    fn render_records_page(&self, ctx: &Context<Self>, state: &Value) -> Html {
        let en = self.ui_language == "en";
        html! {<>
            {self.render_page_header("学习记录","Learning records","活动、实验与学习记忆。","Activities, experiments and learning memories.")}
            <section class="learning-records" id="learning-records" aria-label={if en{"Learning records"}else{"学习记录"}}><details class="panel activity-disclosure" open={self.has_session()}><summary><span>{icon("history")}<strong>{if en{"Activities"}else{"活动记录"}}</strong><small>{if en{"Current · experiments · history"}else{"当前 · 实验 · 历史"}}</small></span>{icon("chevron")}</summary>{self.render_activity(ctx,state)}</details><div class="lower-grid">{self.render_memories(ctx,state)}{self.render_diagnostics()}</div></section>
        </>}
    }

    fn render_voice_companion(&self, ctx: &Context<Self>, camera_open: bool) -> Html {
        let session = &self.data["session"];
        let turns = list(&session["turns"]);
        let en = self.ui_language == "en";
        let generating = matches!(session["status"].as_str(), Some("generating" | "running"));
        let can_look = self.data["teaching"]["provider"] == "qwen_omni"
            && self.data["teaching"]["available"] == true
            && self.data["camera"]["state"] == "running";
        let mut companion_audio = self.data["audio"].clone();
        if self.vision_speaking {
            companion_audio["state"] = json!("speaking");
        }
        let transcript = self
            .recording_draft
            .review(ReviewKind::Transcript)
            .map(str::to_owned);
        let submission = self
            .recording_draft
            .review(ReviewKind::Submission)
            .map(str::to_owned);
        let keydown = ctx.link().batch_callback(|event: KeyboardEvent| {
            if event.key() == "Enter" && !event.shift_key() && !event.is_composing() {
                event.prevent_default();
                Some(Msg::Action(Action::Send))
            } else {
                None
            }
        });
        let status = if self.vision_speaking {
            if en {
                "Xiao Yu is speaking…"
            } else {
                "小宇正在说…"
            }
        } else if self.recording {
            if en {
                "Release to finish"
            } else {
                "松开结束"
            }
        } else if self.transcribing {
            if en {
                "Transcribing locally…"
            } else {
                "正在本机转写…"
            }
        } else if generating {
            if en {
                "Xiao Yu is thinking…"
            } else {
                "小宇正在思考…"
            }
        } else if matches!(
            self.data["audio"]["state"].as_str(),
            Some("synthesizing" | "preparing")
        ) {
            if en {
                "Preparing speech…"
            } else {
                "正在准备朗读…"
            }
        } else if matches!(
            self.data["audio"]["state"].as_str(),
            Some("speaking" | "playing")
        ) {
            if en {
                "Xiao Yu is speaking…"
            } else {
                "小宇正在说…"
            }
        } else {
            ""
        };
        let draft_label = if crate::companion_activity::selected_activity(&self.input, en).is_some()
        {
            if en {
                "Activity prompt"
            } else {
                "活动引导语"
            }
        } else if self.input.trim().is_empty() {
            if en {
                "Type instead"
            } else {
                "使用文字输入"
            }
        } else if en {
            "Text draft"
        } else {
            "文字草稿"
        };
        let latest_reply = turns.iter().rev().find(|turn|turn["role"]=="assistant").map(|turn| {
            let tid=text(&turn["turn_id"]);
            let binding=StreamKey::from_head(&self.session_id(),&session["tutor_stream"]).filter(|binding|is_stream_turn(turn)&&binding.turn==tid);
            html!{<details class="voice-caption" open={self.recording||generating||matches!(self.data["audio"]["state"].as_str(),Some("speaking"|"playing"|"synthesizing"|"preparing"))}><summary>{icon("book")}{if en{"Reply text · optional"}else{"文字回复 · 可选"}}</summary><div class="voice-caption-content" ref={self.conversation.clone()}>
                {match binding {
                    Some(binding) => {
                        let sid=binding.session.clone();
                        let sync_sid=sid.clone();
                        let complete_sid=sid.clone();
                        let request=binding.request.clone();
                        html!{<TutorStream binding={binding} enabled={self.stream_display_enabled()} ack_enabled={self.stream_ack_enabled()} viewport={self.conversation.clone()} head_revision={session["tutor_stream"]["revision"].as_u64().unwrap_or(0)} head_phase={text(&session["tutor_stream"]["phase"])} persisted_text={text(&turn["content"])} ui_language={self.ui_language.clone()} on_progress={ctx.link().callback(move |tid|Msg::StreamProgress(sid.clone(),tid))} on_complete={ctx.link().callback(move |_tid|Msg::StreamComplete(complete_sid.clone()))} on_sync={ctx.link().callback(move |_|Msg::StreamSync(sync_sid.clone(),request.clone()))}/>}
                    },
                    None => html!{<p>{text(&turn["content"])}</p>},
                }}
            </div></details>}});
        let controls = if transcript.is_some() {
            html! {<section class="voice-confirm"><strong>{if en{"Review before sending"}else{"确认后发送"}}</strong><p>{transcript.unwrap_or_default()}</p><div><button class="primary" disabled={self.input_disabled()||!self.busy.is_empty()} onclick={ctx.link().callback(|_|Msg::ConfirmTranscript)}>{icon("send")}{if en{"Confirm and send"}else{"确认并发送"}}</button><button class="text-button" onclick={ctx.link().callback(|_|Msg::DismissRecordingText(ReviewKind::Transcript))}>{if en{"Record again"}else{"重新说"}}</button></div></section>}
        } else if submission.is_some() {
            html! {<RecordingReview transcript={Option::<String>::None} submission={submission} ui_language={self.ui_language.clone()} enabled={!self.input_disabled()&&self.busy.is_empty()} on_adopt={ctx.link().callback(|(kind,append)|Msg::AdoptRecordingText(kind,append))} on_dismiss={ctx.link().callback(Msg::DismissRecordingText)}/>}
        } else {
            html! {<><div class="voice-controls">{self.render_ptt_button(ctx)}{if self.vision_audio_ready{html!{<button class="secondary look-button play-vision-button" disabled={!self.busy.is_empty()} onclick={click(ctx,Action::PlayVision)}>{icon("volume")}{if en{"Hear the answer"}else{"听回答"}}</button>}}else if can_look{html!{<button class="secondary look-button" disabled={self.input_disabled()||!self.busy.is_empty()||self.recording||self.transcribing} title={if en{"Send only the current camera picture to Xiao Yu, once"}else{"只把当前这一张画面发给小宇看一次"}} aria-label={if en{"Show this picture to Xiao Yu. Sends one current camera picture."}else{"看看这个。只发送当前一张摄像头画面。"}} onclick={click(ctx,Action::LookAtCamera)}>{icon("camera")}{if en{"Look at this"}else{"看看这个"}}</button>}}else{Html::default()}}<button class="stop-button" disabled={!self.stop_available()} title={t(&self.ui_language,"停止生成和播报")} onclick={click(ctx,Action::Stop)}>{icon("stop")}{t(&self.ui_language,"停止")}</button></div>
                <details class="voice-draft"><summary>{icon("study")}{draft_label}{icon("chevron")}</summary><div class="voice-draft-body"><textarea ref={self.composer_input.clone()} aria-label={t(&self.ui_language,"给小宇的问题或新发现")} placeholder={if en{"Type a question or idea…"}else{"输入问题或想法…"}} value={self.input.clone()} disabled={self.input_disabled()} maxlength="4000" oninput={textarea(ctx,Field::Input)} onkeydown={keydown}/><div><button class="primary" disabled={self.input_disabled()||self.sending_request()||!self.busy.is_empty()||self.input.trim().is_empty()} onclick={click(ctx,Action::Send)}>{icon("send")}{t(&self.ui_language,"发送")}</button>{if !self.input.is_empty(){html!{<button class="text-button" onclick={ctx.link().callback(|_|Msg::ClearInputDraft)}>{if en{"Clear"}else{"清空"}}</button>}}else{Html::default()}}</div></div></details>
                <details class="voice-live-caption" open={self.recording}><summary>{if en{"Live transcription"}else{"实时转写"}}</summary><LiveTranscript recording={self.caption_ticket()} ui_language={self.ui_language.clone()} automatic={self.voice_loop.active()} on_status={ctx.link().callback(|(ticket,result)|Msg::VoiceAudioStatus(ticket,result))}/></details>
            </>}
        };
        html! {<section class="voice-companion" aria-label={if en{"Voice companion"}else{"语音陪伴"}}>
            <div class="voice-links"><a href="#/conversation" onclick={ctx.link().callback(|_|Msg::FocusComposer)}>{icon("study")}{if en{"Text conversation"}else{"文字对话"}}</a></div>
            <div class="home-robot-scene voice-robot-scene"><RobotCompanion ptz={self.data["ptz"].clone()} camera={self.data["camera"].clone()} audio={companion_audio} ui_language={self.ui_language.clone()} generating={generating}/></div>
            <p class="voice-status" role="status">{status}</p>
            {controls}
            {latest_reply.unwrap_or_default()}
            {if camera_open{html!{<aside class="home-active-camera" aria-label={if en{"Active camera controls"}else{"使用中的摄像头控制"}}>{self.render_camera_dock(ctx,true)}</aside>}}else{Html::default()}}
        </section>}
    }

    fn render_ptt_button(&self, ctx: &Context<Self>) -> Html {
        let session = &self.data["session"];
        let pointer_down = ctx.link().callback(|event: PointerEvent| {
            if let Some(target) = event
                .current_target()
                .and_then(|value| value.dyn_into::<web_sys::Element>().ok())
            {
                let _ = target.set_pointer_capture(event.pointer_id());
            }
            Msg::RecordStart
        });
        let key_down = ctx.link().batch_callback(|event: KeyboardEvent| {
            if (event.key() == " " || event.key() == "Enter") && !event.repeat() {
                event.prevent_default();
                Some(Msg::RecordStart)
            } else {
                None
            }
        });
        let key_up = ctx.link().batch_callback(|event: KeyboardEvent| {
            if event.key() == " " || event.key() == "Enter" {
                event.prevent_default();
                Some(Msg::RecordEnd)
            } else {
                None
            }
        });
        html! {<button ref={self.voice_button.clone()} class={classes!("ptt",self.recording.then_some("recording"))} disabled={self.voice_loop.active()||!self.has_session()||session["status"]=="finished"||self.transcribing||!self.busy.is_empty()||self.recording_draft.review(ReviewKind::Transcript).is_some()} onpointerdown={pointer_down} onpointerup={ctx.link().callback(|_|Msg::RecordEnd)} onpointercancel={ctx.link().callback(|_|Msg::RecordEnd)} onblur={ctx.link().callback(|_|Msg::RecordEnd)} onkeydown={key_down} onkeyup={key_up}>{icon("mic")}{if self.recording{t(&self.ui_language,"松开结束")}else{t(&self.ui_language,"按住说话")}}</button>}
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
        html! {<div class="dialogue-column" id="learning-conversation"><article class="panel dialogue-panel"><div class="panel-title"><div><span class="dialogue-title-icon">{icon("sparkle")}</span><h2>{if self.ui_language=="en"{"Let's think together"}else{"和小宇聊一聊"}}</h2></div><span class="model-label">{self.teaching_label()}</span></div>
            {if finished{html!{<div class="session-complete" role="status"><div><strong>{t(&self.ui_language,"本次活动已结束")}</strong><p>{if self.ui_language=="en"{"Your records are saved. Continue this exploration or begin a new one; today's setup will need confirmation."}else{"学习记录已保存。可以续做这次探索，也可以换一个新目标；今天的搭建仍需重新确认。"}}</p></div><div class="session-complete-actions">{crate::activity_history_state::HistoricalActivity::from_finished_session(session).map(|source|html!{<button class="primary" disabled={!self.busy.is_empty()||self.activity_transition_pending()||self.comparison_saving()} onclick={click(ctx,Action::ContinueActivity(source))}>{if self.ui_language=="en"{"Continue this exploration"}else{"继续这次探索"}}{icon("arrow")}</button>}).unwrap_or_default()}<button class="secondary" disabled={!self.busy.is_empty()||self.activity_transition_pending()||self.comparison_saving()} onclick={click(ctx,Action::NewActivity)}>{if self.ui_language=="en"{"Start a new exploration"}else{"开始新的探索"}}</button></div></div>}}else{Html::default()}}
            <div class={classes!("history-switch",(turns.is_empty()&&!self.chat_viewport.history_open()).then_some("empty-history"))}>
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
            {if turns.is_empty()&&!recalled.is_empty(){html!{<div class="resume-card"><strong>{t(&self.ui_language,"相关学习记忆")}</strong><ul>{for recalled.iter().take(2).map(|memory|html!{<li>{text(&memory["content"])}</li>})}</ul><p>{if session["state"]["requires_inventory_confirmation"]==true{t(&self.ui_language,"这些是历史报告。先确认今天的搭建，再继续实验或提出新想法。")}else if self.ui_language=="en"{"These are notes from past companion time. Decide what still applies."}else{"这些是来自过去陪伴的记录，是否仍适用由你决定。"}}</p></div>}}else{Html::default()}}
            {if turns.is_empty(){html!{<div class="chat-empty"><span class="tutor-emblem">{icon("sparkle")}</span><h3>{if self.ui_language=="en"{"Start a conversation"}else{"开始对话"}}</h3><p>{if self.ui_language=="en"{"Learning, stories, games and feelings can all start here. If you are not sure what to say, choose one of these beginnings."}else{"学习、故事、游戏和心情，都可以从这里开始。不知道怎么说时，也可以选一个下面的开头。"}}</p><div class="prompt-suggestions"><small>{if self.ui_language=="en"{"A little inspiration to get started"}else{"不知道从哪里开始？选一个灵感"}}</small>{for [
                ("给我讲一个关于月亮和勇气的互动故事，每次讲一小段，再让我选择接下来发生什么。", "Tell me an interactive story about the moon and courage, one short part at a time, then let me choose what happens next.", "听一个互动故事", "Start a story", "book"),
                ("我有一道不会的题，请先听我说说已经试过的方法，再一步一步引导我。", "I have a question I cannot solve. Listen to what I have tried, then guide me one step at a time.", "一起解决一道题", "Solve a problem together", "study"),
                ("我想和你说说今天发生的一件事，请先听我把事情和感受讲完。", "I want to tell you about my day. Please listen to what happened and how I feel first.", "说说我的心里话", "Share how I feel", "heart")
            ].iter().map(|(zh,en,zh_label,en_label,icon_name)|{
                let prompt=if self.ui_language=="en"{*en}else{*zh}.to_owned();
                html!{<button class={classes!("prompt-chip",format!("prompt-{}",icon_name))} disabled={self.input_disabled()||!self.busy.is_empty()||!self.input.trim().is_empty()} onclick={ctx.link().callback(move |_|Msg::Field(Field::Input,prompt.clone()))}><span class="prompt-icon">{icon(icon_name)}</span><span>{if self.ui_language=="en"{*en_label}else{*zh_label}}</span>{icon("arrow")}</button>}
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
                        if self.ui_language == "en" { "Companion reply" } else { "陪伴回复" }
                    } else {
                        if self.ui_language == "en" { "Text reply" } else { "文字回复" }
                    }
                } else {
                    match turn["status"].as_str().unwrap_or("") {
                        "cancelled" => t(&self.ui_language,"已取消"), "generated" => t(&self.ui_language,"待投递"),
                        "speech_interrupted" => t(&self.ui_language,"朗读已打断"),
                        _ => if assistant { t(&self.ui_language,"陪伴回复") } else { source_label(&self.ui_language,"learner_report") }
                    }
                };
                html!{<div class={classes!("message",if assistant{"tutor-message"}else{"learner-message"},(turn["status"]=="cancelled").then_some("cancelled"))} key={tid.clone()}>
                    <div class="message-meta"><span>{if assistant{if self.ui_language=="en"{"Xiao Yu · AI Tutor".to_owned()}else{"小宇 · AI Tutor".to_owned()}}else{name.clone()}}</span><small>{message_status}</small></div>
                    {if let Some(binding) = binding {
                        let sid = binding.session.clone();
                        let sync_sid = sid.clone();
                        let complete_sid = sid.clone();
                        let request = binding.request.clone();
                        html!{<TutorStream binding={binding} enabled={self.stream_display_enabled()} ack_enabled={self.stream_ack_enabled()} viewport={self.conversation.clone()}
                            head_revision={session["tutor_stream"]["revision"].as_u64().unwrap_or(0)}
                            head_phase={text(&session["tutor_stream"]["phase"])} persisted_text={text(&turn["content"])}
                            ui_language={self.ui_language.clone()}
                            on_progress={ctx.link().callback(move |tid| Msg::StreamProgress(sid.clone(),tid))}
                            on_complete={ctx.link().callback(move |_tid| Msg::StreamComplete(complete_sid.clone()))}
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
            <textarea ref={self.composer_input.clone()} aria-label={t(&self.ui_language,"给小宇的问题或新发现")} aria-describedby="composer-help" placeholder={if finished{t(&self.ui_language,"本次活动已结束，请开始下一次活动")}else if self.has_session(){t(&self.ui_language,"说说你的问题、想法或新发现…")}else{t(&self.ui_language,"先选择一个学习者档案…")}} value={self.input.clone()} disabled={self.input_disabled()} maxlength="4000" oninput={textarea(ctx,Field::Input)} onkeydown={keydown}/>
            <div class="composer-actions">{self.render_ptt_button(ctx)}<button class="stop-button" disabled={!self.stop_available()} title={t(&self.ui_language,"停止生成和播报")} onclick={click(ctx,Action::Stop)}>{icon("stop")}{t(&self.ui_language,"停止")}</button><button class="send-button" disabled={self.input_disabled()||self.sending_request()||!self.busy.is_empty()||self.input.trim().is_empty()||self.recording_draft.review(ReviewKind::Submission).is_some()} onclick={click(ctx,Action::Send)}><span>{t(&self.ui_language,"发送")}</span>{icon("send")}</button></div><div class="composer-meta"><span>{if self.ui_language=="en"{"Enter to send · Shift+Enter for a new line"}else{"Enter 发送 · Shift+Enter 换行"}}</span><span class="input-count">{format!("{} / 4000",self.input.chars().count())}</span></div><p class="composer-note" id="composer-help">{self.teaching_input_notice()}</p></div>
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
        let empty_activity = !finished
            && text(&state["goal"]).is_empty()
            && text(&state["latest_input"]).is_empty()
            && text(&state["reported_result"]).is_empty()
            && text(&state["pending_step"]).is_empty()
            && state["modules"].is_null()
            && list(&state["attempts"]).is_empty();
        html! {<article class="panel activity-panel">
            {if self.continuation.uncertain {html!{<div class="continuation-review" role="status"><strong>{if self.ui_language=="en"{"Continuation result needs checking"}else{"续做结果待核对"}}</strong><p>{if self.ui_language=="en"{"The request may have completed. Refresh to check, or retry this same request. No second activity will be requested automatically."}else{"请求可能已经完成。可以刷新核对，或重试同一次请求；不会自动另建一次活动。"}}</p><div class="history-controls"><button class="text-button" onclick={ctx.link().callback(|_|Msg::FetchState(true))}>{if self.ui_language=="en"{"Refresh current activity"}else{"刷新核对当前活动"}}</button><button class="secondary" disabled={self.continuation.in_flight||!self.busy.is_empty()} onclick={ctx.link().callback(|_|Msg::RetryContinuation)}>{if self.ui_language=="en"{"Check this request again"}else{"核对同一次续做"}}</button></div></div>}}else{Html::default()}}
            {if empty_activity {html!{<div class="activity-empty">
                <span class="activity-empty-icon">{icon("sparkle")}</span>
                <h2>{if self.ui_language=="en"{"No activity yet"}else{"还没有活动记录"}}</h2>
                <p>{if self.ui_language=="en"{"Choose an activity, or tell Xiao Yu what is on your mind."}else{"选择一项活动，或直接和小宇说说你的想法。"}}</p>
                <div class="activity-empty-actions"><a class="secondary" href="#/activities" onclick={navigate_click(ctx,AppView::Activities)}>{icon("sparkle")}{if self.ui_language=="en"{"Activities"}else{"选择活动"}}</a><a class="primary" href="#/voice" onclick={ctx.link().batch_callback(|event:MouseEvent|{event.prevent_default();vec![Msg::FocusVoice]})}>{icon("mic")}{if self.ui_language=="en"{"Start talking"}else{"开始对话"}}</a></div>
            </div>}} else {html!{<>
                <ActivitySummary state={state.clone()} ui_language={self.ui_language.clone()} finished={finished} pending_step_needs_confirmation={pending_needs_confirmation(&self.data["session"])}/>
                <ExperimentComparison owner={self.comparison_owner()} ui_language={self.ui_language.clone()} reading_level={self.reading_level.clone()}
                    writable={!finished} enabled={self.busy.is_empty()&&!self.activity_transition_pending()&&!self.recording&&!self.record_start_pending&&!self.transcribing}
                    save_enabled={!self.comparison_saving()} generation={self.data["session"]["generation"].as_u64().unwrap_or(0)} focus_revision={self.comparison_focus}
                    on_guard={ctx.link().callback(Msg::ComparisonChanged)} on_settled={ctx.link().callback(|(learner,session)|Msg::ComparisonSettled(learner,session))}/>
                <div class="activity-footer">{icon("memory")}<span>{if finished{t(&self.ui_language,"本次学习记录已保存")}else{t(&self.ui_language,"当前活动会自动保存")}}<br/><small>{t(&self.ui_language,if finished{"下次从相关记忆继续，并重新确认搭建。"}else{"完成尝试后，可结束并保存本次活动。"})}</small></span>{if self.has_session()&&!finished{html!{<button class="text-button" disabled={!self.busy.is_empty()||self.comparison_saving()} onclick={click(ctx,Action::Finish)}>{t(&self.ui_language,"结束并保存")}</button>}}else{Html::default()}}</div>
            </>}}}
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
            format!("{} 条记录", self.memories.len())
        };
        let preference_summary = preference_text(preferences);
        let preference_summary = if language == "en" && preference_summary == "使用档案设置" {
            "Profile defaults".into()
        } else {
            preference_summary
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
                        <p class="empty-note">{if language=="en" {"No learning memories yet."} else {"这个档案还没有学习记忆。"}}</p>
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
                                            <textarea ref={self.memory_edit_input.clone()} disabled={!self.busy.is_empty()} aria-label={if language=="en" {"Correct memory content"} else {"更正记忆内容"}} value={self.edit_text.clone()} maxlength="800" oninput={textarea(ctx,Field::EditMemory)}/>
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
                                    <button id={format!("memory-edit-{id}")} class="text-button" disabled={!self.busy.is_empty()||self.edit_memory.is_some()} onclick={ctx.link().callback(move |_|Msg::Edit(edit.clone()))}>{t(language,"更正")}</button>
                                    <button class="icon-button danger" aria-label={if language=="en" {"Forget this memory"} else {"忘记这条记忆"}} disabled={!self.busy.is_empty()||self.edit_memory.is_some()} onclick={ctx.link().callback(move |_|Msg::Delete(delete.clone()))}>{icon("trash")}</button>
                                </div>
                            </div>
                        }
                    })}}}
                    <details class="snapshot"><summary>{if language=="en"{"Session memory and recalled notes"}else{"本轮快记忆与召回条目"}}</summary><dl>
                        <dt>{if language=="en"{"Last delivered hint"}else{"上一条已投递提示"}}</dt><dd>{fallback(&state["last_delivered_hint"],if language=="en"{"None"}else{"暂无"})}</dd>
                        <dt>{if language=="en"{"Session preferences"}else{"本轮偏好"}}</dt><dd>{preference_summary}</dd>
                        <dt>{if language=="en"{"Recalled notes"}else{"本轮召回"}}</dt><dd>{if recall.is_empty(){if language=="en"{"No relevant history yet".into()}else{"尚无相关历史".into()}}else{recall.iter().map(|m|text(&m["content"])).collect::<Vec<_>>().join("；")}}</dd>
                    </dl></details>
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
        let unknown = |value: String| {
            if en && value == "未知" {
                "Unknown".into()
            } else {
                value
            }
        };
        let local_asr = if audio["asr_ready"] == true {
            fallback(
                &audio["asr_provider"],
                if en {
                    "Local ASR ready"
                } else {
                    "本地 ASR 可用"
                },
            )
        } else if en {
            "Local ASR model not ready".into()
        } else {
            "本地 ASR 模型待准备".into()
        };
        let local_tts = if voice_ready {
            fallback(
                &audio["tts_provider"],
                if en {
                    "Local TTS ready"
                } else {
                    "本地 TTS 可用"
                },
            )
        } else if en {
            "No local voice detected".into()
        } else {
            "本地声音待探测".into()
        };
        let kimi_status = if kimi["available"] == true {
            if en {
                "Tool-free backend available".into()
            } else {
                "无工具后端可用".into()
            }
        } else {
            fallback(
                &kimi["error"],
                if en {
                    "Backend unavailable"
                } else {
                    "后端尚未可用"
                },
            )
        };
        let values = vec![
            (
                if en {
                    "Camera processing"
                } else {
                    "检测处理帧率"
                },
                self.data["camera"]["fps"]
                    .as_f64()
                    .map(|v| format!("{v:.1} fps"))
                    .unwrap_or_else(|| {
                        if en {
                            "Unknown".into()
                        } else {
                            "未知".into()
                        }
                    }),
            ),
            (
                if en { "Frame age" } else { "服务端帧龄" },
                unknown(milliseconds(&self.data["camera"]["frame_age_ms"])),
            ),
            (
                if en {
                    "Teaching requests"
                } else {
                    "教学请求数"
                },
                fallback(&metrics["requests"], if en { "Unknown" } else { "未知" }),
            ),
            (
                if en {
                    "Model latency"
                } else {
                    "模型调用耗时"
                },
                unknown(milliseconds(&metrics["last_latency_ms"])),
            ),
            (
                if en {
                    "Provider tokens"
                } else {
                    "供应商 token 用量"
                },
                fallback(
                    &metrics["reported_tokens"],
                    if en { "Unknown" } else { "未知" },
                ),
            ),
            (
                if en {
                    "Local ASR latency"
                } else {
                    "本地 ASR 耗时"
                },
                unknown(milliseconds(
                    audio.get("asr_ms").unwrap_or(&audio["last_asr_ms"]),
                )),
            ),
        ];
        html! {
            <details class="panel diagnostics-panel">
                <summary><span>{icon("activity")}<strong>{t(&self.ui_language,"运行状态")}</strong><small>{if en {"Capabilities and latency"} else {"实际能力与延迟"}}</small></span>{icon("chevron")}</summary>
                <div class="details-content">
                    <div class="metrics-grid">{for values.into_iter().map(|(label,value)|html!{<div><small>{label}</small><strong>{value}</strong></div>})}</div>
                    <dl class="capabilities">
                        <dt>{if en {"Teaching model"} else {"教学模型"}}</dt><dd>{teaching_detail}</dd>
                        <dt>{"Kimi Code"}</dt><dd>{format!("{} · {}",fallback(&kimi["version"],if en {"Version unknown"} else {"版本待探测"}),kimi_status)}</dd>
                        <dt>{if en {"Local speech"} else {"本地语音"}}</dt><dd>{format!("{local_asr} / {local_tts}")}</dd>
                        <dt>{if en {"Emotion model"} else {"情绪模型"}}</dt><dd>{if en{"Off · no emotion inference from faces"}else{"关闭 · 不根据人脸推断情绪"}}</dd>
                        <dt>{if en {"Automatic hardware actions"} else {"积木自动执行"}}</dt><dd>{if en {"Not enabled"} else {"未启用"}}</dd>
                        <dt>{if en {"UI runtime"} else {"界面运行时"}}</dt><dd>{"Rust · WebAssembly"}</dd>
                    </dl>
                </div>
            </details>
        }
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
        html! {<section class="panel teacher-panel"><div class="teacher-header"><div class="teacher-heading-icon">{icon("settings")}</div><div><h2>{if self.ui_language=="en"{"Teacher tools"}else{"教师工具"}}</h2><p>{if self.ui_language=="en"{"Choose a learner, prepare the modules, and make the guidance feel right."}else{"选择学习者、准备今天的模块，调整适合的引导方式。"}}</p></div><span class="settings-label">{if self.ui_language=="en"{"TEACHER TOOLS"}else{"教师工具"}}</span><button class="icon-button" aria-label={if self.ui_language=="en"{"Close grown-up settings"}else{"收起家长与教师设置"}} onclick={ctx.link().callback(|_|Msg::ToggleTeacher)}>{icon("close")}</button></div><div class="teacher-grid">
            <div><h3>{t(&self.ui_language,"学习者档案")}</h3><label class="field-label">{t(&self.ui_language,"选择档案")}<SelectControl value={lid} disabled={!self.busy.is_empty()} onchange={ctx.link().callback(|event:Event|Msg::Action(Action::Switch(event.target_unchecked_into::<HtmlSelectElement>().value())))}><option value="">{t(&self.ui_language,"选择匿名档案")}</option>{for list(&self.data["learners"]).iter().map(|l|html!{<option value={text(&l["learner_id"])}>{format!("{}{}",text(&l["name"]),if l["namespace"]=="simulator"{if self.ui_language=="en"{" · Simulation"}else{" · 模拟"}}else{""})}</option>})}</SelectControl></label><div class="create-profile"><input aria-label={t(&self.ui_language,"新匿名档案名称")} placeholder={t(&self.ui_language,"新档案名称，如 学习者 A")} value={self.name.clone()} maxlength="40" oninput={input(ctx,Field::Name)}/><SelectControl label={t(&self.ui_language,"档案数据空间")} value={self.namespace.clone()} onchange={select(ctx,Field::Namespace)}><option value="real">{t(&self.ui_language,"真实记录")}</option><option value="simulator">{t(&self.ui_language,"独立模拟")}</option></SelectControl><button class="secondary" disabled={self.name.trim().is_empty()||!self.busy.is_empty()} onclick={click(ctx,Action::Create)}>{icon("plus")}{t(&self.ui_language,"创建")}</button></div><p class="fine-print">{t(&self.ui_language,"只使用匿名名称。视觉目标由教师另行选择。")}</p>{if !no_session{html!{<div class="session-actions"><button class="secondary" disabled={!self.busy.is_empty()} onclick={click(ctx,Action::NewActivity)}>{t(&self.ui_language,"开启新的学习活动")}</button>{if self.data["session"]["status"]!="finished"{html!{<button class="secondary" disabled={!self.busy.is_empty()} onclick={click(ctx,Action::Finish)}>{t(&self.ui_language,"结束本次活动并保存")}</button>}}else{Html::default()}}</div>}}else{Html::default()}}</div>
            <div class="module-confirmation" ref={self.module_confirmation.clone()} tabindex="-1"><ModuleInventory ui_language={self.ui_language.clone()} modules={self.inventory.modules.clone()} modules_dirty={self.inventory.modules_dirty}
                report={self.inventory.report.clone()} notice={self.report_notice.clone()} source={self.module_source.clone()} simulated={simulator} inactive={inactive} busy={!self.busy.is_empty()}
                can_save={self.inventory.payload(&self.module_source).is_some()} on_module={ctx.link().callback(|(id,value)|Msg::Module(id,value))}
                on_report={ctx.link().callback(|value|Msg::Field(Field::Report,value))} on_source={ctx.link().callback(|value|Msg::Field(Field::ModuleSource,value))}
                on_confirm={click(ctx,Action::ConfirmInventory)} on_save={click(ctx,Action::Modules)}/></div>
            <div><h3>{t(&self.ui_language,"教学与本地音频")}</h3><label class="field-label">{t(&self.ui_language,"教学语言")}<SelectControl value={self.language.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::Language)}><option value="zh">{"中文"}</option><option value="en">{"English"}</option></SelectControl></label><label class="field-label">{t(&self.ui_language,"档案提示方式")}<SelectControl value={self.help_style.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::HelpStyle)}><option value="one_step">{t(&self.ui_language,"一次一步")}</option><option value="direct">{t(&self.ui_language,"直接解释")}</option><option value="guided">{t(&self.ui_language,"适量提示")}</option></SelectControl></label>{self.render_session_preferences(ctx)}<label class="field-label">{t(&self.ui_language,"阅读表达")}<SelectControl value={self.reading_level.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::ReadingLevel)}><option value="plain">{t(&self.ui_language,"短句、少术语")}</option><option value="standard">{t(&self.ui_language,"常规说明")}</option><option value="technical">{t(&self.ui_language,"术语与完整解释")}</option></SelectControl></label><label class="field-label">{t(&self.ui_language,"朗读语速")}<SelectControl value={self.speech_speed.clone()} disabled={no_session||!self.busy.is_empty()} onchange={select(ctx,Field::SpeechSpeed)}><option value="slow">{t(&self.ui_language,"较慢")}</option><option value="normal">{t(&self.ui_language,"正常")}</option><option value="fast">{t(&self.ui_language,"较快")}</option></SelectControl></label><p class="fine-print">{t(&self.ui_language,"按本次需要选择表达方式，不根据年龄推断能力。")}</p><label class="field-label">{t(&self.ui_language,"麦克风")}<SelectControl value={self.microphone_id.clone()} onchange={select(ctx,Field::Microphone)}><option value="">{t(&self.ui_language,"系统默认输入")}</option>{for self.microphones.iter().filter(|d|d["input_channels"].as_u64()!=Some(0)).map(|d|{let id=d.get("device_id").or_else(||d.get("index")).map(text).unwrap_or_default();html!{<option value={id}>{fallback(&d["name"],"麦克风")}</option>}})}</SelectControl></label><button class="text-button" onclick={click(ctx,Action::SpeechStop)}>{icon("volume")}{t(&self.ui_language,"停止本地播报")}</button></div>
            <div class="privacy-card">{icon("shield")}<h3>{t(&self.ui_language,"数据与隐私")}</h3><p>{self.teaching_privacy_notice()}</p><button class="danger-link" disabled={no_session} onclick={ctx.link().callback(move |_|Msg::Delete(json!({"all":true,"content":learner_name})))}>{icon("trash")}{t(&self.ui_language,"清除当前档案学习数据")}</button></div>
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

use crate::{companion_activity::ACTIVITIES, robot_companion::RobotCompanion, view::icon};
use serde_json::Value;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RobotHomeProps {
    pub ptz: Value,
    pub camera: Value,
    pub audio: Value,
    pub ui_language: String,
    pub generating: bool,
    pub has_session: bool,
    pub learner_name: String,
    pub simulator: bool,
    pub on_chat: Callback<MouseEvent>,
    pub on_activities: Callback<MouseEvent>,
    pub on_profile: Callback<MouseEvent>,
}

#[function_component(RobotHome)]
pub fn robot_home(props: &RobotHomeProps) -> Html {
    let en = props.ui_language == "en";
    let avatar = props.learner_name.chars().next().unwrap_or('?').to_string();
    html! {
        <section class="robot-home" id="learning-space" aria-label={if en {"AI Tutor home"} else {"AI Tutor 首页"}}>
            <h1 class="visually-hidden">{if en {"AI Tutor companion for children"} else {"AI Tutor 儿童成长陪伴空间"}}</h1>
            <button class="home-profile" type="button" onclick={props.on_profile.clone()} title={if en {"Parent center"} else {"家长中心"}}>
                <span class="home-profile-avatar">{avatar}</span>
                <span class="home-profile-copy"><small>{if en {"Child"} else {"孩子"}}</small><strong>{&props.learner_name}</strong></span>
                <span class="home-profile-arrow">{icon("chevron")}</span>
            </button>
            <div class="home-robot-scene has-room">
                <div class="home-room-layer" aria-hidden="true"><ai-tutor-room/></div>
                <RobotCompanion ptz={props.ptz.clone()} camera={props.camera.clone()} audio={props.audio.clone()} ui_language={props.ui_language.clone()} generating={props.generating}/>
            </div>
            <div class="home-primary-actions">
                <button class="primary home-talk-button" type="button" disabled={!props.has_session} onclick={props.on_chat.clone()}><span class="home-action-icon">{icon("mic")}</span><span>{if en {"Start talking"} else {"开始对话"}}</span></button>
                <button class="home-activities-button" type="button" onclick={props.on_activities.clone()}><span class="home-action-icon">{icon("compass")}</span><span>{if en {"Activities"} else {"选择活动"}}</span></button>
            </div>
        </section>
    }
}

#[derive(Properties, PartialEq)]
pub struct ActivityGalleryProps {
    pub ui_language: String,
    pub disabled: bool,
    pub has_draft: bool,
    pub selected: Option<&'static str>,
    pub on_select: Callback<String>,
}

#[function_component(ActivityGallery)]
pub fn activity_gallery(props: &ActivityGalleryProps) -> Html {
    let en = props.ui_language == "en";
    let active_id = use_state(|| props.selected.unwrap_or(ACTIVITIES[0].id));
    let activity = ACTIVITIES
        .iter()
        .find(|activity| activity.id == *active_id)
        .unwrap_or(&ACTIVITIES[0]);
    let prompt = activity.prompt(en).to_owned();
    let start_disabled = props.disabled || props.has_draft;
    let on_start = props.on_select.clone();
    let title = if en {
        activity.title_en
    } else {
        activity.title_zh
    };
    let description = if en {
        activity.description_en
    } else {
        activity.description_zh
    };
    html! {
        <section class="activity-gallery activity-page" id="activity-gallery" aria-labelledby="activity-gallery-title">
            <header class="activity-page-heading">
                <h1 id="activity-gallery-title">{if en {"Activities"} else {"活动"}}</h1>
            </header>
            {if props.has_draft {html!{<p class="activity-draft-note" role="status">{if en {"Send or clear your current draft before choosing an activity."} else {"当前有未发送的草稿，请先发送或清空，再选择活动。"}}</p>}} else {Html::default()}}
            <div class="activity-page-grid" role="group" aria-label={if en {"Choose an activity to preview"} else {"选择一个活动查看引导语"}}>
                {for ACTIVITIES.iter().map(|activity| {
                    let selected = *active_id == activity.id;
                    let title = if en { activity.title_en } else { activity.title_zh };
                    let description = if en { activity.description_en } else { activity.description_zh };
                    let choose = {
                        let active_id = active_id.clone();
                        Callback::from(move |_| active_id.set(activity.id))
                    };
                    html! {
                        <button key={activity.id} type="button" class={classes!("activity-card",selected.then_some("selected"),(activity.id=="story").then_some("featured"))} data-activity={activity.id} aria-pressed={selected.to_string()} aria-controls="activity-preview" onclick={choose}>
                            <span class={classes!("activity-icon",format!("activity-{}",activity.id))}>{icon(activity.icon)}</span>
                            <span class="activity-copy"><strong>{title}</strong><small>{description}</small></span>
                            <span class="activity-choice-status">{if selected {html!{<>{icon("check")}{if en {"Selected"} else {"已选择"}}</>}} else {html!{<>{if en {"Preview"} else {"查看"}}{icon("arrow")}</>}}}</span>
                        </button>
                    }
                })}
            </div>
            <section class="activity-preview" id="activity-preview" aria-labelledby="activity-preview-title">
                <span class={classes!("activity-preview-icon",format!("activity-{}",activity.id))}>{icon(activity.icon)}</span>
                <div class="activity-preview-copy" aria-live="polite">
                    <small>{if en {"Selected activity"} else {"当前选择"}}</small>
                    <h2 id="activity-preview-title">{title}</h2>
                    <p class="activity-prompt">{description}</p>
                    <details class="activity-prompt-details"><summary>{if en {"View prompt"} else {"查看引导语"}}</summary><p>{prompt.clone()}</p></details>
                </div>
                <a class="primary activity-start" href="#/voice" aria-disabled={start_disabled.to_string()} tabindex={if start_disabled {"-1"} else {"0"}}
                    onclick={on_start.reform(move |event: MouseEvent| {
                        if start_disabled {
                            event.prevent_default();
                        }
                        prompt.clone()
                    })}>
                    {if en {"Start activity"} else {"开始活动"}}{icon("arrow")}
                </a>
            </section>
        </section>
    }
}

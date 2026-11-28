use crate::view::{icon, SelectControl};
use serde_json::Value;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ParentCenterProps {
    pub parent_digest: Value,
    pub ui_language: String,
    pub help_style: String,
    pub reading_level: String,
    pub speech_speed: String,
    pub settings_disabled: bool,
    pub clear_disabled: bool,
    pub privacy_notice: String,
    pub on_close: Callback<MouseEvent>,
    pub on_help_style: Callback<Event>,
    pub on_reading_level: Callback<Event>,
    pub on_speech_speed: Callback<Event>,
    pub on_clear_data: Callback<MouseEvent>,
    pub on_records: Callback<MouseEvent>,
}

struct ParentDigest {
    child_name: String,
    activity_title: String,
    activity_finished: bool,
    recent_child_words: Vec<String>,
    memory_count: u64,
    has_records: bool,
}

impl ParentDigest {
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.get("schema_version")?.as_u64()? != 1 {
            return None;
        }
        let child_name = object.get("child_name")?.as_str()?.trim().to_owned();
        let activity_title = object.get("activity_title")?.as_str()?.trim().to_owned();
        let activity_finished = object.get("activity_finished")?.as_bool()?;
        let recent_child_words = object
            .get("recent_child_words")?
            .as_array()?
            .iter()
            .map(|word| Some(word.as_str()?.trim().to_owned()))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .filter(|word| !word.is_empty())
            .collect();
        let memory_count = object.get("memory_count")?.as_u64()?;
        let has_records = object.get("has_records")?.as_bool()?;
        Some(Self {
            child_name,
            activity_title,
            activity_finished,
            recent_child_words,
            memory_count,
            has_records,
        })
    }
}

fn empty_message(language: &str, zh: &'static str, en: &'static str) -> Html {
    html! {<p class="parent-empty-note">{if language == "en" { en } else { zh }}</p>}
}

fn render_activity(digest: &ParentDigest, language: &str) -> Html {
    html! {<section class="parent-record-card parent-activity-card" aria-labelledby="parent-activity-title">
        <div class="parent-section-label"><span>{icon("book")}</span><h3 id="parent-activity-title">{if language == "en" { "Recent activity" } else { "最近活动" }}</h3></div>
        {if digest.activity_title.is_empty() {
            empty_message(language, "暂时没有可展示的活动。", "No activity is available to show yet.")
        } else {html!{<>
            <h4>{&digest.activity_title}</h4>
            <span class={classes!("parent-activity-state", digest.activity_finished.then_some("finished"))}>
                {if digest.activity_finished {if language == "en" { "Finished" } else { "已结束" }} else {if language == "en" { "In progress" } else { "进行中" }}}
            </span>
        </>}}}
    </section>}
}

fn render_growth_clues(digest: &ParentDigest, language: &str) -> Html {
    html! {<section class="parent-record-card parent-growth-card" aria-labelledby="parent-growth-title">
        <div class="parent-section-label"><span>{icon("sparkle")}</span><h3 id="parent-growth-title">{if language == "en" { "Growth clues" } else { "成长线索" }}</h3></div>
        {if digest.memory_count == 0 {
            empty_message(language, "还没有已保存的成长线索。", "No growth clues have been saved yet.")
        } else {html!{<>
            <p class="parent-memory-count"><strong>{digest.memory_count}</strong><span>{if language == "en" { "saved clues" } else { "条已保存的成长线索" }}</span></p>
            <p class="parent-clue-note">{if language == "en" { "These are records from companion time, not grades or conclusions about ability." } else { "这些来自陪伴过程，不是评分，也不代表能力结论。" }}</p>
        </>}}}
    </section>}
}

fn render_child_words(digest: &ParentDigest, language: &str) -> Html {
    html! {<section class="parent-record-card parent-words-card" aria-labelledby="parent-words-title">
        <div class="parent-section-label"><span>{icon("chat")}</span><h3 id="parent-words-title">{if language == "en" { "In your child's own words" } else { "孩子最近说" }}</h3><small>{if language == "en" { "Recorded words only" } else { "仅展示记录中的原话" }}</small></div>
        {if digest.recent_child_words.is_empty() {
            empty_message(language, "暂时没有可展示的孩子原话。", "No recorded words from your child are available yet.")
        } else {html!{<ul class="parent-child-words">{for digest.recent_child_words.iter().map(|word| html!{<li><span aria-hidden="true">{"“"}</span><p>{word}</p></li>})}</ul>}}}
    </section>}
}

fn render_records(digest: Option<&ParentDigest>, language: &str) -> Html {
    let Some(digest) = digest else {
        return html! {<section class="parent-records-empty" role="status"><span>{icon("book")}</span><div><h3>{if language == "en" { "Records unavailable" } else { "暂时无法读取陪伴记录" }}</h3><p>{if language == "en" { "Please try again shortly." } else { "请稍后重试。" }}</p></div></section>};
    };
    if !digest.has_records {
        return html! {<section class="parent-records-empty" role="status"><span>{icon("book")}</span><div><h3>{if language == "en" { "No companion records yet" } else { "还没有陪伴记录" }}</h3><p>{if language == "en" { "Activities and your child's words will appear here." } else { "活动和孩子原话会显示在这里。" }}</p></div></section>};
    }
    html! {<div class="parent-record-grid">
        {render_activity(digest, language)}
        {render_growth_clues(digest, language)}
        {render_child_words(digest, language)}
    </div>}
}

#[function_component(ParentCenter)]
pub fn parent_center(props: &ParentCenterProps) -> Html {
    let language = props.ui_language.as_str();
    let digest = ParentDigest::from_value(&props.parent_digest);
    let child_name = digest
        .as_ref()
        .map(|digest| digest.child_name.as_str())
        .filter(|name| !name.is_empty());
    let settings_disabled = props.settings_disabled;
    html! {<section class="parent-center" aria-labelledby="parent-center-title">
        <header class="parent-center-header">
            <span class="parent-center-mark">{icon("heart")}</span>
            <div><small>{if language == "en" { "FAMILY" } else { "家庭" }}</small><h2 id="parent-center-title">{if language == "en" { "Parent center" } else { "家长中心" }}</h2><p>{if language == "en" { "Companion records and settings" } else { "陪伴记录与设置" }}</p></div>
            <button class="icon-button parent-center-close" aria-label={if language == "en" { "Close parent center" } else { "关闭家长中心" }} onclick={props.on_close.clone()}>{icon("close")}</button>
        </header>
        <div class="parent-child-heading">
            <span class="parent-child-avatar">{child_name.and_then(|name| name.chars().next()).map(|initial| html!{{initial}}).unwrap_or_else(|| icon("heart"))}</span>
            <div><small>{if language == "en" { "Child" } else { "孩子" }}</small><h3>{child_name.unwrap_or(if language == "en" { "No child selected" } else { "尚未选择孩子" })}</h3></div>
            <button class="text-button parent-all-records" onclick={props.on_records.clone()}>{icon("history")}{if language == "en" { "All records" } else { "全部记录" }}</button>
        </div>
        {render_records(digest.as_ref(), language)}
        <section class="parent-companion-settings" aria-labelledby="parent-settings-title">
            <div class="parent-section-label"><span>{icon("settings")}</span><h3 id="parent-settings-title">{if language == "en" { "Companion settings" } else { "陪伴设置" }}</h3></div>
            <p class="parent-section-intro">{if language == "en" { "Adjust Xiao Yu's explanations and read-aloud voice." } else { "调整小宇的讲解与朗读方式。" }}</p>
            <div class="parent-setting-grid">
                <label><span>{if language == "en" { "Guidance" } else { "引导方式" }}</span><SelectControl label={if language == "en" { "Guidance" } else { "引导方式" }} value={props.help_style.clone()} disabled={settings_disabled} onchange={props.on_help_style.clone()}><option value="one_step">{if language == "en" { "One step at a time" } else { "一次一步" }}</option><option value="direct">{if language == "en" { "Direct explanation" } else { "直接解释" }}</option><option value="guided">{if language == "en" { "A little guidance" } else { "适量提示" }}</option></SelectControl></label>
                <label><span>{if language == "en" { "Reading style" } else { "阅读表达" }}</span><SelectControl label={if language == "en" { "Reading style" } else { "阅读表达" }} value={props.reading_level.clone()} disabled={settings_disabled} onchange={props.on_reading_level.clone()}><option value="plain">{if language == "en" { "Short, plain sentences" } else { "短句、少术语" }}</option><option value="standard">{if language == "en" { "Everyday explanation" } else { "常规说明" }}</option><option value="technical">{if language == "en" { "Detailed explanation" } else { "详细讲解" }}</option></SelectControl></label>
                <label><span>{if language == "en" { "Read-aloud pace" } else { "朗读语速" }}</span><SelectControl label={if language == "en" { "Read-aloud pace" } else { "朗读语速" }} value={props.speech_speed.clone()} disabled={settings_disabled} onchange={props.on_speech_speed.clone()}><option value="slow">{if language == "en" { "Slower" } else { "较慢" }}</option><option value="normal">{if language == "en" { "Normal" } else { "正常" }}</option><option value="fast">{if language == "en" { "Faster" } else { "较快" }}</option></SelectControl></label>
            </div>
        </section>
        <section class="parent-privacy" aria-labelledby="parent-privacy-title">
            <span class="parent-privacy-icon">{icon("shield")}</span>
            <div><h3 id="parent-privacy-title">{if language == "en" { "Privacy and your child's data" } else { "隐私与孩子的数据" }}</h3><p>{&props.privacy_notice}</p></div>
            <button class="danger-link" disabled={props.clear_disabled} onclick={props.on_clear_data.clone()}>{icon("trash")}{if language == "en" { "Clear this child's learning data" } else { "清除当前孩子的学习数据" }}</button>
        </section>
    </section>}
}

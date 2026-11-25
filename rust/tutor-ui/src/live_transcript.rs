//! Temporary recording captions. This component cannot send a tutor message or
//! change the editable composer; only the final recording result may do that.
use crate::{
    api::{ApiResult, api},
    i18n::t,
    preview_browser::now,
    recording_draft::RecordingTicket,
};
use gloo_timers::callback::Timeout;
use serde_json::Value;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CaptionProps {
    pub recording: Option<RecordingTicket>,
    pub ui_language: String,
    #[prop_or_default]
    pub automatic: bool,
    #[prop_or_default]
    pub on_status: Callback<(RecordingTicket, ApiResult)>,
}

#[derive(Default, PartialEq, Clone)]
struct CaptionState {
    key: Option<RecordingTicket>,
    revision: Option<u64>,
    text: String,
    state: String,
    provider: String,
    fallback_reason: String,
}
impl CaptionState {
    fn bind(&mut self, key: Option<RecordingTicket>) {
        if self.key != key {
            *self = Self {
                key,
                ..Self::default()
            };
        }
    }
    fn apply(&mut self, key: &RecordingTicket, value: &Value) -> bool {
        if self.key.as_ref() != Some(key) {
            return false;
        }
        let previous = self.clone();
        if value["recording_generation"].as_u64() != Some(key.recording_generation) {
            self.text.clear();
            self.state = "idle".into();
            self.provider.clear();
            self.fallback_reason.clear();
            return *self != previous;
        }
        self.state = value["asr_streaming_state"]
            .as_str()
            .unwrap_or("disabled")
            .into();
        self.provider = value["asr_provider"].as_str().unwrap_or("").into();
        self.fallback_reason = value["asr_fallback_reason"].as_str().unwrap_or("").into();
        let partial = &value["asr_partial"];
        if partial.is_null() {
            self.text.clear();
        } else if partial["recording_generation"].as_u64() == Some(key.recording_generation) {
            if let (Some(revision), Some(text)) =
                (partial["revision"].as_u64(), partial["text"].as_str())
            {
                if self.revision.is_none_or(|current| revision > current) {
                    self.revision = Some(revision);
                    self.text = text.to_owned();
                }
            }
        }
        *self != previous
    }
}

pub enum CaptionMsg {
    Fetch,
    Result(u64, RecordingTicket, f64, ApiResult),
}

#[derive(Default)]
struct CaptionPoll {
    sequence: u64,
    pending: Option<u64>,
}
impl CaptionPoll {
    fn begin(&mut self) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.pending = Some(self.sequence);
        Some(self.sequence)
    }
    fn finish(&mut self, sequence: u64) -> bool {
        if self.pending != Some(sequence) {
            return false;
        }
        self.pending = None;
        true
    }
}

fn poll_delay_ms(
    current: Option<&RecordingTicket>,
    completed: &RecordingTicket,
    failed: bool,
    elapsed_ms: f64,
) -> Option<u32> {
    let current = current?;
    if current != completed {
        // A retired request held the only slot. Fetch the new binding now,
        // without inheriting either its normal cadence or its error backoff.
        return Some(0);
    }
    if failed {
        return Some(500);
    }
    let elapsed = if elapsed_ms.is_finite() {
        elapsed_ms.max(0.0)
    } else {
        0.0
    };
    Some((100.0 - elapsed).max(0.0).ceil() as u32)
}

pub struct LiveTranscript {
    caption: CaptionState,
    poll: CaptionPoll,
    timer: Option<Timeout>,
    unavailable: bool,
}
impl LiveTranscript {
    fn key(props: &CaptionProps) -> Option<RecordingTicket> {
        props.recording.clone()
    }
    fn schedule(&mut self, ctx: &Context<Self>, delay: Option<u32>) {
        if let Some(delay) = delay {
            if delay == 0 {
                ctx.link().send_message(CaptionMsg::Fetch);
                return;
            }
            let link = ctx.link().clone();
            self.timer = Some(Timeout::new(delay, move || {
                link.send_message(CaptionMsg::Fetch)
            }));
        }
    }
}
impl Component for LiveTranscript {
    type Message = CaptionMsg;
    type Properties = CaptionProps;
    fn create(ctx: &Context<Self>) -> Self {
        let mut caption = CaptionState::default();
        caption.bind(Self::key(ctx.props()));
        if caption.key.is_some() {
            ctx.link().send_message(CaptionMsg::Fetch);
        }
        Self {
            caption,
            poll: CaptionPoll::default(),
            timer: None,
            unavailable: false,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Self::Properties) -> bool {
        let key = Self::key(ctx.props());
        if key != self.caption.key {
            self.timer = None;
            self.caption.bind(key);
            self.unavailable = false;
            // Keep a prior in-flight request in its slot; its response cannot
            // update the new recording, and then polling can safely resume.
            if self.caption.key.is_some() && self.poll.pending.is_none() {
                ctx.link().send_message(CaptionMsg::Fetch);
            }
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: CaptionMsg) -> bool {
        match msg {
            CaptionMsg::Fetch => {
                let Some(key) = self.caption.key.clone() else {
                    return false;
                };
                let Some(sequence) = self.poll.begin() else {
                    return false;
                };
                self.timer = None;
                ctx.link().send_future(async move {
                    let started_ms = now();
                    let result = api("/api/audio/status", "GET", None).await;
                    CaptionMsg::Result(sequence, key, started_ms, result)
                });
                false
            }
            CaptionMsg::Result(sequence, key, started_ms, result) => {
                if !self.poll.finish(sequence) {
                    return false;
                }
                let failed = result.is_err();
                let mut changed = false;
                if self.caption.key.as_ref() == Some(&key) {
                    ctx.props().on_status.emit((key.clone(), result.clone()));
                    match result {
                        Ok(value) => {
                            changed = self.unavailable;
                            self.unavailable = false;
                            changed |= self.caption.apply(&key, &value);
                        }
                        Err(_) => {
                            changed = !self.unavailable;
                            self.unavailable = true;
                        }
                    }
                }
                self.schedule(
                    ctx,
                    poll_delay_ms(self.caption.key.as_ref(), &key, failed, now() - started_ms),
                );
                changed
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        if self.caption.key.is_none() {
            return Html::default();
        }
        let language = &ctx.props().ui_language;
        let state = self.caption.state.as_str();
        let status = if self.unavailable {
            "字幕状态暂不可用；结束录音后检查转写结果。"
        } else {
            match state {
                "finalizing" => "正在整理最终转写…",
                "disabled" | "fallback" => {
                    if ctx.props().automatic {
                        "正在整理最终转写…"
                    } else {
                        "松开后进行本地转写。"
                    }
                }
                "idle" => "等待本次录音的转写状态…",
                _ => "正在识别，临时文字可能修订…",
            }
        };
        html! {
            <section class="live-transcript" aria-label={t(language,"录音字幕（未发送）")}>
                <strong>{t(language,"录音字幕（未发送）")}</strong>
                <p class="live-transcript-status">{t(language,status)}</p>
                {if !self.caption.text.is_empty() {html!{<p class="live-transcript-text" aria-live="polite" aria-atomic="true">{&self.caption.text}</p>}} else {Html::default()}}
                <small>{if ctx.props().automatic {if language=="en" {"Experimental mode: only an untouched final transcript after a detected speech end is sent automatically. Stop or type to pause."} else {"实验模式：仅在检测到语句结束后自动发送未编辑的终稿；停止或输入文字可暂停。"}} else {t(language,"松开后检查并编辑，再发送。")}}</small>
                {if !self.caption.provider.is_empty() || !self.caption.fallback_reason.is_empty() {html!{
                    <details><summary>{t(language,"识别状态")}</summary><p>{&self.caption.provider}</p>
                        {if !self.caption.fallback_reason.is_empty() {html!{<p>{&self.caption.fallback_reason}</p>}} else {Html::default()}}
                    </details>
                }} else {Html::default()}}
            </section>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn key(generation: u64) -> RecordingTicket {
        RecordingTicket {
            session_id: "A".into(),
            session_generation: 5,
            language: "zh".into(),
            recording_generation: generation,
        }
    }
    fn partial(generation: u64, revision: u64, text: &str) -> Value {
        json!({"recording_generation":generation,"asr_streaming_state":"streaming",
            "asr_partial":{"recording_generation":generation,"revision":revision,"text":text}})
    }
    #[test]
    fn caption_poll_uses_start_cadence_without_overlapping_or_catch_up_requests() {
        let current = key(1);
        let mut poll = CaptionPoll::default();
        let first = poll.begin().unwrap();
        assert_eq!(poll.begin(), None, "only one request may be in flight");
        assert!(
            !poll.finish(first + 1),
            "a foreign result cannot release its slot"
        );
        assert_eq!(poll.begin(), None);
        assert!(poll.finish(first));
        assert_eq!(
            poll_delay_ms(Some(&current), &current, false, 24.2),
            Some(76)
        );
        assert_eq!(
            poll_delay_ms(Some(&current), &current, false, 100.0),
            Some(0)
        );
        assert_eq!(
            poll_delay_ms(Some(&current), &current, false, 350.0),
            Some(0)
        );
        let second = poll.begin().unwrap();
        assert_ne!(first, second);
        assert!(
            !poll.finish(first),
            "duplicate old result cannot free the new slot"
        );
        assert_eq!(poll.begin(), None);
        assert!(poll.finish(second));
        // A slow prior request does not earn repeated immediate polls.
        assert_eq!(
            poll_delay_ms(Some(&current), &current, false, 5.0),
            Some(95)
        );
        assert_eq!(
            poll_delay_ms(Some(&current), &current, true, 1500.0),
            Some(500)
        );
    }
    #[test]
    fn retired_recording_releases_slot_and_current_binding_resumes_immediately() {
        let old = key(1);
        let current = key(2);
        let mut state = CaptionState::default();
        state.bind(Some(old.clone()));
        let mut poll = CaptionPoll::default();
        let request = poll.begin().unwrap();
        state.bind(Some(current.clone()));
        assert_eq!(
            poll.begin(),
            None,
            "rebinding keeps the prior request in its slot"
        );
        assert!(poll.finish(request));
        assert!(!state.apply(&old, &partial(1, 9, "late old caption")));
        assert!(state.text.is_empty());
        assert_eq!(
            poll_delay_ms(state.key.as_ref(), &old, false, 10.0),
            Some(0)
        );
        assert_eq!(
            poll_delay_ms(state.key.as_ref(), &old, true, 1500.0),
            Some(0)
        );
        state.bind(None);
        assert_eq!(
            poll_delay_ms(state.key.as_ref(), &current, false, 10.0),
            None
        );
        assert_eq!(
            poll_delay_ms(state.key.as_ref(), &current, true, 1500.0),
            None
        );
    }
    #[test]
    fn captions_accept_revisions_only_for_the_current_recording() {
        let mut state = CaptionState::default();
        state.bind(Some(key(1)));
        assert!(state.apply(&key(1), &partial(1, 2, "你好 hello")));
        assert!(!state.apply(&key(1), &partial(1, 1, "旧字幕")));
        assert_eq!(state.text, "你好 hello");
        state.bind(Some(key(2)));
        assert!(!state.apply(&key(1), &partial(1, 3, "迟到字幕")));
        assert!(state.text.is_empty());
        assert!(state.apply(&key(2), &partial(2, 1, "第二次")));
        state.bind(None);
        assert!(state.text.is_empty());
        assert!(!state.apply(&key(2), &partial(2, 2, "结束后迟到")));
    }
    #[test]
    fn fallback_and_finalization_clear_partial_without_inventing_a_final_transcript() {
        let mut state = CaptionState::default();
        state.bind(Some(key(1)));
        state.apply(&key(1), &partial(1, 1, "还会修订"));
        state.apply(
            &key(1),
            &json!({"recording_generation":1,"asr_partial":null,
            "asr_streaming_state":"fallback","asr_provider":"Whisper local",
            "asr_fallback_reason":"stream disconnected"}),
        );
        assert!(state.text.is_empty());
        assert_eq!(state.fallback_reason, "stream disconnected");
        state.apply(&key(1), &partial(1, 1, "不应复活"));
        assert!(state.text.is_empty());
        state.apply(
            &key(1),
            &json!({"recording_generation":1,"asr_partial":null,
            "asr_streaming_state":"finalizing"}),
        );
        assert_eq!(state.state, "finalizing");
        assert!(state.text.is_empty());
    }
    #[test]
    fn captions_cannot_rebind_an_old_language_or_session_generation() {
        let old = key(1);
        let mut current = old.clone();
        current.language = "en-US".into();
        current.session_generation = 6;
        let mut state = CaptionState::default();
        state.bind(Some(current));
        assert!(!state.apply(&old, &partial(1, 9, "旧语音")));
        assert!(state.text.is_empty());
    }
}

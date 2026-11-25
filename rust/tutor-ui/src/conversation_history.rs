//! A read-only, bounded history page. It has no delivery, speech or send API.
use crate::api::{api, ApiResult};
use serde_json::Value;
use std::collections::VecDeque;
use web_sys::HtmlElement;
use yew::prelude::*;

const PAGE_SIZE: usize = 50;
const MAX_BACK_PAGES: usize = 64;
fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}
fn history_status(turn: &Value, language: &str) -> &'static str {
    if crate::tutor_stream_state::is_stream_turn(turn) {
        return if turn["stream_phase"] == "completed" {
            tr(language, "已完成的历史回复", "Completed historical reply")
        } else {
            tr(
                language,
                "回复未完成 · 已显示前缀",
                "Incomplete reply · displayed prefix",
            )
        };
    }
    match turn["status"].as_str().unwrap_or("") {
        "cancelled" => tr(language, "已取消", "Cancelled"),
        "generated" => tr(language, "当时尚未投递", "Not delivered at that time"),
        "speech_interrupted" => tr(language, "朗读曾被打断", "Speech was interrupted"),
        _ => tr(language, "历史消息", "Historical message"),
    }
}

#[derive(Clone, PartialEq, Debug)]
struct Owner {
    learner: String,
    session: String,
    learner_epoch: i64,
}
#[derive(Clone, Copy, Debug)]
struct Position {
    before: Option<i64>,
    top: i32,
}
struct HistoryPages {
    owner: Owner,
    boundary: Option<i64>,
    current: Position,
    previous: VecDeque<Position>,
    page: usize,
    items: Vec<Value>,
    next_before: Option<i64>,
    revision: u64,
    loading: bool,
    loaded: bool,
    error: bool,
    restore_top: Option<i32>,
}
impl HistoryPages {
    fn new(owner: Owner, before: Option<i64>) -> Self {
        Self {
            owner,
            boundary: before,
            current: Position { before, top: 0 },
            previous: VecDeque::new(),
            page: 1,
            items: vec![],
            next_before: None,
            revision: 0,
            loading: false,
            loaded: false,
            error: false,
            restore_top: None,
        }
    }
    fn bind(&mut self, owner: Owner, before: Option<i64>) {
        if self.owner != owner || self.boundary != before {
            let revision = self.revision.wrapping_add(1);
            *self = Self::new(owner, before);
            self.revision = revision;
        }
    }
    fn needs_request(&self) -> bool {
        !self.loading && !self.loaded && !self.error
    }
    fn begin(&mut self) -> Option<u64> {
        if !self.needs_request() {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        self.loading = true;
        Some(self.revision)
    }
    fn complete(&mut self, revision: u64, result: ApiResult) -> bool {
        if revision != self.revision || !self.loading {
            return false;
        }
        self.loading = false;
        let Ok(value) = result else {
            self.error = true;
            return true;
        };
        let valid_cursor = value["next_before"].is_null()
            || value["next_before"].as_i64().is_some_and(|next| {
                next > 0 && self.current.before.is_none_or(|before| next < before)
            });
        if value["learner_id"] != self.owner.learner
            || value["session_id"] != self.owner.session
            || !valid_cursor
            || !value["items"]
                .as_array()
                .is_some_and(|items| items.len() <= PAGE_SIZE)
        {
            self.error = true;
            return true;
        }
        self.items = value["items"].as_array().unwrap().clone();
        self.next_before = value["next_before"].as_i64();
        self.loaded = true;
        self.restore_top = Some(self.current.top);
        true
    }
    fn clear_page(&mut self) {
        self.items.clear();
        self.next_before = None;
        self.loaded = false;
        self.error = false;
        self.restore_top = None;
    }
    fn older(&mut self, top: i32) -> bool {
        let Some(before) = self.next_before.filter(|_| !self.loading && self.loaded) else {
            return false;
        };
        self.current.top = top.max(0);
        self.previous.push_back(self.current);
        if self.previous.len() > MAX_BACK_PAGES {
            self.previous.pop_front();
        }
        self.current = Position {
            before: Some(before),
            top: 0,
        };
        self.page += 1;
        self.clear_page();
        true
    }
    fn newer(&mut self) -> bool {
        if self.loading {
            return false;
        }
        let Some(position) = self.previous.pop_back() else {
            return false;
        };
        self.current = position;
        self.page -= 1;
        self.clear_page();
        true
    }
}

#[derive(Properties, PartialEq)]
pub struct HistoryProps {
    pub learner_id: String,
    pub session_id: String,
    pub learner_epoch: i64,
    pub before: Option<i64>,
    pub ui_language: String,
    #[prop_or_default]
    pub other_activity: bool,
}
pub enum HistoryMsg {
    Fetch,
    Result(u64, ApiResult),
    Older,
    Newer,
    Retry,
}
pub struct ConversationHistory {
    pages: HistoryPages,
    pending: Option<u64>,
    viewport: NodeRef,
}
impl ConversationHistory {
    fn owner(props: &HistoryProps) -> Owner {
        Owner {
            learner: props.learner_id.clone(),
            session: props.session_id.clone(),
            learner_epoch: props.learner_epoch,
        }
    }
}
impl Component for ConversationHistory {
    type Message = HistoryMsg;
    type Properties = HistoryProps;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(HistoryMsg::Fetch);
        Self {
            pages: HistoryPages::new(Self::owner(ctx.props()), ctx.props().before),
            pending: None,
            viewport: NodeRef::default(),
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Self::Properties) -> bool {
        self.pages
            .bind(Self::owner(ctx.props()), ctx.props().before);
        if self.pages.needs_request() {
            ctx.link().send_message(HistoryMsg::Fetch);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: HistoryMsg) -> bool {
        self.pages
            .bind(Self::owner(ctx.props()), ctx.props().before);
        match msg {
            HistoryMsg::Fetch => {
                if self.pending.is_some() {
                    return false;
                }
                let Some(revision) = self.pages.begin() else {
                    return false;
                };
                let mut path = format!(
                    "/api/learners/{}/sessions/{}/history?limit={PAGE_SIZE}",
                    self.pages.owner.learner, self.pages.owner.session
                );
                if let Some(before) = self.pages.current.before {
                    path.push_str(&format!("&before={before}"));
                }
                self.pending = Some(revision);
                ctx.link().send_future(async move {
                    HistoryMsg::Result(revision, api(&path, "GET", None).await)
                });
                true
            }
            HistoryMsg::Result(revision, result) => {
                if self.pending != Some(revision) {
                    return false;
                }
                self.pending = None;
                let changed = self.pages.complete(revision, result);
                if self.pages.needs_request() {
                    ctx.link().send_message(HistoryMsg::Fetch);
                }
                changed
            }
            HistoryMsg::Older => {
                let top = self
                    .viewport
                    .cast::<HtmlElement>()
                    .map(|node| node.scroll_top())
                    .unwrap_or(0);
                if !self.pages.older(top) {
                    return false;
                }
                ctx.link().send_message(HistoryMsg::Fetch);
                true
            }
            HistoryMsg::Newer => {
                if !self.pages.newer() {
                    return false;
                }
                ctx.link().send_message(HistoryMsg::Fetch);
                true
            }
            HistoryMsg::Retry => {
                if self.pending.is_some() {
                    return false;
                }
                self.pages.clear_page();
                ctx.link().send_message(HistoryMsg::Fetch);
                true
            }
        }
    }
    fn rendered(&mut self, _: &Context<Self>, _: bool) {
        if let Some(top) = self.pages.restore_top.take() {
            if let Some(viewport) = self.viewport.cast::<HtmlElement>() {
                viewport.set_scroll_top(top);
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let language = &ctx.props().ui_language;
        html! {
            <section class="conversation-history" aria-label={tr(language,"历史记录（只读）","Conversation history (read only)")}>
                <div class="history-caption"><strong>{tr(language,"历史记录 · 只读","History · Read only")}</strong><span>{if language=="en" {format!("Page {}",self.pages.page)}else{format!("第 {} 页",self.pages.page)}}</span></div>
                <p class="history-note">{if ctx.props().other_activity {tr(language,"这些记录属于过去的活动。下方草稿仍发送到当前活动，不会发到此历史活动。","These records belong to a past activity. The draft below still goes to your current activity, not this historical one.")}else{tr(language,"此页是更早的记录。下方草稿仍发送到当前活动，不会重新提交历史消息。","These are earlier records. The draft below still belongs to the current session; history is not resubmitted.")}}</p>
                <div class="history-controls">
                    <button class="text-button" disabled={self.pages.previous.is_empty()||self.pages.loading} onclick={ctx.link().callback(|_|HistoryMsg::Newer)}>{tr(language,"较新一页","Newer page")}</button>
                    <button class="text-button" disabled={self.pages.next_before.is_none()||self.pages.loading} onclick={ctx.link().callback(|_|HistoryMsg::Older)}>{tr(language,"更早一页","Older page")}</button>
                </div>
                <div class="conversation history-conversation" ref={self.viewport.clone()} tabindex="0">
                    {if self.pages.loading {html!{<p class="history-note" role="status">{tr(language,"正在读取历史记录…","Loading conversation history…")}</p>}}
                    else if self.pages.error {html!{<p class="history-note" role="status">{tr(language,"历史记录暂不可用。可以重试，或返回当前消息。","History is unavailable. Retry or return to current messages.")}<button class="text-button" onclick={ctx.link().callback(|_|HistoryMsg::Retry)}>{tr(language,"重试","Retry")}</button></p>}}
                    else if self.pages.loaded && self.pages.items.is_empty() {html!{<p class="history-note">{if ctx.props().other_activity{tr(language,"这次活动没有聊天消息。实验比较记录可在上方查看。","This activity has no chat messages. Its experiment comparisons are shown above.")}else{tr(language,"没有更早的聊天消息。","There are no earlier chat messages.")}}</p>}}
                    else {Html::default()}}
                    {for self.pages.items.iter().map(|turn| {
                        let assistant=turn["role"]=="assistant";
                        let status=history_status(turn,language);
                        html!{<div class={classes!("message",if assistant{"tutor-message"}else{"learner-message"},(turn["status"]=="cancelled").then_some("cancelled"))} key={turn["turn_id"].as_str().unwrap_or("").to_owned()}>
                            <div class="message-meta"><span>{if assistant{"AI Tutor"}else{tr(language,"学习者","Learner")}}</span><small>{status}</small></div>
                            <div class="message-bubble">{turn["content"].as_str().unwrap_or("")}</div>
                        </div>}
                    })}
                </div>
            </section>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn historical_stream_prefix_is_not_presented_as_a_completed_reply() {
        for phase in [
            "generating",
            "final_pending",
            "failed",
            "cancelled",
            "interrupted",
        ] {
            let turn = json!({"delivery_mode":"segments","stream_phase":phase,"status":"shown"});
            assert_eq!(
                history_status(&turn, "en"),
                "Incomplete reply · displayed prefix"
            );
            assert_eq!(history_status(&turn, "zh"), "回复未完成 · 已显示前缀");
        }
        assert_eq!(
            history_status(
                &json!({"delivery_mode":"segments","stream_phase":"completed"}),
                "en"
            ),
            "Completed historical reply"
        );
        assert_eq!(
            history_status(&json!({"status":"generated"}), "en"),
            "Not delivered at that time"
        );
    }
    fn owner(session: &str) -> Owner {
        Owner {
            learner: "learner".into(),
            session: session.into(),
            learner_epoch: 0,
        }
    }
    fn page(session: &str, next: Option<i64>) -> ApiResult {
        Ok(
            json!({"learner_id":"learner","session_id":session,"items":[{"turn_id":"old","content":"历史原句"}],"next_before":next}),
        )
    }
    #[test]
    fn old_session_reply_cannot_fill_a_new_history_window() {
        let mut pages = HistoryPages::new(owner("a"), Some(200));
        let revision = pages.begin().unwrap();
        pages.bind(owner("b"), Some(100));
        assert!(!pages.complete(revision, page("a", Some(150))));
        assert!(pages.items.is_empty());
        let revision = pages.begin().unwrap();
        assert!(pages.complete(revision, page("a", Some(50))));
        assert!(pages.error && pages.items.is_empty());
    }
    #[test]
    fn changed_learner_epoch_clears_same_session_history_and_rejects_old_results() {
        let mut pages = HistoryPages::new(owner("a"), Some(200));
        let old = pages.begin().unwrap();
        assert!(pages.complete(old, page("a", Some(150))));
        assert!(!pages.items.is_empty());
        let mut updated = owner("a");
        updated.learner_epoch = 1;
        pages.bind(updated, Some(200));
        assert!(pages.items.is_empty());
        assert!(pages.next_before.is_none());
        assert!(!pages.complete(old, page("a", Some(150))));
        let current = pages.begin().unwrap();
        assert!(!pages.complete(old, page("a", Some(150))));
        assert!(pages.items.is_empty());
        assert!(pages.complete(current, page("a", None)));
    }
    #[test]
    fn paging_restores_scroll_and_keeps_the_original_boundary() {
        let mut pages = HistoryPages::new(owner("a"), Some(200));
        let revision = pages.begin().unwrap();
        pages.complete(revision, page("a", Some(150)));
        assert!(pages.older(347));
        assert_eq!(pages.current.before, Some(150));
        assert!(pages.items.is_empty());
        let revision = pages.begin().unwrap();
        pages.complete(revision, page("a", Some(100)));
        assert!(pages.newer());
        assert_eq!(pages.current.before, Some(200));
        let revision = pages.begin().unwrap();
        pages.complete(revision, page("a", Some(150)));
        assert_eq!(pages.restore_top, Some(347));
    }
    #[test]
    fn history_bounds_both_page_text_and_back_navigation_metadata() {
        let mut pages = HistoryPages::new(owner("a"), Some(1000));
        for next in (900..980).rev() {
            let revision = pages.begin().unwrap();
            pages.complete(revision, page("a", Some(next)));
            assert!(pages.older(0));
            assert!(pages.previous.len() <= MAX_BACK_PAGES);
            assert!(pages.items.is_empty());
        }
        let revision = pages.begin().unwrap();
        let oversized = json!({"learner_id":"learner","session_id":"a","items":vec![json!({});51],"next_before":null});
        pages.complete(revision, Ok(oversized));
        assert!(pages.error && pages.items.is_empty());
    }
    #[test]
    fn changed_boundary_and_late_previous_page_do_not_replace_current_results() {
        let mut pages = HistoryPages::new(owner("a"), Some(200));
        let original = pages.begin().unwrap();
        pages.bind(owner("a"), Some(300));
        let current = pages.begin().unwrap();
        assert!(!pages.complete(original, page("a", Some(150))));
        assert!(pages.loading && pages.items.is_empty());
        assert!(pages.complete(current, page("a", Some(250))));
        assert_eq!(pages.current.before, Some(300));
        assert!(pages.older(90));
        let older = pages.begin().unwrap();
        assert!(!pages.complete(current, page("a", Some(250))));
        assert!(pages.loading && pages.items.is_empty());
        assert!(pages.complete(older, page("a", Some(200))));
        assert_eq!(pages.current.before, Some(250));
        assert_eq!(pages.boundary, Some(300));
    }
    #[test]
    fn page_cursor_must_make_progress_and_null_marks_the_end() {
        for next in [200, 201, 0, -1] {
            let mut pages = HistoryPages::new(owner("a"), Some(200));
            let revision = pages.begin().unwrap();
            pages.complete(revision, page("a", Some(next)));
            assert!(pages.error && pages.items.is_empty());
            assert!(!pages.older(0));
        }
        let mut pages = HistoryPages::new(owner("a"), Some(200));
        let revision = pages.begin().unwrap();
        assert!(pages.complete(revision, page("a", None)));
        assert!(pages.loaded && !pages.error);
        assert!(!pages.older(0));
    }
}

//! Explicit, bounded review of background memory suggestions.
use crate::{
    api::{api, ApiResult},
    evidence_labels::source_label,
    memory_evidence::{kind_label, scope_label},
    view::icon,
};
use gloo_timers::callback::Timeout;
use serde_json::{json, Value};
use web_sys::HtmlElement;
use yew::prelude::*;

fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub learner_id: String,
    pub namespace: String,
    pub session_id: String,
    pub epoch: i64,
    pub generation: i64,
    pub state_revision: i64,
    pub ui_language: String,
    pub enabled: bool,
    pub on_retained: Callback<()>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    learner: String,
    namespace: String,
    session: String,
    epoch: i64,
    generation: i64,
    state_revision: i64,
}
impl Binding {
    fn from_props(props: &Props) -> Self {
        Self {
            learner: props.learner_id.clone(),
            namespace: props.namespace.clone(),
            session: props.session_id.clone(),
            epoch: props.epoch,
            generation: props.generation,
            state_revision: props.state_revision,
        }
    }
    fn valid(&self) -> bool {
        !self.learner.is_empty()
            && !self.session.is_empty()
            && matches!(self.namespace.as_str(), "real" | "simulator")
            && self.epoch >= 0
            && self.generation >= 0
            && self.state_revision >= 0
    }
    fn path(&self) -> String {
        format!(
            "/api/learners/{}/sessions/{}/memory-review",
            self.learner, self.session
        )
    }
    fn matches(&self, value: &Value) -> bool {
        value["learner_id"] == self.learner
            && value["session_id"] == self.session
            && value["namespace"] == self.namespace
            && value["epoch"].as_i64() == Some(self.epoch)
            && value["generation"].as_i64() == Some(self.generation)
            && value["state_revision"].as_i64() == Some(self.state_revision)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Start,
    Cancel,
    Retain(String),
    Dismiss(String),
}
impl Action {
    fn request(&self, binding: &Binding) -> (String, Value) {
        let path = binding.path();
        match self {
            Self::Start => (
                path,
                json!({"expected_generation":binding.generation,
                "expected_epoch":binding.epoch,"expected_state_revision":binding.state_revision}),
            ),
            Self::Cancel => (format!("{path}/cancel"), json!({})),
            Self::Retain(id) => (
                format!("{path}/candidates/{id}"),
                json!({"decision":"retain"}),
            ),
            Self::Dismiss(id) => (
                format!("{path}/candidates/{id}"),
                json!({"decision":"dismiss"}),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    sequence: u64,
    context: u64,
    binding: Binding,
}
enum ReviewError {
    Read(String),
    Action(String),
}

struct ReviewState {
    binding: Binding,
    enabled: bool,
    open: bool,
    context: u64,
    sequence: u64,
    value: Option<Value>,
    read_pending: Option<Ticket>,
    action_pending: Option<(Ticket, Action)>,
    needs_read: bool,
    observing: bool,
    manual_cancellable: bool,
    error: Option<ReviewError>,
    notice: Option<Action>,
}
impl ReviewState {
    fn new(binding: Binding, enabled: bool) -> Self {
        Self {
            binding,
            enabled,
            open: false,
            context: 0,
            sequence: 0,
            value: None,
            read_pending: None,
            action_pending: None,
            needs_read: false,
            observing: false,
            manual_cancellable: false,
            error: None,
            notice: None,
        }
    }
    fn visible(&self) -> bool {
        self.open && self.enabled && self.binding.valid()
    }
    fn clear_context(&mut self) {
        self.context = self.context.wrapping_add(1);
        self.value = None;
        self.error = None;
        self.notice = None;
        self.observing = false;
        self.manual_cancellable = false;
        self.needs_read = self.visible();
    }
    fn bind(&mut self, binding: Binding, enabled: bool) {
        if self.binding != binding || self.enabled != enabled {
            self.binding = binding;
            self.enabled = enabled;
            self.clear_context();
        }
    }
    fn toggle(&mut self, open: bool) {
        if self.open != open {
            self.open = open;
            self.clear_context();
        }
    }
    fn ticket(&mut self) -> Ticket {
        self.sequence = self.sequence.wrapping_add(1);
        Ticket {
            sequence: self.sequence,
            context: self.context,
            binding: self.binding.clone(),
        }
    }
    fn current(&self, ticket: &Ticket) -> bool {
        self.visible() && ticket.context == self.context && ticket.binding == self.binding
    }
    fn begin_read(&mut self) -> Option<Ticket> {
        if !self.visible()
            || !self.needs_read
            || self.read_pending.is_some()
            || self.action_pending.is_some()
        {
            return None;
        }
        let ticket = self.ticket();
        self.read_pending = Some(ticket.clone());
        self.needs_read = false;
        Some(ticket)
    }
    fn read(&mut self, ticket: &Ticket, result: ApiResult) {
        if self.read_pending.as_ref() != Some(ticket) {
            return;
        }
        self.read_pending = None;
        if !self.current(ticket) {
            return;
        }
        match result {
            Ok(value) if self.binding.matches(&value) => {
                self.manual_cancellable = value["review_running"] == true;
                self.observing = self.manual_cancellable
                    || (value["job"]["status"] == "leased" && value["worker_busy"] == true);
                self.value = Some(value);
                if matches!(self.error, Some(ReviewError::Read(_))) {
                    self.error = None;
                }
            }
            Ok(_) => {
                self.value = None;
                self.observing = false;
                self.manual_cancellable = false;
                self.error = Some(ReviewError::Read(String::new()));
            }
            Err(detail) => {
                self.value = None;
                self.error = Some(ReviewError::Read(detail));
            }
        }
    }
    fn can_act(&self, action: &Action) -> bool {
        if !self.visible() || self.action_pending.is_some() {
            return false;
        }
        if matches!(action, Action::Cancel) {
            return self.manual_cancellable;
        }
        let Some(value) = &self.value else {
            return false;
        };
        match action {
            Action::Start => {
                !self.observing
                    && value["provider_available"] == true
                    && value["worker_busy"] != true
                    && value["candidates"].as_array().is_some_and(Vec::is_empty)
            }
            Action::Cancel => false,
            Action::Retain(id) | Action::Dismiss(id) => value["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|candidate| {
                    candidate["candidate_id"] == *id
                        && (matches!(action, Action::Dismiss(_)) || candidate["can_retain"] == true)
                }),
        }
    }
    fn begin_action(&mut self, action: Action) -> Option<Ticket> {
        if !self.can_act(&action) {
            return None;
        }
        // An older GET must not put a just-reviewed candidate back on screen.
        self.context = self.context.wrapping_add(1);
        let ticket = self.ticket();
        if matches!(action, Action::Start) {
            self.manual_cancellable = true;
        }
        self.action_pending = Some((ticket.clone(), action));
        self.needs_read = false;
        self.error = None;
        self.notice = None;
        Some(ticket)
    }
    fn finish_action(&mut self, ticket: &Ticket, action: &Action, result: ApiResult) -> bool {
        if self.action_pending.as_ref() != Some(&(ticket.clone(), action.clone())) {
            return false;
        }
        self.action_pending = None;
        if !self.current(ticket) {
            // Collapsing the panel revokes its source view, not an already
            // accepted save for this unchanged owner. Refresh saved memories
            // without reopening or repopulating the review.
            return ticket.binding == self.binding
                && result.is_ok()
                && matches!(action, Action::Retain(_));
        }
        self.value = None;
        self.needs_read = true;
        match result {
            Ok(_) => {
                self.notice = Some(action.clone());
                self.error = None;
                if matches!(action, Action::Start | Action::Cancel) {
                    self.observing = true;
                }
                matches!(action, Action::Retain(_))
            }
            Err(detail) => {
                self.error = Some(ReviewError::Action(detail));
                // Only a GET follows an uncertain POST. Another model request
                // always requires another explicit click.
                false
            }
        }
    }
    fn should_poll(&self) -> bool {
        self.visible() && self.observing && self.action_pending.is_none()
    }
}

pub enum Msg {
    Toggle(bool),
    Refresh,
    Poll,
    Read(Ticket, ApiResult),
    Act(Action),
    Done(Ticket, Action, ApiResult),
}
pub struct MemoryReview {
    state: ReviewState,
    timer: Option<Timeout>,
    node: NodeRef,
}
impl MemoryReview {
    fn displayed(&self) -> bool {
        let page_visible = web_sys::window()
            .and_then(|window| window.document())
            .is_some_and(|document| !document.hidden());
        page_visible
            && self
                .node
                .cast::<HtmlElement>()
                .is_some_and(|node| node.offset_width() > 0 && node.offset_height() > 0)
    }
    fn drive_read(&mut self, ctx: &Context<Self>) {
        if !self.displayed() {
            return;
        }
        if let Some(ticket) = self.state.begin_read() {
            self.timer = None;
            let path = ticket.binding.path();
            ctx.link()
                .send_future(async move { Msg::Read(ticket, api(&path, "GET", None).await) });
        }
    }
    fn schedule(&mut self, ctx: &Context<Self>) {
        if !self.state.visible() || (!self.state.observing && !self.state.needs_read) {
            self.timer = None;
        } else if self.timer.is_none()
            && self.state.read_pending.is_none()
            && self.state.action_pending.is_none()
        {
            let link = ctx.link().clone();
            self.timer = Some(Timeout::new(2000, move || link.send_message(Msg::Poll)));
        }
    }
}
impl Component for MemoryReview {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        Self {
            state: ReviewState::new(Binding::from_props(ctx.props()), ctx.props().enabled),
            timer: None,
            node: NodeRef::default(),
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Props) -> bool {
        self.state
            .bind(Binding::from_props(ctx.props()), ctx.props().enabled);
        self.drive_read(ctx);
        self.schedule(ctx);
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        self.state
            .bind(Binding::from_props(ctx.props()), ctx.props().enabled);
        match msg {
            Msg::Toggle(open) => self.state.toggle(open),
            Msg::Refresh => {
                self.state.error = None;
                self.state.needs_read = self.state.visible();
            }
            Msg::Poll => {
                self.timer = None;
                self.state.needs_read |= self.state.should_poll();
                if !self.displayed() {
                    self.state.value = None;
                }
            }
            Msg::Read(ticket, result) => self.state.read(&ticket, result),
            Msg::Act(action) => {
                let Some(ticket) = self.state.begin_action(action.clone()) else {
                    return false;
                };
                let (path, body) = action.request(&ticket.binding);
                ctx.link().send_future(async move {
                    Msg::Done(ticket, action, api(&path, "POST", Some(body)).await)
                });
            }
            Msg::Done(ticket, action, result) => {
                if self.state.finish_action(&ticket, &action, result) {
                    ctx.props().on_retained.emit(());
                }
            }
        }
        self.drive_read(ctx);
        self.schedule(ctx);
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let language = ctx.props().ui_language.as_str();
        let value = self.state.value.as_ref();
        let pending = value.is_some_and(|value| {
            value["job"]["status"] == "pending"
                || (value["job"]["status"] == "leased" && !self.state.observing)
        });
        let busy = self.state.action_pending.is_some();
        html! {
            <details class="memory-review" ref={self.node.clone()} open={self.state.open}
                ontoggle={ctx.link().batch_callback(|event:Event|{let element=event.target_unchecked_into::<HtmlElement>();(element.get_attribute("class").as_deref()==Some("memory-review")).then(||Msg::Toggle(element.has_attribute("open")))})}>
                <summary><span>{icon("memory")}{tr(language,"从本次学习中，挑选值得留下的线索","Choose what to remember from this activity")}</span>{icon("chevron")}</summary>
                {if self.state.open {html!{<div class="memory-review-content">
                    <p class="memory-review-intro">{tr(language,"本次来源文本由 Kimi 在后台整理。每条摘要只有经你确认保留后，才会进入学习记忆。","Kimi organizes this activity's source text in the background. A summary becomes a learning memory only when you choose to keep it.")}</p>
                    <div class="memory-review-actions"><button class="secondary" disabled={!self.state.can_act(&Action::Start)} onclick={ctx.link().callback(|_|Msg::Act(Action::Start))}>{icon("sparkle")}{if pending{tr(language,"继续整理本次线索","Continue organizing this activity")}else{tr(language,"整理本次学习线索","Organize this activity's learning notes")}}</button>
                    {if self.state.manual_cancellable {html!{<button class="text-button" disabled={!self.state.can_act(&Action::Cancel)} onclick={ctx.link().callback(|_|Msg::Act(Action::Cancel))}>{tr(language,"取消这次整理","Cancel this review")}</button>}}else{Html::default()}}
                    <button class="text-button" disabled={!self.state.visible()||busy||self.state.read_pending.is_some()} onclick={ctx.link().callback(|_|Msg::Refresh)}>{tr(language,"刷新","Refresh")}</button></div>
                    <p class="memory-review-note">{tr(language,"取消只停止这次后台整理，不影响当前对话。","Canceling stops only this background review; your current conversation continues.")}</p>
                    {if !self.state.enabled || !self.state.binding.valid() {html!{<p class="memory-review-status" role="status">{tr(language,"请先选择学习者和活动，待当前操作结束后查看。","Select a learner and activity, then wait for the current operation to finish.")}</p>}}else if let Some(value)=value{html!{
                        <><p class="memory-review-status" role="status">{status_label(language,value)}</p>
                        {if value["provider_available"]!=true{html!{<p class="memory-review-note">{tr(language,"后台整理服务尚未就绪；已有线索与学习记忆仍可查看。","The background service is not ready. Existing notes and memories remain available.")}</p>}}else if value["worker_busy"]==true && !self.state.observing{html!{<p class="memory-review-note">{tr(language,"后台正在处理另一项整理，请稍后再试。","Another background review is running. Try again shortly.")}</p>}}else{Html::default()}}
                        {for value["candidates"].as_array().into_iter().flatten().map(|candidate| render_candidate(ctx,&self.state,candidate))}
                        {if value["has_more_sources"]==true{html!{<p class="memory-review-note">{tr(language,"还有尚未整理的学习线索，可以继续整理。","More source notes are available to organize.")}</p>}}else{Html::default()}}</>
                    }}else if self.state.read_pending.is_some(){html!{<p class="memory-review-status" role="status">{tr(language,"正在读取本次整理状态…","Loading this activity's review…")}</p>}}else{Html::default()}}
                    {if let Some(error)=&self.state.error {let (action,detail)=match error{ReviewError::Read(detail)=>(false,detail),ReviewError::Action(detail)=>(true,detail)};html!{<div class="memory-review-error" role="status"><p>{if action{tr(language,"上次操作未取得确认。请核对最新状态；整理不会自动重发。","The last action was not confirmed. Check the latest status; organizing is never restarted automatically.")}else{tr(language,"当前来源或整理状态暂不可用，请刷新后核对。","The sources or review status are unavailable. Refresh to check again.")}}</p>{if !detail.is_empty(){html!{<details><summary>{tr(language,"查看服务详情","View service details")}</summary><p>{detail}</p></details>}}else{Html::default()}}</div>}}else{Html::default()}}
                    {if let Some(action)=&self.state.notice {let label=match action{Action::Retain(_)=>Some(tr(language,"已保留到学习记忆，可在下方更正或忘记。","Kept in learning memories. You can correct or forget it below.")),Action::Dismiss(_)=>Some(tr(language,"本次未保留这条摘要，原始学习记录仍保留。","This summary was skipped for this review. Its original learning records remain.")),_=>None};label.map(|label|html!{<p class="memory-review-saved" role="status">{icon("check")}{label}</p>}).unwrap_or_default()}else{Html::default()}}
                </div>}}else{Html::default()}}
            </details>
        }
    }
}

fn status_label(language: &str, value: &Value) -> String {
    if value["review_running"] == true
        || (value["job"]["status"] == "leased" && value["worker_busy"] == true)
    {
        return tr(
            language,
            "正在后台整理，你可以继续当前学习。",
            "Organizing in the background. You can continue learning.",
        )
        .into();
    }
    let count = value["candidates"].as_array().map_or(0, Vec::len);
    if count > 0 {
        return if language == "en" {
            format!("{count} notes are ready for your review.")
        } else {
            format!("有 {count} 条线索等待你核对。")
        };
    }
    if value["review_outcome"] == "failed" {
        return tr(
            language,
            "这次整理未完成；需要时可以明确继续。",
            "This review did not finish. You can choose to continue it.",
        )
        .into();
    }
    match value["job"]["status"].as_str() {
        Some("pending" | "leased") => tr(
            language,
            "整理已暂停；需要时点击继续，不会自动重试。",
            "Review is paused. Choose Continue when ready; it will not retry automatically.",
        ),
        Some("completed" | "applied" | "shadow_ready") => tr(
            language,
            "本次整理已完成，没有等待确认的线索。",
            "This review is complete. No notes are waiting for confirmation.",
        ),
        Some("exhausted") => tr(
            language,
            "本次整理已达到尝试上限，尚未保存新的学习记忆。",
            "This review reached its attempt limit. No new learning memory was saved.",
        ),
        Some("invalidated") => tr(
            language,
            "学习来源已变化，请根据当前活动重新整理。",
            "The learning sources changed. Organize the current activity again.",
        ),
        _ => tr(
            language,
            "先整理本次学习线索，再逐条选择是否保留。",
            "Organize this activity's notes, then choose which ones to keep.",
        ),
    }
    .into()
}
fn reasons(language: &str, payload: &Value) -> Vec<&'static str> {
    [
        (
            "goal_relevance",
            "与学习目标相关",
            "Relevant to the learning goal",
        ),
        ("continuity", "便于之后继续", "Useful for continuing later"),
        ("novel_change", "记录了新的变化", "Records a new change"),
        (
            "explicit_request",
            "来源中明确要求保留",
            "Explicitly requested in the source",
        ),
    ]
    .iter()
    .filter_map(|&(key, zh, en)| {
        payload["importance"][key]
            .as_u64()
            .filter(|score| (1..=3).contains(score))
            .map(|_| tr(language, zh, en))
    })
    .collect()
}
fn source_text(content: &Value) -> Vec<&str> {
    let mut quotes = vec![];
    for key in ["text", "report", "correction"] {
        if let Some(text) = content[key].as_str().filter(|text| !text.trim().is_empty()) {
            if !quotes.contains(&text) {
                quotes.push(text);
            }
        }
    }
    quotes
}
fn source_date(source: &Value) -> &str {
    source["date"]
        .as_str()
        .or_else(|| source["created_at"].as_str())
        .and_then(|date| date.get(..10))
        .filter(|date| {
            date.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        })
        .unwrap_or("")
}
fn render_source(language: &str, namespace: &str, source: &Value) -> Html {
    let grade = if namespace == "simulator" {
        "simulated"
    } else {
        source["source_type"].as_str().unwrap_or("")
    };
    let quotes = source_text(&source["content"]);
    html! {<section>
        <div><span class="source">{source_label(language,grade)}</span><time>{source_date(source)}</time></div>
        {if quotes.is_empty() {
            html! {<p>{tr(language,"这条来源没有可展示的原句。","This source has no original wording to display.")}</p>}
        } else {
            html! {for quotes.iter().map(|quote|html!{<blockquote>{*quote}</blockquote>})}
        }}
    </section>}
}
fn render_candidate(ctx: &Context<MemoryReview>, state: &ReviewState, candidate: &Value) -> Html {
    let language = ctx.props().ui_language.as_str();
    let payload = &candidate["payload"];
    let id = candidate["candidate_id"].as_str().unwrap_or("").to_owned();
    let retain = Action::Retain(id.clone());
    let dismiss = Action::Dismiss(id);
    let can_retain = state.can_act(&retain);
    let can_dismiss = state.can_act(&dismiss);
    let grade = if state.binding.namespace == "simulator" {
        "simulated"
    } else {
        payload["source_grade"].as_str().unwrap_or("")
    };
    let why = reasons(language, payload);
    html! {<article class="memory-review-candidate">
        <div class="memory-review-candidate-meta"><strong>{kind_label(language,payload["kind"].as_str().unwrap_or(""))}</strong><span class={classes!("source",(grade=="simulated").then_some("simulation"))}>{source_label(language,grade)}</span></div>
        <p class="memory-review-original">{payload["content"].as_str().unwrap_or("")}</p>
        <p class="memory-review-scope">{scope_label(language,payload["scope"].as_str().unwrap_or(""))}</p>
        <div class="memory-review-reasons"><small>{tr(language,"为什么值得回顾","Why it may be useful")}</small>{if why.is_empty(){html!{<p>{tr(language,"未记录具体保留理由，请结合原句判断。","No specific reason was recorded. Review the original wording before deciding.")}</p>}}else{html!{<ul>{for why.iter().map(|reason|html!{<li>{*reason}</li>})}</ul>}}}</div>
        <details class="memory-review-sources"><summary>{tr(language,"核对原始来源","Check the original sources")}</summary>{for candidate["sources"].as_array().into_iter().flatten().map(|source|render_source(language,&state.binding.namespace,source))}</details>
        {if candidate["can_retain"]!=true{html!{<div class="memory-review-blocked"><p>{tr(language,"来源条件尚未满足，暂不能保留。","The source requirements are not yet met, so this note cannot be kept.")}</p>{candidate["blocked_reason"].as_str().filter(|reason|!reason.is_empty()).map(|reason|html!{<details><summary>{tr(language,"查看原因","View the reason")}</summary><p>{reason}</p></details>}).unwrap_or_default()}</div>}}else{Html::default()}}
        <div class="memory-review-candidate-actions"><button class="primary" disabled={!can_retain} onclick={ctx.link().callback(move |_|Msg::Act(retain.clone()))}>{tr(language,"保留到学习记忆","Keep in learning memories")}</button><button class="text-button" disabled={!can_dismiss} onclick={ctx.link().callback(move |_|Msg::Act(dismiss.clone()))}>{tr(language,"本次不保留","Skip this summary")}</button></div>
    </article>}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        Binding {
            learner: "l".into(),
            namespace: "simulator".into(),
            session: "s".into(),
            epoch: 2,
            generation: 4,
            state_revision: 6,
        }
    }
    fn snapshot() -> Value {
        json!({"learner_id":"l","session_id":"s","namespace":"simulator","epoch":2,"generation":4,"state_revision":6,"job":null,"candidates":[],"provider_available":true,"worker_busy":false,"review_running":false})
    }
    fn loaded() -> ReviewState {
        let mut state = ReviewState::new(binding(), true);
        state.toggle(true);
        let ticket = state.begin_read().unwrap();
        state.read(&ticket, Ok(snapshot()));
        state
    }
    #[test]
    fn collapsed_review_does_not_fetch_or_keep_late_sources() {
        let mut state = ReviewState::new(binding(), true);
        assert!(state.begin_read().is_none());
        state.toggle(true);
        let ticket = state.begin_read().unwrap();
        state.toggle(false);
        state.read(&ticket, Ok(snapshot()));
        assert!(state.value.is_none());
        assert!(!state.should_poll());
        state.toggle(true);
        assert!(state.begin_read().is_some());
    }
    #[test]
    fn every_source_context_change_revokes_a_pending_read() {
        for change in 0..6 {
            let mut state = ReviewState::new(binding(), true);
            state.toggle(true);
            let ticket = state.begin_read().unwrap();
            let mut next = binding();
            match change {
                0 => next.learner = "other".into(),
                1 => next.namespace = "real".into(),
                2 => next.session = "other".into(),
                3 => next.epoch += 1,
                4 => next.generation += 1,
                _ => next.state_revision += 1,
            }
            state.bind(next, true);
            state.read(&ticket, Ok(snapshot()));
            assert!(state.value.is_none());
            assert!(state.begin_read().is_some());
        }
    }
    #[test]
    fn pending_only_polls_while_the_manual_worker_is_running() {
        let mut state = loaded();
        for running in [true, false] {
            let mut value = snapshot();
            value["job"] = json!({"status":"pending"});
            value["review_running"] = json!(running);
            state.needs_read = true;
            let ticket = state.begin_read().unwrap();
            state.read(&ticket, Ok(value));
            assert_eq!(state.should_poll(), running);
        }
        assert!(state.can_act(&Action::Start));
        assert!(state.action_pending.is_none());
    }
    #[test]
    fn orphaned_lease_can_continue_but_live_manual_work_still_polls_and_cancels() {
        let mut state = loaded();
        let mut value = snapshot();
        value["job"] = json!({"status":"leased"});
        state.needs_read = true;
        let ticket = state.begin_read().unwrap();
        state.read(&ticket, Ok(value.clone()));
        assert!(!state.should_poll());
        assert!(state.can_act(&Action::Start));
        assert!(!state.can_act(&Action::Cancel));
        assert!(status_label("en", &value).contains("paused"));

        value["review_running"] = json!(true);
        value["worker_busy"] = json!(true);
        state.needs_read = true;
        let ticket = state.begin_read().unwrap();
        state.read(&ticket, Ok(value));
        assert!(state.should_poll());
        assert!(!state.can_act(&Action::Start));
        assert!(state.can_act(&Action::Cancel));
    }
    #[test]
    fn automatic_worker_lease_is_observed_without_offering_manual_cancellation() {
        let mut state = loaded();
        let action = Action::Start;
        let ticket = state.begin_action(action.clone()).unwrap();
        state.finish_action(&ticket, &action, Ok(json!({})));
        assert!(state.can_act(&Action::Cancel)); // Our start awaits its first snapshot.
        let mut value = snapshot();
        value["job"] = json!({"status":"leased"});
        value["worker_busy"] = json!(true);
        value["review_running"] = json!(false);
        let ticket = state.begin_read().unwrap();
        state.read(&ticket, Ok(value));
        assert!(state.should_poll());
        assert!(!state.can_act(&Action::Start));
        assert!(!state.can_act(&Action::Cancel));
    }
    #[test]
    fn uncertain_start_only_queues_a_read_and_never_an_automatic_restart() {
        let mut state = loaded();
        let action = Action::Start;
        let ticket = state.begin_action(action.clone()).unwrap();
        assert!(!state.finish_action(&ticket, &action, Err("lost response".into())));
        assert!(state.needs_read);
        assert!(state.action_pending.is_none());
        assert!(state.begin_read().is_some());
        assert!(!state.can_act(&Action::Start));
        assert_eq!(
            action.request(&binding()).1,
            json!({"expected_generation":4,"expected_epoch":2,"expected_state_revision":6})
        );
    }
    #[test]
    fn retain_requires_backend_permission_and_stale_success_does_not_notify() {
        let mut state = loaded();
        let mut value = snapshot();
        value["candidates"] = json!([{"candidate_id":"c","can_retain":false}]);
        state.value = Some(value.clone());
        assert!(!state.can_act(&Action::Retain("c".into())));
        assert!(state.can_act(&Action::Dismiss("c".into())));
        value["candidates"][0]["can_retain"] = json!(true);
        state.value = Some(value);
        let action = Action::Retain("c".into());
        let ticket = state.begin_action(action.clone()).unwrap();
        let mut next = binding();
        next.epoch += 1;
        state.bind(next, true);
        assert!(!state.finish_action(&ticket, &action, Ok(json!({"ok":true}))));
        assert!(state.notice.is_none());
        assert!(state.value.is_none());
    }
    #[test]
    fn old_read_cannot_restore_a_candidate_after_a_decision() {
        let mut state = loaded();
        let mut value = snapshot();
        value["candidates"] = json!([{"candidate_id":"c","can_retain":true}]);
        state.value = Some(value.clone());
        state.needs_read = true;
        let read = state.begin_read().unwrap();
        let action = Action::Dismiss("c".into());
        let write = state.begin_action(action.clone()).unwrap();
        state.read(&read, Ok(value));
        assert!(!state.finish_action(&write, &action, Ok(json!({}))));
        assert!(state.value.is_none());
        assert!(state.begin_read().is_some());
    }
    #[test]
    fn source_projection_and_reasons_do_not_invent_evidence_or_scores() {
        assert_eq!(
            source_text(
                &json!({"text":"原句\nunchanged","internal_prompt":"hidden","lease_token":"hidden"})
            ),
            vec!["原句\nunchanged"]
        );
        assert_eq!(
            reasons(
                "en",
                &json!({"importance":{"continuity":2,"explicit_request":0,"novel_change":99}})
            ),
            vec!["Useful for continuing later"]
        );
        assert!(reasons("zh", &Value::Null).is_empty());
    }
    #[test]
    fn collapsing_during_retention_refreshes_memories_without_reopening_sources() {
        let mut state = loaded();
        let mut value = snapshot();
        value["candidates"] = json!([{"candidate_id":"c","can_retain":true}]);
        state.value = Some(value);
        let action = Action::Retain("c".into());
        let ticket = state.begin_action(action.clone()).unwrap();
        state.toggle(false);
        assert!(state.finish_action(&ticket, &action, Ok(json!({}))));
        assert!(!state.open);
        assert!(state.value.is_none());
        assert!(!state.should_poll());
    }
}

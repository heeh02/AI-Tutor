//! Public, owner-bound local retrieval status. Never renders runtime diagnostics.
use crate::api::{api, ApiResult};
use serde_json::{json, Value};
use yew::prelude::*;

fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}
fn coverage(status: &Value) -> Option<(u64, u64)> {
    let eligible = status["coverage"]["eligible"].as_u64()?;
    let indexed = status["coverage"]["indexed"].as_u64()?;
    (indexed <= eligible).then_some((indexed, eligible))
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Display {
    ScopeUnavailable,
    Preparing,
    Indexing,
    Blocked,
    Ready,
    Stopping,
    Stopped,
    Degraded,
    ConfigurationError,
    CleanupFailed,
    Unknown,
}
impl Display {
    fn from_status(status: &Value) -> Option<Self> {
        let phase = status["phase"].as_str()?;
        if phase == "disabled" {
            return None;
        }
        if phase == "configuration_error" {
            return Some(Self::ConfigurationError);
        }
        if status["available_for_profile"] != true {
            return Some(Self::ScopeUnavailable);
        }
        if status["shutting_down"] == true {
            return Some(Self::Stopping);
        }
        Some(match phase {
            "starting" | "loading" => Self::Preparing,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            "degraded" => Self::Degraded,
            "configuration_error" => Self::ConfigurationError,
            "cleanup_failed" => Self::CleanupFailed,
            "active"
                if status["mode"] == "bge_cpu"
                    && status["active"] == true
                    && status["ready"] == true =>
            {
                if status["index_blocked"] == true {
                    Self::Blocked
                } else if status["indexing"] != false
                    || status["index_blocked"] != false
                    || !coverage(status).is_some_and(|(indexed, eligible)| indexed == eligible)
                {
                    Self::Indexing
                } else {
                    Self::Ready
                }
            }
            "active" => Self::Preparing,
            _ => Self::Unknown,
        })
    }
    fn title(self, language: &str) -> &'static str {
        match self {
            Self::ScopeUnavailable => tr(
                language,
                "此档案未启用中英记忆检索",
                "Chinese-English memory search is not enabled for this profile",
            ),
            Self::Preparing => tr(
                language,
                "正在准备中英记忆检索",
                "Preparing Chinese-English memory search",
            ),
            Self::Indexing => tr(
                language,
                "正在准备本档案的学习记录",
                "Preparing this profile's learning records",
            ),
            Self::Blocked => tr(
                language,
                "部分学习记录暂不能用于中英检索",
                "Some learning records are not ready for Chinese-English search",
            ),
            Self::Ready => tr(
                language,
                "中英记忆检索已就绪",
                "Chinese-English memory search is ready",
            ),
            Self::Stopping => tr(
                language,
                "正在停止本地记忆检索",
                "Stopping local memory search",
            ),
            Self::Stopped => tr(
                language,
                "本地记忆检索已停止",
                "Local memory search is stopped",
            ),
            Self::Degraded => tr(
                language,
                "中英记忆检索暂不可用",
                "Chinese-English memory search is temporarily unavailable",
            ),
            Self::ConfigurationError => tr(
                language,
                "本地记忆检索设置需要检查",
                "Local memory search settings need attention",
            ),
            Self::CleanupFailed => tr(
                language,
                "本地记忆服务尚未确认停止",
                "The local memory service has not confirmed that it stopped",
            ),
            Self::Unknown => tr(
                language,
                "正在核对记忆检索状态",
                "Checking memory search status",
            ),
        }
    }
    fn description(self, language: &str) -> &'static str {
        match self {
            Self::Ready=>tr(language,"后续对话可从本档案的有效学习记忆中查找中英文线索。检索结果仍需结合来源判断。","Future conversations can search this profile's valid memories across Chinese and English. Retrieved notes still need to be considered alongside their sources."),
            Self::ScopeUnavailable=>tr(language,"此档案继续使用词语匹配，不显示其他档案的准备进度。","This profile continues to use word matching. Other profiles' preparation counts are not shown."),
            Self::Preparing|Self::Indexing=>tr(language,"仍可继续对话；准备完成前暂用词语匹配查找学习记忆。","You can keep chatting. Word matching is used to find memories while preparation continues."),
            Self::Blocked=>tr(language,"部分记录超过当前处理长度，暂用词语匹配。你仍可继续对话。","Some records exceed the current processing length, so word matching is used for now. You can keep chatting."),
            Self::ConfigurationError=>tr(language,"检查本机设置后可重试。对话仍可继续，记忆暂用词语匹配。","Check the local settings before retrying. Conversation continues, with word matching for memories."),
            Self::CleanupFailed=>tr(language,"停止尚未得到确认，可再次请求停止。对话仍可继续，记忆暂用词语匹配。","Stopping is not yet confirmed; you can request it again. Conversation continues, with word matching for memories."),
            Self::Degraded|Self::Stopped=>tr(language,"仍可继续对话，记忆暂用词语匹配；需要时可以手动重试准备。","You can keep chatting using word matching for memories, and retry preparation when needed."),
            Self::Stopping|Self::Unknown=>tr(language,"仍可继续对话，学习记忆暂用词语匹配。","You can keep chatting. Learning memories use word matching for now."),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Retry,
    Stop,
}
impl Command {
    fn path(self) -> &'static str {
        match self {
            Self::Retry => "/api/local-memory/retry",
            Self::Stop => "/api/local-memory/stop",
        }
    }
    fn allowed(self, status: &Value) -> bool {
        if self == Self::Retry && status["phase"] == "configuration_error" {
            return status["retry_available"] == true && status["shutting_down"] == false;
        }
        if status["mode"] != "bge_cpu"
            || status["available_for_profile"] != true
            || status["shutting_down"] != false
        {
            return false;
        }
        match self {
            Self::Retry => {
                status["retry_available"] == true
                    && matches!(
                        status["phase"].as_str(),
                        Some("degraded" | "stopped" | "configuration_error")
                    )
            }
            Self::Stop => matches!(
                status["phase"].as_str(),
                Some("starting" | "loading" | "active" | "degraded" | "cleanup_failed")
            ),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Notice {
    Requested,
    Unconfirmed,
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub status: Value,
    pub learner_id: String,
    pub learner_epoch: i64,
    pub namespace: String,
    pub session_id: String,
    pub ui_language: String,
    pub enabled: bool,
    pub on_refresh: Callback<()>,
}
#[derive(Clone, Debug, PartialEq)]
struct Owner {
    learner: String,
    epoch: i64,
    namespace: String,
    session: String,
}
impl Owner {
    fn from_props(props: &Props) -> Self {
        Self {
            learner: props.learner_id.clone(),
            epoch: props.learner_epoch,
            namespace: props.namespace.clone(),
            session: props.session_id.clone(),
        }
    }
    fn valid(&self) -> bool {
        !self.learner.is_empty()
            && !self.session.is_empty()
            && self.epoch >= 0
            && matches!(self.namespace.as_str(), "real" | "simulator")
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    owner: Owner,
    revision: u64,
    command: Command,
}
struct Intent {
    owner: Owner,
    revision: u64,
    pending: Option<Ticket>,
    notice: Option<Notice>,
}
impl Intent {
    fn new(owner: Owner) -> Self {
        Self {
            owner,
            revision: 0,
            pending: None,
            notice: None,
        }
    }
    fn bind(&mut self, owner: Owner) {
        if self.owner != owner {
            self.owner = owner;
            self.revision = self.revision.wrapping_add(1);
            self.notice = None;
        }
    }
    fn begin(&mut self, command: Command) -> Option<Ticket> {
        if self.pending.is_some() {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        let ticket = Ticket {
            owner: self.owner.clone(),
            revision: self.revision,
            command,
        };
        self.pending = Some(ticket.clone());
        self.notice = None;
        Some(ticket)
    }
    fn finish(&mut self, ticket: &Ticket, success: bool) -> bool {
        if self.pending.as_ref() != Some(ticket) {
            return false;
        }
        self.pending = None;
        if ticket.owner == self.owner && ticket.revision == self.revision {
            self.notice = Some(if success {
                Notice::Requested
            } else {
                Notice::Unconfirmed
            });
        }
        true
    }
}

pub enum Msg {
    Command(Command),
    Finished(Ticket, ApiResult),
}
pub struct LocalMemoryStatus {
    intent: Intent,
}
impl Component for LocalMemoryStatus {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        Self {
            intent: Intent::new(Owner::from_props(ctx.props())),
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Props) -> bool {
        self.intent.bind(Owner::from_props(ctx.props()));
        if old.status != ctx.props().status && self.intent.pending.is_none() {
            self.intent.notice = None;
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        self.intent.bind(Owner::from_props(ctx.props()));
        match msg {
            Msg::Command(command) => {
                let global_configuration = ctx.props().status["phase"] == "configuration_error";
                if !ctx.props().enabled
                    || !command.allowed(&ctx.props().status)
                    || (!self.intent.owner.valid() && !global_configuration)
                {
                    return false;
                }
                let Some(ticket) = self.intent.begin(command) else {
                    return false;
                };
                ctx.link().send_future(async move {
                    Msg::Finished(ticket, api(command.path(), "POST", Some(json!({}))).await)
                });
            }
            Msg::Finished(ticket, result) => {
                if !self.intent.finish(&ticket, result.is_ok()) {
                    return false;
                }
                // The operation is global to the local runtime; refresh current
                // state even when its old profile's notice has been discarded.
                ctx.props().on_refresh.emit(());
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let Some(display) = Display::from_status(&props.status) else {
            return Html::default();
        };
        let language = props.ui_language.as_str();
        let pending = self.intent.pending.is_some();
        let disabled = !props.enabled
            || pending
            || (!self.intent.owner.valid() && display != Display::ConfigurationError);
        html! {<section class="local-speech-status local-memory-status" aria-label={tr(language,"本地记忆检索","Local memory search")}>
            {render_status(display,&props.status,language)}
            <div class="local-speech-actions">
                {if Command::Retry.allowed(&props.status){html!{<button class="text-button" style="min-height:44px" disabled={disabled} onclick={ctx.link().callback(|_|Msg::Command(Command::Retry))}>{tr(language,"重试记忆检索准备","Retry memory search preparation")}</button>}}else{Html::default()}}
                {if Command::Stop.allowed(&props.status){html!{<button class="text-button" style="min-height:44px" disabled={disabled} onclick={ctx.link().callback(|_|Msg::Command(Command::Stop))}>{tr(language,"停止本地记忆检索","Stop local memory search")}</button>}}else{Html::default()}}
                {if pending{html!{<span role="status">{tr(language,"正在处理请求…","Applying request…")}</span>}}else{Html::default()}}
            </div>
            {self.intent.notice.map(|notice|html!{<p role="status">{match notice{
                Notice::Requested=>tr(language,"请求已提交，以当前服务状态为准。","Request submitted. The current service status shows the outcome."),
                Notice::Unconfirmed=>tr(language,"操作结果尚未确认，请检查更新后的状态；不会自动重试。","The result was not confirmed. Check the refreshed status; there is no automatic retry."),
            }}</p>}).unwrap_or_default()}
        </section>}
    }
}
fn render_status(display: Display, status: &Value, language: &str) -> Html {
    html! {<><p role="status"><strong>{display.title(language)}</strong></p><p>{display.description(language)}</p>
        {if !matches!(display,Display::ScopeUnavailable|Display::ConfigurationError){coverage(status).map(|(indexed,eligible)|html!{<small>{if language=="en"{format!("This profile: {indexed} of {eligible} records prepared") }else{format!("本档案已准备 {indexed} / {eligible} 条记录")}}</small>}).unwrap_or_default()}else{Html::default()}}
    </>}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn status() -> Value {
        json!({"mode":"bge_cpu","phase":"active","active":true,"ready":true,"available_for_profile":true,
        "coverage":{"eligible":2,"indexed":2},"indexing":false,"index_blocked":false,"retry_available":false,"shutting_down":false})
    }
    fn owner() -> Owner {
        Owner {
            learner: "a".into(),
            epoch: 0,
            namespace: "simulator".into(),
            session: "s".into(),
        }
    }
    #[test]
    fn ready_requires_complete_current_coverage_and_no_indexing_or_length_block() {
        assert_eq!(Display::from_status(&status()), Some(Display::Ready));
        for (field, value, expected) in [
            ("ready", json!(false), Display::Preparing),
            ("indexing", json!(true), Display::Indexing),
            ("index_blocked", json!(true), Display::Blocked),
            ("coverage", Value::Null, Display::Indexing),
            (
                "coverage",
                json!({"eligible":3,"indexed":2}),
                Display::Indexing,
            ),
        ] {
            let mut sample = status();
            sample[field] = value;
            assert_eq!(Display::from_status(&sample), Some(expected));
        }
        assert_eq!(
            Display::from_status(&json!({"mode":"disabled","phase":"disabled"})),
            None
        );
        assert_eq!(Display::from_status(&Value::Null), None);
    }
    #[test]
    fn another_profile_counts_and_runtime_diagnostics_are_never_rendered() {
        let mut sample = status();
        sample["available_for_profile"] = json!(false);
        sample["coverage"] = json!({"eligible":73921,"indexed":38219});
        sample["error"] = json!("private path and source text");
        sample["worker_pid"] = json!(98123);
        let display = Display::from_status(&sample).unwrap();
        for language in ["zh", "en"] {
            let rendered = format!("{:?}", render_status(display, &sample, language));
            assert!(rendered.contains(display.title(language)));
            for hidden in ["73921", "38219", "98123", "private"] {
                assert!(!rendered.contains(hidden));
            }
        }
        assert!(!Command::Retry.allowed(&sample) && !Command::Stop.allowed(&sample));
    }
    #[test]
    fn invalid_global_configuration_allows_explicit_retry_without_showing_scope_counts() {
        let sample = json!({"mode":"disabled","phase":"configuration_error","available_for_profile":false,
            "coverage":{"eligible":73921,"indexed":38219},"retry_available":true,"shutting_down":false});
        assert_eq!(
            Display::from_status(&sample),
            Some(Display::ConfigurationError)
        );
        assert!(Command::Retry.allowed(&sample));
        assert!(!Command::Stop.allowed(&sample));
        for language in ["zh", "en"] {
            let rendered = format!(
                "{:?}",
                render_status(Display::ConfigurationError, &sample, language)
            );
            assert!(rendered.contains(Display::ConfigurationError.title(language)));
            assert!(!rendered.contains("73921") && !rendered.contains("38219"));
        }
    }
    #[test]
    fn changing_owner_or_epoch_clears_notice_and_cannot_adopt_late_operation_notice() {
        for change in 0..4 {
            let mut intent = Intent::new(owner());
            let ticket = intent.begin(Command::Stop).unwrap();
            assert!(intent.begin(Command::Retry).is_none());
            let mut next = owner();
            match change {
                0 => next.learner = "b".into(),
                1 => next.epoch += 1,
                2 => next.namespace = "real".into(),
                _ => next.session = "other".into(),
            }
            intent.bind(next);
            assert!(intent.notice.is_none());
            assert!(intent.finish(&ticket, true));
            assert!(intent.notice.is_none());
            assert!(intent.pending.is_none());
            assert!(!intent.finish(&ticket, true));
            let current = intent.begin(Command::Retry).unwrap();
            assert!(intent.finish(&current, true));
            assert_eq!(intent.notice, Some(Notice::Requested));
            let mut next_owner = owner();
            next_owner.session = "fresh-owner".into();
            intent.bind(next_owner);
            assert!(intent.notice.is_none());
        }
    }
    #[test]
    fn actions_require_explicit_profile_scope_phase_and_retry_permission() {
        let mut sample = status();
        assert!(Command::Stop.allowed(&sample));
        assert!(!Command::Retry.allowed(&sample));
        sample["phase"] = json!("degraded");
        sample["retry_available"] = json!(true);
        assert!(Command::Retry.allowed(&sample));
        sample["shutting_down"] = json!(true);
        assert!(!Command::Retry.allowed(&sample) && !Command::Stop.allowed(&sample));
        assert_eq!(Command::Retry.path(), "/api/local-memory/retry");
        assert_eq!(Command::Stop.path(), "/api/local-memory/stop");
        let mut intent = Intent::new(owner());
        let ticket = intent.begin(Command::Retry).unwrap();
        assert!(intent.finish(&ticket, false));
        assert_eq!(intent.notice, Some(Notice::Unconfirmed));
    }
}

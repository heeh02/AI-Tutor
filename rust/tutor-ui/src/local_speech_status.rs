//! Optional workstation voice status; no automatic retry or policy mutation.
use crate::api::{ApiResult, api};
use serde_json::{Value, json};
use yew::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    WaitingGpu,
    Configured,
    Starting,
    Checking,
    Ready,
    Warming,
    PendingInstall,
    Active,
    Degraded,
    Stopping,
    Stopped,
    CleanupFailed,
    ConfigurationError,
    Unknown,
}
impl Phase {
    fn from_status(status: &Value) -> Option<Self> {
        if !status.is_object() || status["phase"] == "disabled" {
            return None;
        }
        Some(match status["phase"].as_str().unwrap_or("") {
            "configured" => Self::Configured,
            "waiting_gpu" => Self::WaitingGpu,
            "starting" => Self::Starting,
            "checking" => Self::Checking,
            "ready_cold" => Self::Ready,
            "warming" => Self::Warming,
            "warm_ready_pending_install" => Self::PendingInstall,
            "active" if status["active"] == true => Self::Active,
            "degraded" => Self::Degraded,
            "stopping" | "cancelled" => Self::Stopping,
            "stopped" => Self::Stopped,
            "cleanup_failed" => Self::CleanupFailed,
            "configuration_error" => Self::ConfigurationError,
            _ => Self::Unknown,
        })
    }
    fn title(self, en: bool) -> &'static str {
        match (self, en) {
            (Self::WaitingGpu, false) => "正在等待显卡资源",
            (Self::WaitingGpu, true) => "Waiting for GPU resources",
            (Self::Configured, false) => "等待准备本地增强语音",
            (Self::Configured, true) => "Local voice preparation is queued",
            (Self::Starting, false) => "正在启动本地语音服务",
            (Self::Starting, true) => "Starting the local voice service",
            (Self::Checking, false) => "正在检查本地语音服务",
            (Self::Checking, true) => "Checking the local voice service",
            (Self::Ready, false) => "服务已响应，等待双语准备",
            (Self::Ready, true) => "Service ready; bilingual preparation is next",
            (Self::Warming, false) => "正在准备中文和英文语音",
            (Self::Warming, true) => "Preparing Chinese and English speech",
            (Self::PendingInstall, false) => "语音已准备，等待空闲时启用",
            (Self::PendingInstall, true) => {
                "Voice prepared; waiting for an idle moment to enable it"
            }
            (Self::Active, false) => "本地增强语音已启用",
            (Self::Active, true) => "Enhanced local voice is enabled",
            (Self::Degraded, false) => "本地增强语音暂不可用",
            (Self::Degraded, true) => "Enhanced local voice is unavailable",
            (Self::Stopping, false) => "正在停用本地增强语音",
            (Self::Stopping, true) => "Turning off the enhanced local voice",
            (Self::Stopped, false) => "本地增强语音已停用",
            (Self::Stopped, true) => "Enhanced local voice is off",
            (Self::CleanupFailed, false) => "语音服务清理尚未确认",
            (Self::CleanupFailed, true) => "Voice service cleanup is unconfirmed",
            (Self::ConfigurationError, false) => "本地语音设置需要检查",
            (Self::ConfigurationError, true) => "Local voice settings need attention",
            (Self::Unknown, false) => "本地语音状态待更新",
            (Self::Unknown, true) => "Waiting for an updated local voice status",
        }
    }
    fn description(self, en: bool) -> &'static str {
        match (self, en) {
            (Self::Active, false) => "用于后续本机朗读；这里显示的是启用状态。",
            (Self::Active, true) => {
                "Available for subsequent local reading; this shows the enabled state."
            }
            (Self::CleanupFailed, false) => "请检查本机语音服务后再继续配置。可以继续文字交流。",
            (Self::CleanupFailed, true) => {
                "Check the local voice service before configuring it again. You can continue using text."
            }
            (Self::ConfigurationError, false) => "检查本机语音设置后可重试，也可继续文字交流。",
            (Self::ConfigurationError, true) => {
                "Check the local voice settings, then retry. You can also continue using text."
            }
            (_, false) => "可以继续文字交流。准备过程不会播放声音。",
            (_, true) => "You can continue using text. Preparation does not play audio.",
        }
    }
    fn can_retry(self, status: &Value) -> bool {
        matches!(
            self,
            Self::Degraded | Self::Stopped | Self::ConfigurationError
        ) && status["retry_available"] == true
            && status["busy"] == false
            && status["shutting_down"] == false
    }
    fn can_stop(self, status: &Value) -> bool {
        matches!(
            self,
            Self::Configured
                | Self::WaitingGpu
                | Self::Starting
                | Self::Checking
                | Self::Ready
                | Self::Warming
                | Self::PendingInstall
                | Self::Active
        ) && matches!(status["mode"].as_str(), Some("managed" | "external"))
            && status["busy"] == true
            && status["shutting_down"] == false
    }
}

fn model_name(status: &Value) -> Option<&'static str> {
    // Only reviewed names enter presentation; never render arbitrary diagnostics,
    // instance IDs, credential fields, paths or the full status JSON.
    match status["model"]
        .as_str()
        .or_else(|| status["receipt"]["model"].as_str())
    {
        Some("tutor-qwen-tts") => Some("Qwen3-TTS"),
        Some("tutor-cosy-tts") => Some("CosyVoice3"),
        _ => None,
    }
}
fn preparation_seconds(status: &Value) -> Option<String> {
    status["receipt"]["preparation_ms"]
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| format!("{:.2} s", value / 1000.0))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Retry,
    Stop,
}
impl Command {
    fn path(self) -> &'static str {
        match self {
            Self::Retry => "/api/audio/local-speech/retry",
            Self::Stop => "/api/audio/local-speech/stop",
        }
    }
}
#[derive(Clone, Copy)]
enum Notice {
    Requested,
    Failed,
}

#[derive(Properties, PartialEq)]
pub struct LocalSpeechStatusProps {
    pub status: Value,
    pub ui_language: String,
    pub enabled: bool,
    pub on_refresh: Callback<()>,
}
pub enum Msg {
    Retry,
    Stop,
    Finished(ApiResult),
}
pub struct LocalSpeechStatus {
    pending: Option<Command>,
    notice: Option<Notice>,
}
impl Component for LocalSpeechStatus {
    type Message = Msg;
    type Properties = LocalSpeechStatusProps;

    fn create(_: &Context<Self>) -> Self {
        Self {
            pending: None,
            notice: None,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        if old.status != ctx.props().status && self.pending.is_none() {
            self.notice = None;
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Finished(result) => {
                self.pending = None;
                self.notice = Some(if result.is_ok() {
                    Notice::Requested
                } else {
                    Notice::Failed
                });
                // POST receipts are not authoritative lifecycle snapshots. The
                // existing App poll sequence discards superseded state replies.
                ctx.props().on_refresh.emit(());
                true
            }
            Msg::Retry | Msg::Stop => {
                let command = if matches!(msg, Msg::Retry) {
                    Command::Retry
                } else {
                    Command::Stop
                };
                let status = &ctx.props().status;
                let allowed = Phase::from_status(status).is_some_and(|phase| match command {
                    Command::Retry => phase.can_retry(status),
                    Command::Stop => phase.can_stop(status),
                });
                if self.pending.is_some() || !ctx.props().enabled || !allowed {
                    return false;
                }
                self.pending = Some(command);
                self.notice = None;
                ctx.link().send_future(async move {
                    Msg::Finished(api(command.path(), "POST", Some(json!({}))).await)
                });
                true
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let Some(phase) = Phase::from_status(&props.status) else {
            return Html::default();
        };
        let en = props.ui_language == "en";
        let disabled = !props.enabled || self.pending.is_some();
        let boolean = |value: &Value| {
            if value == true {
                if en { "Yes" } else { "是" }
            } else {
                if en { "Not confirmed" } else { "尚未确认" }
            }
        };
        html! {<section class="local-speech-status" aria-label={if en {"Local voice"} else {"本地语音"}}>
            <p role="status"><strong>{phase.title(en)}</strong></p>
            <p>{phase.description(en)}</p>
            {if props.status["shutting_down"] == true {html!{<p>{if en {"The workstation is closing."} else {"工作台正在关闭。"}}</p>}} else {Html::default()}}
            <div class="local-speech-actions">
                {if phase.can_retry(&props.status) {html!{<button class="text-button" disabled={disabled} onclick={ctx.link().callback(|_|Msg::Retry)}>{if en {"Retry local voice preparation"} else {"重试本地语音准备"}}</button>}} else {Html::default()}}
                {if phase.can_stop(&props.status) {html!{<button class="text-button" disabled={disabled} onclick={ctx.link().callback(|_|Msg::Stop)}>{if en {"Turn off enhanced local voice"} else {"停用本地增强语音"}}</button>}} else {Html::default()}}
                {if self.pending.is_some() {html!{<span role="status">{if en {"Applying request…"} else {"正在处理请求…"}}</span>}} else {Html::default()}}
            </div>
            {self.notice.map(|notice|html!{<p role="status">{match (notice,en) {
                (Notice::Requested,false)=>"请求已提交，等待状态更新。",
                (Notice::Requested,true)=>"Request submitted; waiting for a status update.",
                (Notice::Failed,false)=>"操作未完成。请检查本机服务；状态会继续更新。",
                (Notice::Failed,true)=>"The action did not complete. Check the local service; status updates will continue.",
            }}</p>}).unwrap_or_default()}
            <details><summary>{if en {"Voice details"} else {"语音详情"}}</summary><dl>
                {model_name(&props.status).map(|name|html!{<><dt>{if en {"Model"} else {"模型"}}</dt><dd>{name}</dd></>}).unwrap_or_default()}
                <dt>{if en {"Service responded"} else {"服务已响应"}}</dt><dd>{boolean(&props.status["ready"])}</dd>
                <dt>{if en {"Bilingual preparation completed"} else {"双语准备已完成"}}</dt><dd>{boolean(&props.status["warmup_completed"])}</dd>
                <dt>{if en {"Enabled for reading"} else {"已启用朗读"}}</dt><dd>{boolean(&props.status["active"])}</dd>
                {preparation_seconds(&props.status).map(|duration|html!{<><dt>{if en {"Bilingual preparation time"} else {"双语准备时长"}}</dt><dd>{duration}</dd></>}).unwrap_or_default()}
            </dl><p>{if en {"Preparation time is not playback latency or measured speaker output."} else {"准备时长不代表播报延迟或实测扬声器输出。"}}</p></details>
        </section>}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_wait_is_stoppable_without_enabling_the_voice() {
        let status = json!({"phase":"waiting_gpu","mode":"managed","busy":true,
            "active":false,"ready":false,"warmup_completed":false,"shutting_down":false});
        let phase = Phase::from_status(&status).unwrap();
        assert_eq!(phase, Phase::WaitingGpu);
        assert!(phase.can_stop(&status));
        assert!(!phase.can_retry(&status));
    }

    #[test]
    fn legacy_and_disabled_backends_do_not_offer_an_activation_action() {
        assert_eq!(Phase::from_status(&Value::Null), None);
        assert_eq!(Phase::from_status(&json!({"phase":"disabled"})), None);
        let old = json!({"phase":"degraded","retry_available":true});
        assert!(!Phase::from_status(&old).unwrap().can_retry(&old));
    }
    #[test]
    fn readiness_and_preparation_never_imply_an_installed_voice() {
        for (name, expected) in [
            ("ready_cold", Phase::Ready),
            ("warming", Phase::Warming),
            ("warm_ready_pending_install", Phase::PendingInstall),
        ] {
            let status = json!({"phase":name,"ready":true,"warmup_completed":true,"active":false});
            assert_eq!(Phase::from_status(&status), Some(expected));
        }
        assert_eq!(
            Phase::from_status(&json!({"phase":"active","active":false})),
            Some(Phase::Unknown)
        );
    }
    #[test]
    fn retry_requires_explicit_availability_and_an_idle_open_lifecycle() {
        let mut status =
            json!({"phase":"degraded","retry_available":true,"busy":false,"shutting_down":false});
        assert!(Phase::Degraded.can_retry(&status));
        for field in ["retry_available", "busy", "shutting_down"] {
            status[field] = json!(field != "retry_available");
            assert!(!Phase::Degraded.can_retry(&status));
            status[field] = json!(field == "retry_available");
        }
        status["phase"] = json!("cleanup_failed");
        assert!(!Phase::from_status(&status).unwrap().can_retry(&status));
        assert_eq!(Phase::from_status(&status), Some(Phase::CleanupFailed));
    }
    #[test]
    fn active_can_be_stopped_while_busy_but_shutdown_cannot_be_reopened() {
        let mut status = json!({"phase":"active","mode":"managed","active":true,"busy":true,"shutting_down":false});
        assert!(Phase::Active.can_stop(&status));
        assert!(!Phase::Active.can_retry(&status));
        status["shutting_down"] = json!(true);
        assert!(!Phase::Active.can_stop(&status));
        assert_eq!(
            Phase::from_status(&json!({"phase":"cancelled"})),
            Some(Phase::Stopping)
        );
    }
    #[test]
    fn detail_projection_whitelists_model_names_and_only_reports_preparation_time() {
        let status = json!({"model":"C:/private/token","error":"secret","receipt":{"preparation_ms":7245.0}});
        assert_eq!(model_name(&status), None);
        assert_eq!(preparation_seconds(&status).as_deref(), Some("7.25 s"));
        assert_eq!(
            preparation_seconds(&json!({"receipt":{"preparation_ms":-1}})),
            None
        );
        for phase in [
            Phase::Starting,
            Phase::Checking,
            Phase::Ready,
            Phase::Warming,
            Phase::PendingInstall,
            Phase::Active,
            Phase::Degraded,
            Phase::Stopping,
            Phase::Stopped,
            Phase::CleanupFailed,
            Phase::ConfigurationError,
        ] {
            assert!(phase.title(true).is_ascii());
            assert!(phase.description(true).is_ascii());
        }
    }
}

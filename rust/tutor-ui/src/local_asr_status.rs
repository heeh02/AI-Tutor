//! Optional local transcription preparation; never opens a microphone.
use crate::api::{ApiResult, api};
use serde_json::{Value, json};
use yew::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    WaitingGpu,
    Starting,
    Loading,
    Warming,
    Active,
    Degraded,
    Stopping,
    Stopped,
    CleanupFailed,
    ConfigurationError,
    Unknown,
}
impl Phase {
    fn title_for(self, precise: bool, en: bool) -> &'static str {
        if !precise {
            return self.title(en);
        }
        match (self, en) {
            (Self::Starting | Self::Loading, false) => "正在准备精确转写",
            (Self::Starting | Self::Loading, true) => "Preparing precise transcription",
            (Self::Warming, false) => "正在预热精确转写",
            (Self::Warming, true) => "Warming up precise transcription",
            (Self::Active, false) => "精确转写已启用",
            (Self::Active, true) => "Precise transcription is enabled",
            (Self::Degraded, false) => "精确转写暂不可用",
            (Self::Degraded, true) => "Precise transcription is unavailable",
            (Self::Stopping, false) => "正在停用精确转写",
            (Self::Stopping, true) => "Turning off precise transcription",
            (Self::Stopped, false) => "精确转写已停用",
            (Self::Stopped, true) => "Precise transcription is off",
            _ => self.title(en),
        }
    }
    fn description_for(self, precise: bool, en: bool) -> &'static str {
        if !precise {
            return self.description(en);
        }
        match (self, en) {
            (Self::Active, false) => {
                "用于后续录音结束时的精确转写。实时字幕继续显示，终稿可修改后再发送。"
            }
            (Self::Active, true) => {
                "Improves the final transcript of subsequent recordings. Live captions continue; you can edit the final text before sending."
            }
            (Self::WaitingGpu, false) => {
                "正在等待另一语音模型完成准备。可以继续文字交流，也可以取消准备。"
            }
            (Self::WaitingGpu, true) => {
                "Waiting for the other speech model to finish preparing. You can continue using text or cancel preparation."
            }
            (Self::Starting | Self::Loading | Self::Warming, false) => {
                "首次准备需要加载和预热。完成后保持就绪，不会自动打开麦克风。"
            }
            (Self::Starting | Self::Loading | Self::Warming, true) => {
                "Initial preparation loads and warms the model. It stays ready afterwards and does not open the microphone automatically."
            }
            (_, false) => "仍可使用已配置的其他本地识别方式或编辑文字。",
            (_, true) => {
                "Other configured local transcription options and text editing remain available."
            }
        }
    }
    fn from_status(status: &Value) -> Option<Self> {
        Some(match status["phase"].as_str()? {
            "disabled" => return None,
            "waiting_gpu" => Self::WaitingGpu,
            "starting" => Self::Starting,
            "loading" => Self::Loading,
            "warming" => Self::Warming,
            "active" if status["active"] == true => Self::Active,
            "degraded" => Self::Degraded,
            "stopping" => Self::Stopping,
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
            (Self::Starting, false) => "正在准备本地实时转写",
            (Self::Starting, true) => "Preparing local live transcription",
            (Self::Loading, false) => "正在加载本地识别模型",
            (Self::Loading, true) => "Loading the local recognition model",
            (Self::Warming, false) => "正在预热本地识别模型",
            (Self::Warming, true) => "Warming up the local recognition model",
            (Self::Active, false) => "本地实时转写已启用",
            (Self::Active, true) => "Local live transcription is enabled",
            (Self::Degraded, false) => "本地实时转写暂不可用",
            (Self::Degraded, true) => "Local live transcription is unavailable",
            (Self::Stopping, false) => "正在停用本地实时转写",
            (Self::Stopping, true) => "Turning off local live transcription",
            (Self::Stopped, false) => "本地实时转写已停用",
            (Self::Stopped, true) => "Local live transcription is off",
            (Self::CleanupFailed, false) => "识别服务清理尚未确认",
            (Self::CleanupFailed, true) => "Recognition service cleanup is unconfirmed",
            (Self::ConfigurationError, false) => "本地识别设置需要检查",
            (Self::ConfigurationError, true) => "Local recognition settings need attention",
            (Self::Unknown, false) => "本地识别状态待更新",
            (Self::Unknown, true) => "Waiting for an updated recognition status",
        }
    }
    fn description(self, en: bool) -> &'static str {
        match (self, en) {
            (Self::Active, false) => {
                "用于后续录音的实时字幕；不会自动打开麦克风。转写后仍可修改再发送。"
            }
            (Self::Active, true) => {
                "Available for live captions in subsequent recordings. It does not open the microphone automatically; you can edit the transcript before sending."
            }
            (Self::CleanupFailed, false) => {
                "请检查本机识别服务；这里尚未确认清理完成。可以继续编辑文字。"
            }
            (Self::CleanupFailed, true) => {
                "Check the local recognition service; cleanup has not been confirmed. You can continue editing text."
            }
            (Self::ConfigurationError, false) => {
                "检查本机识别设置后可重试。可以继续编辑文字；录音后的离线转写仍需可用麦克风和本机 Whisper 模型。"
            }
            (Self::ConfigurationError, true) => {
                "Check the local recognition settings before retrying. You can edit text; offline transcription after recording still requires a working microphone and local Whisper model."
            }
            (Self::Degraded | Self::Stopped | Self::Stopping, false) => {
                "可以继续编辑文字。录音后的离线转写仍需可用麦克风和本机 Whisper 模型。"
            }
            (Self::Degraded | Self::Stopped | Self::Stopping, true) => {
                "You can continue editing text. Offline transcription after recording still requires a working microphone and local Whisper model."
            }
            (_, false) => "准备过程不会打开麦克风。可以继续编辑文字。",
            (_, true) => "Preparation does not open the microphone. You can continue editing text.",
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
            Self::WaitingGpu | Self::Starting | Self::Loading | Self::Warming | Self::Active
        ) && status["mode"] == "managed"
            && status["busy"] == true
            && status["shutting_down"] == false
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Retry,
    Stop,
}
impl Command {
    fn path_for(self, precise: bool) -> &'static str {
        if !precise {
            return self.path();
        }
        match self {
            Self::Retry => "/api/audio/final-asr/retry",
            Self::Stop => "/api/audio/final-asr/stop",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Retry => "/api/audio/local-asr/retry",
            Self::Stop => "/api/audio/local-asr/stop",
        }
    }
}
#[derive(Clone, Copy)]
enum Notice {
    Requested,
    Unconfirmed,
}

#[derive(Properties, PartialEq)]
pub struct LocalAsrStatusProps {
    pub status: Value,
    pub ui_language: String,
    pub enabled: bool,
    #[prop_or_default]
    pub precise: bool,
    pub on_refresh: Callback<()>,
}
pub enum Msg {
    Retry,
    Stop,
    Finished(ApiResult),
}
pub struct LocalAsrStatus {
    pending: Option<Command>,
    notice: Option<Notice>,
}
impl Component for LocalAsrStatus {
    type Message = Msg;
    type Properties = LocalAsrStatusProps;

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
                    Notice::Unconfirmed
                });
                // Refresh the sequenced workstation snapshot. An HTTP response
                // alone does not prove readiness, activation or successful cleanup.
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
                let precise = ctx.props().precise;
                ctx.link().send_future(async move {
                    Msg::Finished(api(command.path_for(precise), "POST", Some(json!({}))).await)
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
        let confirmed = |value: &Value| match (value == true, en) {
            (true, false) => "是",
            (true, true) => "Yes",
            (false, false) => "尚未确认",
            (false, true) => "Not confirmed",
        };
        html! {<section class="local-speech-status local-asr-status" aria-label={if en {"Local transcription"} else {"本地转写"}}>
            <p role="status"><strong>{phase.title_for(props.precise, en)}</strong></p>
            <p>{phase.description_for(props.precise, en)}</p>
            {if props.status["shutting_down"] == true {html!{<p>{if en {"The workstation is closing."} else {"工作台正在关闭。"}}</p>}} else {Html::default()}}
            <div class="local-speech-actions">
                {if phase.can_retry(&props.status) {html!{<button class="text-button" disabled={disabled} onclick={ctx.link().callback(|_|Msg::Retry)}>{if en {"Retry local transcription preparation"} else {"重试本地转写准备"}}</button>}} else {Html::default()}}
                {if phase.can_stop(&props.status) {html!{<button class="text-button" disabled={disabled} onclick={ctx.link().callback(|_|Msg::Stop)}>{if props.precise {if en {"Turn off precise transcription"} else {"停用精确转写"}} else if en {"Turn off live transcription"} else {"停用实时转写"}}</button>}} else {Html::default()}}
                {if self.pending.is_some() {html!{<span role="status">{if en {"Applying request…"} else {"正在处理请求…"}}</span>}} else {Html::default()}}
            </div>
            {self.notice.map(|notice|html!{<p role="status">{match (notice,en) {
                (Notice::Requested,false)=>"请求已提交，等待状态更新。",
                (Notice::Requested,true)=>"Request submitted; waiting for a status update.",
                (Notice::Unconfirmed,false)=>"未能确认操作结果。请检查当前状态；不会自动重试。",
                (Notice::Unconfirmed,true)=>"The result could not be confirmed. Check the current status; there is no automatic retry.",
            }}</p>}).unwrap_or_default()}
            <details><summary>{if en {"Transcription details"} else {"转写详情"}}</summary><dl>
                <dt>{if en {"Service ready"} else {"服务已就绪"}}</dt><dd>{confirmed(&props.status["ready"])}</dd>
                <dt>{if en {"Warm-up completed"} else {"预热已完成"}}</dt><dd>{confirmed(&props.status["warmup_completed"])}</dd>
                <dt>{if en {"Enabled for future recordings"} else {"已启用于后续录音"}}</dt><dd>{confirmed(&props.status["active"])}</dd>
            </dl><p>{if en {"These preparation milestones do not mean that the microphone is recording or that a transcript is accurate."} else {"这些准备状态不表示麦克风正在录音，也不代表转写结果准确。"}}</p></details>
        </section>}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_wait_can_be_canceled_without_claiming_readiness() {
        let status = json!({"phase":"waiting_gpu","mode":"managed","busy":true,
            "active":false,"ready":false,"warmup_completed":false,"shutting_down":false});
        let phase = Phase::from_status(&status).unwrap();
        assert_eq!(phase, Phase::WaitingGpu);
        assert!(phase.can_stop(&status));
        assert!(!phase.can_retry(&status));
    }

    #[test]
    fn missing_or_disabled_configuration_is_hidden_and_legacy_flags_do_not_enable_retry() {
        for status in [Value::Null, json!({}), json!({"phase":"disabled"})] {
            assert_eq!(Phase::from_status(&status), None);
        }
        let old = json!({"phase":"degraded","retry_available":true});
        assert!(!Phase::from_status(&old).unwrap().can_retry(&old));
    }
    #[test]
    fn service_readiness_and_warmth_do_not_claim_an_installed_capture_client() {
        for (phase, expected) in [("loading", Phase::Loading), ("warming", Phase::Warming)] {
            assert_eq!(
                Phase::from_status(
                    &json!({"phase":phase,"ready":true,"warmup_completed":true,"active":false})
                ),
                Some(expected)
            );
        }
        assert_eq!(
            Phase::from_status(&json!({"phase":"active","active":false})),
            Some(Phase::Unknown)
        );
    }
    #[test]
    fn retry_requires_a_terminal_phase_and_all_explicit_permissions() {
        let good =
            json!({"phase":"degraded","retry_available":true,"busy":false,"shutting_down":false});
        assert!(Phase::Degraded.can_retry(&good));
        for field in ["retry_available", "busy", "shutting_down"] {
            let mut status = good.clone();
            status[field] = json!(field != "retry_available");
            assert!(!Phase::Degraded.can_retry(&status));
        }
        for phase in [
            Phase::Active,
            Phase::Stopping,
            Phase::CleanupFailed,
            Phase::Unknown,
        ] {
            assert!(!phase.can_retry(&good));
        }
    }
    #[test]
    fn active_owned_service_can_stop_while_busy_without_stopping_the_recording_ui() {
        let mut status = json!({"phase":"active","mode":"managed","active":true,"busy":true,"shutting_down":false});
        assert!(Phase::Active.can_stop(&status));
        assert_eq!(Command::Stop.path(), "/api/audio/local-asr/stop");
        status["shutting_down"] = json!(true);
        assert!(!Phase::Active.can_stop(&status));
        status["shutting_down"] = json!(false);
        assert!(!Phase::Stopping.can_stop(&status));
        status["mode"] = json!("unknown");
        assert!(!Phase::Active.can_stop(&status));
    }
    #[test]
    fn fixed_bilingual_projection_cannot_render_backend_paths_or_secrets() {
        let raw = json!({"phase":"cleanup_failed","active":false,"error":"secret credential C:/private", "ownership_stop":{"path":"private"}});
        let phase = Phase::from_status(&raw).unwrap();
        assert_eq!(phase, Phase::CleanupFailed);
        for en in [false, true] {
            assert!(!phase.title(en).contains("private"));
            assert!(!phase.description(en).contains("secret"));
        }
        for phase in [
            Phase::Starting,
            Phase::Loading,
            Phase::Warming,
            Phase::Active,
            Phase::Degraded,
            Phase::Stopping,
            Phase::Stopped,
            Phase::CleanupFailed,
            Phase::ConfigurationError,
            Phase::Unknown,
        ] {
            assert!(phase.title(true).is_ascii());
            assert!(phase.description(true).is_ascii());
        }
    }
}

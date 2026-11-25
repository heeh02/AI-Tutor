use crate::{
    api::{api, ApiResult},
    app::Action,
    i18n::t,
    model::{fallback, frame_box, list, selected_track_label, text, track_label},
    preview_browser::{self, ImageSource, PaintOpportunity, PreviewSocket, VisibilityListener},
    preview_stream::{frame_age, parse_packet, FrameGate, FrameKey, FrameTicket},
    view::{icon, SelectControl},
};
use gloo_timers::callback::Timeout;
use serde_json::{json, Value};
use web_sys::{HtmlImageElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct CameraProps {
    pub ui_language: String,
    #[prop_or_default]
    pub compact: bool,
    pub camera: Value,
    pub ptz: Value,
    pub cameras: Vec<Value>,
    pub camera_id: String,
    pub busy: bool,
    pub on_action: Callback<Action>,
    pub on_camera: Callback<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Transport {
    WebSocket,
    Http,
}

pub enum CameraMsg {
    Resume,
    Fetch,
    HttpFrame(u64, u64, ApiResult),
    Wire(u64, Result<Vec<u8>, String>),
    ConnectionTimeout(u64),
    ImageLoaded(FrameTicket),
    ImageDecoded(FrameTicket, bool),
    ImageFailed(FrameTicket),
    Paint(FrameTicket, bool),
    PaintFailed(FrameTicket),
    Age(FrameTicket),
    Visibility(bool),
    UseHttp,
    RetryStream,
    FrameAction(FrameTicket, Action),
    ToggleCamera,
    ToggleMirror,
}

/// Owns all preview work. Parent updates carry controls/status, never pixels.
pub struct CameraPreview {
    frame: Value,
    gate: FrameGate,
    ticket: Option<FrameTicket>,
    source: Option<ImageSource>,
    image: NodeRef,
    ready: bool,
    decoding: bool,
    received_at: f64,
    received_age: f64,
    paint_after_render: Option<FrameTicket>,
    paint: Option<PaintOpportunity>,
    decode_timeout: Option<Timeout>,
    age_timer: Option<Timeout>,
    timer: Option<Timeout>,
    socket: Option<PreviewSocket>,
    transport: Transport,
    connection: u64,
    http_sequence: u64,
    http_pending: Option<u64>,
    visibility: Option<VisibilityListener>,
    visible: bool,
    closing: bool,
    error: String,
    mirrored: bool,
}

fn selected_key(frame: &Value, _camera: &Value) -> String {
    // Selection belongs only to the decoded frame, never an older state poll.
    text(&frame["selected_track_key"])
}
fn click(ctx: &Context<CameraPreview>, action: Action) -> Callback<MouseEvent> {
    ctx.props().on_action.reform(move |_| action.clone())
}

impl CameraPreview {
    fn active(&self, ctx: &Context<Self>) -> bool {
        self.visible
            && !self.closing
            && matches!(
                ctx.props().camera["state"].as_str(),
                Some("running" | "opening" | "connecting" | "reconnecting")
            )
    }
    fn age(&self) -> f64 {
        frame_age(self.received_age, preview_browser::now() - self.received_at)
    }
    fn clear_image(&mut self) -> Option<FrameKey> {
        if let Some(image) = self.image.cast::<HtmlImageElement>() {
            let _ = image.remove_attribute("src");
        }
        self.source = None; // Revoke Blob URL before an old decode can complete.
        self.ticket = None;
        self.ready = false;
        self.decoding = false;
        self.paint_after_render = None;
        self.paint = None;
        self.decode_timeout = None;
        self.age_timer = None;
        self.gate.discard()
    }
    fn stop_transport(&mut self) {
        self.connection = self.connection.wrapping_add(1);
        self.socket = None;
        self.timer = None;
        self.clear_image();
        self.gate.reset();
        self.frame = Value::Null;
        // An outstanding HTTP request retains the single request slot. A new
        // WebSocket cannot begin until that response releases it.
    }
    fn fail(&mut self, error: String) {
        self.stop_transport();
        self.error = error;
    }
    fn acknowledge(&mut self, key: FrameKey) {
        if self.transport == Transport::WebSocket {
            if let Some(socket) = &self.socket {
                if let Err(error) = socket.ack(&key.ack()) {
                    self.fail(error);
                }
            }
        }
    }
    fn resume(&mut self, ctx: &Context<Self>) {
        if !self.active(ctx) || !self.error.is_empty() || self.http_pending.is_some() {
            return;
        }
        if self.transport == Transport::Http {
            ctx.link().send_message(CameraMsg::Fetch);
        } else if self.socket.is_none() {
            self.connection = self.connection.wrapping_add(1);
            let connection = self.connection;
            match PreviewSocket::connect(
                ctx.link()
                    .callback(move |data| CameraMsg::Wire(connection, data)),
            ) {
                Ok(socket) => {
                    self.socket = Some(socket);
                    let link = ctx.link().clone();
                    self.timer = Some(Timeout::new(5000, move || {
                        link.send_message(CameraMsg::ConnectionTimeout(connection))
                    }));
                }
                Err(error) => self.fail(error),
            }
        }
    }
    fn next_http(&mut self, ctx: &Context<Self>, delay: u32) {
        if self.transport == Transport::Http && self.active(ctx) && self.error.is_empty() {
            let link = ctx.link().clone();
            self.timer = Some(Timeout::new(delay, move || {
                link.send_message(CameraMsg::Fetch)
            }));
        }
    }
    fn age_deadline(&mut self, ctx: &Context<Self>, ticket: FrameTicket) {
        let age = self.age();
        let limit = if age < 400.0 { 400.0 } else { 1000.0 };
        let delay = if age.is_finite() {
            (limit - age).max(1.0) as u32
        } else {
            1
        };
        let link = ctx.link().clone();
        self.age_timer = Some(Timeout::new(delay, move || {
            link.send_message(CameraMsg::Age(ticket))
        }));
    }
    fn begin_image(
        &mut self,
        ctx: &Context<Self>,
        meta: Value,
        key: FrameKey,
        source: ImageSource,
    ) -> Result<(), String> {
        let ticket = self.gate.begin(key)?;
        self.received_age = crate::preview_stream::arrival_age(&meta, js_sys::Date::now());
        self.frame = meta;
        self.source = Some(source);
        self.ready = false;
        self.decoding = false;
        self.received_at = preview_browser::now();
        self.ticket = Some(ticket.clone());
        self.age_deadline(ctx, ticket.clone());
        let link = ctx.link().clone();
        self.decode_timeout = Some(Timeout::new(1500, move || {
            link.send_message(CameraMsg::ImageFailed(ticket))
        }));
        Ok(())
    }
}

impl Component for CameraPreview {
    type Message = CameraMsg;
    type Properties = CameraProps;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(CameraMsg::Resume);
        Self {
            frame: Value::Null,
            gate: FrameGate::default(),
            ticket: None,
            source: None,
            image: NodeRef::default(),
            ready: false,
            decoding: false,
            received_at: 0.0,
            received_age: f64::INFINITY,
            paint_after_render: None,
            paint: None,
            decode_timeout: None,
            age_timer: None,
            timer: None,
            socket: None,
            transport: Transport::WebSocket,
            connection: 0,
            http_sequence: 0,
            http_pending: None,
            visibility: VisibilityListener::new(ctx.link().callback(CameraMsg::Visibility)),
            visible: !preview_browser::hidden(),
            closing: false,
            error: String::new(),
            mirrored: true,
        }
    }
    fn update(&mut self, ctx: &Context<Self>, msg: CameraMsg) -> bool {
        match msg {
            CameraMsg::Resume => {
                self.resume(ctx);
            }
            CameraMsg::Fetch => {
                if self.transport != Transport::Http
                    || !self.active(ctx)
                    || self.http_pending.is_some()
                    || !self.error.is_empty()
                    || self.gate.pending()
                {
                    return false;
                }
                self.timer = None;
                self.http_sequence = self.http_sequence.wrapping_add(1);
                let sequence = self.http_sequence;
                let connection = self.connection;
                self.http_pending = Some(sequence);
                ctx.link().send_future(async move {
                    CameraMsg::HttpFrame(
                        connection,
                        sequence,
                        api("/api/video/frame", "GET", None).await,
                    )
                });
                return false;
            }
            CameraMsg::HttpFrame(connection, sequence, result) => {
                if self.http_pending != Some(sequence) {
                    return false;
                }
                self.http_pending = None;
                if connection != self.connection
                    || self.transport != Transport::Http
                    || !self.active(ctx)
                {
                    self.resume(ctx);
                    return false;
                }
                match result {
                    Ok(mut meta) => {
                        if let Some(jpeg) = meta["jpeg"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .map(str::to_owned)
                        {
                            meta["delivery_id"] = json!(sequence);
                            let image = ImageSource::legacy(&jpeg);
                            meta.as_object_mut().unwrap().remove("jpeg");
                            let result = FrameKey::from_metadata(&meta)
                                .and_then(|key| self.begin_image(ctx, meta, key, image));
                            if let Err(error) = result {
                                self.fail(error);
                            }
                        } else {
                            self.clear_image();
                            self.frame = meta;
                            self.next_http(ctx, 250);
                        }
                    }
                    Err(error) => self.fail(error),
                }
            }
            CameraMsg::Wire(connection, result) => {
                if connection != self.connection || self.socket.is_none() || !self.active(ctx) {
                    return false;
                }
                self.timer = None;
                let result = result.and_then(|bytes| parse_packet(&bytes));
                match result {
                    Ok(packet) => {
                        if let Err(error) = self.gate.observe_delivery(packet.delivery_id) {
                            self.fail(error);
                            return true;
                        }
                        if let Some(key) = packet.key {
                            let result = ImageSource::jpeg(&packet.jpeg).and_then(|source| {
                                self.begin_image(ctx, packet.metadata, key, source)
                            });
                            if let Err(error) = result {
                                self.fail(error);
                            }
                        } else {
                            let discarded = self.clear_image();
                            self.frame = packet.metadata;
                            if let Some(key) = discarded {
                                self.acknowledge(key);
                            }
                        }
                    }
                    Err(error) => self.fail(error),
                }
            }
            CameraMsg::ConnectionTimeout(connection) => {
                if connection != self.connection || self.socket.is_none() {
                    return false;
                }
                self.fail("二进制预览连接未及时返回状态。".into());
            }
            CameraMsg::ImageLoaded(ticket) => {
                if !self.gate.current(&ticket) || self.decoding || !self.active(ctx) {
                    return false;
                }
                let Some(image) = self.image.cast::<HtmlImageElement>() else {
                    return false;
                };
                let Some(source) = self.source.as_ref().map(|v| v.url.clone()) else {
                    return false;
                };
                self.decoding = true;
                let width = self.frame["width"].as_u64().unwrap_or(0) as u32;
                let height = self.frame["height"].as_u64().unwrap_or(0) as u32;
                ctx.link().send_future(async move {
                    CameraMsg::ImageDecoded(
                        ticket,
                        preview_browser::decode_image(image, source, width, height).await,
                    )
                });
                return false;
            }
            CameraMsg::ImageDecoded(ticket, success) => {
                if !self.gate.current(&ticket) || !self.active(ctx) {
                    return false;
                }
                if !success {
                    self.fail("预览图像解码失败，旧画面已清除。".into());
                } else if self.gate.decoded(&ticket) {
                    self.decode_timeout = None;
                    self.ready = true;
                    self.paint_after_render = Some(ticket);
                }
            }
            CameraMsg::ImageFailed(ticket) => {
                if !self.gate.current(&ticket) || self.ready {
                    return false;
                }
                self.fail("预览图像无法解码，旧画面已清除。".into());
            }
            CameraMsg::PaintFailed(ticket) => {
                if !self.gate.current(&ticket) {
                    return false;
                }
                self.fail("浏览器无法安排预览绘制。".into());
            }
            CameraMsg::Paint(ticket, second) => {
                if !self.gate.current(&ticket) || !self.active(ctx) {
                    return false;
                }
                self.paint = None;
                if second {
                    if let Some(key) = self.gate.painted(&ticket) {
                        self.acknowledge(key);
                        self.next_http(ctx, 33);
                    }
                } else {
                    self.paint = PaintOpportunity::new(
                        ctx.link()
                            .callback(move |_| CameraMsg::Paint(ticket.clone(), true)),
                    );
                    if self.paint.is_none() {
                        self.fail("浏览器无法安排预览绘制。".into());
                    }
                }
                return !self.error.is_empty();
            }
            CameraMsg::Age(ticket) => {
                if !self.gate.current(&ticket) {
                    return false;
                }
                if self.age() >= 1000.0 {
                    if let Some(key) = self.clear_image() {
                        self.acknowledge(key);
                    }
                    self.next_http(ctx, 33);
                } else {
                    self.age_deadline(ctx, ticket);
                }
            }
            CameraMsg::Visibility(hidden) => {
                self.visible = !hidden;
                self.stop_transport();
                if self.visible {
                    self.resume(ctx);
                }
            }
            CameraMsg::UseHttp | CameraMsg::RetryStream => {
                self.stop_transport();
                self.transport = if matches!(msg, CameraMsg::UseHttp) {
                    Transport::Http
                } else {
                    Transport::WebSocket
                };
                self.error.clear();
                self.closing = false;
                self.resume(ctx);
            }
            CameraMsg::FrameAction(ticket, action) => {
                if self.active(ctx)
                    && self.ready
                    && self.age() < 400.0
                    && self.gate.current(&ticket)
                    && !ctx.props().busy
                {
                    ctx.props().on_action.emit(action);
                }
                return false;
            }
            CameraMsg::ToggleCamera => {
                if ctx.props().busy {
                    return false;
                }
                let close = matches!(
                    ctx.props().camera["state"].as_str(),
                    Some("running" | "opening" | "connecting" | "reconnecting")
                );
                self.closing = close;
                self.stop_transport();
                self.error.clear();
                ctx.props().on_action.emit(Action::Camera(close));
            }
            CameraMsg::ToggleMirror => {
                self.mirrored = !self.mirrored;
            }
        }
        true
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        let next = &ctx.props().camera;
        let phase_changed = old.camera["state"] != next["state"];
        let epoch_changed = old.camera["camera_session_id"] != next["camera_session_id"];
        if epoch_changed || (phase_changed && next["state"] != "running") {
            self.stop_transport();
            self.error.clear();
        }
        if !matches!(
            next["state"].as_str(),
            Some("running" | "opening" | "connecting" | "reconnecting")
        ) {
            self.closing = false;
        }
        self.resume(ctx);
        true
    }
    fn rendered(&mut self, ctx: &Context<Self>, _: bool) {
        if let Some(ticket) = self.paint_after_render.take() {
            if self.gate.current(&ticket) && self.active(ctx) {
                // Two rAF callbacks provide a paint opportunity between them;
                // this is not a hardware presentation timestamp.
                let failed = ticket.clone();
                self.paint = PaintOpportunity::new(
                    ctx.link()
                        .callback(move |_| CameraMsg::Paint(ticket.clone(), false)),
                );
                if self.paint.is_none() {
                    ctx.link().send_message(CameraMsg::PaintFailed(failed));
                }
            }
        }
    }
    fn destroy(&mut self, _: &Context<Self>) {
        self.stop_transport();
        self.visibility = None;
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        self.render_panel(ctx)
    }
}

struct PtzPresentation {
    label: &'static str,
    can_stop: bool,
    direction_calibrated: bool,
}

fn ptz_presentation(ptz: &Value) -> PtzPresentation {
    let phase = ptz["status"].as_str().unwrap_or("");
    let direction_calibrated = ptz["direction_calibration"]["result"] == "PASS_DIRECTION_ONLY"
        && ptz["direction_calibration"]["calibration"]["direction_verified"] == true;
    let can_stop = matches!(
        phase,
        "ARMING" | "VERIFYING" | "PAUSED" | "STOPPING" | "FOLLOWING" | "SETTLING"
    ) || ptz["mode"] == "external_yolo"
        || ptz["external_yolo_enabled"] == true;
    let label = match phase {
        "PAUSED" => "暂时暂停 · 等待原目标",
        "ARMING" | "VERIFYING" => "正在核验设备 · 可随时停止",
        "STOPPING" => "正在停止跟随",
        _ if can_stop => "物理跟随已启用",
        _ if direction_calibrated && ptz["ready_for_follow"] == true => "方向已标定 · 等待启用",
        _ if direction_calibrated => "方向已标定 · 等待有效目标",
        _ => "物理跟随关闭 · 尚未通过标定",
    };
    PtzPresentation {
        label,
        can_stop,
        direction_calibrated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cleared_selection_never_reuses_the_previous_state_poll() {
        let old = json!({"selected_track_key":"old-epoch:target"});
        assert_eq!(selected_key(&json!({"selected_track_key":null}), &old), "");
        assert_eq!(
            selected_key(&json!({"selected_track_key":"new:target"}), &old),
            "new:target"
        );
        assert_eq!(selected_key(&Value::Null, &old), "");
    }

    fn direction_proof() -> Value {
        json!({"result":"PASS_DIRECTION_ONLY","calibration":{"direction_verified":true}})
    }

    #[test]
    fn paused_following_keeps_stop_available_without_claiming_it_is_uncalibrated() {
        let display = ptz_presentation(&json!({
            "status":"PAUSED","mode":"external_yolo","external_yolo_enabled":false,
            "ready_for_follow":false,"direction_calibration":direction_proof()
        }));
        assert_eq!(display.label, "暂时暂停 · 等待原目标");
        assert!(display.can_stop);
        assert!(display.direction_calibrated);
    }

    #[test]
    fn arming_verification_and_stopping_can_always_be_cancelled() {
        for phase in ["ARMING", "VERIFYING", "STOPPING"] {
            let display = ptz_presentation(&json!({
                "status":phase,"mode":"off","external_yolo_enabled":false,"ready_for_follow":false
            }));
            assert!(
                display.can_stop,
                "{phase} must leave the stop control available"
            );
            assert!(!display.label.contains("尚未通过标定"));
        }
    }

    #[test]
    fn current_target_readiness_does_not_erase_verified_direction_capability() {
        let mut state = json!({
            "status":"BLOCKED","mode":"off","external_yolo_enabled":false,
            "ready_for_follow":false,"direction_calibration":direction_proof()
        });
        let awaiting_target = ptz_presentation(&state);
        assert!(awaiting_target.direction_calibrated);
        assert!(!awaiting_target.can_stop);
        assert_eq!(awaiting_target.label, "方向已标定 · 等待有效目标");
        state["ready_for_follow"] = json!(true);
        assert_eq!(ptz_presentation(&state).label, "方向已标定 · 等待启用");
        state["direction_calibration"]["calibration"]["direction_verified"] = json!(false);
        let unverified = ptz_presentation(&state);
        assert!(
            !unverified.direction_calibrated,
            "readiness alone is not direction proof"
        );
        assert_eq!(unverified.label, "物理跟随关闭 · 尚未通过标定");
    }

    #[test]
    fn active_following_remains_stoppable_when_no_new_arm_is_ready() {
        let display = ptz_presentation(&json!({
            "status":"FOLLOWING","mode":"external_yolo","external_yolo_enabled":true,
            "ready_for_follow":false,"direction_calibration":direction_proof()
        }));
        assert_eq!(display.label, "物理跟随已启用");
        assert!(display.can_stop);
    }
}

impl CameraPreview {
    fn frame_action(&self, ctx: &Context<Self>, action: Action) -> Callback<MouseEvent> {
        let ticket = self.ticket.clone();
        let link = ctx.link().clone();
        Callback::from(move |_| {
            if let Some(ticket) = &ticket {
                link.send_message(CameraMsg::FrameAction(ticket.clone(), action.clone()));
            }
        })
    }
    fn render_panel(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let frame = &self.frame;
        let image_visible = self.source.is_some() && self.active(ctx) && self.age() < 1000.0;
        let has_frame = image_visible && self.ready;
        let selectable = has_frame && self.age() < 400.0 && frame["preview_selectable"] != false;
        let camera_open = matches!(
            props.camera["state"].as_str(),
            Some("running" | "opening" | "connecting" | "reconnecting")
        );
        let camera_state = frame["state"]
            .as_str()
            .or_else(|| props.camera["state"].as_str())
            .unwrap_or("closed");
        let interrupted = !has_frame && (camera_state == "error" || camera_state == "running");
        let camera_error = if !self.error.is_empty() {
            json!(self.error)
        } else if text(&frame["error"]).is_empty() {
            props.camera["error"].clone()
        } else {
            frame["error"].clone()
        };
        let selected = if has_frame {
            selected_key(frame, &props.camera)
        } else {
            String::new()
        };
        let selected_name = selected_track_label(&frame["tracks"], &selected);
        let selected_status = match frame["selected_status"].as_str() {
            Some("visible") => "目标在画面内",
            Some("temporarily_lost") => "目标暂时丢失",
            _ => "临时视觉目标",
        };
        let frame_style = if image_visible {
            format!(
                "aspect-ratio:{}/{}",
                frame["width"].as_u64().unwrap_or(16),
                frame["height"].as_u64().unwrap_or(9)
            )
        } else {
            String::new()
        };
        let ptz = ptz_presentation(&props.ptz);
        let ptz_ready = props.ptz["ready_for_follow"] == true;
        let ptz_can_enable = selectable
            && ptz_ready
            && !ptz.can_stop
            && ptz.direction_calibrated
            && !selected.is_empty()
            && frame["selected_status"] == "visible"
            && frame["association_ambiguous"] != true;
        let ptz_label = ptz.label;
        let device_selector = html! {<div class="camera-toolbar"><SelectControl label={if props.ui_language=="en"{"Camera"}else{"摄像头"}} value={props.camera_id.clone()} onchange={props.on_camera.reform(|event:Event|event.target_unchecked_into::<HtmlSelectElement>().value())}><option value="">{t(&props.ui_language,"自动选择已发现的物理相机")}</option>{for props.cameras.iter().map(|camera|{let id=camera.get("device_id").or_else(||camera.get("id")).map(text).unwrap_or_default();html!{<option value={id}>{fallback(&camera["name"],"摄像头")}</option>}})}</SelectControl><button class="icon-button" title={if props.ui_language=="en"{"Discover devices again"}else{"重新发现设备"}} onclick={click(ctx,Action::Discover)}>{icon("refresh")}</button></div>};
        html! {<article class={classes!("panel","camera-panel",props.compact.then_some("camera-compact"))}><div class="panel-title"><div><span class="section-number">{"01"}</span>{if props.compact{icon("camera")}else{Html::default()}}<h2>{if props.compact{if props.ui_language=="en"{"A little window into the room"}else{"看见你的世界"}}else{t(&props.ui_language,"看见当前目标")}}</h2></div><span class="small-label">{if props.compact{if props.ui_language=="en"{"Local camera"}else{"本机摄像头"}}else{t(&props.ui_language,"仅人脸检测")}}</span></div>
            {if !props.compact{device_selector.clone()}else{Html::default()}}
            <div class="camera-main">
            <div class={classes!("camera-view",has_frame.then_some("has-frame"))} style={frame_style}>
            {if image_visible{let source=self.source.as_ref().unwrap();let ticket=self.ticket.as_ref().unwrap().clone();let failed=ticket.clone();html!{<><img key={source.url.clone()} ref={self.image.clone()} src={source.url.clone()} onload={ctx.link().callback(move |_|CameraMsg::ImageLoaded(ticket.clone()))} onerror={ctx.link().callback(move |_|CameraMsg::ImageFailed(failed.clone()))} alt="本机摄像头当前画面" style={if self.mirrored{"transform:scaleX(-1)"}else{""}}/>{if has_frame{html!{<div class="face-overlay">{for list(&frame["tracks"]).iter().filter_map(|track|{let style=frame_box(&track["box"],self.mirrored)?;let key=text(&track["track_key"]);let label=track_label(track);let active=key==selected;Some(html!{<button class={classes!("face-box",active.then_some("selected"))} style={style} disabled={!selectable||props.busy} onclick={self.frame_action(ctx,Action::Track(key.clone()))} aria-label={format!("选择画面内目标 {label}")}><span>{if active{icon("focus")}else{Html::default()}}{label}</span></button>})})}</div>}}else{Html::default()}}<span class="live-label"><i/>{t(&props.ui_language,"本机当前画面")}</span></>}}else{html!{<div class="camera-empty">{icon("camera")}<strong>{if matches!(camera_state,"opening"|"connecting"|"reconnecting"){t(&props.ui_language,"正在连接摄像头")}else if interrupted{t(&props.ui_language,"摄像头连接中断")}else{t(&props.ui_language,"摄像头尚未开启")}}</strong><p>{fallback(&camera_error,"当前没有可用画面。")}</p><p>{if interrupted{"请确认设备已开启、连接正常，再点击「重新开启摄像头」。旧画面和脸框已隐藏。"}else{"开启真实设备，点击人脸框选择本次视觉目标。"}}</p></div>}}}
            </div>
            <div class="camera-quick-controls">
            {if props.compact{html!{<p class="camera-quick-note">{if props.ui_language=="en"{"A live preview from your device. Choose a face in the picture to select a visual target."}else{"画面来自真实设备。点击画面中的人脸框，选择当前视觉目标。"}}</p>}}else{Html::default()}}
            <div class="camera-controls"><button class={if camera_open{"secondary"}else{"primary"}} disabled={props.busy} onclick={ctx.link().callback(|_|CameraMsg::ToggleCamera)}>{icon("camera")}{if camera_open{t(&props.ui_language,"关闭摄像头")}else if interrupted{t(&props.ui_language,"重新开启摄像头")}else{t(&props.ui_language,"开启摄像头")}}</button><label class="toggle-label"><input type="checkbox" checked={self.mirrored} onchange={ctx.link().callback(|_|CameraMsg::ToggleMirror)}/>{t(&props.ui_language,"镜像显示")}</label></div>
            {if props.compact&&ptz.can_stop{html!{<button class="stop-small compact-ptz-stop" title={if props.ui_language=="en"{"Cancel following and request the camera to stop"}else{"取消跟随并发送停止请求"}} onclick={click(ctx,Action::PtzStop)}>{icon("stop")}{if props.ui_language=="en"{"Stop camera following"}else{"停止摄像头跟随"}}</button>}}else{Html::default()}}
            </div></div>
            <details class="camera-settings" open={!props.compact}>
            <summary><span>{icon("settings")}{if props.ui_language=="en"{"Device, target & following settings"}else{"设备、视觉目标与跟随设置"}}</span>{icon("chevron")}</summary>
            {if props.compact{device_selector}else{Html::default()}}
            <div class="preview-transport"><span>{t(&props.ui_language,if self.transport==Transport::Http{"兼容预览（HTTP）"}else{"二进制预览"})}</span>{if camera_open && (!self.error.is_empty() || self.transport==Transport::Http){html!{<><button class="text-button" onclick={ctx.link().callback(|_|CameraMsg::RetryStream)}>{t(&props.ui_language,"重试二进制预览")}</button>{if self.transport==Transport::WebSocket || !self.error.is_empty(){html!{<button class="text-button" onclick={ctx.link().callback(|_|CameraMsg::UseHttp)}>{t(&props.ui_language,"使用兼容预览")}</button>}}else{Html::default()}}</>}}else{Html::default()}}</div>
            <div class="target-summary"><span class="target-icon">{icon("focus")}</span><div><small>{t(&props.ui_language,"当前视觉目标（画面内）")}</small><strong>{t(&props.ui_language,&selected_name).to_owned()}</strong><span>{if selected.is_empty(){t(&props.ui_language,"选择人脸不会切换学习者档案")}else{t(&props.ui_language,selected_status)}}</span></div>{if !selected.is_empty(){html!{<button class="text-button" disabled={props.busy||!selectable} onclick={self.frame_action(ctx,Action::ClearTrack)}>{t(&props.ui_language,"清除")}</button>}}else{Html::default()}}</div>
            <p class="tracking-boundary">{t(&props.ui_language,"点击脸框只选择画面内目标，不会开启物理云台跟随。")}</p>
            <div class="vision-footnote">{icon("shield")}<p>{t(&props.ui_language,"画面只在本机处理，不发送给任何远程教学服务。临时标签不代表身份、说话者或情绪。")}</p></div>
            <div class="ptz-row"><div><small>{t(&props.ui_language,"物理云台跟随")}</small><span>{t(&props.ui_language,ptz_label)}</span></div><button class="stop-small" disabled={!ptz.can_stop} title="取消跟随并发送停止请求；物理急停尚未独立验收" onclick={click(ctx,Action::PtzStop)}>{icon("stop")}{t(&props.ui_language,"停止")}</button></div>
            <details class="ptz-details"><summary>{t(&props.ui_language,"云台高级控制与标定")}{icon("chevron")}</summary><div><p class="ptz-reason">{fallback(&props.ptz["reason"],"物理运动尚未通过核验，保持关闭。")}</p><p class="ptz-instructions">{"摄像头预览、人脸框和视觉目标选择可独立使用。"}</p><p class="ptz-instructions">{if ptz.direction_calibrated{"成人试用：先点选脸框，再启用物理跟随。短暂遮挡时暂停，原目标及时恢复后可继续；长时间丢失、关联不确定或切换目标后，需要重新选择并启用。请保持厂商 AI 跟踪和 AI 模式关闭。"}else{"请先关闭厂商 AI 跟踪和 AI 模式，并完成方向标定。物理跟随与停止仍需独立实测。"}}</p>{if ptz.direction_calibrated{html!{<button class="secondary" disabled={!ptz_can_enable || props.busy} onclick={self.frame_action(ctx,Action::PtzEnable(json!({"mode":"external_yolo","camera_session_id":frame["camera_session_id"],"track_key":frame["selected_track_key"],"target_generation":frame["target_generation"]})))}>{t(&props.ui_language,"启用物理跟随（成人试用）")}</button>}}else{Html::default()}}</div></details>
            </details>
        </article>}
    }
}

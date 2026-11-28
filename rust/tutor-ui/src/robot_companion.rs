//! Read-only camera companion. Demo poses never enter a hardware command path.
use crate::api::{api, ApiResult};
use crate::view::icon;
use gloo_timers::callback::Interval;
use serde_json::{json, Value};
use wasm_bindgen::JsCast;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub ptz: Value,
    pub camera: Value,
    pub audio: Value,
    pub ui_language: String,
    pub generating: bool,
}

pub enum Msg {
    Tick,
    Pose(u64, f64, ApiResult),
    ToggleDemo,
    DemoPose(f64, f64),
    ResetView,
    Greet,
}

pub struct RobotCompanion {
    pose: Value,
    request_started_at: f64,
    request_epoch: u64,
    pending: bool,
    demo: bool,
    demo_pan: f64,
    demo_tilt: f64,
    node: NodeRef,
    _timer: Interval,
}

fn now() -> f64 {
    crate::preview_browser::now()
}
fn visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| !d.hidden())
}
fn fresh(pose: &Value, elapsed_ms: f64) -> bool {
    pose["pose_source"] == "controller_readback"
        && pose["freshness"] == "fresh"
        && pose["active"] == true
        && pose["connected"] == true
        && elapsed_ms.is_finite()
        && elapsed_ms >= 0.0
        && ["sample_age_ms", "frame_age_ms"].iter().all(|key| {
            pose[*key]
                .as_f64()
                .is_some_and(|age| age.is_finite() && age >= 0.0 && age + elapsed_ms <= 1000.0)
        })
        && pose["pan_deg"]
            .as_f64()
            .is_some_and(|v| v.is_finite() && v.abs() <= 180.0)
        && pose["tilt_deg"]
            .as_f64()
            .is_some_and(|v| v.is_finite() && v.abs() <= 90.0)
}
fn awaiting_pose(camera: &Value) -> Value {
    // General state responses have no per-pose transport timing. Wait for the
    // timed pose poll before presenting them as current device feedback.
    json!({"camera_session_id":camera["camera_session_id"],
        "connected":camera["state"] == "running", "active":false,
        "freshness":"unavailable", "pose_source":"unknown"})
}
fn response_pose(camera: &Value, mut pose: Value) -> Value {
    let expected = &camera["camera_session_id"];
    if expected.as_str().is_none_or(str::is_empty) || pose["camera_session_id"] != *expected {
        // The scalar endpoint may observe a reconnect before the general-state
        // poll. Its new session invalidates both the old display and JS cache.
        pose["active"] = json!(false);
        pose["freshness"] = json!("unavailable");
    }
    pose
}
fn render_pose(pose: &Value, elapsed_ms: f64) -> Value {
    let live = fresh(pose, elapsed_ms);
    json!({"pan_deg":if live {pose["pan_deg"].clone()} else {Value::Null},
        "tilt_deg":if live {pose["tilt_deg"].clone()} else {Value::Null},
        "neutral_pan_deg":0,"neutral_tilt_deg":0,"pan_sign":-1,"tilt_sign":1,
        "source":if live {"feedback"} else {"unknown"},"fresh":live,
        "connected":pose["connected"]==true,"revision":pose["revision"],
        "camera_session_id":pose["camera_session_id"]})
}
fn degrees(value: f64) -> String {
    if value.abs() < 0.05 {
        "0°".into()
    } else {
        format!("{value:+.0}°")
    }
}

impl Component for RobotCompanion {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        let link = ctx.link().clone();
        Self {
            pose: awaiting_pose(&ctx.props().camera),
            request_started_at: now(),
            request_epoch: 0,
            pending: false,
            demo: false,
            demo_pan: 0.0,
            demo_tilt: 0.0,
            node: NodeRef::default(),
            _timer: Interval::new(200, move || link.send_message(Msg::Tick)),
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Props) -> bool {
        if ctx.props().camera["camera_session_id"] != old.camera["camera_session_id"]
            || ctx.props().camera["state"] != old.camera["state"]
        {
            self.request_epoch += 1;
            self.pending = false;
            self.pose = awaiting_pose(&ctx.props().camera);
            self.request_started_at = now();
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Tick => {
                if !visible() {
                    return false;
                }
                if !self.pending && ctx.props().camera["state"] == "running" {
                    self.pending = true;
                    let epoch = self.request_epoch;
                    let started_at = now();
                    let link = ctx.link().clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        link.send_message(Msg::Pose(
                            epoch,
                            started_at,
                            api("/api/camera/pose", "GET", None).await,
                        ));
                    });
                }
                // Expire a once-fresh pose even if the next request never returns.
                !self.demo
                    && self.pose["active"] == true
                    && !fresh(&self.pose, now() - self.request_started_at)
            }
            Msg::Pose(epoch, started_at, result) => {
                if epoch != self.request_epoch {
                    return false;
                }
                self.pending = false;
                match result {
                    Ok(pose) => {
                        self.pose = response_pose(&ctx.props().camera, pose);
                        // Charge the full round trip conservatively; starting a
                        // new age clock at receipt would revive delayed samples.
                        self.request_started_at = started_at;
                    }
                    Err(_) => {
                        self.pose["active"] = json!(false);
                        self.pose["freshness"] = json!("stale");
                    }
                }
                true
            }
            Msg::ToggleDemo => {
                self.demo = !self.demo;
                self.demo_pan = 0.0;
                self.demo_tilt = 0.0;
                true
            }
            Msg::DemoPose(pan, tilt) => {
                if !self.demo {
                    return false;
                }
                self.demo_pan = pan;
                self.demo_tilt = tilt;
                true
            }
            Msg::ResetView => {
                if let Some(node) = self.node.cast::<web_sys::HtmlElement>() {
                    if let Ok(method) = js_sys::Reflect::get(node.as_ref(), &"resetView".into()) {
                        if let Some(method) = method.dyn_ref::<js_sys::Function>() {
                            let _ = method.call0(node.as_ref());
                        }
                    }
                }
                false
            }
            Msg::Greet => {
                if let Some(node) = self.node.cast::<web_sys::HtmlElement>() {
                    if let Ok(method) = js_sys::Reflect::get(node.as_ref(), &"greet".into()) {
                        if let Some(method) = method.dyn_ref::<js_sys::Function>() {
                            let _ = method.call0(node.as_ref());
                        }
                    }
                }
                false
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let en = ctx.props().ui_language == "en";
        let live = fresh(&self.pose, now() - self.request_started_at);
        let (pose, pan, tilt) = if self.demo {
            (
                json!({"pan_deg":self.demo_pan,"tilt_deg":self.demo_tilt,
                "neutral_pan_deg":0,"neutral_tilt_deg":0,"pan_sign":-1,"tilt_sign":1,
                "source":"demo","connected":false,"fresh":true,
                "camera_session_id":self.pose["camera_session_id"],
                "revision":format!("demo:{}:{}",self.demo_pan,self.demo_tilt)}),
                degrees(self.demo_pan),
                degrees(self.demo_tilt),
            )
        } else {
            (
                render_pose(&self.pose, now() - self.request_started_at),
                if live {
                    degrees(self.pose["pan_deg"].as_f64().unwrap())
                } else {
                    "—".into()
                },
                if live {
                    degrees(self.pose["tilt_deg"].as_f64().unwrap())
                } else {
                    "—".into()
                },
            )
        };
        let camera_running = ctx.props().camera["state"] == "running";
        let status = if self.demo {
            if en {
                "Demo movement"
            } else {
                "演示动作"
            }
        } else if ctx.props().audio["recording"] == true {
            if en {
                "I'm listening"
            } else {
                "我在认真听"
            }
        } else if ctx.props().generating {
            if en {
                "Let me think…"
            } else {
                "让我想一想…"
            }
        } else if matches!(
            ctx.props().audio["state"].as_str(),
            Some("speaking" | "playing")
        ) {
            if en {
                "Sharing an idea"
            } else {
                "说给你听"
            }
        } else if live {
            if en {
                "Camera synced"
            } else {
                "摄像头同步中"
            }
        } else if camera_running {
            if en {
                "Waiting for pose"
            } else {
                "等待姿态反馈"
            }
        } else {
            if en {
                "Ready to chat"
            } else {
                "可以开始对话"
            }
        };
        let source = if self.demo {
            if en {
                "Local preview · no device movement"
            } else {
                "本地体验 · 不控制设备"
            }
        } else if live {
            if en {
                "Live controller readback"
            } else {
                "来自设备控制器的实时反馈"
            }
        } else if camera_running {
            if en {
                "No fresh feedback · holding the last pose"
            } else {
                "暂无新鲜反馈，保留上次姿态"
            }
        } else {
            if en {
                "Connect a camera to sync the little robot"
            } else {
                "连接摄像头，让小宇同步真实转动"
            }
        };
        let audio = &ctx.props().audio;
        let mood = if audio["recording"] == true {
            "listening"
        } else if matches!(audio["state"].as_str(), Some("speaking" | "playing")) {
            "speaking"
        } else if matches!(audio["state"].as_str(), Some("synthesizing" | "preparing"))
            || ctx.props().generating
        {
            "thinking"
        } else {
            "idle"
        };
        html! {
            <div class={classes!("robot-companion",self.demo.then_some("is-demo"),live.then_some("is-live"))} data-pose-source={if self.demo {"demo"} else if live {"controller_readback"} else {"unknown"}}>
                <style>{include_str!("../../../frontend/robot-companion.css")}</style>
                <div class="robot-topline"><span class="robot-presence"><i/>{status}</span>
                    <button class="robot-reset" onclick={ctx.link().callback(|_|Msg::ResetView)} title={if en {"Reset viewing angle"} else {"恢复观察视角"}} aria-label={if en {"Reset viewing angle"} else {"恢复观察视角"}}>{icon("refresh")}</button>
                </div>
                <div class="robot-canvas-wrap"><ai-tutor-robot ref={self.node.clone()} pose={pose.to_string()} mood={mood} lang={if en {"en"} else {"zh"}} aria-label={if en {"3D robot showing camera orientation"} else {"显示摄像头转动姿态的3D机器人"}}/></div>
                <div class="robot-view-hint">{if en {"Drag to look around · double-click to reset"} else {"拖动看看小宇 · 双击恢复视角"}}</div>
                <details class="robot-motion-panel">
                    <summary onclick={ctx.link().callback(|_|Msg::Greet)} title={if en {"Tap the robot to say hello, or try a pose"} else {"轻点机器人打个招呼，或体验动作"}}>{icon("hand")}<span>{if en {"Say hello"} else {"和小宇互动"}}</span>{icon("chevron")}</summary>
                    <div class="robot-motion-content">
                        <div class="robot-pose-strip">
                            <div class="robot-angle"><span>{if en {"PAN"} else {"水平转动"}}</span><strong>{pan}</strong></div>
                            <div class="robot-angle"><span>{if en {"TILT"} else {"抬头低头"}}</span><strong>{tilt}</strong></div>
                            <button class={classes!("robot-demo-toggle",self.demo.then_some("selected"))} aria-pressed={self.demo.to_string()} onclick={ctx.link().callback(|_|Msg::ToggleDemo)}>{if self.demo {if en {"Back to live"} else {"回到真实同步"}} else if en {"Try a pose"} else {"体验动作"}}</button>
                        </div>
                        <p class="robot-source-note" role="status">{source}</p>
                        {if self.demo {html!{<div class="robot-demo-poses" aria-label={if en {"Demo poses only"} else {"仅演示姿态"}}>
                            <button onclick={ctx.link().callback(|_|Msg::DemoPose(25.0,0.0))}>{if en {"Look left"} else {"向左看"}}</button>
                            <button onclick={ctx.link().callback(|_|Msg::DemoPose(0.0,18.0))}>{if en {"Look up"} else {"抬抬头"}}</button>
                            <button onclick={ctx.link().callback(|_|Msg::DemoPose(-25.0,0.0))}>{if en {"Look right"} else {"向右看"}}</button>
                            <button onclick={ctx.link().callback(|_|Msg::DemoPose(0.0,0.0))}>{if en {"Hello!"} else {"看这里"}}</button>
                        </div>}} else {Html::default()}}
                    </div>
                </details>
            </div>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_current_controller_feedback_drives_the_live_robot() {
        let mut pose = json!({"pose_source":"controller_readback","freshness":"fresh","active":true,"connected":true,"pan_deg":17,"tilt_deg":24,"sample_age_ms":100,"frame_age_ms":80,"revision":"a:2"});
        let view = render_pose(&pose, 50.0);
        assert_eq!(view["source"], "feedback");
        assert_eq!(view["pan_deg"], 17);
        assert_eq!(view["tilt_deg"], 24);
        assert_eq!(view["pan_sign"], -1);
        assert_eq!(view["tilt_sign"], 1);
        assert_eq!(render_pose(&pose, 901.0)["source"], "unknown");
        pose["pose_source"] = json!("commanded");
        assert!(render_pose(&pose, 0.0)["pan_deg"].is_null());
        pose["pose_source"] = json!("controller_readback");
        pose["connected"] = json!(false);
        assert_eq!(render_pose(&pose, 0.0)["source"], "unknown");
    }

    #[test]
    fn transport_delay_and_frame_age_cannot_revive_stale_feedback() {
        let mut pose = json!({"pose_source":"controller_readback","freshness":"fresh","active":true,"connected":true,"pan_deg":17,"tilt_deg":24,"sample_age_ms":100,"frame_age_ms":80});
        // A response created at t=0 arrives at t=1200, below the API timeout.
        assert_eq!(render_pose(&pose, 1200.0)["source"], "unknown");
        pose["sample_age_ms"] = json!(20);
        pose["frame_age_ms"] = json!(990);
        assert_eq!(render_pose(&pose, 10.0)["source"], "feedback");
        assert_eq!(render_pose(&pose, 20.0)["source"], "unknown");
    }

    #[test]
    fn unmeasured_state_snapshots_wait_for_the_timed_pose_poll() {
        let camera = json!({"state":"running","camera_session_id":"b",
            "pose":{"pose_source":"controller_readback","freshness":"fresh",
                "active":true,"connected":true,"pan_deg":40,"tilt_deg":10,
                "sample_age_ms":0,"frame_age_ms":0}});
        let view = render_pose(&awaiting_pose(&camera), 0.0);
        assert_eq!(view["source"], "unknown");
        assert_eq!(view["camera_session_id"], "b");
        assert!(view["pan_deg"].is_null());
    }

    #[test]
    fn new_camera_response_invalidates_the_old_live_display_immediately() {
        let camera = json!({"state":"running","camera_session_id":"a"});
        let pose = json!({"camera_session_id":"b","pose_source":"controller_readback",
            "freshness":"fresh","active":true,"connected":true,"pan_deg":17,
            "tilt_deg":24,"sample_age_ms":20,"frame_age_ms":20});
        let accepted = response_pose(&camera, pose);
        let view = render_pose(&accepted, 0.0);
        assert_eq!(view["source"], "unknown");
        assert_eq!(view["camera_session_id"], "b");
        assert!(view["pan_deg"].is_null());
    }
}

//! Explicitly armed, finite-position OSC following. UDP receipts are not physical proof.
use crate::{
    obsbot_motion::{CalibrationConfig, OscMotionSession, StopReceipt},
    obsbot_osc::{GimbalProfile, ReadState},
    ptz_follow::{
        AngleReadback, Calibration, Decision, Follower, RefreshError, RefreshedCommand,
        TargetFrame, TargetIdentity, VendorAutoState,
    },
    vision::CameraService,
};
use serde_json::{Value, json};
use std::{
    fs,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex as AsyncMutex, Notify},
    task::JoinHandle,
    time::{sleep, timeout},
};

const ABORT_LIMITATION: &str = "仅发送每轴至多3°、速度4的有限位置目标；异常退出时最后一个已接受的有限目标仍可能完成。UDP停止回执不证明物理停止。";
const STOP_FAULT_REASON: &str =
    "上一次停止未确认，已持久锁定；必须独立人工或实际状态复核，不能通过重新启用或重启清除";

pub struct NativePTZ {
    inner: Arc<Inner>,
}
struct Inner {
    root: PathBuf,
    camera: Option<Arc<CameraService>>,
    intent: AtomicU64,
    closing: AtomicBool,
    changed: Notify,
    lifecycle: AsyncMutex<()>,
    task: Mutex<Option<JoinHandle<()>>>,
    runtime: Mutex<Runtime>,
}
#[derive(Default)]
struct Runtime {
    owner: Option<u64>,
    active: bool,
    mode: &'static str,
    phase: &'static str,
    reason: String,
    commands_sent: u64,
    // Historical command counters survive pause/resume. This flag tracks only
    // accepted nonzero targets since the last complete four-zero submission.
    nonzero_since_zero: bool,
    attempts: u64,
    zero: Option<Value>,
    last_command: Option<Value>,
    last_visual_authorization: Option<Value>,
    last_visual_rejection: Option<Value>,
    last_settling_readback: Option<Value>,
    soft_skips: u64,
    pause_count: u64,
    resume_count: u64,
    pause_reason: Option<String>,
    last_dispatch_at: Option<Instant>,
    stop_fault: bool,
}

impl NativePTZ {
    /// Construction never opens a socket or a device.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::construct(root.into(), None)
    }
    pub fn with_camera(root: impl Into<PathBuf>, camera: Arc<CameraService>) -> Self {
        Self::construct(root.into(), Some(camera))
    }
    fn construct(root: PathBuf, camera: Option<Arc<CameraService>>) -> Self {
        let stop_fault = persistent_stop_fault(&root);
        Self {
            inner: Arc::new(Inner {
                root,
                camera,
                intent: AtomicU64::new(0),
                closing: AtomicBool::new(false),
                changed: Notify::new(),
                lifecycle: AsyncMutex::new(()),
                task: Mutex::new(None),
                runtime: Mutex::new(Runtime {
                    mode: "off",
                    phase: "BLOCKED",
                    stop_fault,
                    reason: if stop_fault {
                        STOP_FAULT_REASON.into()
                    } else {
                        String::new()
                    },
                    ..Default::default()
                }),
            }),
        }
    }
    pub fn intent_generation(&self) -> u64 {
        self.inner.intent.load(Ordering::SeqCst)
    }
    pub fn status(&self) -> Value {
        let mut value = status(&self.inner.root);
        let ready = if self.inner.closing.load(Ordering::SeqCst) {
            Err("服务已经关闭，不能重新启用".to_owned())
        } else {
            Gate::load(&self.inner.root).and_then(|gate| {
                let camera = self.inner.camera.as_ref().ok_or("尚未绑定实时摄像头服务")?;
                CameraTarget::parse(&camera.status(), &gate).map(|_| ())
            })
        };
        let runtime = self.inner.runtime.lock().unwrap();
        let stop_fault = runtime.stop_fault || persistent_stop_fault(&self.inner.root);
        value["ready_for_follow"] = json!(ready.is_ok() && !runtime.active && !stop_fault);
        value["stop_fault"] = json!(stop_fault);
        value["mode"] = json!(if runtime.active { runtime.mode } else { "off" });
        value["status"] = json!(if stop_fault {
            "BLOCKED"
        } else if runtime.active || !runtime.reason.is_empty() {
            runtime.phase
        } else if ready.is_ok() {
            "READY"
        } else {
            "BLOCKED"
        });
        value["reason"] = json!(if stop_fault {
            format!("{STOP_FAULT_REASON}。{}", runtime.reason)
        } else if !runtime.reason.is_empty() {
            runtime.reason.clone()
        } else {
            ready.err().unwrap_or_else(|| {
                "已具备方向标定和绑定证据；仍需教师明确启用并重新核验实时设备".into()
            })
        });
        value["commands_sent"] = json!(runtime.commands_sent);
        value["command_attempts"] = json!(runtime.attempts);
        value["last_command"] = json!(runtime.last_command);
        value["last_visual_authorization"] = json!(runtime.last_visual_authorization);
        value["last_visual_rejection"] = json!(runtime.last_visual_rejection);
        value["last_settling_readback"] = json!(runtime.last_settling_readback);
        value["soft_skips"] = json!(runtime.soft_skips);
        value["pause_count"] = json!(runtime.pause_count);
        value["resume_count"] = json!(runtime.resume_count);
        value["pause_reason"] = json!(runtime.pause_reason);
        value["zero_receipt"] = json!(runtime.zero);
        value["hardware_stop_command_sent"] = json!(
            runtime
                .zero
                .as_ref()
                .is_some_and(|r| r["all_zero_commands_sent"] == true)
        );
        value["intent_generation"] = json!(self.intent_generation());
        value["external_yolo_enabled"] =
            json!(runtime.active && runtime.mode == "external_yolo" && runtime.phase != "PAUSED");
        value["closed"] = json!(self.inner.closing.load(Ordering::SeqCst));
        value
    }
    pub fn probe(&self) -> Value {
        self.status()
    }
    pub fn enable_manual(&self) -> Result<Value, String> {
        Err("手动和厂商自动模式未开放；请使用经过标定的显式有限位置跟随".into())
    }
    pub fn manual_move(&self, _axis: &str, _delta: i32) -> Result<Value, String> {
        self.enable_manual()
    }
    pub fn set_mode(&self, mode: &str) -> Result<Value, String> {
        if mode == "off" {
            Ok(self.stop())
        } else {
            self.enable_manual()
        }
    }
    pub async fn enable_external(&self) -> Result<Value, String> {
        self.enable_external_for_intent(self.intent_generation())
            .await
    }
    /// The HTTP request's token is captured before waiting for any server lock.
    pub async fn enable_external_for_intent(&self, expected_intent: u64) -> Result<Value, String> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        ensure_intent(&self.inner, expected_intent)?;
        if self.inner.runtime.lock().unwrap().stop_fault || persistent_stop_fault(&self.inner.root)
        {
            return Err(STOP_FAULT_REASON.into());
        }
        if self.inner.runtime.lock().unwrap().active {
            return Err("跟随已启动或正在停止，请先完成停止".into());
        }
        if self
            .inner
            .task
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|t| !t.is_finished())
        {
            return Err("上一轮停止仍在清理，请稍后明确重新启用".into());
        }
        let gate = Gate::load(&self.inner.root)?;
        let camera = self.inner.camera.as_ref().ok_or("尚未绑定实时摄像头服务")?;
        let target = CameraTarget::parse(&camera.status(), &gate)?;
        // Stop can race all reads above: only the original request may claim an owner.
        let owner = expected_intent.wrapping_add(1);
        self.inner
            .intent
            .compare_exchange(expected_intent, owner, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "启用请求已被停止或新请求取消".to_owned())?;
        ensure_intent(&self.inner, owner)?;
        {
            let mut runtime = self.inner.runtime.lock().unwrap();
            ensure_intent(&self.inner, owner)?;
            if runtime.stop_fault || persistent_stop_fault(&self.inner.root) {
                return Err(STOP_FAULT_REASON.into());
            }
            *runtime = Runtime {
                owner: Some(owner),
                active: true,
                mode: "off",
                phase: "ARMING",
                reason: "正在只读核验设备身份、厂商跟踪关闭和角度稳定性".into(),
                ..Default::default()
            };
        }
        let inner = self.inner.clone();
        *self.inner.task.lock().unwrap() = Some(tokio::spawn(async move {
            run_actor(inner, owner, gate, target).await;
        }));
        Ok(self.status())
    }
    /// First operation invalidates active dispatches AND every queued arm request.
    pub fn stop(&self) -> Value {
        self.inner.intent.fetch_add(1, Ordering::SeqCst);
        self.inner.changed.notify_waiters();
        {
            let mut runtime = self.inner.runtime.lock().unwrap();
            runtime.mode = "off";
            if runtime.active {
                runtime.phase = "STOPPING";
                runtime.reason = "应用已取消跟随，正在尝试四方向零值停止".into();
            }
        }
        let mut value = self.status();
        value["application_transactions_cancelled"] = json!(true);
        value
    }
    pub async fn stop_and_wait(&self) -> Value {
        self.stop_with_budget(Duration::from_secs(2)).await
    }
    async fn stop_with_budget(&self, budget: Duration) -> Value {
        self.stop();
        let _lifecycle = self.inner.lifecycle.lock().await;
        // A concurrently claimed enable is invalidated again before taking ownership.
        self.inner.intent.fetch_add(1, Ordering::SeqCst);
        self.inner.changed.notify_waiters();
        let task = self.inner.task.lock().unwrap().take();
        if let Some(mut task) = task {
            if timeout(budget, &mut task).await.is_err() {
                task.abort();
                // Abort requests are cooperative. Do not add an unbounded join
                // after the public two-second cleanup budget has expired.
                // Keep ownership until the cancelled task really finishes; its
                // final cleanup must never overlap a newly armed controller.
                *self.inner.task.lock().unwrap() = Some(task);
                let mut runtime = self.inner.runtime.lock().unwrap();
                runtime.active = false;
                runtime.mode = "off";
                runtime.phase = "BLOCKED";
                runtime.reason = format!("停止清理超时，运行任务已取消。{ABORT_LIMITATION}");
                latch_stop_fault(&self.inner.root, &mut runtime, 2);
            }
        }
        self.status()
    }
    pub fn close(&self) -> Value {
        self.inner.closing.store(true, Ordering::SeqCst);
        self.stop()
    }
}
impl Drop for NativePTZ {
    fn drop(&mut self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        self.inner.intent.fetch_add(1, Ordering::SeqCst);
        self.inner.changed.notify_waiters();
    }
}

fn ensure_intent(inner: &Inner, expected: u64) -> Result<(), String> {
    if inner.closing.load(Ordering::SeqCst) {
        Err("服务已关闭".into())
    } else if inner.intent.load(Ordering::SeqCst) != expected {
        Err("跟随已停止或启用请求已经过期，需要重新明确启用".into())
    } else {
        Ok(())
    }
}

#[derive(Clone)]
struct Gate {
    config: CalibrationConfig,
    calibration: Calibration,
    width: u64,
    height: u64,
    signature: Vec<u8>,
}
impl Gate {
    fn load(root: &Path) -> Result<Self, String> {
        let config_bytes = bounded_read(&root.join("runtime/ptz-osc-config.json"), 65536)?;
        let report_bytes = bounded_read(&root.join("runtime/ptz-calibration-rust.json"), 65536)?;
        let saved = bounded_read(&root.join("runtime/rust_camera_device.txt"), 4096)?;
        let config: Value = serde_json::from_slice(&config_bytes).map_err(|_| "OSC配置格式无效")?;
        let report: Value =
            serde_json::from_slice(&report_bytes).map_err(|_| "方向标定报告格式无效")?;
        let saved_device = std::str::from_utf8(&saved)
            .map_err(|_| "稳定摄像头标识不是UTF-8")?
            .trim()
            .to_owned();
        let mut gate = Self::parse(
            &config,
            &report,
            &saved_device,
            [config_bytes, report_bytes, saved].concat(),
        )?;
        gate.config.canonical_root = Some(root.to_path_buf());
        Ok(gate)
    }
    fn parse(
        config: &Value,
        report: &Value,
        saved_device: &str,
        signature: Vec<u8>,
    ) -> Result<Self, String> {
        let fail = |s: &str| Err(s.to_owned());
        if config["operator_confirmation"]["vendor_ai_tracking_master_off"] != true
            || config["operator_confirmation"]["vendor_ai_mode_master_off"] != true
        {
            return fail("必须明确确认 OBSBOT Center 的两个自动跟踪总开关均已关闭");
        }
        if config["expected_name"] != "Tiny 3"
            || config["device_index"] != 0
            || config["gimbal_profile"] != "Tiny3Center2015PanPitch"
        {
            return fail("OSC设备名称、序号或协议配置与已核验 Tiny 3 不一致");
        }
        if config["pose_source"] != "windows_native_confirmed_osc" {
            return fail("必须使用经过原生读数交叉确认的云台角度");
        }
        let device = config["camera_device_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("缺少稳定摄像头标识")?;
        if device != saved_device {
            return fail("OSC绑定与当前保存的物理摄像头标识不同");
        }
        let port = config["server_port"]
            .as_u64()
            .filter(|p| *p > 0 && *p <= 65535)
            .ok_or("OSC服务端口无效")? as u16;
        // A previous session remains available for final zero-stop while the next is verified.
        if config["reply_port"] != 0 || config["command_bind_port"] != 0 {
            return fail("跟随运行时要求独立临时本地端口，避免重新核验时覆盖停止通道");
        }
        if report["result"] != "PASS_DIRECTION_ONLY"
            || report["calibration"]["direction_verified"] != true
        {
            return fail("尚无通过的双轴物理方向标定报告");
        }
        for key in [
            "camera_device_id",
            "server_port",
            "device_index",
            "expected_name",
            "gimbal_profile",
            "pose_source",
        ] {
            if report["binding"][key].is_null() || report["binding"][key] != config[key] {
                return fail("方向标定的设备、端口或协议绑定与当前配置不一致");
            }
        }
        if report["binding"]["capture_width"] != 1280 || report["binding"]["capture_height"] != 720
        {
            return fail("方向标定必须绑定原始1280×720采集坐标");
        }
        let sign = |key: &str| -> Result<i8, String> {
            match report["calibration"][key].as_i64() {
                Some(-1) => Ok(-1),
                Some(1) => Ok(1),
                _ => Err("方向标定符号必须为±1".into()),
            }
        };
        Ok(Self {
            config: CalibrationConfig {
                server_port: port,
                reply_port: 0,
                command_bind_port: 0,
                device_index: 0,
                expected_name: "Tiny 3".into(),
                query_timeout: Duration::from_millis(600),
                gimbal_profile: GimbalProfile::Tiny3Center2015PanPitch,
                canonical_root: None,
            },
            calibration: Calibration {
                complete: true,
                pan_sign: sign("pan_sign")?,
                tilt_sign: sign("tilt_sign")?,
            },
            width: 1280,
            height: 720,
            signature,
        })
    }
}
fn bounded_read(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = fs::metadata(path).map_err(|_| {
        format!(
            "缺少本地能力证据：{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })?;
    if meta.len() > max {
        return Err("本地能力证据超出长度限制".into());
    }
    fs::read(path).map_err(|_| "无法读取本地能力证据".into())
}

#[derive(Clone, Debug)]
struct CameraTarget {
    frame: TargetFrame,
    target_generation: u64,
    selection_generation: u64,
}
impl CameraTarget {
    fn parse(value: &Value, gate: &Gate) -> Result<Self, String> {
        if value["state"] != "running"
            || value["width"] != gate.width
            || value["height"] != gate.height
        {
            return Err("摄像头未运行或采集尺寸已改变，需重新标定".into());
        }
        if value["selected_status"] != "visible" || value["association_ambiguous"] != false {
            return Err("所选视觉目标丢失或关联不明确，需重新选择并启用".into());
        }
        let key = value["selected_track_key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("请先选择当前画面内目标")?;
        let epoch = value["camera_session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("缺少摄像头连接代次")?;
        let age = value["frame_age_ms"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 250.0)
            .ok_or("画面超过250毫秒或帧龄无效，跟随已停止")?
            .ceil() as u64;
        let track = value["tracks"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t["track_key"] == key))
            .ok_or("所选目标不在当前原始画面中")?;
        let coordinates = track["box"]
            .as_array()
            .filter(|v| v.len() == 4)
            .ok_or("原始目标坐标无效")?;
        let mut bbox = [0.0; 4];
        for (i, coordinate) in coordinates.iter().enumerate() {
            bbox[i] = coordinate
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or("原始目标坐标无效")?;
        }
        if bbox[0] >= bbox[2] || bbox[1] >= bbox[3] {
            return Err("原始目标坐标无效".into());
        }
        Ok(Self {
            frame: TargetFrame {
                frame_id: value["frame_id"].as_u64().ok_or("缺少有效帧序号")?,
                frame_age_ms: age,
                camera_epoch: epoch.into(),
                target_key: key.into(),
                bbox,
                ambiguous: false,
            },
            target_generation: value["target_generation"]
                .as_u64()
                .ok_or("缺少视觉选择代次")?,
            selection_generation: value["selection_generation"]
                .as_u64()
                .ok_or("缺少教师选择代次")?,
        })
    }
    fn same_selection(&self, other: &Self) -> bool {
        self.target_generation == other.target_generation
            && self.selection_generation == other.selection_generation
            && self.frame.camera_epoch == other.frame.camera_epoch
            && self.frame.target_key == other.frame.target_key
    }
}

#[derive(Debug)]
enum ActorExit {
    Visual(String),
    Hard(String),
}
impl ActorExit {
    fn context(self, prefix: &str) -> Self {
        match self {
            Self::Visual(reason) => Self::Visual(format!("{prefix}：{reason}")),
            Self::Hard(reason) => Self::Hard(format!("{prefix}：{reason}")),
        }
    }
}
impl From<String> for ActorExit {
    fn from(reason: String) -> Self {
        Self::Hard(reason)
    }
}
impl From<&str> for ActorExit {
    fn from(reason: &str) -> Self {
        Self::Hard(reason.into())
    }
}

enum PauseObservation {
    Waiting,
    Fresh(CameraTarget),
}
const MAX_VISUAL_PAUSE_MS: u64 = 600;

/// Pure classification. A visibility generation may change, while the explicit
/// teacher selection, epoch and opaque track key must remain identical.
fn pause_observation(
    value: &Value,
    gate: &Gate,
    armed: &CameraTarget,
    elapsed_ms: u64,
    initial_last_seen_age_ms: f64,
) -> Result<PauseObservation, String> {
    if elapsed_ms > MAX_VISUAL_PAUSE_MS
        || !initial_last_seen_age_ms.is_finite()
        || initial_last_seen_age_ms < 0.0
        || initial_last_seen_age_ms + elapsed_ms as f64 > MAX_VISUAL_PAUSE_MS as f64
    {
        return Err("短暂视觉暂停已超过600毫秒，需重新选择并启用".into());
    }
    if value["state"] != "running" || value["width"] != gate.width || value["height"] != gate.height
    {
        return Err("摄像头停止或采集几何已改变".into());
    }
    if value["association_ambiguous"] != false {
        return Err("视觉关联不明确，禁止自动恢复".into());
    }
    if value["camera_session_id"] != armed.frame.camera_epoch
        || value["selected_track_key"] != armed.frame.target_key
        || value["selection_generation"].as_u64() != Some(armed.selection_generation)
        || value["target_generation"].as_u64().is_none()
    {
        return Err("摄像头或教师选择已改变，禁止自动恢复".into());
    }
    let bounded_age = |key: &str| {
        value[key]
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..=600.0).contains(v))
    };
    let age = bounded_age("frame_age_ms").ok_or("画面超过600毫秒或帧龄无效")?;
    bounded_age("selected_last_seen_age_ms").ok_or("所选目标超过600毫秒未见或缺少可见时间证据")?;
    if value["selected_status"] == "visible" && age < 250.0 {
        return CameraTarget::parse(value, gate).map(PauseObservation::Fresh);
    }
    if value["selected_status"] == "temporarily_lost" || value["selected_status"] == "visible" {
        Ok(PauseObservation::Waiting)
    } else {
        Err("所选目标已清空或不可用，禁止自动恢复".into())
    }
}

fn same_gate(inner: &Inner, gate: &Gate) -> Result<(), String> {
    match Gate::load(&inner.root) {
        Ok(new) if new.signature == gate.signature => Ok(()),
        Ok(_) => Err("运行配置或方向证据已改变，需重新明确启用".into()),
        Err(error) => Err(error),
    }
}
fn current_target(
    inner: &Inner,
    owner: u64,
    gate: &Gate,
    armed: &CameraTarget,
) -> Result<CameraTarget, String> {
    ensure_intent(inner, owner)?;
    let target = CameraTarget::parse(&inner.camera.as_ref().ok_or("未绑定摄像头")?.status(), gate)?;
    if !armed.same_selection(&target) {
        return Err("视觉选择或摄像头代次已改变；需要重新明确启用".into());
    }
    Ok(target)
}
async fn guarded<T>(
    inner: &Inner,
    owner: u64,
    gate: &Gate,
    armed: &CameraTarget,
    future: impl Future<Output = anyhow::Result<T>>,
) -> Result<T, ActorExit> {
    tokio::pin!(future);
    loop {
        current_target(inner, owner, gate, armed).map_err(ActorExit::Visual)?;
        tokio::select! {
            biased;
            _ = inner.changed.notified() => { current_target(inner, owner, gate, armed).map_err(ActorExit::Visual)?; }
            _ = sleep(Duration::from_millis(25)) => {}
            result = &mut future => {
                // A real provider/readback failure must not become resumable
                // merely because a face disappeared at the same instant.
                let value=result.map_err(|error|ActorExit::Hard(error.to_string()))?;
                current_target(inner, owner, gate, armed).map_err(ActorExit::Visual)?;
                return Ok(value);
            }
        }
    }
}
fn progress(inner: &Inner, owner: u64, phase: &'static str, reason: &str) {
    let mut r = inner.runtime.lock().unwrap();
    if ensure_intent(inner, owner).is_ok() && r.owner == Some(owner) {
        r.phase = phase;
        r.mode = "external_yolo";
        r.reason = reason.into();
    }
}

async fn run_actor(inner: Arc<Inner>, owner: u64, gate: Gate, mut armed: CameraTarget) {
    let mut cleanup = ActorCleanup {
        inner: inner.clone(),
        owner,
        completed: false,
    };
    let mut session: Option<OscMotionSession> = None;
    let reason = 'running: loop {
        let exit = actor_body(&inner, owner, &gate, &armed, &mut session).await;
        let interruption = match exit {
            ActorExit::Hard(reason) => break reason,
            ActorExit::Visual(reason) => reason,
        };
        if let Err(error) = ensure_intent(&inner, owner).and_then(|_| same_gate(&inner, &gate)) {
            break error;
        }
        let camera = match inner.camera.as_ref() {
            Some(camera) => camera,
            None => break "摄像头服务不可用".into(),
        };
        let initial = camera.status();
        let initial_last_seen = initial["selected_last_seen_age_ms"]
            .as_f64()
            .unwrap_or(f64::INFINITY);
        if let Err(error) = pause_observation(&initial, &gate, &armed, 0, initial_last_seen) {
            break format!("{interruption}；{error}");
        }
        let paused_at = Instant::now();
        {
            let mut runtime = inner.runtime.lock().unwrap();
            if ensure_intent(&inner, owner).is_err() {
                break 'running "跟随已取消".into();
            }
            runtime.pause_count += 1;
            runtime.pause_reason = Some(interruption.clone());
            runtime.phase = "PAUSED";
            runtime.mode = "external_yolo";
            runtime.reason = format!("短暂视觉中断，已作废旧步骤并暂停：{interruption}");
            runtime.last_visual_authorization = None;
        }
        // actor_body's pending future and Follower have already been dropped.
        // Only an existing verified writer can submit the four zero commands.
        let pause_zero = if let Some(ref mut verified) = session {
            Some(verified.stop().await.map(|receipt|stop_value(&receipt)).unwrap_or_else(|error|json!({"all_zero_commands_sent":false,"error":error.to_string(),"physical_stop_verified":false})))
        } else {
            None
        };
        session = None;
        {
            let mut runtime = inner.runtime.lock().unwrap();
            if let Err(error) = apply_pause_stop(&inner.root, &mut runtime, pause_zero) {
                break 'running error;
            }
        }
        loop {
            if let Err(error) = ensure_intent(&inner, owner).and_then(|_| same_gate(&inner, &gate))
            {
                break 'running error;
            }
            if inner.runtime.lock().unwrap().stop_fault || persistent_stop_fault(&inner.root) {
                break 'running STOP_FAULT_REASON.into();
            }
            let meta = camera.status();
            match pause_observation(
                &meta,
                &gate,
                &armed,
                paused_at.elapsed().as_millis() as u64,
                initial_last_seen,
            ) {
                Ok(PauseObservation::Fresh(recovered)) => {
                    // This new visibility generation is only a binding for a
                    // completely new proof/Follower, never the prior proposal.
                    armed = recovered;
                    let mut runtime = inner.runtime.lock().unwrap();
                    if ensure_intent(&inner, owner).is_err() {
                        break 'running "跟随已取消".into();
                    }
                    runtime.resume_count += 1;
                    runtime.phase = "ARMING";
                    runtime.reason =
                        "目标重新可见，正在重新核验身份、角度与跟随权限；旧步骤不会复用".into();
                    break;
                }
                Ok(PauseObservation::Waiting) => {}
                Err(error) => break 'running format!("{interruption}；{error}"),
            }
            tokio::select! {
                biased;
                _=inner.changed.notified()=>{},
                _=sleep(Duration::from_millis(25))=>{},
            }
        }
    };
    let zero = if let Some(ref mut session) = session {
        Some(session.stop().await.map(|r| stop_value(&r)).unwrap_or_else(|e| json!({"all_zero_commands_sent":false,"error":e.to_string(),"physical_stop_verified":false})))
    } else {
        None
    };
    let mut runtime = inner.runtime.lock().unwrap();
    if runtime.owner == Some(owner) {
        runtime.active = false;
        runtime.mode = "off";
        let cancelled = ensure_intent(&inner, owner).is_err();
        let partial = if zero.is_some() {
            apply_stop_receipt(&inner.root, &mut runtime, zero)
        } else {
            false
        };
        runtime.phase = if cancelled && !partial && !runtime.stop_fault {
            "STOPPED"
        } else {
            "BLOCKED"
        };
        runtime.reason = if partial {
            format!("{reason}；四方向零值停止未全部发送。{ABORT_LIMITATION}")
        } else {
            format!("{reason}；需要教师明确重新启用。{ABORT_LIMITATION}")
        };
    }
    cleanup.completed = true;
}
struct ActorCleanup {
    inner: Arc<Inner>,
    owner: u64,
    completed: bool,
}
impl Drop for ActorCleanup {
    fn drop(&mut self) {
        if !self.completed {
            let mut r = self.inner.runtime.lock().unwrap();
            if r.owner == Some(self.owner) {
                r.active = false;
                r.mode = "off";
                r.phase = "BLOCKED";
                r.reason = format!("运行任务异常结束，停止发送未得到完整回执。{ABORT_LIMITATION}");
                latch_stop_fault(&self.inner.root, &mut r, 3);
            }
        }
    }
}
fn stop_value(receipt: &StopReceipt) -> Value {
    serde_json::to_value(receipt)
        .unwrap_or_else(|_| json!({"all_zero_commands_sent":false,"physical_stop_verified":false}))
}
fn apply_stop_receipt(root: &Path, runtime: &mut Runtime, zero: Option<Value>) -> bool {
    let partial = zero
        .as_ref()
        .is_some_and(|z| z["all_zero_commands_sent"] != true);
    if zero
        .as_ref()
        .is_some_and(|receipt| receipt["all_zero_commands_sent"] == true)
    {
        runtime.nonzero_since_zero = false;
    }
    runtime.zero = zero;
    if partial {
        latch_stop_fault(root, runtime, 1);
    }
    partial
}

fn apply_pause_stop(root: &Path, runtime: &mut Runtime, zero: Option<Value>) -> Result<(), String> {
    if let Some(zero) = zero {
        if apply_stop_receipt(root, runtime, Some(zero)) {
            return Err("暂停时四方向零值未全部提交，禁止自动恢复".into());
        }
    } else if runtime.nonzero_since_zero {
        latch_stop_fault(root, runtime, 3);
        return Err("暂停时缺少已验证停止通道，禁止自动恢复".into());
    }
    Ok(())
}

/// Presence (including malformed/unreadable content) is fail-closed. This marker
/// is never removed or overwritten by this runtime or its public endpoints.
fn persistent_stop_fault(root: &Path) -> bool {
    match fs::metadata(root.join("runtime/stop-fault.json")) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}
fn latch_stop_fault(root: &Path, runtime: &mut Runtime, code: u8) {
    runtime.stop_fault = true;
    let data = json!({"schema_version":1,"fault_code":code,"owner_generation":runtime.owner.unwrap_or(0),
        "commands_sent":runtime.commands_sent,"command_attempts":runtime.attempts,
        "zero_commands_sent":runtime.zero.as_ref().and_then(|v|v["zero_commands_sent"].as_u64()).unwrap_or(0),
        "zero_commands_attempted":runtime.zero.as_ref().and_then(|v|v["commands_attempted"].as_u64()).unwrap_or(0),
        "created_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).map(|d|d.as_millis()).unwrap_or(0)});
    let written = (|| -> std::io::Result<()> {
        fs::create_dir_all(root.join("runtime"))?;
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join("runtime/stop-fault.json"))
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => return Err(e),
        };
        file.write_all(data.to_string().as_bytes())?;
        file.sync_all()
    })();
    if written.is_err() {
        runtime
            .reason
            .push_str("；停止故障标记无法写入，当前进程仍保持锁定");
    }
}

async fn actor_body(
    inner: &Inner,
    owner: u64,
    gate: &Gate,
    armed: &CameraTarget,
    session: &mut Option<OscMotionSession>,
) -> ActorExit {
    // Rebuilding a Follower after a pause must not reset the physical command
    // rate budget retained by the outer actor.
    let cooldown = inner
        .runtime
        .lock()
        .unwrap()
        .last_dispatch_at
        .map(|at| {
            Duration::from_millis(crate::ptz_follow::MIN_COMMAND_INTERVAL_MS)
                .saturating_sub(at.elapsed())
        })
        .unwrap_or_default();
    if !cooldown.is_zero() {
        if let Err(error) = guarded(inner, owner, gate, armed, async {
            sleep(cooldown).await;
            Ok(())
        })
        .await
        {
            return error;
        }
    }
    let mut follower = Follower::new();
    if let Err(e) = follower.arm(
        TargetIdentity {
            camera_epoch: armed.frame.camera_epoch.clone(),
            target_key: armed.frame.target_key.clone(),
        },
        gate.calibration,
        VendorAutoState::ConfirmedOff,
    ) {
        return format!("无法启用有限跟随：{e:?}").into();
    }
    let origin = Instant::now();
    let mut last_frame = None;
    loop {
        if let Err(e) = current_target(inner, owner, gate, armed) {
            return ActorExit::Visual(e);
        }
        // File changes never silently inherit the old physical direction authority.
        match Gate::load(&inner.root) {
            Ok(new) if new.signature == gate.signature => {}
            Ok(_) => return "运行配置或方向证据已改变，需重新明确启用".into(),
            Err(e) => return e.into(),
        }
        let verified = match guarded(
            inner,
            owner,
            gate,
            armed,
            OscMotionSession::verify_for_calibration(gate.config.clone()),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return e.context("实时OSC核验停止"),
        };
        *session = Some(verified);
        let refreshed_at = Instant::now();
        progress(
            inner,
            owner,
            "FOLLOWING",
            "有限位置跟随已启用；每轴每步至多3°，画面丢失立即取消，不代表已完成物理验收",
        );
        loop {
            let target = match current_target(inner, owner, gate, armed) {
                Ok(t) => t,
                Err(e) => return ActorExit::Visual(e),
            };
            // Refresh a centered/limited idle controller before its one-shot
            // permission expires; a later off-center face must not reuse it.
            if refreshed_at.elapsed() >= Duration::from_secs(1) {
                break;
            }
            if last_frame.is_some_and(|id| target.frame.frame_id < id) {
                return "帧序号倒退，跟随已停止".into();
            }
            if last_frame == Some(target.frame.frame_id) {
                sleep(Duration::from_millis(25)).await;
                continue;
            }
            last_frame = Some(target.frame.frame_id);
            let pose = session.as_ref().unwrap().verified_pose();
            let now = origin.elapsed().as_millis() as u64;
            let decision = follower.update(
                now,
                Some(&target.frame),
                AngleReadback {
                    pan_degrees: pose.yaw_degrees.into(),
                    tilt_degrees: pose.pitch_degrees.into(),
                },
                gate.calibration,
                VendorAutoState::ConfirmedOff,
            );
            match decision {
                Decision::Stop(reason) => return format!("有限跟随保护停止：{reason:?}").into(),
                Decision::Hold(_) => {
                    sleep(Duration::from_millis(25)).await;
                }
                Decision::FinitePosition(command) => {
                    let dp = (command.pan_degrees - f64::from(pose.yaw_degrees)) as f32;
                    let dt = (command.tilt_degrees - f64::from(pose.pitch_degrees)) as f32;
                    let refreshed_authority = Mutex::new(None::<RefreshedCommand>);
                    let visual_rejection = Mutex::new(None::<RefreshError>);
                    let target_rejection = Mutex::new(None::<String>);
                    let dispatch = || {
                        let latest = match current_target(inner, owner, gate, armed) {
                            Ok(target) => target,
                            Err(error) => {
                                *target_rejection.lock().unwrap() = Some(error);
                                return false;
                            }
                        };
                        let checked = follower.refresh_command(
                            origin.elapsed().as_millis() as u64,
                            Some(&latest.frame),
                            AngleReadback {
                                pan_degrees: pose.yaw_degrees.into(),
                                tilt_degrees: pose.pitch_degrees.into(),
                            },
                            gate.calibration,
                            VendorAutoState::ConfirmedOff,
                            &command,
                        );
                        match checked {
                            Ok(authority) => {
                                // Stop/selection invalidation is atomic and remains
                                // the final check before the provider's UDP send.
                                if origin.elapsed().as_millis()
                                    >= u128::from(authority.expires_at_ms)
                                {
                                    *visual_rejection.lock().unwrap() = Some(RefreshError::Stop(
                                        crate::ptz_follow::StopReason::StaleFrame,
                                    ));
                                    return false;
                                }
                                *refreshed_authority.lock().unwrap() = Some(authority);
                                ensure_intent(inner, owner).is_ok()
                            }
                            Err(reason) => {
                                *visual_rejection.lock().unwrap() = Some(reason);
                                false
                            }
                        }
                    };
                    {
                        let mut r = inner.runtime.lock().unwrap();
                        r.attempts += 1;
                    }
                    let result = guarded(
                        inner,
                        owner,
                        gate,
                        armed,
                        session
                            .as_mut()
                            .unwrap()
                            .finite_follow_step_guarded(dp, dt, &dispatch),
                    )
                    .await;
                    let target_failure = target_rejection.lock().unwrap().take();
                    let rejection = *visual_rejection.lock().unwrap();
                    let result = match result {
                        Err(ActorExit::Hard(error))
                            if error == "osc_motion_dispatch_guard_rejected"
                                && target_failure.is_some() =>
                        {
                            Err(ActorExit::Visual(target_failure.unwrap()))
                        }
                        Err(ActorExit::Hard(error))
                            if error == "osc_motion_dispatch_guard_rejected"
                                && rejection
                                    == Some(RefreshError::Stop(
                                        crate::ptz_follow::StopReason::StaleFrame,
                                    )) =>
                        {
                            Err(ActorExit::Visual("发送前画面已短暂过期".into()))
                        }
                        other => other,
                    };
                    let authority = refreshed_authority.lock().unwrap().clone();
                    {
                        let mut r = inner.runtime.lock().unwrap();
                        r.last_visual_authorization=authority.as_ref().map(|permit|json!({"command":permit,"target_generation":armed.target_generation,"intent_generation":owner}));
                        r.last_visual_rejection=rejection.map(|reason|json!({"reason":format!("{reason:?}"),"proposal_frame_id":command.frame_id,"generated_at_ms":origin.elapsed().as_millis(),"udp_sent":false}));
                    }
                    let receipt = match result {
                        Ok(r) => r,
                        Err(ActorExit::Hard(ref e)) if soft_visual_skip(e, rejection) => {
                            inner.runtime.lock().unwrap().soft_skips += 1;
                            progress(
                                inner,
                                owner,
                                "FOLLOWING",
                                "最新画面不再支持原步骤，未发送，正在重新观察和规划",
                            );
                            break; // Consumed permission; reverify before replanning.
                        }
                        Err(e) => return e.context("有限位置发送停止"),
                    };
                    if !receipt.udp_send_accepted {
                        return "有限位置UDP发送未得到完整回执".into();
                    }
                    {
                        if let Err(reason) =
                            follower.record_dispatch(origin.elapsed().as_millis() as u64)
                        {
                            return format!("发送时钟无效，已停止：{reason:?}").into();
                        }
                        let mut r = inner.runtime.lock().unwrap();
                        r.commands_sent += 1;
                        r.nonzero_since_zero = true;
                        r.last_dispatch_at = Some(Instant::now());
                        let mut record = serde_json::to_value(&receipt).unwrap_or(Value::Null);
                        record["visual_authorization"] = json!(authority);
                        record["target_generation"] = json!(armed.target_generation);
                        record["intent_generation"] = json!(owner);
                        r.last_command = Some(record);
                    }
                    // Do not accumulate goals while an integer angle readback has not arrived.
                    progress(
                        inner,
                        owner,
                        "SETTLING",
                        "等待上一条有限目标到位，期间继续监测目标与取消状态",
                    );
                    let deadline = Instant::now() + Duration::from_secs(4);
                    // Sequential OSC and native reads observe different instants
                    // during acceleration. Allow a bounded initial settling
                    // window before requiring their strict stationary agreement.
                    // The visual target and cancellation remain checked every 25ms.
                    if let Err(error) = guarded(inner, owner, gate, armed, async {
                        sleep(Duration::from_millis(500)).await;
                        Ok(())
                    })
                    .await
                    {
                        return error;
                    }
                    let mut reached = 0;
                    let mut previous_candidate: Option<(crate::obsbot_motion::Pose, String)> = None;
                    loop {
                        if Instant::now() >= deadline {
                            return "上一条有限目标未在4秒内得到两次到位读回，已停止".into();
                        }
                        let observed = match guarded(
                            inner,
                            owner,
                            gate,
                            armed,
                            session.as_mut().unwrap().read_pose(),
                        )
                        .await
                        {
                            Ok(p) => p,
                            Err(e) => return e.context("到位读回失败"),
                        };
                        if Instant::now() >= deadline {
                            return "上一条有限目标的到位读回超过4秒期限，已停止".into();
                        }
                        let raw = match &observed.read_receipt.reply.state {
                            ReadState::GimbalPosition {
                                roll_unused_degrees,
                                pitch_degrees,
                                yaw_degrees,
                            } => crate::obsbot_motion::Pose {
                                roll_unused_degrees: *roll_unused_degrees,
                                pitch_degrees: *pitch_degrees,
                                yaw_degrees: *yaw_degrees,
                            },
                            _ => return "到位查询返回了错误类型，已停止".into(),
                        };
                        let assessment = crate::settling::assess(
                            pose,
                            command.pan_degrees,
                            command.tilt_degrees,
                            observed.pose,
                            raw,
                        );
                        inner.runtime.lock().unwrap().last_settling_readback = Some(json!({
                            "start":pose,"target_pan":command.pan_degrees,"target_pitch":command.tilt_degrees,
                            "native":observed.pose,"raw_osc":raw,"assessment":assessment,
                            "arrival_integer_boundary_units":1,"transit_target_boundary_units":2,"maximum_finite_step_degrees":3,
                            "held_axis_maximum_integer_difference":1
                        }));
                        if !assessment.safe {
                            return format!("到位读回保护停止：{}", assessment.reason).into();
                        }
                        let sample_id = observed
                            .canonical_pose_evidence
                            .as_ref()
                            .map(|e| e.native_timestamp_utc.clone())
                            .unwrap_or_else(|| observed.read_receipt.received_at.clone());
                        if assessment.arrival {
                            reached = match &previous_candidate {
                                Some((p, id)) if *p == observed.pose && *id != sample_id => {
                                    reached + 1
                                }
                                _ => 1,
                            };
                            previous_candidate = Some((observed.pose, sample_id));
                        } else {
                            reached = 0;
                            previous_candidate = None;
                        }
                        if reached >= 2 {
                            break;
                        }
                        // Split the settling interval so cancellation never waits for it.
                        for _ in 0..4 {
                            if let Err(e) = current_target(inner, owner, gate, armed) {
                                return ActorExit::Visual(e);
                            }
                            sleep(Duration::from_millis(25)).await;
                        }
                    }
                    break; // Reverify identity, AI-off and stable pose for the next one-shot authority.
                }
            }
        }
    }
}

fn soft_visual_skip(error: &str, rejection: Option<RefreshError>) -> bool {
    error == "osc_motion_dispatch_guard_rejected"
        && matches!(rejection, Some(RefreshError::Replan(_)))
}

fn read_json(path: impl AsRef<Path>) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}
pub fn status(root: &Path) -> Value {
    let read = read_json(root.join("runtime/uvc_probe.json"));
    let historical = read_json(root.join("runtime/ptz_motion_probe.json"));
    let calibration = read_json(root.join("runtime/ptz-calibration-rust.json"));
    let capabilities = read
        .as_ref()
        .and_then(|r| r["devices"].as_array())
        .and_then(|d| d.iter().find(|d| d["matches_saved_stable_path"] == true))
        .map(|d| d["camera_control"].clone());
    json!({"mode":"off","status":"BLOCKED","runtime":"Rust native / guarded finite OSC positions","ready_for_follow":false,
        "hardware_read_capable":capabilities.as_ref().is_some_and(|c| c["interface_supported"] == true),"capabilities":capabilities,
        "capabilities_source":"historical read-only evidence; not current physical verification",
        "low_amplitude_calibration":calibration.as_ref().and_then(|r|r["result"].as_str()).or_else(|| historical.as_ref().and_then(|r|r["result"].as_str())).unwrap_or("NOT_RUN"),
        "historical_uvc_calibration":historical,"direction_calibration":calibration,
        "external_follow":"explicit arm required","external_yolo_enabled":false,"vendor_auto_state":"requires operator off confirmation plus live lock query",
        "manual_enabled":false,"manual_driver_verified":false,"physical_follow_verified":false,"physical_stop_verified":false,
        "device_emergency_stop_verified":false,"continuous_speed":false,"hardware_stop_command_sent":false,"cached_evidence_only":true,
        "reason":Gate::load(root).err().unwrap_or_else(|| "设备方向证据就绪，尚未明确启用或核验实时目标".into()),"limitation":ABORT_LIMITATION})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_visual_replan_guard_rejections_are_soft_skipped() {
        use crate::ptz_follow::{ReplanReason, StopReason};
        assert!(soft_visual_skip(
            "osc_motion_dispatch_guard_rejected",
            Some(RefreshError::Replan(ReplanReason::DirectionNoLongerValid))
        ));
        assert!(soft_visual_skip(
            "osc_motion_dispatch_guard_rejected",
            Some(RefreshError::Replan(ReplanReason::NoNewerFrame))
        ));
        assert!(!soft_visual_skip(
            "osc_motion_dispatch_guard_rejected",
            Some(RefreshError::Stop(StopReason::StaleFrame))
        ));
        assert!(!soft_visual_skip(
            "target changed",
            Some(RefreshError::Replan(ReplanReason::NoNewerFrame))
        ));
        assert!(!soft_visual_skip(
            "osc_motion_dispatch_guard_rejected",
            None
        ));
    }
    fn evidence() -> (Value, Value) {
        let c = json!({"server_port":16284,"reply_port":0,"command_bind_port":0,"device_index":0,"expected_name":"Tiny 3","gimbal_profile":"Tiny3Center2015PanPitch","pose_source":"windows_native_confirmed_osc","camera_device_id":"stable","operator_confirmation":{"vendor_ai_tracking_master_off":true,"vendor_ai_mode_master_off":true}});
        let mut binding = c.clone();
        binding["capture_width"] = json!(1280);
        binding["capture_height"] = json!(720);
        let r = json!({"result":"PASS_DIRECTION_ONLY","calibration":{"direction_verified":true,"pan_sign":1,"tilt_sign":-1},"binding":binding});
        (c, r)
    }
    fn gate() -> Gate {
        let (c, r) = evidence();
        Gate::parse(&c, &r, "stable", vec![]).unwrap()
    }
    fn camera() -> Value {
        json!({"state":"running","width":1280,"height":720,"selected_status":"visible","association_ambiguous":false,"selected_track_key":"A","camera_session_id":"capture1","frame_age_ms":0,"frame_id":5,"target_generation":7,"selection_generation":11,"selected_last_seen_age_ms":0,"tracks":[{"track_key":"A","box":[0.6,0.3,0.8,0.5]}]})
    }
    #[test]
    fn pause_short_miss_retains_only_explicit_binding_and_requires_new_proof() {
        let gate = gate();
        let armed = CameraTarget::parse(&camera(), &gate).unwrap();
        let mut lost = camera();
        lost["selected_status"] = json!("temporarily_lost");
        lost["tracks"] = json!([]);
        lost["target_generation"] = json!(8);
        lost["selected_last_seen_age_ms"] = json!(30);
        assert!(matches!(
            pause_observation(&lost, &gate, &armed, 0, 30.0),
            Ok(PauseObservation::Waiting)
        ));
        let mut recovered = camera();
        recovered["target_generation"] = json!(8);
        recovered["frame_id"] = json!(6);
        let resumed = match pause_observation(&recovered, &gate, &armed, 70, 30.0).unwrap() {
            PauseObservation::Fresh(target) => target,
            _ => panic!("fresh binding expected"),
        };
        assert_eq!(resumed.selection_generation, armed.selection_generation);
        assert_eq!(resumed.target_generation, 8);
        assert!(
            !armed.same_selection(&resumed),
            "old one-shot binding must stay invalid"
        );
        assert_eq!(armed.target_generation, 7);
    }
    #[test]
    fn pause_keeps_250ms_frame_limit_and_600ms_total_absence_boundary() {
        let gate = gate();
        let armed = CameraTarget::parse(&camera(), &gate).unwrap();
        let mut meta = camera();
        for age in [250, 251, 600] {
            meta["frame_age_ms"] = json!(age);
            meta["selected_last_seen_age_ms"] = json!(age);
            assert!(matches!(
                pause_observation(&meta, &gate, &armed, 0, age as f64),
                Ok(PauseObservation::Waiting)
            ));
        }
        meta["frame_age_ms"] = json!(100);
        meta["selected_last_seen_age_ms"] = json!(600);
        assert!(matches!(
            pause_observation(&meta, &gate, &armed, 500, 100.0),
            Ok(PauseObservation::Fresh(_))
        ));
        assert!(pause_observation(&meta, &gate, &armed, 501, 100.0).is_err());
        meta["selected_last_seen_age_ms"] = json!(0);
        assert!(
            pause_observation(&meta, &gate, &armed, 501, 100.0).is_err(),
            "new visibility cannot reset the original absence budget"
        );
        assert!(pause_observation(&meta, &gate, &armed, 601, 0.0).is_err());
    }
    #[test]
    fn pause_never_resumes_aba_clear_ambiguity_epoch_or_invalid_metadata() {
        let gate = gate();
        let armed = CameraTarget::parse(&camera(), &gate).unwrap();
        for (field, value) in [
            ("selection_generation", json!(13)),
            ("selection_generation", Value::Null),
            ("target_generation", Value::Null),
            ("selected_track_key", json!("B")),
            ("selected_track_key", Value::Null),
            ("camera_session_id", json!("capture2")),
            ("association_ambiguous", json!(true)),
            ("state", json!("error")),
            ("width", json!(640)),
            ("frame_age_ms", json!(601)),
            ("selected_last_seen_age_ms", json!(601)),
            ("selected_last_seen_age_ms", Value::Null),
            ("selected_status", json!("unselected")),
        ] {
            let mut meta = camera();
            meta[field] = value;
            assert!(
                pause_observation(&meta, &gate, &armed, 0, 0.0).is_err(),
                "{field}"
            );
        }
        assert!(pause_observation(&camera(), &gate, &armed, 0, f64::NAN).is_err());
    }
    #[test]
    fn provider_errors_stay_hard_and_stop_invalidates_paused_owner_immediately() {
        assert!(matches!(
            ActorExit::Hard("OSC readback failure".into()).context("read"),
            ActorExit::Hard(_)
        ));
        assert!(matches!(
            ActorExit::Visual("temporary loss".into()).context("read"),
            ActorExit::Visual(_)
        ));
        let directory = tempfile::tempdir().unwrap();
        let ptz = NativePTZ::new(directory.path());
        let owner = ptz.intent_generation();
        {
            let mut runtime = ptz.inner.runtime.lock().unwrap();
            runtime.owner = Some(owner);
            runtime.active = true;
            runtime.phase = "PAUSED";
            runtime.mode = "external_yolo";
            runtime.pause_count = 2;
            runtime.commands_sent = 9;
        }
        assert!(!ptz.status()["external_yolo_enabled"].as_bool().unwrap());
        ptz.stop();
        assert!(ensure_intent(&ptz.inner, owner).is_err());
        let runtime = ptz.inner.runtime.lock().unwrap();
        assert_eq!(runtime.pause_count, 2);
        assert_eq!(runtime.commands_sent, 9);
    }
    #[test]
    fn gate_requires_exact_binding_and_both_off_confirmations() {
        let (c, r) = evidence();
        assert!(Gate::parse(&c, &r, "stable", vec![]).is_ok());
        for field in [
            "camera_device_id",
            "server_port",
            "device_index",
            "expected_name",
            "gimbal_profile",
            "capture_width",
            "capture_height",
        ] {
            let mut wrong = r.clone();
            wrong["binding"][field] = Value::Null;
            assert!(
                Gate::parse(&c, &wrong, "stable", vec![]).is_err(),
                "{field}"
            );
        }
        for field in ["vendor_ai_tracking_master_off", "vendor_ai_mode_master_off"] {
            let mut wrong = c.clone();
            wrong["operator_confirmation"][field] = json!(false);
            assert!(Gate::parse(&wrong, &r, "stable", vec![]).is_err());
        }
        assert!(Gate::parse(&c, &r, "other", vec![]).is_err());
        for sign in [-2, 0, 2] {
            let mut wrong = r.clone();
            wrong["calibration"]["pan_sign"] = json!(sign);
            assert!(Gate::parse(&c, &wrong, "stable", vec![]).is_err());
        }
    }
    #[test]
    fn fresh_original_numeric_coordinates_required() {
        let g = gate();
        assert!(CameraTarget::parse(&camera(), &g).is_ok());
        for (field, value) in [
            ("frame_age_ms", json!(251)),
            ("frame_age_ms", json!(-1)),
            ("frame_age_ms", json!("0")),
            ("association_ambiguous", json!(true)),
            ("selected_status", json!("lost")),
            ("width", json!(640)),
            ("target_generation", Value::Null),
            ("selection_generation", Value::Null),
        ] {
            let mut c = camera();
            c[field] = value;
            assert!(CameraTarget::parse(&c, &g).is_err(), "{field}");
        }
        let mut invalid = camera();
        invalid["tracks"][0]["box"] = json!([0.8, 0.3, 0.6, 0.5]);
        assert!(CameraTarget::parse(&invalid, &g).is_err());
    }
    #[test]
    fn aba_and_capture_reconnect_cannot_reuse_arm() {
        let g = gate();
        let a = CameraTarget::parse(&camera(), &g).unwrap();
        for (field, value) in [
            ("target_generation", json!(9)),
            ("selection_generation", json!(13)),
            ("camera_session_id", json!("capture2")),
        ] {
            let mut c = camera();
            c[field] = value;
            assert!(!a.same_selection(&CameraTarget::parse(&c, &g).unwrap()));
        }
    }
    #[tokio::test]
    async fn queued_arm_cannot_adopt_token_after_stop_and_close_is_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let p = Arc::new(NativePTZ::new(dir.path()));
        let ticket = p.intent_generation();
        let lock = p.inner.lifecycle.lock().await;
        let other = p.clone();
        let task = tokio::spawn(async move { other.enable_external_for_intent(ticket).await });
        tokio::task::yield_now().await;
        p.stop();
        drop(lock);
        assert!(task.await.unwrap().unwrap_err().contains("过期"));
        p.close();
        assert!(p.enable_external().await.unwrap_err().contains("关闭"));
        assert_eq!(p.stop_and_wait().await["hardware_stop_command_sent"], false);
    }
    #[test]
    fn partial_zero_receipt_is_not_a_complete_stop() {
        let receipt = StopReceipt {
            device_index: 0,
            zero_commands_sent: 3,
            commands_attempted: 4,
            all_zero_commands_sent: false,
            command_results: vec![],
            total_elapsed_ms: 200,
            physical_stop_verified: false,
            limitation: "test",
        };
        let v = stop_value(&receipt);
        assert_eq!(v["all_zero_commands_sent"], false);
        assert_eq!(v["zero_commands_sent"], 3);
        assert_eq!(v["commands_attempted"], 4);
        assert_eq!(v["physical_stop_verified"], false);
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = Runtime::default();
        assert!(apply_stop_receipt(dir.path(), &mut runtime, Some(v)));
        assert!(runtime.stop_fault);
        let saved = read_json(dir.path().join("runtime/stop-fault.json")).unwrap();
        assert_eq!(saved["fault_code"], 1);
        assert_eq!(saved["zero_commands_sent"], 3);
        assert!(saved.as_object().unwrap().values().all(Value::is_number));
    }
    #[test]
    fn historical_commands_after_complete_pause_stop_do_not_fault_a_new_arming_miss() {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = Runtime {
            commands_sent: 9,
            nonzero_since_zero: true,
            ..Default::default()
        };
        let complete = json!({"all_zero_commands_sent":true,"zero_commands_sent":4,"commands_attempted":4,"physical_stop_verified":false});
        apply_pause_stop(directory.path(), &mut runtime, Some(complete.clone())).unwrap();
        assert!(!runtime.nonzero_since_zero);
        // Reverification has no session yet and has accepted no new target.
        apply_pause_stop(directory.path(), &mut runtime, None).unwrap();
        assert!(!runtime.stop_fault);
        assert!(!persistent_stop_fault(directory.path()));
        assert_eq!(runtime.commands_sent, 9, "history must remain intact");
        assert_eq!(runtime.zero, Some(complete));
    }
    #[test]
    fn outstanding_nonzero_without_writer_or_with_partial_zero_still_latches_fault() {
        let missing = tempfile::tempdir().unwrap();
        let mut runtime = Runtime {
            commands_sent: 10,
            nonzero_since_zero: true,
            ..Default::default()
        };
        assert!(apply_pause_stop(missing.path(), &mut runtime, None).is_err());
        assert!(runtime.stop_fault && runtime.nonzero_since_zero);
        assert!(persistent_stop_fault(missing.path()));
        let partial = tempfile::tempdir().unwrap();
        let mut runtime = Runtime {
            commands_sent: 10,
            nonzero_since_zero: true,
            ..Default::default()
        };
        assert!(apply_pause_stop(partial.path(),&mut runtime,Some(json!({"all_zero_commands_sent":false,"zero_commands_sent":3,"commands_attempted":4}))).is_err());
        assert!(runtime.stop_fault && runtime.nonzero_since_zero);
        assert!(persistent_stop_fault(partial.path()));
        assert_eq!(runtime.commands_sent, 10);
    }
    #[test]
    fn complete_zero_receipt_does_not_latch_fault_and_old_fault_is_never_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = Runtime::default();
        let complete = json!({"all_zero_commands_sent":true,"zero_commands_sent":4,"commands_attempted":4,"physical_stop_verified":false});
        assert!(!apply_stop_receipt(
            dir.path(),
            &mut runtime,
            Some(complete.clone())
        ));
        assert!(!runtime.stop_fault);
        assert!(!persistent_stop_fault(dir.path()));
        latch_stop_fault(dir.path(), &mut runtime, 3);
        assert!(!apply_stop_receipt(
            dir.path(),
            &mut runtime,
            Some(complete)
        ));
        assert!(runtime.stop_fault);
        assert!(persistent_stop_fault(dir.path()));
        let restarted = NativePTZ::new(dir.path());
        assert_eq!(restarted.status()["stop_fault"], true);
        assert_eq!(restarted.status()["ready_for_follow"], false);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_noncooperative_owner_retains_handle_and_persistent_fault() {
        use std::sync::Condvar;
        let dir = tempfile::tempdir().unwrap();
        let p = NativePTZ::new(dir.path());
        let barrier = Arc::new((Mutex::new(false), Condvar::new()));
        let blocked = barrier.clone();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = started.send(());
            // Deliberately noncooperative fixture: abort cannot finish this poll
            // until the test releases it. It has no sockets/devices or cleanup.
            {
                let (lock, wake) = &*blocked;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            std::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        *p.inner.task.lock().unwrap() = Some(task);
        {
            let mut r = p.inner.runtime.lock().unwrap();
            r.active = true;
            r.owner = Some(0);
        }
        let result = p.stop_with_budget(Duration::from_millis(15)).await;
        let retained = p
            .inner
            .task
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|task| !task.is_finished());
        // Release before assertions so a failed check cannot strand a worker.
        {
            let (lock, wake) = &*barrier;
            *lock.lock().unwrap() = true;
            wake.notify_all();
        }
        assert!(
            retained,
            "cancelled owner handle was dropped before completion"
        );
        assert_eq!(result["stop_fault"], true);
        assert_eq!(result["ready_for_follow"], false);
        assert_eq!(
            read_json(dir.path().join("runtime/stop-fault.json")).unwrap()["fault_code"],
            2
        );
        assert!(p.enable_external().await.unwrap_err().contains("锁定"));
        for _ in 0..20 {
            if p.inner.task.lock().unwrap().as_ref().unwrap().is_finished() {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
        assert!(p.inner.task.lock().unwrap().as_ref().unwrap().is_finished());
        let restarted = NativePTZ::new(dir.path());
        assert!(
            restarted
                .enable_external()
                .await
                .unwrap_err()
                .contains("锁定")
        );
        p.stop();
        assert_eq!(p.status()["stop_fault"], true);
    }
    #[test]
    fn abnormal_cleanup_latches_fault_without_runtime_recovery_api() {
        let dir = tempfile::tempdir().unwrap();
        let p = NativePTZ::new(dir.path());
        p.inner.runtime.lock().unwrap().owner = Some(4);
        drop(ActorCleanup {
            inner: p.inner.clone(),
            owner: 4,
            completed: false,
        });
        assert_eq!(p.status()["stop_fault"], true);
        assert_eq!(
            read_json(dir.path().join("runtime/stop-fault.json")).unwrap()["fault_code"],
            3
        );
    }
    #[test]
    fn settle_never_stacks_or_accepts_cross_axis_drift() {
        let p = |yaw, pitch| crate::obsbot_motion::Pose {
            yaw_degrees: yaw,
            pitch_degrees: pitch,
            roll_unused_degrees: 0,
        };
        let start = p(17, -24);
        assert!(!crate::settling::assess(start, 18., -24., start, start).arrival);
        assert!(crate::settling::assess(start, 18., -24., p(18, -24), p(18, -24)).arrival);
        assert!(!crate::settling::assess(start, 18., -24., p(21, -24), p(21, -24)).safe);
        assert!(!crate::settling::assess(start, 18., -24., p(18, -22), p(18, -22)).safe);
    }
    #[test]
    fn old_failure_remains_history_not_direction_authority() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("runtime")).unwrap();
        fs::write(
            dir.path().join("runtime/ptz_motion_probe.json"),
            r#"{"result":"FAIL"}"#,
        )
        .unwrap();
        let s = status(dir.path());
        assert_eq!(s["historical_uvc_calibration"]["result"], "FAIL");
        assert_eq!(s["ready_for_follow"], false);
        assert_eq!(s["low_amplitude_calibration"], "FAIL");
        let p = NativePTZ::new(dir.path());
        assert!(p.enable_manual().is_err());
        assert!(p.set_mode("vendor_auto").is_err());
    }
}

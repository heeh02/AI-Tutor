//! Native Rust camera capture + face-only ONNX inference + real ByteTrack.
//! The only spawned program is this same executable in a restricted camera role.
#[path = "vision_compare.rs"]
pub mod comparison;
#[cfg(test)]
#[path = "vision_test_worker.rs"]
mod test_worker;
use base64::{Engine, engine::general_purpose::STANDARD};
use edgefirst_tracker::{ByteTrackBuilder, DetectionBox, TrackInfo, Tracker};
use image::{
    ImageFormat, ImageReader, Limits, RgbImage, codecs::jpeg::JpegEncoder, imageops::FilterType,
};
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{
        ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
        Resolution,
    },
};
use ort::{session::Session, value::Tensor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MODEL_SHA: &str = "8d0bfb0c3383c5bd7a78dd24ef79a21e2aa456619b6ab5e53867092d1c7dc414";
const ORT_DLL_SHA: &str = "2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492";
const MAX_META: usize = 32 * 1024;
const MAX_JPEG: usize = 5 * 1024 * 1024;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn command(root: &Path) -> Result<Command, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(test)]
    let executable = test_worker::resolve(root, executable)?;
    #[cfg(not(test))]
    let _ = root;
    let mut cmd = Command::new(executable);
    // The helper must not acquire a device before the parent has attached its job.
    cmd.env("AI_TUTOR_CAMERA_JOB_GATE", "1")
        .stdin(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    Ok(cmd)
}
fn read_launch_gate(reader: &mut impl Read) -> Result<(), String> {
    let mut marker = [0u8; 4];
    reader
        .read_exact(&mut marker)
        .map_err(|_| "Parent closed before camera helper job assignment".to_string())?;
    if marker != *b"RUN\n" {
        return Err("Invalid camera helper launch marker".into());
    }
    Ok(())
}
fn await_parent_job() -> Result<(), String> {
    if std::env::var_os("AI_TUTOR_CAMERA_JOB_GATE").as_deref() == Some(std::ffi::OsStr::new("1")) {
        read_launch_gate(&mut std::io::stdin().lock())?;
    }
    Ok(())
}
fn release_launch_gate(child: &mut Child) -> Result<(), String> {
    let mut input = child
        .stdin
        .take()
        .ok_or("Missing camera helper launch pipe")?;
    input
        .write_all(b"RUN\n")
        .and_then(|_| input.flush())
        .map_err(|e| e.to_string())
}

/// The handle is non-inheritable and owned only by the parent. Windows closes it
/// even if this process aborts, terminating its helpers independently of frame I/O.
#[cfg(windows)]
struct ProcessJob {
    handle: isize,
}
#[cfg(windows)]
impl ProcessJob {
    fn attach(child: &Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(format!(
                "无法建立相机子进程回收保障：{}",
                std::io::Error::last_os_error()
            ));
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            let error = std::io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(format!("相机子进程回收保障配置失败：{error}"));
        }
        if unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(format!("无法隔离相机专用进程，已拒绝运行：{error}"));
        }
        Ok(Self {
            handle: handle as isize,
        })
    }
}
#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle as _) };
    }
}
#[cfg(not(windows))]
struct ProcessJob;
#[cfg(not(windows))]
impl ProcessJob {
    fn attach(_child: &Child) -> Result<Self, String> {
        Ok(Self)
    }
}

/// Intended for the executable's --camera-discover dispatch. Never opens a stream.
pub fn discovery_worker() -> Result<Vec<Value>, String> {
    await_parent_job()?;
    let devices = nokhwa::query(ApiBackend::MediaFoundation).map_err(|e| e.to_string())?;
    Ok(devices.into_iter().take(16).map(|d| {
        let name=d.human_name().to_owned(); let id=d.misc().to_owned();
        let virtual_camera=["virtual","manycam","obs virtual"].iter().any(|v|name.to_lowercase().contains(v));
        json!({"id":id,"device_id":id,"name":name,"stable_id":true,"virtual":virtual_camera,
               "backend":"Windows MediaFoundation / nokhwa","formats":"native formats negotiated on open"})
    }).collect())
}

#[derive(Clone)]
pub struct FrameSnapshot {
    pub jpeg: Arc<Vec<u8>>,
    pub meta: Value,
    received: Instant,
}
const TRACK_AMBIGUITY_SCOPE: &str = "per_track_v1";

/// New workers identify only the tracks involved in overlap. Older worker
/// packets retain their conservative global veto; an unknown/malformed scope
/// must never silently turn that veto into permission to follow.
fn association_is_ambiguous(meta: &Value, key: Option<&str>) -> bool {
    match meta.get("association_ambiguity_scope") {
        None => meta["association_ambiguous"] == true,
        Some(scope) if scope.as_str() == Some(TRACK_AMBIGUITY_SCOPE) => {
            let Some(keys) = meta["ambiguous_track_keys"].as_array() else {
                return true;
            };
            if keys
                .iter()
                .any(|value| value.as_str().is_none_or(str::is_empty))
            {
                return true;
            }
            key.is_some_and(|key| keys.iter().any(|value| value.as_str() == Some(key)))
        }
        Some(_) => true,
    }
}
struct State {
    phase: String,
    error: Option<String>,
    diagnostics: Vec<Value>,
    latest: Option<FrameSnapshot>,
    selected: Option<String>,
    selected_seen: Instant,
    // Independent of the capture-process generation and camera_session_id.
    target_generation: u64,
    // Identity/lifecycle changes only. Short visibility misses retain this
    // generation while still invalidating proposals through target_generation.
    selection_generation: u64,
    target_visible: bool,
    last_frame_stamp: Option<(String, u64)>,
    last_ambiguous: bool,
}
impl State {
    fn new() -> Self {
        Self {
            phase: "closed".into(),
            error: None,
            diagnostics: Vec::new(),
            latest: None,
            selected: None,
            selected_seen: Instant::now(),
            target_generation: 0,
            selection_generation: 0,
            target_visible: false,
            last_frame_stamp: None,
            last_ambiguous: false,
        }
    }
    fn bump_target(&mut self) {
        self.target_generation = self.target_generation.wrapping_add(1);
    }
    fn bump_selection(&mut self) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
    }
    fn clear_selection(&mut self) {
        if self.selected.is_some() || self.target_visible {
            self.bump_target();
        }
        if self.selected.is_some() {
            self.bump_selection();
        }
        self.selected = None;
        self.target_visible = false;
    }
    fn reset_target_lifecycle(&mut self) {
        self.bump_target();
        self.bump_selection();
        self.selected = None;
        self.target_visible = false;
        self.last_frame_stamp = None;
        self.last_ambiguous = false;
    }
    fn select_key(&mut self, key: &str, at: Instant, visible: bool) {
        if self.selected.as_deref() != Some(key) {
            self.bump_selection();
        }
        if self.selected.as_deref() != Some(key) || !self.target_visible {
            self.bump_target();
        }
        self.selected = Some(key.to_owned());
        self.selected_seen = Self::observation_time(at, self.frame_age_ms(at).unwrap_or(0.0));
        self.target_visible = visible;
    }
    fn observation_time(at: Instant, source_age_ms: f64) -> Instant {
        if source_age_ms.is_finite() && (0.0..=1000.0).contains(&source_age_ms) {
            at.checked_sub(Duration::from_secs_f64(source_age_ms / 1000.0))
                .unwrap_or(at)
        } else {
            at
        }
    }
    fn selected_last_seen_age_ms(&self, at: Instant) -> Option<f64> {
        self.selected.as_ref().map(|_| {
            at.saturating_duration_since(self.selected_seen)
                .as_secs_f64()
                * 1000.0
        })
    }
    fn frame_age_ms(&self, at: Instant) -> Option<f64> {
        self.latest.as_ref().map(|frame| {
            frame.meta["frame_age_ms"].as_f64().unwrap_or(f64::INFINITY)
                + at.saturating_duration_since(frame.received).as_secs_f64() * 1000.0
        })
    }
    fn expire_selection(&mut self, at: Instant) {
        let Some(age) = self.frame_age_ms(at) else {
            return;
        };
        let mut changed = false;
        if age >= 400.0 && self.target_visible {
            self.target_visible = false;
            changed = true;
        }
        if self.selected.is_some()
            && (age > 600.0
                || self
                    .selected_last_seen_age_ms(at)
                    .is_some_and(|last_seen| last_seen > 600.0))
        {
            self.bump_selection();
            self.selected = None;
            self.target_visible = false;
            changed = true;
        }
        // Age expiration changes a state once, not once per status/UI poll.
        if changed {
            self.bump_target();
        }
    }
    fn latest_meta(&mut self, at: Instant) -> Option<Value> {
        self.expire_selection(at);
        // Status and target checks need no pixels. Keep their cost independent
        // of JPEG size while preserving the same age/selection transitions.
        let mut meta = self.latest.as_ref()?.meta.clone();
        if meta.get("association_ambiguity_scope").is_some() {
            let ambiguous = association_is_ambiguous(&meta, self.selected.as_deref());
            meta["association_ambiguous"] = json!(ambiguous);
        }
        meta["frame_age_ms"] = json!(self.frame_age_ms(at)?.round());
        meta["selected_track_key"] = json!(self.selected);
        meta["selected_status"] = json!(if self.selected.is_none() {
            "unselected"
        } else if self.target_visible {
            "visible"
        } else {
            "temporarily_lost"
        });
        meta["target_generation"] = json!(self.target_generation);
        meta["selection_generation"] = json!(self.selection_generation);
        meta["selected_last_seen_age_ms"] = json!(self.selected_last_seen_age_ms(at));
        Some(meta)
    }
    fn latest_snapshot(&mut self, at: Instant) -> Option<FrameSnapshot> {
        let meta = self.latest_meta(at)?;
        let frame = self.latest.as_ref()?;
        Some(FrameSnapshot {
            meta,
            jpeg: frame.jpeg.clone(),
            received: frame.received,
        })
    }
    fn accept_frame(&mut self, meta: Value, jpeg: Vec<u8>, at: Instant) -> bool {
        let Some(frame_id) = meta["frame_id"].as_u64() else {
            return false;
        };
        let Some(epoch) = meta["camera_session_id"]
            .as_str()
            .filter(|epoch| !epoch.is_empty())
            .map(str::to_owned)
        else {
            return false;
        };
        if self
            .last_frame_stamp
            .as_ref()
            .is_some_and(|(previous_epoch, previous_id)| {
                previous_epoch == &epoch && frame_id <= *previous_id
            })
        {
            return false;
        }
        self.expire_selection(at);
        let epoch_changed = self
            .last_frame_stamp
            .as_ref()
            .is_none_or(|(previous_epoch, _)| previous_epoch != &epoch);
        let ambiguous = association_is_ambiguous(&meta, self.selected.as_deref());
        let old_selected = self.selected.clone();
        let old_visible = self.target_visible;
        if epoch_changed || ambiguous {
            self.selected = None;
            self.target_visible = false;
        } else if let Some(selected) = self.selected.as_deref() {
            let present = meta["tracks"].as_array().is_some_and(|tracks| {
                tracks
                    .iter()
                    .any(|track| track["track_key"].as_str() == Some(selected))
            });
            let source_age = meta["frame_age_ms"].as_f64().unwrap_or(f64::INFINITY);
            self.target_visible = present && source_age < 400.0;
            if self.target_visible {
                self.selected_seen = Self::observation_time(at, source_age);
            } else if source_age > 600.0
                || at.saturating_duration_since(self.selected_seen) > Duration::from_millis(600)
            {
                self.selected = None;
            }
        } else {
            self.target_visible = false;
        }
        if epoch_changed || old_selected != self.selected {
            self.bump_selection();
        }
        if epoch_changed
            || old_selected != self.selected
            || (old_visible && !self.target_visible)
            || (ambiguous && !self.last_ambiguous)
        {
            self.bump_target();
        }
        self.last_frame_stamp = Some((epoch, frame_id));
        self.last_ambiguous = ambiguous;
        self.phase = "running".into();
        self.error = None;
        self.latest = Some(FrameSnapshot {
            jpeg: Arc::new(jpeg),
            meta,
            received: at,
        });
        true
    }
}
struct Control {
    stop: AtomicBool,
    child: Mutex<Option<Child>>,
    last_packet: Mutex<Instant>,
    #[cfg(all(test, windows))]
    diagnostic: test_worker::OwnedReceipt,
}
pub struct CameraService {
    root: PathBuf,
    state: Arc<Mutex<State>>,
    generation: Arc<AtomicU64>,
    owner: Mutex<Option<Arc<Control>>>,
    preview_slots: Arc<tokio::sync::Semaphore>,
    // A coalesced wake-up only. Pixels and observation timestamps remain in
    // the single authoritative snapshot; subscribers never queue frames.
    preview_changed: tokio::sync::watch::Sender<()>,
}

impl CameraService {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state: Arc::new(Mutex::new(State::new())),
            generation: Arc::new(AtomicU64::new(0)),
            owner: Mutex::new(None),
            preview_slots: Arc::new(tokio::sync::Semaphore::new(
                crate::video_stream::MAX_CLIENTS,
            )),
            preview_changed: tokio::sync::watch::channel(()).0,
        }
    }
    pub fn discover(&self) -> Result<Vec<Value>, String> {
        let mut child = command(&self.root)?
            .arg("--camera-discover")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let _job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if let Err(error) = release_launch_gate(&mut child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let out_reader = thread::spawn(move || {
            let mut out = Vec::new();
            stdout
                .take((MAX_META * 2) as u64)
                .read_to_end(&mut out)
                .map(|_| out)
        });
        let err_reader = thread::spawn(move || {
            let mut out = Vec::new();
            stderr.take(4096).read_to_end(&mut out).map(|_| out)
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) => break status,
                None if start.elapsed() > Duration::from_secs(8) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Native camera discovery timed out".into());
                }
                None => thread::sleep(Duration::from_millis(25)),
            }
        };
        let out = out_reader
            .join()
            .map_err(|_| "Camera enumeration reader failed")?
            .map_err(|e| e.to_string())?;
        let err = err_reader
            .join()
            .map_err(|_| "Camera diagnostics reader failed")?
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(String::from_utf8_lossy(&err).chars().take(800).collect());
        }
        serde_json::from_slice(&out).map_err(|e| format!("Invalid native discovery result: {e}"))
    }
    pub fn open(&self, device_id: Option<String>) -> Result<Value, String> {
        self.close();
        let devices = self.discover()?;
        if devices.is_empty() {
            return Err("未发现摄像头，请连接设备并检查 Windows 相机权限".into());
        }
        let saved = fs::read_to_string(self.root.join("runtime/rust_camera_device.txt")).ok();
        let chosen = if let Some(id) = device_id {
            devices
                .iter()
                .find(|d| d["id"].as_str() == Some(id.as_str()))
        } else {
            devices
                .iter()
                .find(|d| d["id"].as_str() == saved.as_deref())
                .or_else(|| {
                    devices.iter().find(|d| {
                        d["name"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("obsbot")
                            && d["virtual"] == false
                    })
                })
                .or_else(|| devices.iter().find(|d| d["virtual"] == false))
                .or_else(|| devices.first())
        }
        .ok_or("所选摄像头已断开，请刷新设备列表")?;
        let id = chosen["id"]
            .as_str()
            .ok_or("Missing native camera identifier")?
            .to_owned();
        fs::create_dir_all(self.root.join("runtime")).map_err(|e| e.to_string())?;
        fs::write(self.root.join("runtime/rust_camera_device.txt"), &id)
            .map_err(|e| e.to_string())?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let control = Arc::new(Control {
            stop: AtomicBool::new(false),
            child: Mutex::new(None),
            last_packet: Mutex::new(Instant::now()),
            #[cfg(all(test, windows))]
            diagnostic: test_worker::OwnedReceipt::default(),
        });
        *self.owner.lock().unwrap() = Some(control.clone());
        {
            let mut s = self.state.lock().unwrap();
            s.phase = "connecting".into();
            s.error = None;
            s.latest = None;
            s.reset_target_lifecycle();
        }
        let state = self.state.clone();
        let current = self.generation.clone();
        let root = self.root.clone();
        self.preview_changed.send_replace(());
        let preview_changed = self.preview_changed.clone();
        thread::spawn(move || {
            supervise(
                root,
                id,
                generation,
                current,
                state,
                control,
                preview_changed,
            )
        });
        Ok(self.status())
    }
    pub fn close(&self) -> Value {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let response = {
            let mut s = self.state.lock().unwrap();
            s.phase = "closed".into();
            s.error = None;
            s.latest = None;
            s.reset_target_lifecycle();
            json!({"state":"closed","selected_track_key":null,"selected_status":"unselected","target_generation":s.target_generation,"selection_generation":s.selection_generation,"selected_last_seen_age_ms":null})
        };
        self.preview_changed.send_replace(());
        if let Some(control) = self.owner.lock().unwrap().take() {
            control.stop.store(true, Ordering::SeqCst);
            if let Some(mut child) = control.child.lock().unwrap().take() {
                let _ = child.kill();
                let _waited = child.wait();
                #[cfg(all(test, windows))]
                control.diagnostic.waited(_waited.is_ok());
            }
        }
        response
    }
    pub fn latest(&self) -> Option<FrameSnapshot> {
        self.state.lock().unwrap().latest_snapshot(Instant::now())
    }
    pub fn target_generation(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.expire_selection(Instant::now());
        state.target_generation
    }
    pub fn selection_generation(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.expire_selection(Instant::now());
        state.selection_generation
    }
    pub fn frame(&self) -> Value {
        let (mut meta, jpeg) = self.preview_snapshot();
        // The legacy HTTP fallback encodes after releasing the camera mutex.
        meta["jpeg"] = jpeg.map_or(Value::Null, |jpeg| json!(STANDARD.encode(jpeg.as_slice())));
        meta
    }
    pub(crate) fn preview_slot(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
        self.preview_slots.clone().try_acquire_owned()
    }
    pub(crate) fn subscribe_preview(&self) -> tokio::sync::watch::Receiver<()> {
        self.preview_changed.subscribe()
    }
    pub(crate) fn preview_snapshot(&self) -> (Value, Option<Arc<Vec<u8>>>) {
        // Image, phase, age and overlays must come from one coherent snapshot.
        // Diagnostic history remains available through status(), never as a
        // frozen image that could be mistaken for a working camera.
        let mut state = self.state.lock().unwrap();
        let at = Instant::now();
        let mut meta = state.latest_meta(at).unwrap_or_else(|| json!({}));
        let live = state.phase == "running"
            && meta["frame_age_ms"]
                .as_f64()
                .is_some_and(|age| age.is_finite() && (0.0..=1000.0).contains(&age));
        meta["state"] = json!(state.phase);
        meta["error"] = json!(state.error);
        meta["target_generation"] = json!(state.target_generation);
        meta["selection_generation"] = json!(state.selection_generation);
        meta["selected_last_seen_age_ms"] = json!(state.selected_last_seen_age_ms(at));
        meta["preview_available"] = json!(live);
        meta["preview_selectable"] =
            json!(live && meta["frame_age_ms"].as_f64().is_some_and(|age| age < 400.0));
        let jpeg = live.then(|| state.latest.as_ref().unwrap().jpeg.clone());
        if !live {
            meta["tracks"] = json!([]);
            meta["selected_track_key"] = Value::Null;
            meta["selected_status"] = json!("unavailable");
        }
        meta["pose"] = crate::camera_pose::display_pose(&meta, now_ms());
        (meta, jpeg)
    }
    #[cfg(test)]
    pub(crate) fn inject_preview_fixture(&self, meta: Value, jpeg: Vec<u8>) -> bool {
        let accepted = self
            .state
            .lock()
            .unwrap()
            .accept_frame(meta, jpeg, Instant::now());
        if accepted {
            self.preview_changed.send_replace(());
        }
        accepted
    }
    pub fn status(&self) -> Value {
        let mut s = self.state.lock().unwrap();
        let mut meta = s
            .latest_meta(Instant::now())
            .unwrap_or_else(|| json!({"selected_track_key":null,"selected_status":"unselected"}));
        meta["target_generation"] = json!(s.target_generation);
        meta["selection_generation"] = json!(s.selection_generation);
        meta["selected_last_seen_age_ms"] = json!(s.selected_last_seen_age_ms(Instant::now()));
        meta["state"] = json!(s.phase);
        meta["error"] = json!(s.error);
        meta["diagnostics"] = json!(s.diagnostics);
        meta["tracker"] = json!("ByteTrack / edgefirst-tracker 0.38.2 (Kalman + two-pass LAPJV)");
        meta["face_model"] = json!("lindevs/yolov8n-face ONNX / ort CPU");
        meta["runtime"] = json!("Rust native / MediaFoundation");
        meta["pose"] = crate::camera_pose::display_pose(&meta, now_ms());
        meta
    }
    pub fn native_pose_snapshot(&self) -> Result<Value, String> {
        crate::camera_pose::snapshot(&self.status(), now_ms())
    }
    pub async fn next_native_pose(&self) -> Result<Value, String> {
        // A cached sample can age out between capture and detection. Wait for a
        // fresh successor instead of treating that scheduling gap as device loss.
        let requested_at_ms = now_ms();
        let initial = self.status();
        if initial["state"] != "running" {
            return Err("camera capture owner is not running".into());
        }
        let seq = initial["native_pose"]["sample_seq"].as_u64().unwrap_or(0);
        let epoch = initial["camera_session_id"].clone();
        if epoch.as_str().is_none_or(str::is_empty) {
            return Err("native pose epoch missing".into());
        }
        let start = Instant::now();
        let mut last_error = String::from("no new sample");
        while start.elapsed() < Duration::from_millis(500) {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let meta = self.status();
            if meta["state"] != "running" {
                return Err("camera capture owner stopped while awaiting pose".into());
            }
            if meta["camera_session_id"] != epoch {
                return Err("native pose epoch changed".into());
            }
            let mut sample = match crate::camera_pose::snapshot(&meta, now_ms()) {
                Ok(sample) => sample,
                Err(error) => {
                    last_error = error;
                    continue;
                }
            };
            if sample["sample_seq"]
                .as_u64()
                .is_some_and(|value| value > seq)
                && sample["sampled_at_ms"]
                    .as_u64()
                    .is_some_and(|at| at >= requested_at_ms)
            {
                sample["after_sample_seq"] = json!(seq);
                sample["initial_camera_session_id"] = epoch;
                return Ok(sample);
            }
        }
        Err(format!(
            "no new capture-owner native sample within 500ms: {last_error}"
        ))
    }
    pub fn select(&self, key: &str) -> Result<Value, String> {
        let at = Instant::now();
        let mut state = self.state.lock().unwrap();
        let meta = state.latest_meta(at).ok_or("尚无实时画面")?;
        if meta["frame_age_ms"].as_f64().unwrap_or(10000.0) > 500.0
            || !meta["tracks"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|t| t["track_key"].as_str() == Some(key)))
        {
            return Err("目标已消失或画面过期，请重新选择".into());
        }
        if association_is_ambiguous(&meta, Some(key)) {
            return Err("目标相互遮挡，关联不确定；请分开后重新选择".into());
        }
        state.select_key(
            key,
            at,
            meta["frame_age_ms"].as_f64().unwrap_or(f64::INFINITY) < 400.0,
        );
        drop(state);
        self.preview_changed.send_replace(());
        Ok(self.status())
    }
    pub fn clear(&self) -> Value {
        self.state.lock().unwrap().clear_selection();
        self.preview_changed.send_replace(());
        self.status()
    }
}
impl Drop for CameraService {
    fn drop(&mut self) {
        self.close();
    }
}

fn state_error(state: &Arc<Mutex<State>>, current: &AtomicU64, generation: u64, error: String) {
    if current.load(Ordering::SeqCst) == generation {
        let mut s = state.lock().unwrap();
        if current.load(Ordering::SeqCst) != generation {
            return;
        }
        let entering_reconnect = s.phase != "reconnecting";
        let prior_selection_generation = s.selection_generation;
        s.phase = "reconnecting".into();
        s.diagnostics.push(json!({"at":chrono::Utc::now().to_rfc3339(),"error":error.chars().take(600).collect::<String>()}));
        if s.diagnostics.len() > 8 {
            s.diagnostics.remove(0);
        }
        s.error = Some(error);
        s.clear_selection();
        if entering_reconnect && s.selection_generation == prior_selection_generation {
            s.bump_selection();
        }
    }
}

#[cfg(test)]
mod target_generation_tests {
    use super::*;

    fn meta(epoch: &str, frame_id: u64, keys: &[&str], ambiguous: bool) -> Value {
        let tracks: Vec<Value> = keys
            .iter()
            .map(|key| json!({"track_key":key,"box":[0.1,0.1,0.3,0.3]}))
            .collect();
        json!({"camera_session_id":epoch,"frame_id":frame_id,"frame_age_ms":0,"tracks":tracks,"association_ambiguous":ambiguous})
    }
    fn selected() -> (State, Instant) {
        let at = Instant::now();
        let mut state = State::new();
        assert!(state.accept_frame(meta("epoch-A", 1, &["A", "B"], false), vec![], at));
        state.select_key("A", at + Duration::from_millis(1), true);
        (state, at)
    }
    #[test]
    fn preview_wakes_coalesce_and_rejected_observations_do_not_publish() {
        let camera = CameraService::new(PathBuf::from("."));
        let mut changes = camera.subscribe_preview();
        assert!(!changes.has_changed().unwrap());
        for frame in 1..=3 {
            assert!(camera.inject_preview_fixture(meta("epoch-A", frame, &[], false), vec![1]));
        }
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        assert_eq!(camera.preview_snapshot().0["frame_id"], 3);
        assert!(!changes.has_changed().unwrap());
        assert!(!camera.inject_preview_fixture(meta("epoch-A", 2, &[], false), vec![2]));
        assert!(!changes.has_changed().unwrap());
        assert_eq!(camera.preview_snapshot().0["frame_id"], 3);
        camera.close();
        assert!(changes.has_changed().unwrap());
        assert!(camera.preview_snapshot().1.is_none());
    }
    #[test]
    fn display_pose_is_shared_by_status_and_preview_without_arming_or_opening() {
        let dir = tempfile::tempdir().unwrap();
        let camera = CameraService::new(dir.path().to_owned());
        assert_eq!(camera.status()["pose"]["pose_source"], "unknown");
        assert_eq!(camera.status()["pose"]["active"], false);
        let mut frame = meta("pose-epoch", 1, &[], false);
        let axis = |value| json!({"value":value,"minimum":-90,"maximum":90,
            "step":1,"manual":true,"query_success":true});
        frame["native_pose"] = json!({"schema_version":"1.0","status":"ok",
            "source":"camera_capture_owner","camera_session_id":"pose-epoch",
            "sampled_at_ms":now_ms(),"sample_seq":1,"read_only":true,"set_calls":0,
            "additional_device_opens":0,"pan":axis(17),"tilt":axis(24)});
        assert!(camera.inject_preview_fixture(frame, vec![1]));
        let status = camera.status();
        let preview = camera.preview_snapshot().0;
        for value in [&status, &preview] {
            assert_eq!(value["pose"]["pose_source"], "controller_readback");
            assert_eq!(value["pose"]["pan_deg"], 17);
            assert_eq!(value["pose"]["tilt_deg"], 24);
            assert_eq!(value["pose"]["active"], true);
            assert!(value["selected_track_key"].is_null());
        }
        assert_eq!(status["pose"]["revision"], preview["pose"]["revision"]);
        assert!(camera.owner.lock().unwrap().is_none());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
        camera.close();
        for value in [camera.status(), camera.preview_snapshot().0] {
            assert_eq!(value["pose"]["active"], false);
            assert_eq!(value["pose"]["connected"], false);
            assert!(value["pose"]["pan_deg"].is_null());
        }
    }
    #[tokio::test]
    async fn native_pose_waits_for_a_distinct_capture_sample() {
        let dir = tempfile::tempdir().unwrap();
        let camera = Arc::new(CameraService::new(dir.path().to_owned()));
        let sample = |frame_id, seq| {
            let mut value = meta("pose-epoch", frame_id, &[], false);
            value["native_pose"] = json!({"status":"ok","source":"camera_capture_owner","camera_session_id":"pose-epoch","sampled_at_ms":now_ms(),"sample_seq":seq});
            value
        };
        let mut captured_before_request = sample(2, 3);
        captured_before_request["native_pose"]["sampled_at_ms"] = json!(now_ms() - 5);
        camera
            .state
            .lock()
            .unwrap()
            .accept_frame(sample(1, 2), vec![], Instant::now());
        let pending = camera.clone();
        let task = tokio::spawn(async move { pending.next_native_pose().await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !task.is_finished(),
            "same cached sample must not become two observations"
        );
        camera
            .state
            .lock()
            .unwrap()
            .accept_frame(captured_before_request, vec![], Instant::now());
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !task.is_finished(),
            "a delayed detection frame must not authorize a pre-request control sample"
        );
        camera
            .state
            .lock()
            .unwrap()
            .accept_frame(sample(3, 4), vec![], Instant::now());
        let next = task.await.unwrap().unwrap();
        assert_eq!(next["sample_seq"], 4);
        assert_eq!(next["after_sample_seq"], 2);
        assert!(next.get("jpeg").is_none());
    }

    #[tokio::test]
    async fn native_pose_waits_past_a_stale_cached_sample_without_relaxing_freshness() {
        let dir = tempfile::tempdir().unwrap();
        let camera = Arc::new(CameraService::new(dir.path().to_owned()));
        let sample = |frame_id, seq, timestamp| {
            let mut value = meta("pose-epoch", frame_id, &[], false);
            value["native_pose"] = json!({"status":"ok","source":"camera_capture_owner","camera_session_id":"pose-epoch","sampled_at_ms":timestamp,"sample_seq":seq});
            value
        };
        camera.state.lock().unwrap().accept_frame(
            sample(1, 2, now_ms() - 400),
            vec![],
            Instant::now(),
        );
        let pending = camera.clone();
        let task = tokio::spawn(async move { pending.next_native_pose().await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!task.is_finished());
        camera
            .state
            .lock()
            .unwrap()
            .accept_frame(sample(2, 3, now_ms()), vec![], Instant::now());
        let next = task.await.unwrap().unwrap();
        assert_eq!(next["sample_seq"], 3);
        assert!(next["sample_age_ms"].as_u64().unwrap() <= 250);
    }

    #[tokio::test]
    async fn native_pose_rejects_a_camera_epoch_change_while_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let camera = Arc::new(CameraService::new(dir.path().to_owned()));
        camera.state.lock().unwrap().accept_frame(
            meta("old", 1, &[], false),
            vec![],
            Instant::now(),
        );
        let pending = camera.clone();
        let task = tokio::spawn(async move { pending.next_native_pose().await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        camera.state.lock().unwrap().accept_frame(
            meta("new", 1, &[], false),
            vec![],
            Instant::now(),
        );
        assert!(task.await.unwrap().unwrap_err().contains("epoch changed"));
    }

    #[test]
    fn a_b_a_selection_changes_are_detectable_and_same_selection_is_idempotent() {
        let (mut state, at) = selected();
        let initial = state.target_generation;
        let initial_selection = state.selection_generation;
        state.select_key("B", at + Duration::from_millis(2), true);
        state.select_key("A", at + Duration::from_millis(3), true);
        assert_eq!(state.selected.as_deref(), Some("A"));
        assert_eq!(state.target_generation, initial + 2);
        assert_eq!(state.selection_generation, initial_selection + 2);
        state.select_key("A", at + Duration::from_millis(4), true);
        assert_eq!(state.target_generation, initial + 2);
        assert_eq!(state.selection_generation, initial_selection + 2);
        for _ in 0..20 {
            let frame = state
                .latest_snapshot(at + Duration::from_millis(5))
                .unwrap();
            assert_eq!(frame.meta["target_generation"], initial + 2);
            assert_eq!(frame.meta["selection_generation"], initial_selection + 2);
            assert_eq!(frame.meta["selected_track_key"], "A");
        }
    }
    #[test]
    fn a_short_lost_then_visible_interval_leaves_a_persistent_generation_change() {
        let (mut state, at) = selected();
        let before = state.target_generation;
        let selected_generation = state.selection_generation;
        state.accept_frame(
            meta("epoch-A", 2, &["B"], false),
            vec![],
            at + Duration::from_millis(20),
        );
        assert_eq!(state.selected.as_deref(), Some("A"));
        assert!(!state.target_visible);
        assert_eq!(state.target_generation, before + 1);
        assert_eq!(state.selection_generation, selected_generation);
        state.accept_frame(
            meta("epoch-A", 3, &["A", "B"], false),
            vec![],
            at + Duration::from_millis(40),
        );
        let visible = state
            .latest_snapshot(at + Duration::from_millis(41))
            .unwrap();
        assert_eq!(visible.meta["selected_status"], "visible");
        assert_eq!(visible.meta["target_generation"], before + 1);
        assert_eq!(visible.meta["selection_generation"], selected_generation);
        state.accept_frame(
            meta("epoch-A", 4, &["A", "B"], false),
            vec![],
            at + Duration::from_millis(60),
        );
        assert_eq!(state.target_generation, before + 1);
        assert_eq!(state.selection_generation, selected_generation);
    }
    #[test]
    fn selected_seen_age_includes_source_age_and_missing_frames_cannot_extend_it() {
        let at = Instant::now();
        let mut state = State::new();
        let mut source = meta("age-epoch", 1, &["A"], false);
        source["frame_age_ms"] = json!(200.0);
        state.accept_frame(source, vec![], at);
        state.select_key("A", at, true);
        let selected_generation = state.selection_generation;
        let target_generation = state.target_generation;
        let visible = state
            .latest_snapshot(at + Duration::from_millis(30))
            .unwrap();
        assert_eq!(visible.meta["selected_last_seen_age_ms"], 230.0);
        state.accept_frame(
            meta("age-epoch", 2, &[], false),
            vec![],
            at + Duration::from_millis(40),
        );
        let missing = state
            .latest_snapshot(at + Duration::from_millis(100))
            .unwrap();
        assert_eq!(missing.meta["selected_last_seen_age_ms"], 300.0);
        assert_eq!(missing.meta["selection_generation"], selected_generation);
        assert_eq!(missing.meta["target_generation"], target_generation + 1);
        state.accept_frame(
            meta("age-epoch", 3, &[], false),
            vec![],
            at + Duration::from_millis(400),
        );
        assert_eq!(
            state.selected.as_deref(),
            Some("A"),
            "600ms is retained, not extended by a fresh missing frame"
        );
        state.accept_frame(
            meta("age-epoch", 4, &[], false),
            vec![],
            at + Duration::from_millis(401),
        );
        assert!(state.selected.is_none());
        assert_eq!(state.selection_generation, selected_generation + 1);
        assert_eq!(state.target_generation, target_generation + 2);
        assert!(
            state
                .latest_snapshot(at + Duration::from_millis(402))
                .unwrap()
                .meta["selected_last_seen_age_ms"]
                .is_null()
        );
    }

    #[test]
    fn selection_metadata_is_present_in_closed_and_empty_fallback_responses() {
        let camera = CameraService::new(PathBuf::from("."));
        for value in [camera.status(), camera.frame()] {
            assert_eq!(value["selection_generation"], 0);
            assert!(value.get("selected_last_seen_age_ms").unwrap().is_null());
        }
        let closed = camera.close();
        assert_eq!(closed["selection_generation"], 1);
        assert!(closed["selected_last_seen_age_ms"].is_null());
        assert_eq!(camera.selection_generation(), 1);
        assert_eq!(camera.status()["selection_generation"], 1);
        assert_eq!(camera.frame()["selection_generation"], 1);
    }

    #[test]
    fn a_late_visible_frame_cannot_restore_a_selection_expired_behind_fresh_missing_frames() {
        let (mut state, at) = selected();
        let selected_generation = state.selection_generation;
        let target_generation = state.target_generation;
        let mut missing = meta("epoch-A", 2, &["B"], false);
        missing["frame_age_ms"] = json!(150.0);
        state.accept_frame(missing, vec![], at + Duration::from_millis(500));
        assert_eq!(state.selected.as_deref(), Some("A"));
        assert_eq!(state.target_generation, target_generation + 1);
        assert_eq!(state.selection_generation, selected_generation);
        assert_eq!(
            state.frame_age_ms(at + Duration::from_millis(620)),
            Some(270.0)
        );
        assert_eq!(
            state.selected_last_seen_age_ms(at + Duration::from_millis(620)),
            Some(620.0)
        );

        state.accept_frame(
            meta("epoch-A", 3, &["A", "B"], false),
            vec![],
            at + Duration::from_millis(620),
        );
        assert!(
            state.selected.is_none(),
            "late visibility cannot renew an expired selection before it is cleared"
        );
        assert_eq!(state.target_generation, target_generation + 2);
        assert_eq!(state.selection_generation, selected_generation + 1);
        let snapshot = state
            .latest_snapshot(at + Duration::from_millis(621))
            .unwrap();
        assert_eq!(snapshot.meta["selected_status"], "unselected");
        assert!(snapshot.meta["selected_last_seen_age_ms"].is_null());

        state.accept_frame(
            meta("epoch-A", 4, &["A", "B"], false),
            vec![],
            at + Duration::from_millis(640),
        );
        assert!(state.selected.is_none());
        assert_eq!(state.selection_generation, selected_generation + 1);
        assert_eq!(state.target_generation, target_generation + 2);
    }

    #[test]
    fn reconnect_without_a_selection_invalidates_its_lifecycle_once() {
        let state = Arc::new(Mutex::new(State::new()));
        let current = AtomicU64::new(5);
        state_error(&state, &current, 5, "disconnect".into());
        assert_eq!(state.lock().unwrap().selection_generation, 1);
        state_error(&state, &current, 5, "same disconnect".into());
        state_error(&state, &current, 4, "old worker".into());
        assert_eq!(state.lock().unwrap().selection_generation, 1);
        assert_eq!(state.lock().unwrap().target_generation, 0);
    }
    #[test]
    fn repeated_or_old_frames_do_not_repeat_visibility_or_ambiguity_transitions() {
        let (mut state, at) = selected();
        let before = state.target_generation;
        assert!(!state.accept_frame(
            meta("epoch-A", 1, &[], true),
            vec![],
            at + Duration::from_millis(20)
        ));
        assert!(!state.accept_frame(
            meta("epoch-A", 0, &[], true),
            vec![],
            at + Duration::from_millis(21)
        ));
        assert!(state.target_visible);
        assert_eq!(state.target_generation, before);
        assert_eq!(state.latest.as_ref().unwrap().meta["frame_id"], 1);
    }
    #[test]
    fn ambiguity_invalidates_once_per_transition_and_clears_the_binding() {
        let (mut state, at) = selected();
        let before = state.target_generation;
        let selected_generation = state.selection_generation;
        state.accept_frame(
            meta("epoch-A", 2, &["A", "B"], true),
            vec![],
            at + Duration::from_millis(20),
        );
        assert!(state.selected.is_none());
        assert_eq!(state.target_generation, before + 1);
        assert_eq!(state.selection_generation, selected_generation + 1);
        state.accept_frame(
            meta("epoch-A", 3, &["A", "B"], true),
            vec![],
            at + Duration::from_millis(40),
        );
        assert_eq!(state.target_generation, before + 1);
        state.accept_frame(
            meta("epoch-A", 4, &["A", "B"], false),
            vec![],
            at + Duration::from_millis(60),
        );
        state.accept_frame(
            meta("epoch-A", 5, &["A", "B"], true),
            vec![],
            at + Duration::from_millis(80),
        );
        assert_eq!(state.target_generation, before + 2);
        assert_eq!(state.selection_generation, selected_generation + 1);
    }
    #[test]
    fn a_new_camera_epoch_invalidates_even_if_an_opaque_track_key_is_reused() {
        let (mut state, at) = selected();
        let before = state.target_generation;
        let selected_generation = state.selection_generation;
        state.accept_frame(
            meta("epoch-B", 1, &["A"], false),
            vec![],
            at + Duration::from_millis(20),
        );
        let frame = state
            .latest_snapshot(at + Duration::from_millis(21))
            .unwrap();
        assert_eq!(frame.meta["camera_session_id"], "epoch-B");
        assert!(frame.meta["selected_track_key"].is_null());
        assert_eq!(frame.meta["target_generation"], before + 1);
        assert_eq!(frame.meta["selection_generation"], selected_generation + 1);
        assert!(frame.meta["selected_last_seen_age_ms"].is_null());
    }
    #[test]
    fn stale_expiry_increments_only_on_actual_state_boundaries() {
        let (mut state, at) = selected();
        let before = state.target_generation;
        let selected_generation = state.selection_generation;
        let lost = state
            .latest_snapshot(at + Duration::from_millis(401))
            .unwrap();
        assert_eq!(lost.meta["selected_status"], "temporarily_lost");
        assert_eq!(lost.meta["target_generation"], before + 1);
        assert_eq!(lost.meta["selection_generation"], selected_generation);
        assert_eq!(lost.meta["selected_last_seen_age_ms"], 401.0);
        for _ in 0..20 {
            assert_eq!(
                state
                    .latest_snapshot(at + Duration::from_millis(450))
                    .unwrap()
                    .meta["target_generation"],
                before + 1
            );
        }
        let expired = state
            .latest_snapshot(at + Duration::from_millis(601))
            .unwrap();
        assert!(expired.meta["selected_track_key"].is_null());
        assert_eq!(expired.meta["target_generation"], before + 2);
        assert_eq!(
            expired.meta["selection_generation"],
            selected_generation + 1
        );
        assert!(expired.meta["selected_last_seen_age_ms"].is_null());
        assert_eq!(
            state
                .latest_snapshot(at + Duration::from_millis(602))
                .unwrap()
                .meta["target_generation"],
            before + 2
        );
    }
    #[test]
    fn clear_is_idempotent_but_explicit_camera_lifecycle_always_invalidates() {
        let (mut state, _) = selected();
        let before = state.target_generation;
        let selected_generation = state.selection_generation;
        state.clear_selection();
        assert_eq!(state.target_generation, before + 1);
        assert_eq!(state.selection_generation, selected_generation + 1);
        state.clear_selection();
        assert_eq!(state.target_generation, before + 1);
        assert_eq!(state.selection_generation, selected_generation + 1);
        state.reset_target_lifecycle();
        assert_eq!(state.target_generation, before + 2);
        assert_eq!(state.selection_generation, selected_generation + 2);
        state.reset_target_lifecycle();
        assert_eq!(state.target_generation, before + 3);
        assert_eq!(state.selection_generation, selected_generation + 3);
    }
    #[test]
    fn owned_snapshots_echo_the_generation_of_their_own_selection() {
        let (mut state, at) = selected();
        let a = state
            .latest_snapshot(at + Duration::from_millis(2))
            .unwrap();
        state.select_key("B", at + Duration::from_millis(3), true);
        let b = state
            .latest_snapshot(at + Duration::from_millis(4))
            .unwrap();
        assert_eq!(a.meta["selected_track_key"], "A");
        assert_eq!(b.meta["selected_track_key"], "B");
        assert_eq!(
            b.meta["target_generation"].as_u64().unwrap(),
            a.meta["target_generation"].as_u64().unwrap() + 1
        );
        assert_eq!(
            b.meta["selection_generation"].as_u64().unwrap(),
            a.meta["selection_generation"].as_u64().unwrap() + 1
        );
    }
    #[test]
    fn repeated_or_old_worker_errors_do_not_keep_invalidating_a_new_selection() {
        let (state, _) = selected();
        let before = state.target_generation;
        let state = Arc::new(Mutex::new(state));
        let current = AtomicU64::new(5);
        state_error(&state, &current, 4, "old worker".into());
        assert_eq!(state.lock().unwrap().target_generation, before);
        state_error(&state, &current, 5, "disconnect".into());
        assert_eq!(state.lock().unwrap().target_generation, before + 1);
        state_error(&state, &current, 5, "same disconnect".into());
        assert_eq!(state.lock().unwrap().target_generation, before + 1);
    }
    #[test]
    fn status_latest_and_getter_return_the_same_target_generation_without_poll_churn() {
        let (service_state, _) = selected();
        let service = CameraService::new(PathBuf::from("."));
        *service.state.lock().unwrap() = service_state;
        let generation = service.target_generation();
        let selection_generation = service.selection_generation();
        for _ in 0..10 {
            assert_eq!(
                service.latest().unwrap().meta["target_generation"],
                generation
            );
            assert_eq!(service.status()["target_generation"], generation);
            assert_eq!(service.target_generation(), generation);
            assert_eq!(
                service.latest().unwrap().meta["selection_generation"],
                selection_generation
            );
            assert_eq!(
                service.status()["selection_generation"],
                selection_generation
            );
            assert_eq!(
                service.frame()["selection_generation"],
                selection_generation
            );
            assert_eq!(service.selection_generation(), selection_generation);
        }
    }

    #[test]
    fn metadata_poll_preserves_frame_and_matches_snapshot_through_target_expiry() {
        let (mut state, at) = selected();
        let pixels = vec![42; 1024 * 1024];
        state.latest.as_mut().unwrap().jpeg = Arc::new(pixels.clone());
        let address = state.latest.as_ref().unwrap().jpeg.as_ptr();
        for age in [5, 401, 450, 601, 602] {
            let time = at + Duration::from_millis(age);
            let meta = state.latest_meta(time).unwrap();
            let snapshot = state.latest_snapshot(time).unwrap();
            assert_eq!(meta, snapshot.meta);
            assert_eq!(*snapshot.jpeg, pixels);
            assert!(Arc::ptr_eq(
                &snapshot.jpeg,
                &state.latest.as_ref().unwrap().jpeg
            ));
            assert_eq!(state.latest.as_ref().unwrap().jpeg.as_ptr(), address);
            assert!(meta.get("jpeg").is_none());
        }
        assert!(state.selected.is_none());
    }
}
fn supervise(
    root: PathBuf,
    id: String,
    generation: u64,
    current: Arc<AtomicU64>,
    state: Arc<Mutex<State>>,
    control: Arc<Control>,
    preview_changed: tokio::sync::watch::Sender<()>,
) {
    #[cfg(all(test, windows))]
    let _supervisor_receipt = test_worker::SupervisorReceipt(control.clone());
    for attempt in 0..3 {
        if control.stop.load(Ordering::SeqCst) || current.load(Ordering::SeqCst) != generation {
            return;
        }
        let mut child = match command(&root).and_then(|mut c| {
            c.arg("--camera-worker")
                .arg(&root)
                .arg(&id)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())
        }) {
            Ok(c) => c,
            Err(e) => {
                state_error(&state, &current, generation, e);
                return;
            }
        };
        #[cfg(all(test, windows))]
        control
            .diagnostic
            .child_started
            .fetch_add(1, Ordering::SeqCst);
        let _job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _waited = child.wait();
                #[cfg(all(test, windows))]
                control.diagnostic.waited(_waited.is_ok());
                state_error(&state, &current, generation, error);
                return;
            }
        };
        #[cfg(all(test, windows))]
        let _job_receipt = test_worker::JobReceipt::new(&_job, &control);
        if let Err(error) = release_launch_gate(&mut child) {
            let _ = child.kill();
            let _waited = child.wait();
            #[cfg(all(test, windows))]
            control.diagnostic.waited(_waited.is_ok());
            state_error(&state, &current, generation, error);
            return;
        }
        let mut stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let diagnostics = Arc::new(Mutex::new(String::new()));
        let diag = diagnostics.clone();
        thread::spawn(move || {
            let mut rdr = stderr;
            let mut b = [0u8; 1024];
            while let Ok(n) = rdr.read(&mut b) {
                if n == 0 {
                    break;
                }
                let mut d = diag.lock().unwrap();
                d.push_str(&String::from_utf8_lossy(&b[..n]));
                if d.len() > 4096 {
                    let tail = d
                        .chars()
                        .rev()
                        .take(1000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    *d = tail;
                }
            }
        });
        *control.last_packet.lock().unwrap() = Instant::now();
        *control.child.lock().unwrap() = Some(child);
        let monitor = control.clone();
        let mon_current = current.clone();
        let mon_state = state.clone();
        let mon_diagnostics = diagnostics.clone();
        let monitor_done = Arc::new(AtomicBool::new(false));
        let done = monitor_done.clone();
        thread::spawn(move || {
            while !done.load(Ordering::SeqCst)
                && !monitor.stop.load(Ordering::SeqCst)
                && mon_current.load(Ordering::SeqCst) == generation
            {
                if monitor.last_packet.lock().unwrap().elapsed() > Duration::from_secs(10) {
                    state_error(
                        &mon_state,
                        &mon_current,
                        generation,
                        format!(
                            "原生相机驱动或推理超时；正在回收专用进程。{}",
                            mon_diagnostics
                                .lock()
                                .unwrap()
                                .chars()
                                .take(1000)
                                .collect::<String>()
                        ),
                    );
                    if let Some(c) = monitor.child.lock().unwrap().as_mut() {
                        let _ = c.kill();
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
        });
        loop {
            if control.stop.load(Ordering::SeqCst) || current.load(Ordering::SeqCst) != generation {
                break;
            }
            match read_packet(&mut stdout) {
                Ok((meta, jpeg)) => {
                    *control.last_packet.lock().unwrap() = Instant::now();
                    if meta["kind"] == "error" {
                        state_error(
                            &state,
                            &current,
                            generation,
                            meta["error"].as_str().unwrap_or("Camera error").to_owned(),
                        );
                        break;
                    }
                    let mut s = state.lock().unwrap();
                    if current.load(Ordering::SeqCst) != generation {
                        break;
                    }
                    let accepted = s.accept_frame(meta, jpeg, Instant::now());
                    drop(s);
                    if accepted {
                        #[cfg(all(test, windows))]
                        control
                            .diagnostic
                            .accepted_processed_frames
                            .fetch_add(1, Ordering::SeqCst);
                        preview_changed.send_replace(());
                    }
                }
                Err(e) => {
                    state_error(&state, &current, generation, e);
                    break;
                }
            }
        }
        monitor_done.store(true, Ordering::SeqCst);
        if let Some(mut c) = control.child.lock().unwrap().take() {
            let _ = c.kill();
            let _waited = c.wait();
            #[cfg(all(test, windows))]
            control.diagnostic.waited(_waited.is_ok());
        }
        if control.stop.load(Ordering::SeqCst) || current.load(Ordering::SeqCst) != generation {
            return;
        }
        if attempt == 2 {
            let mut s = state.lock().unwrap();
            s.phase = "error".into();
            s.error = Some(format!(
                "相机连续三次启动/采集失败。{} {}",
                s.error.clone().unwrap_or_default(),
                diagnostics
                    .lock()
                    .unwrap()
                    .chars()
                    .take(600)
                    .collect::<String>()
            ));
            return;
        }
        thread::sleep(Duration::from_secs(1));
    }
}
fn read_packet(r: &mut impl Read) -> Result<(Value, Vec<u8>), String> {
    let mut n = [0u8; 4];
    r.read_exact(&mut n).map_err(|e| e.to_string())?;
    let count = u32::from_le_bytes(n) as usize;
    if count > MAX_META || count == 0 {
        return Err("Invalid camera metadata length".into());
    }
    let mut meta = vec![0; count];
    r.read_exact(&mut meta).map_err(|e| e.to_string())?;
    let meta: Value = serde_json::from_slice(&meta).map_err(|e| e.to_string())?;
    r.read_exact(&mut n).map_err(|e| e.to_string())?;
    let count = u32::from_le_bytes(n) as usize;
    if count > MAX_JPEG {
        return Err("Camera image exceeds configured bound".into());
    }
    let mut jpeg = vec![0; count];
    r.read_exact(&mut jpeg).map_err(|e| e.to_string())?;
    Ok((meta, jpeg))
}
fn write_packet(w: &mut impl Write, meta: &Value, jpeg: &[u8]) -> Result<(), String> {
    let m = serde_json::to_vec(meta).map_err(|e| e.to_string())?;
    if m.len() > MAX_META || jpeg.len() > MAX_JPEG {
        return Err("Frame packet exceeds bounds".into());
    }
    w.write_all(&(m.len() as u32).to_le_bytes())
        .and_then(|_| w.write_all(&m))
        .and_then(|_| w.write_all(&(jpeg.len() as u32).to_le_bytes()))
        .and_then(|_| w.write_all(jpeg))
        .and_then(|_| w.flush())
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug)]
struct FaceDetection {
    bbox: [f32; 4],
    score: f32,
}
impl DetectionBox for FaceDetection {
    fn bbox(&self) -> [f32; 4] {
        self.bbox
    }
    fn score(&self) -> f32 {
        self.score
    }
    fn label(&self) -> usize {
        0
    }
}
struct Detector {
    session: Session,
}
#[derive(Debug, Default)]
struct DetectionTiming {
    preprocess_ms: f64,
    ort_run_ms: f64,
    output_decode_ms: f64,
    nms_ms: f64,
}
fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
impl Detector {
    fn new(root: &Path) -> Result<Self, String> {
        let path = root.join("models/yolov8n-face-lindevs.onnx");
        let data = fs::read(&path).map_err(|e| e.to_string())?;
        if format!("{:x}", Sha256::digest(&data)) != MODEL_SHA {
            return Err("人脸权重 SHA-256 不匹配，拒绝加载".into());
        }
        let dll = root.join("models/native/onnxruntime.dll");
        if !dll.is_file() {
            return Err("缺少项目本地 ONNX Runtime DLL".into());
        }
        if format!(
            "{:x}",
            Sha256::digest(fs::read(&dll).map_err(|e| e.to_string())?)
        ) != ORT_DLL_SHA
        {
            return Err("ONNX Runtime DLL 摘要不匹配，拒绝动态加载".into());
        }
        ort::init_from(dll.display().to_string())
            .with_telemetry(false)
            .commit()
            .map_err(|e| e.to_string())?;
        let session = Session::builder()
            .map_err(|e| e.to_string())?
            .with_intra_threads(3)
            .map_err(|e| e.to_string())?
            .with_inter_threads(1)
            .map_err(|e| e.to_string())?
            .commit_from_file(path)
            .map_err(|e| e.to_string())?;
        let names = session
            .metadata()
            .map_err(|e| e.to_string())?
            .custom("names")
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        if names != "{0: 'face'}" && names != "{0: \"face\"}" {
            return Err(format!("权重类别不是专用 face: {names}"));
        }
        Ok(Self { session })
    }
    #[cfg(test)]
    fn detect(&mut self, image: &RgbImage) -> Result<Vec<FaceDetection>, String> {
        self.detect_timed(image).map(|(faces, _)| faces)
    }
    fn detect_timed(
        &mut self,
        image: &RgbImage,
    ) -> Result<(Vec<FaceDetection>, DetectionTiming), String> {
        let start = Instant::now();
        let (input, transform) = letterbox(image);
        let tensor = Tensor::from_array(([1usize, 3, 640, 640], input.into_boxed_slice()))
            .map_err(|e| e.to_string())?;
        let preprocess_ms = elapsed_ms(start);
        let start = Instant::now();
        let outputs = self
            .session
            .run(ort::inputs![tensor])
            .map_err(|e| e.to_string())?;
        let ort_run_ms = elapsed_ms(start);
        let start = Instant::now();
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        if shape.as_ref() != [1i64, 5, 8400] {
            return Err(format!("Unexpected face output shape: {shape:?}"));
        }
        let candidates = decode_candidates(data, image.width(), image.height(), transform);
        let output_decode_ms = elapsed_ms(start);
        let start = Instant::now();
        let faces = suppress_candidates(candidates);
        let nms_ms = elapsed_ms(start);
        Ok((
            faces,
            DetectionTiming {
                preprocess_ms,
                ort_run_ms,
                output_decode_ms,
                nms_ms,
            },
        ))
    }
}
#[derive(Clone, Copy, Debug)]
struct Letterbox {
    scale: f32,
    left: f32,
    top: f32,
}
fn letterbox(image: &RgbImage) -> (Vec<f32>, Letterbox) {
    let scale = (640.0 / image.width() as f32).min(640.0 / image.height() as f32);
    let w = (image.width() as f32 * scale).round() as u32;
    let h = (image.height() as f32 * scale).round() as u32;
    let left = (640 - w) / 2;
    let top = (640 - h) / 2;
    let resized = image::imageops::resize(image, w, h, FilterType::Triangle);
    let mut input = vec![114.0 / 255.0; 3 * 640 * 640];
    for (x, y, p) in resized.enumerate_pixels() {
        let index = ((y + top) * 640 + x + left) as usize;
        for c in 0..3 {
            input[c * 640 * 640 + index] = p[c] as f32 / 255.0
        }
    }
    (
        input,
        Letterbox {
            scale,
            left: left as f32,
            top: top as f32,
        },
    )
}
fn unletterbox(mut b: [f32; 4], t: Letterbox, w: u32, h: u32) -> [f32; 4] {
    for i in [0, 2] {
        b[i] = ((b[i] - t.left) / t.scale).clamp(0.0, w as f32) / w as f32
    }
    for i in [1, 3] {
        b[i] = ((b[i] - t.top) / t.scale).clamp(0.0, h as f32) / h as f32
    }
    b
}
fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let inter =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.0) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    inter / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter).max(1e-8)
}
fn overlap(a: [f32; 4], b: [f32; 4]) -> f32 {
    let inter =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.0) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    inter
        / ((a[2] - a[0]) * (a[3] - a[1]))
            .min((b[2] - b[0]) * (b[3] - b[1]))
            .max(1e-8)
}
#[cfg(test)]
fn postprocess(data: &[f32], w: u32, h: u32, t: Letterbox) -> Vec<FaceDetection> {
    suppress_candidates(decode_candidates(data, w, h, t))
}
fn decode_candidates(data: &[f32], w: u32, h: u32, t: Letterbox) -> Vec<FaceDetection> {
    let mut candidates = Vec::new();
    for i in 0..8400 {
        let score = data[4 * 8400 + i];
        if !score.is_finite() || score < 0.15 {
            continue;
        }
        let (cx, cy, bw, bh) = (
            data[i],
            data[8400 + i],
            data[2 * 8400 + i],
            data[3 * 8400 + i],
        );
        let bbox = unletterbox(
            [cx - bw / 2.0, cy - bh / 2.0, cx + bw / 2.0, cy + bh / 2.0],
            t,
            w,
            h,
        );
        if bbox.iter().all(|v| v.is_finite()) && bbox[2] > bbox[0] && bbox[3] > bbox[1] {
            candidates.push(FaceDetection { bbox, score })
        }
    }
    candidates
}
fn suppress_candidates(mut candidates: Vec<FaceDetection>) -> Vec<FaceDetection> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates.truncate(400);
    let mut kept: Vec<FaceDetection> = Vec::new();
    for candidate in candidates {
        if kept.iter().all(|d| iou(d.bbox, candidate.bbox) < 0.45) {
            kept.push(candidate);
            if kept.len() == 32 {
                break;
            }
        }
    }
    kept
}

/// Keep the existing 25% smaller-box overlap threshold. Calculate involvement
/// before filtering young/unassigned detections so an unconfirmed second face
/// cannot make an overlapping selected target look unambiguous.
fn ambiguous_detections(detections: &[FaceDetection]) -> Vec<bool> {
    let mut ambiguous = vec![false; detections.len()];
    for (index, a) in detections.iter().enumerate() {
        for (other, b) in detections.iter().enumerate().skip(index + 1) {
            if overlap(a.bbox, b.bbox) > 0.25 {
                ambiguous[index] = true;
                ambiguous[other] = true;
            }
        }
    }
    ambiguous
}

fn tracked_faces(
    epoch: &str,
    detections: &[FaceDetection],
    infos: &[Option<TrackInfo>],
    ambiguous: &[bool],
    labels: &mut HashMap<Uuid, u32>,
    serial: &mut u32,
) -> (Vec<Value>, Vec<String>) {
    let mut tracks = Vec::new();
    let mut ambiguous_keys = Vec::new();
    for (index, (d, info)) in detections.iter().zip(infos.iter()).enumerate() {
        let Some(info) = info else { continue };
        if info.count < 3 {
            continue;
        }
        let label = *labels.entry(info.uuid).or_insert_with(|| {
            *serial += 1;
            *serial
        });
        let key = format!("{epoch}:{}", info.uuid);
        let involved = ambiguous.get(index).copied().unwrap_or(true);
        if involved {
            ambiguous_keys.push(key.clone());
        }
        tracks.push(json!({"track_key":key,"label":format!("Track {label}"),"box":d.bbox,"confidence":d.score,"association_ambiguous":involved}));
    }
    (tracks, ambiguous_keys)
}

struct Captured {
    image: RgbImage,
    jpeg: Option<Vec<u8>>,
    id: u64,
    epoch: String,
    captured: u64,
    capture_time: Instant,
    published: Instant,
    format: Value,
    capture_fps: f64,
    native_pose: Value,
    read_sample_ms: f64,
    decode_ms: f64,
    native_pose_last_read_ms: f64,
}

fn decode_mjpeg(bytes: &[u8], width: u32, height: u32) -> Result<RgbImage, String> {
    const ALLOCATION_LIMIT: u64 = 32 * 1024 * 1024;
    if bytes.is_empty()
        || bytes.len() > MAX_JPEG
        || width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > ALLOCATION_LIMIT / 3
    {
        return Err("MJPEG input exceeds capture limits".into());
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(ALLOCATION_LIMIT);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| format!("Rust MJPEG decode failed: {e}"))?
        .to_rgb8();
    if image.dimensions() != (width, height) {
        return Err("JPEG dimensions do not match negotiated capture format".into());
    }
    Ok(image)
}

#[derive(Clone)]
struct WorkerStage(Arc<Mutex<(&'static str, Instant)>>);
impl WorkerStage {
    fn new(stage: &'static str) -> Self {
        Self(Arc::new(Mutex::new((stage, Instant::now()))))
    }
    fn mark(&self, stage: &'static str) {
        *self.0.lock().unwrap() = (stage, Instant::now());
    }
}
fn watch_worker_stages(capture: WorkerStage, processing: WorkerStage) {
    thread::spawn(move || {
        let mut logged = [false; 2];
        loop {
            thread::sleep(Duration::from_millis(250));
            for (index, (role, progress)) in [("capture", &capture), ("processing", &processing)]
                .iter()
                .enumerate()
            {
                let (stage, since) = *progress.0.lock().unwrap();
                let idle_ms = since.elapsed().as_millis();
                if idle_ms < 2000 {
                    logged[index] = false;
                } else if !logged[index] {
                    eprintln!(
                        "{}",
                        json!({"kind":"camera_worker_stall","role":role,"stage":stage,"idle_ms":idle_ms})
                    );
                    logged[index] = true;
                }
            }
        }
    });
}
/// This function owns only a native camera role; stdout is a bounded binary protocol.
pub fn camera_worker(root: PathBuf, id: String) -> Result<(), String> {
    await_parent_job()?;
    let capture_progress = WorkerStage::new("waiting_for_detector");
    let processing_progress = WorkerStage::new("loading_detector");
    watch_worker_stages(capture_progress.clone(), processing_progress.clone());
    let mut detector = Detector::new(&root)?;
    let slot: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap_slot = slot.clone();
    let cap_error = error.clone();
    thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            capture_progress.mark("opening_device");
            let desired = CameraFormat::new(Resolution::new(1280, 720), FrameFormat::MJPEG, 30);
            let request = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(desired));
            let mut camera = Camera::with_backend(
                CameraIndex::String(id.clone()),
                request,
                ApiBackend::MediaFoundation,
            )
            .map_err(|e| e.to_string())?;
            let native_formats = camera.compatible_camera_formats().unwrap_or_default();
            let truncated = native_formats.len() > 32;
            let formats=native_formats.into_iter().take(32).map(|f|json!({"width":f.width(),"height":f.height(),"fps":f.frame_rate(),"format":format!("{:?}",f.format())})).collect::<Vec<_>>();
            camera.open_stream().map_err(|e| e.to_string())?;
            let actual = camera.camera_format();
            let info = json!({"width":actual.width(),"height":actual.height(),"driver_reported_fps":actual.frame_rate(),"requested_fps":30,"pixel_format":format!("{:?}",actual.format()),"available_formats":formats,"available_formats_truncated":truncated,"fps_note":"Native format metadata is separate from measured capture_fps"});
            let epoch = Uuid::new_v4().to_string();
            let mut frame_id = 0u64;
            let mut pose_seq = 0u64;
            let mut pose = Value::Null;
            let mut last_pose = Instant::now() - Duration::from_secs(1);
            let mut pose_read_ms = 0.0;
            let capture_started = Instant::now();
            loop {
                capture_progress.mark("read_sample");
                let read_started = Instant::now();
                let buffer = camera.frame().map_err(|e| e.to_string())?;
                let read_sample_ms = read_started.elapsed().as_secs_f64() * 1000.0;
                let capture_time = Instant::now();
                let captured = now_ms();
                if last_pose.elapsed() >= Duration::from_millis(150) {
                    pose_seq += 1;
                    let pose_started = Instant::now();
                    pose = crate::camera_pose::sample_observed(
                        &camera,
                        &id,
                        &epoch,
                        pose_seq,
                        |stage| capture_progress.mark(stage),
                    );
                    pose_read_ms = pose_started.elapsed().as_secs_f64() * 1000.0;
                    last_pose = Instant::now();
                }
                let native_jpeg = if buffer.source_frame_format() == FrameFormat::MJPEG
                    && buffer.buffer().starts_with(&[0xff, 0xd8])
                    && buffer.buffer().len() <= MAX_JPEG
                {
                    Some(buffer.buffer().to_vec())
                } else {
                    None
                };
                let resolution = buffer.resolution();
                capture_progress.mark("decode_frame");
                let decode_started = Instant::now();
                let image = if buffer.source_frame_format() == FrameFormat::MJPEG {
                    decode_mjpeg(buffer.buffer(), resolution.width(), resolution.height())?
                } else {
                    let decoded = buffer
                        .decode_image::<RgbFormat>()
                        .map_err(|e| e.to_string())?;
                    RgbImage::from_raw(resolution.width(), resolution.height(), decoded.into_raw())
                        .ok_or("Invalid RGB camera buffer")?
                };
                let decode_ms = decode_started.elapsed().as_secs_f64() * 1000.0;
                frame_id += 1;
                let capture_fps =
                    frame_id as f64 / capture_started.elapsed().as_secs_f64().max(0.001);
                capture_progress.mark("publish_latest_frame");
                let mut captured_frame = Captured {
                    image,
                    jpeg: native_jpeg,
                    id: frame_id,
                    epoch: epoch.clone(),
                    captured,
                    capture_time,
                    published: Instant::now(),
                    format: info.clone(),
                    capture_fps,
                    native_pose: pose.clone(),
                    read_sample_ms,
                    decode_ms,
                    native_pose_last_read_ms: pose_read_ms,
                };
                let mut latest = cap_slot.lock().unwrap();
                captured_frame.published = Instant::now();
                *latest = Some(captured_frame);
            }
        })();
        if let Err(e) = result {
            *cap_error.lock().unwrap() = Some(e);
        }
    });
    let mut tracker = ByteTrackBuilder::new()
        .track_high_conf(0.4)
        .track_iou(0.25)
        .track_update(0.25)
        .track_extra_lifespan(600_000_000)
        .build::<FaceDetection>();
    let mut labels: HashMap<Uuid, u32> = HashMap::new();
    let mut serial = 0u32;
    let started = Instant::now();
    let mut count = 0u64;
    let mut last = 0u64;
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    loop {
        processing_progress.mark("waiting_latest_frame");
        if let Some(error) = error.lock().unwrap().take() {
            write_packet(&mut writer, &json!({"kind":"error","error":error}), &[])?;
            return Err("Native capture ended".into());
        }
        let frame = slot.lock().unwrap().take();
        let Some(frame) = frame else {
            thread::sleep(Duration::from_millis(2));
            continue;
        };
        if frame.id == last {
            continue;
        }
        last = frame.id;
        let tick = Instant::now();
        let latest_slot_wait_ms = tick
            .saturating_duration_since(frame.published)
            .as_secs_f64()
            * 1000.0;
        let capture_to_process_ms = tick
            .saturating_duration_since(frame.capture_time)
            .as_secs_f64()
            * 1000.0;
        processing_progress.mark("face_inference");
        let (detections, detection_timing) = detector.detect_timed(&frame.image)?;
        let detector_ms = tick.elapsed().as_secs_f64() * 1000.0;
        let timestamp = started.elapsed().as_nanos() as u64;
        processing_progress.mark("face_tracking");
        let tracking_started = Instant::now();
        let infos = tracker.update(&detections, timestamp);
        let tracking_ms = elapsed_ms(tracking_started);
        let association_started = Instant::now();
        let ambiguous = ambiguous_detections(&detections);
        let (tracks, ambiguous_keys) = tracked_faces(
            &frame.epoch,
            &detections,
            &infos,
            &ambiguous,
            &mut labels,
            &mut serial,
        );
        if labels.len() > 2048 {
            let active = tracker.get_active_tracks();
            labels.retain(|id, _| active.iter().any(|t| t.info.uuid == *id));
        }
        let association_ms = elapsed_ms(association_started);
        let scene_ambiguous = ambiguous.iter().any(|value| *value);
        processing_progress.mark("encode_preview");
        let jpeg_reused = frame.jpeg.is_some();
        let encode_started = Instant::now();
        let jpeg = if let Some(jpeg) = frame.jpeg {
            jpeg
        } else {
            let mut jpeg = Vec::new();
            JpegEncoder::new_with_quality(&mut jpeg, 78)
                .encode_image(&frame.image)
                .map_err(|e| e.to_string())?;
            jpeg
        };
        let preview_prepare_ms = elapsed_ms(encode_started);
        let jpeg_encode_ms = if jpeg_reused { 0.0 } else { preview_prepare_ms };
        count += 1;
        let meta = json!({"kind":"frame","frame_id":frame.id,"camera_session_id":frame.epoch,"width":frame.image.width(),"height":frame.image.height(),"captured_at_ms":frame.captured,
            "tracks":tracks,"association_ambiguous":scene_ambiguous,"scene_association_ambiguous":scene_ambiguous,"association_ambiguity_scope":TRACK_AMBIGUITY_SCOPE,"ambiguous_track_keys":ambiguous_keys,
            "inference_ms":detector_ms,"processing_ms":tick.elapsed().as_secs_f64()*1000.0,"capture_fps":frame.capture_fps,
            "fps":count as f64/started.elapsed().as_secs_f64().max(0.001),"frame_age_ms":frame.capture_time.elapsed().as_secs_f64()*1000.0,"negotiated":frame.format,
            "runtime":"Rust native","face_class":"face","raw_media_saved":false,"native_pose":frame.native_pose,
            "read_sample_ms":frame.read_sample_ms,"decode_ms":frame.decode_ms,"native_pose_last_read_ms":frame.native_pose_last_read_ms,
            "timings_ms":{"preprocess":detection_timing.preprocess_ms,"ort_run":detection_timing.ort_run_ms,"output_decode":detection_timing.output_decode_ms,"nms":detection_timing.nms_ms,"tracking":tracking_ms,"association_metadata":association_ms,"jpeg_encode":jpeg_encode_ms,"preview_prepare":preview_prepare_ms,"latest_slot_wait":latest_slot_wait_ms,"capture_to_process":capture_to_process_ms},
            "preview_jpeg_reused":jpeg_reused,"frame_age_origin":"after_camera_frame_return","timing_clock":"rust_instant"});
        processing_progress.mark("write_frame_packet");
        write_packet(&mut writer, &meta, &jpeg)?;
    }
}

#[cfg(test)]
#[path = "vision_replay_tests.rs"]
mod vision_replay_tests;

#[cfg(test)]
#[path = "vision_photo_probe.rs"]
mod vision_photo_probe;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mjpeg_decode_checks_dimensions_and_rejects_bad_or_unbounded_input() {
        let image = RgbImage::from_pixel(32, 24, image::Rgb([40, 100, 170]));
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 90)
            .encode_image(&image)
            .unwrap();
        let decoded = decode_mjpeg(&bytes, 32, 24).unwrap();
        assert_eq!(decoded.dimensions(), (32, 24));
        assert!(decode_mjpeg(&bytes, 16, 24).is_err());
        assert!(decode_mjpeg(&bytes, 64, 24).is_err());
        assert!(decode_mjpeg(&[0xff, 0xd8, 0x00], 32, 24).is_err());
        assert!(decode_mjpeg(&bytes[..bytes.len() / 2], 32, 24).is_err());
        assert!(decode_mjpeg(&bytes, u32::MAX, u32::MAX).is_err());
    }
    #[test]
    fn letterbox_roundtrip() {
        for (w, h) in [(1280, 720), (720, 1280), (301, 300)] {
            let image = RgbImage::new(w, h);
            let (data, t) = letterbox(&image);
            assert_eq!(data.len(), 3 * 640 * 640);
            let original = [0.1, 0.2, 0.8, 0.9];
            let padded = [
                original[0] * w as f32 * t.scale + t.left,
                original[1] * h as f32 * t.scale + t.top,
                original[2] * w as f32 * t.scale + t.left,
                original[3] * h as f32 * t.scale + t.top,
            ];
            let restored = unletterbox(padded, t, w, h);
            for i in 0..4 {
                assert!((restored[i] - original[i]).abs() < 1e-5)
            }
        }
    }
    #[test]
    fn packet_bounds() {
        let mut input = (u32::MAX).to_le_bytes().as_slice().to_vec();
        assert!(read_packet(&mut input.as_slice()).is_err());
        let mut encoded = Vec::new();
        write_packet(&mut encoded, &json!({"frame_id":3}), &[1, 2, 3]).unwrap();
        let (meta, jpeg) = read_packet(&mut encoded.as_slice()).unwrap();
        assert_eq!(meta["frame_id"], 3);
        assert_eq!(jpeg, [1, 2, 3]);
        input.clear();
    }
    #[test]
    fn real_bytetrack_recovers_low_confidence_same_uuid() {
        let mut tracker = ByteTrackBuilder::new()
            .track_high_conf(0.4)
            .track_extra_lifespan(600_000_000)
            .build::<FaceDetection>();
        let first = vec![FaceDetection {
            bbox: [0.1, 0.1, 0.3, 0.4],
            score: 0.9,
        }];
        let id = tracker.update(&first, 1_000_000)[0].unwrap().uuid;
        let low = vec![FaceDetection {
            bbox: [0.11, 0.1, 0.31, 0.4],
            score: 0.2,
        }];
        assert_eq!(tracker.update(&low, 50_000_000)[0].unwrap().uuid, id);
    }
    #[test]
    fn construction_never_opens_camera() {
        let s = CameraService::new(PathBuf::from("."));
        assert_eq!(s.status()["state"], "closed");
        assert!(s.latest().is_none());
        assert!(s.select("old:1").is_err());
    }
    #[test]
    fn preview_suppresses_stale_images_and_overlays_but_keeps_diagnostics() {
        let service = CameraService::new(PathBuf::from("."));
        {
            let mut s = service.state.lock().unwrap();
            s.phase = "running".into();
            s.latest = Some(FrameSnapshot {
                jpeg: Arc::new(vec![1, 2, 3]),
                received: Instant::now() - Duration::from_secs(2),
                meta: json!({"frame_id":9,"frame_age_ms":0,"tracks":[{"track_key":"old"}]}),
            });
        }
        let frame = service.frame();
        assert_eq!(frame["jpeg"], Value::Null);
        assert_eq!(frame["tracks"], json!([]));
        assert_eq!(frame["selected_track_key"], Value::Null);
        assert_eq!(frame["state"], "running");
        assert!(frame["frame_age_ms"].as_f64().unwrap() >= 2000.0);
        assert_eq!(service.status()["frame_id"], 9);
        assert!(service.state.lock().unwrap().latest.is_some());
    }
    #[test]
    fn preview_requires_running_even_when_the_last_image_is_fresh() {
        let service = CameraService::new(PathBuf::from("."));
        {
            let mut s = service.state.lock().unwrap();
            s.phase = "error".into();
            s.error = Some("capture interrupted".into());
            s.latest = Some(FrameSnapshot {
                jpeg: Arc::new(vec![1, 2, 3]),
                received: Instant::now(),
                meta: json!({"frame_age_ms":0,"tracks":[{"track_key":"old"}]}),
            });
        }
        let frame = service.frame();
        assert_eq!(frame["jpeg"], Value::Null);
        assert_eq!(frame["tracks"], json!([]));
        assert_eq!(frame["state"], "error");
        assert_eq!(frame["error"], "capture interrupted");
        service.state.lock().unwrap().phase = "running".into();
        let frame = service.frame();
        assert_eq!(frame["jpeg"], "AQID");
        assert_eq!(frame["tracks"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn native_face_model_contract_and_blank_inference() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut detector =
            Detector::new(&root).expect("Pinned native model and ONNX runtime must load");
        let faces = detector
            .detect(&RgbImage::new(640, 640))
            .expect("Native ONNX inference must run");
        assert!(faces.is_empty(), "Blank image must not manufacture a face");
    }
    #[test]
    fn explicit_selection_ignores_largest_face_and_rejects_stale_or_old_epoch() {
        let service = CameraService::new(PathBuf::from("."));
        service.state.lock().unwrap().latest = Some(FrameSnapshot {
            jpeg: Arc::new(vec![]),
            received: Instant::now(),
            meta: json!({"frame_age_ms":0,"tracks":[{"track_key":"epoch1:small","box":[0.1,0.1,0.2,0.2]},{"track_key":"epoch1:large","box":[0.3,0.3,0.9,0.9]}]}),
        });
        assert_eq!(service.status()["selected_track_key"], Value::Null);
        service.select("epoch1:small").unwrap();
        assert_eq!(service.status()["selected_track_key"], "epoch1:small");
        service
            .state
            .lock()
            .unwrap()
            .latest
            .as_mut()
            .unwrap()
            .received = Instant::now() - Duration::from_secs(1);
        assert!(service.select("epoch1:large").is_err());
        assert_eq!(service.status()["selected_track_key"], Value::Null);
        service.state.lock().unwrap().latest = Some(FrameSnapshot {
            jpeg: Arc::new(vec![]),
            received: Instant::now(),
            meta: json!({"frame_age_ms":0,"tracks":[{"track_key":"epoch2:small"}]}),
        });
        assert!(service.select("epoch1:small").is_err());
    }
    #[test]
    fn overlapping_faces_refuse_binding() {
        let service = CameraService::new(PathBuf::from("."));
        service.state.lock().unwrap().latest = Some(FrameSnapshot {
            jpeg: Arc::new(vec![]),
            received: Instant::now(),
            meta: json!({"frame_age_ms":0,"association_ambiguous":true,"tracks":[{"track_key":"one"}]}),
        });
        assert!(service.select("one").is_err());
    }
    #[test]
    fn yolo_face_nms_preserves_independent_face_and_coordinates() {
        let mut output = vec![0.0; 5 * 8400];
        for (i, cx, score) in [(0, 100.0, 0.9), (1, 101.0, 0.8), (2, 400.0, 0.7)] {
            output[i] = cx;
            output[8400 + i] = 100.0;
            output[2 * 8400 + i] = 50.0;
            output[3 * 8400 + i] = 60.0;
            output[4 * 8400 + i] = score;
        }
        let found = postprocess(
            &output,
            640,
            640,
            Letterbox {
                scale: 1.0,
                left: 0.0,
                top: 0.0,
            },
        );
        assert_eq!(found.len(), 2);
        assert!((found[0].bbox[0] - 75.0 / 640.0).abs() < 1e-6);
    }
    #[test]
    #[cfg(windows)]
    fn job_close_terminates_only_owned_idle_child() {
        use std::os::windows::process::CommandExt;
        let executable = std::env::var_os("SYSTEMROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32/ping.exe");
        let mut child = Command::new(executable)
            .args(["127.0.0.1", "-n", "30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .expect("Native idle test process must start");
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{error}")
            }
        };
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let start = Instant::now();
        let mut exited = false;
        while start.elapsed() < Duration::from_secs(3) {
            if child.try_wait().unwrap().is_some() {
                exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !exited {
            let _ = child.kill();
        }
        let _ = child.wait();
        assert!(
            exited,
            "Closing the parent-owned job must reap the helper without relying on output/broken pipe"
        );
    }
    #[test]
    fn launch_gate_rejects_parent_eof_or_invalid_marker_before_hardware_initialization() {
        assert!(read_launch_gate(&mut b"".as_slice()).is_err());
        assert!(read_launch_gate(&mut b"BAD!".as_slice()).is_err());
        assert!(read_launch_gate(&mut b"RUN\n".as_slice()).is_ok());
    }
}

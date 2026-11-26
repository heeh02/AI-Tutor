//! One-shot finite calibration. No retries, AI switching or velocity commands.
//! Frames remain in memory; the exclusive journal contains numerical evidence.
use ai_tutor::{
    frame_motion::{
        BackgroundMotion, BackgroundPlan, MotionEstimate, RejectionReason,
        estimate_background_motion, prepare_background,
    },
    obsbot_motion::{CalibrationConfig, OscMotionSession, Pose, motion_error_details},
    obsbot_osc::{GimbalProfile, OscArg, OscMessage, encode_message},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const PROFILE: &str = "Tiny3Center2015PanPitch";
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
// Calibration sends one finite <=1 degree target. This 1000ms visual lease is
// distinct from the production follow controller's 250ms frame requirement.
const CALIBRATION_VISUAL_LEASE: Duration = Duration::from_millis(1000);
const STOP_ADDRESSES: [&str; 4] = [
    "/OBSBOT/WebCam/General/SetGimbalUp",
    "/OBSBOT/WebCam/General/SetGimbalDown",
    "/OBSBOT/WebCam/General/SetGimbalLeft",
    "/OBSBOT/WebCam/General/SetGimbalRight",
];
const STOP_LIMITATION: &str = "Four nonblocking localhost UDP zero submissions are best effort, not a verified hardware emergency stop. TerminateProcess/power loss/OS crash can bypass Drop. Only finite absolute targets of at most one degree are sent; no continuous velocity command exists. Console-close/shutdown callbacks are subject to Windows delivery and termination limits.";

fn read_small(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= limit,
        "configuration/evidence size limit"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "configuration/evidence size limit"
    );
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        bytes.drain(..3);
    }
    Ok(bytes)
}
fn validate_operator_config(
    value: &Value,
    config: &CalibrationConfig,
    device_id: &str,
) -> Result<()> {
    ensure!(
        config.gimbal_profile == GimbalProfile::Tiny3Center2015PanPitch,
        "unsupported calibration profile"
    );
    ensure!(
        value["server_port"].as_u64() == Some(u64::from(config.server_port)),
        "confirmed OSC port mismatch"
    );
    ensure!(
        value["device_index"].as_i64() == Some(i64::from(config.device_index)),
        "confirmed device index mismatch"
    );
    ensure!(
        value["expected_name"].as_str() == Some(config.expected_name.as_str()),
        "confirmed device name mismatch"
    );
    ensure!(
        value["gimbal_profile"].as_str() == Some(PROFILE),
        "confirmed profile mismatch"
    );
    ensure!(
        value["camera_device_id"].as_str() == Some(device_id),
        "confirmed stable camera device mismatch"
    );
    let confirmation = &value["operator_confirmation"];
    ensure!(
        confirmation["vendor_ai_tracking_master_off"] == true
            && confirmation["vendor_ai_mode_master_off"] == true,
        "both vendor AI master-off confirmations required"
    );
    ensure!(
        confirmation["source_type"].as_str() == Some("user_confirmation"),
        "operator confirmation source required"
    );
    ensure!(
        confirmation["at"].as_str().is_some_and(|at| !at.is_empty()),
        "operator confirmation timestamp required"
    );
    Ok(())
}
fn local_preflight(root: &Path, config: &CalibrationConfig) -> Result<(Value, Value)> {
    let device = String::from_utf8(read_small(
        &root.join("runtime/rust_camera_device.txt"),
        2048,
    )?)?;
    let device = device.trim();
    ensure!(
        device.starts_with(r"\\?\usb#") && !device.contains(['\0', '\r', '\n']),
        "exact stable USB camera path required"
    );
    ensure!(
        device.to_ascii_lowercase().contains("vid_3564&pid_ff02"),
        "actual Tiny 3 stable camera path required"
    );
    let confirmed: Value = serde_json::from_slice(&read_small(
        &root.join("runtime/ptz-osc-config.json"),
        64 * 1024,
    )?)?;
    validate_operator_config(&confirmed, config, device)?;
    ensure!(
        confirmed["pose_source"] == "windows_native_confirmed_osc",
        "canonical native pose configuration required"
    );
    let binding = json!({"camera_device_id":device,"server_port":config.server_port,"device_index":config.device_index,"expected_name":config.expected_name,"gimbal_profile":PROFILE,"pose_source":"windows_native_confirmed_osc","capture_width":WIDTH,"capture_height":HEIGHT});
    Ok((binding, confirmed["operator_confirmation"].clone()))
}

struct Journal(Mutex<File>);
impl Journal {
    fn append(&self, event: Value) -> Result<()> {
        let mut file = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("journal lock poisoned"))?;
        serde_json::to_writer(
            &mut *file,
            &json!({"at":chrono::Local::now().to_rfc3339(),"event":event}),
        )?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }
    fn emergency_append(&self, event: &Value) {
        // A console-control thread must not wait for a suspended file owner.
        if let Ok(mut file) = self.0.try_lock() {
            let _ = serde_json::to_writer(
                &mut *file,
                &json!({"at":chrono::Local::now().to_rfc3339(),"event":event}),
            );
            let _ = file.write_all(b"\n");
            let _ = file.sync_data();
        }
    }
}
struct Reservation {
    report: File,
    journal: Arc<Journal>,
    initial: Value,
}
impl Reservation {
    fn create(root: &Path, binding: Value, confirmation: Value) -> Result<Self> {
        let runtime = root.join("runtime").canonicalize()?;
        ensure!(
            runtime.starts_with(root),
            "runtime must remain in workspace"
        );
        let initial = json!({"at":chrono::Local::now().to_rfc3339(),"run_id":uuid::Uuid::new_v4(),"result":"IN_PROGRESS","binding":binding,"operator_confirmation":confirmation,"automatic_retry_allowed":false,"journal":"runtime/ptz-calibration-rust.journal.jsonl","stop_limitation":STOP_LIMITATION});
        // Each reservation is create_new; all records remain after any error.
        // A crash between these calls retains the latch and prohibits a rerun.
        let mut latch = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(runtime.join("ptz-calibration-rust.latch.json"))
            .context("calibration already reserved/attempted; automatic retry forbidden")?;
        serde_json::to_writer_pretty(&mut latch, &initial)?;
        latch.sync_all()?;
        let journal = Arc::new(Journal(Mutex::new(
            OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(runtime.join("ptz-calibration-rust.journal.jsonl"))?,
        )));
        let mut report = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(runtime.join("ptz-calibration-rust.json"))
            .context("existing calibration report must not be overwritten")?;
        serde_json::to_writer_pretty(&mut report, &initial)?;
        report.sync_all()?;
        journal.append(json!({"kind":"exclusive_reservation","binding":initial["binding"],"automatic_retry_allowed":false}))?;
        Ok(Self {
            report,
            journal,
            initial,
        })
    }
    fn finish(&mut self, report: &Value) -> Result<()> {
        // Only this invocation's exclusively-created handle may be updated.
        let bytes = serde_json::to_vec_pretty(report)?;
        self.report.seek(SeekFrom::Start(0))?;
        self.report.write_all(&bytes)?;
        self.report.set_len(bytes.len() as u64)?;
        self.report.sync_all()?;
        Ok(())
    }
}

#[derive(Default)]
struct Signals {
    cancelled: AtomicBool,
    code: AtomicU32,
    notify: Notify,
    stop: Mutex<Option<Weak<StopCore>>>,
}
impl Signals {
    fn trigger(&self, code: u32) {
        self.code.store(code, Ordering::SeqCst);
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(slot) = self.stop.try_lock() {
            if let Some(core) = slot.as_ref().and_then(Weak::upgrade) {
                core.stop_now("windows_console_signal", Some(code));
            }
        }
        self.notify.notify_one();
    }
    async fn wait(&self) {
        loop {
            let notification = self.notify.notified();
            if self.cancelled.load(Ordering::SeqCst) {
                return;
            }
            notification.await;
        }
    }
}
static SIGNALS: OnceLock<Arc<Signals>> = OnceLock::new();
#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}
#[cfg(windows)]
unsafe extern "system" fn control_handler(code: u32) -> i32 {
    if !matches!(code, 0 | 1 | 2 | 5 | 6) {
        return 0;
    }
    // No console output, runtime calls, awaited work or finite targets here.
    if let Some(signals) = SIGNALS.get() {
        signals.trigger(code);
    }
    1
}
struct ConsoleHandler;
impl ConsoleHandler {
    fn install(signals: Arc<Signals>) -> Result<Self> {
        SIGNALS
            .set(signals)
            .map_err(|_| anyhow::anyhow!("signal handler already installed"))?;
        #[cfg(windows)]
        ensure!(
            unsafe { SetConsoleCtrlHandler(Some(control_handler), 1) } != 0,
            "Windows console handler installation failed"
        );
        Ok(Self)
    }
}
impl Drop for ConsoleHandler {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            SetConsoleCtrlHandler(Some(control_handler), 0);
        }
    }
}

struct StopCore {
    socket: UdpSocket,
    packets: [Vec<u8>; 4],
    armed: AtomicBool,
    signals: Arc<Signals>,
    journal: Arc<Journal>,
    last_stop: Mutex<Option<Value>>,
}
impl StopCore {
    fn stop_now(&self, reason: &str, signal_code: Option<u32>) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        let start = Instant::now();
        let results=self.packets.iter().enumerate().map(|(direction,packet)| {
            match self.socket.send(packet) {
                Ok(bytes)=>json!({"direction_index":direction,"result_code":if bytes==packet.len(){0}else{4},"bytes_sent":bytes,"expected_bytes":packet.len(),"os_error":null}),
                Err(error)=>json!({"direction_index":direction,"result_code":1,"bytes_sent":0,"expected_bytes":packet.len(),"os_error":error.raw_os_error()}),
            }
        }).collect::<Vec<_>>();
        let all = results.iter().all(|item| item["result_code"] == 0);
        let event = json!({"kind":"synchronous_zero_stop","reason":reason,"signal_code":signal_code,"commands_attempted":4,"command_results":results,"all_zero_commands_sent":all,"physical_stop_verified":false,"elapsed_us":start.elapsed().as_micros(),"limitation":STOP_LIMITATION});
        if let Ok(mut slot) = self.last_stop.try_lock() {
            *slot = Some(event.clone());
        }
        self.journal.emergency_append(&event);
    }
}
struct ZeroStopOnDrop(Arc<StopCore>);
impl ZeroStopOnDrop {
    fn after_verified(
        session: &OscMotionSession,
        config: &CalibrationConfig,
        journal: Arc<Journal>,
        signals: Arc<Signals>,
    ) -> Result<Self> {
        ensure!(
            session.verification().expected_name == config.expected_name
                && session.verification().device_index == config.device_index,
            "zero guard requires real session gate"
        );
        Self::new(config.server_port, config.device_index, journal, signals)
    }
    fn new(port: u16, index: i32, journal: Arc<Journal>, signals: Arc<Signals>) -> Result<Self> {
        let packets = STOP_ADDRESSES
            .iter()
            .map(|address| {
                encode_message(&OscMessage {
                    address: (*address).into(),
                    arguments: vec![OscArg::Int(index), OscArg::Int(0)],
                })
            })
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .map_err(|_| anyhow::anyhow!("four zero commands required"))?;
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        socket.connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
        socket.set_nonblocking(true)?;
        let core = Arc::new(StopCore {
            socket,
            packets,
            armed: AtomicBool::new(false),
            signals: signals.clone(),
            journal,
            last_stop: Mutex::new(None),
        });
        *signals
            .stop
            .lock()
            .map_err(|_| anyhow::anyhow!("signal lock poisoned"))? = Some(Arc::downgrade(&core));
        Ok(Self(core))
    }
    fn arm(&self, axis: &str, kind: &str) -> Result<()> {
        self.0.journal.append(
            json!({"kind":"finite_send_intent","axis":axis,"phase":kind,"possible_send":true}),
        )?;
        self.0.armed.store(true, Ordering::SeqCst);
        if self.0.signals.cancelled.load(Ordering::SeqCst) {
            self.0.stop_now("cancelled_before_dispatch", None);
            bail!("calibration cancelled");
        }
        Ok(())
    }
    fn allowed(&self) -> bool {
        self.0.armed.load(Ordering::SeqCst) && !self.0.signals.cancelled.load(Ordering::SeqCst)
    }
    fn complete(&self) -> Result<()> {
        self.0
            .journal
            .append(json!({"kind":"original_integer_poses_restored_and_direction_validated","subdegree_or_pixel_return_verified":false}))?;
        self.0.armed.store(false, Ordering::SeqCst);
        Ok(())
    }
}
impl Drop for ZeroStopOnDrop {
    fn drop(&mut self) {
        self.0.stop_now("guard_drop", None);
    }
}

struct CameraLink {
    client: reqwest::Client,
    url: String,
    cookie: String,
}
struct Frame {
    id: u64,
    epoch: String,
    jpeg: Vec<u8>,
    face_boxes: Vec<[f64; 4]>,
    observed_at: Instant,
    age_at_observation: Duration,
}
fn visual_permit(frame: &Frame, epoch: &str, now: Instant) -> bool {
    frame.epoch == epoch
        && now
            .checked_duration_since(frame.observed_at)
            .is_some_and(|elapsed| {
                frame.age_at_observation.saturating_add(elapsed) <= CALIBRATION_VISUAL_LEASE
            })
}
impl CameraLink {
    fn open(root: &Path) -> Result<Self> {
        let descriptor: Value =
            serde_json::from_slice(&read_small(&root.join("runtime/server.json"), 64 * 1024)?)?;
        let port = descriptor["port"]
            .as_u64()
            .context("missing local server port")?;
        ensure!((1..=65535).contains(&port), "invalid local port");
        let token = descriptor["token"]
            .as_str()
            .context("missing local access token")?;
        ensure!(
            !token.is_empty() && token.bytes().all(|b| b.is_ascii_alphanumeric()),
            "invalid local access token"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()?,
            url: format!("http://127.0.0.1:{port}/api/video/frame"),
            cookie: format!("ai_tutor_local={token}"),
        })
    }
    async fn frame(&self) -> Result<Frame> {
        // Include the complete HTTP round trip conservatively in the lease;
        // receiving or parsing an old JPEG must never refresh its source age.
        let observed_at = Instant::now();
        let mut response = self
            .client
            .get(&self.url)
            .header("Cookie", &self.cookie)
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            response.content_length().unwrap_or(0) <= 8 * 1024 * 1024,
            "frame response too large"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                "frame response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        let age_at_observation = Duration::from_secs_f64(
            value["frame_age_ms"]
                .as_f64()
                .filter(|age| age.is_finite() && (0.0..=250.0).contains(age))
                .context("camera frame is stale")?
                / 1000.0,
        );
        ensure!(
            value["frame_age_ms"]
                .as_f64()
                .is_some_and(|v| v.is_finite() && (0.0..=250.0).contains(&v)),
            "camera frame is stale"
        );
        let id = value["frame_id"].as_u64().context("missing frame ID")?;
        let epoch = value["camera_session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("missing camera epoch")?
            .to_owned();
        let encoded = value["jpeg"].as_str().context("missing live frame")?;
        ensure!(encoded.len() <= 7 * 1024 * 1024, "encoded frame too large");
        let jpeg = STANDARD.decode(encoded)?;
        ensure!(
            !jpeg.is_empty() && jpeg.len() <= 5 * 1024 * 1024,
            "frame size invalid"
        );
        let dimensions =
            image::ImageReader::with_format(Cursor::new(&jpeg), image::ImageFormat::Jpeg)
                .into_dimensions()?;
        ensure!(
            dimensions == (WIDTH, HEIGHT),
            "calibration requires original 1280x720 capture geometry"
        );
        let tracks = value["tracks"]
            .as_array()
            .context("missing current face exclusion geometry")?;
        ensure!(tracks.len() <= 32, "too many face exclusion rectangles");
        let mut face_boxes = Vec::with_capacity(tracks.len());
        for track in tracks {
            let rectangle = track["box"]
                .as_array()
                .filter(|r| r.len() == 4)
                .context("invalid current face rectangle")?;
            let coordinates = rectangle
                .iter()
                .map(|v| {
                    v.as_f64()
                        .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
                })
                .collect::<Option<Vec<_>>>()
                .context("invalid normalized face rectangle")?;
            face_boxes.push([
                coordinates[0],
                coordinates[1],
                coordinates[2],
                coordinates[3],
            ]);
        }
        Ok(Frame {
            id,
            epoch,
            jpeg,
            face_boxes,
            observed_at,
            age_at_observation,
        })
    }
}
async fn pose(session: &mut OscMotionSession) -> Result<Pose> {
    Ok(session.read_pose().await?.pose)
}
async fn reached(
    session: &mut OscMotionSession,
    origin: Pose,
    pan: f32,
    tilt: f32,
    axis: &str,
) -> Result<Pose> {
    let start = Instant::now();
    loop {
        let current = pose(session).await?;
        if axis == "pan" {
            ensure!(
                current.pitch_degrees == origin.pitch_degrees,
                "uncommanded tilt drift"
            );
        }
        if axis == "tilt" {
            ensure!(
                current.yaw_degrees == origin.yaw_degrees,
                "uncommanded pan drift"
            );
        }
        ensure!(
            (current.yaw_degrees - origin.yaw_degrees).abs() <= 1
                && (current.pitch_degrees - origin.pitch_degrees).abs() <= 1,
            "unexpected position displacement"
        );
        ensure!(
            (current.yaw_degrees - origin.yaw_degrees) as f32 * (pan - origin.yaw_degrees as f32)
                >= 0.0
                && (current.pitch_degrees - origin.pitch_degrees) as f32
                    * (tilt - origin.pitch_degrees as f32)
                    >= 0.0,
            "position moved against the commanded direction"
        );
        if (current.yaw_degrees as f32 - pan).abs() < 0.1
            && (current.pitch_degrees as f32 - tilt).abs() < 0.1
        {
            return Ok(current);
        }
        ensure!(
            start.elapsed() < Duration::from_secs(4),
            "finite position did not settle"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
fn direction_sign(motion: &MotionEstimate, axis: &str, commanded: f32) -> Result<i32> {
    ensure!(motion.decidable, "scene direction could not be established");
    ensure!(
        motion.source_width == WIDTH && motion.source_height == HEIGHT,
        "optical geometry mismatch"
    );
    let (component, other) = match axis {
        "pan" => (motion.dx, motion.dy),
        "tilt" => (motion.dy, motion.dx),
        _ => bail!("unknown calibration axis"),
    };
    let component = component.context("no motion component")?;
    let other = other.context("no cross-axis component")?;
    ensure!(
        component.is_finite()
            && other.is_finite()
            && commanded.is_finite()
            && commanded.abs() == 1.0,
        "invalid direction values"
    );
    ensure!(
        component.abs() >= 0.5 && other.abs() < component.abs() * 0.6,
        "scene axis direction is ambiguous"
    );
    Ok(if component * f64::from(commanded) < 0.0 {
        1
    } else {
        -1
    })
}

/// Calibration-only assessment. It never mutates the general translation
/// estimator or static gate. The caller journals the complete assessment before
/// propagating any rejection or issuing a restoring command.
fn assess_direction(
    background: &BackgroundMotion,
    axis: &str,
    commanded: f32,
) -> (Result<i32>, Value) {
    let mut evidence = json!({
        "method":"translation_first","axis":axis,"commanded_degrees":commanded,
        "limitation":"Local direction evidence only; a bounded rigid fit does not confirm image roll as the cause or establish a full-frame motion model.",
        "roll_cause_confirmed":false,"full_frame_rigid_motion_verified":false,
        "scale":1.0,"shear":false,"patches_discarded":0
    });
    let result = (|| -> Result<i32> {
        if background.motion.decidable {
            return direction_sign(&background.motion, axis, commanded);
        }
        ensure!(
            background.motion.rejection_reason == Some(RejectionReason::PatchMotionDisagreement),
            "rigid direction fallback requires translation patch disagreement only"
        );
        evidence["method"] = json!("bounded_rigid_direction_only");
        ensure!(
            background.motion.source_width == WIDTH
                && background.motion.source_height == HEIGHT
                && background.motion.analysis_width == 160
                && background.motion.analysis_height == 90,
            "rigid direction analysis/capture geometry mismatch"
        );
        ensure!(
            ["pan", "tilt"].contains(&axis) && commanded.is_finite() && commanded.abs() == 1.0,
            "invalid rigid direction axis or commanded step"
        );
        ensure!(
            (2..=3).contains(&background.patches.len())
                && background
                    .patches
                    .iter()
                    .all(|patch| patch.decidable && patch.rejection_reason.is_none()),
            "rigid direction requires every original fixed patch quality gate to pass"
        );
        let mut points = Vec::new();
        let mut flows = Vec::new();
        for patch in &background.patches {
            let [x1, y1, x2, y2] = patch.region;
            ensure!(
                x1 >= 0 && y1 >= 0 && x1 < x2 && y1 < y2 && x2 <= 160 && y2 <= 90,
                "rigid direction ROI geometry invalid"
            );
            let flow = [
                patch.dx.context("missing patch dx")?,
                patch.dy.context("missing patch dy")?,
            ];
            ensure!(
                flow.iter().all(|n| n.is_finite()),
                "rigid direction patch flow is nonfinite"
            );
            points.push([f64::from(x1 + x2) / 2.0, f64::from(y1 + y2) / 2.0]);
            flows.push(flow);
        }
        evidence["patch_centers_analysis_pixels"] = json!(points);
        evidence["raw_patch_flows_analysis_pixels"] = json!(flows);
        let primary = if axis == "pan" { 0 } else { 1 };
        let cross = 1 - primary;
        let first = flows[0][primary];
        ensure!(
            flows
                .iter()
                .all(|flow| flow[primary].abs() >= 0.5 && flow[primary] * first > 0.0),
            "rigid direction raw patch primary flows disagree or are too small"
        );
        let count = points.len() as f64;
        let mean_p = [
            points.iter().map(|p| p[0]).sum::<f64>() / count,
            points.iter().map(|p| p[1]).sum::<f64>() / count,
        ];
        let mean_q = [
            points
                .iter()
                .zip(&flows)
                .map(|(p, v)| p[0] + v[0])
                .sum::<f64>()
                / count,
            points
                .iter()
                .zip(&flows)
                .map(|(p, v)| p[1] + v[1])
                .sum::<f64>()
                / count,
        ];
        let mut dot = 0.0;
        let mut cross_sum = 0.0;
        let mut spread = 0.0;
        for (p, v) in points.iter().zip(&flows) {
            let a = [p[0] - mean_p[0], p[1] - mean_p[1]];
            let b = [p[0] + v[0] - mean_q[0], p[1] + v[1] - mean_q[1]];
            spread += a[0] * a[0] + a[1] * a[1];
            dot += a[0] * b[0] + a[1] * b[1];
            cross_sum += a[0] * b[1] - a[1] * b[0];
        }
        ensure!(
            spread > 0.0 && dot.is_finite() && cross_sum.is_finite() && dot.hypot(cross_sum) > 0.0,
            "rigid direction fit is degenerate"
        );
        // Exact unit-scale 2-D Procrustes rotation, not a linearized angle,
        // affine transform, independent patch adjustment, or scale fit.
        let theta = cross_sum.atan2(dot);
        let (sin, cos) = theta.sin_cos();
        let rotate = |p: [f64; 2]| [cos * p[0] - sin * p[1], sin * p[0] + cos * p[1]];
        let center = [80.0, 45.0];
        let rotated_mean = rotate([mean_p[0] - center[0], mean_p[1] - center[1]]);
        let translation = [
            mean_q[0] - center[0] - rotated_mean[0],
            mean_q[1] - center[1] - rotated_mean[1],
        ];
        let mut residuals = Vec::new();
        for (p, v) in points.iter().zip(&flows) {
            let offset = [p[0] - center[0], p[1] - center[1]];
            let rotated = rotate(offset);
            residuals.push([
                v[0] - (rotated[0] - offset[0] + translation[0]),
                v[1] - (rotated[1] - offset[1] + translation[1]),
            ]);
        }
        let mut pair_differences = Vec::new();
        for i in 0..residuals.len() {
            for j in i + 1..residuals.len() {
                pair_differences.push([
                    residuals[i][0] - residuals[j][0],
                    residuals[i][1] - residuals[j][1],
                ]);
            }
        }
        evidence["rotation_degrees"] = json!(theta.to_degrees());
        evidence["rotation_limit_degrees"] = json!(1.0);
        evidence["center_analysis_pixels"] = json!(center);
        evidence["center_flow_analysis_pixels"] = json!(translation);
        evidence["patch_residuals_analysis_pixels"] = json!(residuals);
        evidence["pair_residual_differences_analysis_pixels"] = json!(pair_differences);
        evidence["pair_residual_axis_limit_pixels"] = json!(0.75);
        evidence["center_cross_primary_ratio"] =
            json!(translation[cross].abs() / translation[primary].abs());
        ensure!(
            theta.to_degrees().abs() <= 1.0,
            "rigid direction rotation exceeds one degree"
        );
        ensure!(
            pair_differences
                .iter()
                .all(|difference| difference.iter().all(|n| n.is_finite() && n.abs() <= 0.75)),
            "rigid direction compensated pair residual exceeds original limit"
        );
        ensure!(
            translation.iter().all(|n| n.is_finite())
                && translation[primary].abs() >= 0.5
                && translation[primary] * first > 0.0
                && translation[cross].abs() < translation[primary].abs() * 0.6,
            "rigid direction center flow is ambiguous or cross-axis dominated"
        );
        Ok(if translation[primary] * f64::from(commanded) < 0.0 {
            1
        } else {
            -1
        })
    })();
    evidence["accepted"] = json!(result.is_ok());
    evidence["rejection"] = json!(result.as_ref().err().map(|error| error.to_string()));
    evidence["direction_sign"] = json!(result.as_ref().ok());
    (result, evidence)
}

fn validate_static_quality(motion: &MotionEstimate) -> Result<()> {
    ensure!(
        motion.decidable,
        "static background texture/peak is unsuitable for calibration"
    );
    ensure!(
        motion.source_width == WIDTH && motion.source_height == HEIGHT,
        "static preflight capture geometry mismatch"
    );
    ensure!(
        motion
            .dx
            .is_some_and(|value| value.is_finite() && value.abs() <= 0.25)
            && motion
                .dy
                .is_some_and(|value| value.is_finite() && value.abs() <= 0.25),
        "no-motion background preflight must be stationary"
    );
    Ok(())
}

fn validate_axis_start(initial: Pose, current: Pose) -> Result<()> {
    ensure!(
        initial.yaw_degrees == current.yaw_degrees
            && initial.pitch_degrees == current.pitch_degrees,
        "axis start differs from initial canonical integer pose"
    );
    Ok(())
}

fn validate_local_pair(before: &Frame, after: &Frame, expected_epoch: &str) -> Result<()> {
    ensure!(
        before.epoch == expected_epoch && after.epoch == expected_epoch && after.id > before.id,
        "local stationary check requires distinct frames in the original camera epoch"
    );
    ensure!(
        before.age_at_observation <= Duration::from_millis(250)
            && after.age_at_observation <= Duration::from_millis(250)
            && after
                .observed_at
                .checked_duration_since(before.observed_at)
                .is_some(),
        "local stationary check requires fresh ordered observations"
    );
    Ok(())
}

/// The fixed plan is never reselected. A return to an integer device angle does
/// not prove a return to the same subdegree angle or pixels. The old anchor is
/// diagnostic only; the new local pair remains subject to the original limits.
fn journal_local_stationarity(
    journal: &Journal,
    axis: &str,
    plan: &BackgroundPlan,
    anchor: &Frame,
    before: &Frame,
    after: &Frame,
    expected_epoch: &str,
) -> Result<()> {
    let measured = validate_local_pair(before, after, expected_epoch).and_then(|()| {
        estimate_background_motion(
            plan,
            &before.jpeg,
            &after.jpeg,
            &before.face_boxes,
            &after.face_boxes,
        )
        .map_err(anyhow::Error::msg)
    });
    let residual = if anchor.epoch == expected_epoch && after.epoch == expected_epoch {
        estimate_background_motion(
            plan,
            &anchor.jpeg,
            &after.jpeg,
            &anchor.face_boxes,
            &after.face_boxes,
        )
        .map_err(anyhow::Error::msg)
    } else {
        Err(anyhow::anyhow!("global anchor epoch differs"))
    };
    // Commit numerical evidence before rejecting quality or movement. No JPEG
    // or face crop is written to this journal.
    journal.append(json!({
        "axis":axis,"kind":"fixed_roi_local_stationary_before_dispatch",
        "frame_before":before.id,"frame_after":after.id,
        "camera_epoch_before":before.epoch,"camera_epoch_after":after.epoch,
        "frame_age_before_at_observation_ms":before.age_at_observation.as_millis(),
        "frame_age_after_at_observation_ms":after.age_at_observation.as_millis(),
        "observation_interval_ms":after.observed_at.checked_duration_since(before.observed_at).map(|gap|gap.as_millis()),
        "evidence":measured.as_ref().ok(),
        "measurement_error":measured.as_ref().err().map(|error|error.to_string()),
        "global_anchor_residual_diagnostic":residual.as_ref().ok(),
        "global_anchor_diagnostic_error":residual.as_ref().err().map(|error|error.to_string()),
        "global_anchor_residual_is_gate":false,"subdegree_or_pixel_return_verified":false,
        "fixed_background_plan_reselected":false
    }))?;
    validate_static_quality(&measured?.motion)
}

struct VisualPreflight {
    report: Value,
    plan: BackgroundPlan,
    anchor: Frame,
}
async fn visual_preflight(root: &Path) -> Result<VisualPreflight> {
    let camera = CameraLink::open(root)?;
    let before = camera.frame().await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after = camera.frame().await?;
    ensure!(
        before.epoch == after.epoch && after.id > before.id,
        "preflight requires distinct frames in the same camera epoch"
    );
    let prepared = prepare_background(
        &before.jpeg,
        &after.jpeg,
        &before.face_boxes,
        &after.face_boxes,
    )
    .map_err(anyhow::Error::msg)?;
    let check = validate_static_quality(&prepared.quality.motion);
    let report = json!({"at":chrono::Local::now().to_rfc3339(),"result":if check.is_ok()&&prepared.plan.is_some(){"PASS"}else{"BLOCKED_NO_MOTION"},"frame_before":before.id,"frame_after":after.id,"camera_session_id":after.epoch,"identical_frame_only":false,"quality":prepared.quality.motion,"dynamic_background":prepared.quality,"fixed_background_plan":prepared.plan,"error":check.as_ref().err().map(|error|error.to_string()),"device_commands_sent":0,"raw_frames_saved":false,"latch_created":false});
    let runtime = root.join("runtime").canonicalize()?;
    ensure!(
        runtime.starts_with(root),
        "runtime must remain in workspace"
    );
    // This is an explicitly latest no-motion quality snapshot, not a motion
    // report or retry latch. It may be refreshed after changing the background.
    let mut file = File::create(runtime.join("ptz-visual-preflight.json"))?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.sync_all()?;
    check?;
    Ok(VisualPreflight {
        report,
        plan: prepared
            .plan
            .context("two independent stable background patches required")?,
        anchor: after,
    })
}

async fn run(
    root: &Path,
    config: CalibrationConfig,
    events: &mut Vec<Value>,
    journal: Arc<Journal>,
    signals: Arc<Signals>,
    preflight: &VisualPreflight,
) -> Result<Value> {
    let camera = CameraLink::open(root)?;
    let first_epoch = camera.frame().await?.epoch;
    ensure!(
        first_epoch == preflight.anchor.epoch,
        "camera reconnected after dynamic preflight"
    );
    let mut first_session = Some(OscMotionSession::verify_for_calibration(config.clone()).await?);
    let initial_pose = first_session.as_ref().unwrap().verification().baseline_pose;
    journal.append(json!({"kind":"initial_canonical_integer_pose","pose":initial_pose,"subdegree_or_pixel_return_verified":false}))?;
    // Independent writer construction follows the real read-only session gate.
    let guard = ZeroStopOnDrop::after_verified(
        first_session.as_ref().unwrap(),
        &config,
        journal.clone(),
        signals,
    )?;
    let mut signs = Vec::new();
    for (axis, delta_pan, delta_tilt) in [("pan", 1.0f32, 0.0f32), ("tilt", 0.0f32, -1.0f32)] {
        let mut session = if let Some(session) = first_session.take() {
            session
        } else {
            OscMotionSession::verify_for_calibration(config.clone()).await?
        };
        let original = session.verification().baseline_pose;
        journal.append(json!({"axis":axis,"kind":"axis_canonical_integer_pose_before_dispatch","initial_pose":initial_pose,"current_pose":original}))?;
        validate_axis_start(initial_pose, original)?;
        let local_before = camera.frame().await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let before = camera.frame().await?;
        journal_local_stationarity(
            &journal,
            axis,
            &preflight.plan,
            &preflight.anchor,
            &local_before,
            &before,
            &first_epoch,
        )?;
        let attempt=async {
            guard.arm(axis,"outbound")?;
            events.push(json!({"axis":axis,"kind":"outbound_possible_send"}));
            let moved=session.finite_step_guarded(delta_pan,delta_tilt,&||guard.allowed()&&visual_permit(&before,&first_epoch,Instant::now())).await?;
            journal.append(json!({"axis":axis,"step":moved}))?;
            events.push(json!({"axis":axis,"step":moved}));
            reached(&mut session,original,moved.target_pan_degrees,moved.target_tilt_degrees,axis).await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let after=camera.frame().await?;
            ensure!(after.epoch==before.epoch&&after.id>before.id,"camera epoch/frame changed unexpectedly");
            let background=estimate_background_motion(&preflight.plan,&before.jpeg,&after.jpeg,&before.face_boxes,&after.face_boxes).map_err(anyhow::Error::msg)?;
            let motion=&background.motion;
            journal.append(json!({"axis":axis,"frame_before":before.id,"frame_after":after.id,"motion":motion,"dynamic_background":background}))?;
            events.push(json!({"axis":axis,"frame_before":before.id,"frame_after":after.id,"motion":motion,"dynamic_background":background}));
            // Every optical/sign check precedes ANY restoring target. Rejected
            // evidence immediately stops and fails, with no compensating move.
            let commanded=if axis=="pan"{delta_pan}else{delta_tilt};
            let (direction,direction_evidence)=assess_direction(&background,axis,commanded);
            journal.append(json!({"axis":axis,"kind":"local_direction_assessment","evidence":direction_evidence}))?;
            events.push(json!({"axis":axis,"kind":"local_direction_assessment","evidence":direction_evidence}));
            let sign=direction?;
            journal.append(json!({"axis":axis,"direction_sign":sign,"optical_validation":"PASS_BEFORE_RESTORE"}))?;
            let mut restore=OscMotionSession::verify_for_calibration(config.clone()).await?;
            let observed=restore.verification().baseline_pose;
            ensure!(observed.yaw_degrees as f32==moved.target_pan_degrees&&observed.pitch_degrees as f32==moved.target_tilt_degrees,"pose drift before restore");
            let restore_frame=camera.frame().await?;
            ensure!(restore_frame.epoch==first_epoch&&restore_frame.id>=after.id,"camera epoch/frame changed before restore");
            guard.arm(axis,"restore")?;
            events.push(json!({"axis":axis,"kind":"restore_possible_send"}));
            let restored=restore.finite_step_guarded(-delta_pan,-delta_tilt,&||guard.allowed()&&visual_permit(&restore_frame,&first_epoch,Instant::now())).await?;
            journal.append(json!({"axis":axis,"restore":restored}))?;
            events.push(json!({"axis":axis,"restore":restored}));
            reached(&mut restore,observed,restored.target_pan_degrees,restored.target_tilt_degrees,axis).await?;
            let current=pose(&mut restore).await?;
            ensure!(current.yaw_degrees==original.yaw_degrees&&current.pitch_degrees==original.pitch_degrees,"original integer pose not restored");
            journal.append(json!({"axis":axis,"kind":"integer_pose_restored","initial_pose":initial_pose,"original_pose":original,"current_pose":current,"subdegree_or_pixel_return_verified":false}))?;
            Ok::<i32,anyhow::Error>(sign)
        }.await;
        match attempt {
            Ok(sign) => signs.push(sign),
            Err(error) => {
                guard.0.stop_now("attempt_error", None);
                let stop = session.stop().await.map_err(|e| e.to_string());
                let all_submitted = stop
                    .as_ref()
                    .is_ok_and(|receipt| receipt.all_zero_commands_sent);
                let event = json!({"axis":axis,"stop":stop,"all_async_zero_commands_sent":all_submitted,"physical_stop_verified":false,"result":"FAIL","error":error.to_string(),"motion_error_details":motion_error_details(&error)});
                journal.emergency_append(&event);
                events.push(event);
                return Err(error);
            }
        }
    }
    guard.complete()?;
    Ok(
        json!({"pan_sign":signs[0],"tilt_sign":signs[1],"camera_session_id":first_epoch,"direction_verified":true,"physical_follow_verified":false,"loss_and_shutdown_stop_verified":false}),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .context("root, server port, device index, exact observed device name required")?,
    )
    .canonicalize()?;
    let server_port = args.next().context("server port required")?.parse()?;
    let device_index = args.next().context("device index required")?.parse()?;
    let expected_name = args.next().context("exact observed device name required")?;
    ensure!(args.next().is_none(), "unexpected argument");
    let config = CalibrationConfig {
        server_port,
        reply_port: 0,
        command_bind_port: 0,
        device_index,
        expected_name,
        query_timeout: Duration::from_millis(750),
        gimbal_profile: GimbalProfile::Tiny3Center2015PanPitch,
        canonical_root: Some(root.clone()),
    };
    ensure!(
        server_port != 0 && device_index == 0 && config.expected_name == "Tiny 3",
        "explicit sole Tiny 3 endpoint required"
    );
    let (binding, confirmation) = local_preflight(&root, &config)?;
    let visual_quality = visual_preflight(&root).await?;
    let mut reservation = Reservation::create(&root, binding.clone(), confirmation.clone())?;
    reservation
        .journal
        .append(json!({"kind":"no_motion_visual_preflight","evidence":visual_quality.report}))?;
    let signals = Arc::new(Signals::default());
    let _console = ConsoleHandler::install(signals.clone())?;
    let mut events = Vec::new();
    let result = tokio::select! {
        biased;
        _=signals.wait()=>Err(anyhow::anyhow!("calibration cancelled by console signal {}",signals.code.load(Ordering::SeqCst))),
        result=run(&root,config,&mut events,reservation.journal.clone(),signals.clone(),&visual_quality)=>result,
    };
    let status = if result.is_ok() {
        "PASS_DIRECTION_ONLY"
    } else if signals.cancelled.load(Ordering::SeqCst) {
        "CANCELLED_NO_RETRY"
    } else if events.is_empty() {
        "BLOCKED_PRECONDITIONS_NO_RETRY"
    } else {
        "FAIL_NO_RETRY"
    };
    let report = json!({"at":chrono::Local::now().to_rfc3339(),"run_id":reservation.initial["run_id"],"implementation":"Rust / official OBSBOT Center OSC finite calibration v2 dynamic background","result":status,"binding":binding,"operator_confirmation":confirmation,"calibration":result.as_ref().ok(),"error":result.as_ref().err().map(|e|e.to_string()),"events":events,"calibration_visual_lease_ms":CALIBRATION_VISUAL_LEASE.as_millis(),"visual_lease_limitation":"A bounded last observation, not a live camera-close watchdog; production following has its own stricter 250ms requirement.","journal":"runtime/ptz-calibration-rust.journal.jsonl","latch":"runtime/ptz-calibration-rust.latch.json","automatic_retry_allowed":false,"raw_frames_saved":false,"continuous_velocity_commands":false,"automatically_enables_follow":false,"physical_stop_verified":false,"stop_limitation":STOP_LIMITATION});
    reservation
        .journal
        .append(json!({"kind":"final_result","result":status}))?;
    reservation.finish(&report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if result.is_err() {
        bail!("calibration did not pass; persistent latch retained and automatic retry forbidden");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_tutor::frame_motion::estimate_motion;
    #[test]
    fn visual_dispatch_permit_rejects_stale_epoch_or_future_observation() {
        let now = Instant::now();
        let frame = Frame {
            id: 1,
            epoch: "epoch-a".into(),
            jpeg: vec![],
            face_boxes: vec![],
            observed_at: now,
            age_at_observation: Duration::from_millis(200),
        };
        assert!(visual_permit(
            &frame,
            "epoch-a",
            now + Duration::from_millis(799)
        ));
        assert!(!visual_permit(
            &frame,
            "epoch-a",
            now + Duration::from_millis(801)
        ));
        assert!(!visual_permit(&frame, "epoch-b", now));
        assert!(!visual_permit(
            &frame,
            "epoch-a",
            now - Duration::from_millis(1)
        ));
    }
    use ai_tutor::obsbot_osc::decode_message;
    fn journal(dir: &tempfile::TempDir) -> Arc<Journal> {
        Arc::new(Journal(Mutex::new(
            OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(dir.path().join("journal.jsonl"))
                .unwrap(),
        )))
    }
    fn fixture_guard(dir: &tempfile::TempDir) -> (UdpSocket, ZeroStopOnDrop, Arc<Signals>) {
        let fixture = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        fixture
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let signals = Arc::new(Signals::default());
        let guard = ZeroStopOnDrop::new(
            fixture.local_addr().unwrap().port(),
            0,
            journal(dir),
            signals.clone(),
        )
        .unwrap();
        (fixture, guard, signals)
    }
    fn assert_four_zeros(socket: &UdpSocket) {
        let mut buffer = [0u8; 4096];
        for address in STOP_ADDRESSES {
            let bytes = socket.recv(&mut buffer).unwrap();
            let packet = decode_message(&buffer[..bytes]).unwrap();
            assert_eq!(packet.address, address);
            assert_eq!(packet.arguments, vec![OscArg::Int(0), OscArg::Int(0)]);
        }
        assert!(socket.recv(&mut buffer).is_err());
    }
    #[test]
    fn armed_drop_submits_exactly_four_zero_only_commands_and_journals() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, guard, _) = fixture_guard(&dir);
        guard.arm("pan", "fixture").unwrap();
        drop(guard);
        assert_four_zeros(&socket);
        let evidence = std::fs::read_to_string(dir.path().join("journal.jsonl")).unwrap();
        assert!(
            evidence.contains("synchronous_zero_stop")
                && evidence.contains("\"commands_attempted\":4")
        );
        assert!(evidence.contains("\"physical_stop_verified\":false"));
    }
    #[test]
    fn console_signal_stops_before_drop_and_forbids_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, guard, signals) = fixture_guard(&dir);
        guard.arm("pan", "fixture").unwrap();
        signals.trigger(2);
        assert!(!guard.allowed());
        assert_four_zeros(&socket);
        drop(guard);
        assert!(socket.recv(&mut [0; 256]).is_err());
    }
    #[tokio::test]
    async fn cancelled_future_drops_armed_guard_and_sends_only_four_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, guard, _) = fixture_guard(&dir);
        guard.arm("pan", "fixture").unwrap();
        let task = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_four_zeros(&socket);
    }
    #[test]
    fn unwinding_error_drops_armed_guard_before_return() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, guard, _) = fixture_guard(&dir);
        guard.arm("pan", "fixture").unwrap();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = guard;
            panic!("offline fixture panic");
        }));
        assert!(caught.is_err());
        assert_four_zeros(&socket);
    }
    #[test]
    fn unarmed_or_successfully_completed_guard_sends_nothing() {
        for complete in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (socket, guard, _) = fixture_guard(&dir);
            if complete {
                guard.arm("pan", "fixture").unwrap();
                guard.complete().unwrap();
            }
            drop(guard);
            assert!(socket.recv(&mut [0u8; 256]).is_err());
        }
    }
    #[test]
    fn reservation_survives_failure_and_preserves_existing_evidence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("runtime")).unwrap();
        let root = dir.path().canonicalize().unwrap();
        let reservation = Reservation::create(&root, json!({"test":1}), json!({})).unwrap();
        let first = std::fs::read(root.join("runtime/ptz-calibration-rust.json")).unwrap();
        drop(reservation);
        assert!(Reservation::create(&root, json!({"test":2}), json!({})).is_err());
        assert_eq!(
            first,
            std::fs::read(root.join("runtime/ptz-calibration-rust.json")).unwrap()
        );
        assert!(
            root.join("runtime/ptz-calibration-rust.latch.json")
                .exists()
        );
    }
    #[test]
    fn master_off_and_exact_binding_are_mandatory() {
        let config = CalibrationConfig {
            server_port: 16284,
            reply_port: 0,
            command_bind_port: 0,
            device_index: 0,
            expected_name: "Tiny 3".into(),
            query_timeout: Duration::from_millis(100),
            gimbal_profile: GimbalProfile::Tiny3Center2015PanPitch,
            canonical_root: None,
        };
        let confirmed = json!({"server_port":16284,"device_index":0,"expected_name":"Tiny 3","gimbal_profile":PROFILE,"camera_device_id":"exact-path","operator_confirmation":{"vendor_ai_tracking_master_off":true,"vendor_ai_mode_master_off":true,"source_type":"user_confirmation","at":"fixture"}});
        validate_operator_config(&confirmed, &config, "exact-path").unwrap();
        for (field, bad) in [
            ("server_port", json!(1)),
            ("expected_name", json!("other")),
            ("gimbal_profile", json!("guess")),
            ("camera_device_id", json!("other")),
        ] {
            let mut value = confirmed.clone();
            value[field] = bad;
            assert!(validate_operator_config(&value, &config, "exact-path").is_err());
        }
        for field in ["vendor_ai_tracking_master_off", "vendor_ai_mode_master_off"] {
            let mut value = confirmed.clone();
            value["operator_confirmation"][field] = json!(false);
            assert!(validate_operator_config(&value, &config, "exact-path").is_err());
        }
    }
    #[test]
    fn static_preflight_accepts_textured_zero_motion_but_rejects_flat_background() {
        let jpeg = |flat: bool| {
            let image = image::GrayImage::from_fn(WIDTH, HEIGHT, |x, y| {
                let mut value = (x / 8 + (y / 8) * 997).wrapping_add(0x1937);
                value ^= value >> 16;
                value = value.wrapping_mul(0x7feb352d);
                value ^= value >> 15;
                image::Luma([if flat { 128 } else { value as u8 }])
            });
            let mut bytes = Cursor::new(Vec::new());
            image::DynamicImage::ImageLuma8(image)
                .write_to(&mut bytes, image::ImageFormat::Jpeg)
                .unwrap();
            bytes.into_inner()
        };
        let texture = jpeg(false);
        let flat = jpeg(true);
        let textured_quality = estimate_motion(&texture, &texture).unwrap();
        validate_static_quality(&textured_quality).unwrap();
        assert!(validate_static_quality(&estimate_motion(&flat, &flat).unwrap()).is_err());
    }
    fn local_fixture_frame(shift: u32, id: u64, epoch: &str, observed_at: Instant) -> Frame {
        let image = image::GrayImage::from_fn(WIDTH, HEIGHT, |x, y| {
            let mut value = (x.saturating_sub(shift) / 8 + (y / 8) * 997).wrapping_add(0x1937);
            value ^= value >> 16;
            value = value.wrapping_mul(0x7feb352d);
            value ^= value >> 15;
            image::Luma([value as u8])
        });
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        Frame {
            id,
            epoch: epoch.into(),
            jpeg: jpeg.into_inner(),
            face_boxes: vec![],
            observed_at,
            age_at_observation: Duration::from_millis(20),
        }
    }
    #[test]
    fn local_stationary_pair_accepts_static_residual_without_reselecting_fixed_regions() {
        let dir = tempfile::tempdir().unwrap();
        let journal = journal(&dir);
        let time = Instant::now();
        let anchor = local_fixture_frame(0, 1, "epoch", time);
        let plan = prepare_background(&anchor.jpeg, &anchor.jpeg, &[], &[])
            .unwrap()
            .plan
            .unwrap();
        let regions = plan.regions.clone();
        let before = local_fixture_frame(8, 2, "epoch", time + Duration::from_millis(250));
        let after = local_fixture_frame(8, 3, "epoch", time + Duration::from_millis(500));
        journal_local_stationarity(&journal, "tilt", &plan, &anchor, &before, &after, "epoch")
            .unwrap();
        let entry: Value = serde_json::from_str(
            std::fs::read_to_string(dir.path().join("journal.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(plan.regions, regions);
        assert!(
            entry["event"]["evidence"]["motion"]["dx"]
                .as_f64()
                .unwrap()
                .abs()
                <= 0.25
        );
        assert!(
            entry["event"]["global_anchor_residual_diagnostic"]["motion"]["dx"]
                .as_f64()
                .unwrap()
                .abs()
                > 0.5
        );
        assert_eq!(entry["event"]["global_anchor_residual_is_gate"], false);
        assert_eq!(entry["event"]["fixed_background_plan_reselected"], false);
        assert_eq!(entry["event"]["subdegree_or_pixel_return_verified"], false);
    }
    #[test]
    fn moving_local_pair_rejects_and_journals_numeric_evidence_before_validation() {
        let dir = tempfile::tempdir().unwrap();
        let journal = journal(&dir);
        let time = Instant::now();
        let anchor = local_fixture_frame(0, 1, "epoch", time);
        let plan = prepare_background(&anchor.jpeg, &anchor.jpeg, &[], &[])
            .unwrap()
            .plan
            .unwrap();
        let before = local_fixture_frame(8, 2, "epoch", time + Duration::from_millis(250));
        let after = local_fixture_frame(16, 3, "epoch", time + Duration::from_millis(500));
        assert!(
            journal_local_stationarity(&journal, "tilt", &plan, &anchor, &before, &after, "epoch")
                .is_err()
        );
        let entry: Value = serde_json::from_str(
            std::fs::read_to_string(dir.path().join("journal.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert!(
            entry["event"]["evidence"]["motion"]["dx"]
                .as_f64()
                .unwrap()
                .abs()
                > 0.25
        );
        assert!(
            entry["event"]["evidence"]["patches"]
                .as_array()
                .unwrap()
                .len()
                >= 2
        );
    }
    #[test]
    fn local_pair_rejects_epoch_changes_reused_frames_and_stale_observations() {
        let time = Instant::now();
        let before = local_fixture_frame(0, 1, "epoch", time);
        let mut after = local_fixture_frame(0, 2, "other", time + Duration::from_millis(250));
        assert!(validate_local_pair(&before, &after, "epoch").is_err());
        after.epoch = "epoch".into();
        assert!(validate_local_pair(&before, &after, "epoch").is_ok());
        after.id = 1;
        assert!(validate_local_pair(&before, &after, "epoch").is_err());
        after.id = 2;
        after.age_at_observation = Duration::from_millis(251);
        assert!(validate_local_pair(&before, &after, "epoch").is_err());
        after.age_at_observation = Duration::from_millis(20);
        after.observed_at = time - Duration::from_millis(1);
        assert!(validate_local_pair(&before, &after, "epoch").is_err());
    }
    #[test]
    fn every_axis_requires_exact_initial_canonical_integer_pose() {
        let initial = Pose {
            roll_unused_degrees: 0,
            pitch_degrees: -24,
            yaw_degrees: 17,
        };
        assert!(validate_axis_start(initial, initial).is_ok());
        assert!(
            validate_axis_start(
                initial,
                Pose {
                    yaw_degrees: 18,
                    ..initial
                }
            )
            .is_err()
        );
        assert!(
            validate_axis_start(
                initial,
                Pose {
                    pitch_degrees: -23,
                    ..initial
                }
            )
            .is_err()
        );
    }
    fn rigid_direction_fixture(flows: [[f64; 2]; 2]) -> BackgroundMotion {
        let frame = local_fixture_frame(0, 1, "synthetic", Instant::now());
        let mut background = prepare_background(&frame.jpeg, &frame.jpeg, &[], &[])
            .unwrap()
            .quality;
        assert_eq!(background.patches.len(), 2);
        background.motion.decidable = false;
        background.motion.rejection_reason = Some(RejectionReason::PatchMotionDisagreement);
        background.motion.dx = None;
        background.motion.dy = None;
        for (index, patch) in background.patches.iter_mut().enumerate() {
            patch.region = if index == 0 {
                [14, 26, 54, 50]
            } else {
                [106, 50, 146, 74]
            };
            patch.dx = Some(flows[index][0]);
            patch.dy = Some(flows[index][1]);
            patch.decidable = true;
            patch.rejection_reason = None;
        }
        background
    }
    fn exact_rigid_flows(degrees: f64, translation: [f64; 2]) -> [[f64; 2]; 2] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        [[34.0 - 80.0, 38.0 - 45.0], [126.0 - 80.0, 62.0 - 45.0]].map(|p| {
            [
                cos * p[0] - sin * p[1] - p[0] + translation[0],
                sin * p[0] + cos * p[1] - p[1] + translation[1],
            ]
        })
    }
    #[test]
    fn bounded_rigid_numeric_fixture_is_compatible_but_does_not_reclassify_old_failure() {
        // Offline numeric fixture only. The recorded physical attempt remains
        // FAIL; absent images cannot confirm a rotation cause retroactively.
        let mut background = rigid_direction_fixture([
            [1.2370965273040802, 1.1639752269432888],
            [1.796980237429239, 0.3434317162718218],
        ]);
        background.patches[0].similarity = Some(0.9985822742606777);
        background.patches[0].peak_margin = Some(0.0509462649062411);
        background.patches[1].similarity = Some(0.9815753667385815);
        background.patches[1].peak_margin = Some(0.07444977817419729);
        let (sign, evidence) = assess_direction(&background, "pan", 1.0);
        assert_eq!(sign.unwrap(), -1);
        assert_eq!(evidence["method"], "bounded_rigid_direction_only");
        assert!(evidence["rotation_degrees"].as_f64().unwrap().abs() < 1.0);
        assert!(evidence["center_cross_primary_ratio"].as_f64().unwrap() < 0.6);
        assert_eq!(evidence["roll_cause_confirmed"], false);
        assert_eq!(evidence["patches_discarded"], 0);
        assert!(!background.motion.decidable);
        assert_eq!(
            background.motion.rejection_reason,
            Some(RejectionReason::PatchMotionDisagreement)
        );
    }
    #[test]
    fn exact_rigid_synthetic_rotation_passes_with_unit_scale_and_pair_residual_bound() {
        for (axis, command, translation) in
            [("pan", 1.0, [2.0, 0.05]), ("tilt", -1.0, [0.05, -2.0])]
        {
            let background = rigid_direction_fixture(exact_rigid_flows(0.5, translation));
            let (sign, evidence) = assess_direction(&background, axis, command);
            assert_eq!(sign.unwrap(), -1);
            assert!((evidence["rotation_degrees"].as_f64().unwrap() - 0.5).abs() < 1e-8);
            let residual = evidence["pair_residual_differences_analysis_pixels"][0]
                .as_array()
                .unwrap();
            assert!(residual.iter().all(|n| n.as_f64().unwrap().abs() < 1e-10));
            assert_eq!(evidence["scale"], 1.0);
            assert_eq!(evidence["shear"], false);
            assert_eq!(evidence["pair_residual_axis_limit_pixels"], 0.75);
        }
    }
    #[test]
    fn normal_translation_keeps_original_direction_path() {
        let mut background = rigid_direction_fixture([[1.0, 0.0], [1.0, 0.0]]);
        background.motion.decidable = true;
        background.motion.rejection_reason = None;
        background.motion.dx = Some(1.0);
        background.motion.dy = Some(0.0);
        let (sign, evidence) = assess_direction(&background, "pan", 1.0);
        assert_eq!(sign.unwrap(), -1);
        assert_eq!(evidence["method"], "translation_first");
        assert!(evidence["rotation_degrees"].is_null());
    }
    #[test]
    fn rigid_fallback_rejects_reverse_flow_large_rotation_and_pair_residual_not_per_point() {
        let opposite = rigid_direction_fixture([[1.0, 0.0], [-1.0, 0.0]]);
        assert!(
            assess_direction(&opposite, "pan", 1.0)
                .0
                .unwrap_err()
                .to_string()
                .contains("raw patch primary")
        );
        let big = rigid_direction_fixture(exact_rigid_flows(1.25, [2.0, 0.0]));
        let (check, evidence) = assess_direction(&big, "pan", 1.0);
        assert!(check.unwrap_err().to_string().contains("rotation exceeds"));
        assert!(evidence["rotation_degrees"].as_f64().unwrap() > 1.0);
        // Residuals of opposite sign are individually <.75 but their pair
        // difference is >.75. A per-point threshold would incorrectly accept.
        let disagreement = rigid_direction_fixture([[1.0, 0.0], [2.0, 0.0]]);
        let (check, evidence) = assess_direction(&disagreement, "pan", 1.0);
        assert!(
            check
                .unwrap_err()
                .to_string()
                .contains("compensated pair residual")
        );
        assert!(
            evidence["patch_residuals_analysis_pixels"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|r| r.as_array().unwrap())
                .all(|n| n.as_f64().unwrap().abs() < 0.75)
        );
        assert!(
            evidence["pair_residual_differences_analysis_pixels"][0][0]
                .as_f64()
                .unwrap()
                .abs()
                > 0.75
        );
    }
    #[test]
    fn scene_or_patch_quality_failure_and_cross_axis_dominance_cannot_use_rigid_fallback() {
        let mut changed = rigid_direction_fixture(exact_rigid_flows(0.5, [2.0, 0.0]));
        changed.motion.rejection_reason = Some(RejectionReason::SceneChanged);
        assert!(
            assess_direction(&changed, "pan", 1.0)
                .0
                .unwrap_err()
                .to_string()
                .contains("patch disagreement only")
        );
        changed.motion.rejection_reason = Some(RejectionReason::PatchMotionDisagreement);
        changed.patches[0].decidable = false;
        changed.patches[0].rejection_reason = Some(RejectionReason::AmbiguousPeak);
        assert!(
            assess_direction(&changed, "pan", 1.0)
                .0
                .unwrap_err()
                .to_string()
                .contains("every original fixed patch")
        );
        let cross = rigid_direction_fixture(exact_rigid_flows(0.5, [1.0, 1.0]));
        let (check, evidence) = assess_direction(&cross, "pan", 1.0);
        assert!(
            check
                .unwrap_err()
                .to_string()
                .contains("cross-axis dominated")
        );
        assert!(evidence["center_cross_primary_ratio"].as_f64().unwrap() > 0.6);
    }
    #[test]
    fn undecidable_cross_axis_nonfinite_optics_reject_before_restore_gate() {
        let image = image::GrayImage::from_fn(WIDTH, HEIGHT, |x, y| {
            image::Luma([((x * 17 + y * 37 + (x * y) % 97) % 256) as u8])
        });
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        let mut motion = estimate_motion(jpeg.get_ref(), jpeg.get_ref()).unwrap();
        motion.decidable = true;
        motion.dx = Some(-2.0);
        motion.dy = Some(0.0);
        assert_eq!(direction_sign(&motion, "pan", 1.0).unwrap(), 1);
        motion.decidable = false;
        assert!(direction_sign(&motion, "pan", 1.0).is_err());
        motion.decidable = true;
        motion.dy = Some(1.5);
        assert!(direction_sign(&motion, "pan", 1.0).is_err());
        motion.dy = Some(0.0);
        motion.dx = Some(f64::NAN);
        assert!(direction_sign(&motion, "pan", 1.0).is_err());
        motion.dx = Some(2.0);
        assert_eq!(direction_sign(&motion, "pan", 1.0).unwrap(), -1);
    }
}

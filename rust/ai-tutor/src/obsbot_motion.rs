//! Temporary, single-step OBSBOT Center calibration authority. No automatic
//! follow controller, AI switching, continuous-speed API, or motion CLI exists.
//! The official 2026-05-08 OSC table rows 20-26 define the only writable
//! addresses below; none is sent during construction/verification.
use crate::native_canonical::{NativeCanonicalEvidence, read_native_canonical};
use crate::obsbot_osc::{
    GimbalProfile, OscArg, OscMessage, OscReadClient, ReadQuery, ReadReceipt, ReadState,
    encode_message,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::Value;
use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    time::{Instant, sleep, timeout, timeout_at},
};

pub const AUTHORITY_TTL: Duration = Duration::from_secs(10);
const VERIFICATION_DEADLINE: Duration = Duration::from_secs(8);
const SAMPLE_GAP: Duration = Duration::from_millis(100);
const SEND_DEADLINE: Duration = Duration::from_millis(250);
const STOP_COMMAND_DEADLINE: Duration = Duration::from_millis(50);
const ABSOLUTE_ADDRESS: &str = "/OBSBOT/WebCam/General/SetGimMotorDegreeEx";
#[derive(Clone, Copy)]
enum FiniteProfile {
    Calibration,
    Follow,
}
impl FiniteProfile {
    fn limit(self) -> f32 {
        match self {
            Self::Calibration => 1.0,
            Self::Follow => 3.0,
        }
    }
    fn speed(self) -> f32 {
        match self {
            Self::Calibration => 1.0,
            Self::Follow => 4.0,
        }
    }
    fn limit_error(self) -> &'static str {
        match self {
            Self::Calibration => "osc_motion_finite_step_limit",
            Self::Follow => "osc_motion_follow_step_limit",
        }
    }
}
const STOP_ADDRESSES: [&str; 4] = [
    "/OBSBOT/WebCam/General/SetGimbalUp",
    "/OBSBOT/WebCam/General/SetGimbalDown",
    "/OBSBOT/WebCam/General/SetGimbalLeft",
    "/OBSBOT/WebCam/General/SetGimbalRight",
];

/// No Default: callers must explicitly select both UDP bindings, endpoint,
/// exact observed name and device index. A port value of 0 explicitly requests
/// an ephemeral local binding, never a guessed vendor port.
#[derive(Clone, Debug)]
pub struct CalibrationConfig {
    pub server_port: u16,
    pub reply_port: u16,
    pub command_bind_port: u16,
    pub device_index: i32,
    pub expected_name: String,
    pub query_timeout: Duration,
    pub gimbal_profile: GimbalProfile,
    /// Explicit opt-in. None preserves the original strict OSC-only contract.
    pub canonical_root: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Pose {
    pub roll_unused_degrees: i32,
    pub pitch_degrees: i32,
    pub yaw_degrees: i32,
}

impl Pose {
    fn from_state(state: &ReadState) -> Result<Self> {
        match *state {
            ReadState::GimbalPosition {
                roll_unused_degrees,
                pitch_degrees,
                yaw_degrees,
            } => Ok(Self {
                roll_unused_degrees,
                pitch_degrees,
                yaw_degrees,
            }),
            _ => bail!("osc_motion_pose_reply_required"),
        }
    }

    fn same_axes(self, other: Self) -> bool {
        self.pitch_degrees == other.pitch_degrees && self.yaw_degrees == other.yaw_degrees
    }

    fn in_command_range(self) -> bool {
        (-129..=129).contains(&self.yaw_degrees) && (-59..=59).contains(&self.pitch_degrees)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CalibrationProof {
    pub verified_at: String,
    pub authority_ttl_ms: u128,
    pub maximum_finite_steps: usize,
    pub expected_name: String,
    pub device_index: i32,
    pub baseline_pose: Pose,
    pub source_type: &'static str,
    pub read_receipts: Vec<ReadReceipt>,
    pub canonical_pose_evidence: Vec<NativeCanonicalEvidence>,
    pub limitation: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct FiniteStepReceipt {
    pub device_index: i32,
    pub before_pose: Pose,
    pub target_pan_degrees: f32,
    pub target_tilt_degrees: f32,
    pub speed: f32,
    pub udp_send_accepted: bool,
    pub authority_consumed: bool,
    pub physical_motion_verified: bool,
    pub fresh_read_receipts: Vec<ReadReceipt>,
    pub canonical_pose_evidence: Option<NativeCanonicalEvidence>,
}

/// Structured, pre-dispatch rejection. The stable Display code is retained for
/// existing callers; diagnostics may downcast or call motion_error_details().
#[derive(Clone, Debug, Serialize)]
pub struct PoseChangedBeforeDispatch {
    pub code: &'static str,
    pub baseline_pose: Pose,
    pub current_pose: Pose,
    pub baseline_raw_poses: Vec<OscMessage>,
    pub current_raw_pose: OscMessage,
    pub current_source: SocketAddr,
    pub gimbal_profile: GimbalProfile,
    pub nonzero_commands_sent: usize,
    pub canonical_baseline_evidence: Vec<NativeCanonicalEvidence>,
    pub canonical_current_evidence: Option<NativeCanonicalEvidence>,
}

impl std::fmt::Display for PoseChangedBeforeDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for PoseChangedBeforeDispatch {}

pub fn motion_error_details(error: &anyhow::Error) -> Option<Value> {
    if let Some(details) = error.downcast_ref::<PoseChangedBeforeDispatch>() {
        return serde_json::to_value(details).ok();
    }
    error
        .downcast_ref::<CanonicalConsistencyRejected>()
        .and_then(|details| serde_json::to_value(details).ok())
}

#[derive(Clone, Debug, Serialize)]
pub struct CanonicalConsistencyRejected {
    pub code: &'static str,
    pub osc_pose: Pose,
    pub canonical: NativeCanonicalEvidence,
    pub nonzero_commands_sent: usize,
}
impl std::fmt::Display for CanonicalConsistencyRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for CanonicalConsistencyRejected {}

fn confirm_consistency(osc: Pose, canonical: &NativeCanonicalEvidence) -> Result<Pose> {
    let pose = canonical.pose;
    if (i64::from(osc.yaw_degrees) - i64::from(pose.yaw_degrees)).abs() > 1
        || (i64::from(osc.pitch_degrees) - i64::from(pose.pitch_degrees)).abs() > 1
    {
        return Err(CanonicalConsistencyRejected {
            code: "osc_motion_canonical_consistency_rejected",
            osc_pose: osc,
            canonical: canonical.clone(),
            nonzero_commands_sent: 0,
        }
        .into());
    }
    // OSC integer uncertainty is evidence only. It never changes this target base.
    Ok(pose)
}

async fn referenced_pose(
    config: &CalibrationConfig,
    receipt: &ReadReceipt,
) -> Result<(Pose, Option<NativeCanonicalEvidence>)> {
    let osc = Pose::from_state(&receipt.reply.state)?;
    match &config.canonical_root {
        None => Ok((osc, None)),
        Some(root) => {
            ensure!(
                config.gimbal_profile == GimbalProfile::Tiny3Center2015PanPitch,
                "osc_motion_canonical_profile_required"
            );
            let canonical = read_native_canonical(root).await?;
            let pose = confirm_consistency(osc, &canonical)?;
            Ok((pose, Some(canonical)))
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StopCommandResult {
    pub direction_index: usize,
    pub address: &'static str,
    pub attempted: bool,
    /// 0=full UDP send, 1=I/O error, 2=per-command timeout,
    /// 3=total deadline exhausted before attempt, 4=partial send,
    /// 5=encoding rejected. None of these codes proves physical stopping.
    pub result_code: i32,
    pub bytes_sent: usize,
    pub expected_bytes: usize,
    pub os_error: Option<i32>,
    pub elapsed_ms: u128,
}

#[derive(Clone, Debug, Serialize)]
pub struct StopReceipt {
    pub device_index: i32,
    pub zero_commands_sent: usize,
    pub commands_attempted: usize,
    pub all_zero_commands_sent: bool,
    pub command_results: Vec<StopCommandResult>,
    pub total_elapsed_ms: u128,
    pub physical_stop_verified: bool,
    pub limitation: &'static str,
}

/// Private and zero-stop-only: tests can inject a send failure without exposing
/// an arbitrary network-send interface to application callers.
trait ZeroStopTransport: Sync {
    fn send_zero<'a>(
        &'a self,
        packet: &'a [u8],
        target: SocketAddr,
    ) -> impl Future<Output = std::io::Result<usize>> + Send + 'a;
}

impl ZeroStopTransport for UdpSocket {
    fn send_zero<'a>(
        &'a self,
        packet: &'a [u8],
        target: SocketAddr,
    ) -> impl Future<Output = std::io::Result<usize>> + Send + 'a {
        async move { self.send_to(packet, target).await }
    }
}

async fn best_effort_zero_stop(
    transport: &impl ZeroStopTransport,
    device_index: i32,
    server: SocketAddr,
    deadline: Instant,
) -> StopReceipt {
    let start = Instant::now();
    let mut results = Vec::with_capacity(STOP_ADDRESSES.len());
    for (direction_index, address) in STOP_ADDRESSES.into_iter().enumerate() {
        let item_start = Instant::now();
        let mut result = StopCommandResult {
            direction_index,
            address,
            attempted: false,
            result_code: 3,
            bytes_sent: 0,
            expected_bytes: 0,
            os_error: None,
            elapsed_ms: 0,
        };
        let packet = encode_message(&OscMessage {
            address: address.into(),
            arguments: vec![OscArg::Int(device_index), OscArg::Int(0)],
        });
        match packet {
            Err(_) => result.result_code = 5,
            Ok(packet) => {
                result.expected_bytes = packet.len();
                if Instant::now() < deadline {
                    result.attempted = true;
                    // A stalled first send cannot consume the whole stop budget.
                    let item_deadline = deadline.min(Instant::now() + STOP_COMMAND_DEADLINE);
                    match timeout_at(item_deadline, transport.send_zero(&packet, server)).await {
                        Err(_) => result.result_code = 2,
                        Ok(Err(error)) => {
                            result.result_code = 1;
                            result.os_error = error.raw_os_error();
                        }
                        Ok(Ok(size)) => {
                            result.bytes_sent = size;
                            result.result_code = if size == packet.len() { 0 } else { 4 };
                        }
                    }
                }
            }
        }
        result.elapsed_ms = item_start.elapsed().as_millis();
        results.push(result);
    }
    let sent = results
        .iter()
        .filter(|result| result.result_code == 0)
        .count();
    StopReceipt {
        device_index,
        zero_commands_sent: sent,
        commands_attempted: results.iter().filter(|result| result.attempted).count(),
        all_zero_commands_sent: sent == STOP_ADDRESSES.len(),
        command_results: results,
        total_elapsed_ms: start.elapsed().as_millis(),
        physical_stop_verified: false,
        limitation: "Per-command results report only local UDP send outcomes, including partial failures; check all_zero_commands_sent. No physical stop/arrival acknowledgement exists, and device identity may have changed since verification.",
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PoseObservation {
    pub pose: Pose,
    pub read_receipt: ReadReceipt,
    pub canonical_pose_evidence: Option<NativeCanonicalEvidence>,
}

fn check_identity(config: &CalibrationConfig, receipt: &ReadReceipt) -> Result<()> {
    let ReadState::DeviceInfo(info) = &receipt.reply.state else {
        bail!("osc_motion_device_info_required")
    };
    let connected = info
        .slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.connected)
        .collect::<Vec<_>>();
    ensure!(connected.len() == 1, "osc_motion_unique_device_required");
    let (index, slot) = connected[0];
    ensure!(
        index as i32 == config.device_index && info.selected_device_index == config.device_index,
        "osc_motion_device_selection_mismatch"
    );
    ensure!(
        slot.name == config.expected_name,
        "osc_motion_exact_name_mismatch"
    );
    ensure!(
        info.selected_device_running,
        "osc_motion_device_not_running"
    );
    Ok(())
}

fn check_tracking_unlocked(receipt: &ReadReceipt) -> Result<()> {
    ensure!(
        matches!(
            receipt.reply.state,
            ReadState::AiTrackingInfo { locked: false }
        ),
        "osc_motion_tracking_must_be_unlocked"
    );
    Ok(())
}

/// A session can only be created through six successful, bounded read queries.
/// It grants one finite step, not arbitrary writes. Any attempted step consumes
/// the permission; errors, expiry or stop require verification again.
pub struct OscMotionSession {
    reader: OscReadClient,
    writer: UdpSocket,
    server: SocketAddr,
    config: CalibrationConfig,
    proof: CalibrationProof,
    expires_at: Instant,
    permission_used: bool,
}

impl OscMotionSession {
    pub async fn verify_for_calibration(config: CalibrationConfig) -> Result<Self> {
        ensure!(
            config.canonical_root.is_none()
                || config.gimbal_profile == GimbalProfile::Tiny3Center2015PanPitch,
            "osc_motion_canonical_profile_required"
        );
        ensure!(
            config.expected_name == "Tiny 3",
            "osc_motion_exact_tiny3_name_required"
        );
        ensure!(
            config.device_index == 0,
            "osc_motion_sole_tiny3_index_zero_required"
        );
        ensure!(
            !config.query_timeout.is_zero() && config.query_timeout <= Duration::from_secs(1),
            "osc_motion_query_timeout_limit"
        );
        ensure!(
            config.command_bind_port == 0
                || (config.command_bind_port != config.server_port
                    && config.command_bind_port != config.reply_port),
            "osc_motion_command_port_conflict"
        );
        ReadQuery::DeviceInfo.request(config.device_index)?;
        let mut reader = OscReadClient::bind_with_profile(
            config.server_port,
            config.reply_port,
            config.query_timeout,
            config.gimbal_profile,
        )
        .await?;
        let (baseline_pose, read_receipts, canonical_pose_evidence) =
            timeout(VERIFICATION_DEADLINE, async {
                let mut receipts = Vec::with_capacity(6);
                let mut canonical_evidence = Vec::with_capacity(2);
                let first_identity = reader.handshake_device_info(config.device_index).await?;
                check_identity(&config, &first_identity)?;
                receipts.push(first_identity);
                let first_ai = reader
                    .query(ReadQuery::AiTrackingInfo, config.device_index)
                    .await?;
                check_tracking_unlocked(&first_ai)?;
                receipts.push(first_ai);
                let first_pose = reader
                    .query(ReadQuery::GimbalPosition, config.device_index)
                    .await?;
                let (before, native_before) = referenced_pose(&config, &first_pose).await?;
                if let Some(evidence) = native_before {
                    canonical_evidence.push(evidence);
                }
                ensure!(
                    before.in_command_range(),
                    "osc_motion_start_outside_command_range"
                );
                receipts.push(first_pose);
                sleep(SAMPLE_GAP).await;
                let second_ai = reader
                    .query(ReadQuery::AiTrackingInfo, config.device_index)
                    .await?;
                check_tracking_unlocked(&second_ai)?;
                receipts.push(second_ai);
                let second_pose = reader
                    .query(ReadQuery::GimbalPosition, config.device_index)
                    .await?;
                let (after, native_after) = referenced_pose(&config, &second_pose).await?;
                if let Some(evidence) = native_after {
                    canonical_evidence.push(evidence);
                }
                ensure!(before.same_axes(after), "osc_motion_pose_not_stable");
                receipts.push(second_pose);
                let final_identity = reader.handshake_device_info(config.device_index).await?;
                check_identity(&config, &final_identity)?;
                receipts.push(final_identity);
                Ok::<_, anyhow::Error>((after, receipts, canonical_evidence))
            })
            .await
            .context("osc_motion_verification_timeout")??;
        // This socket is created only after the read-only gate succeeds.
        let writer = UdpSocket::bind(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            config.command_bind_port,
        ))
        .await
        .context("osc_motion_command_bind_failed")?;
        let proof = CalibrationProof {
            verified_at: chrono::Local::now().to_rfc3339(),
            authority_ttl_ms: AUTHORITY_TTL.as_millis(),
            maximum_finite_steps: 1,
            expected_name: config.expected_name.clone(),
            device_index: config.device_index,
            baseline_pose,
            source_type: if config.canonical_root.is_some() {
                "windows_native_confirmed_osc"
            } else {
                "vendor_osc_status"
            },
            read_receipts,
            canonical_pose_evidence,
            limitation: "AI unlock is not a global AI-mode-off guarantee. UDP has no authentication/nonce; observations are not physical calibration or a verified emergency stop.",
        };
        Ok(Self {
            reader,
            writer,
            server: SocketAddrV4::new(Ipv4Addr::LOCALHOST, config.server_port).into(),
            config,
            proof,
            expires_at: Instant::now() + AUTHORITY_TTL,
            permission_used: false,
        })
    }

    pub fn verification(&self) -> &CalibrationProof {
        &self.proof
    }

    /// Returns the two-sample OSC baseline, not a physical calibration result.
    pub fn verified_pose(&self) -> Pose {
        self.proof.baseline_pose
    }

    /// Observation only: does not renew, grant or consume movement authority.
    pub async fn read_pose(&mut self) -> Result<PoseObservation> {
        let read_receipt = self
            .reader
            .query(ReadQuery::GimbalPosition, self.config.device_index)
            .await?;
        let (pose, canonical_pose_evidence) = referenced_pose(&self.config, &read_receipt).await?;
        Ok(PoseObservation {
            pose,
            read_receipt,
            canonical_pose_evidence,
        })
    }

    pub fn command_local_addr(&self) -> Result<SocketAddr> {
        Ok(self.writer.local_addr()?)
    }

    fn ensure_live(&self) -> Result<()> {
        ensure!(!self.permission_used, "osc_motion_authority_consumed");
        ensure!(
            Instant::now() < self.expires_at,
            "osc_motion_authority_expired"
        );
        Ok(())
    }

    /// One absolute target at fixed speed 1. Each axis is limited to +/-1 degree
    /// relative to a freshly queried pose, which must still match the stable
    /// calibration baseline. There is no continuous-speed command path.
    pub async fn finite_step(
        &mut self,
        delta_pan: f32,
        delta_tilt: f32,
    ) -> Result<FiniteStepReceipt> {
        self.finite_step_guarded(delta_pan, delta_tilt, &|| true)
            .await
    }

    /// The caller supplies a quick nonblocking atomic dispatch check. It runs
    /// once after all awaited reads/encoding, immediately before UDP dispatch.
    /// False consumes this one-step permission without sending a motor packet.
    pub async fn finite_step_guarded(
        &mut self,
        delta_pan: f32,
        delta_tilt: f32,
        dispatch_guard: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<FiniteStepReceipt> {
        self.finite_bounded_step_guarded(
            delta_pan,
            delta_tilt,
            FiniteProfile::Calibration,
            dispatch_guard,
        )
        .await
    }

    /// Production following only: one guarded absolute target, at most 3 degrees
    /// per axis and fixed official-protocol speed 4. Calibration remains 1/1.
    /// Identity, AI, canonical-pose, range, cancellation and single-use authority
    /// checks are identical to the calibration path; no velocity is requested.
    pub async fn finite_follow_step_guarded(
        &mut self,
        delta_pan: f32,
        delta_tilt: f32,
        dispatch_guard: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<FiniteStepReceipt> {
        self.finite_bounded_step_guarded(
            delta_pan,
            delta_tilt,
            FiniteProfile::Follow,
            dispatch_guard,
        )
        .await
    }

    async fn finite_bounded_step_guarded(
        &mut self,
        delta_pan: f32,
        delta_tilt: f32,
        profile: FiniteProfile,
        dispatch_guard: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<FiniteStepReceipt> {
        ensure!(
            delta_pan.is_finite()
                && delta_tilt.is_finite()
                && delta_pan.abs() <= profile.limit()
                && delta_tilt.abs() <= profile.limit()
                && (delta_pan != 0.0 || delta_tilt != 0.0),
            profile.limit_error()
        );
        self.ensure_live()?;
        self.permission_used = true;
        let mut receipts = Vec::with_capacity(3);
        let identity = self
            .reader
            .handshake_device_info(self.config.device_index)
            .await?;
        check_identity(&self.config, &identity)?;
        receipts.push(identity);
        let ai = self
            .reader
            .query(ReadQuery::AiTrackingInfo, self.config.device_index)
            .await?;
        check_tracking_unlocked(&ai)?;
        receipts.push(ai);
        let pose_receipt = self
            .reader
            .query(ReadQuery::GimbalPosition, self.config.device_index)
            .await?;
        let (pose, canonical_pose_evidence) = referenced_pose(&self.config, &pose_receipt).await?;
        if !pose.same_axes(self.proof.baseline_pose) {
            return Err(PoseChangedBeforeDispatch {
                code: "osc_motion_pose_changed_reverify",
                baseline_pose: self.proof.baseline_pose,
                current_pose: pose,
                baseline_raw_poses: self
                    .proof
                    .read_receipts
                    .iter()
                    .filter(|receipt| receipt.query == ReadQuery::GimbalPosition)
                    .map(|receipt| receipt.message.clone())
                    .collect(),
                current_raw_pose: pose_receipt.message.clone(),
                current_source: pose_receipt.source,
                gimbal_profile: self.config.gimbal_profile,
                nonzero_commands_sent: 0,
                canonical_baseline_evidence: self.proof.canonical_pose_evidence.clone(),
                canonical_current_evidence: canonical_pose_evidence,
            }
            .into());
        }
        ensure!(
            pose.in_command_range(),
            "osc_motion_start_outside_command_range"
        );
        receipts.push(pose_receipt);
        ensure!(
            Instant::now() < self.expires_at,
            "osc_motion_authority_expired"
        );
        let target_pan = pose.yaw_degrees as f32 + delta_pan;
        let target_tilt = pose.pitch_degrees as f32 + delta_tilt;
        ensure!(
            (-129.0..=129.0).contains(&target_pan) && (-59.0..=59.0).contains(&target_tilt),
            "osc_motion_target_out_of_range"
        );
        let command = OscMessage {
            address: ABSOLUTE_ADDRESS.into(),
            arguments: vec![
                OscArg::Int(self.config.device_index),
                OscArg::Float(profile.speed()),
                OscArg::Float(target_pan),
                OscArg::Float(target_tilt),
            ],
        };
        let packet = encode_message(&command)?;
        let deadline = self.expires_at.min(Instant::now() + SEND_DEADLINE);
        ensure!(dispatch_guard(), "osc_motion_dispatch_guard_rejected");
        ensure!(
            Instant::now() < self.expires_at,
            "osc_motion_authority_expired"
        );
        let sent = timeout_at(deadline, self.writer.send_to(&packet, self.server))
            .await
            .context("osc_motion_send_timeout")?
            .context("osc_motion_send_failed")?;
        ensure!(sent == packet.len(), "osc_motion_incomplete_send");
        Ok(FiniteStepReceipt {
            device_index: self.config.device_index,
            before_pose: pose,
            target_pan_degrees: target_pan,
            target_tilt_degrees: target_tilt,
            speed: profile.speed(),
            udp_send_accepted: true,
            authority_consumed: true,
            physical_motion_verified: false,
            fresh_read_receipts: receipts,
            canonical_pose_evidence,
        })
    }

    /// Best-effort four-direction zero commands, not a verified physical stop.
    /// Available after expiry/consumption so a stale movement permission cannot
    /// prevent a zero-only stop request. It never renews movement permission.
    /// Device identity may have changed since verification; this limitation is
    /// explicit and prevents use as a claimed independent emergency watchdog.
    /// `Ok(receipt)` preserves partial failures; callers must check
    /// `all_zero_commands_sent` instead of treating Ok as four successful sends.
    pub async fn stop(&mut self) -> Result<StopReceipt> {
        self.permission_used = true;
        let deadline = Instant::now() + SEND_DEADLINE;
        // Always retain four item results: an early error must neither suppress
        // later zero requests nor erase successfully submitted earlier ones.
        Ok(best_effort_zero_stop(
            &self.writer,
            self.config.device_index,
            self.server,
            deadline,
        )
        .await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obsbot_osc::decode_message;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    fn canonical_fixture(pan: i32, tilt: i32) -> NativeCanonicalEvidence {
        NativeCanonicalEvidence {
            source: "synthetic_test_fixture",
            pose: Pose {
                yaw_degrees: pan,
                pitch_degrees: -tilt,
                roll_unused_degrees: 0,
            },
            native_pan_degrees: pan,
            native_tilt_degrees: tilt,
            saved_camera_device_path: "synthetic-bound-instance".into(),
            observed_native_device_path: "synthetic-bound-instance".into(),
            device_binding_verified: true,
            native_timestamp_utc: "2026-11-01T08:37:10+00:00".into(),
            elapsed_ms: 40,
            limitation: "Synthetic fixture, no hardware.",
        }
    }

    #[test]
    fn canonical_fusion_never_uses_osc_jitter_as_target_or_uncommanded_axis() {
        let canonical = canonical_fixture(17, 24);
        for (yaw, pitch) in [(16, -24), (17, -24), (18, -25)] {
            let chosen = confirm_consistency(
                Pose {
                    yaw_degrees: yaw,
                    pitch_degrees: pitch,
                    roll_unused_degrees: 0,
                },
                &canonical,
            )
            .unwrap();
            assert_eq!(chosen.yaw_degrees, 17);
            assert_eq!(chosen.pitch_degrees, -24);
        }
        let changed = canonical_fixture(18, 24);
        let current = confirm_consistency(
            Pose {
                yaw_degrees: 17,
                pitch_degrees: -24,
                roll_unused_degrees: 0,
            },
            &changed,
        )
        .unwrap();
        assert!(
            !current.same_axes(canonical.pose),
            "canonical movement remains a strict baseline failure"
        );
    }

    #[test]
    fn canonical_fusion_rejects_two_or_more_degrees_with_numeric_evidence() {
        let canonical = canonical_fixture(17, 24);
        for osc in [
            Pose {
                yaw_degrees: 15,
                pitch_degrees: -24,
                roll_unused_degrees: 0,
            },
            Pose {
                yaw_degrees: 17,
                pitch_degrees: -28,
                roll_unused_degrees: 0,
            },
        ] {
            let error = confirm_consistency(osc, &canonical).unwrap_err();
            assert_eq!(
                error.to_string(),
                "osc_motion_canonical_consistency_rejected"
            );
            let details = motion_error_details(&error).unwrap();
            assert_eq!(details["canonical"]["pose"]["pitch_degrees"], -24);
            assert_eq!(details["osc_pose"]["pitch_degrees"], osc.pitch_degrees);
            assert_eq!(details["nonzero_commands_sent"], 0);
        }
    }

    struct FixtureState {
        name: String,
        connected: [bool; 4],
        selected: i32,
        ai_locked: bool,
        ai_samples: VecDeque<bool>,
        pose: Pose,
        pose_samples: VecDeque<Pose>,
        silent: bool,
        profile: GimbalProfile,
    }
    struct Fixture {
        port: u16,
        state: Arc<Mutex<FixtureState>>,
        commands: Arc<Mutex<Vec<OscMessage>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Fixture {
        async fn new(pitch: i32, yaw: i32) -> Self {
            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = socket.local_addr().unwrap().port();
            let state = Arc::new(Mutex::new(FixtureState {
                name: "Tiny 3".into(),
                connected: [true, false, false, false],
                selected: 0,
                ai_locked: false,
                ai_samples: VecDeque::new(),
                pose: Pose {
                    roll_unused_degrees: 0,
                    pitch_degrees: pitch,
                    yaw_degrees: yaw,
                },
                pose_samples: VecDeque::new(),
                silent: false,
                profile: GimbalProfile::DocumentedRollPitchYaw,
            }));
            let commands = Arc::new(Mutex::new(Vec::new()));
            let server_state = state.clone();
            let received_commands = commands.clone();
            let task = tokio::spawn(async move {
                let mut buffer = [0u8; 4097];
                loop {
                    let (len, peer) = socket.recv_from(&mut buffer).await.unwrap();
                    let message = decode_message(&buffer[..len]).unwrap();
                    let response = {
                        let mut state = server_state.lock().unwrap();
                        if message.address == ReadQuery::DeviceInfoViaHandshake.address()
                            || ReadQuery::ALL
                                .iter()
                                .any(|query| query.address() == message.address)
                        {
                            if state.silent {
                                None
                            } else {
                                let (address, arguments) = if message.address
                                    == ReadQuery::DeviceInfo.address()
                                    || message.address
                                        == ReadQuery::DeviceInfoViaHandshake.address()
                                {
                                    let mut args = Vec::new();
                                    for connected in state.connected {
                                        args.push(OscArg::Int(i32::from(connected)));
                                        args.push(OscArg::String(if connected {
                                            state.name.clone()
                                        } else {
                                            String::new()
                                        }));
                                    }
                                    args.extend([
                                        OscArg::Int(state.selected),
                                        OscArg::Int(1),
                                        OscArg::Int(999),
                                    ]);
                                    (ReadQuery::DeviceInfo.response_address(), args)
                                } else if message.address == ReadQuery::AiTrackingInfo.address() {
                                    if let Some(value) = state.ai_samples.pop_front() {
                                        state.ai_locked = value;
                                    }
                                    (
                                        ReadQuery::AiTrackingInfo.response_address(),
                                        vec![OscArg::Int(i32::from(state.ai_locked))],
                                    )
                                } else {
                                    if let Some(value) = state.pose_samples.pop_front() {
                                        state.pose = value;
                                    }
                                    let p = state.pose;
                                    (
                                        ReadQuery::GimbalPosition.response_address(),
                                        if state.profile == GimbalProfile::DocumentedRollPitchYaw {
                                            vec![
                                                OscArg::Int(p.roll_unused_degrees),
                                                OscArg::Int(p.pitch_degrees),
                                                OscArg::Int(p.yaw_degrees),
                                            ]
                                        } else {
                                            vec![
                                                OscArg::Int(p.yaw_degrees),
                                                OscArg::Int(p.pitch_degrees),
                                            ]
                                        },
                                    )
                                };
                                Some(OscMessage {
                                    address: address.into(),
                                    arguments,
                                })
                            }
                        } else {
                            received_commands.lock().unwrap().push(message);
                            None
                        }
                    };
                    if let Some(response) = response {
                        socket
                            .send_to(&encode_message(&response).unwrap(), peer)
                            .await
                            .unwrap();
                    }
                }
            });
            Self {
                port,
                state,
                commands,
                task,
            }
        }
        fn config(&self) -> CalibrationConfig {
            CalibrationConfig {
                server_port: self.port,
                reply_port: 0,
                command_bind_port: 0,
                device_index: 0,
                expected_name: "Tiny 3".into(),
                query_timeout: Duration::from_millis(100),
                gimbal_profile: self.state.lock().unwrap().profile,
                canonical_root: None,
            }
        }
        async fn commands(&self, expected: usize) -> Vec<OscMessage> {
            if expected == 0 {
                sleep(Duration::from_millis(10)).await;
            }
            timeout(Duration::from_millis(500), async {
                loop {
                    let messages = self.commands.lock().unwrap().clone();
                    if messages.len() >= expected {
                        return messages;
                    }
                    sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn verification_is_read_only_and_requires_unique_exact_binding() {
        for case in 0..4 {
            let fixture = Fixture::new(47, 0).await;
            {
                let mut s = fixture.state.lock().unwrap();
                match case {
                    0 => s.connected[1] = true,
                    1 => s.name = "wrong device".into(),
                    2 => s.selected = 1,
                    _ => s.connected = [false, true, false, false],
                }
            }
            assert!(
                OscMotionSession::verify_for_calibration(fixture.config())
                    .await
                    .is_err()
            );
            assert!(fixture.commands(0).await.is_empty());
        }
        let fixture = Fixture::new(47, 0).await;
        let session = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .unwrap();
        assert_eq!(session.verification().read_receipts.len(), 6);
        assert_eq!(
            session.verification().read_receipts[0].query,
            ReadQuery::DeviceInfoViaHandshake
        );
        assert_eq!(
            session.verification().read_receipts[5].query,
            ReadQuery::DeviceInfoViaHandshake
        );
        assert_eq!(session.verification().baseline_pose.pitch_degrees, 47);
        assert!(fixture.commands(0).await.is_empty());
    }

    #[tokio::test]
    async fn both_tracking_samples_and_stable_pose_are_mandatory() {
        for case in 0..3 {
            let fixture = Fixture::new(47, 0).await;
            {
                let mut s = fixture.state.lock().unwrap();
                match case {
                    0 => s.ai_samples = VecDeque::from([true, false]),
                    1 => s.ai_samples = VecDeque::from([false, true]),
                    _ => {
                        s.pose_samples = VecDeque::from([
                            s.pose,
                            Pose {
                                pitch_degrees: 48,
                                ..s.pose
                            },
                        ])
                    }
                }
            }
            assert!(
                OscMotionSession::verify_for_calibration(fixture.config())
                    .await
                    .is_err()
            );
            assert!(fixture.commands(0).await.is_empty());
        }
    }

    #[tokio::test]
    async fn one_degree_fixed_speed_uses_current_47_degree_tilt() {
        let fixture = Fixture::new(47, 0).await;
        fixture.state.lock().unwrap().profile = GimbalProfile::Tiny3Center2015PanPitch;
        let mut session = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .unwrap();
        let receipt = session.finite_step(0.0, 1.0).await.unwrap();
        assert_eq!(receipt.target_tilt_degrees, 48.0);
        assert_eq!(receipt.speed, 1.0);
        assert!(!receipt.physical_motion_verified);
        assert_eq!(receipt.fresh_read_receipts.len(), 3);
        assert_eq!(
            receipt.fresh_read_receipts[0].query,
            ReadQuery::DeviceInfoViaHandshake
        );
        assert_eq!(
            receipt.fresh_read_receipts[2].reply.gimbal_profile,
            Some(GimbalProfile::Tiny3Center2015PanPitch)
        );
        let commands = fixture.commands(1).await;
        assert_eq!(
            commands,
            vec![OscMessage {
                address: ABSOLUTE_ADDRESS.into(),
                arguments: vec![
                    OscArg::Int(0),
                    OscArg::Float(1.0),
                    OscArg::Float(0.0),
                    OscArg::Float(48.0)
                ]
            }]
        );
        assert!(session.finite_step(0.0, -1.0).await.is_err());
        assert_eq!(fixture.commands(1).await.len(), 1);
    }

    #[tokio::test]
    async fn production_follow_three_degrees_uses_fixed_speed_four_without_changing_calibration() {
        let fixture = Fixture::new(47, 10).await;
        fixture.state.lock().unwrap().profile = GimbalProfile::Tiny3Center2015PanPitch;
        let mut session = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .unwrap();
        let receipt = session
            .finite_follow_step_guarded(3.0, -3.0, &|| true)
            .await
            .unwrap();
        assert_eq!(
            (
                receipt.target_pan_degrees,
                receipt.target_tilt_degrees,
                receipt.speed
            ),
            (13.0, 44.0, 4.0)
        );
        assert!(!receipt.physical_motion_verified);
        assert_eq!(receipt.fresh_read_receipts.len(), 3);
        assert_eq!(
            fixture.commands(1).await,
            vec![OscMessage {
                address: ABSOLUTE_ADDRESS.into(),
                arguments: vec![
                    OscArg::Int(0),
                    OscArg::Float(4.0),
                    OscArg::Float(13.0),
                    OscArg::Float(44.0)
                ]
            }]
        );
        assert!(
            session
                .finite_step_guarded(-1.0, 0.0, &|| true)
                .await
                .is_err()
        );
        assert!(
            session
                .finite_follow_step_guarded(-3.0, 0.0, &|| true)
                .await
                .is_err()
        );
        assert_eq!(fixture.commands(1).await.len(), 1);
    }

    #[tokio::test]
    async fn production_follow_rejects_invalid_limits_and_global_range_without_any_write() {
        for (pitch, yaw, dp, dt) in [
            (0, 0, 3.01, 0.0),
            (0, 0, 0.0, -3.01),
            (0, 0, f32::NAN, 0.0),
            (0, 0, 0.0, f32::INFINITY),
            (0, 0, 0.0, 0.0),
            (0, 127, 3.0, 0.0),
            (-58, 0, 0.0, -3.0),
        ] {
            let fixture = Fixture::new(pitch, yaw).await;
            let mut session = OscMotionSession::verify_for_calibration(fixture.config())
                .await
                .unwrap();
            assert!(
                session
                    .finite_follow_step_guarded(dp, dt, &|| true)
                    .await
                    .is_err()
            );
            assert!(fixture.commands(0).await.is_empty());
        }
        // A production-sized value cannot widen the existing calibration API.
        let fixture = Fixture::new(0, 0).await;
        let mut session = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .unwrap();
        assert_eq!(
            session
                .finite_step_guarded(3.0, 0.0, &|| true)
                .await
                .unwrap_err()
                .to_string(),
            "osc_motion_finite_step_limit"
        );
        let receipt = session
            .finite_step_guarded(1.0, 0.0, &|| true)
            .await
            .unwrap();
        assert_eq!(receipt.speed, 1.0);
        assert_eq!(receipt.target_pan_degrees, 1.0);
    }

    #[tokio::test]
    async fn production_follow_still_rechecks_live_identity_pose_ai_and_final_dispatch_guard() {
        for case in 0..4 {
            let fixture = Fixture::new(0, 0).await;
            let mut session = OscMotionSession::verify_for_calibration(fixture.config())
                .await
                .unwrap();
            {
                let mut state = fixture.state.lock().unwrap();
                match case {
                    0 => state.name = "different device".into(),
                    1 => state.ai_locked = true,
                    2 => state.pose.yaw_degrees = 1,
                    _ => {}
                }
            }
            let result = session
                .finite_follow_step_guarded(3.0, -3.0, &|| case != 3)
                .await;
            assert!(result.is_err());
            if case == 3 {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "osc_motion_dispatch_guard_rejected"
                );
            }
            assert!(session.permission_used);
            assert!(fixture.commands(0).await.is_empty());
        }
    }

    #[tokio::test]
    async fn target_ranges_and_step_nonfinite_limits_reject_before_write() {
        for (pitch, yaw, dp, dt) in [
            (59, 0, 0.0, 1.0),
            (0, 129, 1.0, 0.0),
            (47, 0, 2.0, 0.0),
            (47, 0, f32::NAN, 0.0),
            (47, 0, 0.0, 0.0),
        ] {
            let fixture = Fixture::new(pitch, yaw).await;
            let mut session = OscMotionSession::verify_for_calibration(fixture.config())
                .await
                .unwrap();
            assert!(session.finite_step(dp, dt).await.is_err());
            assert!(fixture.commands(0).await.is_empty());
        }
        let fixture = Fixture::new(60, 0).await;
        assert!(
            OscMotionSession::verify_for_calibration(fixture.config())
                .await
                .is_err()
        );
        assert!(fixture.commands(0).await.is_empty());
    }

    #[tokio::test]
    async fn finite_step_rechecks_identity_ai_and_pose_and_consumes_failed_gate() {
        for case in 0..4 {
            let fixture = Fixture::new(47, 0).await;
            let mut session = OscMotionSession::verify_for_calibration(fixture.config())
                .await
                .unwrap();
            {
                let mut state = fixture.state.lock().unwrap();
                match case {
                    0 => state.name = "changed device".into(),
                    1 => state.ai_locked = true,
                    2 => state.pose.yaw_degrees = 1,
                    _ => state.selected = 1,
                }
            }
            assert!(session.finite_step(1.0, 0.0).await.is_err());
            assert!(session.permission_used);
            assert!(fixture.commands(0).await.is_empty());
        }
    }

    #[tokio::test]
    async fn expired_authority_rejects_motion_but_stop_sends_only_four_zeros() {
        let fixture = Fixture::new(47, 0).await;
        let mut session = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .unwrap();
        // Synthetic monotonic expiry, not a shortened production TTL.
        session.expires_at = Instant::now() - Duration::from_millis(1);
        let error = session.finite_step(1.0, 0.0).await.unwrap_err();
        assert_eq!(error.to_string(), "osc_motion_authority_expired");
        assert!(fixture.commands(0).await.is_empty());
        let stop = session.stop().await.unwrap();
        assert_eq!(stop.zero_commands_sent, 4);
        assert_eq!(stop.commands_attempted, 4);
        assert!(stop.all_zero_commands_sent);
        assert_eq!(stop.command_results.len(), 4);
        assert!(
            stop.command_results
                .iter()
                .all(|result| result.result_code == 0)
        );
        assert!(!stop.physical_stop_verified);
        let messages = fixture.commands(4).await;
        assert_eq!(messages.len(), 4);
        for (message, address) in messages.iter().zip(STOP_ADDRESSES) {
            assert_eq!(message.address, address);
            assert_eq!(message.arguments, vec![OscArg::Int(0), OscArg::Int(0)]);
        }
        assert!(session.finite_step(1.0, 0.0).await.is_err());
    }

    #[tokio::test]
    async fn verification_timeout_and_invalid_configuration_grant_no_write_permission() {
        let fixture = Fixture::new(47, 0).await;
        fixture.state.lock().unwrap().silent = true;
        let error = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "osc_timeout");
        assert!(fixture.commands(0).await.is_empty());
        let mut config = fixture.config();
        config.expected_name.clear();
        assert!(
            OscMotionSession::verify_for_calibration(config)
                .await
                .is_err()
        );
        let mut config = fixture.config();
        config.query_timeout = Duration::from_secs(2);
        assert!(
            OscMotionSession::verify_for_calibration(config)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn final_dispatch_guard_rejects_after_reads_without_sending_motor_command() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixture = Fixture::new(47, 0).await;
        let mut session = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .unwrap();
        let calls = AtomicUsize::new(0);
        let guard = || {
            calls.fetch_add(1, Ordering::SeqCst);
            false
        };
        let error = session
            .finite_step_guarded(1.0, 0.0, &guard)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "osc_motion_dispatch_guard_rejected");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(session.permission_used);
        assert!(fixture.commands(0).await.is_empty());
        assert_eq!(
            session.finite_step(1.0, 0.0).await.unwrap_err().to_string(),
            "osc_motion_authority_consumed"
        );
    }

    #[tokio::test]
    async fn pose_change_error_preserves_numeric_baseline_current_and_raw_replies() {
        let fixture = Fixture::new(47, 0).await;
        fixture.state.lock().unwrap().profile = GimbalProfile::Tiny3Center2015PanPitch;
        let mut session = OscMotionSession::verify_for_calibration(fixture.config())
            .await
            .unwrap();
        fixture.state.lock().unwrap().pose.pitch_degrees = 46;
        let error = session
            .finite_step_guarded(1.0, 0.0, &|| false)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "osc_motion_pose_changed_reverify");
        let details = motion_error_details(&error).unwrap();
        assert_eq!(details["baseline_pose"]["pitch_degrees"], 47);
        assert_eq!(details["current_pose"]["pitch_degrees"], 46);
        assert_eq!(details["baseline_raw_poses"].as_array().unwrap().len(), 2);
        assert_eq!(
            details["baseline_raw_poses"][0]["arguments"][1]["value"],
            47
        );
        assert_eq!(details["current_raw_pose"]["arguments"][1]["value"], 46);
        assert_eq!(
            details["current_source"],
            format!("127.0.0.1:{}", fixture.port)
        );
        assert_eq!(details["nonzero_commands_sent"], 0);
        assert!(fixture.commands(0).await.is_empty());
    }

    #[derive(Clone, Copy)]
    enum StopFault {
        Io(i32),
        Stall,
        Partial,
        ActualUdp,
    }

    struct FaultStopTransport {
        socket: UdpSocket,
        plan: [StopFault; 4],
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FaultStopTransport {
        async fn new(plan: [StopFault; 4]) -> Self {
            Self {
                socket: UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(),
                plan,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl ZeroStopTransport for FaultStopTransport {
        fn send_zero<'a>(
            &'a self,
            packet: &'a [u8],
            target: SocketAddr,
        ) -> impl Future<Output = std::io::Result<usize>> + Send + 'a {
            async move {
                // Fault injection is limited to the four known zero packets.
                let message = decode_message(packet).unwrap();
                assert!(STOP_ADDRESSES.contains(&message.address.as_str()));
                assert_eq!(message.arguments, vec![OscArg::Int(0), OscArg::Int(0)]);
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                match self.plan[call] {
                    StopFault::Io(code) => Err(std::io::Error::from_raw_os_error(code)),
                    StopFault::Stall => std::future::pending().await,
                    StopFault::Partial => Ok(packet.len() - 4),
                    StopFault::ActualUdp => self.socket.send_to(packet, target).await,
                }
            }
        }
    }

    #[tokio::test]
    async fn stop_retains_io_timeout_and_later_actual_udp_successes() {
        let fixture = Fixture::new(47, 0).await;
        let transport = FaultStopTransport::new([
            StopFault::Io(10061),
            StopFault::ActualUdp,
            StopFault::Stall,
            StopFault::ActualUdp,
        ])
        .await;
        let target = SocketAddrV4::new(Ipv4Addr::LOCALHOST, fixture.port).into();
        let receipt =
            best_effort_zero_stop(&transport, 0, target, Instant::now() + SEND_DEADLINE).await;
        assert_eq!(transport.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert_eq!(receipt.commands_attempted, 4);
        assert_eq!(receipt.zero_commands_sent, 2);
        assert!(!receipt.all_zero_commands_sent);
        assert!(!receipt.physical_stop_verified);
        assert_eq!(
            receipt
                .command_results
                .iter()
                .map(|item| item.result_code)
                .collect::<Vec<_>>(),
            vec![1, 0, 2, 0]
        );
        assert_eq!(receipt.command_results[0].os_error, Some(10061));
        for index in [1, 3] {
            assert_eq!(
                receipt.command_results[index].bytes_sent,
                receipt.command_results[index].expected_bytes
            );
        }
        let received = fixture.commands(2).await;
        assert_eq!(
            received
                .iter()
                .map(|message| message.address.as_str())
                .collect::<Vec<_>>(),
            vec![STOP_ADDRESSES[1], STOP_ADDRESSES[3]]
        );
    }

    #[tokio::test]
    async fn stop_partial_send_is_preserved_without_counting_it_as_success() {
        let fixture = Fixture::new(47, 0).await;
        let transport = FaultStopTransport::new([
            StopFault::Partial,
            StopFault::ActualUdp,
            StopFault::ActualUdp,
            StopFault::ActualUdp,
        ])
        .await;
        let target = SocketAddrV4::new(Ipv4Addr::LOCALHOST, fixture.port).into();
        let receipt =
            best_effort_zero_stop(&transport, 0, target, Instant::now() + SEND_DEADLINE).await;
        assert_eq!(receipt.commands_attempted, 4);
        assert_eq!(receipt.zero_commands_sent, 3);
        assert!(!receipt.all_zero_commands_sent);
        let partial = &receipt.command_results[0];
        assert_eq!(partial.result_code, 4);
        assert_eq!(partial.bytes_sent + 4, partial.expected_bytes);
        assert_eq!(fixture.commands(3).await.len(), 3);
    }

    #[tokio::test]
    async fn stop_exhausted_total_deadline_records_all_four_unattempted_items() {
        let fixture = Fixture::new(47, 0).await;
        let transport = FaultStopTransport::new([StopFault::ActualUdp; 4]).await;
        let target = SocketAddrV4::new(Ipv4Addr::LOCALHOST, fixture.port).into();
        let receipt = best_effort_zero_stop(
            &transport,
            0,
            target,
            Instant::now() - Duration::from_millis(1),
        )
        .await;
        assert_eq!(transport.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(receipt.commands_attempted, 0);
        assert_eq!(receipt.zero_commands_sent, 0);
        assert_eq!(receipt.command_results.len(), 4);
        assert!(
            receipt
                .command_results
                .iter()
                .all(|item| !item.attempted && item.result_code == 3)
        );
        assert!(fixture.commands(0).await.is_empty());
    }
}

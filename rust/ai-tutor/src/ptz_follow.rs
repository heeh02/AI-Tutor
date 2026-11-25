//! Pure finite-position follower preparation. This module never accesses a device.
//!
//! An adapter must explicitly arm the selected camera epoch + opaque track key,
//! supply verified direction calibration and vendor tracking OFF, and recheck the
//! returned arm generation before dispatch. `Stop` is an instruction to the adapter
//! to cancel its transactions; this module does not claim a verified hardware stop.
//! Boxes use original, unmirrored normalized image coordinates. Display mirroring
//! is deliberately absent from this API. Readback and output angles are degrees.
use serde::Serialize;

pub const MIN_COMMAND_INTERVAL_MS: u64 = 250;
pub const MAX_FRAME_AGE_MS: u64 = 250;
pub const DEAD_ZONE: f64 = 0.12;
pub const MAX_STEP_DEGREES: f64 = 3.0;
pub const PAN_LIMIT_DEGREES: f64 = 120.0;
pub const TILT_LIMIT_DEGREES: f64 = 55.0;
const LOW_PASS_ALPHA: f64 = 0.35;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetIdentity {
    pub camera_epoch: String,
    pub target_key: String,
}

#[derive(Clone, Debug)]
pub struct TargetFrame {
    pub frame_id: u64,
    pub frame_age_ms: u64,
    pub camera_epoch: String,
    pub target_key: String,
    /// Original image xyxy coordinates in [0, 1], before display mirroring.
    pub bbox: [f64; 4],
    pub ambiguous: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AngleReadback {
    pub pan_degrees: f64,
    pub tilt_degrees: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    /// True only after both axes' physical direction calibration was completed.
    pub complete: bool,
    pub pan_sign: i8,
    pub tilt_sign: i8,
}
impl Calibration {
    fn valid(self) -> bool {
        self.complete && matches!(self.pan_sign, -1 | 1) && matches!(self.tilt_sign, -1 | 1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VendorAutoState {
    ConfirmedOff,
    On,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    NotArmed,
    InvalidSelection,
    ExplicitStopRequired,
    TargetLost,
    TargetChanged,
    CameraEpochChanged,
    StaleFrame,
    FrameNotNew,
    AmbiguousAssociation,
    InvalidBox,
    InvalidReadback,
    ReadbackOutOfRange,
    CalibrationIncompleteOrInvalid,
    CalibrationChanged,
    VendorTrackingOn,
    VendorTrackingUnknown,
    ClockRegressed,
    OperatorStop,
    CommandGenerationChanged,
    InvalidCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldReason {
    RateLimited,
    DeadZone,
    AngleLimit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FinitePosition {
    pub pan_degrees: f64,
    pub tilt_degrees: f64,
    pub frame_id: u64,
    pub camera_epoch: String,
    pub target_key: String,
    pub arm_generation: u64,
    pub generated_at_ms: u64,
    /// Latest dispatch time before the source frame exceeds its age budget.
    pub expires_at_ms: u64,
}

/// A distinct observation authorizes exactly the existing finite target after
/// bounded device queries. It never extends or relabels the proposal's old frame.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RefreshedCommand {
    pub pan_degrees: f64,
    pub tilt_degrees: f64,
    pub proposal_frame_id: u64,
    pub frame_id: u64,
    pub camera_epoch: String,
    pub target_key: String,
    pub arm_generation: u64,
    pub generated_at_ms: u64,
    pub expires_at_ms: u64,
    pub frame_age_at_refresh_ms: u64,
    pub raw_error: [f64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplanReason {
    NoNewerFrame,
    DirectionNoLongerValid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshError {
    Stop(StopReason),
    Replan(ReplanReason),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Stop(StopReason),
    Hold(HoldReason),
    FinitePosition(FinitePosition),
}

#[derive(Clone, Debug)]
struct ArmedTarget {
    identity: TargetIdentity,
    calibration: Calibration,
}

#[derive(Default, Debug)]
pub struct Follower {
    armed: Option<ArmedTarget>,
    smoothed_error: Option<[f64; 2]>,
    last_frame: Option<(String, u64)>,
    last_observed_ms: Option<u64>,
    last_command_ms: Option<u64>,
    generation: u64,
}

impl Follower {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_armed(&self) -> bool {
        self.armed.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn armed_target(&self) -> Option<&TargetIdentity> {
        self.armed.as_ref().map(|armed| &armed.identity)
    }

    /// Explicit arm only. It never emits a motion command or replaces a live arm.
    /// An error leaves the controller disarmed; callers must handle it as Stop.
    pub fn arm(
        &mut self,
        identity: TargetIdentity,
        calibration: Calibration,
        vendor_auto: VendorAutoState,
    ) -> Result<(), StopReason> {
        let problem = if let Some(current) = self.armed.as_ref() {
            Some(if current.identity != identity {
                StopReason::TargetChanged
            } else {
                StopReason::ExplicitStopRequired
            })
        } else if identity.camera_epoch.trim().is_empty() || identity.target_key.trim().is_empty() {
            Some(StopReason::InvalidSelection)
        } else {
            preflight(calibration, vendor_auto)
        };
        if let Some(reason) = problem {
            self.stop(reason);
            return Err(reason);
        }
        self.generation = self.generation.wrapping_add(1);
        self.smoothed_error = None;
        self.armed = Some(ArmedTarget {
            identity,
            calibration,
        });
        // Command timestamps/frame history survive rearm to prevent rate and
        // duplicate-frame checks being bypassed by toggling the arm control.
        Ok(())
    }

    /// Immediately disarm and clear filtering. No subsequent input can move
    /// until a separate explicit arm. Hardware stopping belongs to the adapter.
    pub fn stop(&mut self, reason: StopReason) -> Decision {
        self.armed = None;
        self.smoothed_error = None;
        self.generation = self.generation.wrapping_add(1);
        Decision::Stop(reason)
    }

    /// Synchronous, non-mutating dispatch check. The adapter still atomically
    /// checks its cancellation/selection generation, and the provider checks the
    /// unchanged canonical pose before this call. Only a strictly newer fresh
    /// raw observation can replace an expired proposal's visual authority.
    pub fn refresh_command(
        &self,
        now_ms: u64,
        latest: Option<&TargetFrame>,
        readback: AngleReadback,
        calibration: Calibration,
        vendor_auto: VendorAutoState,
        proposal: &FinitePosition,
    ) -> Result<RefreshedCommand, RefreshError> {
        use RefreshError::{Replan, Stop};
        let armed = self.armed.as_ref().ok_or(Stop(StopReason::NotArmed))?;
        if let Some(reason) = preflight(calibration, vendor_auto) {
            return Err(Stop(reason));
        }
        if calibration != armed.calibration {
            return Err(Stop(StopReason::CalibrationChanged));
        }
        if self.generation != proposal.arm_generation {
            return Err(Stop(StopReason::CommandGenerationChanged));
        }
        if now_ms < proposal.generated_at_ms
            || self.last_observed_ms.is_some_and(|last| now_ms < last)
        {
            return Err(Stop(StopReason::ClockRegressed));
        }
        if proposal.camera_epoch != armed.identity.camera_epoch {
            return Err(Stop(StopReason::CameraEpochChanged));
        }
        if proposal.target_key != armed.identity.target_key {
            return Err(Stop(StopReason::TargetChanged));
        }
        if self.last_frame.as_ref() != Some(&(proposal.camera_epoch.clone(), proposal.frame_id)) {
            return Err(Stop(StopReason::CommandGenerationChanged));
        }
        let frame = latest.ok_or(Stop(StopReason::TargetLost))?;
        if frame.camera_epoch != armed.identity.camera_epoch {
            return Err(Stop(StopReason::CameraEpochChanged));
        }
        if frame.target_key != armed.identity.target_key {
            return Err(Stop(StopReason::TargetChanged));
        }
        if frame.ambiguous {
            return Err(Stop(StopReason::AmbiguousAssociation));
        }
        if frame.frame_age_ms >= MAX_FRAME_AGE_MS {
            return Err(Stop(StopReason::StaleFrame));
        }
        if !valid_box(frame.bbox) {
            return Err(Stop(StopReason::InvalidBox));
        }
        if frame.frame_id < proposal.frame_id {
            return Err(Stop(StopReason::FrameNotNew));
        }
        if frame.frame_id == proposal.frame_id {
            return Err(Replan(ReplanReason::NoNewerFrame));
        }
        if !readback.pan_degrees.is_finite() || !readback.tilt_degrees.is_finite() {
            return Err(Stop(StopReason::InvalidReadback));
        }
        if readback.pan_degrees.abs() > PAN_LIMIT_DEGREES
            || readback.tilt_degrees.abs() > TILT_LIMIT_DEGREES
        {
            return Err(Stop(StopReason::ReadbackOutOfRange));
        }
        let step = [
            proposal.pan_degrees - readback.pan_degrees,
            proposal.tilt_degrees - readback.tilt_degrees,
        ];
        if !proposal.pan_degrees.is_finite()
            || !proposal.tilt_degrees.is_finite()
            || proposal.pan_degrees.fract() != 0.0
            || proposal.tilt_degrees.fract() != 0.0
            || proposal.pan_degrees.abs() > PAN_LIMIT_DEGREES
            || proposal.tilt_degrees.abs() > TILT_LIMIT_DEGREES
            || step
                .iter()
                .any(|delta| !delta.is_finite() || delta.abs() > MAX_STEP_DEGREES)
            || step == [0.0, 0.0]
        {
            return Err(Stop(StopReason::InvalidCommand));
        }
        let [x1, y1, x2, y2] = frame.bbox;
        let raw = [x1 + x2 - 1.0, y1 + y2 - 1.0];
        let signs = [
            f64::from(calibration.pan_sign),
            f64::from(calibration.tilt_sign),
        ];
        if (0..2).any(|axis| {
            step[axis] != 0.0
                && (raw[axis].abs() <= DEAD_ZONE || raw[axis] * signs[axis] * step[axis] <= 0.0)
        }) {
            return Err(Replan(ReplanReason::DirectionNoLongerValid));
        }
        Ok(RefreshedCommand {
            pan_degrees: proposal.pan_degrees,
            tilt_degrees: proposal.tilt_degrees,
            proposal_frame_id: proposal.frame_id,
            frame_id: frame.frame_id,
            camera_epoch: frame.camera_epoch.clone(),
            target_key: frame.target_key.clone(),
            arm_generation: self.generation,
            generated_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(MAX_FRAME_AGE_MS - frame.frame_age_ms),
            frame_age_at_refresh_ms: frame.frame_age_ms,
            raw_error: raw,
        })
    }

    /// Preserve the command-rate budget at actual UDP acceptance, rather than
    /// at the older proposal time before asynchronous device verification.
    pub fn record_dispatch(&mut self, now_ms: u64) -> Result<(), StopReason> {
        if self.last_observed_ms.is_some_and(|last| now_ms < last) {
            self.stop(StopReason::ClockRegressed);
            return Err(StopReason::ClockRegressed);
        }
        self.last_observed_ms = Some(now_ms);
        self.last_command_ms = Some(now_ms);
        Ok(())
    }

    /// All guard checks precede rate limiting, so a bad observation returns Stop
    /// even between two otherwise allowed finite-position dispatches.
    pub fn update(
        &mut self,
        now_ms: u64,
        target: Option<&TargetFrame>,
        readback: AngleReadback,
        calibration: Calibration,
        vendor_auto: VendorAutoState,
    ) -> Decision {
        if self
            .last_observed_ms
            .is_some_and(|previous| now_ms < previous)
        {
            return self.stop(StopReason::ClockRegressed);
        }
        self.last_observed_ms = Some(now_ms);
        let Some(armed) = self.armed.clone() else {
            return self.stop(StopReason::NotArmed);
        };
        if let Some(reason) = preflight(calibration, vendor_auto) {
            return self.stop(reason);
        }
        if armed.calibration != calibration {
            return self.stop(StopReason::CalibrationChanged);
        }
        let Some(target) = target else {
            return self.stop(StopReason::TargetLost);
        };
        if target.camera_epoch != armed.identity.camera_epoch {
            return self.stop(StopReason::CameraEpochChanged);
        }
        if target.target_key != armed.identity.target_key {
            return self.stop(StopReason::TargetChanged);
        }
        if target.ambiguous {
            return self.stop(StopReason::AmbiguousAssociation);
        }
        if target.frame_age_ms > MAX_FRAME_AGE_MS {
            return self.stop(StopReason::StaleFrame);
        }
        if self.last_frame.as_ref().is_some_and(|(epoch, frame)| {
            epoch == &target.camera_epoch && target.frame_id <= *frame
        }) {
            return self.stop(StopReason::FrameNotNew);
        }
        if !valid_box(target.bbox) {
            return self.stop(StopReason::InvalidBox);
        }
        if !readback.pan_degrees.is_finite() || !readback.tilt_degrees.is_finite() {
            return self.stop(StopReason::InvalidReadback);
        }
        if readback.pan_degrees.abs() > PAN_LIMIT_DEGREES
            || readback.tilt_degrees.abs() > TILT_LIMIT_DEGREES
        {
            // Never jump from an out-of-range readback to a boundary: that
            // could exceed the bounded finite-step limit.
            return self.stop(StopReason::ReadbackOutOfRange);
        }
        self.last_frame = Some((target.camera_epoch.clone(), target.frame_id));
        let [x1, y1, x2, y2] = target.bbox;
        let raw = [x1 + x2 - 1.0, y1 + y2 - 1.0];
        let previous = self.smoothed_error.unwrap_or([0.0; 2]);
        let filtered = [
            filtered_axis(previous[0], raw[0]),
            filtered_axis(previous[1], raw[1]),
        ];
        self.smoothed_error = Some(filtered);
        if self
            .last_command_ms
            .is_some_and(|previous| now_ms.saturating_sub(previous) < MIN_COMMAND_INTERVAL_MS)
        {
            return Decision::Hold(HoldReason::RateLimited);
        }
        let step_pan = axis_step(filtered[0]) * f64::from(calibration.pan_sign);
        let step_tilt = axis_step(filtered[1]) * f64::from(calibration.tilt_sign);
        if step_pan == 0.0 && step_tilt == 0.0 {
            return Decision::Hold(HoldReason::DeadZone);
        }
        let pan = (readback.pan_degrees + step_pan).clamp(-PAN_LIMIT_DEGREES, PAN_LIMIT_DEGREES);
        let tilt =
            (readback.tilt_degrees + step_tilt).clamp(-TILT_LIMIT_DEGREES, TILT_LIMIT_DEGREES);
        if pan == readback.pan_degrees && tilt == readback.tilt_degrees {
            return Decision::Hold(HoldReason::AngleLimit);
        }
        self.last_command_ms = Some(now_ms);
        Decision::FinitePosition(FinitePosition {
            pan_degrees: pan,
            tilt_degrees: tilt,
            frame_id: target.frame_id,
            camera_epoch: target.camera_epoch.clone(),
            target_key: target.target_key.clone(),
            arm_generation: self.generation,
            generated_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(MAX_FRAME_AGE_MS - target.frame_age_ms),
        })
    }
}

fn preflight(calibration: Calibration, vendor: VendorAutoState) -> Option<StopReason> {
    if !calibration.valid() {
        Some(StopReason::CalibrationIncompleteOrInvalid)
    } else {
        match vendor {
            VendorAutoState::ConfirmedOff => None,
            VendorAutoState::On => Some(StopReason::VendorTrackingOn),
            VendorAutoState::Unknown => Some(StopReason::VendorTrackingUnknown),
        }
    }
}

fn valid_box([x1, y1, x2, y2]: [f64; 4]) -> bool {
    [x1, y1, x2, y2]
        .iter()
        .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
        && x1 < x2
        && y1 < y2
}

fn filtered_axis(previous: f64, raw: f64) -> f64 {
    if raw.abs() <= DEAD_ZONE {
        return 0.0;
    }
    // Reset an axis on an observed sign crossing rather than driving away
    // from the current raw target while a previous-direction filter decays.
    let previous = if previous * raw < 0.0 { 0.0 } else { previous };
    LOW_PASS_ALPHA * raw + (1.0 - LOW_PASS_ALPHA) * previous
}

fn axis_step(error: f64) -> f64 {
    if error.abs() <= DEAD_ZONE {
        0.0
    } else {
        // Keep whole-degree targets, reducing the step as the face approaches
        // the center. Calibration uses a separate one-degree motion path.
        error.signum() * (error.abs() * 12.0).round().clamp(1.0, MAX_STEP_DEGREES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CAL: Calibration = Calibration {
        complete: true,
        pan_sign: 1,
        tilt_sign: 1,
    };
    const ANGLES: AngleReadback = AngleReadback {
        pan_degrees: 0.0,
        tilt_degrees: 0.0,
    };
    const OFF: VendorAutoState = VendorAutoState::ConfirmedOff;

    fn identity() -> TargetIdentity {
        TargetIdentity {
            camera_epoch: "camera-A".into(),
            target_key: "opaque-target-A".into(),
        }
    }
    fn frame(id: u64) -> TargetFrame {
        TargetFrame {
            frame_id: id,
            frame_age_ms: 20,
            camera_epoch: "camera-A".into(),
            target_key: "opaque-target-A".into(),
            bbox: [0.8, 0.8, 1.0, 1.0],
            ambiguous: false,
        }
    }
    fn armed() -> Follower {
        let mut follower = Follower::new();
        follower.arm(identity(), CAL, OFF).unwrap();
        follower
    }
    fn position(decision: Decision) -> FinitePosition {
        if let Decision::FinitePosition(value) = decision {
            value
        } else {
            panic!("expected finite position, got {decision:?}");
        }
    }
    fn tick(follower: &mut Follower, now: u64, frame: &TargetFrame) -> Decision {
        follower.update(now, Some(frame), ANGLES, CAL, OFF)
    }

    #[test]
    fn a_face_never_arms_itself_and_stop_requires_explicit_rearm() {
        let mut follower = Follower::new();
        assert_eq!(
            tick(&mut follower, 0, &frame(1)),
            Decision::Stop(StopReason::NotArmed)
        );
        follower.arm(identity(), CAL, OFF).unwrap();
        position(tick(&mut follower, 250, &frame(2)));
        follower.stop(StopReason::OperatorStop);
        assert!(follower.smoothed_error.is_none());
        assert_eq!(
            tick(&mut follower, 500, &frame(3)),
            Decision::Stop(StopReason::NotArmed)
        );
    }
    #[test]
    fn missing_old_or_ambiguous_target_immediately_disarms() {
        for (target, reason) in [
            (None, StopReason::TargetLost),
            (
                Some(TargetFrame {
                    frame_age_ms: 251,
                    ..frame(2)
                }),
                StopReason::StaleFrame,
            ),
            (
                Some(TargetFrame {
                    ambiguous: true,
                    ..frame(2)
                }),
                StopReason::AmbiguousAssociation,
            ),
        ] {
            let mut follower = armed();
            position(tick(&mut follower, 0, &frame(1)));
            assert_eq!(
                follower.update(1, target.as_ref(), ANGLES, CAL, OFF),
                Decision::Stop(reason)
            );
            assert!(!follower.is_armed());
            assert!(follower.smoothed_error.is_none());
        }
    }
    #[test]
    fn repeated_and_out_of_order_frames_stop_even_inside_rate_limit() {
        for id in [9, 10] {
            let mut follower = armed();
            position(tick(&mut follower, 0, &frame(10)));
            assert_eq!(
                tick(&mut follower, 1, &frame(id)),
                Decision::Stop(StopReason::FrameNotNew)
            );
        }
    }
    #[test]
    fn camera_epoch_and_target_changes_never_switch_automatically() {
        for (next, reason) in [
            (
                TargetFrame {
                    camera_epoch: "camera-B".into(),
                    ..frame(2)
                },
                StopReason::CameraEpochChanged,
            ),
            (
                TargetFrame {
                    target_key: "other-face".into(),
                    ..frame(2)
                },
                StopReason::TargetChanged,
            ),
        ] {
            let mut follower = armed();
            position(tick(&mut follower, 0, &frame(1)));
            assert_eq!(tick(&mut follower, 250, &next), Decision::Stop(reason));
            assert_eq!(
                tick(&mut follower, 500, &next),
                Decision::Stop(StopReason::NotArmed)
            );
        }
    }
    #[test]
    fn arm_rejects_invalid_signs_incomplete_calibration_and_unconfirmed_vendor_state() {
        for calibration in [
            Calibration {
                complete: false,
                ..CAL
            },
            Calibration { pan_sign: 0, ..CAL },
            Calibration {
                tilt_sign: 2,
                ..CAL
            },
        ] {
            assert_eq!(
                Follower::new().arm(identity(), calibration, OFF),
                Err(StopReason::CalibrationIncompleteOrInvalid)
            );
        }
        assert_eq!(
            Follower::new().arm(identity(), CAL, VendorAutoState::On),
            Err(StopReason::VendorTrackingOn)
        );
        assert_eq!(
            Follower::new().arm(identity(), CAL, VendorAutoState::Unknown),
            Err(StopReason::VendorTrackingUnknown)
        );
        assert_eq!(
            Follower::new().arm(
                TargetIdentity {
                    target_key: String::new(),
                    ..identity()
                },
                CAL,
                OFF
            ),
            Err(StopReason::InvalidSelection)
        );
    }
    #[test]
    fn calibration_or_vendor_state_change_stops_a_live_arm() {
        for (calibration, vendor, reason) in [
            (
                Calibration {
                    complete: false,
                    ..CAL
                },
                OFF,
                StopReason::CalibrationIncompleteOrInvalid,
            ),
            (
                Calibration {
                    pan_sign: -1,
                    ..CAL
                },
                OFF,
                StopReason::CalibrationChanged,
            ),
            (
                CAL,
                VendorAutoState::Unknown,
                StopReason::VendorTrackingUnknown,
            ),
            (CAL, VendorAutoState::On, StopReason::VendorTrackingOn),
        ] {
            let mut follower = armed();
            position(tick(&mut follower, 0, &frame(1)));
            assert_eq!(
                follower.update(1, Some(&frame(2)), ANGLES, calibration, vendor),
                Decision::Stop(reason)
            );
            assert!(!follower.is_armed());
        }
    }
    #[test]
    fn calibrated_signs_apply_to_original_coordinates_with_low_pass_filtering() {
        for (pan_sign, tilt_sign) in [(1, -1), (-1, 1)] {
            let calibration = Calibration {
                pan_sign,
                tilt_sign,
                ..CAL
            };
            let mut follower = Follower::new();
            follower.arm(identity(), calibration, OFF).unwrap();
            let next = position(follower.update(0, Some(&frame(1)), ANGLES, calibration, OFF));
            assert_eq!(next.pan_degrees.signum(), f64::from(pan_sign));
            assert_eq!(next.tilt_degrees.signum(), f64::from(tilt_sign));
            assert!(next.pan_degrees.abs() <= 3.0 && next.tilt_degrees.abs() <= 3.0);
            assert!(
                follower
                    .smoothed_error
                    .unwrap()
                    .iter()
                    .all(|v| (v - 0.28).abs() < 1e-10)
            );
        }
    }
    #[test]
    fn output_is_bounded_to_three_degrees_per_axis_every_250ms() {
        let mut follower = armed();
        position(tick(&mut follower, 0, &frame(1)));
        assert_eq!(
            tick(&mut follower, 249, &frame(2)),
            Decision::Hold(HoldReason::RateLimited)
        );
        let next = position(tick(&mut follower, 250, &frame(3)));
        assert!(next.pan_degrees.abs() <= 3.0 && next.tilt_degrees.abs() <= 3.0);
        assert!(next.pan_degrees.abs() > 1.0 && next.tilt_degrees.abs() > 1.0);
        assert_eq!(next.frame_id, 3);
        assert_eq!(next.expires_at_ms, 480);
    }
    #[test]
    fn absolute_angle_bounds_do_not_create_a_large_snap_from_readback() {
        let mut follower = armed();
        let angles = AngleReadback {
            pan_degrees: 119.8,
            tilt_degrees: 54.9,
        };
        let next = position(follower.update(0, Some(&frame(1)), angles, CAL, OFF));
        assert_eq!(next.pan_degrees, 120.0);
        assert_eq!(next.tilt_degrees, 55.0);
        assert!(
            (next.pan_degrees - angles.pan_degrees).abs() <= 1.0
                && (next.tilt_degrees - angles.tilt_degrees).abs() <= 3.0
        );
        let mut follower = armed();
        let initial = AngleReadback {
            tilt_degrees: 47.0,
            ..ANGLES
        };
        let next = position(follower.update(0, Some(&frame(1)), initial, CAL, OFF));
        assert!(next.tilt_degrees > 47.0 && next.tilt_degrees - initial.tilt_degrees <= 3.0);
        let mut follower = armed();
        assert_eq!(
            follower.update(
                0,
                Some(&frame(1)),
                AngleReadback {
                    tilt_degrees: 55.1,
                    ..ANGLES
                },
                CAL,
                OFF
            ),
            Decision::Stop(StopReason::ReadbackOutOfRange)
        );
        let mut follower = armed();
        assert_eq!(
            follower.update(
                0,
                Some(&frame(1)),
                AngleReadback {
                    pan_degrees: 121.0,
                    ..ANGLES
                },
                CAL,
                OFF
            ),
            Decision::Stop(StopReason::ReadbackOutOfRange)
        );
    }
    #[test]
    fn stop_clears_filter_but_rearming_cannot_bypass_command_budget() {
        let mut follower = armed();
        position(tick(&mut follower, 0, &frame(1)));
        let generation = follower.generation();
        follower.stop(StopReason::TargetLost);
        assert!(follower.smoothed_error.is_none());
        assert_ne!(generation, follower.generation());
        follower.arm(identity(), CAL, OFF).unwrap();
        assert_eq!(
            tick(&mut follower, 1, &frame(2)),
            Decision::Hold(HoldReason::RateLimited)
        );
        follower.stop(StopReason::OperatorStop);
        follower.arm(identity(), CAL, OFF).unwrap();
        let mut left = frame(3);
        left.bbox = [0.0, 0.0, 0.2, 0.2];
        let next = position(tick(&mut follower, 250, &left));
        assert!(next.pan_degrees < 0.0 && next.tilt_degrees < 0.0);
        assert!(next.pan_degrees >= -3.0 && next.tilt_degrees >= -3.0);
    }
    #[test]
    fn invalid_geometry_numbers_or_clock_stop_instead_of_emitting_positions() {
        for bbox in [
            [f64::NAN, 0.2, 0.4, 0.6],
            [-0.1, 0.2, 0.4, 0.6],
            [0.4, 0.2, 0.4, 0.6],
        ] {
            let mut follower = armed();
            assert_eq!(
                tick(&mut follower, 0, &TargetFrame { bbox, ..frame(1) }),
                Decision::Stop(StopReason::InvalidBox)
            );
        }
        let mut follower = armed();
        assert_eq!(
            follower.update(
                0,
                Some(&frame(1)),
                AngleReadback {
                    pan_degrees: f64::INFINITY,
                    ..ANGLES
                },
                CAL,
                OFF
            ),
            Decision::Stop(StopReason::InvalidReadback)
        );
        let mut follower = armed();
        position(tick(&mut follower, 250, &frame(1)));
        assert_eq!(
            tick(&mut follower, 249, &frame(2)),
            Decision::Stop(StopReason::ClockRegressed)
        );
    }
    #[test]
    fn centered_target_holds_and_direction_crossing_cannot_continue_away() {
        let mut follower = armed();
        let centered = TargetFrame {
            bbox: [0.45, 0.45, 0.55, 0.55],
            ..frame(1)
        };
        assert_eq!(
            tick(&mut follower, 0, &centered),
            Decision::Hold(HoldReason::DeadZone)
        );
        assert!(follower.is_armed());
        position(tick(&mut follower, 250, &frame(2)));
        let left = TargetFrame {
            bbox: [0.0, 0.0, 0.2, 0.2],
            ..frame(3)
        };
        let next = position(tick(&mut follower, 500, &left));
        assert!(next.pan_degrees < 0.0 && next.tilt_degrees < 0.0);
    }
    #[test]
    fn replacing_a_live_arm_requires_stop_and_fresh_rearm() {
        let mut follower = armed();
        assert_eq!(
            follower.arm(
                TargetIdentity {
                    target_key: "other".into(),
                    ..identity()
                },
                CAL,
                OFF
            ),
            Err(StopReason::TargetChanged)
        );
        assert!(!follower.is_armed());
        follower
            .arm(
                TargetIdentity {
                    target_key: "other".into(),
                    ..identity()
                },
                CAL,
                OFF,
            )
            .unwrap();
        assert!(follower.is_armed());
    }
    #[test]
    fn expired_proposal_uses_distinct_fresh_frame_with_unchanged_finite_target() {
        let mut follower = armed();
        let proposal = position(tick(&mut follower, 0, &frame(1)));
        assert!(proposal.expires_at_ms < 300);
        let newest = TargetFrame {
            frame_age_ms: 15,
            ..frame(2)
        };
        let authority = follower
            .refresh_command(300, Some(&newest), ANGLES, CAL, OFF, &proposal)
            .unwrap();
        assert_eq!(authority.proposal_frame_id, 1);
        assert_eq!(authority.frame_id, 2);
        assert_eq!(authority.generated_at_ms, 300);
        assert_eq!(authority.expires_at_ms, 535);
        assert_eq!(authority.frame_age_at_refresh_ms, 15);
        assert_eq!(authority.pan_degrees, proposal.pan_degrees);
        assert_eq!(authority.tilt_degrees, proposal.tilt_degrees);
        assert_eq!(
            proposal.expires_at_ms, 230,
            "old frame lease is never modified"
        );
        assert!(authority.pan_degrees.abs() <= 3.0 && authority.tilt_degrees.abs() <= 3.0);
    }
    #[test]
    fn same_frame_cannot_extend_lease_and_regressed_or_stale_frames_stop() {
        let mut follower = armed();
        let proposal = position(tick(&mut follower, 0, &frame(10)));
        assert_eq!(
            follower.refresh_command(300, Some(&frame(10)), ANGLES, CAL, OFF, &proposal),
            Err(RefreshError::Replan(ReplanReason::NoNewerFrame))
        );
        assert_eq!(
            follower.refresh_command(300, Some(&frame(9)), ANGLES, CAL, OFF, &proposal),
            Err(RefreshError::Stop(StopReason::FrameNotNew))
        );
        let stale = TargetFrame {
            frame_age_ms: 251,
            ..frame(11)
        };
        assert_eq!(
            follower.refresh_command(300, Some(&stale), ANGLES, CAL, OFF, &proposal),
            Err(RefreshError::Stop(StopReason::StaleFrame))
        );
    }
    #[test]
    fn fresh_center_or_reversal_soft_replans_without_authorizing_old_direction() {
        let mut follower = armed();
        let proposal = position(tick(&mut follower, 0, &frame(1)));
        for bbox in [
            [0.45, 0.45, 0.55, 0.55],
            [0.0, 0.0, 0.2, 0.2],
            [0.8, 0.0, 1.0, 0.2],
        ] {
            let latest = TargetFrame { bbox, ..frame(2) };
            assert_eq!(
                follower.refresh_command(300, Some(&latest), ANGLES, CAL, OFF, &proposal),
                Err(RefreshError::Replan(ReplanReason::DirectionNoLongerValid))
            );
            assert!(
                follower.is_armed(),
                "replanning does not silently change identity or arm"
            );
        }
    }
    #[test]
    fn refresh_rejects_generation_epoch_key_ambiguity_and_oversized_action() {
        let mut follower = armed();
        let proposal = position(tick(&mut follower, 0, &frame(1)));
        for (latest, reason) in [
            (
                TargetFrame {
                    camera_epoch: "other".into(),
                    ..frame(2)
                },
                StopReason::CameraEpochChanged,
            ),
            (
                TargetFrame {
                    target_key: "other".into(),
                    ..frame(2)
                },
                StopReason::TargetChanged,
            ),
            (
                TargetFrame {
                    ambiguous: true,
                    ..frame(2)
                },
                StopReason::AmbiguousAssociation,
            ),
        ] {
            assert_eq!(
                follower.refresh_command(300, Some(&latest), ANGLES, CAL, OFF, &proposal),
                Err(RefreshError::Stop(reason))
            );
        }
        let mut changed = proposal.clone();
        changed.arm_generation = changed.arm_generation.wrapping_add(1);
        assert_eq!(
            follower.refresh_command(300, Some(&frame(2)), ANGLES, CAL, OFF, &changed),
            Err(RefreshError::Stop(StopReason::CommandGenerationChanged))
        );
        changed = proposal.clone();
        changed.pan_degrees = 1.01;
        assert_eq!(
            follower.refresh_command(300, Some(&frame(2)), ANGLES, CAL, OFF, &changed),
            Err(RefreshError::Stop(StopReason::InvalidCommand))
        );
        follower.stop(StopReason::OperatorStop);
        assert_eq!(
            follower.refresh_command(300, Some(&frame(2)), ANGLES, CAL, OFF, &proposal),
            Err(RefreshError::Stop(StopReason::NotArmed))
        );
    }
    #[test]
    fn fresh_raw_error_must_agree_with_actual_calibrated_step_signs() {
        let calibration = Calibration {
            pan_sign: -1,
            tilt_sign: 1,
            ..CAL
        };
        let mut follower = Follower::new();
        follower.arm(identity(), calibration, OFF).unwrap();
        let proposal = position(follower.update(0, Some(&frame(1)), ANGLES, calibration, OFF));
        assert!(proposal.pan_degrees < 0.0 && proposal.tilt_degrees > 0.0);
        let renewed = follower
            .refresh_command(300, Some(&frame(2)), ANGLES, calibration, OFF, &proposal)
            .unwrap();
        assert_eq!(
            (renewed.pan_degrees, renewed.tilt_degrees),
            (proposal.pan_degrees, proposal.tilt_degrees)
        );
        let reverse_pan = TargetFrame {
            bbox: [0.0, 0.8, 0.2, 1.0],
            ..frame(3)
        };
        assert_eq!(
            follower.refresh_command(300, Some(&reverse_pan), ANGLES, calibration, OFF, &proposal),
            Err(RefreshError::Replan(ReplanReason::DirectionNoLongerValid))
        );
    }
    #[test]
    fn actual_dispatch_time_preserves_250ms_budget_after_long_verification() {
        let mut follower = armed();
        position(tick(&mut follower, 0, &frame(1)));
        follower.record_dispatch(900).unwrap();
        assert_eq!(
            tick(&mut follower, 1100, &frame(2)),
            Decision::Hold(HoldReason::RateLimited)
        );
        position(tick(&mut follower, 1150, &frame(3)));
    }
}

//! Explicit integer-readback model for production following, not calibration.
use crate::{
    obsbot_motion::Pose,
    ptz_follow::{MAX_STEP_DEGREES, PAN_LIMIT_DEGREES, TILT_LIMIT_DEGREES},
};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Assessment {
    pub safe: bool,
    pub arrival: bool,
    pub reason: &'static str,
}
fn axis(start: i32, target: f64, native: i32, raw: i32) -> Assessment {
    let fail = |reason| Assessment {
        safe: false,
        arrival: false,
        reason,
    };
    if !target.is_finite()
        || target.fract() != 0.0
        || (target - f64::from(start)).abs() > MAX_STEP_DEGREES
    {
        return fail("invalid_finite_target");
    }
    let s = f64::from(start);
    let n = f64::from(native);
    let r = f64::from(raw);
    let d = target - s;
    if d == 0.0 {
        return if (n - s).abs() <= 1.0 {
            Assessment {
                safe: true,
                arrival: (r - target).abs() <= 1.0,
                reason: "held_axis_integer_band",
            }
        } else {
            fail("uncommanded_axis_changed")
        };
    }
    let safe = if d > 0.0 {
        n >= s - 1.0 && n <= target + 2.0
    } else {
        n <= s + 1.0 && n >= target - 2.0
    };
    if !safe {
        return fail("outside_start_to_target_integer_band");
    }
    let progress = if d > 0.0 { n > s } else { n < s };
    Assessment {
        safe: true,
        arrival: progress && (n - target).abs() <= 1.0 && (r - target).abs() <= 1.0,
        reason: if progress {
            "quantized_progress"
        } else {
            "not_moved"
        },
    }
}
pub fn assess(start: Pose, pan: f64, tilt: f64, native: Pose, raw: Pose) -> Assessment {
    if f64::from(native.yaw_degrees).abs() > PAN_LIMIT_DEGREES
        || f64::from(native.pitch_degrees).abs() > TILT_LIMIT_DEGREES
    {
        return Assessment {
            safe: false,
            arrival: false,
            reason: "outside_global_limits",
        };
    }
    let a = axis(start.yaw_degrees, pan, native.yaw_degrees, raw.yaw_degrees);
    let b = axis(
        start.pitch_degrees,
        tilt,
        native.pitch_degrees,
        raw.pitch_degrees,
    );
    if !a.safe {
        return a;
    }
    if !b.safe {
        return b;
    }
    Assessment {
        safe: true,
        arrival: a.arrival && b.arrival,
        reason: if a.arrival && b.arrival {
            "candidate_requires_distinct_stable_sample"
        } else {
            "in_transit"
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn p(yaw: i32, pitch: i32) -> Pose {
        Pose {
            yaw_degrees: yaw,
            pitch_degrees: pitch,
            roll_unused_degrees: 0,
        }
    }
    #[test]
    fn integer_boundary_can_arrive_but_unchanged_start_cannot() {
        let s = p(19, -29);
        assert!(assess(s, 19., -28., p(19, -27), p(18, -27)).arrival);
        assert!(!assess(s, 19., -28., s, p(18, -28)).arrival);
    }
    #[test]
    fn reverse_extra_motion_and_uncommanded_axis_stop() {
        let s = p(19, -29);
        for n in [p(19, -31), p(19, -25), p(21, -28)] {
            assert!(!assess(s, 19., -28., n, n).safe);
        }
    }
    #[test]
    fn raw_target_agreement_and_global_limit_still_required() {
        assert!(!assess(p(19, -29), 19., -28., p(19, -27), p(18, -26)).arrival);
        assert!(!assess(p(19, 54), 19., 55., p(19, 56), p(19, 55)).safe);
        assert!(!assess(p(19, 20), 19., 24., p(19, 24), p(19, 24)).safe);
    }

    #[test]
    fn one_unit_start_boundary_waits_but_cannot_count_as_arrival() {
        let a = assess(p(21, -26), 22., -25., p(22, -27), p(21, -26));
        assert!(a.safe);
        assert!(!a.arrival);
        let overshoot = assess(p(22, -19), 22., -22., p(22, -24), p(22, -23));
        assert!(
            overshoot.safe,
            "observed bounded transit must be allowed to settle"
        );
        assert!(
            !overshoot.arrival,
            "a transit overshoot is never successful arrival"
        );
        assert!(assess(p(21, -26), 24., -23., p(24, -23), p(23, -23)).arrival);
        assert!(!assess(p(21, -26), 24., -23., p(22, -25), p(22, -25)).arrival);
    }
}

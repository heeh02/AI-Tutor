//! Read standard controls through the existing capture owner, never a second device.
use nokhwa::{
    Camera,
    utils::{ControlValueDescription, KnownCameraControl, KnownCameraControlFlag},
};
use serde_json::{Value, json};

fn axis(camera: &Camera, control: KnownCameraControl) -> Result<Value, String> {
    let value = camera.camera_control(control).map_err(|e| e.to_string())?;
    if !value.active()
        || !value.flag().contains(&KnownCameraControlFlag::Manual)
        || value.flag().contains(&KnownCameraControlFlag::Automatic)
    {
        return Err("native control is not a manual readback".into());
    }
    match value.description() {
        ControlValueDescription::IntegerRange {
            value,
            min,
            max,
            step,
            ..
        } if *step == 1 && value >= min && value <= max && i32::try_from(*value).is_ok() => Ok(
            json!({"value":value,"minimum":min,"maximum":max,"step":step,
                "manual":true,"query_success":true}),
        ),
        _ => Err("native control is not a one-degree integer range".into()),
    }
}

pub fn sample(camera: &Camera, device: &str, epoch: &str, sequence: u64) -> Value {
    sample_observed(camera, device, epoch, sequence, |_| {})
}

pub fn sample_observed(
    camera: &Camera,
    device: &str,
    epoch: &str,
    sequence: u64,
    mut observe: impl FnMut(&'static str),
) -> Value {
    let result = (|| {
        observe("native_pan_get");
        let pan = axis(camera, KnownCameraControl::Pan)?;
        observe("native_tilt_get");
        let tilt = axis(camera, KnownCameraControl::Tilt)?;
        Ok::<_, String>((pan, tilt))
    })();
    let at = chrono::Utc::now();
    let mut value = json!({"schema_version":"1.0","source":"camera_capture_owner",
        "device_id":device,"camera_session_id":epoch,"sample_seq":sequence,
        "sampled_at_ms":at.timestamp_millis(),"sampled_at_utc":at.to_rfc3339(),
        "read_only":true,"set_calls":0,"additional_device_opens":0});
    match result {
        Ok((pan, tilt)) => {
            value["status"] = json!("ok");
            value["pan"] = pan;
            value["tilt"] = tilt;
        }
        Err(error) => {
            value["status"] = json!("unavailable");
            value["error"] = json!(error.chars().take(400).collect::<String>());
        }
    }
    value
}

pub fn snapshot(meta: &Value, now_ms: u64) -> Result<Value, String> {
    if meta["state"] != "running" {
        return Err("camera capture owner is not running".into());
    }
    if !meta["frame_age_ms"]
        .as_f64()
        .is_some_and(|v| v.is_finite() && (0.0..=250.0).contains(&v))
    {
        return Err("camera capture frame is stale".into());
    }
    let mut pose = meta["native_pose"].clone();
    if pose["source"] != "camera_capture_owner" || pose["status"] != "ok" {
        return Err(format!(
            "capture-owner pose unavailable: {}",
            pose["error"].as_str().unwrap_or("no control sample")
        ));
    }
    if pose["camera_session_id"] != meta["camera_session_id"] {
        return Err("capture-owner pose epoch mismatch".into());
    }
    let sample_at = pose["sampled_at_ms"]
        .as_u64()
        .ok_or("capture-owner timestamp missing")?;
    let age = now_ms
        .checked_sub(sample_at)
        .ok_or("capture-owner clock regressed")?;
    if age > 250 {
        return Err("capture-owner pose is stale".into());
    }
    pose["sample_age_ms"] = json!(age);
    pose["frame_age_ms"] = meta["frame_age_ms"].clone();
    pose["frame_id"] = meta["frame_id"].clone();
    pose["server_pid"] = json!(std::process::id());
    Ok(pose)
}

/// This tolerance is for passive display only. Motion authorization continues
/// to use snapshot() and its existing 250 ms bound.
pub const DISPLAY_POSE_MAX_AGE_MS: u64 = 1_000;

fn display_axis(sample: &Value, name: &str, limit: i64) -> Option<i64> {
    let axis = &sample[name];
    let value = axis["value"].as_i64()?;
    let min = axis["minimum"].as_i64()?;
    let max = axis["maximum"].as_i64()?;
    (axis["manual"] == true
        && axis["query_success"] == true
        && axis["step"] == 1
        && min <= value
        && value <= max
        && (-limit..=limit).contains(&value))
    .then_some(value)
}

/// Projects only the existing capture owner's readback into a UI pose.
///
/// This function cannot open a device, read controls, select a target, arm PTZ,
/// or issue motion. A driver control Get is controller-reported position, not
/// independently verified encoder or optical measurement. In particular, PTZ
/// targets, settling history and historical capability files are not inputs.
/// Native tilt is preserved here; OSC canonical pitch uses the opposite sign.
pub fn display_pose(meta: &Value, now_ms: u64) -> Value {
    let sample = &meta["native_pose"];
    let state = meta["state"].as_str().unwrap_or("unknown");
    let connected = state == "running";
    let epoch = meta["camera_session_id"].as_str().unwrap_or("");
    let seq = sample["sample_seq"].as_u64().filter(|v| *v > 0);
    let sampled_at = sample["sampled_at_ms"].as_u64();
    let sample_age = sampled_at.and_then(|at| now_ms.checked_sub(at));
    let frame_age = meta["frame_age_ms"]
        .as_f64()
        .filter(|age| age.is_finite() && *age >= 0.0);
    let pan = display_axis(sample, "pan", 180);
    let tilt = display_axis(sample, "tilt", 90);

    let protocol_valid = sample["schema_version"] == "1.0"
        && sample["source"] == "camera_capture_owner"
        && sample["status"] == "ok"
        && sample["read_only"] == true
        && sample["set_calls"] == 0
        && sample["additional_device_opens"] == 0;
    let identity_valid = !epoch.is_empty() && sample["camera_session_id"] == epoch && seq.is_some();
    let (source, freshness, reason) = if !connected {
        ("unknown", "unavailable", "capture_inactive")
    } else if !protocol_valid {
        ("unknown", "unavailable", "feedback_unavailable")
    } else if !identity_valid {
        ("unknown", "unavailable", "capture_session_mismatch")
    } else if pan.is_none() || tilt.is_none() {
        ("unknown", "unavailable", "invalid_control_readback")
    } else if sample_age.is_none() || frame_age.is_none() {
        ("unknown", "unavailable", "invalid_sample_time")
    } else if sample_age.unwrap() > DISPLAY_POSE_MAX_AGE_MS {
        ("controller_readback", "stale", "stale_feedback")
    } else if frame_age.unwrap() > DISPLAY_POSE_MAX_AGE_MS as f64 {
        ("controller_readback", "stale", "stale_frame")
    } else {
        ("controller_readback", "fresh", "fresh_controller_readback")
    };
    let active = freshness == "fresh";
    json!({
        "pose_source": source,
        "pan_deg": if active { pan } else { None },
        "tilt_deg": if active { tilt } else { None },
        "freshness": freshness,
        // connected refers to the capture owner, not a separate USB probe.
        // active means fresh display feedback, not enabled physical following.
        "connected": connected,
        "active": active,
        "revision": format!("{epoch}:{}:{state}:{freshness}", seq.unwrap_or(0)),
        "camera_session_id": meta["camera_session_id"],
        "sample_seq": seq,
        "sampled_at_ms": sampled_at,
        "sample_age_ms": sample_age,
        "frame_age_ms": frame_age,
        "max_age_ms": DISPLAY_POSE_MAX_AGE_MS,
        "reason": reason,
        "read_only": true,
        "coordinate_system": "windows_native_pan_tilt_degrees",
        "encoder_verified": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn meta() -> Value {
        json!({"state":"running","frame_age_ms":80,"frame_id":7,"camera_session_id":"epoch-a",
        "native_pose":{"status":"ok","source":"camera_capture_owner","camera_session_id":"epoch-a","sampled_at_ms":900,"sample_seq":2}})
    }
    #[test]
    fn snapshot_is_fresh_scalar_and_rejects_old_or_different_epoch() {
        let m = meta();
        assert_eq!(snapshot(&m, 1000).unwrap()["sample_age_ms"], 100);
        assert!(snapshot(&m, 1200).is_err());
        assert!(snapshot(&m, 899).is_err());
        let mut changed = m.clone();
        changed["camera_session_id"] = json!("epoch-b");
        assert!(snapshot(&changed, 1000).is_err());
        changed = m.clone();
        changed["state"] = json!("error");
        assert!(snapshot(&changed, 1000).is_err());
        changed = m.clone();
        changed["native_pose"]["status"] = json!("unavailable");
        assert!(snapshot(&changed, 1000).is_err());
    }

    fn display_meta() -> Value {
        let axis = |value| {
            json!({"value":value,"minimum":-90,"maximum":90,"step":1,
                "manual":true,"query_success":true})
        };
        json!({"state":"running","frame_age_ms":80,"camera_session_id":"epoch-a",
            "native_pose":{"schema_version":"1.0","status":"ok","source":"camera_capture_owner",
                "camera_session_id":"epoch-a","sampled_at_ms":900,"sample_seq":2,
                "read_only":true,"set_calls":0,"additional_device_opens":0,
                "pan":axis(17),"tilt":axis(24)}})
    }

    #[test]
    fn display_uses_controller_readback_preserves_native_sign_and_never_commands() {
        let mut meta = display_meta();
        meta["ptz"] = json!({"mode":"off","last_command":{"pan":99,"tilt":-99},
            "last_settling_readback":{"native":{"yaw_degrees":77,"pitch_degrees":-88}},
            "capabilities":{"pan":{"current":66}}});
        let before = meta.clone();
        let pose = display_pose(&meta, 1000);
        assert_eq!(pose["pose_source"], "controller_readback");
        assert_eq!(pose["pan_deg"], 17);
        assert_eq!(pose["tilt_deg"], 24);
        assert_eq!(pose["freshness"], "fresh");
        assert_eq!(pose["active"], true);
        assert_eq!(pose["connected"], true);
        assert_eq!(pose["read_only"], true);
        assert_eq!(pose["encoder_verified"], false);
        assert_eq!(pose["sample_age_ms"], 100);
        assert_eq!(
            meta, before,
            "projection must not mutate camera or PTZ state"
        );

        meta["native_pose"] = Value::Null;
        let unknown = display_pose(&meta, 1000);
        assert_eq!(unknown["pose_source"], "unknown");
        assert!(unknown["pan_deg"].is_null());
        assert!(unknown["tilt_deg"].is_null());
        assert_eq!(unknown["active"], false);
    }

    #[test]
    fn display_expires_readback_without_relaxing_motion_snapshot() {
        let mut meta = display_meta();
        assert!(snapshot(&meta, 1150).is_ok());
        assert!(snapshot(&meta, 1151).is_err());
        let edge = display_pose(&meta, 1900);
        assert_eq!(edge["freshness"], "fresh");
        let stale = display_pose(&meta, 1901);
        assert_eq!(stale["freshness"], "stale");
        assert_eq!(stale["pose_source"], "controller_readback");
        assert_eq!(stale["active"], false);
        assert!(stale["pan_deg"].is_null());
        assert!(stale["tilt_deg"].is_null());
        assert_ne!(edge["revision"], stale["revision"]);
        meta["frame_age_ms"] = json!(1001);
        assert_eq!(display_pose(&meta, 1000)["reason"], "stale_frame");
        meta["state"] = json!("error");
        let disconnected = display_pose(&meta, 1000);
        assert_eq!(disconnected["connected"], false);
        assert_eq!(disconnected["freshness"], "unavailable");
        assert!(disconnected["pan_deg"].is_null());
    }

    #[test]
    fn display_rejects_wrong_epoch_clock_and_untrusted_axis_metadata() {
        let fixture = display_meta();
        for (pointer, invalid) in [
            ("/camera_session_id", json!("epoch-b")),
            ("/camera_session_id", json!("")),
            ("/native_pose/source", json!("last_command")),
            ("/native_pose/status", json!("unavailable")),
            ("/native_pose/read_only", json!(false)),
            ("/native_pose/set_calls", json!(1)),
            ("/native_pose/additional_device_opens", json!(1)),
            ("/native_pose/sample_seq", json!(0)),
            ("/native_pose/sampled_at_ms", json!(1001)),
            ("/native_pose/pan/value", json!(91)),
            ("/native_pose/pan/manual", json!(false)),
            ("/native_pose/tilt/query_success", json!(false)),
            ("/native_pose/tilt/step", json!(2)),
            ("/native_pose/tilt/value", json!("24")),
            ("/frame_age_ms", json!(-1)),
        ] {
            let mut meta = fixture.clone();
            *meta.pointer_mut(pointer).unwrap() = invalid;
            let pose = display_pose(&meta, 1000);
            assert_eq!(pose["pose_source"], "unknown", "{pointer}");
            assert_eq!(pose["active"], false, "{pointer}");
            assert!(pose["pan_deg"].is_null(), "{pointer}");
            assert!(pose["tilt_deg"].is_null(), "{pointer}");
        }
    }

    #[test]
    fn display_revision_follows_samples_and_capture_epochs() {
        let mut meta = display_meta();
        let initial = display_pose(&meta, 1000);
        assert_eq!(initial["revision"], display_pose(&meta, 1100)["revision"]);
        meta["native_pose"]["sample_seq"] = json!(3);
        let next = display_pose(&meta, 1100);
        assert_ne!(initial["revision"], next["revision"]);
        meta["camera_session_id"] = json!("epoch-b");
        meta["native_pose"]["camera_session_id"] = json!("epoch-b");
        let reconnected = display_pose(&meta, 1100);
        assert_ne!(next["revision"], reconnected["revision"]);
        assert_eq!(reconnected["active"], true);
    }
}

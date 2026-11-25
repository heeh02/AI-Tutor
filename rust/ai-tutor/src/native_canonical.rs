//! Fixed, read-only native helper used only by the explicit canonical pose mode.
//! No shell, output file, motor command, stop command or fallback is available.
use crate::obsbot_motion::Pose;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::Value;
use std::{io::Read, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::Semaphore,
    time::{Instant, timeout},
};

pub const NATIVE_READ_TIMEOUT: Duration = Duration::from_millis(900);
const STDOUT_CAP: usize = 128 * 1024;
const STDERR_CAP: usize = 32 * 1024;
static READ_SLOT: Semaphore = Semaphore::const_new(1);

#[derive(Clone, Debug, Serialize)]
pub struct NativeCanonicalEvidence {
    pub source: &'static str,
    pub pose: Pose,
    pub native_pan_degrees: i32,
    pub native_tilt_degrees: i32,
    pub saved_camera_device_path: String,
    pub observed_native_device_path: String,
    pub device_binding_verified: bool,
    pub native_timestamp_utc: String,
    pub elapsed_ms: u128,
    pub limitation: &'static str,
}

fn read_saved(root: &Path) -> Result<String> {
    let mut file = std::fs::File::open(root.join("runtime/rust_camera_device.txt"))
        .context("canonical_saved_device_missing")?;
    let mut data = String::new();
    Read::by_ref(&mut file)
        .take(4097)
        .read_to_string(&mut data)?;
    ensure!(data.len() <= 4096, "canonical_device_path_limit");
    let path = data.trim();
    ensure!(
        !path.is_empty() && !path.contains(['\0', '\r', '\n']),
        "canonical_saved_device_invalid"
    );
    Ok(path.to_owned())
}

/// Match the full USB instance; only the known MF/DirectShow interface GUID is
/// normalized. Device names and VID/PID alone never establish same-instance.
fn physical_instance(path: &str) -> Result<String> {
    ensure!(
        path.len() <= 4096 && !path.contains(['\0', '\r', '\n']),
        "canonical_device_path_invalid"
    );
    let lower = path.to_ascii_lowercase();
    ensure!(
        lower.starts_with(r"\\?\usb#") && lower.contains("vid_3564&pid_ff02"),
        "canonical_tiny3_usb_path_required"
    );
    let (instance, interface) = lower
        .rsplit_once("#{")
        .context("canonical_camera_interface_missing")?;
    ensure!(
        matches!(
            interface,
            "e5323777-f976-4f5b-9b55-b94699c46e44}\\global"
                | "65e8773d-8f56-11d0-a3b9-00a0c9223196}\\global"
        ),
        "canonical_camera_interface_unrecognized"
    );
    ensure!(!instance.is_empty(), "canonical_usb_instance_missing");
    Ok(instance.to_owned())
}

fn parse_report(value: &Value, saved: &str, elapsed_ms: u128) -> Result<NativeCanonicalEvidence> {
    ensure!(
        value["result"] == "PASS_READ_ONLY_QUERY"
            && value["read_only"] == true
            && value["exact_device_path_match"] == true,
        "canonical_native_query_not_verified"
    );
    ensure!(
        value["set_calls"] == 0
            && value["ptz_motion_commands"] == 0
            && value["capture_graph_created"] == false
            && value["capture_stream_started"] == false,
        "canonical_native_side_effect_contract"
    );
    ensure!(
        value["sample_count"] == 1 && value["sample_interval_ms"] == 0,
        "canonical_exactly_one_fresh_sample_required"
    );
    let samples = value["samples"]
        .as_array()
        .filter(|samples| samples.len() == 1)
        .context("canonical_sample_shape")?;
    let actual = value["matched_device"]["device_path"]
        .as_str()
        .context("canonical_native_device_missing")?;
    let expected = value["expected_device_path"]
        .as_str()
        .context("canonical_native_expected_device_missing")?;
    ensure!(
        actual.eq_ignore_ascii_case(expected),
        "canonical_native_exact_path_mismatch"
    );
    ensure!(
        physical_instance(saved)? == physical_instance(actual)?,
        "canonical_video_native_usb_instance_mismatch"
    );
    let sample = &samples[0];
    for axis in ["pan", "tilt"] {
        ensure!(
            sample[axis]["get_hresult"] == "0x00000000" && sample[axis]["flags"] == 2,
            "canonical_native_get_failed_or_not_manual"
        );
    }
    let pan: i32 = sample["pan"]["value"]
        .as_i64()
        .context("canonical_pan_not_integer")?
        .try_into()?;
    let tilt: i32 = sample["tilt"]["value"]
        .as_i64()
        .context("canonical_tilt_not_integer")?
        .try_into()?;
    ensure!(
        (-130..=130).contains(&pan) && (-90..=90).contains(&tilt),
        "canonical_native_angle_out_of_range"
    );
    let timestamp = value["timestamp_utc"]
        .as_str()
        .filter(|value| !value.is_empty())
        .context("canonical_native_timestamp_missing")?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .context("canonical_native_timestamp_invalid")?;
    Ok(NativeCanonicalEvidence {
        source: "windows_native_confirmed_osc",
        pose: Pose {
            yaw_degrees: pan,
            pitch_degrees: -tilt,
            roll_unused_degrees: 0,
        },
        native_pan_degrees: pan,
        native_tilt_degrees: tilt,
        saved_camera_device_path: saved.into(),
        observed_native_device_path: actual.into(),
        device_binding_verified: true,
        native_timestamp_utc: timestamp.into(),
        elapsed_ms,
        limitation: "Native integer Get is a canonical readback, not a subdegree truth or physical direction proof. OSC remains an independent bounded consistency check.",
    })
}

fn owner_reader_selected(root: &Path) -> Result<bool> {
    let path = root.join("runtime/ptz-osc-config.json");
    if !path.exists() {
        return Ok(false);
    }
    let data = std::fs::read(path)?;
    ensure!(data.len() <= 64 * 1024, "canonical_config_limit");
    let config: Value = serde_json::from_slice(&data)?;
    match config["canonical_reader"]
        .as_str()
        .unwrap_or("one_shot_directshow")
    {
        "one_shot_directshow" => Ok(false),
        "capture_owner" => Ok(true),
        _ => anyhow::bail!("unknown_canonical_reader"),
    }
}

fn parse_owner(
    value: &Value,
    saved: &str,
    pid: u64,
    elapsed: u128,
) -> Result<NativeCanonicalEvidence> {
    ensure!(
        value["schema_version"] == "1.0"
            && value["source"] == "camera_capture_owner"
            && value["status"] == "ok",
        "owner_pose_protocol"
    );
    ensure!(
        value["read_only"] == true
            && value["set_calls"] == 0
            && value["additional_device_opens"] == 0,
        "owner_pose_side_effect_contract"
    );
    ensure!(
        value["server_pid"].as_u64() == Some(pid),
        "owner_pose_server_mismatch"
    );
    let epoch = value["camera_session_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .context("owner_epoch_missing")?;
    ensure!(
        value["initial_camera_session_id"] == epoch,
        "owner_epoch_changed"
    );
    let seq = value["sample_seq"]
        .as_u64()
        .context("owner_sequence_missing")?;
    let after = value["after_sample_seq"]
        .as_u64()
        .context("owner_previous_sequence_missing")?;
    ensure!(seq > after, "owner_sample_not_new");
    for key in ["sample_age_ms", "frame_age_ms"] {
        ensure!(
            value[key]
                .as_f64()
                .is_some_and(|v| v.is_finite() && (0.0..=250.0).contains(&v)),
            "owner_sample_stale"
        );
    }
    let device = value["device_id"]
        .as_str()
        .context("owner_device_missing")?;
    ensure!(
        physical_instance(device)? == physical_instance(saved)?,
        "owner_device_mismatch"
    );
    let axis = |key: &str| -> Result<i32> {
        let a = &value[key];
        ensure!(
            a["manual"] == true && a["query_success"] == true && a["step"] == 1,
            "owner_control_not_manual_integer"
        );
        let n = a["value"].as_i64().context("owner_axis_missing")?;
        let min = a["minimum"].as_i64().context("owner_range_missing")?;
        let max = a["maximum"].as_i64().context("owner_range_missing")?;
        ensure!(min <= n && n <= max, "owner_axis_out_of_range");
        Ok(i32::try_from(n)?)
    };
    let pan = axis("pan")?;
    let tilt = axis("tilt")?;
    ensure!(pan.abs() <= 180 && tilt.abs() <= 90, "owner_pose_range");
    let at = value["sampled_at_utc"]
        .as_str()
        .context("owner_timestamp_missing")?;
    chrono::DateTime::parse_from_rfc3339(at).context("owner_timestamp_invalid")?;
    Ok(NativeCanonicalEvidence {
        source: "windows_native_confirmed_osc",
        pose: Pose {
            yaw_degrees: pan,
            pitch_degrees: -tilt,
            roll_unused_degrees: 0,
        },
        native_pan_degrees: pan,
        native_tilt_degrees: tilt,
        saved_camera_device_path: saved.into(),
        observed_native_device_path: device.into(),
        device_binding_verified: true,
        native_timestamp_utc: at.into(),
        elapsed_ms: elapsed,
        limitation: "Fresh integer control sample from the existing MF capture owner; no additional camera/filter is opened. Not subdegree truth or an emergency-stop proof.",
    })
}

async fn read_owner(root: &Path, saved: &str, start: Instant) -> Result<NativeCanonicalEvidence> {
    let descriptor = std::fs::read(root.join("runtime/server.json"))?;
    ensure!(descriptor.len() <= 8192, "owner_descriptor_limit");
    let descriptor: Value = serde_json::from_slice(&descriptor)?;
    ensure!(
        descriptor["implementation"] == "Rust",
        "owner_descriptor_implementation"
    );
    let port = descriptor["port"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 65535)
        .context("owner_port_invalid")?;
    let pid = descriptor["pid"].as_u64().context("owner_pid_missing")?;
    let token = descriptor["token"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 512 && v.bytes().all(|b| b.is_ascii_alphanumeric()))
        .context("owner_cookie_invalid")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_millis(100))
        .timeout(Duration::from_millis(750))
        .build()?;
    let mut cookie = reqwest::header::HeaderValue::from_str(&format!("ai_tutor_local={token}"))?;
    cookie.set_sensitive(true);
    let mut response = client
        .get(format!("http://127.0.0.1:{port}/api/camera/native-pose"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .context("owner_pose_request_failed")?;
    ensure!(response.status().is_success(), "owner_pose_unavailable");
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            data.len() + chunk.len() <= 16 * 1024,
            "owner_pose_response_limit"
        );
        data.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&data).context("owner_pose_json_invalid")?;
    parse_owner(&value, saved, pid, start.elapsed().as_millis())
}

async fn bounded(mut stream: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Ok(result);
        }
        ensure!(result.len() + count <= cap, "canonical_helper_output_limit");
        result.extend_from_slice(&chunk[..count]);
    }
}

pub async fn read_native_canonical(root: &Path) -> Result<NativeCanonicalEvidence> {
    let start = Instant::now();
    // The timeout includes slot waiting, spawn, concurrent pipes and process wait.
    timeout(NATIVE_READ_TIMEOUT, async {
        let _permit = READ_SLOT
            .acquire()
            .await
            .context("canonical_read_slot_closed")?;
        let root = root.canonicalize().context("canonical_root_invalid")?;
        let saved = read_saved(&root)?;
        physical_instance(&saved)?;
        if owner_reader_selected(&root)? {
            let evidence = read_owner(&root, &saved, start).await?;
            ensure!(
                read_saved(&root)? == saved,
                "canonical_saved_device_changed_during_read"
            );
            return Ok(evidence);
        }
        let helper = root
            .join("bin/ptz-read-probe.exe")
            .canonicalize()
            .context("canonical_read_helper_missing")?;
        ensure!(helper.starts_with(&root), "canonical_helper_outside_root");
        let mut command = Command::new(helper);
        command
            .arg("--root")
            .arg(&root)
            .args(["--once", "--stdout-only"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn().context("canonical_helper_spawn_failed")?;
        let out = child
            .stdout
            .take()
            .context("canonical_helper_stdout_missing")?;
        let err = child
            .stderr
            .take()
            .context("canonical_helper_stderr_missing")?;
        let (stdout, _stderr, status) =
            tokio::try_join!(bounded(out, STDOUT_CAP), bounded(err, STDERR_CAP), async {
                Ok::<_, anyhow::Error>(child.wait().await?)
            })?;
        ensure!(status.success(), "canonical_helper_exit_failed");
        let value: Value =
            serde_json::from_slice(&stdout).context("canonical_helper_json_invalid")?;
        // Recheck saved device binding after the asynchronous helper completes.
        ensure!(
            read_saved(&root)? == saved,
            "canonical_saved_device_changed_during_read"
        );
        parse_report(&value, &saved, start.elapsed().as_millis())
    })
    .await
    .context("canonical_native_read_timeout")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const SAVED: &str = r"\\?\usb#vid_3564&pid_ff02&mi_00#6&736750&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
    const NATIVE: &str = r"\\?\usb#vid_3564&pid_ff02&mi_00#6&736750&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
    fn owner() -> Value {
        json!({"schema_version":"1.0","source":"camera_capture_owner","status":"ok","read_only":true,"set_calls":0,"additional_device_opens":0,
        "server_pid":42,"camera_session_id":"epoch-a","initial_camera_session_id":"epoch-a","sample_seq":9,"after_sample_seq":8,"sample_age_ms":100,"frame_age_ms":90,
        "device_id":SAVED,"pan":{"value":19,"minimum":-130,"maximum":130,"step":1,"manual":true,"query_success":true},
        "tilt":{"value":27,"minimum":-90,"maximum":90,"step":1,"manual":true,"query_success":true},"sampled_at_utc":"2026-11-01T14:00:00Z"})
    }
    #[test]
    fn capture_owner_requires_new_bound_manual_fresh_sample() {
        let good = owner();
        assert_eq!(
            parse_owner(&good, SAVED, 42, 10)
                .unwrap()
                .pose
                .pitch_degrees,
            -27
        );
        for (key, value) in [
            ("sample_seq", json!(8)),
            ("sample_age_ms", json!(251)),
            ("server_pid", json!(43)),
            ("initial_camera_session_id", json!("epoch-b")),
            ("set_calls", json!(1)),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(parse_owner(&bad, SAVED, 42, 10).is_err(), "{key}");
        }
        let mut bad = good.clone();
        bad["pan"]["manual"] = json!(false);
        assert!(parse_owner(&bad, SAVED, 42, 10).is_err());
        assert!(parse_owner(&good, &SAVED.replace("736750", "different"), 42, 10).is_err());
    }
    #[test]
    fn capture_owner_is_explicit_not_an_automatic_fallback() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!owner_reader_selected(dir.path()).unwrap());
        std::fs::create_dir(dir.path().join("runtime")).unwrap();
        let path = dir.path().join("runtime/ptz-osc-config.json");
        std::fs::write(&path, r#"{"canonical_reader":"capture_owner"}"#).unwrap();
        assert!(owner_reader_selected(dir.path()).unwrap());
        std::fs::write(&path, r#"{"canonical_reader":"unknown"}"#).unwrap();
        assert!(owner_reader_selected(dir.path()).is_err());
    }
    #[tokio::test]
    async fn capture_owner_http_is_authenticated_and_bound() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("runtime")).unwrap();
        std::fs::write(
            dir.path().join("runtime/server.json"),
            json!({"implementation":"Rust","pid":42,"port":port,"token":"SYNTHETICTOKEN"})
                .to_string(),
        )
        .unwrap();
        let body = owner().to_string();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = vec![0; 4096];
            let n = stream.read(&mut data).await.unwrap();
            let header = String::from_utf8_lossy(&data[..n]);
            assert!(header.contains("GET /api/camera/native-pose "));
            assert!(header.contains("ai_tutor_local=SYNTHETICTOKEN"));
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let pose = read_owner(dir.path(), SAVED, Instant::now()).await.unwrap();
        assert_eq!(pose.pose.yaw_degrees, 19);
        task.await.unwrap();
    }
    fn report() -> Value {
        json!({"result":"PASS_READ_ONLY_QUERY","read_only":true,"exact_device_path_match":true,"set_calls":0,"ptz_motion_commands":0,"capture_graph_created":false,"capture_stream_started":false,"sample_count":1,"sample_interval_ms":0,"expected_device_path":NATIVE,"matched_device":{"device_path":NATIVE},"samples":[{"pan":{"value":17,"get_hresult":"0x00000000","flags":2},"tilt":{"value":24,"get_hresult":"0x00000000","flags":2}}],"timestamp_utc":"2026-11-01T08:37:10+00:00"})
    }
    #[test]
    fn exact_single_read_normalizes_only_known_interface_and_tilt_sign() {
        let p = parse_report(&report(), SAVED, 40).unwrap();
        assert_eq!(p.pose.yaw_degrees, 17);
        assert_eq!(p.pose.pitch_degrees, -24);
        assert!(parse_report(&report(), &SAVED.replace("736750", "DIFFERENT"), 40).is_err());
        assert!(
            physical_instance(&SAVED.replace(
                "e5323777-f976-4f5b-9b55-b94699c46e44",
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
            ))
            .is_err()
        );
    }
    #[test]
    fn side_effects_old_samples_bad_hresult_and_nonintegers_fail_closed() {
        for (field, value) in [
            ("sample_count", json!(12)),
            ("set_calls", json!(1)),
            ("read_only", json!(false)),
            ("capture_stream_started", json!(true)),
            ("exact_device_path_match", json!(false)),
        ] {
            let mut v = report();
            v[field] = value;
            assert!(parse_report(&v, SAVED, 40).is_err());
        }
        let mut v = report();
        v["samples"][0]["pan"]["get_hresult"] = json!("0x80004005");
        assert!(parse_report(&v, SAVED, 40).is_err());
        let mut v = report();
        v["samples"][0]["pan"]["value"] = json!(17.5);
        assert!(parse_report(&v, SAVED, 40).is_err());
    }
    #[tokio::test]
    async fn missing_helper_fails_without_fallback_or_device() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("runtime")).unwrap();
        std::fs::write(dir.path().join("runtime/rust_camera_device.txt"), SAVED).unwrap();
        assert!(read_native_canonical(dir.path()).await.is_err());
    }
}

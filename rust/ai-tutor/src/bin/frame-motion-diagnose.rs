//! One frame from the existing authenticated local service; numerical output
//! only. No camera opening, OSC messages, image files or remote image transfer.
use ai_tutor::frame_motion::diagnose_frame;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().context("project root required")?).canonicalize()?;
    ensure!(args.next().is_none(), "unexpected argument");
    let descriptor = std::fs::read(root.join("runtime/server.json"))?;
    ensure!(descriptor.len() <= 64 * 1024, "descriptor too large");
    let descriptor: Value = serde_json::from_slice(&descriptor)?;
    let port = descriptor["port"]
        .as_u64()
        .filter(|p| (1..=65535).contains(p))
        .context("invalid local port")?;
    let token = descriptor["token"]
        .as_str()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .context("invalid local token")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let mut response = client
        .get(format!("http://127.0.0.1:{port}/api/video/frame"))
        .header("Cookie", format!("ai_tutor_local={token}"))
        .send()
        .await?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "frame response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let frame: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        frame["frame_age_ms"]
            .as_f64()
            .is_some_and(|age| age.is_finite() && (0.0..=250.0).contains(&age)),
        "stale live frame"
    );
    let encoded = frame["jpeg"].as_str().context("camera frame unavailable")?;
    ensure!(encoded.len() <= 7 * 1024 * 1024, "JPEG too large");
    let jpeg = STANDARD.decode(encoded)?;
    let mut boxes = Vec::new();
    for track in frame["tracks"].as_array().into_iter().flatten().take(32) {
        if let Some(rect) = track["box"].as_array().filter(|rect| rect.len() == 4) {
            if let Some(values) = rect.iter().map(Value::as_f64).collect::<Option<Vec<_>>>() {
                boxes.push([values[0], values[1], values[2], values[3]]);
            }
        }
    }
    let diagnostic = diagnose_frame(&jpeg, &boxes).map_err(anyhow::Error::msg)?;
    let report = json!({"at":chrono::Local::now().to_rfc3339(),"frame_id":frame["frame_id"],"camera_session_id":frame["camera_session_id"],"frame_age_at_fetch_ms":frame["frame_age_ms"],"source":"existing authenticated localhost camera service","diagnostic":diagnostic,"device_commands_sent":0,"raw_frames_saved":false,"image_sent_to_remote":false});
    let runtime = root.join("runtime").canonicalize()?;
    ensure!(runtime.starts_with(&root), "runtime outside project");
    let output = runtime.join("frame-motion-diagnostics.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

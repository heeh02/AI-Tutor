//! Read TWO frames from the existing local service. No OSC dependency calls,
//! capture opening, latch creation, image output or physical commands.
use ai_tutor::frame_motion::{estimate_background_motion, prepare_background};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct Frame {
    id: u64,
    epoch: String,
    jpeg: Vec<u8>,
    faces: Vec<[f64; 4]>,
}
async fn frame(client: &reqwest::Client, url: &str, cookie: &str) -> Result<Frame> {
    let mut response = client
        .get(url)
        .header("Cookie", cookie)
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
        "stale frame"
    );
    let encoded = frame["jpeg"].as_str().context("no current camera frame")?;
    ensure!(encoded.len() <= 7 * 1024 * 1024, "JPEG too large");
    let tracks = frame["tracks"]
        .as_array()
        .context("face exclusion geometry missing")?;
    ensure!(tracks.len() <= 32, "face exclusion limit");
    let mut faces = Vec::new();
    for track in tracks {
        let rect = track["box"]
            .as_array()
            .filter(|r| r.len() == 4)
            .context("invalid face exclusion geometry")?;
        let values = rect
            .iter()
            .map(Value::as_f64)
            .collect::<Option<Vec<_>>>()
            .context("non-numeric face box")?;
        faces.push([values[0], values[1], values[2], values[3]]);
    }
    Ok(Frame {
        id: frame["frame_id"].as_u64().context("frame ID missing")?,
        epoch: frame["camera_session_id"]
            .as_str()
            .context("epoch missing")?
            .to_owned(),
        jpeg: STANDARD.decode(encoded)?,
        faces,
    })
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().context("project root required")?).canonicalize()?;
    let output = args
        .next()
        .unwrap_or_else(|| "ptz-dynamic-preflight.json".into());
    ensure!(
        !output.is_empty()
            && output.ends_with(".json")
            && output
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && !output.contains(".."),
        "output must be a new JSON basename within runtime"
    );
    let observation_ms = args
        .next()
        .map(|s| s.parse::<u64>())
        .transpose()?
        .unwrap_or(0);
    ensure!(observation_ms <= 5000, "observation limit is five seconds");
    let start_unix_ms = args
        .next()
        .map(|s| s.parse::<u128>())
        .transpose()?
        .unwrap_or(0);
    ensure!(args.next().is_none(), "unexpected argument");
    if start_unix_ms != 0 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        ensure!(
            start_unix_ms.saturating_sub(now) <= 15000,
            "scheduled start too distant"
        );
        tokio::time::sleep(Duration::from_millis(
            start_unix_ms.saturating_sub(now) as u64
        ))
        .await;
    }
    let started_at = chrono::Local::now().to_rfc3339();
    let descriptor = std::fs::read(root.join("runtime/server.json"))?;
    ensure!(descriptor.len() <= 64 * 1024, "descriptor too large");
    let descriptor: Value = serde_json::from_slice(&descriptor)?;
    let port = descriptor["port"]
        .as_u64()
        .filter(|p| (1..=65535).contains(p))
        .context("local port invalid")?;
    let token = descriptor["token"]
        .as_str()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .context("local token invalid")?;
    let cookie = format!("ai_tutor_local={token}");
    let url = format!("http://127.0.0.1:{port}/api/video/frame");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let before = frame(&client, &url, &cookie).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after = frame(&client, &url, &cookie).await?;
    let anchor_observed_at = Instant::now();
    ensure!(
        before.epoch == after.epoch && after.id > before.id,
        "two new frames in same camera epoch required"
    );
    // Reserve each face's full vertical column when choosing static patches.
    // A vertical camera correction can otherwise move the face onto a patch
    // that was above it during preflight. Actual current masks are still checked
    // during observation; this never relaxes texture or agreement thresholds.
    let columns = |faces: &[[f64; 4]]| {
        faces
            .iter()
            .map(|b| [b[0], 0.0, b[2], 1.0])
            .collect::<Vec<_>>()
    };
    let prepared = prepare_background(
        &before.jpeg,
        &after.jpeg,
        &columns(&before.faces),
        &columns(&after.faces),
    )
    .map_err(anyhow::Error::msg)?;
    let drift = if observation_ms > 0 {
        tokio::time::sleep(Duration::from_millis(observation_ms)).await;
        let final_frame = frame(&client, &url, &cookie).await?;
        let observed_frame_gap_ms = anchor_observed_at.elapsed().as_millis();
        ensure!(
            final_frame.epoch == after.epoch && final_frame.id > after.id,
            "camera epoch changed during observation"
        );
        let estimate = prepared
            .plan
            .as_ref()
            .map(|plan| {
                estimate_background_motion(
                    plan,
                    &after.jpeg,
                    &final_frame.jpeg,
                    &after.faces,
                    &final_frame.faces,
                )
            })
            .transpose()
            .map_err(anyhow::Error::msg)?;
        Some(
            json!({"frame_before":after.id,"frame_after":final_frame.id,"requested_wait_ms":observation_ms,"observed_frame_gap_ms":observed_frame_gap_ms,"estimate":estimate,"note":"Read-only passive observation, not a commanded physical-motion test"}),
        )
    } else {
        None
    };
    let report = json!({"at":chrono::Local::now().to_rfc3339(),"started_at":started_at,"result":if prepared.plan.is_some(){"PASS_NO_MOTION_ONLY"}else{"BLOCKED_NO_MOTION"},"frame_before":before.id,"frame_after":after.id,"camera_session_id":after.epoch,"preparation":prepared,"passive_drift_observation":drift,"device_commands_sent":0,"latch_created":false,"raw_frames_saved":false,"image_sent_to_remote":false});
    let runtime = root.join("runtime").canonicalize()?;
    ensure!(runtime.starts_with(&root), "runtime outside project");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(runtime.join(output))?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

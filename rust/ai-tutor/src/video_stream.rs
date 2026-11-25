//! Bounded, acknowledged latest-frame preview. This transport never controls a device.
use crate::{server::Workstation, vision::CameraService};
use axum::{
    extract::{
        State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::OwnedSemaphorePermit, time::Instant as TokioInstant};

pub(crate) const MAX_CLIENTS: usize = 4;
const VERSION: u64 = 1;
const MAX_METADATA: usize = 64 * 1024;
const MAX_JPEG: usize = 5 * 1024 * 1024;
const MAX_ACK: usize = 512;
const MAX_PACKET: usize = 4 + MAX_METADATA + MAX_JPEG;
const POLL_INTERVAL: Duration = Duration::from_millis(40);
const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(33_333_334);
const SEND_TIMEOUT: Duration = Duration::from_millis(500);
const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_JS_INTEGER: u64 = (1u64 << 53) - 1;

// The route is inside the existing authenticated, same-origin router. Acquire
// before upgrading so excess clients have a bounded HTTP failure, not a task.
pub(crate) async fn upgrade(
    State(app): State<Arc<Workstation>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Ok(permit) = app.camera.preview_slot() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many preview connections",
        )
            .into_response();
    };
    upgrade
        .max_message_size(MAX_ACK)
        .max_frame_size(MAX_ACK)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_PACKET + 1024)
        .on_upgrade(move |socket| stream(socket, app.camera.clone(), permit))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    frame_id: u64,
    camera_session_id: String,
}
impl Stamp {
    fn from_meta(meta: &Value) -> Result<Self, &'static str> {
        let frame_id = meta["frame_id"]
            .as_u64()
            .filter(|id| *id <= MAX_JS_INTEGER)
            .ok_or("Invalid preview frame id")?;
        let epoch = meta["camera_session_id"]
            .as_str()
            .filter(|epoch| !epoch.is_empty() && epoch.len() <= 128)
            .ok_or("Invalid preview camera epoch")?;
        Ok(Self {
            frame_id,
            camera_session_id: epoch.to_owned(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    protocol_version: u64,
    #[serde(rename = "type")]
    kind: String,
    delivery_id: u64,
    frame_id: u64,
    camera_session_id: String,
}
struct Pending {
    stamp: Stamp,
    delivery_id: u64,
    sent_at: Instant,
}
fn acknowledge(text: &str, pending: &Option<Pending>) -> Result<(), &'static str> {
    if text.len() > MAX_ACK {
        return Err("Preview acknowledgement too large");
    }
    let ack: Ack = serde_json::from_str(text).map_err(|_| "Invalid preview acknowledgement")?;
    let pending = pending
        .as_ref()
        .ok_or("Unexpected preview acknowledgement")?;
    if ack.protocol_version != VERSION
        || ack.kind != "ack"
        || ack.delivery_id != pending.delivery_id
        || ack.frame_id != pending.stamp.frame_id
        || ack.camera_session_id != pending.stamp.camera_session_id
    {
        return Err("Preview acknowledgement does not match pending frame");
    }
    Ok(())
}

// Exclude ordinary changing frame numbers/ages: a frame notification must not
// emit redundant state packets while one JPEG is awaiting its exact ACK.
fn state_fingerprint(meta: &Value) -> Value {
    json!({
        "state":meta["state"],"error":meta["error"],
        "camera_session_id":meta["camera_session_id"],
        "target_generation":meta["target_generation"],
        "selection_generation":meta["selection_generation"],
        "selected_track_key":meta["selected_track_key"],
        "selected_status":meta["selected_status"],
        "preview_available":meta["preview_available"],
        "preview_selectable":meta["preview_selectable"]
    })
}
fn packet(mut meta: Value, jpeg: Option<&[u8]>, delivery_id: u64) -> Result<Vec<u8>, &'static str> {
    if !meta.is_object() || delivery_id == 0 || delivery_id > MAX_JS_INTEGER {
        return Err("Invalid preview metadata");
    }
    let bytes = jpeg.unwrap_or_default();
    if jpeg.is_some() && (bytes.is_empty() || bytes.len() > MAX_JPEG) {
        return Err("Preview JPEG size outside limit");
    }
    if jpeg.is_some() {
        Stamp::from_meta(&meta)?;
    }
    meta.as_object_mut().unwrap().remove("jpeg");
    meta["protocol_version"] = json!(VERSION);
    meta["message_type"] = json!(if jpeg.is_some() { "frame" } else { "state" });
    meta["delivery_id"] = json!(delivery_id);
    meta["jpeg_length"] = json!(bytes.len());
    let metadata = serde_json::to_vec(&meta).map_err(|_| "Cannot serialize preview metadata")?;
    if metadata.len() > MAX_METADATA {
        return Err("Preview metadata outside limit");
    }
    // One required assembly copy into the single WS message. Snapshots and
    // dropped intermediate frames never clone JPEG buffers or encode base64.
    let mut output = Vec::with_capacity(4 + metadata.len() + bytes.len());
    output.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
    output.extend_from_slice(&metadata);
    output.extend_from_slice(bytes);
    Ok(output)
}
async fn send_packet(socket: &mut WebSocket, bytes: Vec<u8>) -> Result<(), &'static str> {
    tokio::time::timeout(SEND_TIMEOUT, socket.send(Message::Binary(bytes.into())))
        .await
        .map_err(|_| "Preview send timed out")?
        .map_err(|_| "Preview connection closed")
}
async fn stream(mut socket: WebSocket, camera: Arc<CameraService>, _permit: OwnedSemaphorePermit) {
    let reason = run(&mut socket, &camera).await;
    // Closing itself must not prolong a stalled connection or hold its slot.
    let _ = tokio::time::timeout(
        SEND_TIMEOUT,
        socket.send(Message::Close(Some(CloseFrame {
            code: if reason.is_ok() { 1000 } else { 1008 },
            reason: reason.err().unwrap_or("Preview closed").into(),
        }))),
    )
    .await;
}
fn frame_wait_until(last_sent: Option<TokioInstant>, now: TokioInstant) -> Option<TokioInstant> {
    last_sent
        .map(|sent| sent + MIN_FRAME_INTERVAL)
        .filter(|deadline| *deadline > now)
}
async fn run(socket: &mut WebSocket, camera: &CameraService) -> Result<(), &'static str> {
    let mut updates = camera.subscribe_preview();
    let mut ticks = tokio::time::interval(POLL_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pending: Option<Pending> = None;
    let mut last_frame: Option<Stamp> = None;
    let mut last_state: Option<Value> = None;
    let mut last_sent = None;
    let mut pacing_deadline = None;
    let mut delivery_id = 0u64;
    let mut rate_window = Instant::now();
    let mut messages_in_window = 0usize;
    loop {
        tokio::select! {
            _ = ticks.tick() => {},
            changed = updates.changed() => {
                if changed.is_err() { return Ok(()); }
            },
            _ = tokio::time::sleep_until(pacing_deadline.unwrap_or_else(TokioInstant::now)),
                if pacing_deadline.is_some() => {},
            message = socket.recv() => {
                let Some(Ok(message)) = message else { return Ok(()); };
                if rate_window.elapsed() >= Duration::from_secs(1) {
                    rate_window = Instant::now(); messages_in_window = 0;
                }
                messages_in_window += 1;
                if messages_in_window > 64 { return Err("Preview client message rate exceeded"); }
                match message {
                    Message::Text(text) => { acknowledge(&text, &pending)?; pending = None; }
                    Message::Close(_) => return Ok(()),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => return Err("Preview client must send acknowledgement text"),
                }
            }
        }
        if pending
            .as_ref()
            .is_some_and(|pending| pending.sent_at.elapsed() >= ACK_TIMEOUT)
        {
            return Err("Preview acknowledgement timed out");
        }
        // Notifications coalesce in watch. ACK and pacing expiry also read the
        // latest slot now; no JPEG is retained while waiting for permission to send.
        let (meta, jpeg) = camera.preview_snapshot();
        let fingerprint = state_fingerprint(&meta);
        let changed = last_state.as_ref().is_some_and(|last| last != &fingerprint);
        if changed || (last_state.is_none() && jpeg.is_none()) {
            delivery_id += 1;
            send_packet(socket, packet(meta.clone(), None, delivery_id)?).await?;
            // State invalidates paint but NEVER retires pending. Its exact old
            // ACK is still required before another JPEG.
        }
        last_state = Some(fingerprint);
        pacing_deadline = None;
        if pending.is_none() {
            if let Some(jpeg) = jpeg {
                let stamp = Stamp::from_meta(&meta)?;
                if last_frame.as_ref() != Some(&stamp) {
                    pacing_deadline = frame_wait_until(last_sent, TokioInstant::now());
                    if pacing_deadline.is_none() {
                        delivery_id += 1;
                        let bytes = packet(meta, Some(jpeg.as_slice()), delivery_id)?;
                        let sending_at = TokioInstant::now();
                        send_packet(socket, bytes).await?;
                        last_sent = Some(sending_at);
                        last_frame = Some(stamp.clone());
                        pending = Some(Pending {
                            stamp,
                            delivery_id,
                            sent_at: Instant::now(),
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "video_stream_tests.rs"]
mod tests;

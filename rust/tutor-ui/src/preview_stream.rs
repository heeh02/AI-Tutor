//! Wire validation and frame ownership, independent of browser APIs.
use serde_json::{Value, json};

pub const MAX_META: usize = 64 * 1024;
pub const MAX_JPEG: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameKey {
    pub delivery_id: u64,
    pub frame_id: u64,
    pub camera_session_id: String,
}
impl FrameKey {
    pub fn from_metadata(meta: &Value) -> Result<Self, String> {
        let camera_session_id = meta["camera_session_id"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .ok_or("画面缺少有效相机会话")?;
        Ok(Self {
            delivery_id: meta["delivery_id"].as_u64().ok_or("画面缺少投递编号")?,
            frame_id: meta["frame_id"].as_u64().ok_or("画面缺少帧编号")?,
            camera_session_id: camera_session_id.into(),
        })
    }
    pub fn ack(&self) -> String {
        json!({"protocol_version":1,"type":"ack","delivery_id":self.delivery_id,
            "frame_id":self.frame_id,"camera_session_id":self.camera_session_id})
        .to_string()
    }
}

pub struct Packet {
    pub metadata: Value,
    pub jpeg: Vec<u8>,
    pub key: Option<FrameKey>,
    pub delivery_id: u64,
}
pub fn parse_packet(bytes: &[u8]) -> Result<Packet, String> {
    if bytes.len() < 4 || bytes.len() > 4 + MAX_META + MAX_JPEG {
        return Err("预览消息大小无效".into());
    }
    let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    if size == 0 || size > MAX_META || size > bytes.len() - 4 {
        return Err("预览元数据长度无效".into());
    }
    let metadata: Value =
        serde_json::from_slice(&bytes[4..4 + size]).map_err(|_| "预览元数据无法读取")?;
    if !metadata.is_object() || metadata["protocol_version"] != 1 {
        return Err("预览协议版本不受支持".into());
    }
    let delivery_id = metadata["delivery_id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or("预览投递编号无效")?;
    let jpeg = &bytes[4 + size..];
    if jpeg.len() > MAX_JPEG || metadata["jpeg_length"].as_u64() != Some(jpeg.len() as u64) {
        return Err("预览图像长度不匹配".into());
    }
    let key = match metadata["message_type"].as_str() {
        Some("state") if jpeg.is_empty() => None,
        Some("frame") if !jpeg.is_empty() => {
            if metadata["state"] != "running"
                || !["width", "height"].iter().all(|field| {
                    metadata[*field]
                        .as_u64()
                        .is_some_and(|v| (1..=16384).contains(&v))
                })
                || !metadata["frame_age_ms"]
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && v >= 0.0)
            {
                return Err("预览画面状态无效".into());
            }
            Some(FrameKey::from_metadata(&metadata)?)
        }
        _ => return Err("预览消息类型无效".into()),
    };
    Ok(Packet {
        metadata,
        jpeg: jpeg.to_vec(),
        key,
        delivery_id,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameTicket {
    revision: u64,
    pub key: FrameKey,
}

/// One undecoded/unacknowledged image, plus the currently displayed image.
/// State messages invalidate image work but keep their delivery ordering.
#[derive(Default)]
pub struct FrameGate {
    revision: u64,
    last_delivery: u64,
    pending: Option<FrameTicket>,
    displayed: Option<FrameTicket>,
}
impl FrameGate {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn observe_delivery(&mut self, delivery: u64) -> Result<(), String> {
        if delivery <= self.last_delivery {
            return Err("预览消息顺序无效".into());
        }
        self.last_delivery = delivery;
        Ok(())
    }
    pub fn begin(&mut self, key: FrameKey) -> Result<FrameTicket, String> {
        if self.pending.is_some() {
            return Err("预览发送超过待确认容量".into());
        }
        self.revision = self.revision.wrapping_add(1);
        let ticket = FrameTicket {
            revision: self.revision,
            key,
        };
        self.pending = Some(ticket.clone());
        self.displayed = None;
        Ok(ticket)
    }
    pub fn current(&self, ticket: &FrameTicket) -> bool {
        ticket.revision == self.revision
            && (self.pending.as_ref() == Some(ticket) || self.displayed.as_ref() == Some(ticket))
    }
    pub fn decoded(&mut self, ticket: &FrameTicket) -> bool {
        if self.pending.as_ref() != Some(ticket) || !self.current(ticket) {
            return false;
        }
        self.displayed = Some(ticket.clone());
        true
    }
    pub fn painted(&mut self, ticket: &FrameTicket) -> Option<FrameKey> {
        if self.pending.as_ref() != Some(ticket) || self.displayed.as_ref() != Some(ticket) {
            return None;
        }
        self.pending.take().map(|v| v.key)
    }
    pub fn discard(&mut self) -> Option<FrameKey> {
        self.revision = self.revision.wrapping_add(1);
        self.displayed = None;
        self.pending.take().map(|v| v.key)
    }
    pub fn reset(&mut self) {
        self.discard();
        self.last_delivery = 0;
    }
}

pub fn frame_age(base: f64, elapsed: f64) -> f64 {
    if !base.is_finite() || base < 0.0 || !elapsed.is_finite() || elapsed < 0.0 {
        f64::INFINITY
    } else {
        base + elapsed
    }
}

/// The server's age predates socket transport. Account for that transit using
/// the same-machine capture timestamp, then accumulate only monotonic time.
pub fn arrival_age(meta: &Value, wall_clock_ms: f64) -> f64 {
    let age = meta["frame_age_ms"].as_f64().unwrap_or(f64::INFINITY);
    let transit_age = meta["captured_at_ms"]
        .as_f64()
        .map(|captured| wall_clock_ms - captured)
        .filter(|age| age.is_finite() && *age >= 0.0)
        .unwrap_or(0.0);
    frame_age(age, 0.0).max(transit_age)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(delivery_id: u64) -> FrameKey {
        FrameKey {
            delivery_id,
            frame_id: 7,
            camera_session_id: "camera-A".into(),
        }
    }
    fn packet(meta: Value, jpeg: &[u8]) -> Vec<u8> {
        let json = serde_json::to_vec(&meta).unwrap();
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(json);
        bytes.extend(jpeg);
        bytes
    }
    fn metadata() -> Value {
        json!({"protocol_version":1,"message_type":"frame","delivery_id":1,
            "frame_id":7,"camera_session_id":"camera-A","jpeg_length":3,"state":"running",
            "width":640,"height":480,"frame_age_ms":10,"selected_track_key":null})
    }
    #[test]
    fn binary_frame_keeps_pixels_and_metadata_together() {
        let parsed = parse_packet(&packet(metadata(), &[1, 2, 3])).unwrap();
        assert_eq!(parsed.jpeg, [1, 2, 3]);
        assert_eq!(parsed.key, Some(key(1)));
        assert!(parsed.metadata["selected_track_key"].is_null());
        let state = parse_packet(&packet(
            json!({"protocol_version":1,"message_type":"state",
            "delivery_id":2,"jpeg_length":0,"state":"closed"}),
            &[],
        ))
        .unwrap();
        assert!(state.key.is_none());
    }
    #[test]
    fn malformed_and_oversized_packets_are_rejected_before_display() {
        for bytes in [vec![], vec![0, 0, 1, 0], vec![255, 255, 255, 255]] {
            assert!(parse_packet(&bytes).is_err());
        }
        let mut meta = metadata();
        meta["jpeg_length"] = json!(4);
        assert!(parse_packet(&packet(meta, &[1, 2, 3])).is_err());
        let mut meta = metadata();
        meta["protocol_version"] = json!(2);
        assert!(parse_packet(&packet(meta, &[1, 2, 3])).is_err());
        let mut meta = metadata();
        meta["message_type"] = json!("state");
        assert!(parse_packet(&packet(meta, &[1, 2, 3])).is_err());
    }
    #[test]
    fn state_cancels_decode_and_old_ack_cannot_release_the_next_frame() {
        let mut gate = FrameGate::default();
        gate.observe_delivery(1).unwrap();
        let old = gate.begin(key(1)).unwrap();
        assert!(gate.begin(key(2)).is_err());
        gate.observe_delivery(2).unwrap();
        assert_eq!(gate.discard(), Some(key(1)));
        assert!(!gate.decoded(&old));
        gate.observe_delivery(3).unwrap();
        let fresh = gate.begin(key(3)).unwrap();
        assert!(gate.painted(&old).is_none());
        assert!(gate.decoded(&fresh));
        assert_eq!(gate.painted(&fresh), Some(key(3)));
        assert!(gate.painted(&fresh).is_none());
        assert!(!gate.current(&old));
        assert!(gate.current(&fresh));
        assert!(gate.observe_delivery(3).is_err());
    }
    #[test]
    fn reconnect_and_local_age_reject_old_clicks_even_without_new_network_data() {
        let mut gate = FrameGate::default();
        let old = gate.begin(key(1)).unwrap();
        gate.decoded(&old);
        gate.reset();
        gate.observe_delivery(1).unwrap();
        let fresh = gate.begin(key(1)).unwrap();
        assert!(!gate.current(&old));
        assert!(gate.current(&fresh));
        assert_eq!(frame_age(100.0, 300.0), 400.0);
        assert_eq!(frame_age(100.0, 900.0), 1000.0);
        assert!(frame_age(0.0, -1.0).is_infinite());
        assert_eq!(
            arrival_age(&json!({"frame_age_ms":20,"captured_at_ms":1000}), 1450.0),
            450.0
        );
        assert_eq!(
            arrival_age(&json!({"frame_age_ms":20,"captured_at_ms":1500}), 1450.0),
            20.0
        );
        let ack: Value = serde_json::from_str(&key(3).ack()).unwrap();
        assert_eq!(ack["delivery_id"], 3);
        assert_eq!(ack["frame_id"], 7);
    }
}

//! Real loopback HTTP/WS tests with synthetic JPEGs. No camera, model or PTZ is opened.
use super::*;
use crate::server::router;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Fixture {
    _dir: tempfile::TempDir,
    app: Arc<Workstation>,
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app = Workstation::new(dir.path().into(), "fixture-video-token".into()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routes = router(app.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        Self {
            _dir: dir,
            app,
            address,
            task,
        }
    }
    async fn connect(&self, cookie: bool, origin: &str) -> (u16, Client) {
        let mut stream = TcpStream::connect(self.address).await.unwrap();
        let request = format!(
            "GET /api/video/stream HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{}\r\n",
            self.address,
            origin,
            if cookie {
                "Cookie: ai_tutor_local=fixture-video-token\r\n"
            } else {
                ""
            }
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let head = tokio::time::timeout(Duration::from_secs(3), async {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                assert!(head.len() < 8192);
                head.push(stream.read_u8().await.unwrap());
            }
            String::from_utf8(head).unwrap()
        })
        .await
        .unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, Client(stream))
    }
    async fn client(&self) -> Client {
        let (status, client) = self
            .connect(true, &format!("http://{}", self.address))
            .await;
        assert_eq!(status, 101);
        client
    }
    fn inject(&self, epoch: &str, id: u64, age: f64, jpeg: Vec<u8>) {
        assert!(
            self.app
                .camera
                .inject_preview_fixture(meta(epoch, id, age), jpeg)
        );
    }
    async fn finish(self) {
        self.task.abort();
        self.app.close().await;
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Client(TcpStream);
impl Client {
    async fn next(&mut self) -> Option<(u8, Vec<u8>)> {
        let first = self.0.read_u8().await.ok()?;
        let length = self.0.read_u8().await.ok()?;
        assert_eq!(first & 0x80, 0x80, "server sends complete messages");
        assert_eq!(length & 0x80, 0, "server messages are unmasked");
        let length = match length & 0x7f {
            126 => self.0.read_u16().await.ok()? as usize,
            127 => usize::try_from(self.0.read_u64().await.ok()?).unwrap(),
            n => n as usize,
        };
        assert!(length <= MAX_PACKET);
        let mut payload = vec![0; length];
        self.0.read_exact(&mut payload).await.ok()?;
        Some((first & 0x0f, payload))
    }
    async fn packet(&mut self) -> (Value, Vec<u8>) {
        let (opcode, payload) = tokio::time::timeout(Duration::from_secs(3), self.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(opcode, 2, "expected binary preview packet");
        let count = u32::from_be_bytes(payload[..4].try_into().unwrap()) as usize;
        assert!(count <= MAX_METADATA && 4 + count <= payload.len());
        let metadata: Value = serde_json::from_slice(&payload[4..4 + count]).unwrap();
        let jpeg = payload[4 + count..].to_vec();
        assert_eq!(metadata["protocol_version"], VERSION);
        assert_eq!(metadata["jpeg_length"], jpeg.len());
        (metadata, jpeg)
    }
    async fn send_text(&mut self, text: &str) {
        let mut frame = vec![0x81];
        if text.len() <= 125 {
            frame.push(0x80 | text.len() as u8);
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(text.len() as u16).to_be_bytes());
        }
        let mask = [17, 29, 41, 53];
        frame.extend_from_slice(&mask);
        frame.extend(text.bytes().enumerate().map(|(i, byte)| byte ^ mask[i % 4]));
        self.0.write_all(&frame).await.unwrap();
    }
    async fn ack(&mut self, frame: &Value) {
        self.send_text(&json!({"protocol_version":1,"type":"ack","delivery_id":frame["delivery_id"],"frame_id":frame["frame_id"],"camera_session_id":frame["camera_session_id"]}).to_string()).await;
    }
    async fn assert_closed(&mut self) {
        tokio::time::timeout(Duration::from_secs(4), async {
            while let Some((opcode, _)) = self.next().await {
                if opcode == 8 {
                    return;
                }
                assert_eq!(opcode, 2);
            }
        })
        .await
        .expect("connection must close within bound");
    }
}
fn meta(epoch: &str, id: u64, age: f64) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    json!({"camera_session_id":epoch,"frame_id":id,"frame_age_ms":age,"width":1,"height":1,"captured_at_ms":now.saturating_sub(age as u64),"tracks":[{"track_key":format!("{epoch}:A"),"box":[0.1,0.1,0.3,0.3]}],"association_ambiguous":false})
}
fn jpeg() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode(&[255, 20, 30], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    bytes
}

#[tokio::test]
async fn loopback_auth_origin_and_connection_limit_share_existing_middleware() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture
            .connect(false, &format!("http://{}", fixture.address))
            .await
            .0,
        401
    );
    assert_eq!(
        fixture.connect(true, "https://untrusted.example").await.0,
        403
    );
    let mut clients = Vec::new();
    for _ in 0..MAX_CLIENTS {
        clients.push(fixture.client().await);
    }
    assert_eq!(
        fixture
            .connect(true, &format!("http://{}", fixture.address))
            .await
            .0,
        429
    );
    clients.pop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    clients.push(fixture.client().await);
    drop(clients);
    fixture.finish().await;
}

#[tokio::test]
async fn loopback_ack_skips_intermediate_frames_and_binds_epoch_and_delivery() {
    let fixture = Fixture::new().await;
    let pixels = jpeg();
    fixture.inject("A", 1, 0.0, pixels.clone());
    let mut client = fixture.client().await;
    let (first, first_jpeg) = client.packet().await;
    assert_eq!(first["message_type"], "frame");
    assert_eq!(first_jpeg, pixels);
    fixture.inject("A", 2, 0.0, pixels.clone());
    fixture.inject("A", 3, 0.0, pixels.clone());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), client.next())
            .await
            .is_err()
    );
    client.ack(&first).await;
    let (latest, _) = client.packet().await;
    assert_eq!(latest["frame_id"], 3);
    assert!(latest["delivery_id"].as_u64() > first["delivery_id"].as_u64());
    fixture.inject("B", 1, 0.0, pixels.clone());
    let (state, no_pixels) = client.packet().await;
    assert_eq!(state["message_type"], "state");
    assert_eq!(state["camera_session_id"], "B");
    assert!(no_pixels.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(80), client.next())
            .await
            .is_err()
    );
    // A state did not free the previous epoch's pending JPEG slot.
    client.ack(&latest).await;
    let (reopened, _) = client.packet().await;
    assert_eq!(reopened["message_type"], "frame");
    assert_eq!(reopened["camera_session_id"], "B");
    assert_eq!(reopened["frame_id"], 1);
    // Duplicate retired ACK must not acknowledge the new epoch's frame 1.
    client.ack(&latest).await;
    client.assert_closed().await;
    fixture.finish().await;
}

#[tokio::test]
async fn loopback_selection_close_reopen_and_age_send_pixel_free_invalidation() {
    let fixture = Fixture::new().await;
    fixture.inject("A", 1, 0.0, jpeg());
    let mut client = fixture.client().await;
    let (first, _) = client.packet().await;
    fixture.app.camera.select("A:A").unwrap();
    let (selected, bytes) = client.packet().await;
    assert!(bytes.is_empty());
    assert_eq!(selected["message_type"], "state");
    assert_eq!(selected["selected_track_key"], "A:A");
    client.ack(&first).await;
    // Selection changes cannot resend the same frame under a new delivery.
    assert!(
        tokio::time::timeout(Duration::from_millis(80), client.next())
            .await
            .is_err()
    );
    fixture.app.camera.close();
    let (closed, bytes) = client.packet().await;
    assert!(bytes.is_empty());
    assert_eq!(closed["state"], "closed");
    assert_eq!(closed["tracks"], json!([]));
    fixture.inject("B", 1, 0.0, jpeg());
    let (state, _) = client.packet().await;
    assert_eq!(state["message_type"], "state");
    let (fresh, _) = client.packet().await;
    assert_eq!(fresh["camera_session_id"], "B");
    client.ack(&fresh).await;
    fixture.inject("B", 2, 1001.0, jpeg());
    let (stale, bytes) = client.packet().await;
    assert!(bytes.is_empty());
    assert_eq!(stale["preview_available"], false);
    assert_eq!(stale["preview_selectable"], false);
    assert_eq!(stale["tracks"], json!([]));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), client.next())
            .await
            .is_err()
    );
    fixture.finish().await;
}

#[tokio::test]
async fn loopback_idle_ack_then_frame_burst_coalesces_without_a_queue() {
    let fixture = Fixture::new().await;
    fixture.inject("A", 1, 0.0, jpeg());
    let mut client = fixture.client().await;
    let (first, _) = client.packet().await;
    client.ack(&first).await;
    // There is no await in this burst: a watch notification carries no queued
    // frame. The first subsequent snapshot must be the final injected frame.
    let pixels = jpeg();
    for id in 2..=64 {
        fixture.inject("A", id, 0.0, pixels.clone());
    }
    let (latest, bytes) = client.packet().await;
    assert_eq!(latest["message_type"], "frame");
    assert_eq!(latest["frame_id"], 64);
    assert_eq!(bytes, pixels);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), client.next())
            .await
            .is_err()
    );
    client.ack(&latest).await;
    fixture.finish().await;
}

#[test]
fn frame_pacing_has_no_catch_up_credit_and_waits_only_until_next_frame() {
    let start = TokioInstant::now();
    assert_eq!(frame_wait_until(None, start), None);
    assert_eq!(
        frame_wait_until(Some(start), start),
        Some(start + MIN_FRAME_INTERVAL)
    );
    assert_eq!(
        frame_wait_until(
            Some(start),
            start + MIN_FRAME_INTERVAL - Duration::from_nanos(1)
        ),
        Some(start + MIN_FRAME_INTERVAL)
    );
    assert_eq!(
        frame_wait_until(Some(start), start + MIN_FRAME_INTERVAL),
        None
    );
    let after_idle = start + Duration::from_secs(3);
    assert_eq!(frame_wait_until(Some(start), after_idle), None);
    // Idle time never accumulates credits to emit several new JPEGs at once.
    assert_eq!(
        frame_wait_until(Some(after_idle), after_idle + Duration::from_nanos(1)),
        Some(after_idle + MIN_FRAME_INTERVAL)
    );
}

#[tokio::test]
async fn loopback_missing_ack_and_oversized_or_malicious_ack_close_within_bound() {
    let fixture = Fixture::new().await;
    fixture.inject("A", 1, 0.0, jpeg());
    let mut timeout_client = fixture.client().await;
    let _ = timeout_client.packet().await;
    timeout_client.assert_closed().await;
    fixture.inject("A", 2, 0.0, jpeg());
    let mut oversize = fixture.client().await;
    let _ = oversize.packet().await;
    oversize.send_text(&"x".repeat(MAX_ACK + 1)).await;
    oversize.assert_closed().await;
    let mut malicious = fixture.client().await;
    let (mut frame, _) = malicious.packet().await;
    frame["delivery_id"] = json!(99);
    malicious.ack(&frame).await;
    malicious.assert_closed().await;
    fixture.finish().await;
}

#[tokio::test]
async fn loopback_oversized_synthetic_jpeg_closes_and_http_fallback_still_works() {
    let fixture = Fixture::new().await;
    let pixels = jpeg();
    fixture.inject("A", 1, 0.0, pixels.clone());
    let response: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{}/api/video/frame", fixture.address))
        .header("cookie", "ai_tutor_local=fixture-video-token")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["frame_id"], 1);
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(response["jpeg"].as_str().unwrap())
            .unwrap(),
        pixels
    );
    fixture.inject("A", 2, 0.0, vec![0; MAX_JPEG + 1]);
    let mut client = fixture.client().await;
    client.assert_closed().await;
    fixture.finish().await;
}

#[test]
fn packet_and_ack_reject_wrong_version_unknown_fields_bounds_and_wrong_generation() {
    let metadata = meta("A", 1, 0.0);
    assert!(packet(metadata.clone(), Some(&[]), 1).is_err());
    assert!(packet(metadata.clone(), Some(&[1]), 0).is_err());
    let mut too_large = metadata.clone();
    too_large["unexpected"] = json!("x".repeat(MAX_METADATA));
    assert!(packet(too_large, Some(&[1]), 1).is_err());
    let mut unsafe_integer = metadata.clone();
    unsafe_integer["frame_id"] = json!(MAX_JS_INTEGER + 1);
    assert!(packet(unsafe_integer, Some(&[1]), 1).is_err());
    let pending = Some(Pending {
        stamp: Stamp::from_meta(&metadata).unwrap(),
        delivery_id: 3,
        sent_at: Instant::now(),
    });
    let ack = json!({"protocol_version":1,"type":"ack","delivery_id":3,"frame_id":1,"camera_session_id":"A"});
    assert!(acknowledge(&ack.to_string(), &pending).is_ok());
    assert!(acknowledge(&ack.to_string(), &None).is_err());
    for (key, value) in [
        ("protocol_version", json!(2)),
        ("delivery_id", json!(2)),
        ("camera_session_id", json!("B")),
        ("unexpected", json!(true)),
    ] {
        let mut invalid = ack.clone();
        invalid[key] = value;
        assert!(acknowledge(&invalid.to_string(), &pending).is_err());
    }
}

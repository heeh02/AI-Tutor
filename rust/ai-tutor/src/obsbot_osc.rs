//! Read-only OBSBOT Center OSC over explicitly configured localhost UDP ports.
//!
//! Reference: OBSBOT Center OSC command table dated 2026-05-08, rows 70-78,
//! https://www.obsbot.com/explore/obsbot-center/osc . An explicit handshake
//! variant follows the official UDP sample's Connected(1) registration; it is
//! never hidden as GetDeviceInfo. No SelectDevice, tracking or motor writes. UDP has no
//! request nonce/authentication; an unindexed reply is not verified device
//! binding and none of these observations authorizes physical movement.
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    time::{Instant as TokioInstant, timeout_at},
};

pub const MAX_DATAGRAM_BYTES: usize = 4096;
pub const MAX_ADDRESS_BYTES: usize = 128;
pub const MAX_STRING_BYTES: usize = 1024;
pub const MAX_ARGUMENTS: usize = 32;
pub const MAX_IGNORED_PACKETS: usize = 16;
pub const MAX_DIAGNOSTIC_MESSAGES: usize = 4;
/// An application resource bound, not a claim about the manufacturer's limit.
pub const MAX_DEVICE_INDEX: i32 = 255;
pub const MAX_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum OscArg {
    Int(i32),
    Float(f32),
    String(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OscMessage {
    pub address: String,
    pub arguments: Vec<OscArg>,
}

fn valid_address(address: &str) -> bool {
    address.starts_with('/')
        && address.len() > 1
        && address.len() <= MAX_ADDRESS_BYTES
        && !address.ends_with('/')
        && !address.contains("//")
        && address
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'_' | b'-'))
}

fn push_string(out: &mut Vec<u8>, value: &str, limit: usize) -> Result<()> {
    ensure!(
        value.len() <= limit && !value.contains('\0'),
        "osc_string_limit"
    );
    out.extend_from_slice(value.as_bytes());
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    ensure!(out.len() <= MAX_DATAGRAM_BYTES, "osc_packet_size");
    Ok(())
}

/// Encodes the small i/f/s subset required by this adapter, without OSC bundles
/// or address patterns. The transport only sends the three ReadQuery variants.
pub fn encode_message(message: &OscMessage) -> Result<Vec<u8>> {
    ensure!(valid_address(&message.address), "osc_address_invalid");
    ensure!(
        message.arguments.len() <= MAX_ARGUMENTS,
        "osc_argument_limit"
    );
    let mut out = Vec::with_capacity(256);
    push_string(&mut out, &message.address, MAX_ADDRESS_BYTES)?;
    let mut tags = String::from(",");
    for arg in &message.arguments {
        tags.push(match arg {
            OscArg::Int(_) => 'i',
            OscArg::Float(_) => 'f',
            OscArg::String(_) => 's',
        });
    }
    push_string(&mut out, &tags, MAX_ARGUMENTS + 1)?;
    for arg in &message.arguments {
        match arg {
            OscArg::Int(value) => out.extend_from_slice(&value.to_be_bytes()),
            OscArg::Float(value) => {
                ensure!(value.is_finite(), "osc_float_nonfinite");
                out.extend_from_slice(&value.to_bits().to_be_bytes());
            }
            OscArg::String(value) => push_string(&mut out, value, MAX_STRING_BYTES)?,
        }
        ensure!(out.len() <= MAX_DATAGRAM_BYTES, "osc_packet_size");
    }
    Ok(out)
}

fn read_string<'a>(packet: &'a [u8], cursor: &mut usize, limit: usize) -> Result<&'a str> {
    let remaining = packet.get(*cursor..).context("osc_truncated_string")?;
    let nul = remaining
        .iter()
        .take(limit + 1)
        .position(|b| *b == 0)
        .context("osc_string_limit")?;
    let padded = (nul + 1).next_multiple_of(4);
    let bytes = remaining.get(..padded).context("osc_truncated_string")?;
    ensure!(bytes[nul..].iter().all(|b| *b == 0), "osc_string_padding");
    let value = std::str::from_utf8(&bytes[..nul]).context("osc_string_utf8")?;
    *cursor += padded;
    Ok(value)
}

fn read_word(packet: &[u8], cursor: &mut usize) -> Result<[u8; 4]> {
    let word = packet
        .get(*cursor..*cursor + 4)
        .context("osc_truncated_argument")?;
    *cursor += 4;
    Ok(word.try_into().expect("checked four-byte OSC word"))
}

/// Strict size, alignment, padding, UTF-8, tag and trailing-byte validation.
/// UTF-8 strings are accepted as a bounded compatibility extension for device
/// names; addresses and tags remain ASCII. Bundles, blobs and nested arrays are
/// deliberately unsupported instead of being recursively or partially parsed.
pub fn decode_message(packet: &[u8]) -> Result<OscMessage> {
    ensure!(
        !packet.is_empty() && packet.len() <= MAX_DATAGRAM_BYTES && packet.len().is_multiple_of(4),
        "osc_packet_size"
    );
    let mut cursor = 0;
    let address = read_string(packet, &mut cursor, MAX_ADDRESS_BYTES)?;
    ensure!(valid_address(address), "osc_address_invalid");
    let tags = read_string(packet, &mut cursor, MAX_ARGUMENTS + 1)?;
    ensure!(
        tags.starts_with(',') && tags.len() <= MAX_ARGUMENTS + 1,
        "osc_type_tags_invalid"
    );
    let mut arguments = Vec::with_capacity(tags.len() - 1);
    for tag in tags.bytes().skip(1) {
        arguments.push(match tag {
            b'i' => OscArg::Int(i32::from_be_bytes(read_word(packet, &mut cursor)?)),
            b'f' => {
                let value = f32::from_bits(u32::from_be_bytes(read_word(packet, &mut cursor)?));
                ensure!(value.is_finite(), "osc_float_nonfinite");
                OscArg::Float(value)
            }
            b's' => OscArg::String(read_string(packet, &mut cursor, MAX_STRING_BYTES)?.to_owned()),
            _ => bail!("osc_type_tag_unsupported"),
        });
    }
    ensure!(cursor == packet.len(), "osc_trailing_bytes");
    Ok(OscMessage {
        address: address.to_owned(),
        arguments,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadQuery {
    DeviceInfo,
    DeviceInfoViaHandshake,
    AiTrackingInfo,
    GimbalPosition,
}

impl ReadQuery {
    /// Default Get-only queries; no implicit registration is added.
    pub const ALL: [Self; 3] = [Self::DeviceInfo, Self::AiTrackingInfo, Self::GimbalPosition];

    pub fn address(self) -> &'static str {
        match self {
            Self::DeviceInfo => "/OBSBOT/WebCam/General/GetDeviceInfo",
            Self::DeviceInfoViaHandshake => "/OBSBOT/WebCam/General/Connected",
            Self::AiTrackingInfo => "/OBSBOT/WebCam/Tiny/GetAiTrackingInfo",
            Self::GimbalPosition => "/OBSBOT/WebCam/General/GetGimbalPosInfo",
        }
    }

    pub fn response_address(self) -> &'static str {
        match self {
            Self::DeviceInfo | Self::DeviceInfoViaHandshake => "/OBSBOT/WebCam/General/DeviceInfo",
            Self::AiTrackingInfo => "/OBSBOT/WebCam/Tiny/AiTrackingInfo",
            Self::GimbalPosition => "/OBSBOT/WebCam/General/GetGimbalPosInfoResp",
        }
    }

    pub fn request(self, device_index: i32) -> Result<OscMessage> {
        check_device_index(device_index)?;
        Ok(OscMessage {
            address: self.address().to_owned(),
            arguments: if self == Self::DeviceInfoViaHandshake {
                vec![OscArg::Int(1)]
            } else {
                vec![OscArg::Int(device_index), OscArg::Int(0)]
            },
        })
    }
}

fn check_device_index(index: i32) -> Result<()> {
    ensure!(
        (0..=MAX_DEVICE_INDEX).contains(&index),
        "osc_device_index_limit"
    );
    Ok(())
}

fn int(arg: &OscArg) -> Result<i32> {
    match arg {
        OscArg::Int(value) => Ok(*value),
        _ => bail!("osc_reply_argument_type"),
    }
}
fn boolean(arg: &OscArg) -> Result<bool> {
    match int(arg)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => bail!("osc_reply_boolean_range"),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceSlot {
    pub connected: bool,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceInfo {
    /// The official 2026-05-08 reply describes four slots. Extra slot layouts
    /// are rejected until an official versioned shape is reviewed.
    pub slots: Vec<DeviceSlot>,
    pub selected_device_index: i32,
    pub selected_device_running: bool,
    /// Kept numeric: the table's old four-value type list does not identify
    /// every newer Tiny 3 model. No unsupported model-name inference is made.
    pub selected_device_type_code: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ReadState {
    DeviceInfo(DeviceInfo),
    AiTrackingInfo {
        locked: bool,
    },
    GimbalPosition {
        roll_unused_degrees: i32,
        pitch_degrees: i32,
        yaw_degrees: i32,
    },
}

/// The two-int interpretation is explicitly opt-in and comes from read-only
/// cross-checks of this workstation's Center 2.0.15/Tiny 3, not from the older
/// official three-int table. This does not validate Set command direction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GimbalProfile {
    #[default]
    DocumentedRollPitchYaw,
    Tiny3Center2015PanPitch,
}

impl GimbalProfile {
    fn argument_count(self) -> usize {
        match self {
            Self::DocumentedRollPitchYaw => 3,
            Self::Tiny3Center2015PanPitch => 2,
        }
    }

    pub fn source_note(self) -> &'static str {
        match self {
            Self::DocumentedRollPitchYaw => {
                "official 2026-05-08 OSC command table: roll,pitch,yaw; Set direction not verified"
            }
            Self::Tiny3Center2015PanPitch => {
                "explicit local read-only cross-check: runtime/osc-read-rust-correlate.json and runtime/ptz-read-rust-correlate.json; [pan,pitch], roll unused=0; native Tilt sign differs; Set direction not verified"
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParsedReply {
    pub state: ReadState,
    pub echoed_device_index: Option<i32>,
    pub device_binding_verified: bool,
    pub binding_note: &'static str,
    pub gimbal_profile: Option<GimbalProfile>,
    pub mapping_source: Option<&'static str>,
}

pub fn parse_reply(
    query: ReadQuery,
    requested_device_index: i32,
    message: &OscMessage,
) -> Result<ParsedReply> {
    parse_reply_with_profile(
        query,
        requested_device_index,
        message,
        GimbalProfile::default(),
    )
}

pub fn parse_reply_with_profile(
    query: ReadQuery,
    requested_device_index: i32,
    message: &OscMessage,
    profile: GimbalProfile,
) -> Result<ParsedReply> {
    check_device_index(requested_device_index)?;
    ensure!(
        message.address == query.response_address(),
        "osc_reply_address_mismatch"
    );
    let args = &message.arguments;
    let (args, echoed) = match query {
        ReadQuery::DeviceInfo | ReadQuery::DeviceInfoViaHandshake if args.len() == 12 => {
            (&args[1..], Some(int(&args[0])?))
        }
        ReadQuery::DeviceInfo | ReadQuery::DeviceInfoViaHandshake if args.len() == 11 => {
            (args.as_slice(), None)
        }
        ReadQuery::AiTrackingInfo if args.len() == 2 => (&args[1..], Some(int(&args[0])?)),
        ReadQuery::AiTrackingInfo if args.len() == 1 => (args.as_slice(), None),
        ReadQuery::GimbalPosition if args.len() == profile.argument_count() => {
            (args.as_slice(), None)
        }
        _ => bail!("osc_reply_argument_count"),
    };
    if let Some(index) = echoed {
        ensure!(index == requested_device_index, "osc_reply_device_mismatch");
    }
    let state = match query {
        ReadQuery::DeviceInfo | ReadQuery::DeviceInfoViaHandshake => {
            let mut slots = Vec::with_capacity(4);
            for pair in args[..8].chunks_exact(2) {
                let connected = boolean(&pair[0])?;
                let OscArg::String(name) = &pair[1] else {
                    bail!("osc_reply_argument_type")
                };
                ensure!(name.len() <= 256, "osc_device_name_limit");
                slots.push(DeviceSlot {
                    connected,
                    name: name.clone(),
                });
            }
            let selected_device_index = int(&args[8])?;
            check_device_index(selected_device_index)?;
            let selected_device_running = boolean(&args[9])?;
            let selected_device_type_code = int(&args[10])?;
            ensure!(
                (0..=1024).contains(&selected_device_type_code),
                "osc_device_type_limit"
            );
            ReadState::DeviceInfo(DeviceInfo {
                slots,
                selected_device_index,
                selected_device_running,
                selected_device_type_code,
            })
        }
        ReadQuery::AiTrackingInfo => ReadState::AiTrackingInfo {
            locked: boolean(&args[0])?,
        },
        ReadQuery::GimbalPosition => {
            let (roll_unused_degrees, pitch_degrees, yaw_degrees) = match profile {
                GimbalProfile::DocumentedRollPitchYaw => {
                    (int(&args[0])?, int(&args[1])?, int(&args[2])?)
                }
                GimbalProfile::Tiny3Center2015PanPitch => (0, int(&args[1])?, int(&args[0])?),
            };
            ensure!(
                (-90..=90).contains(&pitch_degrees) && (-140..=140).contains(&yaw_degrees),
                "osc_gimbal_range"
            );
            ReadState::GimbalPosition {
                roll_unused_degrees,
                pitch_degrees,
                yaw_degrees,
            }
        }
    };
    Ok(ParsedReply {
        state,
        echoed_device_index: echoed,
        device_binding_verified: echoed.is_some(),
        binding_note: if echoed.is_some() {
            "response echoed requested device index; UDP still has no authentication or request nonce"
        } else {
            "response omits device index; requested device binding is not verified"
        },
        gimbal_profile: (query == ReadQuery::GimbalPosition).then_some(profile),
        mapping_source: (query == ReadQuery::GimbalPosition).then_some(profile.source_note()),
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct ReadReceipt {
    pub query: ReadQuery,
    pub requested_device_index: i32,
    pub source: SocketAddr,
    pub local_reply_address: SocketAddr,
    pub received_at: String,
    pub elapsed_ms: u128,
    pub ignored_packets: usize,
    pub message: OscMessage,
    pub reply: ParsedReply,
}

/// Bounded transport diagnostics contain only endpoints and OS error metadata,
/// never rejected packet bodies. A rejected source does not become trusted.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ReadDiagnostics {
    pub rejected_sources: Vec<SocketAddr>,
    pub last_os_error: Option<i32>,
    pub last_io_error_kind: Option<String>,
    /// Only populated after exact configured localhost source verification.
    pub unexpected_messages: Vec<OscMessage>,
    pub last_rejected_reply: Option<OscMessage>,
    pub last_protocol_error: Option<String>,
}

/// The public transport exposes no arbitrary send or writable command method.
/// `&mut self` serializes requests because this OSC protocol has no request ID.
pub struct OscReadClient {
    socket: UdpSocket,
    server: SocketAddr,
    timeout: Duration,
    diagnostics: ReadDiagnostics,
    gimbal_profile: GimbalProfile,
}

impl OscReadClient {
    /// Both ports must be explicitly supplied. `reply_port=0` explicitly asks
    /// the OS for an ephemeral reply port, printed by local_addr()/the probe.
    pub async fn bind(server_port: u16, reply_port: u16, timeout: Duration) -> Result<Self> {
        Self::bind_with_profile(server_port, reply_port, timeout, GimbalProfile::default()).await
    }

    pub async fn bind_with_profile(
        server_port: u16,
        reply_port: u16,
        timeout: Duration,
        gimbal_profile: GimbalProfile,
    ) -> Result<Self> {
        ensure!(server_port != 0, "osc_server_port_required");
        ensure!(
            reply_port == 0 || reply_port != server_port,
            "osc_ports_must_differ"
        );
        ensure!(
            !timeout.is_zero() && timeout <= MAX_TIMEOUT,
            "osc_timeout_limit"
        );
        let local = SocketAddrV4::new(Ipv4Addr::LOCALHOST, reply_port);
        let socket = UdpSocket::bind(local)
            .await
            .context("osc_local_bind_failed")?;
        Ok(Self {
            socket,
            server: SocketAddrV4::new(Ipv4Addr::LOCALHOST, server_port).into(),
            timeout,
            diagnostics: ReadDiagnostics::default(),
            gimbal_profile,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    pub fn diagnostics(&self) -> &ReadDiagnostics {
        &self.diagnostics
    }

    fn remember_io_error(&mut self, error: &std::io::Error) {
        self.diagnostics.last_os_error = error.raw_os_error();
        self.diagnostics.last_io_error_kind = Some(format!("{:?}", error.kind()));
    }

    /// Explicit software client registration, single int 1 as in the official
    /// TouchOSC UDP sample. Awaits DeviceInfo; ConnectedResp remains diagnostic.
    pub async fn handshake_device_info(&mut self, device_index: i32) -> Result<ReadReceipt> {
        self.query(ReadQuery::DeviceInfoViaHandshake, device_index)
            .await
    }

    pub async fn query(&mut self, query: ReadQuery, device_index: i32) -> Result<ReadReceipt> {
        self.diagnostics = ReadDiagnostics::default();
        let packet = encode_message(&query.request(device_index)?)?;
        let mut buffer = [0u8; MAX_DATAGRAM_BYTES + 1];
        let mut ignored = 0;
        // Discard queued packets before issuing a new request, bounded against
        // local floods. Late datagrams still cannot be cryptographically tied
        // to this request; receipts explicitly retain that protocol limitation.
        loop {
            match self.socket.try_recv_from(&mut buffer) {
                Ok(_) => {
                    ignored += 1;
                    ensure!(ignored <= MAX_IGNORED_PACKETS, "osc_ignored_packet_limit");
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.remember_io_error(&error);
                    return Err(error).context("osc_receive_failed");
                }
            }
        }
        let start = Instant::now();
        let deadline = TokioInstant::now() + self.timeout;
        let sent_result = timeout_at(deadline, self.socket.send_to(&packet, self.server))
            .await
            .context("osc_timeout")?;
        let sent = match sent_result {
            Ok(size) => size,
            Err(error) => {
                self.remember_io_error(&error);
                return Err(error).context("osc_send_failed");
            }
        };
        ensure!(sent == packet.len(), "osc_incomplete_send");
        loop {
            let receive_result = timeout_at(deadline, self.socket.recv_from(&mut buffer))
                .await
                .context("osc_timeout")?;
            let (size, source) = match receive_result {
                Ok(received) => received,
                Err(error) => {
                    self.remember_io_error(&error);
                    return Err(error).context("osc_receive_failed");
                }
            };
            if source != self.server {
                if self.diagnostics.rejected_sources.len() <= MAX_IGNORED_PACKETS {
                    self.diagnostics.rejected_sources.push(source);
                }
                ignored += 1;
                ensure!(ignored <= MAX_IGNORED_PACKETS, "osc_ignored_packet_limit");
                continue;
            }
            ensure!(size <= MAX_DATAGRAM_BYTES, "osc_packet_size");
            let message = match decode_message(&buffer[..size]) {
                Ok(message) => message,
                Err(error) => {
                    self.diagnostics.last_protocol_error = Some(error.to_string());
                    return Err(error);
                }
            };
            if message.address != query.response_address() {
                if self.diagnostics.unexpected_messages.len() < MAX_DIAGNOSTIC_MESSAGES {
                    self.diagnostics.unexpected_messages.push(message);
                }
                ignored += 1;
                ensure!(ignored <= MAX_IGNORED_PACKETS, "osc_ignored_packet_limit");
                continue;
            }
            let reply = match parse_reply_with_profile(
                query,
                device_index,
                &message,
                self.gimbal_profile,
            ) {
                Ok(reply) => reply,
                Err(error) => {
                    self.diagnostics.last_protocol_error = Some(error.to_string());
                    self.diagnostics.last_rejected_reply = Some(message);
                    return Err(error).context("osc_reply_schema_rejected");
                }
            };
            return Ok(ReadReceipt {
                query,
                requested_device_index: device_index,
                source,
                local_reply_address: self.local_addr()?,
                received_at: chrono::Local::now().to_rfc3339(),
                elapsed_ms: start.elapsed().as_millis(),
                ignored_packets: ignored,
                message,
                reply,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(address: &str, arguments: Vec<OscArg>) -> OscMessage {
        OscMessage {
            address: address.into(),
            arguments,
        }
    }
    fn tracking(index: Option<i32>, state: i32) -> OscMessage {
        let mut arguments = index.into_iter().map(OscArg::Int).collect::<Vec<_>>();
        arguments.push(OscArg::Int(state));
        msg(ReadQuery::AiTrackingInfo.response_address(), arguments)
    }
    fn device_info() -> OscMessage {
        msg(
            ReadQuery::DeviceInfo.response_address(),
            vec![
                OscArg::Int(1),
                OscArg::String("Synthetic Tiny 3".into()),
                OscArg::Int(0),
                OscArg::String(String::new()),
                OscArg::Int(0),
                OscArg::String(String::new()),
                OscArg::Int(0),
                OscArg::String(String::new()),
                OscArg::Int(0),
                OscArg::Int(1),
                OscArg::Int(999),
            ],
        )
    }
    async fn fixture_socket() -> UdpSocket {
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap()
    }

    #[test]
    fn golden_big_endian_words_and_zero_padding() {
        let packet = encode_message(&msg(
            "/x",
            vec![
                OscArg::Int(7),
                OscArg::Float(1.5),
                OscArg::String("hi".into()),
            ],
        ))
        .unwrap();
        assert_eq!(
            packet,
            vec![
                b'/', b'x', 0, 0, b',', b'i', b'f', b's', 0, 0, 0, 0, 0, 0, 0, 7, 0x3f, 0xc0, 0, 0,
                b'h', b'i', 0, 0
            ]
        );
        assert_eq!(
            decode_message(&packet).unwrap(),
            msg(
                "/x",
                vec![
                    OscArg::Int(7),
                    OscArg::Float(1.5),
                    OscArg::String("hi".into())
                ]
            )
        );
        for query in ReadQuery::ALL {
            let wire = encode_message(&query.request(2).unwrap()).unwrap();
            let address_len = (query.address().len() + 1).next_multiple_of(4);
            assert_eq!(&wire[..query.address().len()], query.address().as_bytes());
            assert!(
                wire[query.address().len()..address_len]
                    .iter()
                    .all(|b| *b == 0)
            );
            assert_eq!(&wire[address_len..address_len + 4], b",ii\0");
            assert_eq!(&wire[address_len + 4..], &[0, 0, 0, 2, 0, 0, 0, 0]);
        }
    }

    #[test]
    fn packet_string_argument_and_address_bounds() {
        assert!(encode_message(&msg("/x", vec![OscArg::Int(0); MAX_ARGUMENTS])).is_ok());
        assert!(encode_message(&msg("/x", vec![OscArg::Int(0); MAX_ARGUMENTS + 1])).is_err());
        let max_string = "a".repeat(MAX_STRING_BYTES);
        let wire = encode_message(&msg("/x", vec![OscArg::String(max_string.clone())])).unwrap();
        assert_eq!(
            decode_message(&wire).unwrap().arguments,
            vec![OscArg::String(max_string)]
        );
        assert!(
            encode_message(&msg(
                "/x",
                vec![OscArg::String("a".repeat(MAX_STRING_BYTES + 1))]
            ))
            .is_err()
        );
        assert!(
            encode_message(&msg(
                "/x",
                vec![OscArg::String("a".repeat(MAX_STRING_BYTES)); 4]
            ))
            .is_err()
        );
        assert!(
            encode_message(&msg(
                &format!("/{}", "a".repeat(MAX_ADDRESS_BYTES - 1)),
                vec![]
            ))
            .is_ok()
        );
        assert!(
            encode_message(&msg(&format!("/{}", "a".repeat(MAX_ADDRESS_BYTES)), vec![])).is_err()
        );
        assert!(decode_message(&vec![0; MAX_DATAGRAM_BYTES + 4]).is_err());
        assert!(encode_message(&msg("/x", vec![OscArg::String("nul\0inside".into())])).is_err());
        assert!(encode_message(&msg("/x/*", vec![])).is_err());
    }

    #[test]
    fn truncated_unsupported_nonfinite_and_malformed_packets_rejected() {
        let wire = encode_message(&msg(
            "/x",
            vec![OscArg::Int(7), OscArg::String("测距😀".into())],
        ))
        .unwrap();
        for cut in 0..wire.len() {
            assert!(decode_message(&wire[..cut]).is_err(), "cut {cut}");
        }
        let mut trailing = wire.clone();
        trailing.extend([0; 4]);
        assert!(decode_message(&trailing).is_err());
        assert!(decode_message(b"#bundle\0\0\0\0\0\0\0\0\x01").is_err());
        assert!(decode_message(b"/x\0\0,b\0\0\0\0\0\0").is_err());
        assert!(decode_message(b"/x\0\0,i\0\0").is_err());
        assert!(decode_message(b"/x\0\x01,i\0\0\0\0\0\0").is_err());
        assert!(decode_message(b"/x\0\0,s\0\0\xff\0\0\0").is_err());
        let mut nonfinite = encode_message(&msg("/x", vec![OscArg::Float(1.0)])).unwrap();
        nonfinite[8..].copy_from_slice(&f32::NAN.to_bits().to_be_bytes());
        assert!(decode_message(&nonfinite).is_err());
        assert!(encode_message(&msg("/x", vec![OscArg::Float(f32::INFINITY)])).is_err());
    }

    #[test]
    fn typed_reply_provenance_and_ranges_are_conservative() {
        let indexed = parse_reply(ReadQuery::AiTrackingInfo, 2, &tracking(Some(2), 0)).unwrap();
        assert!(indexed.device_binding_verified);
        assert_eq!(indexed.state, ReadState::AiTrackingInfo { locked: false });
        let unindexed = parse_reply(ReadQuery::AiTrackingInfo, 2, &tracking(None, 1)).unwrap();
        assert!(!unindexed.device_binding_verified);
        assert_eq!(unindexed.echoed_device_index, None);
        assert!(parse_reply(ReadQuery::AiTrackingInfo, 2, &tracking(Some(3), 0)).is_err());
        assert!(parse_reply(ReadQuery::AiTrackingInfo, 2, &tracking(Some(2), 2)).is_err());
        assert!(
            parse_reply(
                ReadQuery::GimbalPosition,
                2,
                &msg(
                    ReadQuery::GimbalPosition.response_address(),
                    vec![OscArg::Int(0), OscArg::Int(91), OscArg::Int(0)]
                )
            )
            .is_err()
        );
        let info = parse_reply(ReadQuery::DeviceInfo, 2, &device_info()).unwrap();
        assert!(!info.device_binding_verified);
        let ReadState::DeviceInfo(info) = info.state else {
            panic!("device info expected")
        };
        assert_eq!(info.selected_device_type_code, 999); // no guessed newer-model mapping
        assert_eq!(info.slots.len(), 4);
        assert!(ReadQuery::DeviceInfo.request(-1).is_err());
        assert!(ReadQuery::DeviceInfo.request(MAX_DEVICE_INDEX + 1).is_err());
    }

    #[tokio::test]
    async fn localhost_three_read_commands_and_expected_response_shapes() {
        let fixture = fixture_socket().await;
        let port = fixture.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let mut buffer = [0; MAX_DATAGRAM_BYTES + 1];
            for query in ReadQuery::ALL {
                let (size, peer) = fixture.recv_from(&mut buffer).await.unwrap();
                assert_eq!(peer.ip(), Ipv4Addr::LOCALHOST);
                assert_eq!(
                    decode_message(&buffer[..size]).unwrap(),
                    query.request(2).unwrap()
                );
                let reply = match query {
                    ReadQuery::DeviceInfo | ReadQuery::DeviceInfoViaHandshake => device_info(),
                    ReadQuery::AiTrackingInfo => tracking(Some(2), 0),
                    ReadQuery::GimbalPosition => msg(
                        query.response_address(),
                        vec![OscArg::Int(0), OscArg::Int(-5), OscArg::Int(12)],
                    ),
                };
                fixture
                    .send_to(&encode_message(&reply).unwrap(), peer)
                    .await
                    .unwrap();
            }
        });
        let mut client = OscReadClient::bind(port, 0, Duration::from_millis(500))
            .await
            .unwrap();
        for query in ReadQuery::ALL {
            let receipt = client.query(query, 2).await.unwrap();
            assert_eq!(receipt.query, query);
            assert_eq!(receipt.source.port(), port);
            assert_eq!(receipt.ignored_packets, 0);
            if query == ReadQuery::GimbalPosition {
                assert_eq!(
                    receipt.reply.state,
                    ReadState::GimbalPosition {
                        roll_unused_degrees: 0,
                        pitch_degrees: -5,
                        yaw_degrees: 12
                    }
                );
                assert!(!receipt.reply.device_binding_verified);
            }
        }
        task.await.unwrap();
    }

    #[tokio::test]
    async fn foreign_source_port_is_ignored_before_correct_reply() {
        let fixture = fixture_socket().await;
        let foreign = fixture_socket().await;
        let port = fixture.local_addr().unwrap().port();
        let foreign_address = foreign.local_addr().unwrap();
        assert_ne!(port, foreign.local_addr().unwrap().port());
        let task = tokio::spawn(async move {
            let mut buffer = [0; 512];
            let (_, peer) = fixture.recv_from(&mut buffer).await.unwrap();
            foreign
                .send_to(&encode_message(&tracking(Some(0), 1)).unwrap(), peer)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            fixture
                .send_to(&encode_message(&tracking(Some(0), 0)).unwrap(), peer)
                .await
                .unwrap();
        });
        let mut client = OscReadClient::bind(port, 0, Duration::from_millis(500))
            .await
            .unwrap();
        let receipt = client.query(ReadQuery::AiTrackingInfo, 0).await.unwrap();
        assert_eq!(receipt.ignored_packets, 1);
        assert_eq!(client.diagnostics().rejected_sources, vec![foreign_address]);
        assert!(client.diagnostics().unexpected_messages.is_empty());
        assert!(client.diagnostics().last_rejected_reply.is_none());
        assert_eq!(
            receipt.reply.state,
            ReadState::AiTrackingInfo { locked: false }
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_does_not_accept_a_valid_packet_from_wrong_port() {
        let fixture = fixture_socket().await;
        let port = fixture.local_addr().unwrap().port();
        let foreign = fixture_socket().await;
        let foreign_address = foreign.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 512];
            let (_, peer) = fixture.recv_from(&mut buffer).await.unwrap();
            foreign
                .send_to(&encode_message(&tracking(Some(0), 0)).unwrap(), peer)
                .await
                .unwrap();
        });
        let mut client = OscReadClient::bind(port, 0, Duration::from_millis(50))
            .await
            .unwrap();
        let error = client
            .query(ReadQuery::AiTrackingInfo, 0)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "osc_timeout");
        assert_eq!(client.diagnostics().rejected_sources, vec![foreign_address]);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn native_udp_truncated_and_oversized_datagrams_fail_closed() {
        for oversized in [false, true] {
            let fixture = fixture_socket().await;
            let port = fixture.local_addr().unwrap().port();
            let task = tokio::spawn(async move {
                let mut buffer = [0; 512];
                let (_, peer) = fixture.recv_from(&mut buffer).await.unwrap();
                let payload = if oversized {
                    vec![b'x'; MAX_DATAGRAM_BYTES + 200]
                } else {
                    let mut packet = encode_message(&tracking(Some(0), 0)).unwrap();
                    packet.truncate(packet.len() - 4);
                    packet
                };
                fixture.send_to(&payload, peer).await.unwrap();
            });
            let mut client = OscReadClient::bind(port, 0, Duration::from_millis(500))
                .await
                .unwrap();
            let error = client
                .query(ReadQuery::AiTrackingInfo, 0)
                .await
                .unwrap_err();
            assert_ne!(error.to_string(), "osc_timeout");
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn explicit_ports_deadline_and_foreign_packet_budget() {
        assert!(
            OscReadClient::bind(0, 0, Duration::from_millis(50))
                .await
                .is_err()
        );
        assert!(
            OscReadClient::bind(12345, 12345, Duration::from_millis(50))
                .await
                .is_err()
        );
        assert!(OscReadClient::bind(12345, 0, Duration::ZERO).await.is_err());
        assert!(
            OscReadClient::bind(12345, 0, MAX_TIMEOUT + Duration::from_millis(1))
                .await
                .is_err()
        );
        let fixture = fixture_socket().await;
        let port = fixture.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 512];
            let (_, peer) = fixture.recv_from(&mut buffer).await.unwrap();
            let foreign = fixture_socket().await;
            for _ in 0..=MAX_IGNORED_PACKETS {
                foreign
                    .send_to(&encode_message(&tracking(Some(0), 0)).unwrap(), peer)
                    .await
                    .unwrap();
            }
        });
        let mut client = OscReadClient::bind(port, 0, Duration::from_millis(500))
            .await
            .unwrap();
        let error = client
            .query(ReadQuery::AiTrackingInfo, 0)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "osc_ignored_packet_limit");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn only_trusted_source_replies_enter_bounded_protocol_diagnostics() {
        let fixture = fixture_socket().await;
        let port = fixture.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 512];
            let (_, peer) = fixture.recv_from(&mut buffer).await.unwrap();
            for value in 0..6 {
                let unknown = msg(
                    "/OBSBOT/WebCam/General/SyntheticUnknown",
                    vec![OscArg::Int(value)],
                );
                fixture
                    .send_to(&encode_message(&unknown).unwrap(), peer)
                    .await
                    .unwrap();
            }
            let rejected = msg(
                ReadQuery::GimbalPosition.response_address(),
                vec![OscArg::Int(0); 5],
            );
            fixture
                .send_to(&encode_message(&rejected).unwrap(), peer)
                .await
                .unwrap();
        });
        let mut client = OscReadClient::bind(port, 0, Duration::from_millis(500))
            .await
            .unwrap();
        let error = client
            .query(ReadQuery::GimbalPosition, 0)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "osc_reply_schema_rejected");
        let diag = client.diagnostics();
        assert_eq!(diag.unexpected_messages.len(), MAX_DIAGNOSTIC_MESSAGES);
        assert_eq!(diag.unexpected_messages[0].arguments, vec![OscArg::Int(0)]);
        assert_eq!(
            diag.last_rejected_reply.as_ref().unwrap().arguments.len(),
            5
        );
        assert_eq!(
            diag.last_protocol_error.as_deref(),
            Some("osc_reply_argument_count")
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn explicit_handshake_sends_one_integer_and_preserves_real_query_identity() {
        let fixture = fixture_socket().await;
        let port = fixture.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 512];
            let (size, peer) = fixture.recv_from(&mut buffer).await.unwrap();
            let request = decode_message(&buffer[..size]).unwrap();
            assert_eq!(request.address, "/OBSBOT/WebCam/General/Connected");
            assert_eq!(request.arguments, vec![OscArg::Int(1)]);
            let ack = msg("/OBSBOT/WebCam/General/ConnectedResp", vec![OscArg::Int(1)]);
            fixture
                .send_to(&encode_message(&ack).unwrap(), peer)
                .await
                .unwrap();
            fixture
                .send_to(&encode_message(&device_info()).unwrap(), peer)
                .await
                .unwrap();
        });
        let mut client = OscReadClient::bind(port, 0, Duration::from_millis(500))
            .await
            .unwrap();
        let receipt = client.handshake_device_info(0).await.unwrap();
        assert_eq!(receipt.query, ReadQuery::DeviceInfoViaHandshake);
        assert!(matches!(receipt.reply.state, ReadState::DeviceInfo(_)));
        assert_eq!(client.diagnostics().unexpected_messages.len(), 1);
        assert_eq!(
            client.diagnostics().unexpected_messages[0].address,
            "/OBSBOT/WebCam/General/ConnectedResp"
        );
        task.await.unwrap();
    }

    #[test]
    fn explicit_tiny3_mapping_matches_read_only_cross_check_without_native_tilt_flip() {
        let reply = msg(
            ReadQuery::GimbalPosition.response_address(),
            vec![OscArg::Int(17), OscArg::Int(-24)],
        );
        assert!(parse_reply(ReadQuery::GimbalPosition, 0, &reply).is_err());
        let parsed = parse_reply_with_profile(
            ReadQuery::GimbalPosition,
            0,
            &reply,
            GimbalProfile::Tiny3Center2015PanPitch,
        )
        .unwrap();
        assert_eq!(
            parsed.state,
            ReadState::GimbalPosition {
                roll_unused_degrees: 0,
                pitch_degrees: -24,
                yaw_degrees: 17
            }
        );
        assert_eq!(
            parsed.gimbal_profile,
            Some(GimbalProfile::Tiny3Center2015PanPitch)
        );
        assert!(
            parsed
                .mapping_source
                .unwrap()
                .contains("read-only cross-check")
        );
        assert!(!parsed.device_binding_verified);
        let three = msg(
            ReadQuery::GimbalPosition.response_address(),
            vec![OscArg::Int(0), OscArg::Int(-24), OscArg::Int(17)],
        );
        assert!(parse_reply(ReadQuery::GimbalPosition, 0, &three).is_ok());
        assert!(
            parse_reply_with_profile(
                ReadQuery::GimbalPosition,
                0,
                &three,
                GimbalProfile::Tiny3Center2015PanPitch
            )
            .is_err()
        );
        for values in [[141, 0], [0, 91]] {
            let invalid = msg(
                ReadQuery::GimbalPosition.response_address(),
                values.into_iter().map(OscArg::Int).collect(),
            );
            assert!(
                parse_reply_with_profile(
                    ReadQuery::GimbalPosition,
                    0,
                    &invalid,
                    GimbalProfile::Tiny3Center2015PanPitch
                )
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn udp_two_integer_reply_requires_explicit_profile() {
        for profile in [
            GimbalProfile::DocumentedRollPitchYaw,
            GimbalProfile::Tiny3Center2015PanPitch,
        ] {
            let fixture = fixture_socket().await;
            let port = fixture.local_addr().unwrap().port();
            let task = tokio::spawn(async move {
                let mut buffer = [0; 512];
                let (_, peer) = fixture.recv_from(&mut buffer).await.unwrap();
                let reply = msg(
                    ReadQuery::GimbalPosition.response_address(),
                    vec![OscArg::Int(17), OscArg::Int(-24)],
                );
                fixture
                    .send_to(&encode_message(&reply).unwrap(), peer)
                    .await
                    .unwrap();
            });
            let mut client =
                OscReadClient::bind_with_profile(port, 0, Duration::from_millis(500), profile)
                    .await
                    .unwrap();
            let result = client.query(ReadQuery::GimbalPosition, 0).await;
            if profile == GimbalProfile::DocumentedRollPitchYaw {
                assert!(result.is_err());
            } else {
                assert_eq!(
                    result.unwrap().reply.state,
                    ReadState::GimbalPosition {
                        roll_unused_degrees: 0,
                        pitch_degrees: -24,
                        yaw_degrees: 17
                    }
                );
            }
            task.await.unwrap();
        }
    }
}

//! Binary wire format for the dedicated port-forward data plane.
//!
//! Port-forward bytes used to ride the session control socket (`/ws/session`)
//! as base64 strings inside JSON `TunnelData` frames. That coupled a bulk data
//! plane to a control plane: forward traffic contended with agent stdio and
//! **heartbeats**, so a busy preview could delay heartbeats past the liveness
//! deadline, get the connection evicted, and take the agent session down with
//! it. It also paid base64's +33% inflation plus encode/decode CPU on every
//! chunk, and a JSON envelope with the stream id as a 36-char hex string.
//!
//! This module defines the replacement: a second, dial-out WebSocket carrying
//! **binary** frames and nothing else. `ws_bridge`'s blanket `WsCodec` impl
//! encodes any `Serialize + DeserializeOwned` type as JSON *text* (and rejects
//! binary frames outright), so [`TunnelFrame`] deliberately does **not** derive
//! `Serialize`/`Deserialize` — deriving them would both force text framing and
//! collide with the hand-written [`WsCodec`] impl below (E0119).
//!
//! # Frame layout
//!
//! One `u8` type tag, then a per-variant body. Every stream-scoped body starts
//! with the raw 16-byte stream id (not its hex form), so a `Data` frame costs a
//! flat **17 bytes** of overhead regardless of payload:
//!
//! ```text
//! 0x00 Hello   [ticket utf8…]
//! 0x01 Open    [stream_id:16][port:u16 BE]
//! 0x02 Opened  [stream_id:16]
//! 0x03 Refused [stream_id:16][reason:u8]
//! 0x04 Data    [stream_id:16][bytes…]
//! 0x05 Window  [stream_id:16][add_bytes:u32 BE]
//! 0x06 Close   [stream_id:16][reason utf8…]   (empty ⇒ None)
//! ```
//!
//! Multi-byte integers are big-endian. The type tags and the `Refused` reason
//! codes are a **wire contract**: they are matched by build-skewed peers during
//! a rolling deploy, so existing values may never be renumbered — only new
//! ones appended. `tunnel_frame_type_tags_are_stable` and
//! `refuse_reason_codes_are_stable` pin them.
//!
//! The codec validates only *structure* (known tag, sufficient length, valid
//! UTF-8). Chunk-size policy stays with the callers, which enforce it against
//! their negotiated flow-control window.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::{DecodeError, EncodeError, WsCodec, WsEndpoint, WsMessage};

use super::types::TunnelRefuseReason;

/// Per-stream tunnel sizing, negotiated once per connection at registration and
/// then fixed for that connection's life (#1511).
///
/// Both ends must use the same values: the sender chunks payloads to `max_chunk`
/// and the receiver **closes any stream whose frame exceeds it**; the sender's
/// credit gate and the receiver's credit book are both seeded from
/// `initial_window`, so a mismatch makes the sender overrun the book and every
/// stream dies as "beyond granted window". That is why raising these is a
/// negotiation, not a constant bump — see [`crate::PROXY_CAPABILITY_TUNNEL_BINARY_V2`].
///
/// The backend computes the agreed profile from the proxy's advertised
/// capabilities and reports it back in `RegisterAck.tunnel_sizing`; both ends
/// then configure their tunnel to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelSizing {
    /// Max decoded payload bytes per `Data` frame.
    pub max_chunk: u32,
    /// Initial per-stream, per-direction flow-control credit.
    pub initial_window: u32,
}

impl TunnelSizing {
    /// The original profile (`session.tunnel_binary_v1`): 16 KiB frames, 64 KiB
    /// window. Also the default whenever nothing was negotiated — an older
    /// backend that sends no `tunnel_sizing`, or the JSON-over-control-socket
    /// fallback.
    pub const V1: TunnelSizing = TunnelSizing {
        max_chunk: 16 * 1024,
        initial_window: 64 * 1024,
    };

    /// The larger profile (`session.tunnel_binary_v2`): 64 KiB frames, 256 KiB
    /// window. The 4× window-to-chunk ratio matches V1, so a single stream can
    /// keep four frames in flight before waiting on a grant.
    pub const V2: TunnelSizing = TunnelSizing {
        max_chunk: 64 * 1024,
        initial_window: 256 * 1024,
    };

    /// The sizing to use given a proxy's advertised capabilities. The backend is
    /// the negotiator: it supports every profile up to the newest it knows and
    /// picks the highest the proxy also advertised. Unknown/empty capabilities
    /// yield [`TunnelSizing::V1`], which is what a pre-#1511 proxy gets.
    pub fn negotiate(proxy_capabilities: &[String]) -> TunnelSizing {
        let advertises = |cap: &str| proxy_capabilities.iter().any(|c| c == cap);
        if advertises(crate::PROXY_CAPABILITY_TUNNEL_BINARY_V2) {
            TunnelSizing::V2
        } else {
            TunnelSizing::V1
        }
    }
}

impl Default for TunnelSizing {
    fn default() -> Self {
        TunnelSizing::V1
    }
}

/// The dedicated data-plane endpoint. Symmetric: both directions speak
/// [`TunnelFrame`].
///
/// Opened by the proxy *in addition to* [`SessionEndpoint`](super::SessionEndpoint)
/// and bound to that control connection's generation by the `Hello` ticket, so
/// a reconnect can never cross a new data socket with a stale session.
pub struct TunnelDataEndpoint;

impl WsEndpoint for TunnelDataEndpoint {
    const PATH: &'static str = "/ws/session/data";
    type ServerMsg = TunnelFrame;
    type ClientMsg = TunnelFrame;
}

/// Size of a stream-scoped frame header: the type tag plus the raw stream id.
/// A `Data` frame is exactly this many bytes larger than its payload.
pub const TUNNEL_FRAME_HEADER_LEN: usize = 1 + 16;

// Frame type tags. Wire contract — append only, never renumber.
const TAG_HELLO: u8 = 0x00;
const TAG_OPEN: u8 = 0x01;
const TAG_OPENED: u8 = 0x02;
const TAG_REFUSED: u8 = 0x03;
const TAG_DATA: u8 = 0x04;
const TAG_WINDOW: u8 = 0x05;
const TAG_CLOSE: u8 = 0x06;

/// One frame on the binary data plane.
///
/// Intentionally *not* `Serialize`/`Deserialize` — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelFrame {
    /// First frame after connect (proxy → backend): authenticate this socket
    /// and bind it to the session + control-connection generation named by the
    /// ticket. The ticket is a short-TTL JWT handed to the proxy in
    /// `RegisterAck`; it travels in the frame body rather than a query
    /// parameter so it never lands in access logs.
    Hello { ticket: String },

    /// Backend → proxy: dial `127.0.0.1:{port}` for a new stream.
    Open { stream_id: Uuid, port: u16 },

    /// Proxy → backend: the dial succeeded; the stream is live.
    Opened { stream_id: Uuid },

    /// Proxy → backend: the stream could not be opened.
    Refused {
        stream_id: Uuid,
        reason: TunnelRefuseReason,
    },

    /// Either direction: stream payload. Raw bytes — no base64.
    Data { stream_id: Uuid, bytes: Vec<u8> },

    /// Either direction: grant the peer `add_bytes` more send credit.
    Window { stream_id: Uuid, add_bytes: u32 },

    /// Either direction: tear the stream down (no half-close). `reason` is
    /// diagnostic only; an empty reason on the wire decodes as `None`.
    Close {
        stream_id: Uuid,
        reason: Option<String>,
    },
}

impl TunnelFrame {
    /// The stream this frame belongs to, or `None` for connection-scoped
    /// frames (`Hello`).
    pub fn stream_id(&self) -> Option<Uuid> {
        match self {
            Self::Hello { .. } => None,
            Self::Open { stream_id, .. }
            | Self::Opened { stream_id }
            | Self::Refused { stream_id, .. }
            | Self::Data { stream_id, .. }
            | Self::Window { stream_id, .. }
            | Self::Close { stream_id, .. } => Some(*stream_id),
        }
    }
}

/// Numeric code for a refusal reason. Wire contract — append only.
fn refuse_reason_code(reason: TunnelRefuseReason) -> u8 {
    match reason {
        TunnelRefuseReason::NoListener => 0,
        TunnelRefuseReason::StreamLimit => 1,
        TunnelRefuseReason::NotForwarded => 2,
        TunnelRefuseReason::Protocol => 3,
    }
}

fn refuse_reason_from_code(code: u8) -> TunnelRefuseReason {
    match code {
        0 => TunnelRefuseReason::NoListener,
        1 => TunnelRefuseReason::StreamLimit,
        2 => TunnelRefuseReason::NotForwarded,
        // A newer peer sent a reason this build doesn't know. Degrade to
        // `Protocol` rather than dropping the frame: the stream still has to be
        // failed, and inventing a decode error would strand it half-open.
        _ => TunnelRefuseReason::Protocol,
    }
}

/// Split the raw stream id off the front of a frame body.
fn split_stream_id(body: &[u8], what: &str) -> Result<(Uuid, usize), DecodeError> {
    if body.len() < 16 {
        return Err(DecodeError::InvalidData(format!(
            "{what} frame body is {} bytes, need at least 16 for a stream id",
            body.len()
        )));
    }
    let raw: [u8; 16] = body[..16].try_into().expect("length checked above");
    Ok((Uuid::from_bytes(raw), 16))
}

fn exact_len(body: &[u8], want: usize, what: &str) -> Result<(), DecodeError> {
    if body.len() != want {
        return Err(DecodeError::InvalidData(format!(
            "{what} frame body is {} bytes, expected exactly {want}",
            body.len()
        )));
    }
    Ok(())
}

fn utf8(bytes: &[u8], what: &str) -> Result<String, DecodeError> {
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|e| DecodeError::InvalidData(format!("{what} frame text is not valid UTF-8: {e}")))
}

impl WsCodec for TunnelFrame {
    fn encode(&self) -> Result<WsMessage, EncodeError> {
        let mut out = Vec::with_capacity(match self {
            Self::Data { bytes, .. } => TUNNEL_FRAME_HEADER_LEN + bytes.len(),
            _ => TUNNEL_FRAME_HEADER_LEN + 8,
        });
        match self {
            Self::Hello { ticket } => {
                out.push(TAG_HELLO);
                out.extend_from_slice(ticket.as_bytes());
            }
            Self::Open { stream_id, port } => {
                out.push(TAG_OPEN);
                out.extend_from_slice(stream_id.as_bytes());
                out.extend_from_slice(&port.to_be_bytes());
            }
            Self::Opened { stream_id } => {
                out.push(TAG_OPENED);
                out.extend_from_slice(stream_id.as_bytes());
            }
            Self::Refused { stream_id, reason } => {
                out.push(TAG_REFUSED);
                out.extend_from_slice(stream_id.as_bytes());
                out.push(refuse_reason_code(*reason));
            }
            Self::Data { stream_id, bytes } => {
                out.push(TAG_DATA);
                out.extend_from_slice(stream_id.as_bytes());
                out.extend_from_slice(bytes);
            }
            Self::Window {
                stream_id,
                add_bytes,
            } => {
                out.push(TAG_WINDOW);
                out.extend_from_slice(stream_id.as_bytes());
                out.extend_from_slice(&add_bytes.to_be_bytes());
            }
            Self::Close { stream_id, reason } => {
                out.push(TAG_CLOSE);
                out.extend_from_slice(stream_id.as_bytes());
                if let Some(reason) = reason {
                    out.extend_from_slice(reason.as_bytes());
                }
            }
        }
        Ok(WsMessage::Binary(out))
    }

    fn decode(msg: WsMessage) -> Result<Self, DecodeError> {
        let buf = match msg {
            WsMessage::Binary(buf) => buf,
            // The data plane is binary-only; a text frame means a peer is
            // speaking the old JSON protocol on the wrong socket.
            WsMessage::Text(_) => return Err(DecodeError::UnexpectedText),
        };
        let (&tag, body) = buf
            .split_first()
            .ok_or_else(|| DecodeError::InvalidData("empty tunnel frame".to_string()))?;

        match tag {
            TAG_HELLO => Ok(Self::Hello {
                ticket: utf8(body, "Hello")?,
            }),
            TAG_OPEN => {
                let (stream_id, n) = split_stream_id(body, "Open")?;
                exact_len(&body[n..], 2, "Open")?;
                let port = u16::from_be_bytes([body[n], body[n + 1]]);
                Ok(Self::Open { stream_id, port })
            }
            TAG_OPENED => {
                let (stream_id, n) = split_stream_id(body, "Opened")?;
                exact_len(&body[n..], 0, "Opened")?;
                Ok(Self::Opened { stream_id })
            }
            TAG_REFUSED => {
                let (stream_id, n) = split_stream_id(body, "Refused")?;
                exact_len(&body[n..], 1, "Refused")?;
                Ok(Self::Refused {
                    stream_id,
                    reason: refuse_reason_from_code(body[n]),
                })
            }
            TAG_DATA => {
                let (stream_id, n) = split_stream_id(body, "Data")?;
                Ok(Self::Data {
                    stream_id,
                    bytes: body[n..].to_vec(),
                })
            }
            TAG_WINDOW => {
                let (stream_id, n) = split_stream_id(body, "Window")?;
                exact_len(&body[n..], 4, "Window")?;
                let add_bytes =
                    u32::from_be_bytes([body[n], body[n + 1], body[n + 2], body[n + 3]]);
                Ok(Self::Window {
                    stream_id,
                    add_bytes,
                })
            }
            TAG_CLOSE => {
                let (stream_id, n) = split_stream_id(body, "Close")?;
                let rest = &body[n..];
                let reason = if rest.is_empty() {
                    None
                } else {
                    Some(utf8(rest, "Close")?)
                };
                Ok(Self::Close { stream_id, reason })
            }
            other => Err(DecodeError::InvalidData(format!(
                "unknown tunnel frame type 0x{other:02x}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> Uuid {
        Uuid::parse_str("e2d342f5-68c6-4134-a5d8-63cb4afcee9e").unwrap()
    }

    fn roundtrip(frame: &TunnelFrame) -> TunnelFrame {
        let encoded = frame.encode().expect("encode");
        assert!(
            matches!(encoded, WsMessage::Binary(_)),
            "data plane must emit binary frames, got {encoded:?}"
        );
        TunnelFrame::decode(encoded).expect("decode")
    }

    fn bytes_of(frame: &TunnelFrame) -> Vec<u8> {
        match frame.encode().expect("encode") {
            WsMessage::Binary(b) => b,
            other => panic!("expected binary, got {other:?}"),
        }
    }

    #[test]
    fn every_variant_roundtrips() {
        let frames = [
            TunnelFrame::Hello {
                ticket: "eyJhbGciOi.payload.sig".to_string(),
            },
            TunnelFrame::Open {
                stream_id: sid(),
                port: 5173,
            },
            TunnelFrame::Opened { stream_id: sid() },
            TunnelFrame::Refused {
                stream_id: sid(),
                reason: TunnelRefuseReason::StreamLimit,
            },
            TunnelFrame::Data {
                stream_id: sid(),
                bytes: b"GET / HTTP/1.1\r\n\r\n".to_vec(),
            },
            TunnelFrame::Window {
                stream_id: sid(),
                add_bytes: 262_144,
            },
            TunnelFrame::Close {
                stream_id: sid(),
                reason: Some("local write failed".to_string()),
            },
            TunnelFrame::Close {
                stream_id: sid(),
                reason: None,
            },
        ];
        for frame in frames {
            assert_eq!(roundtrip(&frame), frame, "roundtrip failed for {frame:?}");
        }
    }

    #[test]
    fn every_refuse_reason_roundtrips() {
        for reason in [
            TunnelRefuseReason::NoListener,
            TunnelRefuseReason::StreamLimit,
            TunnelRefuseReason::NotForwarded,
            TunnelRefuseReason::Protocol,
        ] {
            let frame = TunnelFrame::Refused {
                stream_id: sid(),
                reason,
            };
            assert_eq!(roundtrip(&frame), frame);
        }
    }

    /// The whole point of the binary plane: payload bytes survive verbatim and
    /// cost a flat 17-byte header — no base64 inflation.
    #[test]
    fn data_payload_is_byte_exact_with_flat_overhead() {
        for payload in [
            Vec::new(),
            vec![0u8; 1],
            // Every byte value, including NULs and invalid UTF-8 — the shapes
            // that forced base64 on the JSON plane.
            (0u8..=255).collect::<Vec<u8>>(),
            vec![0xffu8; 64 * 1024],
        ] {
            let frame = TunnelFrame::Data {
                stream_id: sid(),
                bytes: payload.clone(),
            };
            assert_eq!(
                bytes_of(&frame).len(),
                TUNNEL_FRAME_HEADER_LEN + payload.len(),
                "Data overhead must be exactly {TUNNEL_FRAME_HEADER_LEN} bytes"
            );
            match roundtrip(&frame) {
                TunnelFrame::Data { bytes, stream_id } => {
                    assert_eq!(bytes, payload);
                    assert_eq!(stream_id, sid());
                }
                other => panic!("expected Data, got {other:?}"),
            }
        }
    }

    /// A 16 KiB chunk on the old JSON plane cost base64 (+33%) plus a JSON
    /// envelope with a 36-char hex stream id. Pin the win so a regression to a
    /// text/base64 encoding can't slip back in unnoticed.
    #[test]
    fn binary_framing_beats_the_base64_json_envelope() {
        let payload = vec![0xabu8; 16 * 1024];
        let binary_len = bytes_of(&TunnelFrame::Data {
            stream_id: sid(),
            bytes: payload.clone(),
        })
        .len();

        // What the same chunk costs as `TunnelData { stream_id, data_base64 }`.
        let json_len = serde_json::to_string(&crate::TunnelDataFields {
            stream_id: sid(),
            data_base64: base64_encode(&payload),
        })
        .expect("json")
        .len();

        assert!(
            binary_len < json_len,
            "binary ({binary_len}) should be smaller than JSON+base64 ({json_len})"
        );
        // Base64 alone is +33%, so the saving is substantial, not marginal.
        assert!(
            (binary_len as f64) < (json_len as f64) * 0.80,
            "expected >20% saving; binary={binary_len} json={json_len}"
        );
    }

    /// Minimal base64 encoder so this crate's test doesn't take a dependency
    /// just to size the comparison above.
    fn base64_encode(input: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            let idx = [
                (n >> 18) & 0x3f,
                (n >> 12) & 0x3f,
                (n >> 6) & 0x3f,
                n & 0x3f,
            ];
            for (i, part) in idx.iter().enumerate() {
                if i <= chunk.len() {
                    out.push(ALPHABET[*part as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn stream_id_is_exposed_for_routing_and_none_for_hello() {
        assert_eq!(
            TunnelFrame::Data {
                stream_id: sid(),
                bytes: vec![]
            }
            .stream_id(),
            Some(sid())
        );
        assert_eq!(TunnelFrame::Hello { ticket: "t".into() }.stream_id(), None);
    }

    #[test]
    fn text_frames_are_rejected() {
        // The old JSON protocol arriving on the data socket must fail loudly,
        // not be silently misread.
        let err = TunnelFrame::decode(WsMessage::Text("{\"type\":\"TunnelData\"}".into()));
        assert!(matches!(err, Err(DecodeError::UnexpectedText)));
    }

    #[test]
    fn malformed_frames_are_rejected() {
        // Empty frame.
        assert!(TunnelFrame::decode(WsMessage::Binary(vec![])).is_err());
        // Unknown type tag.
        assert!(TunnelFrame::decode(WsMessage::Binary(vec![0xfe])).is_err());
        // Truncated stream id on every stream-scoped variant.
        for tag in [
            TAG_OPEN,
            TAG_OPENED,
            TAG_REFUSED,
            TAG_DATA,
            TAG_WINDOW,
            TAG_CLOSE,
        ] {
            let mut buf = vec![tag];
            buf.extend_from_slice(&[0u8; 15]);
            assert!(
                TunnelFrame::decode(WsMessage::Binary(buf)).is_err(),
                "tag 0x{tag:02x} with a 15-byte stream id must fail"
            );
        }
        // Wrong fixed-width payload lengths.
        let with_body = |tag: u8, extra: &[u8]| {
            let mut buf = vec![tag];
            buf.extend_from_slice(&[0u8; 16]);
            buf.extend_from_slice(extra);
            WsMessage::Binary(buf)
        };
        assert!(TunnelFrame::decode(with_body(TAG_OPEN, &[1])).is_err()); // port needs 2
        assert!(TunnelFrame::decode(with_body(TAG_WINDOW, &[0, 0, 1])).is_err()); // needs 4
        assert!(TunnelFrame::decode(with_body(TAG_OPENED, &[0])).is_err()); // needs 0
        assert!(TunnelFrame::decode(with_body(TAG_REFUSED, &[])).is_err()); // needs 1
                                                                            // Invalid UTF-8 in a text-bearing frame.
        assert!(TunnelFrame::decode(with_body(TAG_CLOSE, &[0xff, 0xfe])).is_err());
        assert!(TunnelFrame::decode(WsMessage::Binary(vec![TAG_HELLO, 0xff])).is_err());
    }

    /// An unknown refusal reason from a newer peer must still fail the stream
    /// rather than error the frame and strand it half-open.
    #[test]
    fn unknown_refuse_reason_degrades_to_protocol() {
        let mut buf = vec![TAG_REFUSED];
        buf.extend_from_slice(sid().as_bytes());
        buf.push(0x7f);
        assert_eq!(
            TunnelFrame::decode(WsMessage::Binary(buf)).expect("decode"),
            TunnelFrame::Refused {
                stream_id: sid(),
                reason: TunnelRefuseReason::Protocol,
            }
        );
    }

    /// Wire contract: these tags cross build-skewed peers during a rolling
    /// deploy. Renumbering silently corrupts streams, so pin them.
    #[test]
    fn tunnel_frame_type_tags_are_stable() {
        let tag_of = |frame: &TunnelFrame| bytes_of(frame)[0];
        assert_eq!(
            tag_of(&TunnelFrame::Hello {
                ticket: String::new()
            }),
            0x00
        );
        assert_eq!(
            tag_of(&TunnelFrame::Open {
                stream_id: sid(),
                port: 1
            }),
            0x01
        );
        assert_eq!(tag_of(&TunnelFrame::Opened { stream_id: sid() }), 0x02);
        assert_eq!(
            tag_of(&TunnelFrame::Refused {
                stream_id: sid(),
                reason: TunnelRefuseReason::NoListener
            }),
            0x03
        );
        assert_eq!(
            tag_of(&TunnelFrame::Data {
                stream_id: sid(),
                bytes: vec![]
            }),
            0x04
        );
        assert_eq!(
            tag_of(&TunnelFrame::Window {
                stream_id: sid(),
                add_bytes: 0
            }),
            0x05
        );
        assert_eq!(
            tag_of(&TunnelFrame::Close {
                stream_id: sid(),
                reason: None
            }),
            0x06
        );
    }

    #[test]
    fn refuse_reason_codes_are_stable() {
        assert_eq!(refuse_reason_code(TunnelRefuseReason::NoListener), 0);
        assert_eq!(refuse_reason_code(TunnelRefuseReason::StreamLimit), 1);
        assert_eq!(refuse_reason_code(TunnelRefuseReason::NotForwarded), 2);
        assert_eq!(refuse_reason_code(TunnelRefuseReason::Protocol), 3);
    }

    #[test]
    fn data_plane_path_is_distinct_from_the_control_socket() {
        assert_eq!(TunnelDataEndpoint::PATH, "/ws/session/data");
        assert_ne!(
            TunnelDataEndpoint::PATH,
            super::super::SessionEndpoint::PATH
        );
    }
}

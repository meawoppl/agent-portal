//! Chunked image upload protocol used on `/ws/session/upload`.
//!
//! Large images flow from the proxy to the backend on a dedicated WebSocket
//! that mixes JSON-text control frames (`Start`, `Complete`, `Ack`, `Failed`)
//! with raw-binary chunk frames. Sending raw bytes avoids the ~33% base64
//! overhead, and chunking avoids the single-frame size cap that closes the
//! main session socket on oversized payloads.
//!
//! Wire format for a chunk frame (`WsMessage::Binary`):
//! ```text
//!   [ upload_id : 16 bytes ][ offset : u64 LE : 8 bytes ][ raw image bytes ]
//! ```
//! Total header is 24 bytes; `data` is the remainder of the frame.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::{DecodeError, EncodeError, WsCodec, WsEndpoint, WsMessage};

const CHUNK_HEADER_LEN: usize = 24;

pub struct ImageUploadEndpoint;

impl WsEndpoint for ImageUploadEndpoint {
    const PATH: &'static str = "/ws/session/upload";
    type ServerMsg = ImageUploadServerMsg;
    type ClientMsg = ImageUploadClientMsg;
}

/// Proxy -> backend messages on the image upload socket.
///
/// Deliberately does NOT derive `Serialize`/`Deserialize` — that would
/// activate the blanket `WsCodec` impl and force everything through JSON,
/// defeating the point of the binary chunk frames.
#[derive(Debug, Clone)]
pub enum ImageUploadClientMsg {
    /// Announce a new upload. Sent as a JSON text frame.
    Start {
        upload_id: Uuid,
        session_id: Uuid,
        auth_token: String,
        media_type: String,
        total_bytes: u64,
        file_path: Option<String>,
    },
    /// A slice of raw image bytes. Sent as a binary frame.
    Chunk {
        upload_id: Uuid,
        offset: u64,
        data: Vec<u8>,
    },
    /// Finalize the upload. Sent as a JSON text frame.
    Complete { upload_id: Uuid },
}

/// Helper enum used only on the wire for text-frame variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ImageUploadCtrlMsg {
    Start {
        upload_id: Uuid,
        session_id: Uuid,
        auth_token: String,
        media_type: String,
        total_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_path: Option<String>,
    },
    Complete {
        upload_id: Uuid,
    },
}

impl WsCodec for ImageUploadClientMsg {
    fn encode(&self) -> Result<WsMessage, EncodeError> {
        match self {
            Self::Chunk {
                upload_id,
                offset,
                data,
            } => {
                let mut bytes = Vec::with_capacity(CHUNK_HEADER_LEN + data.len());
                bytes.extend_from_slice(upload_id.as_bytes());
                bytes.extend_from_slice(&offset.to_le_bytes());
                bytes.extend_from_slice(data);
                Ok(WsMessage::Binary(bytes))
            }
            Self::Start {
                upload_id,
                session_id,
                auth_token,
                media_type,
                total_bytes,
                file_path,
            } => {
                let ctrl = ImageUploadCtrlMsg::Start {
                    upload_id: *upload_id,
                    session_id: *session_id,
                    auth_token: auth_token.clone(),
                    media_type: media_type.clone(),
                    total_bytes: *total_bytes,
                    file_path: file_path.clone(),
                };
                Ok(WsMessage::Text(serde_json::to_string(&ctrl)?))
            }
            Self::Complete { upload_id } => {
                let ctrl = ImageUploadCtrlMsg::Complete {
                    upload_id: *upload_id,
                };
                Ok(WsMessage::Text(serde_json::to_string(&ctrl)?))
            }
        }
    }

    fn decode(msg: WsMessage) -> Result<Self, DecodeError> {
        match msg {
            WsMessage::Binary(bytes) => {
                if bytes.len() < CHUNK_HEADER_LEN {
                    return Err(DecodeError::InvalidData(format!(
                        "image upload chunk frame too small: {} bytes (need >= {})",
                        bytes.len(),
                        CHUNK_HEADER_LEN
                    )));
                }
                let upload_id = Uuid::from_slice(&bytes[..16]).map_err(|_| {
                    DecodeError::InvalidData("bad upload_id in chunk header".into())
                })?;
                let offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
                let data = bytes[CHUNK_HEADER_LEN..].to_vec();
                Ok(Self::Chunk {
                    upload_id,
                    offset,
                    data,
                })
            }
            WsMessage::Text(text) => {
                let ctrl: ImageUploadCtrlMsg = serde_json::from_str(&text)?;
                Ok(match ctrl {
                    ImageUploadCtrlMsg::Start {
                        upload_id,
                        session_id,
                        auth_token,
                        media_type,
                        total_bytes,
                        file_path,
                    } => Self::Start {
                        upload_id,
                        session_id,
                        auth_token,
                        media_type,
                        total_bytes,
                        file_path,
                    },
                    ImageUploadCtrlMsg::Complete { upload_id } => Self::Complete { upload_id },
                })
            }
        }
    }
}

/// Backend -> proxy responses. JSON-only; uses the blanket `WsCodec` impl.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImageUploadServerMsg {
    /// Upload finalized; image is now retrievable at `image_url`.
    Ack { upload_id: Uuid, image_url: String },
    /// Upload rejected. Proxy should not retry the same upload_id.
    Failed { upload_id: Uuid, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_path() {
        assert_eq!(ImageUploadEndpoint::PATH, "/ws/session/upload");
    }

    #[test]
    fn chunk_roundtrip() {
        let upload_id = Uuid::new_v4();
        let msg = ImageUploadClientMsg::Chunk {
            upload_id,
            offset: 4096,
            data: vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0xff],
        };
        let frame = msg.encode().unwrap();
        match frame {
            WsMessage::Binary(ref bytes) => {
                assert_eq!(bytes.len(), CHUNK_HEADER_LEN + 6);
                assert_eq!(&bytes[..16], upload_id.as_bytes());
                assert_eq!(u64::from_le_bytes(bytes[16..24].try_into().unwrap()), 4096);
            }
            _ => panic!("expected binary frame"),
        }
        let decoded = ImageUploadClientMsg::decode(frame).unwrap();
        match decoded {
            ImageUploadClientMsg::Chunk {
                upload_id: u,
                offset,
                data,
            } => {
                assert_eq!(u, upload_id);
                assert_eq!(offset, 4096);
                assert_eq!(data, vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0xff]);
            }
            _ => panic!("expected Chunk after decode"),
        }
    }

    #[test]
    fn start_roundtrip() {
        let upload_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let msg = ImageUploadClientMsg::Start {
            upload_id,
            session_id,
            auth_token: "tok".into(),
            media_type: "image/png".into(),
            total_bytes: 12_345,
            file_path: Some("/tmp/x.png".into()),
        };
        let frame = msg.encode().unwrap();
        match frame {
            WsMessage::Text(ref text) => {
                assert!(text.contains(r#""type":"Start""#));
                assert!(text.contains(r#""media_type":"image/png""#));
            }
            _ => panic!("expected text frame"),
        }
        let decoded = ImageUploadClientMsg::decode(frame).unwrap();
        match decoded {
            ImageUploadClientMsg::Start {
                upload_id: u,
                media_type,
                total_bytes,
                file_path,
                ..
            } => {
                assert_eq!(u, upload_id);
                assert_eq!(media_type, "image/png");
                assert_eq!(total_bytes, 12_345);
                assert_eq!(file_path.as_deref(), Some("/tmp/x.png"));
            }
            _ => panic!("expected Start after decode"),
        }
    }

    #[test]
    fn complete_roundtrip() {
        let upload_id = Uuid::new_v4();
        let frame = ImageUploadClientMsg::Complete { upload_id }
            .encode()
            .unwrap();
        let decoded = ImageUploadClientMsg::decode(frame).unwrap();
        assert!(matches!(
            decoded,
            ImageUploadClientMsg::Complete { upload_id: u } if u == upload_id
        ));
    }

    #[test]
    fn server_msg_ack_json() {
        let msg = ImageUploadServerMsg::Ack {
            upload_id: Uuid::nil(),
            image_url: "/api/images/abc".into(),
        };
        let frame = msg.encode().unwrap();
        match frame {
            WsMessage::Text(text) => {
                assert!(text.contains(r#""type":"Ack""#));
                assert!(text.contains("/api/images/abc"));
            }
            _ => panic!("expected text frame"),
        }
    }

    #[test]
    fn short_binary_frame_errors() {
        let bad = WsMessage::Binary(vec![0; 10]);
        let err = ImageUploadClientMsg::decode(bad).unwrap_err();
        assert!(matches!(err, DecodeError::InvalidData(_)));
    }
}

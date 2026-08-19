//! Frames the **portal itself** authors into a session transcript.
//!
//! Everything else in the transcript is *foreign*: agent JSON forwarded
//! verbatim from the wire or replayed from the database. Foreign frames are
//! necessarily opaque — we can't type arbitrary agent output — so the renderer
//! falls back to a raw bubble when it doesn't recognize one. That fallback is
//! correct there and **wrong here**: a frame the portal wrote itself should
//! never be unrenderable.
//!
//! It was, though. `{"type":"error","message":…}` was produced at three sites
//! and understood at none, so every failed upload, every server-pushed
//! `ServerToClient::Error`, and every exhausted-reconnect notice rendered as
//! "Unrecognized Message". The wire types were not at fault — the frames were
//! typed right up until they were serialized into the transcript's
//! `content: String` and re-parsed by a consumer that had never been told the
//! shape existed.
//!
//! The rule this module exists to enforce: **one definition per local frame,
//! and one door in.** Producers construct a [`LocalFrame`]; the renderer parses
//! into the same three types. Neither side can drift, because there is nothing
//! to drift from.

use serde::{Deserialize, Serialize};

use crate::api::ErrorMessage;
use crate::PortalMessage;

/// An optimistic user echo, rendered before the agent's own user frame arrives.
///
/// Carries its `"type"` tag as a field — matching [`PortalMessage`] — rather
/// than relying on an enclosing serde tag. That is deliberate: an internally
/// tagged wrapper consumes `type` for its discriminant, so a nested struct
/// declaring `type` can never see it. That exact collision is why the first
/// attempt at rendering portal errors silently matched nothing.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UserFrame {
    #[serde(rename = "type")]
    pub message_type: String,
    pub content: String,
}

impl UserFrame {
    /// The invariant `"type"` tag value.
    pub const MESSAGE_TYPE: &'static str = "user";

    pub fn new(content: String) -> Self {
        Self {
            message_type: Self::MESSAGE_TYPE.to_string(),
            content,
        }
    }
}

/// The closed set of frames the portal writes into a transcript.
///
/// Deliberately **not** `#[serde(tag = "type")]`: each variant's payload
/// already carries its own tag field, and stacking a second one collides. The
/// enum exists to make the set closed and the serialization single-sourced, not
/// to add a layer of tagging.
#[derive(Debug, Clone)]
pub enum LocalFrame {
    Portal(PortalMessage),
    User(UserFrame),
    Error(ErrorMessage),
}

impl LocalFrame {
    /// Convenience for the overwhelmingly common case.
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(ErrorMessage::new(message.into()))
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User(UserFrame::new(content.into()))
    }

    /// The wire `type` tag this frame serializes with. Kept next to
    /// [`Self::to_json`] so the producer's tag and the consumer's dispatch are
    /// read off one list.
    pub fn message_type(&self) -> &'static str {
        match self {
            Self::Portal(_) => PortalMessage::MESSAGE_TYPE,
            Self::User(_) => UserFrame::MESSAGE_TYPE,
            Self::Error(_) => ERROR_MESSAGE_TYPE,
        }
    }

    /// Serialize for the transcript. The **only** supported way to put a
    /// portal-authored frame into a session's message buffer — hand-rolling
    /// `serde_json::to_string` at a call site is what let a shape ship that no
    /// renderer understood.
    pub fn to_json(&self) -> String {
        let serialized = match self {
            Self::Portal(msg) => serde_json::to_string(msg),
            Self::User(msg) => serde_json::to_string(msg),
            Self::Error(msg) => serde_json::to_string(msg),
        };
        serialized.unwrap_or_default()
    }
}

/// The `"type"` tag [`ErrorMessage`] serializes with.
pub const ERROR_MESSAGE_TYPE: &str = "error";

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the module exists for: every frame the portal can author
    /// serializes with a tag, and that tag is the one the renderer dispatches
    /// on. If a variant is added without a tag, this fails.
    #[test]
    fn every_local_frame_serializes_with_its_declared_tag() {
        let frames = [
            LocalFrame::Portal(PortalMessage::text("hi".into())),
            LocalFrame::user("hi"),
            LocalFrame::error("boom"),
        ];
        for frame in frames {
            let json = frame.to_json();
            let value: serde_json::Value =
                serde_json::from_str(&json).expect("local frames are valid json");
            assert_eq!(
                value.get("type").and_then(|t| t.as_str()),
                Some(frame.message_type()),
                "frame {frame:?} serialized without its declared tag: {json}"
            );
        }
    }

    /// A tag is only useful if the payload survives with it.
    #[test]
    fn payloads_round_trip() {
        let json = LocalFrame::error("disk on fire").to_json();
        let back: ErrorMessage = serde_json::from_str(&json).expect("round trips");
        assert_eq!(back.message, "disk on fire");
        assert_eq!(back.error_type, ERROR_MESSAGE_TYPE);

        let json = LocalFrame::user("hello").to_json();
        let back: UserFrame = serde_json::from_str(&json).expect("round trips");
        assert_eq!(back.content, "hello");
        assert_eq!(back.message_type, UserFrame::MESSAGE_TYPE);
    }
}

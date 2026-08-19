//! Thin frontend-only wrappers around the shared Claude Code wire types.
//!
//! Claude messages should parse through `shared::ClaudeOutput`, which re-exports
//! `claude-codes` types. The local shapes below exist only for Portal's
//! frontend-specific envelope and optimistic user messages synthesized before
//! the proxy echoes a typed Claude user frame.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct RenderedMessage {
    pub content: String,
    pub meta: Option<shared::PortalMeta>,
}

impl RenderedMessage {
    /// Wrap an already-serialized frame. Use this for **foreign** frames only
    /// — agent JSON off the wire or replayed from the database, which is opaque
    /// by nature. For a frame the portal itself authors, use [`Self::local`],
    /// which cannot produce a shape the renderer does not understand.
    pub fn new(content: String, meta: Option<shared::PortalMeta>) -> Self {
        Self { content, meta }
    }

    /// The single door for portal-authored frames.
    ///
    /// Everything the portal writes into a transcript goes through here, so the
    /// set of shapes the renderer must handle is exactly the set of
    /// [`shared::LocalFrame`] variants — closed, and enumerable by a test.
    /// Hand-rolling `serde_json::to_string` at a call site is what previously
    /// let `{"type":"error"}` ship from three sites with no renderer at all.
    pub fn local(frame: shared::LocalFrame, meta: Option<shared::PortalMeta>) -> Self {
        Self {
            content: frame.to_json(),
            meta,
        }
    }

    pub fn raw_iso(&self) -> Option<&str> {
        shared::created_at_iso(self.meta.as_ref())
    }

    pub fn delivery(&self) -> Option<&shared::DeliveryMeta> {
        self.meta.as_ref()?.delivery.as_ref()
    }

    pub fn source(&self) -> Option<&shared::MessageSource> {
        self.meta.as_ref()?.source()
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum ClaudeMessage {
    System(shared::SystemMessage),
    Assistant(shared::AssistantMessage),
    Result(shared::ResultMessage),
    User(shared::UserMessage),
    Error(shared::AnthropicError),
    Portal(shared::PortalMessage),
    RateLimitEvent(shared::RateLimitEvent),
    /// `/clear`. Worth its own variant rather than falling to `Unknown`: it is
    /// the visible seam between two conversations in one session, and it also
    /// marks where claude's conversation id rotates (see the render).
    ConversationReset(shared::ConversationResetMessage),
    /// A portal-generated error ([`shared::ErrorMessage`]) — a failed file
    /// upload, say. Distinct from [`Self::Error`], which is Anthropic's nested
    /// `{type:"error", error:{…}}` envelope: this one is flat, so it never
    /// matched `ClaudeOutput` and fell through to a raw `Unknown` bubble. The
    /// portal was rendering its own errors as unrecognized frames.
    LocalError(shared::ErrorMessage),
    OptimisticUser(shared::UserFrame),
    Unknown,
}

impl ClaudeMessage {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        if let Ok(output) = serde_json::from_str::<shared::ClaudeOutput>(json) {
            return Ok(match output {
                shared::ClaudeOutput::System(msg) => Self::System(msg),
                shared::ClaudeOutput::User(msg) => Self::User(msg),
                shared::ClaudeOutput::Assistant(msg) => Self::Assistant(msg),
                shared::ClaudeOutput::Result(msg) => Self::Result(msg),
                shared::ClaudeOutput::Error(msg) => Self::Error(msg),
                shared::ClaudeOutput::RateLimitEvent(msg) => Self::RateLimitEvent(msg),
                shared::ClaudeOutput::ConversationReset(msg) => Self::ConversationReset(msg),
                // Wildcard: control frames plus the 2.1.160 wire additions
                // (stream_event, tool_progress, transcript variants, …) that
                // have no dedicated renderer yet.
                _ => Self::Unknown,
            });
        }

        let value: serde_json::Value = serde_json::from_str(json)?;
        Ok(parse_local_frame(&value).unwrap_or(Self::Unknown))
    }
}

impl<'de> Deserialize<'de> for ClaudeMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Ok(output) = serde_json::from_value::<shared::ClaudeOutput>(value.clone()) {
            return Ok(match output {
                shared::ClaudeOutput::System(msg) => Self::System(msg),
                shared::ClaudeOutput::User(msg) => Self::User(msg),
                shared::ClaudeOutput::Assistant(msg) => Self::Assistant(msg),
                shared::ClaudeOutput::Result(msg) => Self::Result(msg),
                shared::ClaudeOutput::Error(msg) => Self::Error(msg),
                shared::ClaudeOutput::RateLimitEvent(msg) => Self::RateLimitEvent(msg),
                shared::ClaudeOutput::ConversationReset(msg) => Self::ConversationReset(msg),
                // Wildcard: control frames plus the 2.1.160 wire additions
                // (stream_event, tool_progress, transcript variants, …) that
                // have no dedicated renderer yet.
                _ => Self::Unknown,
            });
        }
        Ok(parse_local_frame(&value).unwrap_or(Self::Unknown))
    }
}

/// Parse a frame the **portal itself** authored, dispatching on its `"type"`
/// tag into the shared [`shared::LocalFrame`] vocabulary.
///
/// Dispatching on the tag rather than deserializing an internally-tagged
/// wrapper is what lets each payload keep its own `type` field: serde would
/// otherwise consume that key for the discriminant, leaving the nested struct
/// unable to see its own tag. Every arm here parses a type defined once in
/// `shared` — there is no frontend-local copy to drift from.
fn parse_local_frame(value: &serde_json::Value) -> Option<ClaudeMessage> {
    match value.get("type").and_then(|t| t.as_str())? {
        shared::PortalMessage::MESSAGE_TYPE => serde_json::from_value(value.clone())
            .ok()
            .map(ClaudeMessage::Portal),
        shared::UserFrame::MESSAGE_TYPE => serde_json::from_value(value.clone())
            .ok()
            .map(ClaudeMessage::OptimisticUser),
        shared::ERROR_MESSAGE_TYPE => serde_json::from_value(value.clone())
            .ok()
            .map(ClaudeMessage::LocalError),
        _ => None,
    }
}

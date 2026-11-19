//! Thin frontend-only wrappers around the shared Claude Code wire types.
//!
//! Claude messages should parse through `shared::ClaudeOutput`, which re-exports
//! `claude-codes` types. The local shapes below exist only for Portal's
//! frontend-specific envelope and optimistic user messages synthesized before
//! the proxy echoes a typed Claude user frame.

use super::grouping::FrameFacts;
use serde::{Deserialize, Deserializer, Serialize};
use std::cell::OnceCell;
use std::rc::Rc;

// Probe the tag without allocating large agent bodies or buffering an
// internally tagged enum. Serde recognizes escaped tags and arbitrary key order.
#[derive(Deserialize)]
struct NoticeFrameKind {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Clone)]
pub struct RenderedMessage {
    /// The wire frame. Private and never edited in place: the secret-request
    /// id and the parse cache below are derived from it, and the cache is
    /// shared with every clone. Build a new message through the constructors
    /// instead. Shared rather than owned, so the clones that grouping makes of
    /// every message on each regroup don't copy the body.
    content: Rc<str>,
    pub meta: Option<shared::PortalMeta>,
    secret_request_id: Option<uuid::Uuid>,
    /// Lazily parsed [`FrameFacts`], shared by every clone (including the
    /// copies held in message groups), so a frame is parsed once rather than
    /// by each grouping helper on every render. Keyed by the agent type it
    /// was parsed for, since that decides how a frame parses.
    facts: Rc<OnceCell<(shared::AgentType, Rc<FrameFacts>)>>,
}

/// Equality is the wire frame and its metadata; the facts cache is derived.
impl PartialEq for RenderedMessage {
    fn eq(&self, other: &Self) -> bool {
        self.content == other.content
            && self.meta == other.meta
            && self.secret_request_id == other.secret_request_id
    }
}

impl Eq for RenderedMessage {}

impl RenderedMessage {
    /// Wrap an already-serialized frame. Use this for **foreign** frames only
    /// — agent JSON off the wire or replayed from the database, which is opaque
    /// by nature. For a frame the portal itself authors, use [`Self::local`],
    /// which cannot produce a shape the renderer does not understand.
    pub fn new(content: String, meta: Option<shared::PortalMeta>) -> Self {
        // Cache the logical identity once, rather than reparsing every stored
        // message on each streamed event. Only an ID-only Portal marker counts.
        let secret_request_id = serde_json::from_str::<NoticeFrameKind>(&content)
            .ok()
            .filter(|frame| frame.kind == shared::PortalMessage::MESSAGE_TYPE)
            .and_then(|_| serde_json::from_str::<shared::PortalMessage>(&content).ok())
            .and_then(|message| match message.content.as_slice() {
                [shared::PortalContent::SecretAccessRequest { request_id }]
                | [shared::PortalContent::SecretAccessGroup { request_id }]
                | [shared::PortalContent::SecretDepositProposal { request_id }] => {
                    Some(*request_id)
                }
                _ => None,
            });
        Self {
            content: content.into(),
            meta,
            secret_request_id,
            facts: Rc::default(),
        }
    }

    /// Everything grouping derives from this frame, parsed once for
    /// `agent_type` and cached. A request for another agent type (only test
    /// and demo paths mix them) is computed without replacing the cache.
    pub(super) fn facts(&self, agent_type: shared::AgentType) -> Rc<FrameFacts> {
        match self.facts.get() {
            Some((cached, facts)) if *cached == agent_type => facts.clone(),
            Some(_) => Rc::new(FrameFacts::of(&self.content, agent_type)),
            None => {
                let facts = Rc::new(FrameFacts::of(&self.content, agent_type));
                let _ = self.facts.set((agent_type, facts.clone()));
                facts
            }
        }
    }

    /// The single door for portal-authored frames.
    ///
    /// Everything the portal writes into a transcript goes through here, so the
    /// set of shapes the renderer must handle is exactly the set of
    /// [`shared::LocalFrame`] variants — closed, and enumerable by a test.
    /// Hand-rolling `serde_json::to_string` at a call site is what previously
    /// let `{"type":"error"}` ship from three sites with no renderer at all.
    pub fn local(frame: shared::LocalFrame, meta: Option<shared::PortalMeta>) -> Self {
        Self::new(frame.to_json(), meta)
    }

    /// Whether this frame counts toward the live transcript's render budget
    /// (`thinking_tokens` markers and command-lifecycle frames don't), from
    /// the cached parse.
    pub fn counts_toward_render_limit(&self, agent_type: shared::AgentType) -> bool {
        self.facts(agent_type).counts_toward_render_limit
    }

    /// The wire frame this message renders.
    pub fn content(&self) -> &str {
        &self.content
    }

    pub(crate) fn secret_request_id(&self) -> Option<uuid::Uuid> {
        self.secret_request_id
    }

    pub fn raw_iso(&self) -> Option<&str> {
        shared::created_at_iso(self.meta.as_ref())
    }

    /// Display-only original clock, falling back to the server clock. Keep
    /// raw_iso as the server timestamp for keys, acknowledgements and replay.
    pub fn display_iso(&self) -> Option<&str> {
        shared::display_at_iso(self.meta.as_ref())
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
    /// Keep large SDK payloads off the stack of every rendered message.
    Assistant(Box<shared::AssistantMessage>),
    /// Keep growing SDK result diagnostics off the stack of every rendered message.
    Result(Box<shared::ResultMessage>),
    User(Box<shared::UserMessage>),
    Error(shared::AnthropicError),
    Portal(shared::PortalMessage),
    /// Keep growing rate-limit diagnostics off the stack of every rendered message.
    RateLimitEvent(Box<shared::RateLimitEvent>),
    /// Claude's delivery bookkeeping for an input carrying a client-supplied
    /// UUID. It is typed so it never falls through to the raw-JSON renderer,
    /// with scheduler starts rendered as wakeup markers. Ordinary queued,
    /// started and terminal states remain hidden command transport state.
    CommandLifecycle(shared::CommandLifecycleMessage),
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
}

impl ClaudeMessage {
    /// Strictly typed parse: `Ok` only for wire shapes this renderer actually
    /// owns. Anything else — future `ClaudeOutput` variants without a
    /// renderer, foreign agent frames, malformed JSON — is `Err`, so
    /// `AgentFrame::parse` falls through to the Codex/Muse parsers and finally
    /// the loud RawJson bubble. There is deliberately no silent catch-all
    /// (#1675): the old `Unknown` sentinel let a new wire shape vanish into a
    /// nondescript bubble with no signal that a renderer was missing.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        use serde::de::Error;
        if let Ok(output) = serde_json::from_str::<shared::ClaudeOutput>(json) {
            return Self::from_output(output)
                .ok_or_else(|| serde_json::Error::custom("ClaudeOutput variant has no renderer"));
        }

        let value: serde_json::Value = serde_json::from_str(json)?;
        Self::from_value(value)
            .ok_or_else(|| serde_json::Error::custom("not a claude or portal-local frame"))
    }

    /// Shared dispatch for a typed [`shared::ClaudeOutput`], used by both
    /// [`Self::parse`] and the [`serde::Deserialize`] impl so the two can
    /// never drift apart (a new wire variant added in one path only would
    /// render in some transcripts and raw-bubble in others).
    fn from_output(output: shared::ClaudeOutput) -> Option<Self> {
        match output {
            shared::ClaudeOutput::System(msg) => Some(Self::System(msg)),
            shared::ClaudeOutput::User(msg) => Some(Self::User(Box::new(msg))),
            shared::ClaudeOutput::Assistant(msg) => Some(Self::Assistant(Box::new(msg))),
            shared::ClaudeOutput::Result(msg) => Some(Self::Result(Box::new(msg))),
            shared::ClaudeOutput::Error(msg) => Some(Self::Error(msg)),
            shared::ClaudeOutput::RateLimitEvent(msg) => Some(Self::RateLimitEvent(Box::new(msg))),
            shared::ClaudeOutput::CommandLifecycle(msg) => Some(Self::CommandLifecycle(msg)),
            shared::ClaudeOutput::ConversationReset(msg) => Some(Self::ConversationReset(msg)),
            // Wildcard: control frames plus the 2.1.160 wire additions
            // (stream_event, tool_progress, transcript variants, …) that
            // have no dedicated renderer yet. `None` → the caller falls back
            // to the RawJson bubble, which is loud on purpose.
            _ => None,
        }
    }

    /// A non-`ClaudeOutput` frame is either portal-authored (a
    /// [`shared::LocalFrame`]) or foreign — the raw-bubble fallback reserved
    /// for agent shapes the renderer does not type yet.
    fn from_value(value: serde_json::Value) -> Option<Self> {
        parse_local_frame(&value)
    }
}

impl<'de> Deserialize<'de> for ClaudeMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Ok(output) = serde_json::from_value::<shared::ClaudeOutput>(value.clone()) {
            return Self::from_output(output)
                .ok_or_else(|| D::Error::custom("ClaudeOutput variant has no renderer"));
        }
        Self::from_value(value)
            .ok_or_else(|| D::Error::custom("not a claude or portal-local frame"))
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

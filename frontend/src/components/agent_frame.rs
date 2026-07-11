//! Normalized parse layer for agent transcript frames.
//!
//! This module is intentionally render-free. It captures the dispatch order
//! currently embedded in `message_renderer::MessageRenderer`: parse
//! protocol-agnostic Claude/Portal/User shapes first, then Codex-specific
//! shapes only for Codex sessions, then raw JSON. Later renderer-unification
//! work can switch on `AgentFrame` instead of reparsing in separate renderer
//! trees.

use super::codex_renderer::CodexEvent;
use super::message_renderer::types::ClaudeMessage;

#[derive(Debug, Clone)]
// SDK 2.1.160 grew `ClaudeMessage`'s largest payloads past clippy's variant
// gap threshold. `AgentFrame` is a transient per-render classification (never
// stored in bulk), so boxing would churn every construction/match site for no
// retained-memory win.
#[allow(clippy::large_enum_variant)]
pub enum AgentFrame {
    Claude(ClaudeMessage),
    Codex(CodexEvent),
    RawJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFrameKind {
    ClaudeSystem,
    ClaudeAssistant,
    ClaudeResult,
    ClaudeUser,
    ClaudeError,
    ClaudeRateLimitEvent,
    Portal,
    OptimisticUser,
    CodexThreadStarted,
    CodexTurnStarted,
    CodexTurnCompleted,
    CodexTurnFailed,
    CodexItemStarted,
    CodexItemUpdated,
    CodexItemCompleted,
    CodexError,
    CodexTurnDiffUpdated,
    CodexFileChangePatchUpdated,
    CodexTurnPlanUpdated,
    CodexPlanDelta,
    CodexReasoningSummaryPartAdded,
    CodexReasoningTextDelta,
    CodexThreadCompacted,
    RawJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRenderer {
    Claude,
    Codex,
    RawJson,
}

pub struct AgentFrameRegistry;

impl AgentFrameRegistry {
    pub fn parse(json: &str, agent_type: shared::AgentType) -> AgentFrame {
        AgentFrame::parse(json, agent_type)
    }

    pub fn renderer_for(frame: &AgentFrame) -> FrameRenderer {
        FrameRenderer::for_kind(frame.kind())
    }
}

impl AgentFrame {
    pub fn parse(json: &str, agent_type: shared::AgentType) -> Self {
        if let Ok(message) = ClaudeMessage::parse(json) {
            if !matches!(message, ClaudeMessage::Unknown) {
                return Self::Claude(message);
            }
        }

        if agent_type == shared::AgentType::Codex {
            if let Ok(event) = serde_json::from_str::<CodexEvent>(json) {
                if !matches!(event, CodexEvent::Unknown) {
                    return Self::Codex(event);
                }
            }
        }

        Self::RawJson
    }

    pub fn kind(&self) -> AgentFrameKind {
        match self {
            Self::Claude(message) => message.kind(),
            Self::Codex(event) => event.kind(),
            Self::RawJson => AgentFrameKind::RawJson,
        }
    }
}

impl FrameRenderer {
    fn for_kind(kind: AgentFrameKind) -> Self {
        match kind {
            AgentFrameKind::ClaudeSystem
            | AgentFrameKind::ClaudeAssistant
            | AgentFrameKind::ClaudeResult
            | AgentFrameKind::ClaudeUser
            | AgentFrameKind::ClaudeError
            | AgentFrameKind::ClaudeRateLimitEvent
            | AgentFrameKind::Portal
            | AgentFrameKind::OptimisticUser => Self::Claude,
            AgentFrameKind::CodexThreadStarted
            | AgentFrameKind::CodexTurnStarted
            | AgentFrameKind::CodexTurnCompleted
            | AgentFrameKind::CodexTurnFailed
            | AgentFrameKind::CodexItemStarted
            | AgentFrameKind::CodexItemUpdated
            | AgentFrameKind::CodexItemCompleted
            | AgentFrameKind::CodexError
            | AgentFrameKind::CodexTurnDiffUpdated
            | AgentFrameKind::CodexFileChangePatchUpdated
            | AgentFrameKind::CodexTurnPlanUpdated
            | AgentFrameKind::CodexPlanDelta
            | AgentFrameKind::CodexReasoningSummaryPartAdded
            | AgentFrameKind::CodexReasoningTextDelta
            | AgentFrameKind::CodexThreadCompacted => Self::Codex,
            AgentFrameKind::RawJson => Self::RawJson,
        }
    }
}

impl ClaudeMessage {
    fn kind(&self) -> AgentFrameKind {
        match self {
            Self::System(_) => AgentFrameKind::ClaudeSystem,
            Self::Assistant(_) => AgentFrameKind::ClaudeAssistant,
            Self::Result(_) => AgentFrameKind::ClaudeResult,
            Self::User(_) => AgentFrameKind::ClaudeUser,
            Self::Error(_) => AgentFrameKind::ClaudeError,
            Self::Portal(_) => AgentFrameKind::Portal,
            Self::RateLimitEvent(_) => AgentFrameKind::ClaudeRateLimitEvent,
            Self::OptimisticUser(_) => AgentFrameKind::OptimisticUser,
            Self::Unknown => AgentFrameKind::RawJson,
        }
    }
}

impl CodexEvent {
    fn kind(&self) -> AgentFrameKind {
        match self {
            Self::ThreadStarted { .. } => AgentFrameKind::CodexThreadStarted,
            Self::TurnStarted {} => AgentFrameKind::CodexTurnStarted,
            Self::TurnCompleted { .. } => AgentFrameKind::CodexTurnCompleted,
            Self::TurnFailed { .. } => AgentFrameKind::CodexTurnFailed,
            Self::ItemStarted { .. } => AgentFrameKind::CodexItemStarted,
            Self::ItemUpdated { .. } => AgentFrameKind::CodexItemUpdated,
            Self::ItemCompleted { .. } => AgentFrameKind::CodexItemCompleted,
            Self::Error { .. } => AgentFrameKind::CodexError,
            Self::TurnDiffUpdated { .. } => AgentFrameKind::CodexTurnDiffUpdated,
            Self::FileChangePatchUpdated { .. } => AgentFrameKind::CodexFileChangePatchUpdated,
            Self::TurnPlanUpdated { .. } => AgentFrameKind::CodexTurnPlanUpdated,
            Self::PlanDelta { .. } => AgentFrameKind::CodexPlanDelta,
            Self::ReasoningSummaryPartAdded { .. } => {
                AgentFrameKind::CodexReasoningSummaryPartAdded
            }
            Self::ReasoningTextDelta { .. } => AgentFrameKind::CodexReasoningTextDelta,
            Self::ThreadCompacted { .. } => AgentFrameKind::CodexThreadCompacted,
            Self::Unknown => AgentFrameKind::RawJson,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_for(agent_type: shared::AgentType, json: serde_json::Value) -> AgentFrame {
        AgentFrameRegistry::parse(&json.to_string(), agent_type)
    }

    #[test]
    fn parses_claude_assistant_frame_to_claude_renderer() {
        let frame = parse_for(
            shared::AgentType::Claude,
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": "msg_1",
                    "role": "assistant",
                    "model": "claude-sonnet-4-5-20250929",
                    "content": [{"type": "text", "text": "hello"}]
                },
                "session_id": "01890000-0000-7000-8000-000000000001"
            }),
        );

        assert_eq!(frame.kind(), AgentFrameKind::ClaudeAssistant);
        assert_eq!(
            AgentFrameRegistry::renderer_for(&frame),
            FrameRenderer::Claude
        );
    }

    #[test]
    fn parses_portal_frame_before_agent_specific_fallback() {
        let frame = parse_for(
            shared::AgentType::Codex,
            serde_json::json!({
                "type": "portal",
                "content": [{"type": "text", "text": "Connection restored"}]
            }),
        );

        assert_eq!(frame.kind(), AgentFrameKind::Portal);
        assert_eq!(
            AgentFrameRegistry::renderer_for(&frame),
            FrameRenderer::Claude
        );
    }

    #[test]
    fn parses_optimistic_user_frame_before_agent_specific_fallback() {
        let frame = parse_for(
            shared::AgentType::Codex,
            serde_json::json!({
                "type": "user",
                "content": "pending prompt",
                "_pending": true
            }),
        );

        assert_eq!(frame.kind(), AgentFrameKind::OptimisticUser);
        assert_eq!(
            AgentFrameRegistry::renderer_for(&frame),
            FrameRenderer::Claude
        );
    }

    #[test]
    fn parses_codex_turn_completed_only_for_codex_sessions() {
        let json = serde_json::json!({
            "type": "turn.completed",
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });

        let codex = parse_for(shared::AgentType::Codex, json.clone());
        assert_eq!(codex.kind(), AgentFrameKind::CodexTurnCompleted);
        assert_eq!(
            AgentFrameRegistry::renderer_for(&codex),
            FrameRenderer::Codex
        );

        let claude = parse_for(shared::AgentType::Claude, json);
        assert_eq!(claude.kind(), AgentFrameKind::RawJson);
        assert_eq!(
            AgentFrameRegistry::renderer_for(&claude),
            FrameRenderer::RawJson
        );
    }

    #[test]
    fn unknown_codex_event_stays_raw_json() {
        let frame = parse_for(
            shared::AgentType::Codex,
            serde_json::json!({
                "type": "future.event",
                "payload": {"value": 1}
            }),
        );

        assert_eq!(frame.kind(), AgentFrameKind::RawJson);
        assert_eq!(
            AgentFrameRegistry::renderer_for(&frame),
            FrameRenderer::RawJson
        );
    }
}

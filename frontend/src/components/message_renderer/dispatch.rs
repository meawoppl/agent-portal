use super::renderers;
use super::types::{ClaudeMessage, RenderedMessage};
use crate::components::agent_frame::{AgentFrame, AgentFrameRegistry, FrameRenderer};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;
use yew::prelude::*;

pub(crate) struct FrameRenderContext<'a> {
    pub message: &'a RenderedMessage,
    pub agent_type: shared::AgentType,
    pub session_id: Uuid,
    pub timestamp: Option<&'a str>,
    pub raw_iso: Option<&'a str>,
    pub current_user_id: Option<&'a str>,
    pub turn_metrics: Option<&'a shared::TurnMetrics>,
    pub continuation_statuses: &'a HashMap<Uuid, String>,
    pub on_schedule_continuation: Callback<Uuid>,
}

pub(crate) fn render_frame(ctx: FrameRenderContext<'_>) -> Html {
    if let Some(shared::MessageSource::Agent {
        session_id,
        agent_type,
    }) = ctx.message.source()
    {
        return renderers::render_agent_message_from_source(
            session_id,
            agent_type,
            &message_text(ctx.message),
            ctx.timestamp,
            ctx.session_id,
        );
    }

    let json = ctx.message.content.as_str();
    let frame = AgentFrameRegistry::parse(json, ctx.agent_type);

    // Dispatch on the message shape, not the agent. `User` (the proxy's
    // synthetic echo) and `Portal` (the backend's portal-content envelope)
    // are protocol-agnostic and must render the same way on Claude and
    // Codex sessions. Codex-specific shapes (`item.started`,
    // `turn.completed`, ...) fall through to the Codex renderer below.
    match AgentFrameRegistry::renderer_for(&frame) {
        FrameRenderer::Claude => match frame {
            AgentFrame::Claude(ClaudeMessage::System(msg)) => {
                renderers::render_system_message(&msg, ctx.timestamp)
            }
            AgentFrame::Claude(ClaudeMessage::Assistant(msg)) => {
                renderers::render_assistant_message(
                    &msg,
                    ctx.timestamp,
                    ctx.raw_iso,
                    ctx.session_id,
                )
            }
            AgentFrame::Claude(ClaudeMessage::Result(msg)) => {
                renderers::render_result_message(&msg, ctx.turn_metrics)
            }
            AgentFrame::Claude(ClaudeMessage::User(msg)) => renderers::render_user_message(
                &msg,
                ctx.message.meta.as_ref(),
                ctx.current_user_id,
                ctx.timestamp,
                ctx.session_id,
            ),
            AgentFrame::Claude(ClaudeMessage::OptimisticUser(msg)) => {
                renderers::render_optimistic_user_message(
                    &msg,
                    ctx.message.meta.as_ref(),
                    ctx.current_user_id,
                    ctx.timestamp,
                    ctx.session_id,
                )
            }
            AgentFrame::Claude(ClaudeMessage::Error(msg)) => {
                renderers::render_error_message(&msg, ctx.timestamp)
            }
            AgentFrame::Claude(ClaudeMessage::Portal(msg)) => renderers::render_portal_message(
                &msg,
                ctx.timestamp,
                ctx.session_id,
                ctx.continuation_statuses,
                ctx.on_schedule_continuation,
            ),
            AgentFrame::Claude(ClaudeMessage::RateLimitEvent(msg)) => {
                renderers::render_rate_limit_event(&msg, ctx.timestamp)
            }
            AgentFrame::Claude(ClaudeMessage::Unknown)
            | AgentFrame::Codex(_)
            | AgentFrame::RawJson => render_raw_json(json),
        },
        FrameRenderer::Codex => match frame {
            AgentFrame::Codex(event) => crate::components::codex_renderer::render_codex_frame(
                &event,
                ctx.session_id,
                ctx.turn_metrics,
            ),
            _ => html! {},
        },
        FrameRenderer::RawJson => render_raw_json(json),
    }
}

pub(crate) fn render_identity_group_part(
    message: &RenderedMessage,
    agent_type: shared::AgentType,
    session_id: Uuid,
    continuation_statuses: &HashMap<Uuid, String>,
    on_schedule_continuation: Callback<Uuid>,
) -> Html {
    if let Some(shared::MessageSource::Agent { .. }) = message.source() {
        return renderers::render_agent_message_body(&message_text(message), session_id);
    }

    let json = message.content.as_str();
    let frame = AgentFrameRegistry::parse(json, agent_type);
    match frame {
        AgentFrame::Claude(ClaudeMessage::User(msg)) => {
            renderers::render_user_message_content(&msg, session_id)
        }
        AgentFrame::Claude(ClaudeMessage::OptimisticUser(msg)) => {
            renderers::render_optimistic_user_message_content(&msg, session_id)
        }
        AgentFrame::Claude(ClaudeMessage::Assistant(msg)) => {
            renderers::render_assistant_message_content(&msg, session_id)
        }
        AgentFrame::Claude(ClaudeMessage::Portal(msg)) => renderers::render_portal_message_content(
            &msg,
            session_id,
            continuation_statuses,
            on_schedule_continuation,
        ),
        AgentFrame::Codex(event) => {
            crate::components::codex_renderer::render_codex_frame_content(&event, session_id)
        }
        _ => html! {},
    }
}

fn message_text(message: &RenderedMessage) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(&message.content) {
        match value {
            Value::String(text) => return text,
            Value::Object(_) => {
                if let Ok(portal) = serde_json::from_value::<super::types::PortalMessage>(value) {
                    return renderers::portal_text(&portal);
                }
            }
            _ => {}
        }
    }
    message.content.clone()
}

fn render_raw_json(json: &str) -> Html {
    let display = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| json.to_string());

    html! {
        <div class="claude-message raw-message">
            <div class="message-header">
                <span class="message-type-badge raw">{ "Unrecognized Message" }</span>
            </div>
            <div class="message-body">
                <pre class="raw-json">{ display }</pre>
                <p class="raw-message-hint">
                    { "This message type is not yet supported by the portal. " }
                    <a href="https://github.com/meawoppl/rust-code-agent-sdks/issues"
                       target="_blank" rel="noopener noreferrer">
                        { "Report this issue" }
                    </a>
                </p>
            </div>
        </div>
    }
}

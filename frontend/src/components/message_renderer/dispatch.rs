use super::renderers;
use super::types::{ClaudeMessage, RenderedMessage};
use crate::components::agent_frame::{AgentFrame, AgentFrameRegistry, FrameRenderer};
use crate::components::copy_button::CopyButton;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;
use yew::prelude::*;

pub(crate) struct FrameRenderContext<'a> {
    pub message: &'a RenderedMessage,
    pub agent_type: shared::AgentType,
    pub session_id: Uuid,
    pub timestamp: Option<&'a str>,
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
                renderers::render_assistant_message(&msg, ctx.timestamp, ctx.session_id)
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

/// Render one member of an identity group. `None` when the member produces no
/// visible content, so the caller omits the `grouped-message-part` wrapper
/// (and its flex gap) rather than emitting a spaced-but-empty row.
pub(crate) fn render_identity_group_part(
    message: &RenderedMessage,
    agent_type: shared::AgentType,
    session_id: Uuid,
    continuation_statuses: &HashMap<Uuid, String>,
    on_schedule_continuation: Callback<Uuid>,
) -> Option<Html> {
    if let Some(shared::MessageSource::Agent { .. }) = message.source() {
        return Some(renderers::render_agent_message_body(
            &message_text(message),
            session_id,
        ));
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
        AgentFrame::Claude(ClaudeMessage::Portal(msg)) => {
            Some(renderers::render_portal_message_content(
                &msg,
                session_id,
                continuation_statuses,
                on_schedule_continuation,
            ))
        }
        AgentFrame::Codex(event) => {
            crate::components::codex_renderer::render_codex_frame_content(&event, session_id)
        }
        _ => None,
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

    // Deep-link to a NEW issue prefilled with the frame, not the issues list —
    // the reporter clicks once and the raw JSON the SDK needs is already there.
    let report_href = report_issue_url(&display);

    html! {
        <div class="claude-message raw-message">
            <div class="message-header">
                <span class="message-type-badge raw">{ "Unrecognized Message" }</span>
                <CopyButton text={display.clone()} title="Copy raw JSON" />
            </div>
            <div class="message-body">
                <pre class="raw-json">{ display }</pre>
                <p class="raw-message-hint">
                    { "This message type is not yet supported by the portal. " }
                    <a href={report_href}
                       target="_blank" rel="noopener noreferrer">
                        { "Report this issue" }
                    </a>
                    { " (opens a pre-filled issue with this frame)." }
                </p>
            </div>
        </div>
    }
}

/// Base repo for unrecognized-frame reports: the SDK repo, since an unrendered
/// frame is a typed-model gap there, not a portal bug.
const REPORT_REPO: &str = "https://github.com/meawoppl/rust-code-agent-sdks";

/// Largest raw-JSON snippet to inline in the issue body. GitHub 414s on
/// over-long URLs and percent-encoding a JSON blob roughly triples its size, so
/// we cap the pre-fill well short of the ~8k URL limit; the card's Copy button
/// carries the full frame for anything larger.
const MAX_REPORT_JSON: usize = 2000;

/// Build a `issues/new?title=…&body=…` URL pre-filled with an unrecognized
/// frame: its `payload_type`/`type` in the title, the raw JSON (fenced) in the
/// body. Pure + host-testable (hand-rolled encoder, no `js_sys`).
fn report_issue_url(pretty_json: &str) -> String {
    let frame_type = serde_json::from_str::<Value>(pretty_json)
        .ok()
        .and_then(|v| {
            v.get("payload_type")
                .or_else(|| v.get("type"))
                .and_then(|t| t.as_str())
                .map(str::to_string)
        });
    let title = match &frame_type {
        Some(t) => format!("Unrecognized frame: {t}"),
        None => "Unrecognized frame".to_string(),
    };

    let (snippet, truncated) = if pretty_json.chars().count() > MAX_REPORT_JSON {
        (
            pretty_json
                .chars()
                .take(MAX_REPORT_JSON)
                .collect::<String>(),
            true,
        )
    } else {
        (pretty_json.to_string(), false)
    };

    let mut body =
        String::from("The portal received an agent frame it doesn't render yet.\n\n```json\n");
    body.push_str(&snippet);
    body.push_str("\n```\n");
    if truncated {
        body.push_str("\n_(truncated — use the card's Copy button for the full frame)_\n");
    }
    body.push_str(&format!("\n---\nagent-portal {}\n", crate::VERSION));

    format!(
        "{REPORT_REPO}/issues/new?title={}&body={}",
        percent_encode(&title),
        percent_encode(&body),
    )
}

/// Percent-encode a query-parameter value (RFC 3986 unreserved set kept raw,
/// everything else `%XX`). Mirrors the encoder in `markdown::sanitizer` — kept
/// local and pure so `report_issue_url` is testable without a JS runtime.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_url_titles_from_payload_type() {
        let url = report_issue_url(r#"{"type":"muse_record","payload_type":"task.lifecycle.foo"}"#);
        // payload_type wins over type, and spaces/colons are encoded.
        assert!(url.contains("title=Unrecognized%20frame%3A%20task.lifecycle.foo"));
        assert!(url.starts_with(REPORT_REPO));
        assert!(url.contains("/issues/new?"));
    }

    #[test]
    fn report_url_falls_back_to_type_then_generic() {
        assert!(report_issue_url(r#"{"type":"weird_frame"}"#)
            .contains("title=Unrecognized%20frame%3A%20weird_frame"));
        // Non-JSON → generic title, no panic.
        assert!(report_issue_url("not json").contains("title=Unrecognized%20frame&"));
    }

    #[test]
    fn report_url_body_carries_fenced_json() {
        let url = report_issue_url(r#"{"type":"x"}"#);
        // The ```json fence is percent-encoded (backtick = %60, newline = %0A).
        assert!(url.contains("body="));
        assert!(url.contains("%60%60%60json"));
    }

    #[test]
    fn report_url_truncates_oversized_frames() {
        let big = format!("{{\"blob\":\"{}\"}}", "x".repeat(MAX_REPORT_JSON + 500));
        let url = report_issue_url(&big);
        // Truncation note present (percent-encoded "truncated").
        assert!(url.contains("truncated"));
        // And the URL stays bounded despite a huge input.
        assert!(url.len() < MAX_REPORT_JSON * 4);
    }
}

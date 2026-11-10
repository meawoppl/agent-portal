//! User-message renderers: the real Claude `user` wire shape, the synthetic
//! optimistic echo, and the delivery-progress chip that rides on both.

use super::{
    agent_message_event_from_agent_facing_text, render_agent_message_event, render_content_blocks,
};
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown_for_session;
use crate::components::tool_renderers::{
    has_askuserquestion_answers, render_askuserquestion_result,
};
use crate::components::{split_upload_notice, uploaded_image_media_type, UploadNotice};
use crate::utils;
use shared::UserFrame as OptimisticUserMessage;
use uuid::Uuid;
use yew::prelude::*;

/// Convert single newlines to markdown hard breaks (trailing two spaces)
/// so that user-typed line breaks are preserved when rendered as markdown.
fn preserve_user_newlines(text: &str) -> String {
    text.replace('\n', "  \n")
}

/// Extract the joined text content and tool-result presence from a user
/// message's content blocks. Shared by [`render_user_message`] and
/// [`render_user_message_content`].
fn user_message_text_and_tool_results(msg: &shared::UserMessage) -> (String, bool) {
    let blocks = &msg.message.content;

    let text_content: String = blocks
        .iter()
        .filter_map(|block| match block {
            shared::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let has_tool_results = blocks
        .iter()
        .any(|b| matches!(b, shared::ContentBlock::ToolResult(_)));

    (text_content, has_tool_results)
}

pub fn render_optimistic_user_message(
    msg: &OptimisticUserMessage,
    meta: Option<&shared::PortalMeta>,
    current_user_id: Option<&str>,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    // A synthetic echo carrying an inter-agent `[message from …]` payload (e.g.
    // Codex's `UserEchoEvent`, or an older proxy's raw echo) renders as its own
    // provenance card rather than a raw "You" bubble — mirroring
    // `render_user_message`.
    if let Some(event) = agent_message_event_from_agent_facing_text(&msg.content) {
        return render_agent_message_event(&event, timestamp, session_id);
    }

    let label = human_label(meta, current_user_id);
    let delivery = meta.and_then(|m| m.delivery.as_ref());
    let pending_class = if delivery.is_some_and(shared::DeliveryMeta::pending) {
        " pending"
    } else {
        ""
    };

    html! {
        <div class={format!("claude-message user-message{}", pending_class)}>
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge user">{ &label }</span>
                if delivery.is_some_and(shared::DeliveryMeta::pending) {
                    <span class="pending-indicator" title="Sending...">{ "\u{2022}" }</span>
                }
                { render_delivery_progress(delivery) }
                <CopyButton text={msg.content.clone()} title="Copy message" />
            </div>
            <div class="message-body">{ render_optimistic_user_message_content(msg, session_id).unwrap_or_default() }</div>
        </div>
    }
}

pub fn render_user_message(
    msg: &shared::UserMessage,
    meta: Option<&shared::PortalMeta>,
    current_user_id: Option<&str>,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    let label = if msg.text_not_typed == Some(true) {
        "Automated".to_string()
    } else {
        human_label(meta, current_user_id)
    };
    let delivery = meta.and_then(|m| m.delivery.as_ref());
    let pending_class = if delivery.is_some_and(shared::DeliveryMeta::pending) {
        " pending"
    } else {
        ""
    };
    let (text_content, has_tool_results) = user_message_text_and_tool_results(msg);

    if has_tool_results {
        html! {
            <div class="claude-message user-message tool-result-message">
                <div class="message-body">{ render_user_message_content(msg, session_id).unwrap_or_default() }</div>
            </div>
        }
    } else if !text_content.is_empty() {
        if let Some(event) = agent_message_event_from_agent_facing_text(&text_content) {
            return render_agent_message_event(&event, timestamp, session_id);
        }

        html! {
            <div class={format!("claude-message user-message{}", pending_class)}>
                <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                    <span class="message-type-badge user">{ &label }</span>
                    if delivery.is_some_and(shared::DeliveryMeta::pending) {
                        <span class="pending-indicator" title="Sending...">{ "\u{2022}" }</span>
                    }
                    { render_delivery_progress(delivery) }
                    <CopyButton text={text_content.clone()} title="Copy message" />
                </div>
                <div class="message-body">{ render_user_message_content(msg, session_id).unwrap_or_default() }</div>
            </div>
        }
    } else {
        html! {}
    }
}

pub fn render_optimistic_user_message_content(
    msg: &OptimisticUserMessage,
    session_id: Uuid,
) -> Option<Html> {
    if !utils::is_non_blank(&msg.content) {
        return None;
    }
    if let Some(notice) = split_upload_notice(&msg.content) {
        return Some(render_upload_notice(&notice, session_id));
    }
    Some(html! {
        <div class="user-text">{ render_markdown_for_session(&preserve_user_newlines(&msg.content), session_id) }</div>
    })
}

/// Render the upload pipeline's agent-facing notice as the user's own words
/// plus compact attachment chips. The wire text (which the agent needs — the
/// files just appeared in its working directory) is untouched; only the
/// transcript stops impersonating the user with machine boilerplate.
fn render_upload_notice(notice: &UploadNotice<'_>, session_id: Uuid) -> Html {
    html! {
        <>
            if let Some(text) = notice.user_text {
                <div class="user-text">{ render_markdown_for_session(&preserve_user_newlines(text), session_id) }</div>
            }
            <div class="upload-notice">
                { for notice.files.iter().map(|(name, size)| html! {
                    <div class="upload-attachment">
                        { render_upload_preview(name, session_id) }
                        <span class="upload-chip">
                            <span aria-hidden="true">{ "📎" }</span>
                            <span class="upload-chip-name">{ *name }</span>
                            <span class="upload-chip-size">{ *size }</span>
                        </span>
                    </div>
                }) }
            </div>
        </>
    }
}

/// Inline preview of an uploaded image, pulled back from the session's working
/// directory (where the upload landed). Anything that isn't an image, or whose
/// file can no longer be fetched (agent offline, file moved), shows no preview
/// and the chip alone.
fn render_upload_preview(name: &str, session_id: Uuid) -> Html {
    let Some(media_type) = uploaded_image_media_type(name) else {
        return html! {};
    };
    let src = crate::components::markdown::session_file_pull_url(session_id, name);
    html! {
        <div class="upload-preview">
            { super::media::render_image_url(src, media_type, Some(name.to_string()), true) }
        </div>
    }
}

/// Render a user message's body, returning `None` when it produces nothing
/// (empty text, or a tool-result envelope whose results are all empty) so
/// callers can drop the surrounding wrapper/card rather than emit a blank one.
pub fn render_user_message_content(msg: &shared::UserMessage, session_id: Uuid) -> Option<Html> {
    if let Some(Ok(input)) = msg.tool_use_result_as::<shared::AskUserQuestionInput>() {
        if has_askuserquestion_answers(&input) {
            let approvals = msg.message.content.iter().filter_map(|block| match block {
                shared::ContentBlock::ToolResult(result) => {
                    render_tool_result_permission(msg, &result.tool_use_id)
                }
                _ => None,
            });
            return Some(html! { <>{ for approvals }{ render_askuserquestion_result(&input) }</> });
        }
    }

    let (text_content, has_tool_results) = user_message_text_and_tool_results(msg);

    if has_tool_results {
        let rendered: Vec<_> = msg.message.content.iter().filter_map(|block| {
            let body = render_content_blocks(std::slice::from_ref(block), session_id);
            let permission = match block {
                shared::ContentBlock::ToolResult(result) => render_tool_result_permission(msg, &result.tool_use_id),
                _ => None,
            };
            if body.is_none() && permission.is_none() { return None; }
            Some(html! { <div class="tool-result-with-permission">{ permission.unwrap_or_default() }{ body.unwrap_or_default() }</div> })
        }).collect();
        (!rendered.is_empty()).then(|| html! { <>{ for rendered }</> })
    } else if text_content.is_empty() {
        None
    } else if let Some(notice) = split_upload_notice(&text_content) {
        Some(render_upload_notice(&notice, session_id))
    } else {
        Some(html! {
            <div class="user-text">{ render_markdown_for_session(&preserve_user_newlines(&text_content), session_id) }</div>
        })
    }
}

fn permission_decision_label(decision: &claude_codes::ToolPermissionDecision) -> String {
    use claude_codes::PermissionDecisionOutcome;
    let action = match &decision.decision {
        PermissionDecisionOutcome::Accept => "Allowed",
        PermissionDecisionOutcome::Reject => "Denied",
        PermissionDecisionOutcome::Cancelled => return "Permission cancelled".into(),
        PermissionDecisionOutcome::Unknown(kind) => return format!("Permission: {kind}"),
    };
    let reason = decision
        .reason_type
        .as_ref()
        .and_then(|reason| match reason {
            claude_codes::io::PermissionDecisionReasonType::Other
            | claude_codes::io::PermissionDecisionReasonType::Unknown(_) => None,
            known => Some(known.as_str()),
        });
    let origin = match decision.source.as_str() {
        "hook" | "classifier" | "user_temporary" | "user_permanent" | "user_reject"
        | "user_abort" => decision.source.as_str(),
        source => reason.unwrap_or(source),
    };
    if matches!(decision.decision, PermissionDecisionOutcome::Accept) && origin == "classifier" {
        return "Auto-approved (classifier)".into();
    }
    let origin = match origin {
        "user_temporary" | "user_permanent" | "user_reject" | "user_abort" => "user".to_string(),
        "config" => "configuration".to_string(),
        other => other.replace('_', " "),
    };
    format!("{action} by {origin}")
}

fn render_tool_result_permission(msg: &shared::UserMessage, tool_use_id: &str) -> Option<Html> {
    // Join by id, never by array position: one user wrapper may answer
    // several calls and metadata can be partial or arrive in another order.
    let metadata = msg
        .tool_result_meta
        .as_deref()?
        .iter()
        .find(|meta| meta.id == tool_use_id)?;
    let decision = metadata.permission_decision.as_ref();
    if decision.is_none() && metadata.non_execution_kind.is_none() {
        return None;
    }
    Some(html! {
        <div class="tool-permission-details">
            if let Some(decision) = decision {
                <span class="tool-permission-chip" title={format!("Permission outcome: {}; source: {}; reason: {}. This describes approval, not whether execution completed.", decision.decision, decision.source, decision.reason_type.as_ref().map(|r| r.as_str()).unwrap_or("not reported"))}>
                    { permission_decision_label(decision) }
                </span>
            }
            if let Some(reason) = &metadata.non_execution_kind {
                <span class="tool-permission-chip">{ format!("Not executed: {}", reason.replace('_', " ")) }</span>
            }
        </div>
    })
}

fn render_delivery_progress(delivery: Option<&shared::DeliveryMeta>) -> Html {
    let Some(delivery) = delivery else {
        return html! {};
    };
    let stage = delivery.stage;
    let message = delivery.message.as_deref();

    let active_index = match stage {
        None => 0,
        Some(shared::InputDeliveryStage::ServerReceived) => 1,
        Some(shared::InputDeliveryStage::ProxyReceived) => 2,
        Some(shared::InputDeliveryStage::AgentAccepted) => 3,
        Some(shared::InputDeliveryStage::Failed) => 0,
    };
    let failed = stage == Some(shared::InputDeliveryStage::Failed);
    let queued = delivery.queued && stage == Some(shared::InputDeliveryStage::ServerReceived);
    let title = match (stage, message) {
        (None, _) => "Sent from browser",
        _ if queued => "Queued: the agent is offline; it will be sent when the agent reconnects",
        (Some(shared::InputDeliveryStage::ServerReceived), _) => "Received by server",
        (Some(shared::InputDeliveryStage::ProxyReceived), _) => "At local proxy",
        (Some(shared::InputDeliveryStage::AgentAccepted), _) => "In agent stream",
        (Some(shared::InputDeliveryStage::Failed), Some(msg)) => msg,
        (Some(shared::InputDeliveryStage::Failed), None) => "Delivery failed",
    };
    let label = match stage {
        _ if queued => "queued",
        None => "sent",
        Some(shared::InputDeliveryStage::ServerReceived) => "server",
        Some(shared::InputDeliveryStage::ProxyReceived) => "proxy",
        Some(shared::InputDeliveryStage::AgentAccepted) => "stream",
        Some(shared::InputDeliveryStage::Failed) => "failed",
    };
    let steps = ["sent", "server", "proxy", "stream"];

    html! {
        <span
            class={classes!("input-delivery-progress", failed.then_some("failed"))}
            title={title.to_string()}
            aria-label={format!("Input delivery: {label}")}
        >
            <span class="input-delivery-label">{ label }</span>
            <span class="input-delivery-steps" aria-hidden="true">
                { for steps.iter().enumerate().map(|(idx, step)| {
                    html! {
                        <span
                            class={classes!(
                                "input-delivery-step",
                                (!failed && idx <= active_index).then_some("active"),
                                (failed && idx == 0).then_some("failed"),
                            )}
                            title={*step}
                        />
                    }
                }) }
            </span>
        </span>
    }
}

fn human_label(meta: Option<&shared::PortalMeta>, current_user_id: Option<&str>) -> String {
    match meta.and_then(|m| m.source.as_ref()) {
        Some(shared::MessageSource::Human { account_id, name })
            if current_user_id != Some(account_id.to_string().as_str()) =>
        {
            name.clone()
        }
        _ => "You".to_string(),
    }
}

#[cfg(test)]
mod transcript_metadata_tests {
    use super::*;

    #[test]
    fn permission_chips_join_results_by_id_and_render_without_tool_output() {
        let mut message: shared::UserMessage = serde_json::from_str(r#"{"session_id":"session","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-a"}]},"tool_result_meta":[{"id":"tool-b","permission_decision":{"decision":"accept","source":"classifier","reason_type":"classifier"}},{"id":"tool-a","permission_decision":{"decision":"reject","source":"hook","reason_type":"hook"}}]}"#).unwrap();
        let text =
            super::super::visible_text(&render_tool_result_permission(&message, "tool-a").unwrap());
        assert!(text.contains("Denied by hook"));
        assert!(!text.contains("Auto-approved"));
        assert!(render_user_message_content(&message, Uuid::nil()).is_some());
        assert!(render_tool_result_permission(&message, "missing").is_none());
        message.tool_result_meta = None;
        assert!(render_user_message_content(&message, Uuid::nil()).is_none());
    }

    #[test]
    fn approval_labels_preserve_outcome_and_dont_claim_execution() {
        for (outcome, source, reason, expected) in [
            (
                "accept",
                "config",
                "classifier",
                "Auto-approved (classifier)",
            ),
            ("accept", "config", "rule", "Allowed by rule"),
            ("reject", "config", "classifier", "Denied by classifier"),
            ("cancelled", "user_abort", "other", "Permission cancelled"),
            ("future", "future", "future", "Permission: future"),
        ] {
            let json = format!(
                r#"{{"decision":"{outcome}","source":"{source}","reason_type":"{reason}"}}"#
            );
            let decision = serde_json::from_str(&json).unwrap();
            assert_eq!(permission_decision_label(&decision), expected);
        }
    }
}

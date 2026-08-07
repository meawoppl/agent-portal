//! User-message renderers: the real Claude `user` wire shape, the synthetic
//! optimistic echo, and the delivery-progress chip that rides on both.

use super::super::types::OptimisticUserMessage;
use super::{
    agent_message_event_from_agent_facing_text, render_agent_message_event, render_content_blocks,
};
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown_for_session;
use crate::components::tool_renderers::{
    has_askuserquestion_answers, render_askuserquestion_result,
};
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
    let label = human_label(meta, current_user_id);
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
    (!msg.content.trim().is_empty()).then(|| {
        html! {
            <div class="user-text">{ render_markdown_for_session(&preserve_user_newlines(&msg.content), session_id) }</div>
        }
    })
}

/// Render a user message's body, returning `None` when it produces nothing
/// (empty text, or a tool-result envelope whose results are all empty) so
/// callers can drop the surrounding wrapper/card rather than emit a blank one.
pub fn render_user_message_content(msg: &shared::UserMessage, session_id: Uuid) -> Option<Html> {
    if let Some(Ok(input)) = msg.tool_use_result_as::<shared::AskUserQuestionInput>() {
        if has_askuserquestion_answers(&input) {
            return Some(render_askuserquestion_result(&input));
        }
    }

    let (text_content, has_tool_results) = user_message_text_and_tool_results(msg);

    if has_tool_results {
        render_content_blocks(&msg.message.content, session_id)
    } else if text_content.is_empty() {
        None
    } else {
        Some(html! {
            <div class="user-text">{ render_markdown_for_session(&preserve_user_newlines(&text_content), session_id) }</div>
        })
    }
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
    let title = match (stage, message) {
        (None, _) => "Sent from browser",
        (Some(shared::InputDeliveryStage::ServerReceived), _) => "Received by server",
        (Some(shared::InputDeliveryStage::ProxyReceived), _) => "At local proxy",
        (Some(shared::InputDeliveryStage::AgentAccepted), _) => "In agent stream",
        (Some(shared::InputDeliveryStage::Failed), Some(msg)) => msg,
        (Some(shared::InputDeliveryStage::Failed), None) => "Delivery failed",
    };
    let label = match stage {
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

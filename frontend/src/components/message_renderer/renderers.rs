//! Rendering functions for each message type.

use super::types::*;
use super::{extract_raw_iso, format_duration, shorten_model_name};
use crate::components::copy_button::CopyButton;
use crate::components::expandable::ExpandableText;
use crate::components::markdown::render_markdown;
use crate::components::time_ago::TimeAgo;
use crate::components::tool_renderers::render_tool_use;
use serde::Deserialize;
use serde_json::Value;
use shared::{Citation, ToolResultContent};
use wasm_bindgen::JsCast;
use yew::prelude::*;

/// Convert single newlines to markdown hard breaks (trailing two spaces)
/// so that user-typed line breaks are preserved when rendered as markdown.
fn preserve_user_newlines(text: &str) -> String {
    text.replace('\n', "  \n")
}

fn extract_ephemeral_cache(usage: &UsageInfo) -> (u64, u64) {
    usage
        .cache_creation
        .as_ref()
        .map(|cc| {
            (
                u64::from(cc.ephemeral_1h_input_tokens),
                u64::from(cc.ephemeral_5m_input_tokens),
            )
        })
        .unwrap_or((0, 0))
}

fn build_model_tooltip(model: &str, usage: Option<&UsageInfo>) -> String {
    let mut parts = vec![model.to_string()];
    if let Some(u) = usage {
        if let Some(tier) = &u.service_tier {
            parts.push(tier.clone());
        }
        if let Some(geo) = &u.inference_geo {
            parts.push(geo.clone());
        }
    }
    parts.join(" | ")
}

fn build_usage_tooltip(usage: Option<&UsageInfo>) -> String {
    usage
        .map(|u| {
            let mut tooltip = format!(
                "Input: {} | Output: {} | Cache read: {} | Cache created: {}",
                u.input_tokens.unwrap_or(0),
                u.output_tokens.unwrap_or(0),
                u.cache_read_input_tokens.unwrap_or(0),
                u.cache_creation_input_tokens.unwrap_or(0)
            );
            let (e1h, e5m) = extract_ephemeral_cache(u);
            if e1h > 0 || e5m > 0 {
                tooltip.push_str(&format!(" | Ephemeral 1h: {} | Ephemeral 5m: {}", e1h, e5m));
            }
            tooltip
        })
        .unwrap_or_default()
}

/// Extract concatenated raw text from a list of content blocks.
/// Used for the message header copy button — pulls out text and thinking
/// blocks as markdown, ignoring tool_use/tool_result internals.
fn content_blocks_to_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(text);
            }
            ContentBlock::Thinking { thinking } => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str("<thinking>\n");
                out.push_str(thinking);
                out.push_str("\n</thinking>");
            }
            _ => {}
        }
    }
    out
}

/// Extract raw text from an assistant group's serialized JSON messages.
fn extract_assistant_group_text(messages: &[String]) -> String {
    let mut out = String::new();
    for json in messages {
        if let Ok(ClaudeMessage::Assistant(msg)) = serde_json::from_str::<ClaudeMessage>(json) {
            if let Some(content) = msg.message.and_then(|m| m.content) {
                let text = content_blocks_to_text(&content);
                if !text.is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(&text);
                }
            }
        }
    }
    out
}

// --- Message renderers ---

pub fn render_assistant_group(messages: &[String], timestamp: Option<&str>) -> Html {
    let mut total_output_tokens: u64 = 0;
    let mut total_input_tokens: u64 = 0;
    let mut total_cache_read: u64 = 0;
    let mut total_cache_created: u64 = 0;
    let mut total_ephemeral_1h: u64 = 0;
    let mut total_ephemeral_5m: u64 = 0;
    let mut model_name = String::new();
    let mut first_usage: Option<UsageInfo> = None;

    for json in messages {
        if let Ok(ClaudeMessage::Assistant(msg)) = serde_json::from_str::<ClaudeMessage>(json) {
            if let Some(message) = &msg.message {
                if let Some(usage) = &message.usage {
                    total_output_tokens += usage.output_tokens.unwrap_or(0);
                    total_input_tokens += usage.input_tokens.unwrap_or(0);
                    total_cache_read += usage.cache_read_input_tokens.unwrap_or(0);
                    total_cache_created += usage.cache_creation_input_tokens.unwrap_or(0);
                    let (e1h, e5m) = extract_ephemeral_cache(usage);
                    total_ephemeral_1h += e1h;
                    total_ephemeral_5m += e5m;
                    if first_usage.is_none() {
                        first_usage = Some(usage.clone());
                    }
                }
                if model_name.is_empty() {
                    if let Some(m) = &message.model {
                        model_name = m.clone();
                    }
                }
            }
        }
    }

    let count = messages.len();
    let mut usage_tooltip = format!(
        "Input: {} | Output: {} | Cache read: {} | Cache created: {} | {} messages",
        total_input_tokens, total_output_tokens, total_cache_read, total_cache_created, count
    );
    if total_ephemeral_1h > 0 || total_ephemeral_5m > 0 {
        usage_tooltip.push_str(&format!(
            " | Ephemeral 1h: {} | Ephemeral 5m: {}",
            total_ephemeral_1h, total_ephemeral_5m
        ));
    }
    let model_tooltip = build_model_tooltip(&model_name, first_usage.as_ref());
    let copy_text = extract_assistant_group_text(messages);
    // ISO timestamp of the last message in the group, for the live "time ago" label
    let last_iso = messages.last().and_then(|json| extract_raw_iso(json));

    html! {
        <div class="claude-message assistant-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge assistant">{ "Assistant" }</span>
                {
                    if count > 1 {
                        html! { <span class="message-count" title={format!("{} consecutive messages", count)}>{ format!("{} messages", count) }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(short_name) = shorten_model_name(&model_name) {
                        html! { <span class="model-name" title={model_tooltip.clone()}>{ short_name }</span> }
                    } else {
                        html! {}
                    }
                }
                if !copy_text.is_empty() {
                    <CopyButton text={copy_text} title="Copy assistant text" />
                }
                {
                    if total_input_tokens > 0 || total_output_tokens > 0 {
                        html! {
                            <span class="usage-badge" title={usage_tooltip}>
                                <span class="token-count">{ format!("{}↓ {}↑", total_input_tokens, total_output_tokens) }</span>
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                { for messages.iter().enumerate().map(|(i, json)| {
                    // Key by the message's `_created_at` when available, so
                    // streaming/insert reordering doesn't reset expandable
                    // state inside the body. Fall back to positional index
                    // for messages predating the `_created_at` field.
                    let key = extract_raw_iso(json)
                        .map(|iso| format!("m-{}", iso))
                        .unwrap_or_else(|| format!("m{}", i));
                    html! { <GroupedMessageContent {key} json={json.clone()} /> }
                })}
            </div>
            if let Some(iso) = last_iso {
                <div class="message-footer">
                    <TimeAgo iso={iso} />
                </div>
            }
        </div>
    }
}

/// Renders the content blocks for a single message within an assistant group.
/// Each message is its own component so Yew preserves it across re-renders
/// when new messages are appended to the group.
#[derive(Properties, PartialEq)]
struct GroupedMessageContentProps {
    json: String,
}

#[function_component(GroupedMessageContent)]
fn grouped_message_content(props: &GroupedMessageContentProps) -> Html {
    let blocks = match serde_json::from_str::<ClaudeMessage>(&props.json) {
        Ok(ClaudeMessage::Assistant(msg)) => {
            msg.message.and_then(|m| m.content).unwrap_or_default()
        }
        Ok(ClaudeMessage::User(msg)) => msg.message.and_then(|m| m.content).unwrap_or_default(),
        _ => return html! {},
    };
    render_content_blocks(&blocks)
}

pub fn render_user_message(
    msg: &UserMessage,
    current_user_id: Option<&str>,
    timestamp: Option<&str>,
) -> Html {
    let label = match &msg.sender {
        Some(sender) if current_user_id != Some(sender.user_id.as_str()) => sender.name.clone(),
        _ => "You".to_string(),
    };
    let pending_class = if msg.pending { " pending" } else { "" };

    if let Some(text) = &msg.content {
        html! {
            <div class={format!("claude-message user-message{}", pending_class)}>
                <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                    <span class="message-type-badge user">{ &label }</span>
                    if msg.pending {
                        <span class="pending-indicator" title="Sending...">{ "\u{2022}" }</span>
                    }
                    <CopyButton text={text.clone()} title="Copy message" />
                </div>
                <div class="message-body">
                    <div class="user-text">{ render_markdown(&preserve_user_newlines(text)) }</div>
                </div>
            </div>
        }
    } else if let Some(message) = &msg.message {
        let blocks = message.content.as_ref().cloned().unwrap_or_default();

        let text_content: String = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let has_tool_results = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));

        if has_tool_results {
            html! {
                <div class="claude-message user-message tool-result-message">
                    <div class="message-body">
                        { render_content_blocks(&blocks) }
                    </div>
                </div>
            }
        } else if !text_content.is_empty() {
            html! {
                <div class="claude-message user-message">
                    <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                        <span class="message-type-badge user">{ &label }</span>
                        <CopyButton text={text_content.clone()} title="Copy message" />
                    </div>
                    <div class="message-body">
                        <div class="user-text">{ render_markdown(&preserve_user_newlines(&text_content)) }</div>
                    </div>
                </div>
            }
        } else {
            html! {}
        }
    } else {
        html! {}
    }
}

pub fn render_error_message(msg: &ErrorMessage, timestamp: Option<&str>) -> Html {
    if msg.is_overload() {
        return render_overload_error(msg, timestamp);
    }

    let message = msg.display_message();
    let error_type = msg.error_type();

    html! {
        <div class="claude-message error-message-display">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge result error">{ "Error" }</span>
                {
                    if let Some(err_type) = error_type {
                        html! { <span class="error-type">{ err_type }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                <div class="error-text">{ crate::components::markdown::linkify_urls(message) }</div>
            </div>
        </div>
    }
}

/// Render a run of consecutive plain-text user messages as a single
/// grouped block. Each prompt is still rendered via `render_user_message`
/// (so name/copy chrome works), but the outer wrapper carries the
/// `.user-group` accent.
pub fn render_user_group(
    messages: &[String],
    current_user_id: Option<&str>,
    timestamp: Option<&str>,
) -> Html {
    let count = messages.len();
    let header_title = timestamp.unwrap_or_default().to_string();
    html! {
        <div class="claude-message message-group user-group">
            <div class="message-header" title={header_title}>
                <span class="message-type-badge user">{ "You" }</span>
                if count > 1 {
                    <span class="message-count">{ format!("× {}", count) }</span>
                }
            </div>
            <div class="message-body user-group-body">
                { for messages.iter().map(|json| {
                    match serde_json::from_str::<ClaudeMessage>(json) {
                        Ok(ClaudeMessage::User(msg)) => render_user_message(&msg, current_user_id, None),
                        _ => html! {},
                    }
                }) }
            </div>
        </div>
    }
}

/// Render a run of consecutive Codex protocol events as a single grouped
/// block. Each event is still dispatched through the existing
/// `CodexMessageRenderer` (so the per-event rendering stays in one place),
/// but the outer wrapper carries the `.codex-group` accent.
pub fn render_codex_group(messages: &[String], timestamp: Option<&str>) -> Html {
    let count = messages.len();
    let header_title = timestamp.unwrap_or_default().to_string();
    html! {
        <div class="claude-message message-group codex-group">
            <div class="message-header" title={header_title}>
                <span class="message-type-badge assistant">{ "Codex" }</span>
                if count > 1 {
                    <span class="message-count">{ format!("× {}", count) }</span>
                }
            </div>
            <div class="message-body codex-group-body">
                { for messages.iter().map(|json| {
                    html! { <crate::components::codex_renderer::CodexMessageRenderer json={json.clone()} /> }
                }) }
            </div>
        </div>
    }
}

/// Render a run of consecutive portal messages as a single grouped block.
///
/// Each individual portal message is still emitted with its own header +
/// body via `render_portal_message`, but they share one outer wrapper that
/// gets the `.portal-group` accent so the run reads as one visual unit.
/// The group title carries the count for quick scannability.
pub fn render_portal_group(messages: &[String], timestamp: Option<&str>) -> Html {
    let count = messages.len();
    let header_title = timestamp.unwrap_or_default().to_string();
    html! {
        <div class="claude-message message-group portal-group">
            <div class="message-header" title={header_title}>
                <span class="message-type-badge portal">{ "Portal" }</span>
                if count > 1 {
                    <span class="message-count">{ format!("× {}", count) }</span>
                }
            </div>
            <div class="message-body portal-group-body">
                { for messages.iter().map(|json| {
                    match serde_json::from_str::<ClaudeMessage>(json) {
                        Ok(ClaudeMessage::Portal(msg)) => render_portal_message(&msg, None),
                        _ => html! {},
                    }
                }) }
            </div>
        </div>
    }
}

pub fn render_portal_message(msg: &PortalMessage, timestamp: Option<&str>) -> Html {
    let copy_text: String = msg
        .content
        .iter()
        .filter_map(|c| match c {
            shared::PortalContent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    html! {
        <div class="claude-message portal-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge portal">{ "Portal" }</span>
                if !copy_text.is_empty() {
                    <CopyButton text={copy_text} title="Copy portal text" />
                }
            </div>
            <div class="message-body">
                { for msg.content.iter().map(render_portal_content) }
            </div>
        </div>
    }
}

fn render_portal_content(content: &shared::PortalContent) -> Html {
    match content {
        shared::PortalContent::Text { text } => render_markdown(text),
        shared::PortalContent::Image {
            media_type,
            data,
            file_path,
            file_size,
            source_type,
        } => {
            let source = ImageSource {
                source_type: source_type.clone().unwrap_or_else(|| "base64".to_string()),
                media_type: media_type.clone(),
                data: data.clone(),
            };
            let filename = file_path
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .map(|s| s.to_string());
            html! {
                <>
                    { render_portal_image_header(file_path.as_deref(), *file_size) }
                    { render_image_source(&source, filename) }
                </>
            }
        }
        shared::PortalContent::Reminder { title, body } => {
            html! { <PortalReminder title={title.clone()} body={body.clone()} /> }
        }
    }
}

#[derive(Properties, PartialEq)]
struct PortalReminderProps {
    title: AttrValue,
    body: AttrValue,
}

/// Collapsed-by-default "Portal features reminder" block. Header is always
/// visible; clicking it toggles the markdown body open/closed.
#[function_component(PortalReminder)]
fn portal_reminder(props: &PortalReminderProps) -> Html {
    let expanded = use_state(|| false);
    let on_toggle = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(!*expanded))
    };
    let header_class = if *expanded {
        "portal-reminder-header expanded"
    } else {
        "portal-reminder-header"
    };
    html! {
        <div class="portal-reminder">
            <button type="button" class={header_class} onclick={on_toggle}>
                <span class="portal-reminder-icon">{ "ⓘ" }</span>
                <span class="portal-reminder-title">{ &*props.title }</span>
                <span class="portal-reminder-toggle">{ if *expanded { "▾" } else { "▸" } }</span>
            </button>
            if *expanded {
                <div class="portal-reminder-body">
                    { render_markdown(&props.body) }
                </div>
            }
        </div>
    }
}

fn render_portal_image_header(file_path: Option<&str>, file_size: Option<u64>) -> Html {
    let Some(path) = file_path else {
        return html! {};
    };
    html! {
        <div class="tool-use-header">
            <span class="tool-icon">{ "\u{1f5bc}\u{fe0f}" }</span>
            <span class="read-file-path">{ path }</span>
            {
                if let Some(size) = file_size {
                    html! { <span class="tool-meta">{ format_file_size(size) }</span> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn render_overload_error(msg: &ErrorMessage, timestamp: Option<&str>) -> Html {
    let request_id = msg.request_id.as_deref().unwrap_or("unknown");

    html! {
        <div class="claude-message overload-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge overload">{ "API Busy" }</span>
            </div>
            <div class="message-body">
                <div class="overload-content">
                    <div class="overload-icon">{ "⏳" }</div>
                    <div class="overload-text">
                        <div class="overload-title">{ "Claude API is temporarily overloaded" }</div>
                        <div class="overload-description">
                            { "The API is experiencing high demand. Claude Code will automatically retry the request. Please wait a moment." }
                        </div>
                    </div>
                </div>
                <div class="overload-details">
                    <span class="request-id" title="Request ID for debugging">{ format!("Request: {}", request_id) }</span>
                </div>
            </div>
        </div>
    }
}

pub fn render_rate_limit_event(msg: &RateLimitEventMessage, timestamp: Option<&str>) -> Html {
    let info = msg.rate_limit_info.as_ref();
    let status = info.and_then(|i| i.status.as_deref()).unwrap_or("unknown");
    let rate_type = info
        .and_then(|i| i.rate_limit_type.as_deref())
        .unwrap_or("unknown");
    let resets_at = info.and_then(|i| i.resets_at).unwrap_or(0);
    let using_overage = info.and_then(|i| i.is_using_overage).unwrap_or(false);
    let utilization = info.and_then(|i| i.utilization);

    let reset_text = if resets_at > 0 {
        let now = (js_sys::Date::now() / 1000.0) as u64;
        if resets_at > now {
            let mins = (resets_at - now) / 60;
            if mins > 60 {
                format!("Resets in {}h {}m", mins / 60, mins % 60)
            } else {
                format!("Resets in {}m", mins)
            }
        } else {
            "Reset".to_string()
        }
    } else {
        String::new()
    };

    let format_type = rate_type.replace('_', " ");

    html! {
        <div class="claude-message rate-limit-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge rate-limit">{ "Rate Limit" }</span>
            </div>
            <div class="message-body">
                <div class="overload-content">
                    <div class="overload-icon">{ "\u{23f1}\u{fe0f}" }</div>
                    <div class="overload-text">
                        <div class="overload-title">{ format!("Rate limit: {} ({})", status, format_type) }</div>
                        <div class="overload-description">
                            { &reset_text }
                            { if using_overage { " \u{b7} Using overage" } else { "" } }
                        </div>
                        {
                            if let Some(pct) = utilization {
                                let pct_int = (pct * 100.0).round() as u32;
                                let bar_class = if pct >= 0.9 {
                                    "utilization-bar critical"
                                } else if pct >= 0.7 {
                                    "utilization-bar warning"
                                } else {
                                    "utilization-bar"
                                };
                                html! {
                                    <div class="utilization-row">
                                        <div class={bar_class}>
                                            <div class="utilization-fill" style={format!("width: {}%", pct_int)}></div>
                                        </div>
                                        <span class="utilization-label">{ format!("{}%", pct_int) }</span>
                                    </div>
                                }
                            } else {
                                html! {}
                            }
                        }
                    </div>
                </div>
            </div>
        </div>
    }
}

pub fn render_system_message(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    let subtype = msg.subtype.as_deref().unwrap_or("system");

    // Check if this is a compaction-related message via subtype or status field
    let status_value = msg
        .extra
        .as_ref()
        .and_then(|v| v.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("");

    if status_value == "compacting" {
        return render_compaction_beginning();
    }

    if subtype == "compact_boundary" {
        return render_compaction_completed(msg);
    }

    if subtype == "summary" || subtype == "compaction" || subtype == "context_compaction" {
        return render_compaction_completed(msg);
    }

    if subtype == "task_started" {
        return render_task_started(msg, timestamp);
    }

    if subtype == "task_progress" {
        return html! {};
    }

    if subtype == "task_notification" {
        return render_task_notification(msg, timestamp);
    }

    if subtype == "init" {
        return render_init_bar(msg, timestamp);
    }

    if subtype == "status" {
        return html! {};
    }

    html! {
        <div class="claude-message system-message compact" title={timestamp.unwrap_or_default().to_string()}>
            <span class="message-type-badge system">{ subtype }</span>
        </div>
    }
}

fn render_init_bar(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    let model_short = msg
        .model
        .as_deref()
        .and_then(shorten_model_name)
        .unwrap_or_default();
    let version = msg.claude_code_version.as_deref().unwrap_or("");
    let tool_count = msg.tools.as_ref().map(|t| t.len()).unwrap_or(0);
    let mcp_count = msg.mcp_servers.as_ref().map(|s| s.len()).unwrap_or(0);
    // Typed dispatch over `extra` (closes #752). `InitExtra::fast_mode_state`
    // mirrors `claude_codes::InitMessage::fast_mode_state` (already typed
    // upstream); we use a narrow local mirror because the SDK's full
    // `InitMessage` has many required fields and a partial frame here would
    // otherwise fail to deserialize.
    let init_extra: shared::InitExtra = msg
        .extra
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let fast_mode = init_extra.fast_mode_state.as_deref().unwrap_or("off");

    html! {
        <div class="claude-message system-message compact" title={timestamp.unwrap_or_default().to_string()}>
            <span class="message-type-badge system">{ "Session" }</span>
            if !model_short.is_empty() {
                <span class="init-badge">{ &model_short }</span>
            }
            if !version.is_empty() {
                <span class="init-badge">{ format!("v{}", version) }</span>
            }
            if fast_mode == "on" {
                <span class="init-badge fast">{ "Fast" }</span>
            }
            if mcp_count > 0 {
                <span class="init-badge">{ format!("{} MCP", mcp_count) }</span>
            }
            if tool_count > 0 {
                <span class="init-badge">{ format!("{} tools", tool_count) }</span>
            }
        </div>
    }
}

fn render_compaction_beginning() -> Html {
    html! {
        <div class="claude-message compaction-message compact">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Compaction Beginning" }</span>
            </div>
        </div>
    }
}

fn render_compaction_completed(msg: &SystemMessage) -> Html {
    // Typed dispatch over `extra` (closes #752). The SDK's
    // `CompactBoundaryMessage` currently only exposes
    // `compact_metadata { pre_tokens, trigger }` — not the `summary` /
    // `leaf_message_count` / `duration_ms` fields the renderer needs.
    // TODO(SDK rust-code-agent-sdks#141): drop the local `CompactionExtra`
    // mirror once upstream adds these fields to `CompactBoundaryMessage`,
    // and switch to `CCSystemMessage::as_compact_boundary()` here.
    let compact_extra: shared::CompactionExtra = msg
        .extra
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let summary_text = msg
        .summary
        .as_deref()
        .or_else(|| compact_extra.summary_text());
    let leaf_count = msg
        .leaf_message_count
        .or_else(|| compact_extra.message_count());
    let duration = msg.duration_ms.or(compact_extra.duration_ms);

    html! {
        <div class="claude-message compaction-message">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Compaction Completed" }</span>
                {
                    if let Some(count) = leaf_count {
                        html! {
                            <span class="compaction-stat" title="Messages summarized">
                                { format!("{} messages", count) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(ms) = duration {
                        html! {
                            <span class="compaction-stat" title="Compaction duration">
                                { format_duration(ms) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                <div class="compaction-content">
                    <div class="compaction-icon">{ "📦" }</div>
                    <div class="compaction-text">
                        {
                            if let Some(summary) = summary_text {
                                html! {
                                    <div class="compaction-summary">
                                        <div class="summary-label">{ "Summary:" }</div>
                                        <div class="summary-text">{ render_markdown(summary) }</div>
                                    </div>
                                }
                            } else {
                                html! {
                                    <div class="compaction-description">
                                        { "The conversation context has been summarized to free up space. Previous messages have been condensed while preserving important context." }
                                    </div>
                                }
                            }
                        }
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_task_started(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    let extra = msg.extra.as_ref();
    let description = extra
        .and_then(|v| v.get("description").and_then(|d| d.as_str()))
        .unwrap_or("Background task");
    let task_id = extra
        .and_then(|v| v.get("task_id").and_then(|t| t.as_str()))
        .unwrap_or("");

    let type_label = extra
        .and_then(|v| v.get("task_type"))
        .and_then(|v| serde_json::from_value::<shared::CCTaskType>(v.clone()).ok())
        .map(|tt| match tt {
            shared::CCTaskType::LocalAgent => "Sub-agent",
            shared::CCTaskType::LocalBash => "Background Bash",
        })
        .unwrap_or("Task");

    html! {
        <div class="claude-message task-message compact" title={format!("Task ID: {}", task_id)}>
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge task">{ "Task Started" }</span>
                <span class="task-type-badge">{ type_label }</span>
                <span class="task-description-inline">{ description }</span>
            </div>
        </div>
    }
}

fn render_task_notification(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    // Typed dispatch over `extra` (closes #752). `TaskNotificationExtra`
    // mirrors the renderable subset of `claude_codes::TaskNotificationMessage`
    // (the SDK type's required `session_id` / `summary` are already consumed
    // by the outer lenient `SystemMessage`'s typed top-level fields and would
    // not appear in the flattened `extra` Value).
    let notif: shared::TaskNotificationExtra = msg
        .extra
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let summary_text = msg.summary.as_deref();
    let task_id = notif.task_id.as_deref().unwrap_or("");
    let duration = notif.usage.as_ref().map(|u| u.duration_ms);
    let tool_uses = notif.usage.as_ref().map(|u| u.tool_uses);
    let total_tokens = notif.usage.as_ref().map(|u| u.total_tokens);

    let is_failed = matches!(notif.status, Some(shared::CCTaskStatus::Failed));
    let status_class = if is_failed { "failed" } else { "completed" };

    html! {
        <div class={classes!("claude-message", "task-message", status_class)}
             title={format!("Task ID: {}", task_id)}>
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class={classes!("message-type-badge", "task", status_class)}>
                    { if is_failed { "Task Failed" } else { "Task Completed" } }
                </span>
                {
                    if let Some(ms) = duration {
                        html! { <span class="task-stat">{ format_duration(ms) }</span> }
                    } else { html! {} }
                }
                {
                    if let Some(tools) = tool_uses {
                        html! { <span class="task-stat" title="Tool calls">{ format!("{} tools", tools) }</span> }
                    } else { html! {} }
                }
                {
                    if let Some(tokens) = total_tokens {
                        html! { <span class="task-stat" title="Total tokens">{ format!("{}k tokens", tokens / 1000) }</span> }
                    } else { html! {} }
                }
            </div>
            {
                if let Some(summary) = summary_text {
                    html! {
                        <div class="message-body">
                            <div class="task-summary">{ render_markdown(summary) }</div>
                        </div>
                    }
                } else { html! {} }
            }
        </div>
    }
}

pub fn render_assistant_message(
    msg: &AssistantMessage,
    timestamp: Option<&str>,
    raw_iso: Option<&str>,
) -> Html {
    let blocks = msg
        .message
        .as_ref()
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();

    let usage = msg.message.as_ref().and_then(|m| m.usage.as_ref());
    let model = msg
        .message
        .as_ref()
        .and_then(|m| m.model.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("");
    let stop_reason = msg.message.as_ref().and_then(|m| m.stop_reason.as_deref());
    let is_truncated = stop_reason == Some("max_tokens");

    let model_tooltip = build_model_tooltip(model, usage);
    let usage_tooltip = build_usage_tooltip(usage);
    let copy_text = content_blocks_to_text(&blocks);

    html! {
        <div class="claude-message assistant-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge assistant">{ "Assistant" }</span>
                {
                    if let Some(short_name) = shorten_model_name(model) {
                        html! { <span class="model-name" title={model_tooltip}>{ short_name }</span> }
                    } else {
                        html! {}
                    }
                }
                if !copy_text.is_empty() {
                    <CopyButton text={copy_text} title="Copy assistant text" />
                }
                {
                    if is_truncated {
                        html! { <span class="truncated-badge" title="Response was cut off (max_tokens)">{ "truncated" }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(u) = usage {
                        html! {
                            <span class="usage-badge" title={usage_tooltip}>
                                <span class="token-count">{ format!("{}↓ {}↑", u.input_tokens.unwrap_or(0), u.output_tokens.unwrap_or(0)) }</span>
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                { render_content_blocks(&blocks) }
            </div>
            if let Some(iso) = raw_iso {
                <div class="message-footer">
                    <TimeAgo iso={iso.to_string()} />
                </div>
            }
        </div>
    }
}

pub fn render_content_blocks(blocks: &[ContentBlock]) -> Html {
    html! {
        <>
            {
                blocks.iter().map(|block| {
                    match block {
                        ContentBlock::Text { text, citations } => {
                            html! {
                                <div class="assistant-text">
                                    { render_markdown(text) }
                                    { render_citations(citations) }
                                </div>
                            }
                        }
                        ContentBlock::ToolUse { id: _, name, input } => {
                            render_tool_use(name, input)
                        }
                        ContentBlock::ToolResult { tool_use_id: _, content, is_error } => {
                            let class = if *is_error { "tool-result error" } else { "tool-result" };
                            match content {
                                Some(ToolResultContent::Text(s)) => {
                                    html! {
                                        <div class={class}>
                                            <ExpandableText full_text={s.clone()} max_len={500} class="tool-result-content" />
                                        </div>
                                    }
                                }
                                Some(ToolResultContent::Structured(blocks)) => {
                                    html! {
                                        <div class={class}>
                                            { for blocks.iter().map(|v| {
                                                match serde_json::from_value::<shared::ContentBlock>(v.clone()) {
                                                    Ok(typed) => render_structured_block(&typed),
                                                    Err(_) => {
                                                        let json = serde_json::to_string_pretty(v).unwrap_or_default();
                                                        html! { <pre class="tool-result-content">{ json }</pre> }
                                                    }
                                                }
                                            }) }
                                        </div>
                                    }
                                }
                                None => html! { <div class={class}></div> },
                            }
                        }
                        ContentBlock::Image { source } => {
                            render_image_source(source, None)
                        }
                        ContentBlock::Thinking { thinking } => {
                            html! {
                                <div class="thinking-block">
                                    <span class="thinking-label">{ "thinking" }</span>
                                    <div class="thinking-content">{ crate::components::markdown::linkify_urls(thinking) }</div>
                                </div>
                            }
                        }
                        ContentBlock::ServerToolUse { id: _, name, input } => {
                            render_server_tool_use(name, input)
                        }
                        ContentBlock::WebSearchToolResult { tool_use_id: _, content } => {
                            render_web_search_result(content)
                        }
                        ContentBlock::CodeExecutionToolResult { tool_use_id: _, content } => {
                            render_code_execution_result(content)
                        }
                        ContentBlock::McpToolUse { id: _, name, server_name, input } => {
                            render_mcp_tool_use(name, server_name.as_deref(), input)
                        }
                        ContentBlock::McpToolResult { tool_use_id: _, content, is_error } => {
                            render_mcp_tool_result(content, is_error.unwrap_or(false))
                        }
                        ContentBlock::ContainerUpload { data } => {
                            render_container_upload(data)
                        }
                        ContentBlock::Unknown(value) => {
                            render_unknown_block(value)
                        }
                    }
                }).collect::<Html>()
            }
        </>
    }
}

const ALLOWED_IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

fn render_image_source(source: &ImageSource, filename: Option<String>) -> Html {
    if !ALLOWED_IMAGE_MEDIA_TYPES.contains(&source.media_type.as_str()) {
        return html! {
            <pre class="tool-result-content">
                { format!("[unsupported image type: {}]", source.media_type) }
            </pre>
        };
    }
    // Support both URL sources (from backend image store) and base64 data URIs
    let src = if source.source_type == "url" {
        source.data.clone()
    } else {
        format!("data:{};base64,{}", source.media_type, source.data)
    };
    html! {
        <ImageViewer src={src} media_type={source.media_type.clone()} {filename} />
    }
}

#[derive(Properties, PartialEq)]
struct ImageViewerProps {
    pub src: String,
    pub media_type: String,
    #[prop_or_default]
    pub filename: Option<String>,
}

#[function_component(ImageViewer)]
fn image_viewer(props: &ImageViewerProps) -> Html {
    let expanded = use_state(|| false);

    // Close lightbox on Escape key (capture phase so it doesn't trigger nav mode)
    {
        let expanded = expanded.clone();
        use_effect_with(*expanded, move |is_expanded| {
            let listener = if *is_expanded {
                let expanded = expanded.clone();
                let options = gloo::events::EventListenerOptions {
                    phase: gloo::events::EventListenerPhase::Capture,
                    passive: false,
                };
                Some(gloo::events::EventListener::new_with_options(
                    &gloo::utils::document(),
                    "keydown",
                    options,
                    move |event| {
                        if let Some(ke) = event.dyn_ref::<web_sys::KeyboardEvent>() {
                            if ke.key() == "Escape" {
                                ke.prevent_default();
                                ke.stop_propagation();
                                expanded.set(false);
                            }
                        }
                    },
                ))
            } else {
                None
            };
            move || drop(listener)
        });
    }

    let on_thumb_click = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(true))
    };

    let on_close = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(false))
    };

    let ext = match props.media_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "bin",
    };

    let download_name = props
        .filename
        .clone()
        .unwrap_or_else(|| format!("image.{ext}"));

    html! {
        <>
            <div class="tool-result-image" onclick={on_thumb_click}>
                <img src={props.src.clone()} alt="Tool result image" />
            </div>
            if *expanded {
                <div class="image-lightbox" onclick={on_close.clone()}>
                    <div class="image-lightbox-content" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <img src={props.src.clone()} alt="Full size image" />
                        <div class="image-lightbox-controls">
                            <a
                                class="image-lightbox-download"
                                href={props.src.clone()}
                                download={download_name}
                            >
                                { "Download" }
                            </a>
                            <button class="image-lightbox-close" onclick={on_close}>
                                { "\u{00d7}" }
                            </button>
                        </div>
                    </div>
                </div>
            }
        </>
    }
}

fn render_citations(citations: &[Citation]) -> Html {
    if citations.is_empty() {
        return html! {};
    }
    html! {
        <div class="citation-list">
            { for citations.iter().enumerate().map(|(i, cite)| {
                let url = cite.url.as_deref().unwrap_or("#");
                let title = cite.title.as_deref()
                    .or(cite.cited_text.as_deref())
                    .unwrap_or("source");
                html! {
                    <a class="citation-link"
                       href={url.to_string()}
                       target="_blank"
                       rel="noopener noreferrer"
                       title={title.to_string()}>
                        { format!("[{}]", i + 1) }
                    </a>
                }
            })}
        </div>
    }
}

fn render_server_tool_use(name: &str, input: &Value) -> Html {
    let badge_label = if name.contains("web_search") || name.contains("search") {
        "Web Search"
    } else {
        "Server"
    };
    let args_summary = summarize_input(input);
    html! {
        <div class="tool-use server-tool-use">
            <div class="tool-use-header">
                <span class="tool-badge server">{ badge_label }</span>
                <span class="tool-name">{ name }</span>
                { if !args_summary.is_empty() {
                    html! { <span class="tool-meta">{ args_summary }</span> }
                } else {
                    html! {}
                }}
            </div>
        </div>
    }
}

fn render_web_search_result(content: &Value) -> Html {
    let preview = serde_json::to_string_pretty(content).unwrap_or_else(|_| content.to_string());
    html! {
        <div class="tool-result web-search-result">
            <div class="tool-use-header">
                <span class="tool-badge server">{ "Web Search Result" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={300} class="tool-result-content" />
        </div>
    }
}

fn render_code_execution_result(content: &Value) -> Html {
    let preview = serde_json::to_string_pretty(content).unwrap_or_else(|_| content.to_string());
    html! {
        <div class="tool-result code-execution-result">
            <div class="tool-use-header">
                <span class="tool-badge code-exec">{ "Code Execution" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={500} class="tool-result-content" />
        </div>
    }
}

fn render_mcp_tool_use(name: &str, server_name: Option<&str>, input: &Value) -> Html {
    let display_name = match server_name {
        Some(server) => format!("{} > {}", server, name),
        None => name.to_string(),
    };
    let args_summary = summarize_input(input);
    html! {
        <div class="tool-use mcp-tool-use">
            <div class="tool-use-header">
                <span class="tool-badge mcp">{ "MCP" }</span>
                <span class="tool-name">{ display_name }</span>
                { if !args_summary.is_empty() {
                    html! { <span class="tool-meta">{ args_summary }</span> }
                } else {
                    html! {}
                }}
            </div>
        </div>
    }
}

fn render_mcp_tool_result(content: &Value, is_error: bool) -> Html {
    let class = if is_error {
        "tool-result mcp-tool-result error"
    } else {
        "tool-result mcp-tool-result"
    };
    let preview = serde_json::to_string_pretty(content).unwrap_or_else(|_| content.to_string());
    html! {
        <div class={class}>
            <div class="tool-use-header">
                <span class={if is_error { "tool-badge mcp error" } else { "tool-badge mcp" }}>
                    { if is_error { "MCP Error" } else { "MCP Result" } }
                </span>
            </div>
            <ExpandableText full_text={preview} max_len={500} class="tool-result-content" />
        </div>
    }
}

fn render_container_upload(data: &Value) -> Html {
    let preview = serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
    html! {
        <div class="tool-use container-upload">
            <div class="tool-use-header">
                <span class="tool-badge container">{ "Container Upload" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={300} class="tool-result-content" />
        </div>
    }
}

fn render_unknown_block(value: &Value) -> Html {
    let preview = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    html! {
        <div class="tool-use unknown-block">
            <div class="tool-use-header">
                <span class="tool-badge unknown">{ "Unknown Block" }</span>
            </div>
            <ExpandableText full_text={preview} max_len={300} class="tool-result-content" />
        </div>
    }
}

fn summarize_input(input: &Value) -> String {
    if let Some(obj) = input.as_object() {
        let entries: Vec<String> = obj
            .iter()
            .filter(|(_, v)| v.is_string() || v.is_number() || v.is_boolean())
            .take(3)
            .map(|(k, v)| match v {
                Value::String(s) => {
                    let truncated = if s.len() > 40 {
                        format!("{}...", super::truncate_str(s, 40))
                    } else {
                        s.clone()
                    };
                    format!("{}={}", k, truncated)
                }
                other => format!("{}={}", k, other),
            })
            .collect();
        entries.join(", ")
    } else {
        String::new()
    }
}

fn render_structured_block(block: &shared::ContentBlock) -> Html {
    match block {
        shared::ContentBlock::Image(_) => {
            html! { <span class="tool-result-image-tag">{ "[image]" }</span> }
        }
        shared::ContentBlock::Text(t) => {
            html! { <ExpandableText full_text={t.text.clone()} max_len={500} class="tool-result-content" /> }
        }
        other => {
            let json = serde_json::to_string_pretty(other).unwrap_or_default();
            html! { <pre class="tool-result-content">{ json }</pre> }
        }
    }
}

pub fn render_result_message(msg: &ResultMessage) -> Html {
    let is_error = msg.is_error.unwrap_or(false);
    let status_class = if is_error { "error" } else { "success" };

    let duration_ms = msg.duration_ms.unwrap_or(0);
    let api_ms = msg.duration_api_ms.unwrap_or(0);
    let turns = msg.num_turns.unwrap_or(0);

    let mut timing_tooltip = format!(
        "Total: {}ms | API: {}ms | Turns: {}",
        duration_ms, api_ms, turns
    );

    if let Some(model_usage) = &msg.model_usage {
        for (model, entry) in model_usage {
            timing_tooltip.push_str(&format!(
                " | {}: ${:.4}",
                shorten_model_name(model).unwrap_or_else(|| model.clone()),
                entry.cost_usd
            ));
        }
    }

    let errors_tooltip = if !msg.errors.is_empty() {
        msg.errors.join("\n")
    } else {
        String::new()
    };

    let denials_tooltip = if !msg.permission_denials.is_empty() {
        msg.permission_denials
            .iter()
            .filter_map(|v| {
                v.get("tool_name")
                    .and_then(|t| t.as_str())
                    .or_else(|| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        String::new()
    };

    let extra_badges = html! {
        <>
            {
                if let Some(cost) = msg.total_cost_usd {
                    html! {
                        <span class="stat-item cost" title="Total cost">
                            { format!("${:.2}", cost) }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if msg.stop_reason.as_deref() == Some("max_tokens") {
                    html! {
                        <span class="stat-item stop-reason" title="Session stopped: max tokens reached">
                            { "max tokens" }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if msg.fast_mode_state.as_deref() == Some("on") {
                    html! {
                        <span class="stat-item fast-mode" title="Fast mode enabled">
                            { "Fast" }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if !msg.errors.is_empty() {
                    html! {
                        <span class="stat-item errors" title={errors_tooltip.clone()}>
                            { format!("{} error{}", msg.errors.len(), if msg.errors.len() == 1 { "" } else { "s" }) }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if !msg.permission_denials.is_empty() {
                    html! {
                        <span class="stat-item denials" title={denials_tooltip.clone()}>
                            { format!("{} denied", msg.permission_denials.len()) }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
        </>
    };

    if is_error {
        if let Some(error_html) = try_render_api_error(msg.result.as_deref()) {
            return html! {
                <>
                    { error_html }
                    <div class={classes!("claude-message", "result-message", status_class)}>
                        <div class="result-stats-bar">
                            <span class={classes!("result-status", status_class)}>{ "✗" }</span>
                            <span class="stat-item duration" title={timing_tooltip.clone()}>
                                { format_duration(duration_ms) }
                            </span>
                            { extra_badges.clone() }
                        </div>
                    </div>
                </>
            };
        }
    }

    html! {
        <div class={classes!("claude-message", "result-message", status_class)}>
            <div class="result-stats-bar">
                <span class={classes!("result-status", status_class)}>
                    { if is_error { "✗" } else { "✓" } }
                </span>
                <span class="stat-item duration" title={timing_tooltip.clone()}>
                    { format_duration(duration_ms) }
                </span>
                {
                    if let Some(usage) = &msg.usage {
                        html! {
                            <>
                                <span class="stat-item tokens" title="Input tokens">
                                    { format!("{}↓", usage.input_tokens.unwrap_or(0)) }
                                </span>
                                <span class="stat-item tokens" title="Output tokens">
                                    { format!("{}↑", usage.output_tokens.unwrap_or(0)) }
                                </span>
                            </>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    if turns > 1 {
                        html! {
                            <span class="stat-item turns" title="API turns">
                                { format!("{} turns", turns) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
                { extra_badges }
            </div>
        </div>
    }
}

// --- API error rendering ---

#[derive(Debug, Deserialize)]
struct AnthropicApiError {
    #[serde(rename = "type")]
    error_type: Option<String>,
    error: Option<AnthropicErrorDetails>,
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetails {
    #[serde(rename = "type")]
    error_type: Option<String>,
    message: Option<String>,
}

fn try_render_api_error(result_text: Option<&str>) -> Option<Html> {
    let text = result_text?;

    let json_start = text.find('{')?;
    let json_str = &text[json_start..];

    let api_error: AnthropicApiError = serde_json::from_str(json_str).ok()?;

    if api_error.error_type.as_deref() != Some("error") {
        return None;
    }

    let error_details = api_error.error.as_ref();
    let error_type = error_details
        .and_then(|e| e.error_type.as_deref())
        .unwrap_or("unknown_error");
    let error_message = error_details
        .and_then(|e| e.message.as_deref())
        .unwrap_or("An error occurred");
    let request_id = api_error.request_id.as_deref();

    let http_status = if text.starts_with("API Error:") {
        text.split_whitespace()
            .nth(2)
            .and_then(|s| s.parse::<u16>().ok())
    } else {
        None
    };

    let display_type = format_error_type(error_type);

    Some(html! {
        <div class="claude-message anthropic-error-message">
            <div class="message-header">
                <span class="message-type-badge anthropic-error">{ "Anthropic API Error" }</span>
                {
                    if let Some(status) = http_status {
                        html! { <span class="http-status">{ format!("HTTP {}", status) }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                <div class="anthropic-error-content">
                    <div class="error-icon">{ "⚠" }</div>
                    <div class="error-details">
                        <div class="error-type-display">{ display_type }</div>
                        <div class="error-message-text">{ crate::components::markdown::linkify_urls(error_message) }</div>
                    </div>
                </div>
                {
                    if let Some(req_id) = request_id {
                        html! {
                            <div class="error-request-id">
                                <span class="request-id-label">{ "Request ID: " }</span>
                                <code class="request-id-value">{ req_id }</code>
                            </div>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    })
}

fn format_error_type(error_type: &str) -> String {
    match error_type {
        "api_error" => "Internal Server Error".to_string(),
        "authentication_error" => "Authentication Failed".to_string(),
        "invalid_request_error" => "Invalid Request".to_string(),
        "not_found_error" => "Not Found".to_string(),
        "overloaded_error" => "API Overloaded".to_string(),
        "permission_error" => "Permission Denied".to_string(),
        "rate_limit_error" => "Rate Limited".to_string(),
        "request_too_large" => "Request Too Large".to_string(),
        other => other.replace('_', " ").to_string(),
    }
}

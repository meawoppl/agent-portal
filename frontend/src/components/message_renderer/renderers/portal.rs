use super::super::types::PortalMessage;
use super::{render_image_source, render_video_source};
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown_for_session;
use std::collections::HashMap;
use uuid::Uuid;
use yew::prelude::*;

pub fn render_portal_message(
    msg: &PortalMessage,
    timestamp: Option<&str>,
    session_id: Uuid,
    continuation_statuses: &HashMap<Uuid, String>,
    on_schedule_continuation: Callback<Uuid>,
) -> Html {
    if let Some(event) = agent_message_event(msg) {
        return render_agent_message_event(&event, timestamp, session_id);
    }

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
            <div class="message-body">{ render_portal_message_content(msg, session_id, continuation_statuses, on_schedule_continuation) }</div>
        </div>
    }
}

pub fn render_portal_message_content(
    msg: &PortalMessage,
    session_id: Uuid,
    continuation_statuses: &HashMap<Uuid, String>,
    on_schedule_continuation: Callback<Uuid>,
) -> Html {
    html! { <>{ for msg.content.iter().map(|content| render_portal_content(content, session_id, continuation_statuses, on_schedule_continuation.clone())) }</> }
}

fn render_portal_content(
    content: &shared::PortalContent,
    session_id: Uuid,
    continuation_statuses: &HashMap<Uuid, String>,
    on_schedule_continuation: Callback<Uuid>,
) -> Html {
    match content {
        shared::PortalContent::Text { text } => render_markdown_for_session(text, session_id),
        shared::PortalContent::Image {
            media_type,
            data,
            file_path,
            file_size,
            source_type,
        } => {
            let source = shared::ImageSource {
                source_type: shared::ImageSourceType::from(
                    source_type.as_deref().unwrap_or("base64"),
                ),
                media_type: shared::MediaType::from(media_type.as_str()),
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
        shared::PortalContent::Video {
            media_type,
            data,
            file_path,
            file_size,
            ..
        } => {
            let filename = file_path
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .map(|s| s.to_string());
            html! {
                <>
                    { render_portal_image_header(file_path.as_deref(), *file_size) }
                    { render_video_source(media_type, data, filename) }
                </>
            }
        }
        shared::PortalContent::Reminder { title, body } => {
            html! { <PortalReminder title={title.clone()} body={body.clone()} session_id={session_id} /> }
        }
        shared::PortalContent::ContinuationPrompt {
            continuation_id,
            reset_at,
            status,
            source_message,
            reason,
        } => render_continuation_prompt(
            *continuation_id,
            reset_at,
            continuation_statuses
                .get(continuation_id)
                .map(String::as_str)
                .unwrap_or(status),
            source_message,
            reason,
            on_schedule_continuation,
        ),
        shared::PortalContent::AgentMessage { text, .. } => {
            render_markdown_for_session(text, session_id)
        }
    }
}

fn agent_label(agent_type: &str) -> &'static str {
    match agent_type.to_ascii_lowercase().as_str() {
        "claude" => "Claude",
        "codex" => "Codex",
        _ => "agent",
    }
}

pub(crate) fn render_agent_message_from_source(
    from_session_id: &Uuid,
    from_agent_type: &str,
    text: &str,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    let event = AgentMessageEvent {
        from_agent_type: from_agent_type.to_string(),
        from_session_id: from_session_id.to_string(),
        text: text.to_string(),
    };
    render_agent_message_event_card(&event, timestamp, session_id)
}

fn render_agent_message_event_card(
    event: &AgentMessageEvent,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    let short = event
        .from_session_id
        .split('-')
        .next()
        .unwrap_or(&event.from_session_id);
    let label = agent_label(&event.from_agent_type);
    html! {
        <div class="claude-message user-message other-agent-message agent-message-event">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge other-agent"
                    title={format!("Message from {label} session {}", event.from_session_id)}>
                    { format!("Message from {label} ({short})") }
                </span>
                <CopyButton text={event.text.clone()} title="Copy message" />
            </div>
            <div class="message-body">
                { render_agent_message_body(&event.text, session_id) }
            </div>
        </div>
    }
}

pub(crate) fn render_agent_message_body(text: &str, session_id: Uuid) -> Html {
    html! {
        <div class="user-text">
            { render_markdown_for_session(&text.replace('\n', "  \n"), session_id) }
        </div>
    }
}

pub(crate) fn portal_text(msg: &PortalMessage) -> String {
    msg.content
        .iter()
        .filter_map(|content| match content {
            shared::PortalContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentMessageEvent {
    pub(crate) from_agent_type: String,
    pub(crate) from_session_id: String,
    pub(crate) text: String,
}

pub(crate) fn agent_message_event(msg: &PortalMessage) -> Option<AgentMessageEvent> {
    let [shared::PortalContent::AgentMessage {
        from_agent_type,
        from_session_id,
        text,
    }] = msg.content.as_slice()
    else {
        return None;
    };

    Some(AgentMessageEvent {
        from_agent_type: from_agent_type.clone(),
        from_session_id: from_session_id.clone(),
        text: text.clone(),
    })
}

pub(crate) fn render_agent_message_event(
    event: &AgentMessageEvent,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    render_agent_message_event_card(event, timestamp, session_id)
}

fn render_continuation_prompt(
    continuation_id: Uuid,
    reset_at: &str,
    status: &str,
    source_message: &str,
    reason: &str,
    on_schedule_continuation: Callback<Uuid>,
) -> Html {
    let overloaded = reason == shared::CONTINUATION_REASON_OVERLOADED;
    let terminal = overloaded
        || matches!(
            status,
            "scheduled" | "scheduling" | "fired" | "dropped" | "failed"
        );
    let status_class = status.to_string();
    // Overload retries are auto-scheduled (no user click), so the button is a
    // passive status pill and its wording reflects the retry, not a limit reset.
    let button_label = if overloaded {
        match status {
            "fired" => "Retried",
            "dropped" => "Retry dropped",
            "failed" => "Retry failed",
            _ => "Auto-retrying",
        }
    } else {
        match status {
            "scheduled" => "Scheduled",
            "scheduling" => "Scheduling...",
            "fired" => "Continued",
            "dropped" => "Dropped",
            "failed" => "Failed",
            _ => "Continue 2 min after limit lifts",
        }
    };
    let title = if overloaded {
        "Provider overloaded — auto-retrying"
    } else {
        "Claude session limit reached"
    };
    let onclick = {
        let on_schedule_continuation = on_schedule_continuation.clone();
        Callback::from(move |_: MouseEvent| {
            on_schedule_continuation.emit(continuation_id);
        })
    };

    html! {
        <div class="continuation-card">
            <div class="continuation-copy">
                <div class="continuation-title">{ title }</div>
                if !overloaded {
                    <div class="continuation-detail">{ format_continuation_label(reset_at) }</div>
                }
                if !source_message.is_empty() {
                    <div class="continuation-source">{ source_message }</div>
                }
            </div>
            <button
                type="button"
                class={classes!("continuation-button", status_class)}
                disabled={terminal}
                {onclick}
            >
                { button_label }
            </button>
        </div>
    }
}

fn format_continuation_label(reset_at: &str) -> String {
    let ms = js_sys::Date::parse(reset_at);
    if ms.is_nan() {
        return format!(
            "Limit resets at {}; continuation runs 2 minutes later",
            reset_at
        );
    }
    let reset_date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    let reset_local = reset_date
        .to_locale_string("default", &js_sys::Object::new())
        .as_string()
        .unwrap_or_else(|| reset_at.to_string());
    let continue_date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms + 120_000.0));
    let continue_local = continue_date
        .to_locale_string("default", &js_sys::Object::new())
        .as_string()
        .unwrap_or_else(|| "2 minutes later".to_string());
    format!(
        "Limit resets at {}; continuation runs at {}",
        reset_local, continue_local
    )
}

#[derive(Properties, PartialEq)]
struct PortalReminderProps {
    title: AttrValue,
    body: AttrValue,
    session_id: Uuid,
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
                    { render_markdown_for_session(&props.body, props.session_id) }
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
                    html! { <span class="tool-meta">{ crate::utils::format_file_size(size) }</span> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_message_content() -> Vec<shared::PortalContent> {
        vec![shared::PortalContent::AgentMessage {
            from_agent_type: "claude".to_string(),
            from_session_id: "11111111-1111-1111-1111-111111111111".to_string(),
            text: "hello from stale proxy".to_string(),
        }]
    }

    #[test]
    fn agent_message_event_reads_typed_portal_event_content() {
        let msg = PortalMessage {
            content: agent_message_content(),
        };

        let event = agent_message_event(&msg).expect("event");

        assert_eq!(event.from_agent_type, "claude");
        assert_eq!(
            event.from_session_id,
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(event.text, "hello from stale proxy");
    }

    #[test]
    fn agent_message_event_ignores_plain_portal_text() {
        let msg = PortalMessage {
            content: vec![shared::PortalContent::Text {
                text: "[message from claude 1111]\nbody".to_string(),
            }],
        };

        assert!(agent_message_event(&msg).is_none());
    }
}

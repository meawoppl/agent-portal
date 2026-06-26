use super::super::types::PortalMessage;
use super::render_image_source;
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
        shared::PortalContent::Reminder { title, body } => {
            html! { <PortalReminder title={title.clone()} body={body.clone()} session_id={session_id} /> }
        }
        shared::PortalContent::ContinuationPrompt {
            continuation_id,
            reset_at,
            status,
            source_message,
        } => render_continuation_prompt(
            *continuation_id,
            reset_at,
            continuation_statuses
                .get(continuation_id)
                .map(String::as_str)
                .unwrap_or(status),
            source_message,
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

fn render_agent_message_event(
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

fn render_agent_message_body(text: &str, session_id: Uuid) -> Html {
    html! {
        <div class="user-text">
            { render_markdown_for_session(&text.replace('\n', "  \n"), session_id) }
        </div>
    }
}

fn portal_text(msg: &PortalMessage) -> String {
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
struct AgentMessageEvent {
    from_agent_type: String,
    from_session_id: String,
    text: String,
}

fn agent_message_event(msg: &PortalMessage) -> Option<AgentMessageEvent> {
    if let Some(shared::MessageOrigin::InterAgent {
        from_session_id,
        from_agent_type,
    }) = &msg.origin
    {
        return Some(AgentMessageEvent {
            from_agent_type: from_agent_type.clone(),
            from_session_id: from_session_id.to_string(),
            text: portal_text(msg),
        });
    }

    // Defensive mixed-version fallback: a stale proxy can echo the typed
    // portal event before a new backend has normalized it into record-level
    // provenance. This stays typed and does not revive body-text prefix
    // parsing.
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

fn render_continuation_prompt(
    continuation_id: Uuid,
    reset_at: &str,
    status: &str,
    source_message: &str,
    on_schedule_continuation: Callback<Uuid>,
) -> Html {
    let reset_label = format_reset_label(reset_at);
    let terminal = matches!(
        status,
        "scheduled" | "scheduling" | "fired" | "dropped" | "failed"
    );
    let status_class = status.to_string();
    let button_label = match status {
        "scheduled" => "Scheduled",
        "scheduling" => "Scheduling...",
        "fired" => "Continued",
        "dropped" => "Dropped",
        "failed" => "Failed",
        _ => "Continue when limit lifted",
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
                <div class="continuation-title">{ "Claude session limit reached" }</div>
                <div class="continuation-detail">{ reset_label }</div>
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

fn format_reset_label(reset_at: &str) -> String {
    let ms = js_sys::Date::parse(reset_at);
    if ms.is_nan() {
        return format!("Resets at {}", reset_at);
    }
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    let local = date
        .to_locale_string("default", &js_sys::Object::new())
        .as_string()
        .unwrap_or_else(|| reset_at.to_string());
    format!("Resets at {}", local)
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
    fn agent_message_event_prefers_record_origin() {
        let from_session_id =
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("uuid");
        let msg = PortalMessage {
            content: vec![shared::PortalContent::Text {
                text: "hello from metadata".to_string(),
            }],
            origin: Some(shared::MessageOrigin::InterAgent {
                from_session_id,
                from_agent_type: "codex".to_string(),
            }),
        };

        let event = agent_message_event(&msg).expect("event");

        assert_eq!(event.from_agent_type, "codex");
        assert_eq!(
            event.from_session_id,
            "22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(event.text, "hello from metadata");
    }

    #[test]
    fn agent_message_event_falls_back_to_typed_portal_event() {
        let msg = PortalMessage {
            content: agent_message_content(),
            origin: None,
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
            origin: None,
        };

        assert!(agent_message_event(&msg).is_none());
    }
}

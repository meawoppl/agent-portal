use super::super::types::PortalMessage;
use super::render_image_source;
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown_for_session;
use yew::prelude::*;

pub fn render_portal_message(
    msg: &PortalMessage,
    timestamp: Option<&str>,
    session_id: Option<uuid::Uuid>,
) -> Html {
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
            <div class="message-body">{ render_portal_message_content(msg, session_id) }</div>
        </div>
    }
}

pub fn render_portal_message_content(msg: &PortalMessage, session_id: Option<uuid::Uuid>) -> Html {
    html! { <>{ for msg.content.iter().map(|content| render_portal_content(content, session_id)) }</> }
}

fn render_portal_content(content: &shared::PortalContent, session_id: Option<uuid::Uuid>) -> Html {
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
                    { render_markdown_for_session(&props.body, None) }
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

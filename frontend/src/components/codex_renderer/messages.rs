use super::item_card_classes;
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown_for_session;
use uuid::Uuid;
use yew::prelude::*;

/// `None` until the message has text. An agent message starts empty and only
/// fills in on completion, so rendering it earlier draws a blank card.
pub(super) fn render_agent_message(text: &str, completed: bool, session_id: Uuid) -> Option<Html> {
    let content = render_agent_message_content(text, session_id)?;
    let class = item_card_classes(completed);
    Some(html! {
        <div class={class}>
            <div class="message-header">
                <span class="message-type-badge assistant">{ "Codex" }</span>
                <CopyButton text={text.to_string()} title="Copy message" />
            </div>
            <div class="message-body">{ content }</div>
        </div>
    })
}

pub(super) fn render_agent_message_content(text: &str, session_id: Uuid) -> Option<Html> {
    crate::utils::is_non_blank(text).then(|| {
        html! { <div class="assistant-text">{ render_markdown_for_session(text, session_id) }</div> }
    })
}

/// A reasoning trace renders collapsed under a "reasoning" label. Models that
/// keep their reasoning private send none: while that item is in progress a
/// pulsing label shows the agent is working, and once it completes there is
/// nothing to show, so it renders `None` instead of an empty card.
pub(super) fn render_reasoning(trace: &str, completed: bool) -> Option<Html> {
    let class = item_card_classes(completed);
    if !crate::utils::is_non_blank(trace) {
        return (!completed).then(|| {
            html! {
                <div class={class}>
                    <div class="message-body">
                        <div class="thinking-block">
                            <span class="thinking-label">{ "reasoning…" }</span>
                        </div>
                    </div>
                </div>
            }
        });
    }
    Some(html! {
        <div class={class}>
            <div class="message-body">
                <details class="thinking-block codex-reasoning">
                    <summary class="thinking-label">{ "reasoning" }</summary>
                    <div class="thinking-content">{ crate::components::markdown::linkify_urls(trace) }</div>
                </details>
            </div>
        </div>
    })
}

pub(super) fn render_error_block(message: Option<&str>) -> Html {
    let message = message.unwrap_or("Unknown error");
    html! {
        <div class="claude-message error-message-display">
            <div class="message-header">
                <span class="message-type-badge result error">{ "Error" }</span>
                <CopyButton text={message.to_string()} title="Copy error" />
            </div>
            <div class="message-body">
                <div class="error-text">{ message }</div>
            </div>
        </div>
    }
}

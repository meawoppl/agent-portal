use super::item_card_classes;
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown_for_session;
use uuid::Uuid;
use yew::prelude::*;

pub(super) fn render_agent_message(text: &str, completed: bool, session_id: Uuid) -> Html {
    if text.is_empty() {
        return html! {};
    }
    let class = item_card_classes(completed);
    html! {
        <div class={class}>
            <div class="message-header">
                <span class="message-type-badge assistant">{ "Codex" }</span>
                <CopyButton text={text.to_string()} title="Copy message" />
            </div>
            <div class="message-body">{ render_agent_message_content(text, session_id) }</div>
        </div>
    }
}

pub(super) fn render_agent_message_content(text: &str, session_id: Uuid) -> Html {
    if text.is_empty() {
        html! {}
    } else {
        html! { <div class="assistant-text">{ render_markdown_for_session(text, session_id) }</div> }
    }
}

pub(super) fn render_reasoning(text: &str, completed: bool) -> Html {
    if text.is_empty() {
        return html! {};
    }
    let class = item_card_classes(completed);
    html! {
        <div class={class}>
            <div class="message-body">
                <div class="thinking-block">
                    <span class="thinking-label">{ "reasoning" }</span>
                    <div class="thinking-content">{ text }</div>
                </div>
            </div>
        </div>
    }
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

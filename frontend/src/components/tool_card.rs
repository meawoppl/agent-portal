use super::codex_renderer::item_card_classes;
use yew::prelude::*;

/// Shared tool-style card chrome: wraps a per-variant body in the standard
/// card wrapper (with in-progress styling), message-body, tool-use-section,
/// and a tool-use-header with icon + name + optional `status` meta line.
///
/// Previously duplicated in `codex_renderer::tool_card` and hand-rolled in
/// `muse_renderer::render_task_node` / Claude `tool_renderers`. Centralizing
/// the wrapper here means the `codex-item-in-progress` / `muse-task-in-progress`
/// pulse (`frontend/styles/messages.css:1716`) changes in one place.
///
/// Returns `html! {}` when `body` is empty so callers can short-circuit.
pub(crate) fn tool_card(
    icon: &str,
    name: String,
    status: Option<Html>,
    body: Html,
    completed: bool,
) -> Html {
    html! {
        <div class={item_card_classes(completed)}>
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ icon }</span>
                        <span class="tool-name">{ name }</span>
                        { if let Some(s) = status {
                            html! { <span class="tool-meta">{ s }</span> }
                        } else {
                            html! {}
                        } }
                    </div>
                    { body }
                </div>
            </div>
        </div>
    }
}

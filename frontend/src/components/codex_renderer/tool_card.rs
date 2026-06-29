use super::item_card_classes;
use yew::prelude::*;

/// Wraps a per-variant body in the standard tool-style card chrome:
/// card wrapper (with in-progress styling), message-body, tool-use-section,
/// and a tool-use-header with icon + name + optional `status` meta line.
/// Returns `html! {}` when `body` is empty so callers can short-circuit
/// empty-data cases by handing in a no-op body.
pub(super) fn tool_card(
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

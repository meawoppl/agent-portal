use yew::prelude::*;
use yew::virtual_dom::Key;

/// Renderer-neutral chrome for a tool or task card.
///
/// Callers own their state classes and header vocabulary; this helper owns the
/// shared card, message-body, and tool-use-section nesting.
pub(crate) fn tool_card(class: Classes, header: Html, body: Html) -> Html {
    html! {
        <div {class}>
            { tool_card_contents(header, body) }
        </div>
    }
}

pub(crate) fn keyed_tool_card(class: Classes, key: Key, header: Html, body: Html) -> Html {
    html! {
        <div {key} {class}>
            { tool_card_contents(header, body) }
        </div>
    }
}

fn tool_card_contents(header: Html, body: Html) -> Html {
    html! {
        <div class="message-body">
            <div class="tool-use-section">
                { header }
                { body }
            </div>
        </div>
    }
}

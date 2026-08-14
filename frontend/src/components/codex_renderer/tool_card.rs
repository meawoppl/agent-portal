use yew::prelude::*;

pub(super) fn tool_card(
    icon: &str,
    name: String,
    status: Option<Html>,
    body: Html,
    completed: bool,
) -> Html {
    let header = html! {
        <div class="tool-use-header">
            <span class="tool-icon">{ icon }</span>
            <span class="tool-name">{ name }</span>
            { status.map(|status| html! { <span class="tool-meta">{ status }</span> }).unwrap_or_default() }
        </div>
    };
    crate::components::tool_card::tool_card(
        classes!(super::item_card_classes(completed)),
        header,
        body,
    )
}

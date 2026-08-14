use yew::prelude::*;

/// Codex-specific re-export of the shared tool-card chrome. The shared
/// `crate::components::tool_card::tool_card` now owns the wrapper so the
/// `codex-item-in-progress` pulse changes in one place (see
/// `frontend/src/components/tool_card.rs`).
pub(super) fn tool_card(
    icon: &str,
    name: String,
    status: Option<Html>,
    body: Html,
    completed: bool,
) -> Html {
    crate::components::tool_card::tool_card(icon, name, status, body, completed)
}

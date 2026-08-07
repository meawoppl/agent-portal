mod dispatch;
mod group_renderer;
mod grouping;
mod renderers;
#[cfg(test)]
mod tests;
pub mod turn_metrics_footer;
pub mod types;
pub use types::RenderedMessage;

use std::collections::HashMap;
use uuid::Uuid;
use yew::prelude::*;

use dispatch::FrameRenderContext;
pub use group_renderer::MessageGroupRenderer;
pub use grouping::{group_is_turn_terminator, group_messages, thinking_chip_starts};

/// Format an already-extracted `PortalMeta.created_at` ISO string as local time.
/// Takes an already-extracted `PortalMeta.created_at` value rather than raw
/// message JSON so the renderer only reads the typed sidecar once.
pub(super) fn local_timestamp(iso: &str) -> Option<String> {
    let ms = js_sys::Date::parse(iso);
    if ms.is_nan() {
        return None;
    }
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    date.to_locale_string("default", &js_sys::Object::new())
        .as_string()
}

// --- Components ---

#[derive(Properties, PartialEq)]
pub struct MessageRendererProps {
    pub message: RenderedMessage,
    pub session_id: Uuid,
    #[prop_or_default]
    pub agent_type: shared::AgentType,
    #[prop_or_default]
    pub current_user_id: Option<String>,
    /// Per-turn metrics for the terminator card this `MessageRenderer` is
    /// rendering, if any. Populated by `SessionView::view()` when the
    /// message is the Nth `Result` / `turn.completed` / `turn.failed` and
    /// `SessionView.turn_metrics` has an Nth entry. The renderer ignores it
    /// for non-terminator shapes; terminator renderers (`render_result_message`
    /// for Claude, the dispatch arm for `CodexEvent::TurnCompleted` /
    /// `TurnFailed` for Codex) append a `<div class="turn-metrics-footer">`
    /// chip strip below the existing stats bar when present.
    #[prop_or_default]
    pub turn_metrics: Option<shared::TurnMetrics>,
    #[prop_or_default]
    pub continuation_statuses: HashMap<Uuid, String>,
    #[prop_or_default]
    pub on_schedule_continuation: Callback<Uuid>,
}

#[function_component(MessageRenderer)]
pub fn message_renderer(props: &MessageRendererProps) -> Html {
    let raw_iso = props.message.raw_iso();
    let ts = raw_iso.and_then(local_timestamp);
    dispatch::render_frame(FrameRenderContext {
        message: &props.message,
        agent_type: props.agent_type,
        session_id: props.session_id,
        timestamp: ts.as_deref(),
        current_user_id: props.current_user_id.as_deref(),
        turn_metrics: props.turn_metrics.as_ref(),
        continuation_statuses: &props.continuation_statuses,
        on_schedule_continuation: props.on_schedule_continuation.clone(),
    })
}

// --- Utility functions (used by renderers and tool_renderers) ---

pub(crate) fn shorten_model_name(model: &str) -> Option<String> {
    if model.is_empty() || model.starts_with('<') {
        return None;
    }

    let extract_version = |model: &str| -> Option<String> {
        let parts: Vec<&str> = model.split('-').collect();
        for i in 0..parts.len().saturating_sub(1) {
            let minor_digits: String = parts[i + 1]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if minor_digits.len() >= 8 {
                continue;
            }
            if let (Ok(major), Ok(minor)) = (parts[i].parse::<u32>(), minor_digits.parse::<u32>()) {
                if parts[i + 1].len() >= 8 {
                    continue;
                }
                return Some(format!("{}.{}", major, minor));
            }
        }
        None
    };

    // Single-part versions (e.g. `claude-fable-5`): a lone short numeric
    // segment, skipping 8-digit date stamps.
    let extract_major = |model: &str| -> Option<String> {
        model
            .split('-')
            .filter(|p| !p.is_empty() && p.len() < 8)
            .find(|p| p.chars().all(|c| c.is_ascii_digit()))
            .map(|p| p.to_string())
    };

    const FAMILIES: [(&str, &str); 5] = [
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
        ("fable", "Fable"),
        ("mythos", "Mythos"),
    ];

    let family = FAMILIES
        .iter()
        .find(|(needle, _)| model.contains(needle))
        .map(|(_, name)| *name);

    Some(match family {
        Some(name) => match extract_version(model).or_else(|| extract_major(model)) {
            Some(v) => format!("{} {}", name, v),
            None => name.to_string(),
        },
        None => model.split('-').next().unwrap_or(model).to_string(),
    })
}

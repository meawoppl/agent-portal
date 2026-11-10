//! Display metadata from the app-server item envelope. Keep it beside the
//! exec-compatible payload: normalizing the payload must not discard clocks
//! or the SDK's typed message phase.
use codex_codes::protocol::{MessagePhase, ThreadItem, ThreadItemPayload};
use serde::{Deserialize, Serialize};
use yew::prelude::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexItemMetadata {
    #[serde(
        rename = "startedAtMs",
        alias = "started_at_ms",
        skip_serializing_if = "Option::is_none"
    )]
    pub started_at_ms: Option<i64>,
    #[serde(
        rename = "completedAtMs",
        alias = "completed_at_ms",
        skip_serializing_if = "Option::is_none"
    )]
    pub completed_at_ms: Option<i64>,
    #[serde(
        rename = "durationMs",
        alias = "duration_ms",
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<MessagePhase>,
}

impl CodexItemMetadata {
    pub fn has_values(&self) -> bool {
        self.started_at_ms.is_some()
            || self.completed_at_ms.is_some()
            || self.duration_ms.is_some()
            || self.phase.is_some()
    }

    pub fn from_app_server(item: &ThreadItem) -> Self {
        Self {
            started_at_ms: item.started_at_ms,
            completed_at_ms: item.completed_at_ms,
            duration_ms: match &item.payload {
                ThreadItemPayload::CommandExecution { duration_ms, .. }
                | ThreadItemPayload::McpToolCall { duration_ms, .. }
                | ThreadItemPayload::DynamicToolCall { duration_ms, .. } => *duration_ms,
                _ => None,
            },
            phase: match &item.payload {
                ThreadItemPayload::AgentMessage { phase, .. } => phase.clone(),
                _ => None,
            },
        }
    }

    pub fn elapsed_ms(&self) -> Option<i64> {
        self.duration_ms
            .filter(|duration| *duration >= 0)
            .or_else(|| {
                self.completed_at_ms?
                    .checked_sub(self.started_at_ms?)
                    .filter(|duration| *duration >= 0)
            })
    }

    pub fn phase_class(&self) -> Option<&'static str> {
        match self.phase.as_ref()? {
            MessagePhase::Commentary => Some("codex-commentary"),
            MessagePhase::FinalAnswer | MessagePhase::PartialAnswer => Some("codex-answer"),
        }
    }
}

fn duration_label(duration: i64) -> String {
    if duration < 1000 {
        format!("took {duration}ms")
    } else {
        format!("took {:.1}s", duration as f64 / 1000.0)
    }
}

pub(super) fn with_item_metadata(
    content: Option<Html>,
    metadata: &CodexItemMetadata,
    completed: bool,
    show_duration: bool,
) -> Option<Html> {
    let content = content?;
    let duration = (completed && show_duration)
        .then(|| metadata.elapsed_ms())
        .flatten();
    if metadata.phase_class().is_none() && duration.is_none() {
        return Some(content);
    }
    Some(html! {
        <div class={classes!("codex-item-metadata", metadata.phase_class())}>
            { content }
            { duration.map(|duration| html! { <div class="codex-item-duration">{ duration_label(duration) }</div> }).unwrap_or_default() }
        </div>
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_use_explicit_value_or_valid_lifecycle_delta() {
        let mut metadata = CodexItemMetadata {
            started_at_ms: Some(1000),
            completed_at_ms: Some(4200),
            ..Default::default()
        };
        assert_eq!(metadata.elapsed_ms(), Some(3200));
        assert_eq!(duration_label(metadata.elapsed_ms().unwrap()), "took 3.2s");
        metadata.duration_ms = Some(350);
        assert_eq!(duration_label(metadata.elapsed_ms().unwrap()), "took 350ms");
        metadata.duration_ms = Some(-1);
        metadata.completed_at_ms = Some(500);
        assert_eq!(metadata.elapsed_ms(), None);
        metadata.started_at_ms = Some(i64::MIN);
        metadata.completed_at_ms = Some(i64::MAX);
        assert_eq!(metadata.elapsed_ms(), None);
    }
    #[test]
    fn commentary_is_muted_and_both_answer_phases_are_answers() {
        for (phase, class) in [
            (MessagePhase::Commentary, "codex-commentary"),
            (MessagePhase::PartialAnswer, "codex-answer"),
            (MessagePhase::FinalAnswer, "codex-answer"),
        ] {
            let metadata = CodexItemMetadata {
                phase: Some(phase),
                ..Default::default()
            };
            assert_eq!(metadata.phase_class(), Some(class));
        }
    }
}

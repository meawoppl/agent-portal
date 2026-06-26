use super::dispatch;
use super::grouping::{
    extract_raw_iso, thinking_tokens_estimate, visible_group_indices, GroupCategory, MessageGroup,
};
use super::local_timestamp;
use std::collections::HashMap;
use uuid::Uuid;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct MessageGroupRendererProps {
    pub group: MessageGroup,
    pub session_id: Uuid,
    #[prop_or_default]
    pub agent_type: shared::AgentType,
    #[prop_or_default]
    pub current_user_id: Option<String>,
    /// Per-turn metrics for the terminator card in this group, if the group
    /// is a `Single` carrying a terminator and the SessionView has a matching
    /// metrics entry. Forwarded to the inner `MessageRenderer` for the
    /// `Single` variant only — `IdentityGroup`s never contain terminator
    /// shapes (`Result` / `turn.completed` always render as `Single`).
    #[prop_or_default]
    pub turn_metrics: Option<shared::TurnMetrics>,
    #[prop_or_default]
    pub continuation_statuses: HashMap<Uuid, String>,
    #[prop_or_default]
    pub on_schedule_continuation: Callback<Uuid>,
    /// Odometer seed for `Thinking` groups: the running thinking-token max
    /// across earlier bursts in the same turn (see
    /// `grouping::thinking_chip_starts`). Keeps the count continuous when a
    /// tool call splits a thinking run instead of re-racing each chip from 0.
    #[prop_or(0)]
    pub thinking_start: i64,
}

#[function_component(MessageGroupRenderer)]
pub fn message_group_renderer(props: &MessageGroupRendererProps) -> Html {
    match &props.group {
        MessageGroup::Single(json) => {
            html! { <super::MessageRenderer message={json.clone()} session_id={props.session_id} agent_type={props.agent_type} current_user_id={props.current_user_id.clone()} turn_metrics={props.turn_metrics.clone()} continuation_statuses={props.continuation_statuses.clone()} on_schedule_continuation={props.on_schedule_continuation.clone()} /> }
        }
        MessageGroup::IdentityGroup {
            category,
            label,
            badge_class,
            messages,
        } => {
            let ts = messages
                .first()
                .and_then(extract_raw_iso)
                .and_then(|iso| local_timestamp(&iso));

            // A run of `thinking_tokens` markers collapses to a single compact
            // chip: the `thinking` badge plus an odometer climbing to the run's
            // running thinking-token estimate. No body — these markers carry
            // none. Each marker reports the cumulative estimate, so the chip
            // ticks upward live as more markers stream in.
            if *category == GroupCategory::Thinking {
                let tokens = thinking_tokens_estimate(messages);
                // Seed the odometer with the previous burst's max from this
                // turn so a run split by a tool call continues counting
                // instead of re-racing from 0 (clamped inside CountUp, so a
                // lower-than-seed target renders statically, never reversed).
                let start = props.thinking_start;
                return html! {
                    <div class="claude-message thinking-pulse-group" title={ts.unwrap_or_default()}>
                        <div class="message-header">
                            <span class="message-type-badge thinking">{ "thinking" }</span>
                            if tokens > 0 {
                                <span class="message-count" title={format!("~{} thinking tokens", tokens)}>
                                    <crate::components::CountUp target={tokens} {start} suffix={" tokens"} compact={true} />
                                </span>
                            }
                        </div>
                    </div>
                };
            }

            let last_iso = messages.last().and_then(extract_raw_iso);
            let wrapper_class = match category {
                GroupCategory::User => "user-message",
                GroupCategory::Portal => "portal-message",
                GroupCategory::Assistant | GroupCategory::Codex => "assistant-message",
                // Handled above with an early return; arm kept for exhaustiveness.
                GroupCategory::Thinking => "assistant-message",
            };
            let visible = visible_group_indices(*category, messages);
            let visible_count = visible.len();
            html! {
                <div class={classes!("claude-message", wrapper_class)}>
                    <div class="message-header" title={ts.unwrap_or_default()}>
                        <span class={classes!("message-type-badge", badge_class.clone())}>{ label }</span>
                        if visible_count > 1 {
                            <span class="message-count" title={format!("{} consecutive messages", visible_count)}>
                                { format!("× {}", visible_count) }
                            </span>
                        }
                    </div>
                    <div class="message-body grouped-message-body">
                        { for visible.iter().map(|&i| {
                            let message = &messages[i];
                            let key = extract_raw_iso(message)
                                .map(|iso| format!("m-{}", iso))
                                .unwrap_or_else(|| format!("m{}", i));
                            html! { <div {key} class="grouped-message-part">{ dispatch::render_identity_group_part(message, props.agent_type, props.session_id, &props.continuation_statuses, props.on_schedule_continuation.clone()) }</div> }
                        })}
                    </div>
                    if let Some(iso) = last_iso {
                        <div class="message-footer">
                            <crate::components::time_ago::TimeAgo iso={iso} />
                        </div>
                    }
                </div>
            }
        }
    }
}

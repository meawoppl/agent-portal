use super::dispatch;
use super::grouping::{
    thinking_tokens_estimate, visible_group_indices, GroupCategory, MessageGroup,
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
    /// Ephemeral records for this Muse turn. Replayed after persisted records
    /// so the one card advances live without writing token deltas to history.
    #[prop_or_default]
    pub muse_live_events: Vec<serde_json::Value>,
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
                .and_then(|message| message.raw_iso())
                .and_then(local_timestamp);

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

            // A run of muse journal records renders as ONE task-tree card:
            // the group's records replay through the reducer and the tree
            // draws once, instead of ~100 raw-JSON bubbles per turn. This is
            // also what makes reload work — grouping runs over persisted
            // history, so the tree rebuilds from the transcript itself.
            if *category == GroupCategory::Muse {
                let mut tree = crate::components::muse_renderer::TaskTree::default();
                for message in messages {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) {
                        tree.apply(&value);
                    }
                }
                // Muse's classifier routes each record to exactly one side:
                // Durable → persisted messages, Ephemeral → this overlay.
                // Applying both sets cannot double-count output chunks unless
                // that classifier invariant changes.
                for event in &props.muse_live_events {
                    tree.apply(event);
                }
                // Nothing structural and nothing to footnote — a group of
                // records the reducer consumed without visible effect (e.g.
                // pure identity bookkeeping) renders no card at all.
                if tree.is_empty() && tree.other_records().next().is_none() {
                    return html! {};
                }
                return html! {
                    <div class="claude-message muse-message muse-task-card" title={ts.unwrap_or_default()}>
                        <div class="message-header">
                            <span class="message-type-badge muse">{ "Muse" }</span>
                        </div>
                        <div class="message-body">
                            { crate::components::muse_renderer::render_task_tree(&tree) }
                        </div>
                    </div>
                };
            }

            let wrapper_class = match category {
                GroupCategory::User => "user-message",
                GroupCategory::Portal => "portal-message",
                GroupCategory::Assistant | GroupCategory::Codex => "assistant-message",
                // Handled above with an early return; arm kept for exhaustiveness.
                GroupCategory::Thinking | GroupCategory::Muse => "assistant-message",
            };
            let visible = visible_group_indices(*category, messages);
            // Render each member first, dropping the ones that produce nothing
            // (empty assistant/user bodies, empty tool results, etc.) so we
            // never emit an empty `grouped-message-part` — a zero-height flex
            // item that the body's `gap` still spaces into a blank row.
            let parts: Vec<Html> = visible
                .iter()
                .filter_map(|(i, item_id)| {
                    let i = *i;
                    let message = &messages[i];
                    let content = dispatch::render_identity_group_part(
                        message,
                        props.agent_type,
                        props.session_id,
                        &props.continuation_statuses,
                        props.on_schedule_continuation.clone(),
                    )?;
                    // Prefer the codex item id: it is stable across the
                    // item's whole lifecycle, whereas the surviving message's
                    // timestamp changes each time a later frame wins the dedup
                    // — recreating the card and collapsing anything expanded
                    // inside it.
                    let key = item_id
                        .as_ref()
                        .map(|id| format!("i-{id}"))
                        .or_else(|| message.raw_iso().map(|iso| format!("m-{iso}")))
                        .unwrap_or_else(|| format!("m{i}"));
                    Some(html! { <div {key} class="grouped-message-part">{ content }</div> })
                })
                .collect();
            // Every member rendered empty → collapse the whole group card
            // rather than show a header over an empty body.
            if parts.is_empty() {
                return html! {};
            }
            let visible_count = parts.len();
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
                        { for parts.into_iter() }
                    </div>
                </div>
            }
        }
    }
}

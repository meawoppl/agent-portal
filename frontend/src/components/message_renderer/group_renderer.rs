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
    #[prop_or_default]
    pub on_claude_login: Option<Callback<()>>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReconnectReason {
    ServerRestart,
    UnexpectedDisconnect,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReconnectNotice {
    pub duration: Option<String>,
    pub reason: ReconnectReason,
}

/// The reconnect notices in a group whose members are *all* reconnect notices,
/// or `None` if any member is something else.
///
/// An idle session reconnects on a slow loop, so these arrive as a long run of
/// otherwise-identical one-liners. Collapsing the run to a single line keeps
/// inactive sessions from turning routine portal restarts into a transcript
/// wall. Older transcripts used full markdown text cards, so this recognizes
/// both the typed `ConnectionCycle` variant and historical `Proxy reconnected`
/// prose.
pub(super) fn reconnect_notice_run(
    messages: &[super::types::RenderedMessage],
) -> Option<Vec<ReconnectNotice>> {
    let mut notices = Vec::with_capacity(messages.len());
    for message in messages {
        let portal: shared::PortalMessage = serde_json::from_str(&message.content).ok()?;
        notices.push(reconnect_notice_from_portal(&portal)?);
    }
    (!notices.is_empty()).then_some(notices)
}

fn reconnect_notice_from_portal(portal: &shared::PortalMessage) -> Option<ReconnectNotice> {
    match portal.content.as_slice() {
        [shared::PortalContent::ConnectionCycle { duration }] => Some(ReconnectNotice {
            duration: duration.clone(),
            reason: ReconnectReason::ServerRestart,
        }),
        [shared::PortalContent::Text { text }] => reconnect_notice_from_text(text),
        _ => None,
    }
}

fn reconnect_notice_from_text(text: &str) -> Option<ReconnectNotice> {
    let first_line = text.lines().next()?.trim();
    let rest = first_line
        .strip_prefix("**Proxy reconnected**")
        .or_else(|| first_line.strip_prefix("Proxy reconnected"))?;
    let duration = rest
        .trim_start()
        .strip_prefix("after ")
        .map(|after| {
            after
                .split_once(" (")
                .map_or(after, |(before_reason, _)| before_reason)
                .split_once(" —")
                .map_or_else(
                    || after.trim().to_string(),
                    |(before_context, _)| before_context.trim().to_string(),
                )
        })
        .filter(|duration| !duration.is_empty());
    let reason = if first_line.contains("(server restart)") {
        ReconnectReason::ServerRestart
    } else if first_line.contains("(unexpected disconnect)") {
        ReconnectReason::UnexpectedDisconnect
    } else {
        ReconnectReason::Unknown
    };
    Some(ReconnectNotice { duration, reason })
}

fn duration_sort_key(duration: &str) -> Option<u64> {
    let mut total_ms = 0_u64;
    let mut parsed_any = false;
    for part in duration.split_whitespace() {
        if let Some(raw) = part.strip_suffix("ms") {
            total_ms = total_ms.checked_add(raw.parse::<u64>().ok()?)?;
            parsed_any = true;
        } else if let Some(raw) = part.strip_suffix('s') {
            total_ms = total_ms.checked_add(raw.parse::<u64>().ok()?.checked_mul(1_000)?)?;
            parsed_any = true;
        } else if let Some(raw) = part.strip_suffix('m') {
            total_ms = total_ms.checked_add(raw.parse::<u64>().ok()?.checked_mul(60_000)?)?;
            parsed_any = true;
        }
    }
    parsed_any.then_some(total_ms)
}

fn duration_range(durations: &[&str]) -> Option<String> {
    let mut seen = durations
        .iter()
        .copied()
        .filter(|d| !d.is_empty())
        .collect::<Vec<_>>();
    if seen.is_empty() {
        return None;
    }
    seen.sort_unstable_by(|a, b| {
        duration_sort_key(a)
            .cmp(&duration_sort_key(b))
            .then_with(|| a.cmp(b))
    });
    seen.dedup();
    match (seen.first(), seen.last()) {
        (Some(lo), Some(hi)) if lo == hi => Some((*lo).to_string()),
        (Some(lo), Some(hi)) => Some(format!("{lo}-{hi}")),
        _ => None,
    }
}

fn reason_suffix(notices: &[ReconnectNotice]) -> &'static str {
    if notices
        .iter()
        .all(|notice| notice.reason == ReconnectReason::UnexpectedDisconnect)
    {
        " (unexpected disconnects)"
    } else if notices
        .iter()
        .any(|notice| notice.reason == ReconnectReason::UnexpectedDisconnect)
    {
        " (some unexpected disconnects)"
    } else {
        ""
    }
}

/// One line for a whole run: `reconnected 4x after 36s-38s`.
pub(super) fn render_reconnect_notice_run(notices: &[ReconnectNotice]) -> Html {
    let durations = notices
        .iter()
        .filter_map(|notice| notice.duration.as_deref())
        .collect::<Vec<_>>();
    let label = match notices {
        [] => return html! {},
        [only] => match only.duration.as_deref() {
            Some(duration) if !duration.is_empty() => {
                format!("reconnected after {}{}", duration, reason_suffix(notices))
            }
            _ => format!("reconnected{}", reason_suffix(notices)),
        },
        many => {
            // Durations arrive newest-last and are near-identical; show the
            // span rather than repeating one value N times.
            match duration_range(&durations) {
                Some(range) => {
                    format!(
                        "reconnected {}x after {}{}",
                        many.len(),
                        range,
                        reason_suffix(notices)
                    )
                }
                None => format!("reconnected {}x{}", many.len(), reason_suffix(notices)),
            }
        }
    };
    html! {
        <div class="connection-cycle">
            <span class="connection-cycle-dot" />
            { label }
        </div>
    }
}

#[function_component(MessageGroupRenderer)]
pub fn message_group_renderer(props: &MessageGroupRendererProps) -> Html {
    match &props.group {
        MessageGroup::Single(json) => {
            html! { <super::MessageRenderer message={json.clone()} session_id={props.session_id} agent_type={props.agent_type} current_user_id={props.current_user_id.clone()} turn_metrics={props.turn_metrics.clone()} continuation_statuses={props.continuation_statuses.clone()} on_schedule_continuation={props.on_schedule_continuation.clone()} on_claude_login={props.on_claude_login.clone()} /> }
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

            // A run of reconnect notices collapses to one line.
            if *category == GroupCategory::Portal {
                if let Some(notices) = reconnect_notice_run(messages) {
                    return html! {
                        <div class="claude-message portal-message" title={ts.unwrap_or_default()}>
                            <div class="message-body">
                                { render_reconnect_notice_run(&notices) }
                            </div>
                        </div>
                    };
                }
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

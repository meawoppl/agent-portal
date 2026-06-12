use serde_json::Value;

use super::renderers::assistant_label;
use super::types::{user_meta_from_json, ClaudeMessage};

/// Extract the raw `_created_at` ISO string from a message JSON, for use with
/// the live-updating TimeAgo component (which parses it itself).
pub(super) fn extract_raw_iso(json: &str) -> Option<String> {
    let val: Value = serde_json::from_str(json).ok()?;
    val.get("_created_at")?.as_str().map(|s| s.to_string())
}

/// Category for a run of consecutive related messages — drives both the
/// grouping decision (`classify`) and the wrapper style on the rendered
/// group (`MessageGroupRenderer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCategory {
    /// Assistant messages and the user-shaped envelopes that carry only
    /// tool results back to the agent.
    Assistant,
    /// Consecutive portal messages (connect/disconnect notices, retry
    /// announcements, codex raw-frame attachments, etc.).
    Portal,
    /// Consecutive plain-text user messages typed by the human. Excludes
    /// the tool-result user envelopes which group with Assistant.
    User,
    /// Consecutive Codex protocol events (any non-Unknown `CodexEvent`).
    Codex,
    /// Consecutive `system`/`thinking_tokens` markers emitted by the Claude CLI.
    /// These carry no renderable body — the portal collapses a run of them into
    /// a single compact `thinking × N` chip instead of one empty badge each.
    Thinking,
}

impl GroupCategory {
    /// Short stable prefix for `MessageGroup::key`. Don't change without
    /// understanding the Yew diff implications — these strings end up in
    /// virtual-dom keys and switching them mid-flight would re-mount every
    /// group component on the page.
    fn key_prefix(self) -> &'static str {
        match self {
            GroupCategory::Assistant => "g",
            GroupCategory::Portal => "p",
            GroupCategory::User => "u",
            GroupCategory::Codex => "x",
            GroupCategory::Thinking => "t",
        }
    }
}

/// A group of messages to render together.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageGroup {
    /// A single message that doesn't classify into any group category.
    /// Kept as a distinct variant (rather than a one-element `Grouped`) so
    /// the most common case avoids the group-wrapper render path and keeps
    /// its Yew key stable independent of category.
    Single(String),
    /// One or more consecutive messages with the same display identity.
    IdentityGroup {
        category: GroupCategory,
        label: String,
        badge_class: String,
        messages: Vec<String>,
    },
}

impl MessageGroup {
    /// Stable key for this group derived from the first message's identity.
    ///
    /// A positional index would change whenever an earlier group gets added
    /// or removed, causing Yew to throw away the group component and reset
    /// internal state of every expandable/collapsible inside it (bash
    /// command toggle, `ExpandableText`, image viewer, etc.). Using the
    /// first message's `_created_at` keeps the key stable across reorderings.
    /// `index` is used only as a fallback when no timestamp is present.
    pub fn key(&self, index: usize) -> yew::virtual_dom::Key {
        let (prefix, first) = match self {
            MessageGroup::Single(json) => ("s", json.as_str()),
            MessageGroup::IdentityGroup {
                category, messages, ..
            } => match messages.first() {
                Some(j) => (category.key_prefix(), j.as_str()),
                None => {
                    return yew::virtual_dom::Key::from(format!(
                        "{}{}",
                        category.key_prefix(),
                        index
                    ));
                }
            },
        };
        match extract_raw_iso(first) {
            Some(iso) => yew::virtual_dom::Key::from(format!("{}-{}", prefix, iso)),
            None => yew::virtual_dom::Key::from(format!("{}{}", prefix, index)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MessageIdentity {
    pub(super) category: GroupCategory,
    label: String,
    badge_class: String,
}

/// True iff the parsed `UserMessage` carries only tool-result content blocks
/// — the user-shaped envelope Claude uses to deliver tool output back to the
/// assistant turn. The decision is made on the **nested** `message.content`
/// blocks alone — we deliberately do NOT short-circuit on the top-level
/// `content` field, because that field is the optimistic-send envelope shape
/// and can leak onto real echoes through the cross-process wire wrapping.
/// Gating on it broke serial Read tool-use grouping in the wild (#758).
fn user_is_tool_result_envelope(msg: &shared::UserMessage) -> bool {
    let blocks = &msg.message.content;
    !blocks.is_empty()
        && blocks.iter().all(|b| {
            matches!(
                b,
                shared::ContentBlock::ToolResult(_)
                    | shared::ContentBlock::WebSearchToolResult(_)
                    | shared::ContentBlock::McpToolResult(_)
                    | shared::ContentBlock::CodeExecutionToolResult(_)
            )
        })
}

/// True iff the parsed `UserMessage` is a plain-text human prompt — the kind
/// we want to roll into the User group. Two wire shapes carry this content:
/// the optimistic-send envelope (`UserMessage.content: Some(String)`) and the
/// Claude echo shape (`UserMessage.message.content: Some([Text { .. }, …])`).
/// We deliberately require *all* nested blocks to be `Text` so we don't
/// silently roll a tool-result envelope into User — those belong with
/// Assistant and are caught by `user_is_tool_result_envelope`.
fn user_is_plain_text(msg: &shared::UserMessage) -> bool {
    let blocks = &msg.message.content;
    !blocks.is_empty()
        && blocks
            .iter()
            .all(|b| matches!(b, shared::ContentBlock::Text(_)))
}

/// Classify a single wire message into the display identity it belongs to,
/// or `None` if it shouldn't roll into any group (renders as `Single`).
///
/// Sole entry point for "which identity group does this message belong to"
/// across the codebase — add new categories here, not at the `group_messages`
/// loop level. The wire JSON is parsed at most twice: once as a
/// `ClaudeMessage`, and if (and only if) that parse yields no recognized
/// variant on a Codex session, once as a `CodexEvent`.
///
/// **Variant ordering matters**:
///   1. **User-as-tool-result** runs first because user-tool-result envelopes
///      are user-shaped but belong with the surrounding assistant turn. If
///      the plain-text branch ran first, every Read tool-result would
///      silently land in a User group instead of continuing the assistant
///      run (the regression target of PR 1 of #758).
///   2. **Assistant** is the other half of the assistant group — covered by
///      the same `Assistant` category that user-tool-result envelopes map to.
///   3. **Portal** has its own wire shape so it can't collide with the User
///      arms, but matching it explicitly keeps the dispatch documented.
///   4. **User plain-text** runs after the tool-result branch so prose lands
///      in the User group while tool-result envelopes are already claimed.
///      The sender label is part of the group key, so proxy users don't
///      collapse under the current user's "You" header.
///   5. **Codex** runs last — Codex events parse via a different enum and
///      only the messages that don't match any Claude shape get here.
pub(super) fn classify(
    json: &str,
    agent_type: shared::AgentType,
    current_user_id: Option<&str>,
) -> Option<MessageIdentity> {
    let assistant_identity_label = if agent_type == shared::AgentType::Codex {
        "Codex"
    } else {
        "Claude"
    };

    match ClaudeMessage::parse(json) {
        Ok(ClaudeMessage::Assistant(_)) => {
            return Some(MessageIdentity {
                category: GroupCategory::Assistant,
                label: assistant_identity_label.to_string(),
                badge_class: "assistant".to_string(),
            });
        }
        Ok(ClaudeMessage::User(msg)) => {
            if user_is_tool_result_envelope(&msg) {
                return Some(MessageIdentity {
                    category: GroupCategory::Assistant,
                    label: assistant_identity_label.to_string(),
                    badge_class: "assistant".to_string(),
                });
            }
            if user_is_plain_text(&msg) {
                let meta = user_meta_from_json(json);
                let label = match &meta.sender {
                    Some(sender) if current_user_id != Some(sender.user_id.as_str()) => {
                        sender.name.clone()
                    }
                    _ => "You".to_string(),
                };
                return Some(MessageIdentity {
                    category: GroupCategory::User,
                    label,
                    badge_class: "user".to_string(),
                });
            }
        }
        Ok(ClaudeMessage::OptimisticUser(msg)) => {
            let label = match &msg.sender {
                Some(sender) if current_user_id != Some(sender.user_id.as_str()) => {
                    sender.name.clone()
                }
                _ => "You".to_string(),
            };
            return Some(MessageIdentity {
                category: GroupCategory::User,
                label,
                badge_class: "user".to_string(),
            });
        }
        Ok(ClaudeMessage::Portal(_)) => {
            return Some(MessageIdentity {
                category: GroupCategory::Portal,
                label: "Portal".to_string(),
                badge_class: "portal".to_string(),
            });
        }
        // The Claude CLI emits a bodyless `system`/`thinking_tokens` marker per
        // reasoning step; a long turn produces a wall of them. Fold a run into
        // one `Thinking` group so the renderer can show a single counted chip.
        Ok(ClaudeMessage::System(msg)) if msg.subtype.as_str() == "thinking_tokens" => {
            return Some(MessageIdentity {
                category: GroupCategory::Thinking,
                label: "thinking".to_string(),
                badge_class: "thinking".to_string(),
            });
        }
        _ => {}
    }

    if agent_type == shared::AgentType::Codex {
        use crate::components::codex_renderer::CodexEvent;
        if !matches!(
            serde_json::from_str::<CodexEvent>(json),
            Err(_) | Ok(CodexEvent::Unknown)
        ) {
            return Some(MessageIdentity {
                category: GroupCategory::Codex,
                label: "Codex".to_string(),
                badge_class: "assistant".to_string(),
            });
        }
    }

    None
}

/// Peak `estimated_tokens` across a run of `thinking_tokens` markers — the
/// burst's running thinking-token total. Each marker reports the *cumulative*
/// estimate so far (`50` → `150` → `250` …), so the maximum (last) value is the
/// total for the run; returns `0` when none parse.
///
/// `estimated_tokens` lives in the flattened `SystemMessage.data` because
/// claude-codes 2.1.141 models `thinking_tokens` as `SystemSubtype::Unknown`
/// (no typed fields). TODO(SDK): push a typed `thinking_tokens` system variant
/// upstream to `rust-code-agent-sdks` so this reads a field instead of poking
/// `data`.
pub(super) fn thinking_tokens_estimate(messages: &[String]) -> i64 {
    messages
        .iter()
        .filter_map(|json| match ClaudeMessage::parse(json) {
            Ok(ClaudeMessage::System(msg)) => {
                msg.data.get("estimated_tokens").and_then(|v| v.as_i64())
            }
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn group_label(identity: &MessageIdentity, messages: &[String]) -> String {
    if identity.category != GroupCategory::Assistant || identity.label == "Codex" {
        return identity.label.clone();
    }

    messages
        .iter()
        .filter_map(|json| ClaudeMessage::parse(json).ok())
        .find_map(|msg| match msg {
            ClaudeMessage::Assistant(msg) => Some(assistant_label(&msg.message.model)),
            _ => None,
        })
        .unwrap_or_else(|| identity.label.clone())
}

/// True iff `json` parses as a per-turn terminator: Claude's
/// `ClaudeMessage::Result` or one of Codex's terminator events
/// (`TurnCompleted` / `TurnFailed`).
///
/// Used by `SessionView::view()` to pair the Nth terminator card in the
/// rendered transcript with the Nth row in `SessionView.turn_metrics`. The
/// pair-by-ordering join is the agreed PR 2 strategy — `user_message_id`
/// stays `None` on the proxy-emit side until a future PR wires up the
/// per-turn linkage, so a key-based join would fail on every row today.
pub fn is_turn_terminator(json: &str) -> bool {
    // Claude side: a parsed `ClaudeMessage::Result` is the only terminator.
    // The `User`/`Portal`/`Error`/etc. variants are not terminators (and
    // there's no Codex `Result` shape — Codex terminators parse through
    // `CodexEvent` instead).
    if let Ok(ClaudeMessage::Result(_)) = ClaudeMessage::parse(json) {
        return true;
    }
    // Codex side: terminators are `TurnCompleted` and `TurnFailed`. The
    // explicit match (rather than a substring sniff on the wire) keeps
    // this in lockstep with the typed enum so a renamed variant fails
    // loudly at compile time.
    use crate::components::codex_renderer::CodexEvent;
    matches!(
        serde_json::from_str::<CodexEvent>(json),
        Ok(CodexEvent::TurnCompleted { .. }) | Ok(CodexEvent::TurnFailed { .. })
    )
}

/// True iff the group is a `Single` carrying a turn terminator. Identity
/// groups never contain terminators (Result / TurnCompleted / TurnFailed
/// don't classify into any identity category — see `classify`), so this
/// helper only needs to inspect the `Single` arm.
pub fn group_is_turn_terminator(group: &MessageGroup) -> bool {
    match group {
        MessageGroup::Single(json) => is_turn_terminator(json),
        MessageGroup::IdentityGroup { .. } => false,
    }
}

/// Walk `messages` and collapse consecutive same-category runs into
/// `MessageGroup::IdentityGroup`. Mixed / `None` messages become
/// `MessageGroup::Single`.
pub fn group_messages(
    messages: &[String],
    agent_type: shared::AgentType,
    current_user_id: Option<&str>,
) -> Vec<MessageGroup> {
    let mut groups = Vec::new();
    let mut current: Option<(MessageIdentity, Vec<String>)> = None;

    fn flush(out: &mut Vec<MessageGroup>, slot: &mut Option<(MessageIdentity, Vec<String>)>) {
        if let Some((identity, messages)) = slot.take() {
            let label = group_label(&identity, &messages);
            out.push(MessageGroup::IdentityGroup {
                category: identity.category,
                label,
                badge_class: identity.badge_class,
                messages,
            });
        }
    }

    for json in messages {
        // Cumulative `turn/diff/updated` events are dropped entirely — Codex
        // re-sends the whole-turn diff on every edit tick, so they pile up
        // O(ticks) redundant cards (each the size of the full turn) on top of
        // the per-file diffs that already show the same edits. Skipping here
        // rather than in `classify` keeps the surrounding Codex events in one
        // run instead of fragmenting the group around each dropped diff.
        if agent_type == shared::AgentType::Codex {
            use crate::components::codex_renderer::CodexEvent;
            if matches!(
                serde_json::from_str::<CodexEvent>(json),
                Ok(CodexEvent::TurnDiffUpdated { .. })
            ) {
                continue;
            }
        }
        match classify(json, agent_type, current_user_id) {
            Some(identity) => match current.as_mut() {
                Some((cur_identity, msgs)) if *cur_identity == identity => msgs.push(json.clone()),
                _ => {
                    flush(&mut groups, &mut current);
                    current = Some((identity, vec![json.clone()]));
                }
            },
            None => {
                flush(&mut groups, &mut current);
                groups.push(MessageGroup::Single(json.clone()));
            }
        }
    }

    flush(&mut groups, &mut current);
    groups
}

/// For Codex groups, suppress earlier events that share an `item_id` with a
/// later event in the same group — they represent the same logical card being
/// progressively filled in (`item.started` → `item.updated` → `item.completed`),
/// and rendering all of them creates duplicate near-identical cards (#776 — a
/// bash command would show up as a "running" card immediately followed by a
/// near-identical "completed" card). Non-Codex categories pass through
/// unchanged because their wire shapes don't carry the same lifecycle.
///
/// Events that don't carry an `item_id` (turn-level events, deltas, errors)
/// always pass through — they're standalone signals, not lifecycle stages of
/// a per-item card.
pub(super) fn visible_group_indices(category: GroupCategory, messages: &[String]) -> Vec<usize> {
    if !matches!(category, GroupCategory::Codex) {
        return (0..messages.len()).collect();
    }
    use crate::components::codex_renderer::codex_event_item_id;
    use std::collections::HashMap;
    // Parse each message's item_id once, then resolve the last index per id
    // from the cached vector — parsing twice per message doubled the JSON
    // work on the hot render path (#967).
    let ids: Vec<Option<String>> = messages.iter().map(|j| codex_event_item_id(j)).collect();
    let mut last_idx: HashMap<&str, usize> = HashMap::new();
    for (i, id) in ids.iter().enumerate() {
        if let Some(id) = id {
            last_idx.insert(id.as_str(), i);
        }
    }
    ids.iter()
        .enumerate()
        .filter(|(i, id)| match id {
            Some(id) => last_idx.get(id.as_str()) == Some(i),
            None => true,
        })
        .map(|(i, _)| i)
        .collect()
}

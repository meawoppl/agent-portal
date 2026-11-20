use crate::components::agent_frame::{AgentFrame, AgentFrameKind, AgentFrameRegistry};
use crate::components::codex_renderer::codex_event_item_id;

use std::rc::Rc;

use super::renderers::assistant_label;
use super::types::{ClaudeMessage, RenderedMessage};

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
    /// Consecutive muse journal records. A live turn journals ~100 of them;
    /// the group renders as ONE task-tree card (see `MessageGroupRenderer`)
    /// instead of one raw-JSON bubble each.
    Muse,
    /// Consecutive Agent Portal-authored Antigravity preview frames.
    Antigravity,
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
            GroupCategory::Muse => "m",
            GroupCategory::Antigravity => "a",
        }
    }
}

/// A group of messages to render together.
/// `Eq` matters beyond tidiness: it lets `Rc<MessageGroup>` compare by
/// pointer before comparing contents, which is what keeps an unchanged
/// group's props comparison O(1) when the transcript re-renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageGroup {
    /// A single message that doesn't classify into any group category.
    /// Kept as a distinct variant (rather than a one-element `Grouped`) so
    /// the most common case avoids the group-wrapper render path and keeps
    /// its Yew key stable independent of category.
    Single(RenderedMessage),
    /// One or more consecutive messages with the same display identity.
    IdentityGroup {
        category: GroupCategory,
        label: String,
        badge_class: String,
        messages: Vec<RenderedMessage>,
    },
}

impl MessageGroup {
    pub(crate) fn muse_causation_id(&self) -> Option<String> {
        let MessageGroup::IdentityGroup {
            category: GroupCategory::Muse,
            messages,
            ..
        } = self
        else {
            return None;
        };
        messages.iter().find_map(|message| {
            message
                .facts(shared::AgentType::Muse)
                .muse_causation_id
                .clone()
        })
    }

    /// Stable key for this group derived from the first message's identity.
    ///
    /// A positional index would change whenever an earlier group gets added
    /// or removed, causing Yew to throw away the group component and reset
    /// internal state of every expandable/collapsible inside it (bash
    /// command toggle, `ExpandableText`, image viewer, etc.). Using the
    /// first message's `PortalMeta.created_at` keeps the key stable across
    /// reorderings.
    /// `index` is used only as a fallback when no timestamp is present.
    pub fn key(&self, index: usize) -> yew::virtual_dom::Key {
        let (prefix, first) = match self {
            MessageGroup::Single(message) => ("s", message),
            MessageGroup::IdentityGroup {
                category, messages, ..
            } => match messages.first() {
                Some(j) => (category.key_prefix(), j),
                None => {
                    return yew::virtual_dom::Key::from(format!(
                        "{}{}",
                        category.key_prefix(),
                        index
                    ));
                }
            },
        };
        if let Some(id) = first.secret_request_id() {
            return yew::virtual_dom::Key::from(format!("secret-request-{id}"));
        }
        match first.raw_iso() {
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

fn source_identity(
    source: &shared::MessageSource,
    current_user_id: Option<&str>,
) -> MessageIdentity {
    match source {
        shared::MessageSource::Human { account_id, name } => {
            let account_id = account_id.to_string();
            let label = if current_user_id == Some(account_id.as_str()) {
                "You".to_string()
            } else {
                name.clone()
            };
            MessageIdentity {
                category: GroupCategory::User,
                label,
                badge_class: "user".to_string(),
            }
        }
        shared::MessageSource::Agent {
            session_id,
            agent_type,
        } => {
            let short = session_id
                .to_string()
                .split('-')
                .next()
                .unwrap_or_default()
                .to_string();
            let label = match agent_type.parse::<shared::AgentType>() {
                Ok(agent) => format!("Message from {} ({short})", agent.display_name()),
                Err(_) => format!("Message from agent ({short})"),
            };
            MessageIdentity {
                category: GroupCategory::User,
                label,
                badge_class: "other-agent".to_string(),
            }
        }
        shared::MessageSource::Portal => MessageIdentity {
            category: GroupCategory::Portal,
            label: "Portal".to_string(),
            badge_class: "portal".to_string(),
        },
    }
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

/// True iff `text` is a claude-echoed inter-agent message
/// (`[message from <agent> <uuid>]\n…`). Centralized detector so the two
/// echo shapes and the single-card renderer can't drift.
fn is_inter_agent_text(text: &str) -> bool {
    super::renderers::agent_message_event_from_agent_facing_text(text).is_some()
}

/// True iff a plain-text user message is actually a claude-echoed inter-agent
/// message. See [`is_inter_agent_text`].
fn user_message_is_inter_agent(msg: &shared::UserMessage) -> bool {
    let text = msg
        .message
        .content
        .iter()
        .filter_map(|b| match b {
            shared::ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    is_inter_agent_text(&text)
}

/// True iff a synthetic optimistic-user echo is actually an inter-agent
/// message. See [`is_inter_agent_text`].
fn optimistic_user_is_inter_agent(msg: &shared::UserFrame) -> bool {
    is_inter_agent_text(&msg.content)
}

/// Classify a single wire message into the display identity it belongs to,
/// or `None` if it shouldn't roll into any group (renders as `Single`).
///
/// Messages sent by another agent or by the portal take their sender's
/// identity. Everything else follows the frame's cached [`IdentityRule`]
/// (see [`identity_rule`] for the dispatch order), resolved against the
/// message's source and the current user.
pub(super) fn classify(
    message: &RenderedMessage,
    agent_type: shared::AgentType,
    current_user_id: Option<&str>,
) -> Option<MessageIdentity> {
    let source = message.source();
    if let Some(source @ (shared::MessageSource::Agent { .. } | shared::MessageSource::Portal)) =
        source
    {
        return Some(source_identity(source, current_user_id));
    }
    match &message.facts(agent_type).identity {
        IdentityRule::Fixed(identity) => Some(identity.clone()),
        IdentityRule::SourceOrYou => Some(source.map_or_else(
            || MessageIdentity {
                category: GroupCategory::User,
                label: "You".to_string(),
                badge_class: "user".to_string(),
            },
            |source| source_identity(source, current_user_id),
        )),
        IdentityRule::SourceOnly => source.map(|source| source_identity(source, current_user_id)),
        IdentityRule::Never => None,
    }
}

/// How a frame's display identity is decided, independent of who is
/// viewing: the current user only changes a sender's label ("You" or their
/// name), so that last step is resolved in [`classify`] against the message's
/// own source rather than cached.
#[derive(Debug, Clone)]
pub(super) enum IdentityRule {
    /// The frame's shape alone decides its identity.
    Fixed(MessageIdentity),
    /// A human prompt: the sender's identity, or "You" with no source.
    SourceOrYou,
    /// No identity of its own; the sender's identity when there is one.
    SourceOnly,
    /// Always renders on its own (inter-agent echoes).
    Never,
}

/// Everything grouping and the transcript view derive from one wire frame,
/// computed from a single parse and cached on the [`RenderedMessage`] (see
/// [`RenderedMessage::facts`]). Every helper here used to re-parse the JSON,
/// several times per message on every render.
#[derive(Debug)]
pub(super) struct FrameFacts {
    pub(crate) kind: AgentFrameKind,
    pub(crate) identity: IdentityRule,
    /// Header label for a Claude assistant frame, from its model.
    pub(crate) assistant_label: Option<String>,
    /// `estimated_tokens` of a `thinking_tokens` marker.
    pub(crate) thinking_tokens: Option<i64>,
    /// Codex item id, for collapsing an item's lifecycle frames.
    pub(crate) codex_item_id: Option<String>,
    /// Muse `causation_id`, which joins live records to a turn.
    pub(crate) muse_causation_id: Option<String>,
    /// Whether the frame counts toward the live transcript's render budget
    /// (same rule as `shared::render_budget::counts_toward_render_limit`).
    pub(crate) counts_toward_render_limit: bool,
    /// Transport churn the transcript drops (see [`is_transcript_noise`]).
    pub(crate) transcript_noise: bool,
    /// What a command-lifecycle frame contributes to the transcript:
    /// `Some(command_uuid)` for a started scheduler wakeup (shown once per
    /// command), `None` for the plain acknowledgements that are hidden.
    pub(crate) lifecycle_wakeup: Option<Option<String>>,
    /// Background-task bookkeeping: a `task_started` or a `task_notification`
    /// frame, keyed by `task_id` (see [`finished_task_starts`]).
    pub(crate) task: Option<TaskLifecycle>,
}

/// One run of a Claude background task. A resumed task keeps its `task_id`
/// and gets a new `run_id`, so the pair is what ties a start to its end.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TaskRun {
    task_id: String,
    run_id: Option<String>,
}

/// A Claude background-task frame that bounds a run.
#[derive(Debug)]
pub(crate) enum TaskLifecycle {
    Started {
        run: TaskRun,
        description: String,
    },
    /// A `task_notification` with a terminal status (completed, failed,
    /// killed, stopped). Progress notifications don't end a run. `shown` is
    /// the text its card displays (`TaskReport::text`): `None` for a withheld
    /// or unreported handback, the handback report where there is one, and
    /// the raw summary only without a handback.
    Ended {
        run: TaskRun,
        shown: Option<String>,
    },
}

impl FrameFacts {
    pub(crate) fn of(json: &str, agent_type: shared::AgentType) -> Self {
        let frame = AgentFrameRegistry::parse(json, agent_type);
        let kind = frame.kind();
        let thinking_tokens = match &frame {
            AgentFrame::Claude(ClaudeMessage::System(msg)) => msg
                .as_thinking_tokens()
                .map(|tokens| tokens.estimated_tokens.min(i64::MAX as u64) as i64),
            _ => None,
        };
        let is_thinking_marker = matches!(
            &frame,
            AgentFrame::Claude(ClaudeMessage::System(msg)) if msg.is_thinking_tokens()
        );
        Self {
            kind,
            identity: identity_rule(&frame, agent_type),
            assistant_label: match &frame {
                AgentFrame::Claude(ClaudeMessage::Assistant(msg)) => {
                    Some(assistant_label(&msg.message.model))
                }
                _ => None,
            },
            thinking_tokens,
            codex_item_id: match &frame {
                AgentFrame::Codex(event) => codex_event_item_id(event),
                _ => None,
            },
            muse_causation_id: match &frame {
                AgentFrame::Muse(value) => value
                    .get("causation_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string),
                _ => None,
            },
            counts_toward_render_limit: !is_thinking_marker
                && kind != AgentFrameKind::ClaudeCommandLifecycle,
            transcript_noise: is_transcript_noise(&frame),
            lifecycle_wakeup: match &frame {
                AgentFrame::Claude(ClaudeMessage::CommandLifecycle(message)) => Some(
                    super::renderers::command_wakeup_label(message)
                        .map(|_| message.command_uuid.clone()),
                ),
                _ => None,
            },
            task: match &frame {
                AgentFrame::Claude(ClaudeMessage::System(msg)) => msg
                    .as_task_started()
                    .map(|started| TaskLifecycle::Started {
                        run: TaskRun {
                            task_id: started.task_id,
                            run_id: started.run_id,
                        },
                        description: started.description,
                    })
                    .or_else(|| {
                        msg.as_task_notification()
                            .filter(|ended| {
                                matches!(
                                    ended.status,
                                    shared::TaskStatus::Completed
                                        | shared::TaskStatus::Failed
                                        | shared::TaskStatus::Killed
                                        | shared::TaskStatus::Stopped
                                )
                            })
                            .map(|ended| TaskLifecycle::Ended {
                                shown:
                                    crate::components::task_report::TaskReport::from_notification(
                                        &ended,
                                    )
                                    .text
                                    .map(str::to_string),
                                run: TaskRun {
                                    task_id: ended.task_id,
                                    run_id: ended.run_id,
                                },
                            })
                    }),
                _ => None,
            },
        }
    }
}

/// Indices of `task_started` frames made redundant by their run's completion,
/// so a finished task renders as one card, the way a Codex item's
/// started/completed frames collapse. A start is hidden only when:
/// - a later terminal `task_notification` closes the same run (`task_id` and
///   `run_id`), so a late or replayed completion of one run can't hide a
///   resumed run, and progress notifications never close anything; and
/// - the completion card displays the start's description, so hiding the
///   start loses nothing. A differing text (a sub-agent's report) or one the
///   card doesn't show at all (a withheld handback) keeps both cards.
fn finished_task_starts(
    messages: &[RenderedMessage],
    agent_type: shared::AgentType,
) -> std::collections::HashSet<usize> {
    let mut ended_later = std::collections::HashMap::<TaskRun, Option<String>>::new();
    let mut finished = std::collections::HashSet::new();
    for (i, message) in messages.iter().enumerate().rev() {
        match &message.facts(agent_type).task {
            Some(TaskLifecycle::Ended { run, shown }) => {
                ended_later.insert(run.clone(), shown.clone());
            }
            // The guard always consumes the run's completion, so it pairs
            // with this start even when the texts differ.
            Some(TaskLifecycle::Started { run, description })
                if ended_later
                    .remove(run)
                    .flatten()
                    .is_some_and(|shown| shown.trim() == description.trim()) =>
            {
                finished.insert(i);
            }
            _ => {}
        }
    }
    finished
}

/// The identity rule for a parsed frame. Sole place that decides "which
/// identity group does this message belong to"; add new categories here, not
/// at the `group_messages` loop level. Messages from another agent or the
/// portal itself are decided by their source before this runs (see
/// [`classify`]).
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
fn identity_rule(frame: &AgentFrame, agent_type: shared::AgentType) -> IdentityRule {
    let assistant = || {
        IdentityRule::Fixed(MessageIdentity {
            category: GroupCategory::Assistant,
            label: match agent_type {
                shared::AgentType::Codex => "Codex",
                shared::AgentType::Muse => "Muse",
                shared::AgentType::Antigravity => "Antigravity",
                shared::AgentType::Claude => "Claude",
            }
            .to_string(),
            badge_class: "assistant".to_string(),
        })
    };
    let fixed = |category, label: &str, badge_class: &str| {
        IdentityRule::Fixed(MessageIdentity {
            category,
            label: label.to_string(),
            badge_class: badge_class.to_string(),
        })
    };
    match frame {
        AgentFrame::Claude(ClaudeMessage::Assistant(_)) => assistant(),
        AgentFrame::Claude(ClaudeMessage::User(msg)) => {
            if user_is_tool_result_envelope(msg) {
                assistant()
            } else if user_is_plain_text(msg) {
                // A claude-echoed inter-agent message (`[message from …]`) must
                // render as its own "Message from …" card, not merge into the
                // "You" group and show its raw `[message from …]` / system-
                // reminder wrapper, so it never groups.
                //
                // Deliberately NOT gated on the message having no source: the
                // backend stamps every role="user" row with `meta.source =
                // User(owner)` — including claude's echo of an injected
                // inter-agent message — so a source-gated check silently skips
                // exactly the claude→claude case. If the text parses as an
                // inter-agent wrapper, the raw form is never the right render,
                // whatever the source.
                if user_message_is_inter_agent(msg) {
                    IdentityRule::Never
                } else {
                    IdentityRule::SourceOrYou
                }
            } else {
                IdentityRule::SourceOnly
            }
        }
        // A synthetic user echo carrying an inter-agent `[message from …]`
        // payload must render as its own provenance card, mirroring the
        // `ClaudeMessage::User` arm above. This shape is how Codex's
        // `UserEchoEvent` (top-level `content` string, no `session_id`) and
        // older proxies' echoes parse, so the `User`-arm detection alone
        // missed them.
        AgentFrame::Claude(ClaudeMessage::OptimisticUser(msg)) => {
            if optimistic_user_is_inter_agent(msg) {
                IdentityRule::Never
            } else {
                IdentityRule::SourceOrYou
            }
        }
        AgentFrame::Claude(ClaudeMessage::Portal(_)) => {
            fixed(GroupCategory::Portal, "Portal", "portal")
        }
        // The Claude CLI emits a bodyless `system`/`thinking_tokens` marker per
        // reasoning step; a long turn produces a wall of them. Fold a run into
        // one `Thinking` group so the renderer can show a single counted chip.
        AgentFrame::Claude(ClaudeMessage::System(msg)) if msg.is_thinking_tokens() => {
            fixed(GroupCategory::Thinking, "thinking", "thinking")
        }
        AgentFrame::Codex(_) => fixed(GroupCategory::Codex, "Codex", "assistant"),
        AgentFrame::Muse(_) => fixed(GroupCategory::Muse, "Muse", "assistant"),
        AgentFrame::Antigravity(_) => {
            fixed(GroupCategory::Antigravity, "Antigravity", "antigravity")
        }
        _ => IdentityRule::SourceOnly,
    }
}

/// Peak `estimated_tokens` across a run of `thinking_tokens` markers — the
/// burst's running thinking-token total. Each marker reports the *cumulative*
/// estimate so far (`50` → `150` → `250` …), so the maximum (last) value is the
/// total for the run; returns `0` when none parse.
///
/// Reads through `claude-codes`' typed `ThinkingTokensMessage` accessor so the
/// portal follows the SDK's schema instead of poking at flattened JSON fields.
pub(super) fn thinking_tokens_estimate(
    messages: &[RenderedMessage],
    agent_type: shared::AgentType,
) -> i64 {
    messages
        .iter()
        .filter_map(|message| message.facts(agent_type).thinking_tokens)
        .max()
        .unwrap_or(0)
}

fn group_label(
    identity: &MessageIdentity,
    messages: &[RenderedMessage],
    agent_type: shared::AgentType,
) -> String {
    if identity.category != GroupCategory::Assistant || identity.label == "Codex" {
        return identity.label.clone();
    }

    messages
        .iter()
        .find_map(|message| message.facts(agent_type).assistant_label.clone())
        .unwrap_or_else(|| identity.label.clone())
}

/// True iff `json` parses as a per-turn terminator: Claude's
/// `ClaudeMessage::Result`, Codex's `TurnCompleted`/`TurnFailed`, or
/// Muse's `run.terminal.completed`/`run.terminal.failed` (via
/// `AgentFrameKind::is_terminator`).
///
/// Used by `SessionView::view()` to pair the Nth terminator card in the
/// rendered transcript with the Nth row in `SessionView.turn_metrics`. The
/// pair-by-ordering join is the agreed PR 2 strategy — `user_message_id`
/// stays `None` on the proxy-emit side until a future PR wires up the
/// per-turn linkage, so a key-based join would fail on every row today.
pub fn is_turn_terminator(message: &RenderedMessage, agent_type: shared::AgentType) -> bool {
    message.facts(agent_type).kind.is_terminator()
}

/// True iff the group is a `Single` carrying a turn terminator. Identity
/// groups never contain terminators (Result / TurnCompleted / TurnFailed /
/// run.terminal.completed|failed don't classify into any identity category
/// — see `classify`), so this helper only needs to inspect the `Single` arm.
pub fn group_is_turn_terminator(group: &MessageGroup, agent_type: shared::AgentType) -> bool {
    match group {
        MessageGroup::Single(json) => is_turn_terminator(json, agent_type),
        MessageGroup::IdentityGroup { .. } => false,
    }
}

/// Per-group odometer seed for the `thinking` chips: for each group, the
/// running `thinking_tokens` maximum across *earlier* groups in the same
/// turn (0 for non-`Thinking` groups).
///
/// The Claude CLI's `estimated_tokens` is cumulative across a turn, but a
/// run of markers gets split into separate `Thinking` groups whenever a
/// tool call or assistant message lands between them. Each split mounts a
/// fresh chip whose odometer would otherwise re-race from 0 up to the full
/// cumulative total — visually "resetting" the count after every tool use,
/// including the final one before the answer. Seeding each chip with the
/// previous burst's max keeps the count continuous across splits. The
/// running max resets at turn terminators so the next turn's first chip
/// starts from 0 again. Callers clamp the seed to the chip's own target,
/// so a marker stream that ever restarts low degrades to a static display
/// rather than a backwards animation.
pub fn thinking_chip_starts<G: std::borrow::Borrow<MessageGroup>>(
    groups: &[G],
    agent_type: shared::AgentType,
) -> Vec<i64> {
    let mut running_max = 0i64;
    groups
        .iter()
        .map(|group| match group.borrow() {
            MessageGroup::IdentityGroup {
                category: GroupCategory::Thinking,
                messages,
                ..
            } => {
                let start = running_max;
                running_max = running_max.max(thinking_tokens_estimate(messages, agent_type));
                start
            }
            _ => {
                if group_is_turn_terminator(group.borrow(), agent_type) {
                    running_max = 0;
                }
                0
            }
        })
        .collect()
}

fn is_transcript_noise(frame: &AgentFrame) -> bool {
    match frame {
        AgentFrame::Claude(ClaudeMessage::System(msg)) => {
            msg.subtype.as_str() == "commands_changed"
        }
        AgentFrame::Claude(ClaudeMessage::Portal(portal)) => {
            super::group_renderer::is_reconnect_notice(portal)
        }
        _ => false,
    }
}

/// Share each freshly grouped `MessageGroup` that is unchanged from the
/// previous grouping, matched by its stable key, by reusing the previous `Rc`.
/// Renderers compare group props by pointer first, so a regroup after one new
/// message re-renders only the group that changed (and any new one) instead
/// of deep-comparing every group.
pub fn reuse_unchanged_groups(
    previous: &[Rc<MessageGroup>],
    fresh: Vec<MessageGroup>,
) -> Vec<Rc<MessageGroup>> {
    let previous: std::collections::HashMap<_, _> = previous
        .iter()
        .enumerate()
        .map(|(i, group)| (group.key(i), group))
        .collect();
    fresh
        .into_iter()
        .enumerate()
        .map(|(i, group)| match previous.get(&group.key(i)) {
            Some(old) if ***old == group => Rc::clone(old),
            _ => Rc::new(group),
        })
        .collect()
}

/// Walk `messages` and collapse consecutive same-category runs into
/// `MessageGroup::IdentityGroup`. Mixed / `None` messages become
/// `MessageGroup::Single`.
pub fn group_messages(
    messages: &[RenderedMessage],
    agent_type: shared::AgentType,
    current_user_id: Option<&str>,
) -> Vec<MessageGroup> {
    let mut groups = Vec::new();
    let mut current: Option<(MessageIdentity, Vec<RenderedMessage>)> = None;
    let mut seen_wakeups = std::collections::HashSet::new();
    let mut seen_secret_requests = std::collections::HashSet::new();

    fn flush(
        out: &mut Vec<MessageGroup>,
        slot: &mut Option<(MessageIdentity, Vec<RenderedMessage>)>,
        agent_type: shared::AgentType,
    ) {
        if let Some((identity, messages)) = slot.take() {
            let label = group_label(&identity, &messages, agent_type);
            out.push(MessageGroup::IdentityGroup {
                category: identity.category,
                label,
                badge_class: identity.badge_class,
                messages,
            });
        }
    }

    let finished_starts = finished_task_starts(messages, agent_type);
    for (index, message) in messages.iter().enumerate() {
        if finished_starts.contains(&index) {
            continue;
        }
        // Approval cards own state and async work. Give each request a stable
        // parent independent of neighboring Portal runs, and tolerate replay.
        if let Some(id) = message.secret_request_id() {
            if seen_secret_requests.insert(id) {
                flush(&mut groups, &mut current, agent_type);
                groups.push(MessageGroup::Single(message.clone()));
            }
            continue;
        }
        let facts = message.facts(agent_type);

        // Transport churn with nothing for the reader: reconnect notices and
        // Claude's `commands_changed` marker (re-sent on every reconnect).
        // Dropping them here, not in `classify`, keeps the surrounding run in
        // one group instead of splitting it around each hidden frame.
        if facts.transcript_noise {
            continue;
        }
        let kind = facts.kind;

        // Show exactly one started scheduler fire per command; ordinary
        // lifecycle acknowledgements neither create nor split transcript rows.
        if let Some(wakeup) = &facts.lifecycle_wakeup {
            match wakeup {
                Some(command_uuid) if seen_wakeups.insert(command_uuid.clone()) => {}
                _ => continue,
            }
        }

        // Cumulative `turn/diff/updated` events are dropped entirely — Codex
        // re-sends the whole-turn diff on every edit tick, so they pile up
        // O(ticks) redundant cards (each the size of the full turn) on top of
        // the per-file diffs that already show the same edits. Skipping here
        // rather than in `classify` keeps the surrounding Codex events in one
        // run instead of fragmenting the group around each dropped diff.
        if matches!(kind, AgentFrameKind::CodexTurnDiffUpdated) {
            continue;
        }
        match classify(message, agent_type, current_user_id) {
            Some(identity) => match current.as_mut() {
                Some((cur_identity, msgs)) if *cur_identity == identity => {
                    msgs.push(message.clone())
                }
                _ => {
                    flush(&mut groups, &mut current, agent_type);
                    current = Some((identity, vec![message.clone()]));
                }
            },
            None => {
                flush(&mut groups, &mut current, agent_type);
                groups.push(MessageGroup::Single(message.clone()));
            }
        }
    }

    flush(&mut groups, &mut current, agent_type);
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
/// Returns each surviving index paired with the codex `item_id` it renders,
/// when it has one.
///
/// The id is handed back rather than recomputed by the caller for two reasons.
/// It avoids a second parse of every message on the hot render path (#967) —
/// and, more importantly, it is the card's **stable identity**. Keying the
/// rendered card on the surviving message's timestamp instead means the key
/// changes every time a later lifecycle frame for the same item wins the
/// dedup, so Yew tears the card down and rebuilds it — resetting every
/// expandable inside, which is how an expanded bash-output preview closed
/// itself as a turn progressed.
pub(super) fn visible_group_indices(
    category: GroupCategory,
    messages: &[RenderedMessage],
) -> Vec<(usize, Option<String>)> {
    if !matches!(category, GroupCategory::Codex) {
        return (0..messages.len()).map(|i| (i, None)).collect();
    }
    use std::collections::HashMap;
    // Codex groups only form in Codex sessions, so the frames' cached facts
    // are keyed by that agent type.
    let ids: Vec<Option<String>> = messages
        .iter()
        .map(|message| {
            message
                .facts(shared::AgentType::Codex)
                .codex_item_id
                .clone()
        })
        .collect();
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
        .map(|(i, id)| (i, id.clone()))
        .collect()
}

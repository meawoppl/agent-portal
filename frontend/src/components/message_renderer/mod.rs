mod dispatch;
mod group_renderer;
mod grouping;
mod renderers;
pub mod turn_metrics_footer;
pub mod types;
pub use types::RenderedMessage;

use std::collections::HashMap;
use uuid::Uuid;
use yew::prelude::*;

use dispatch::FrameRenderContext;
pub use group_renderer::MessageGroupRenderer;
#[cfg(test)]
use grouping::classify;
use grouping::extract_raw_iso;
pub use grouping::{group_is_turn_terminator, group_messages, thinking_chip_starts};
#[cfg(test)]
use grouping::{visible_group_indices, GroupCategory, MessageGroup};

/// Format an already-extracted `PortalMeta.created_at` ISO string as local time.
/// Takes the `extract_raw_iso` result rather than the raw JSON so the
/// message string is parsed once per render, not once per consumer.
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
    let raw_iso = extract_raw_iso(&props.message);
    let ts = raw_iso.as_deref().and_then(local_timestamp);
    dispatch::render_frame(FrameRenderContext {
        message: &props.message,
        agent_type: props.agent_type,
        session_id: props.session_id,
        timestamp: ts.as_deref(),
        raw_iso: raw_iso.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(json: impl Into<String>) -> RenderedMessage {
        RenderedMessage::new(json.into(), None)
    }

    fn rendered_vec(messages: &[String]) -> Vec<RenderedMessage> {
        messages.iter().cloned().map(rendered).collect()
    }

    fn classify_category(json: &str) -> Option<GroupCategory> {
        classify(&rendered(json), shared::AgentType::Claude, None).map(|identity| identity.category)
    }

    fn classify_codex_category(json: &str) -> Option<GroupCategory> {
        classify(&rendered(json), shared::AgentType::Codex, None).map(|identity| identity.category)
    }

    fn group_for_tests(messages: &[String]) -> Vec<MessageGroup> {
        group_messages(&rendered_vec(messages), shared::AgentType::Claude, None)
    }

    fn group_for_codex_tests(messages: &[String]) -> Vec<MessageGroup> {
        group_messages(&rendered_vec(messages), shared::AgentType::Codex, None)
    }

    /// A `system`/`thinking_tokens` marker — the bodyless per-reasoning-step
    /// event the Claude CLI emits, which the portal collapses into one chip.
    /// `estimated_tokens` is the cumulative running thinking-token estimate.
    fn thinking_tokens_message(estimated_tokens: i64) -> String {
        serde_json::json!({
            "type": "system",
            "subtype": "thinking_tokens",
            "estimated_tokens": estimated_tokens,
            "estimated_tokens_delta": estimated_tokens,
            "session_id": "01890000-0000-7000-8000-000000000001",
            "uuid": format!("01890000-0000-7000-8000-{estimated_tokens:012}"),
        })
        .to_string()
    }

    /// Realistic Claude wire shape for a user message containing a single
    /// `tool_result` content block (the kind Read / Bash / Edit etc. produce).
    /// Matches `claude-codes` 2.1.x `ClaudeOutput::User(UserMessage)`
    /// serialization with portal metadata carried out-of-band in `PortalMeta`.
    fn read_tool_result_user_message(tool_use_id: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": "file contents...",
                }]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string()
    }

    /// Realistic Claude assistant message with a single `tool_use` block.
    fn assistant_with_tool_use(tool_use_id: &str, tool_name: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "id": format!("msg_{tool_use_id}"),
                "role": "assistant",
                "model": "claude-sonnet-4-5-20250929",
                "content": [{
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": tool_name,
                    "input": {"file_path": "/some/path"},
                }]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string()
    }

    /// A tool-result user message coming from a Claude session MUST classify
    /// into the assistant group — otherwise serial Read tool uses don't roll
    /// together with their preceding assistant turn.
    ///
    /// This is the regression target for the "serial Read tool uses don't
    /// group" symptom on Claude sessions.
    #[test]
    fn user_tool_result_classifies_with_assistant() {
        let user_tool_result = read_tool_result_user_message("toolu_01abc");
        assert_eq!(
            classify_category(&user_tool_result),
            Some(GroupCategory::Assistant),
            "user-tool-result message should classify into Assistant"
        );
    }

    /// Sanity: two consecutive (assistant tool_use + user tool_result) pairs
    /// must collapse into a single assistant identity group of length 4. If the
    /// classifier above is broken, this falls apart.
    #[test]
    fn serial_read_tool_uses_collapse_into_one_group() {
        let messages = vec![
            assistant_with_tool_use("toolu_01", "Read"),
            read_tool_result_user_message("toolu_01"),
            assistant_with_tool_use("toolu_02", "Read"),
            read_tool_result_user_message("toolu_02"),
        ];
        let groups = group_for_tests(&messages);
        assert_eq!(
            groups.len(),
            1,
            "expected one Assistant identity group carrying all 4 messages, got {} groups",
            groups.len()
        );
        match &groups[0] {
            MessageGroup::IdentityGroup {
                category: GroupCategory::Assistant,
                messages,
                ..
            } => assert_eq!(messages.len(), 4),
            other => panic!("expected an Assistant identity run, got {:?}", other),
        }
    }

    /// Edge case: top-level `content` field on a user-tool-result message
    /// (e.g. from the optimistic-send envelope leaking onto a real echo)
    /// trips the existing `msg.content.is_some()` early-bail and breaks the
    /// run. This is a candidate root cause for the reported regression on
    /// production Claude sessions even though the canonical wire shape
    /// doesn't carry top-level `content`.
    #[test]
    fn user_tool_result_with_top_level_content_still_groups() {
        let with_top_level_content = serde_json::json!({
            "type": "user",
            "content": "stale optimistic content",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_01",
                    "content": "file contents...",
                }]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string();
        assert_eq!(
            classify_category(&with_top_level_content),
            Some(GroupCategory::Assistant),
            "user-tool-result with a stale top-level `content` field should \
             still classify into Assistant; the dispatch must look at the \
             nested message blocks, not the envelope's top-level field"
        );
    }

    /// PR 2/4 of #758: portal messages must classify together.
    fn portal_text_message(text: &str) -> String {
        serde_json::json!({
            "type": "portal",
            "content": [{"type": "text", "text": text}],
        })
        .to_string()
    }

    #[test]
    fn portal_messages_classify_into_portal_group() {
        let msg = portal_text_message("Connection restored");
        assert_eq!(classify_category(&msg), Some(GroupCategory::Portal));
    }

    #[test]
    fn serial_portal_messages_collapse_into_one_group() {
        let messages = vec![
            portal_text_message("Disconnected at 2026-05-18T05:00:00Z"),
            portal_text_message("Reconnected at 2026-05-18T05:01:00Z"),
            portal_text_message("Codex frame attached"),
        ];
        let groups = group_for_tests(&messages);
        assert_eq!(
            groups.len(),
            1,
            "expected one Portal group, got {} groups",
            groups.len()
        );
        match &groups[0] {
            MessageGroup::IdentityGroup {
                category: GroupCategory::Portal,
                messages,
                ..
            } => assert_eq!(messages.len(), 3),
            other => panic!("expected Portal identity run, got {:?}", other),
        }
    }

    /// An assistant message between two portal messages must split the run —
    /// portal-group only collapses *consecutive* portal messages.
    #[test]
    fn portal_run_breaks_on_intervening_assistant() {
        let messages = vec![
            portal_text_message("first portal"),
            assistant_with_tool_use("toolu_01", "Read"),
            portal_text_message("second portal"),
        ];
        let groups = group_for_tests(&messages);
        assert_eq!(
            groups.len(),
            3,
            "expected 3 groups (Portal, Assistant, Portal), got {}",
            groups.len()
        );
        let cats: Vec<_> = groups
            .iter()
            .map(|g| match g {
                MessageGroup::IdentityGroup { category, .. } => Some(*category),
                MessageGroup::Single(_) => None,
            })
            .collect();
        assert_eq!(
            cats,
            vec![
                Some(GroupCategory::Portal),
                Some(GroupCategory::Assistant),
                Some(GroupCategory::Portal),
            ]
        );
    }

    /// Edge case: real user input (plain text typed by the human, not a
    /// tool result) must NOT join the assistant group, otherwise prose
    /// would silently get rolled into a previous assistant block.
    #[test]
    fn real_user_text_does_not_group_with_assistant() {
        let plain_user = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "hello agent"}]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string();
        assert_eq!(
            classify_category(&plain_user),
            Some(GroupCategory::User),
            "plain-text user message must classify into User, not Assistant"
        );
    }

    // ---- PR 3/4 of #758: User + Codex grouping ----

    fn plain_user_text(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": text}]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string()
    }

    fn plain_user_text_from_sender(text: &str, user_id: Uuid, name: &str) -> RenderedMessage {
        let content = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": text}]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string();
        RenderedMessage::new(
            content,
            Some(shared::PortalMeta {
                created_at: Some("2026-05-17T10:00:00.000Z".to_string()),
                source: Some(shared::MessageSource::Human {
                    account_id: user_id,
                    name: name.to_string(),
                }),
                delivery: None,
            }),
        )
    }

    fn tool_result_from_sender(tool_use_id: &str, user_id: Uuid, name: &str) -> RenderedMessage {
        RenderedMessage::new(
            read_tool_result_user_message(tool_use_id),
            Some(shared::PortalMeta {
                created_at: Some("2026-05-17T10:00:00.000Z".to_string()),
                source: Some(shared::MessageSource::Human {
                    account_id: user_id,
                    name: name.to_string(),
                }),
                delivery: None,
            }),
        )
    }

    fn codex_item_started_agent_message(text: &str) -> String {
        serde_json::json!({
            "type": "item.started",
            "item": {
                "type": "agent_message",
                "id": "item_abc",
                "text": text,
            },
        })
        .to_string()
    }

    /// Lifecycle helper: a CommandExecution event at a given lifecycle stage.
    /// `stage` is one of `"item.started"` / `"item.updated"` / `"item.completed"`.
    /// All three carry the same `item_id`, mirroring the Codex wire flow that
    /// produced the duplicate-card regression of #776.
    ///
    /// `status` must be a real `CommandExecutionStatus` value (`in_progress`,
    /// `completed`, `failed`, `declined`) — upstream `codex-codes` types
    /// are strict here, the pre-#827 local mirror was looser (any string).
    fn codex_command_event(stage: &str, item_id: &str, status: &str) -> String {
        serde_json::json!({
            "type": stage,
            "item": {
                "type": "command_execution",
                "id": item_id,
                "command": "echo hello",
                "status": status,
            },
        })
        .to_string()
    }

    #[test]
    fn plain_text_user_classifies_into_user_group() {
        let msg = plain_user_text("hello agent");
        assert_eq!(classify_category(&msg), Some(GroupCategory::User));
    }

    /// Predicate ordering guard: a tool-result user envelope must STILL go
    /// into Assistant, not User. If `is_plain_text_user` claimed it first,
    /// every Read tool-result on Claude would silently break the assistant
    /// run.
    #[test]
    fn tool_result_user_envelope_stays_in_assistant_group() {
        let msg = read_tool_result_user_message("toolu_01");
        assert_eq!(classify_category(&msg), Some(GroupCategory::Assistant));
    }

    #[test]
    fn tool_result_user_envelope_with_human_source_stays_in_assistant_group() {
        let user_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let msg = tool_result_from_sender("toolu_01", user_id, "Matt");

        assert_eq!(
            classify(&msg, shared::AgentType::Claude, Some(&user_id.to_string()))
                .map(|identity| identity.category),
            Some(GroupCategory::Assistant)
        );
    }

    #[test]
    fn tool_result_user_envelope_with_human_source_renders_in_assistant_group() {
        let user_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let messages = vec![
            rendered(assistant_with_tool_use("toolu_01", "Read")),
            tool_result_from_sender("toolu_01", user_id, "Matt"),
        ];

        let groups = group_messages(
            &messages,
            shared::AgentType::Claude,
            Some(&user_id.to_string()),
        );

        assert_eq!(groups.len(), 1);
        match &groups[0] {
            MessageGroup::IdentityGroup {
                category: GroupCategory::Assistant,
                label,
                messages,
                ..
            } => {
                assert!(label.starts_with("Claude"));
                assert_eq!(messages.len(), 2);
            }
            other => panic!("expected Assistant identity group, got {:?}", other),
        }
    }

    #[test]
    fn serial_user_text_collapses_into_user_group() {
        let messages = vec![
            plain_user_text("first prompt"),
            plain_user_text("follow-up"),
            plain_user_text("one more thing"),
        ];
        let groups = group_for_tests(&messages);
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            MessageGroup::IdentityGroup {
                category: GroupCategory::User,
                messages,
                ..
            } => assert_eq!(messages.len(), 3),
            other => panic!("expected User identity run, got {:?}", other),
        }
    }

    #[test]
    fn user_grouping_splits_by_sender_identity() {
        let user_a = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let user_b = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let messages = vec![
            plain_user_text_from_sender("first from me", user_a, "Matt"),
            plain_user_text_from_sender("second from me", user_a, "Matt"),
            plain_user_text_from_sender("from someone else", user_b, "Alex"),
            plain_user_text_from_sender("back to me", user_a, "Matt"),
        ];

        let current_user_id = user_a.to_string();
        let groups = group_messages(&messages, shared::AgentType::Claude, Some(&current_user_id));
        assert_eq!(groups.len(), 3);

        let labels: Vec<_> = groups
            .iter()
            .map(|group| match group {
                MessageGroup::IdentityGroup {
                    category: GroupCategory::User,
                    label,
                    ..
                } => label.as_str(),
                other => panic!("expected User identity group, got {:?}", other),
            })
            .collect();
        assert_eq!(labels, vec!["You", "Alex", "You"]);

        match &groups[0] {
            MessageGroup::IdentityGroup { messages, .. } => assert_eq!(messages.len(), 2),
            other => panic!("expected first User group, got {:?}", other),
        }
    }

    #[test]
    fn user_run_breaks_on_intervening_assistant() {
        let messages = vec![
            plain_user_text("question one"),
            assistant_with_tool_use("toolu_01", "Read"),
            plain_user_text("question two"),
        ];
        let groups = group_for_tests(&messages);
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn turn_terminator_detection_covers_claude_and_codex() {
        let claude_result = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "duration_ms": 100,
            "duration_api_ms": 80,
            "num_turns": 1,
            "session_id": "01890000-0000-7000-8000-000000000001",
            "total_cost_usd": 0.0,
        })
        .to_string();
        let codex_completed = serde_json::json!({
            "type": "turn.completed",
            "usage": {"input_tokens": 1, "output_tokens": 2},
        })
        .to_string();
        let codex_failed = serde_json::json!({
            "type": "turn.failed",
            "error": {"message": "nope"},
        })
        .to_string();

        for json in [claude_result, codex_completed, codex_failed] {
            assert!(
                group_is_turn_terminator(&MessageGroup::Single(rendered(json))),
                "single terminator frame should be recognized"
            );
        }
        assert!(!group_is_turn_terminator(&MessageGroup::Single(rendered(
            plain_user_text("hello")
        ))));
        assert!(!group_is_turn_terminator(&MessageGroup::IdentityGroup {
            category: GroupCategory::User,
            label: "You".to_string(),
            badge_class: "user".to_string(),
            messages: vec![rendered(plain_user_text("hello"))],
        }));
    }

    /// A run of `thinking_tokens` markers must collapse into a single
    /// `Thinking` group (one counted chip), not one empty badge per marker —
    /// the regression target for the "wall of THINKING_TOKENS badges" symptom.
    #[test]
    fn serial_thinking_tokens_collapse_into_one_group() {
        let messages = vec![
            thinking_tokens_message(50),
            thinking_tokens_message(150),
            thinking_tokens_message(250),
        ];
        let groups = group_for_tests(&messages);
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            MessageGroup::IdentityGroup {
                category: GroupCategory::Thinking,
                messages,
                label,
                ..
            } => {
                assert_eq!(messages.len(), 3);
                assert_eq!(label, "thinking");
            }
            other => panic!("expected Thinking run, got {:?}", other),
        }
    }

    /// The condensed chip shows a token estimate, not a pulse count: each
    /// marker reports the cumulative `estimated_tokens`, so the run's peak
    /// (last) value is the burst total.
    #[test]
    fn thinking_tokens_estimate_returns_peak() {
        let messages = vec![
            thinking_tokens_message(50),
            thinking_tokens_message(150),
            thinking_tokens_message(250),
        ];
        assert_eq!(
            grouping::thinking_tokens_estimate(&rendered_vec(&messages)),
            250
        );
        // No markers / unparseable input yields 0 (chip hides).
        assert_eq!(grouping::thinking_tokens_estimate(&[]), 0);
    }

    /// When a tool call splits a thinking run, the later chip's odometer is
    /// seeded with the earlier burst's peak so the (turn-cumulative) count
    /// continues instead of re-racing from 0. Terminators reset the seed so
    /// the next turn's first chip starts at 0 again.
    #[test]
    fn thinking_chip_starts_seed_across_splits_and_reset_on_terminator() {
        let result_message = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "duration_ms": 100,
            "duration_api_ms": 80,
            "num_turns": 1,
            "session_id": "01890000-0000-7000-8000-000000000001",
            "total_cost_usd": 0.0,
        })
        .to_string();
        let messages = vec![
            // Turn 1, burst 1: climbs to 150.
            thinking_tokens_message(50),
            thinking_tokens_message(150),
            // Tool call splits the run.
            read_tool_result_user_message("toolu_01"),
            // Turn 1, burst 2: cumulative continues to 400.
            thinking_tokens_message(300),
            thinking_tokens_message(400),
            result_message,
            // Turn 2, burst 1: fresh turn, fresh count.
            thinking_tokens_message(60),
        ];
        let groups = group_for_tests(&messages);
        let starts = grouping::thinking_chip_starts(&groups);
        assert_eq!(starts.len(), groups.len());
        // Burst 1 starts at 0; burst 2 is seeded with burst 1's peak; the
        // turn-2 burst starts at 0 again after the Result terminator.
        let thinking_starts: Vec<i64> = groups
            .iter()
            .zip(&starts)
            .filter_map(|(g, s)| match g {
                MessageGroup::IdentityGroup {
                    category: GroupCategory::Thinking,
                    ..
                } => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(thinking_starts, vec![0, 150, 0]);
        // Non-thinking groups carry a 0 seed.
        for (g, s) in groups.iter().zip(&starts) {
            if !matches!(
                g,
                MessageGroup::IdentityGroup {
                    category: GroupCategory::Thinking,
                    ..
                }
            ) {
                assert_eq!(*s, 0);
            }
        }
    }

    #[test]
    fn codex_event_classifies_into_codex_group() {
        let msg = codex_item_started_agent_message("hi");
        assert_eq!(classify_codex_category(&msg), Some(GroupCategory::Codex));
    }

    #[test]
    fn serial_codex_events_collapse_into_codex_group() {
        let messages = vec![
            codex_item_started_agent_message("starting"),
            codex_item_started_agent_message("more progress"),
            codex_item_started_agent_message("done"),
        ];
        let groups = group_for_codex_tests(&messages);
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            MessageGroup::IdentityGroup {
                category: GroupCategory::Codex,
                messages,
                ..
            } => assert_eq!(messages.len(), 3),
            other => panic!("expected Codex identity run, got {:?}", other),
        }
    }

    /// A portal message between two codex events must split the run —
    /// codex-group only collapses *consecutive* codex events.
    #[test]
    fn codex_run_breaks_on_intervening_portal() {
        let messages = vec![
            codex_item_started_agent_message("first"),
            portal_text_message("reconnected"),
            codex_item_started_agent_message("second"),
        ];
        let groups = group_for_codex_tests(&messages);
        assert_eq!(groups.len(), 3);
        let cats: Vec<_> = groups
            .iter()
            .filter_map(|g| match g {
                MessageGroup::IdentityGroup { category, .. } => Some(*category),
                MessageGroup::Single(_) => None,
            })
            .collect();
        assert_eq!(
            cats,
            vec![
                GroupCategory::Codex,
                GroupCategory::Portal,
                GroupCategory::Codex,
            ]
        );
    }

    /// One canonical wire shape per realistic message kind paired with the
    /// `GroupCategory` the classifier MUST return on a Codex session. The
    /// Codex agent type is the strictly-larger surface (Claude shapes
    /// classify identically on both agent types, and Codex events only
    /// classify on a Codex session), so a single Codex-agent sweep covers
    /// the whole table.
    ///
    /// If a new variant lands in `ClaudeMessage` or `CodexEvent`, extend
    /// this table — the classifier is the only place that needs to know
    /// about the new variant.
    #[test]
    fn classifier_exhaustive_over_realistic_messages() {
        let cases: Vec<(&str, String, Option<GroupCategory>)> = vec![
            (
                "assistant tool_use",
                assistant_with_tool_use("toolu_a", "Read"),
                Some(GroupCategory::Assistant),
            ),
            (
                "user tool_result envelope",
                read_tool_result_user_message("toolu_a"),
                Some(GroupCategory::Assistant),
            ),
            (
                "plain-text user prompt",
                plain_user_text("hello"),
                Some(GroupCategory::User),
            ),
            (
                "portal frame",
                portal_text_message("reconnected"),
                Some(GroupCategory::Portal),
            ),
            (
                "codex item.started",
                codex_item_started_agent_message("starting"),
                Some(GroupCategory::Codex),
            ),
            (
                "system message",
                serde_json::json!({
                    "type": "system",
                    "subtype": "init",
                    "session_id": "01890000-0000-7000-8000-000000000001",
                })
                .to_string(),
                None,
            ),
            (
                "system thinking_tokens marker collapses into the Thinking group",
                thinking_tokens_message(150),
                Some(GroupCategory::Thinking),
            ),
            (
                "result message",
                serde_json::json!({
                    "type": "result",
                    "subtype": "success",
                    "is_error": false,
                    "duration_ms": 100,
                    "duration_api_ms": 80,
                    "num_turns": 1,
                    "session_id": "01890000-0000-7000-8000-000000000001",
                    "total_cost_usd": 0.0,
                })
                .to_string(),
                None,
            ),
            (
                "error message: on Codex agent the `{type: error}` shape \
                 also matches `CodexEvent::Error` and lands in the Codex \
                 group, preserved from the pre-refactor classifier",
                serde_json::json!({
                    "type": "error",
                    "message": "oops",
                })
                .to_string(),
                Some(GroupCategory::Codex),
            ),
            ("unparseable garbage", "not even json".to_string(), None),
        ];

        for (label, json, expected) in cases {
            let got = classify(&rendered(json), shared::AgentType::Codex, None).map(|i| i.category);
            assert_eq!(
                got, expected,
                "{label}: classifier returned {got:?}, expected {expected:?}"
            );
        }
    }

    // ---- #776: codex lifecycle dedup ----

    /// `item.started` + `item.completed` for the same `item_id` should collapse
    /// to a single visible card (the completed one), not render as two
    /// near-identical cards. Regression target for #776.
    #[test]
    fn codex_command_lifecycle_dedupes_to_completed() {
        let messages = vec![
            codex_command_event("item.started", "cmd_1", "in_progress"),
            codex_command_event("item.completed", "cmd_1", "completed"),
        ];
        let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
        assert_eq!(
            visible,
            vec![1],
            "expected only the completed event to remain visible (#776), got {:?}",
            visible
        );
    }

    /// A `started → updated → completed` triple for the same item collapses to
    /// the final completed event. The updated stages add nothing visible past
    /// what completed already shows.
    #[test]
    fn codex_command_started_updated_completed_dedupes_to_completed() {
        let messages = vec![
            codex_command_event("item.started", "cmd_1", "in_progress"),
            codex_command_event("item.updated", "cmd_1", "in_progress"),
            codex_command_event("item.completed", "cmd_1", "completed"),
        ];
        let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
        assert_eq!(visible, vec![2]);
    }

    /// Two distinct items in the same group keep their own cards — dedup is
    /// per-`item_id`, never collapses different items together.
    #[test]
    fn codex_two_distinct_items_each_keep_one_card() {
        let messages = vec![
            codex_command_event("item.started", "cmd_a", "in_progress"),
            codex_command_event("item.completed", "cmd_a", "completed"),
            codex_command_event("item.started", "cmd_b", "in_progress"),
            codex_command_event("item.completed", "cmd_b", "completed"),
        ];
        let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
        // Indices 1 (cmd_a completed) and 3 (cmd_b completed) remain.
        assert_eq!(visible, vec![1, 3]);
    }

    /// Non-item events in a codex group (turn-level, deltas, errors) carry no
    /// `item_id` and must always pass through the dedup unchanged — they're
    /// standalone signals, not lifecycle stages.
    #[test]
    fn codex_non_item_events_always_visible() {
        let turn_completed = serde_json::json!({
            "type": "turn.completed",
            "usage": {"input_tokens": 1, "output_tokens": 2},
        })
        .to_string();
        let messages = vec![
            codex_command_event("item.started", "cmd_1", "in_progress"),
            turn_completed.clone(),
            codex_command_event("item.completed", "cmd_1", "completed"),
        ];
        let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
        // turn.completed (index 1) is kept; the started (index 0) drops in
        // favor of the completed (index 2).
        assert_eq!(visible, vec![1, 2]);
    }

    /// Dedup is Codex-only — assistant, portal, user, and non-grouped paths
    /// must keep every index. Even a degenerate same-id codex-shaped JSON in
    /// a non-Codex group should still render fully (the predicate only runs
    /// for `GroupCategory::Codex`).
    #[test]
    fn visible_group_indices_is_codex_only() {
        let messages = vec![
            codex_command_event("item.started", "cmd_1", "in_progress"),
            codex_command_event("item.completed", "cmd_1", "completed"),
        ];
        for cat in [
            GroupCategory::Assistant,
            GroupCategory::Portal,
            GroupCategory::User,
        ] {
            let visible = visible_group_indices(cat, &rendered_vec(&messages));
            assert_eq!(
                visible,
                vec![0, 1],
                "dedup must not fire for {:?}; got {:?}",
                cat,
                visible
            );
        }
    }

    /// A Codex item with no `id` field must not collapse into a same-shape
    /// neighbor — dedup is keyed on `item_id`, so a missing id means
    /// "definitely not the same item".
    #[test]
    fn codex_items_without_id_do_not_collapse() {
        let no_id_a = serde_json::json!({
            "type": "item.started",
            "item": {"type": "agent_message", "text": "first"},
        })
        .to_string();
        let no_id_b = serde_json::json!({
            "type": "item.completed",
            "item": {"type": "agent_message", "text": "second"},
        })
        .to_string();
        let visible =
            visible_group_indices(GroupCategory::Codex, &rendered_vec(&[no_id_a, no_id_b]));
        assert_eq!(visible, vec![0, 1]);
    }

    #[test]
    fn assistant_group_label_uses_claude_model() {
        let messages = vec![serde_json::json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "role": "assistant",
                "model": "claude-opus-4-7-20260501",
                "content": [{"type": "text", "text": "hello"}],
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string()];

        let groups = group_for_tests(&messages);
        match &groups[0] {
            MessageGroup::IdentityGroup { label, .. } => {
                assert_eq!(label, "Claude - Opus 4.7");
            }
            other => panic!("expected assistant identity group, got {:?}", other),
        }
    }

    #[test]
    fn test_shorten_model_name() {
        assert_eq!(
            shorten_model_name("claude-opus-4-5-20251101"),
            Some("Opus 4.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-sonnet-4-5-20250929"),
            Some("Sonnet 4.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-haiku-4-5-20251001"),
            Some("Haiku 4.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-3-5-sonnet-20241022"),
            Some("Sonnet 3.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-opus-4-6"),
            Some("Opus 4.6".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-opus-4-7[1m]"),
            Some("Opus 4.7".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-sonnet-4-5"),
            Some("Sonnet 4.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-fable-5"),
            Some("Fable 5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-mythos-5"),
            Some("Mythos 5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-fable-5-20260601"),
            Some("Fable 5".to_string())
        );
        assert_eq!(shorten_model_name("claude-opus"), Some("Opus".to_string()));
        assert_eq!(shorten_model_name(""), None);
        assert_eq!(shorten_model_name("<unknown>"), None);
        assert_eq!(shorten_model_name("gpt-4-turbo"), Some("gpt".to_string()));
    }
}

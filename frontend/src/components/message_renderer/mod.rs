mod grouping;
mod renderers;
pub mod turn_metrics_footer;
pub mod types;

use serde_json::Value;
use uuid::Uuid;
use yew::prelude::*;

#[cfg(test)]
use grouping::classify;
use grouping::{extract_raw_iso, visible_group_indices, GroupCategory};
pub use grouping::{group_is_turn_terminator, group_messages, MessageGroup};
use types::{user_meta_from_json, ClaudeMessage};

/// Extract `_created_at` from a raw JSON message string and format it as local time.
fn extract_local_timestamp(json: &str) -> Option<String> {
    let val: Value = serde_json::from_str(json).ok()?;
    let iso = val.get("_created_at")?.as_str()?;
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
    pub json: String,
    #[prop_or_default]
    pub session_id: Option<Uuid>,
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
}

#[function_component(MessageRenderer)]
pub fn message_renderer(props: &MessageRendererProps) -> Html {
    let ts = extract_local_timestamp(&props.json);
    let raw_iso = extract_raw_iso(&props.json);
    let parsed = ClaudeMessage::parse(&props.json);

    // Dispatch on the message shape, not the agent. `User` (the proxy's
    // synthetic echo) and `Portal` (the backend's portal-content envelope)
    // are protocol-agnostic and must render the same way on Claude and
    // Codex sessions — otherwise the Codex renderer's catch-all turns them
    // into raw JSON blocks. Codex-specific shapes (`item.started`,
    // `turn.completed`, …) don't match any `ClaudeMessage` variant and fall
    // through to the codex renderer below.
    match parsed {
        Ok(ClaudeMessage::System(msg)) => {
            return renderers::render_system_message(&msg, ts.as_deref());
        }
        Ok(ClaudeMessage::Assistant(msg)) => {
            return renderers::render_assistant_message(
                &msg,
                ts.as_deref(),
                raw_iso.as_deref(),
                props.session_id,
            );
        }
        Ok(ClaudeMessage::Result(msg)) => {
            return renderers::render_result_message(&msg, props.turn_metrics.as_ref());
        }
        Ok(ClaudeMessage::User(msg)) => {
            let meta = user_meta_from_json(&props.json);
            return renderers::render_user_message(
                &msg,
                &meta,
                props.current_user_id.as_deref(),
                ts.as_deref(),
            );
        }
        Ok(ClaudeMessage::OptimisticUser(msg)) => {
            return renderers::render_optimistic_user_message(
                &msg,
                props.current_user_id.as_deref(),
                ts.as_deref(),
            );
        }
        Ok(ClaudeMessage::Error(msg)) => {
            return renderers::render_error_message(&msg, ts.as_deref());
        }
        Ok(ClaudeMessage::Portal(msg)) => {
            return renderers::render_portal_message(&msg, ts.as_deref(), props.session_id);
        }
        Ok(ClaudeMessage::RateLimitEvent(msg)) => {
            return renderers::render_rate_limit_event(&msg, ts.as_deref());
        }
        Ok(ClaudeMessage::Unknown) | Err(_) => {}
    }

    if props.agent_type == shared::AgentType::Codex {
        html! { <super::codex_renderer::CodexMessageRenderer json={props.json.clone()} turn_metrics={props.turn_metrics.clone()} /> }
    } else {
        render_raw_json(&props.json)
    }
}

#[derive(Properties, PartialEq)]
pub struct MessageGroupRendererProps {
    pub group: MessageGroup,
    #[prop_or_default]
    pub session_id: Option<Uuid>,
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
}

#[function_component(MessageGroupRenderer)]
pub fn message_group_renderer(props: &MessageGroupRendererProps) -> Html {
    match &props.group {
        MessageGroup::Single(json) => {
            html! { <MessageRenderer json={json.clone()} session_id={props.session_id} agent_type={props.agent_type} current_user_id={props.current_user_id.clone()} turn_metrics={props.turn_metrics.clone()} /> }
        }
        MessageGroup::IdentityGroup {
            category,
            label,
            badge_class,
            messages,
        } => {
            let ts = messages
                .first()
                .and_then(|json| extract_local_timestamp(json));
            let last_iso = messages.last().and_then(|json| extract_raw_iso(json));
            let wrapper_class = match category {
                GroupCategory::User => "user-message",
                GroupCategory::Portal => "portal-message",
                GroupCategory::Assistant | GroupCategory::Codex => "assistant-message",
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
                            let json = &messages[i];
                            let key = extract_raw_iso(json)
                                .map(|iso| format!("m-{}", iso))
                                .unwrap_or_else(|| format!("m{}", i));
                            html! { <div {key} class="grouped-message-part">{ render_identity_group_part(json, props.agent_type) }</div> }
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

fn render_identity_group_part(json: &str, agent_type: shared::AgentType) -> Html {
    match ClaudeMessage::parse(json) {
        Ok(ClaudeMessage::User(msg)) => renderers::render_user_message_content(&msg),
        Ok(ClaudeMessage::OptimisticUser(msg)) => {
            renderers::render_optimistic_user_message_content(&msg)
        }
        Ok(ClaudeMessage::Assistant(msg)) => {
            renderers::render_assistant_message_content(&msg, None)
        }
        Ok(ClaudeMessage::Portal(msg)) => renderers::render_portal_message_content(&msg, None),
        _ if agent_type == shared::AgentType::Codex => {
            super::codex_renderer::render_codex_message_content(json)
        }
        _ => html! {},
    }
}

fn render_raw_json(json: &str) -> Html {
    let display = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| json.to_string());

    html! {
        <div class="claude-message raw-message">
            <div class="message-header">
                <span class="message-type-badge raw">{ "Unrecognized Message" }</span>
            </div>
            <div class="message-body">
                <pre class="raw-json">{ display }</pre>
                <p class="raw-message-hint">
                    { "This message type is not yet supported by the portal. " }
                    <a href="https://github.com/meawoppl/rust-code-agent-sdks/issues"
                       target="_blank" rel="noopener noreferrer">
                        { "Report this issue" }
                    </a>
                </p>
            </div>
        </div>
    }
}

// --- Utility functions (used by renderers and tool_renderers) ---

pub fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

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

    let version = extract_version(model);

    Some(if model.contains("opus") {
        match version {
            Some(v) => format!("Opus {}", v),
            None => "Opus".to_string(),
        }
    } else if model.contains("sonnet") {
        match version {
            Some(v) => format!("Sonnet {}", v),
            None => "Sonnet".to_string(),
        }
    } else if model.contains("haiku") {
        match version {
            Some(v) => format!("Haiku {}", v),
            None => "Haiku".to_string(),
        }
    } else {
        model.split('-').next().unwrap_or(model).to_string()
    })
}

pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60000;
        let secs = (ms % 60000) / 1000;
        format!("{}m {}s", mins, secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_category(json: &str) -> Option<GroupCategory> {
        classify(json, shared::AgentType::Claude, None).map(|identity| identity.category)
    }

    fn classify_codex_category(json: &str) -> Option<GroupCategory> {
        classify(json, shared::AgentType::Codex, None).map(|identity| identity.category)
    }

    fn group_for_tests(messages: &[String]) -> Vec<MessageGroup> {
        group_messages(messages, shared::AgentType::Claude, None)
    }

    fn group_for_codex_tests(messages: &[String]) -> Vec<MessageGroup> {
        group_messages(messages, shared::AgentType::Codex, None)
    }

    /// Realistic Claude wire shape for a user message containing a single
    /// `tool_result` content block (the kind Read / Bash / Edit etc. produce).
    /// Matches `claude-codes` 2.1.x `ClaudeOutput::User(UserMessage)`
    /// serialization with the backend's wire envelope additions
    /// (`_created_at` etc.).
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
            "_created_at": "2026-05-17T10:00:00.000Z",
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
            "_created_at": "2026-05-17T10:00:00.000Z",
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
            "_created_at": "2026-05-17T10:00:00.000Z",
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
            "_created_at": "2026-05-18T05:00:00.000Z",
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
            "_created_at": "2026-05-17T10:00:00.000Z",
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
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string()
    }

    fn codex_item_started_agent_message(text: &str) -> String {
        serde_json::json!({
            "type": "item.started",
            "item": {
                "type": "agent_message",
                "id": "item_abc",
                "text": text,
            },
            "_created_at": "2026-05-17T10:00:00.000Z",
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
            "_created_at": "2026-05-17T10:00:00.000Z",
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
                    "_created_at": "2026-05-17T10:00:00.000Z",
                })
                .to_string(),
                None,
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
                    "_created_at": "2026-05-17T10:00:00.000Z",
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
                    "_created_at": "2026-05-17T10:00:00.000Z",
                })
                .to_string(),
                Some(GroupCategory::Codex),
            ),
            ("unparseable garbage", "not even json".to_string(), None),
        ];

        for (label, json, expected) in cases {
            let got = classify(&json, shared::AgentType::Codex, None).map(|i| i.category);
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
        let visible = visible_group_indices(GroupCategory::Codex, &messages);
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
        let visible = visible_group_indices(GroupCategory::Codex, &messages);
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
        let visible = visible_group_indices(GroupCategory::Codex, &messages);
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
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string();
        let messages = vec![
            codex_command_event("item.started", "cmd_1", "in_progress"),
            turn_completed.clone(),
            codex_command_event("item.completed", "cmd_1", "completed"),
        ];
        let visible = visible_group_indices(GroupCategory::Codex, &messages);
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
            let visible = visible_group_indices(cat, &messages);
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
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string();
        let no_id_b = serde_json::json!({
            "type": "item.completed",
            "item": {"type": "agent_message", "text": "second"},
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string();
        let visible = visible_group_indices(GroupCategory::Codex, &[no_id_a, no_id_b]);
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
            "_created_at": "2026-05-17T10:00:00.000Z",
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
        assert_eq!(shorten_model_name("claude-opus"), Some("Opus".to_string()));
        assert_eq!(shorten_model_name(""), None);
        assert_eq!(shorten_model_name("<unknown>"), None);
        assert_eq!(shorten_model_name("gpt-4-turbo"), Some("gpt".to_string()));
    }
}

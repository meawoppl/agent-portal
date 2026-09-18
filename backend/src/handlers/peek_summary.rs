//! Single-line summaries of stored transcript messages for the agent peek
//! endpoint (#1406).
//!
//! **Context safety is the constraint.** Peek output feeds *agent context
//! windows*: an agent glances at a peer to avoid duplicate work, and the worst
//! case must be ~50 short lines — never a raw wire-JSON dump. Every summary is
//! one line, hard-capped at [`MAX_SUMMARY_CHARS`].
//!
//! Dispatch is on the typed protocol enums (`claude-codes` / `codex-codes` /
//! `muse-codes`), never key-poking: when an SDK regenerates its wire types the
//! compiler names what to update here. A frame that fails its typed parse
//! summarizes as its bare discriminator — bounded, and loud enough to notice.

use shared::api::PeekMessage;
use uuid::Uuid;

/// Hard cap per summary line (the issue's ~240-char budget).
const MAX_SUMMARY_CHARS: usize = 240;

/// Summarize one durable message row into a [`PeekMessage`].
pub fn summarize_message(
    id: Uuid,
    agent_type: &str,
    role: &str,
    content: &str,
    created_at: chrono::NaiveDateTime,
) -> PeekMessage {
    let (kind, summary) = summarize(agent_type, role, content);
    PeekMessage {
        id,
        role: role.to_string(),
        created_at: created_at.and_utc().to_rfc3339(),
        summary: cap(&summary),
        kind: kind.to_string(),
    }
}

/// One line, whitespace-collapsed, capped, with an ellipsis when truncated.
fn cap(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX_SUMMARY_CHARS {
        return one_line;
    }
    let mut capped: String = one_line.chars().take(MAX_SUMMARY_CHARS - 1).collect();
    capped.push('…');
    capped
}

/// First meaningful line of free text, with machine-authored notice blocks
/// (`<system-reminder>`/`<task-notification>`) stripped so a peek shows what
/// the human or agent actually said.
fn excerpt(text: &str) -> String {
    let stripped = shared::system_reminder::strip_collapsible_notices(text);
    stripped
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Compact primary argument of a tool input — the same field preference the
/// permission-push snippet uses (`command`, `file_path`, `url`), falling back
/// to compact JSON of the input (still capped by the caller).
fn tool_input_snippet(input: &serde_json::Value) -> String {
    ["command", "file_path", "url", "path", "pattern", "prompt"]
        .iter()
        .find_map(|k| input.get(k).and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| serde_json::to_string(input).ok())
        .unwrap_or_default()
}

fn summarize(agent_type: &str, role: &str, content: &str) -> (&'static str, String) {
    // Portal-authored frames (inter-agent messages, status cards) share one
    // shape across agents; try them first for the roles that carry them.
    if role == "portal" {
        return summarize_portal(content);
    }
    match agent_type {
        "claude" => summarize_claude(content),
        "codex" => summarize_codex(content),
        "muse" => summarize_muse(content),
        _ => fallback(content),
    }
}

fn summarize_portal(content: &str) -> (&'static str, String) {
    let Ok(msg) = serde_json::from_str::<shared::PortalMessage>(content) else {
        return fallback(content);
    };
    for part in &msg.content {
        match part {
            shared::PortalContent::AgentMessage {
                from_agent_type,
                from_session_id,
                text,
            } => {
                let short = shared::short_session_id(from_session_id);
                return (
                    "text",
                    format!("[from {from_agent_type} {short}] {}", excerpt(text)),
                );
            }
            shared::PortalContent::Text { text } => return ("system", excerpt(text)),
            _ => {}
        }
    }
    ("system", "portal event".to_string())
}

fn summarize_claude(content: &str) -> (&'static str, String) {
    use claude_codes::ClaudeOutput;
    let Ok(output) = serde_json::from_str::<ClaudeOutput>(content) else {
        return summarize_local_or_fallback(content);
    };
    match output {
        ClaudeOutput::Assistant(msg) => summarize_claude_blocks(&msg.message.content),
        ClaudeOutput::User(msg) => summarize_claude_blocks(&msg.message.content),
        ClaudeOutput::Result(r) => (
            "turn_end",
            if r.is_error {
                format!(
                    "turn failed: {}",
                    r.result.as_deref().map(excerpt).unwrap_or_default()
                )
            } else {
                format!("turn complete ({} turns)", r.num_turns)
            },
        ),
        ClaudeOutput::System(s) => ("system", format!("system: {:?}", s.subtype)),
        ClaudeOutput::Error(e) => (
            "error",
            format!(
                "error: {}",
                serde_json::to_value(&e)
                    .ok()
                    .and_then(|v| v
                        .get("error")
                        .and_then(|err| err.get("message"))
                        .and_then(|m| m.as_str())
                        .map(str::to_string))
                    .unwrap_or_else(|| "api error".to_string())
            ),
        ),
        ClaudeOutput::RateLimitEvent(_) => ("system", "rate-limit status".to_string()),
        ClaudeOutput::ConversationReset(_) => ("system", "conversation cleared".to_string()),
        other => (
            "unknown",
            format!("claude {}", output_discriminator(&other)),
        ),
    }
}

/// Summarize claude content blocks: the first tool use wins (it names the
/// activity), then text, then tool results, then thinking.
fn summarize_claude_blocks(blocks: &[claude_codes::io::ContentBlock]) -> (&'static str, String) {
    use claude_codes::io::ContentBlock;
    for block in blocks {
        if let ContentBlock::ToolUse(tool) = block {
            return (
                "tool_use",
                format!("{}: {}", tool.name, tool_input_snippet(&tool.input)),
            );
        }
    }
    for block in blocks {
        if let ContentBlock::Text(text) = block {
            let line = excerpt(&text.text);
            if !line.is_empty() {
                return ("text", line);
            }
        }
    }
    for block in blocks {
        match block {
            ContentBlock::ToolResult(result) => {
                let text = match &result.content {
                    Some(claude_codes::io::ToolResultContent::Text(t)) => excerpt(t),
                    Some(_) => "[structured result]".to_string(),
                    None => String::new(),
                };
                return ("tool_result", format!("tool result: {text}"));
            }
            ContentBlock::Thinking(_) => return ("thinking", "thinking…".to_string()),
            ContentBlock::Image(_) => return ("image", "[image]".to_string()),
            _ => {}
        }
    }
    ("unknown", "empty message".to_string())
}

fn output_discriminator(output: &claude_codes::ClaudeOutput) -> String {
    serde_json::to_value(output)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_else(|| "frame".to_string())
}

fn summarize_codex(content: &str) -> (&'static str, String) {
    use codex_codes::ThreadEvent;
    let Ok(event) = serde_json::from_str::<ThreadEvent>(content) else {
        return summarize_local_or_fallback(content);
    };
    match event {
        ThreadEvent::ItemCompleted(e) => summarize_codex_item(&e.item),
        ThreadEvent::ItemStarted(e) => summarize_codex_item(&e.item),
        ThreadEvent::ItemUpdated(e) => summarize_codex_item(&e.item),
        ThreadEvent::TurnCompleted(_) => ("turn_end", "turn complete".to_string()),
        ThreadEvent::TurnFailed(_) => ("turn_end", "turn failed".to_string()),
        ThreadEvent::TurnStarted(_) => ("system", "turn started".to_string()),
        ThreadEvent::ThreadStarted(_) => ("system", "thread started".to_string()),
        ThreadEvent::Error(e) => ("error", format!("error: {}", excerpt(&e.message))),
    }
}

fn summarize_codex_item(item: &codex_codes::io::items::ThreadItem) -> (&'static str, String) {
    use codex_codes::io::items::ThreadItem;
    match item {
        ThreadItem::UserMessage(m) => {
            let text = m
                .content
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            ("text", excerpt(&text))
        }
        ThreadItem::AgentMessage(m) => ("text", excerpt(&m.text)),
        ThreadItem::Reasoning(_) => ("thinking", "thinking…".to_string()),
        ThreadItem::CommandExecution(c) => ("tool_use", format!("bash: {}", excerpt(&c.command))),
        ThreadItem::FileChange(f) => {
            let paths = f
                .changes
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            ("tool_use", format!("file change: {paths}"))
        }
        ThreadItem::McpToolCall(t) => ("tool_use", format!("mcp: {}/{}", t.server, t.tool)),
        ThreadItem::WebSearch(w) => ("tool_use", format!("web search: {}", excerpt(&w.query))),
        ThreadItem::TodoList(_) => ("system", "todo list update".to_string()),
        ThreadItem::Error(e) => ("error", format!("error: {}", excerpt(&e.message))),
    }
}

fn summarize_muse(content: &str) -> (&'static str, String) {
    let Ok(record) = serde_json::from_str::<muse_codes::MuseRecord>(content) else {
        return summarize_local_or_fallback(content);
    };
    use muse_codes::MusePayload;
    match record.typed_payload() {
        Ok(MusePayload::TurnInputUser(input)) => ("text", excerpt(&input.prompt)),
        Ok(MusePayload::RunOutputDelta(delta)) => ("text", excerpt(&delta.text)),
        Ok(MusePayload::ToolResult(result)) => {
            let tool = result
                .correlation_facts
                .as_ref()
                .and_then(|f| f.tool_name.as_deref())
                .unwrap_or("tool");
            ("tool_result", format!("{tool}: {}", excerpt(&result.text)))
        }
        Ok(MusePayload::RunTerminal(t)) => (
            "turn_end",
            format!(
                "run {}{}",
                t.terminal,
                t.text
                    .as_deref()
                    .map(|text| format!(": {}", excerpt(text)))
                    .unwrap_or_default()
            ),
        ),
        Ok(MusePayload::ModelConfigured(m)) => ("system", format!("model: {}", m.model_id)),
        _ => ("system", format!("muse {}", record.payload_type)),
    }
}

/// Non-wire shapes shared across agents: the portal's local frames (a plain
/// user echo `{type:"user",content}`, a portal text card, a local error).
/// Anything else falls to the bounded discriminator fallback.
fn summarize_local_or_fallback(content: &str) -> (&'static str, String) {
    if let Ok(frame) = serde_json::from_str::<shared::UserFrame>(content) {
        if frame.message_type == shared::UserFrame::MESSAGE_TYPE {
            return ("text", excerpt(&frame.content));
        }
    }
    // Bare string content (non-JSON rows degrade to strings at ingest).
    if !content.trim_start().starts_with('{') {
        return ("text", excerpt(content));
    }
    fallback(content)
}

/// Bounded last resort: name the frame's discriminator, never dump its body.
fn fallback(content: &str) -> (&'static str, String) {
    let kind = serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| {
            v.get("type")
                .or_else(|| v.get("payload_type"))
                .and_then(|t| t.as_str())
                .map(str::to_string)
        });
    match kind {
        Some(k) => ("unknown", format!("[{k} frame]")),
        None => ("unknown", "[unrecognized frame]".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summarize_row(agent_type: &str, role: &str, content: &str) -> PeekMessage {
        summarize_message(
            Uuid::nil(),
            agent_type,
            role,
            content,
            chrono::DateTime::from_timestamp(1_700_000_000, 0)
                .expect("valid timestamp")
                .naive_utc(),
        )
    }

    /// The context-safety invariant: no summary ever exceeds the cap or spans
    /// multiple lines, no matter what the stored frame holds.
    #[test]
    fn summaries_are_single_line_and_capped() {
        let huge_text = format!(
            r#"{{"type":"assistant","session_id":"s","message":{{"id":"m1","role":"assistant","model":"claude-fable-5","content":[{{"type":"text","text":"{}"}}]}}}}"#,
            "long word ".repeat(500)
        );
        let m = summarize_row("claude", "assistant", &huge_text);
        assert!(m.summary.chars().count() <= MAX_SUMMARY_CHARS);
        assert!(!m.summary.contains('\n'));
        assert!(m.summary.ends_with('…'));
    }

    #[test]
    fn claude_tool_use_names_the_tool_and_primary_argument() {
        let content = r#"{"type":"assistant","session_id":"s","message":{"id":"m1","role":"assistant","model":"claude-fable-5","content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo build -p backend"}}
        ]}}"#;
        let m = summarize_row("claude", "assistant", content);
        assert_eq!(m.kind, "tool_use");
        assert_eq!(m.summary, "Bash: cargo build -p backend");
    }

    #[test]
    fn claude_result_frame_reads_as_turn_end() {
        let content = r#"{"type":"result","subtype":"success","is_error":false,
            "duration_ms":1,"duration_api_ms":1,"num_turns":3,"session_id":"s",
            "total_cost_usd":0.0,"usage":{"input_tokens":0,"output_tokens":0}}"#;
        let m = summarize_row("claude", "result", content);
        assert_eq!(m.kind, "turn_end");
        assert!(m.summary.contains("turn complete"));
    }

    #[test]
    fn codex_command_execution_summarizes_as_bash() {
        let content = r#"{"type":"item.completed","item":{"id":"i","type":"command_execution",
            "command":"ls -la","aggregated_output":"...","exit_code":0,"status":"completed"}}"#;
        let m = summarize_row("codex", "assistant", content);
        assert_eq!(m.kind, "tool_use");
        assert_eq!(m.summary, "bash: ls -la");
    }

    #[test]
    fn portal_agent_message_carries_sender_attribution() {
        let msg = shared::PortalMessage::agent_message(
            "codex".to_string(),
            "12345678-0000-0000-0000-000000000000".to_string(),
            "on it, running tests".to_string(),
        );
        let m = summarize_row("claude", "portal", &msg.to_json().to_string());
        assert_eq!(m.kind, "text");
        assert_eq!(m.summary, "[from codex 12345678] on it, running tests");
    }

    /// Machine-authored notice blocks are stripped so a peek shows what the
    /// sender actually said, not the portal-reminder envelope.
    #[test]
    fn user_text_strips_system_reminders() {
        let content = r#"{"type":"user","content":"<system-reminder>\nnoise\n</system-reminder>\n\nfix the bug"}"#;
        let m = summarize_row("claude", "user", content);
        assert_eq!(m.kind, "text");
        assert_eq!(m.summary, "fix the bug");
    }

    /// Unknown frames degrade to their bare discriminator — bounded, never a
    /// raw JSON dump of the body.
    #[test]
    fn unknown_frames_fall_back_to_the_discriminator_only() {
        let secret = r#"{"type":"future.frame","payload":{"token":"hunter2"}}"#;
        let m = summarize_row("claude", "unknown", secret);
        assert_eq!(m.kind, "unknown");
        assert_eq!(m.summary, "[future.frame frame]");
        assert!(!m.summary.contains("hunter2"));
    }

    #[test]
    fn muse_run_terminal_reads_as_turn_end() {
        let content = r#"{"schema_version":1,"id":"rec-1","stream":{"kind":"session","id":"s"},
            "sequence":1,"recorded_at":1,"record_type":"event","durability":"durable",
            "causation_id":"c","payload_type":"run.terminal.completed","payload_schema_version":1,
            "payload":{"kind":"run.terminal.completed","command_id":"c",
                "run_stream":{"kind":"run","id":"r"},"terminal":"completed","reason":null,"text":"done"}}"#;
        let m = summarize_row("muse", "assistant", content);
        assert_eq!(m.kind, "turn_end");
        assert_eq!(m.summary, "run completed: done");
    }
}

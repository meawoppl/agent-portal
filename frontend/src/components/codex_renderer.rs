use super::markdown::render_markdown;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use yew::prelude::*;

// Local deserialization types mirroring codex-codes, using Option wrappers
// for lenient parsing (same strategy as message_renderer.rs for Claude).

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodexEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted {
        thread_id: Option<String>,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {},
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        usage: Option<CodexUsage>,
    },
    #[serde(rename = "turn.failed")]
    TurnFailed {
        error: Option<CodexError>,
    },
    #[serde(rename = "item.started")]
    ItemStarted {
        item: Option<CodexItem>,
    },
    #[serde(rename = "item.updated")]
    ItemUpdated {
        item: Option<CodexItem>,
    },
    #[serde(rename = "item.completed")]
    ItemCompleted {
        item: Option<CodexItem>,
    },
    Error {
        message: Option<String>,
    },
    // Streaming-delta / plan / diff notifications. The proxy forwards these as
    // `{"type": "<slash-named method>", "params": <inner-payload>}`. The
    // payload struct field names mirror codex-codes' generated types verbatim
    // (camelCase via the outer `rename_all = "snake_case"` would not match —
    // we provide aliases below for the camelCase wire format).
    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated {
        params: Option<TurnDiffUpdatedParams>,
    },
    #[serde(rename = "item/fileChange/patchUpdated")]
    FileChangePatchUpdated {
        params: Option<FileChangePatchUpdatedParams>,
    },
    #[serde(rename = "turn/plan/updated")]
    TurnPlanUpdated {
        params: Option<TurnPlanUpdatedParams>,
    },
    #[serde(rename = "item/plan/delta")]
    PlanDelta {
        params: Option<PlanDeltaParams>,
    },
    #[serde(rename = "item/reasoning/summaryPartAdded")]
    ReasoningSummaryPartAdded {
        params: Option<serde_json::Value>,
    },
    #[serde(rename = "item/reasoning/textDelta")]
    ReasoningTextDelta {
        params: Option<serde_json::Value>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnDiffUpdatedParams {
    #[serde(default)]
    pub diff: Option<String>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileChangePatchUpdatedParams {
    #[serde(default)]
    pub changes: Option<Vec<FileUpdateChange>>,
    #[serde(default, rename = "itemId", alias = "item_id")]
    pub item_id: Option<String>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileUpdateChange {
    #[serde(default)]
    pub diff: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnPlanUpdatedParams {
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub plan: Option<Vec<TurnPlanStep>>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnPlanStep {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanDeltaParams {
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default, rename = "itemId", alias = "item_id")]
    pub item_id: Option<String>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexUsage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexError {
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodexItem {
    #[serde(alias = "agentMessage")]
    AgentMessage {
        id: Option<String>,
        text: Option<String>,
    },
    #[serde(alias = "reasoning")]
    Reasoning {
        id: Option<String>,
        text: Option<String>,
    },
    #[serde(alias = "commandExecution")]
    CommandExecution {
        id: Option<String>,
        command: Option<String>,
        #[serde(alias = "aggregatedOutput")]
        aggregated_output: Option<String>,
        #[serde(alias = "exitCode")]
        exit_code: Option<i32>,
        status: Option<String>,
    },
    #[serde(alias = "fileChange")]
    FileChange {
        id: Option<String>,
        changes: Option<Vec<FileChange>>,
        status: Option<String>,
    },
    #[serde(alias = "mcpToolCall")]
    McpToolCall {
        id: Option<String>,
        server: Option<String>,
        tool: Option<String>,
        arguments: Option<Value>,
        status: Option<String>,
    },
    #[serde(alias = "webSearch")]
    WebSearch {
        id: Option<String>,
        query: Option<String>,
    },
    #[serde(alias = "todoList")]
    TodoList {
        id: Option<String>,
        items: Option<Vec<TodoEntry>>,
    },
    #[serde(alias = "error")]
    Error {
        id: Option<String>,
        message: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoEntry {
    pub text: Option<String>,
    pub completed: Option<bool>,
}

// --- Components ---

#[derive(Properties, PartialEq)]
pub struct CodexMessageRendererProps {
    pub json: String,
}

#[function_component(CodexMessageRenderer)]
pub fn codex_message_renderer(props: &CodexMessageRendererProps) -> Html {
    let parsed: Result<CodexEvent, _> = serde_json::from_str(&props.json);

    match parsed {
        Ok(CodexEvent::ThreadStarted { .. }) => html! {},
        Ok(CodexEvent::TurnStarted {}) => html! {},
        Ok(CodexEvent::TurnCompleted { usage }) => render_turn_completed(usage.as_ref()),
        Ok(CodexEvent::TurnFailed { error }) => render_turn_failed(error.as_ref()),
        Ok(CodexEvent::ItemStarted { item }) | Ok(CodexEvent::ItemUpdated { item }) => {
            render_item(item.as_ref(), false)
        }
        Ok(CodexEvent::ItemCompleted { item }) => render_item(item.as_ref(), true),
        Ok(CodexEvent::Error { message }) => render_thread_error(message.as_deref()),
        Ok(CodexEvent::TurnDiffUpdated { params }) => {
            render_turn_diff(params.as_ref().and_then(|p| p.diff.as_deref()))
        }
        Ok(CodexEvent::FileChangePatchUpdated { params }) => {
            render_file_change_patch(params.as_ref().and_then(|p| p.changes.as_deref()))
        }
        Ok(CodexEvent::TurnPlanUpdated { params }) => render_turn_plan(
            params.as_ref().and_then(|p| p.plan.as_deref()),
            params.as_ref().and_then(|p| p.explanation.as_deref()),
        ),
        // Per-chunk deltas — the consolidated content lands in `turn/plan/updated`
        // (for plans) or `item.completed` (for reasoning). Emit nothing for the
        // streaming chunks to avoid visual noise without losing information.
        Ok(CodexEvent::PlanDelta { .. })
        | Ok(CodexEvent::ReasoningSummaryPartAdded { .. })
        | Ok(CodexEvent::ReasoningTextDelta { .. }) => html! {},
        Ok(CodexEvent::Unknown) | Err(_) => render_raw_codex(&props.json),
    }
}

fn render_item(item: Option<&CodexItem>, completed: bool) -> Html {
    let Some(item) = item else {
        return html! {};
    };
    match item {
        CodexItem::AgentMessage { text, .. } => render_agent_message(text.as_deref()),
        CodexItem::Reasoning { text, .. } => render_reasoning(text.as_deref()),
        CodexItem::CommandExecution {
            command,
            aggregated_output,
            exit_code,
            status,
            ..
        } => render_command_execution(
            command.as_deref(),
            aggregated_output.as_deref(),
            *exit_code,
            status.as_deref(),
            completed,
        ),
        CodexItem::FileChange {
            changes, status, ..
        } => render_file_change(changes.as_deref(), status.as_deref()),
        CodexItem::McpToolCall {
            server,
            tool,
            status,
            ..
        } => render_mcp_tool_call(server.as_deref(), tool.as_deref(), status.as_deref()),
        CodexItem::WebSearch { query, .. } => render_web_search(query.as_deref()),
        CodexItem::TodoList { items, .. } => render_todo_list(items.as_deref()),
        CodexItem::Error { message, .. } => render_item_error(message.as_deref()),
        CodexItem::Unknown => html! {},
    }
}

fn render_agent_message(text: Option<&str>) -> Html {
    let text = text.unwrap_or("");
    if text.is_empty() {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-header">
                <span class="message-type-badge assistant">{ "Codex" }</span>
            </div>
            <div class="message-body">
                <div class="assistant-text">{ render_markdown(text) }</div>
            </div>
        </div>
    }
}

fn render_reasoning(text: Option<&str>) -> Html {
    let text = text.unwrap_or("");
    if text.is_empty() {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="thinking-block">
                    <span class="thinking-label">{ "reasoning" }</span>
                    <div class="thinking-content">{ text }</div>
                </div>
            </div>
        </div>
    }
}

fn render_command_execution(
    command: Option<&str>,
    output: Option<&str>,
    exit_code: Option<i32>,
    status: Option<&str>,
    completed: bool,
) -> Html {
    let cmd = command.unwrap_or("(unknown command)");
    let out = output.unwrap_or("");

    let status_text = if completed {
        match exit_code {
            Some(0) => "completed".to_string(),
            Some(code) => format!("exit {}", code),
            None => status.unwrap_or("completed").to_string(),
        }
    } else {
        "running...".to_string()
    };

    let is_error = exit_code.is_some_and(|c| c != 0);

    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "$" }</span>
                        <span class="tool-name">{ "Bash" }</span>
                        <span class="tool-meta">{ &status_text }</span>
                    </div>
                    <pre class="tool-input-content">{ cmd }</pre>
                    {
                        if !out.is_empty() {
                            let class = if is_error { "tool-result error" } else { "tool-result" };
                            html! {
                                <div class={class}>
                                    <pre class="tool-result-content">{ out }</pre>
                                </div>
                            }
                        } else {
                            html! {}
                        }
                    }
                </div>
            </div>
        </div>
    }
}

fn render_file_change(changes: Option<&[FileChange]>, status: Option<&str>) -> Html {
    let changes = changes.unwrap_or(&[]);
    if changes.is_empty() {
        return html! {};
    }

    let status_label = status.unwrap_or("completed");

    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{1f4dd}" }</span>
                        <span class="tool-name">{ "File Changes" }</span>
                        <span class="tool-meta">{ status_label }</span>
                    </div>
                    <div class="file-changes-list">
                        { for changes.iter().map(|c| {
                            let path = c.path.as_deref().unwrap_or("(unknown)");
                            let kind = c.kind.as_deref().unwrap_or("update");
                            let kind_class = format!("file-change-kind {}", kind);
                            html! {
                                <div class="file-change-entry">
                                    <span class={kind_class}>{ kind }</span>
                                    <span class="file-change-path">{ path }</span>
                                </div>
                            }
                        })}
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_mcp_tool_call(server: Option<&str>, tool: Option<&str>, status: Option<&str>) -> Html {
    let server = server.unwrap_or("(unknown)");
    let tool = tool.unwrap_or("(unknown)");
    let status = status.unwrap_or("in_progress");

    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{1f50c}" }</span>
                        <span class="tool-name">{ format!("{} / {}", server, tool) }</span>
                        <span class="tool-meta">{ status }</span>
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_web_search(query: Option<&str>) -> Html {
    let query = query.unwrap_or("(no query)");
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{1f50d}" }</span>
                        <span class="tool-name">{ "Web Search" }</span>
                    </div>
                    <pre class="tool-input-content">{ query }</pre>
                </div>
            </div>
        </div>
    }
}

fn render_todo_list(items: Option<&[TodoEntry]>) -> Html {
    let items = items.unwrap_or(&[]);
    if items.is_empty() {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{2611}" }</span>
                        <span class="tool-name">{ "Todo List" }</span>
                    </div>
                    <div class="codex-todo-list">
                        { for items.iter().map(|item| {
                            let text = item.text.as_deref().unwrap_or("");
                            let done = item.completed.unwrap_or(false);
                            let marker = if done { "\u{2611}" } else { "\u{2610}" };
                            let class = if done { "codex-todo done" } else { "codex-todo" };
                            html! {
                                <div class={class}>
                                    <span class="codex-todo-marker">{ marker }</span>
                                    <span class="codex-todo-text">{ text }</span>
                                </div>
                            }
                        })}
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_turn_completed(usage: Option<&CodexUsage>) -> Html {
    let input = usage.and_then(|u| u.input_tokens).unwrap_or(0);
    let output = usage.and_then(|u| u.output_tokens).unwrap_or(0);
    let cached = usage.and_then(|u| u.cached_input_tokens).unwrap_or(0);

    let tooltip = format!("Input: {} | Output: {} | Cached: {}", input, output, cached);

    html! {
        <div class="claude-message result-message success">
            <div class="result-stats-bar">
                <span class="result-status success">{ "\u{2713}" }</span>
                {
                    if input > 0 || output > 0 {
                        html! {
                            <>
                                <span class="stat-item tokens" title={tooltip}>
                                    { format!("{}\u{2193} {}\u{2191}", input, output) }
                                </span>
                            </>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    }
}

fn render_turn_failed(error: Option<&CodexError>) -> Html {
    let message = error
        .and_then(|e| e.message.as_deref())
        .unwrap_or("Turn failed");

    html! {
        <div class="claude-message error-message-display">
            <div class="message-header">
                <span class="message-type-badge result error">{ "Turn Failed" }</span>
            </div>
            <div class="message-body">
                <div class="error-text">{ message }</div>
            </div>
        </div>
    }
}

fn render_thread_error(message: Option<&str>) -> Html {
    let message = message.unwrap_or("Unknown error");
    html! {
        <div class="claude-message error-message-display">
            <div class="message-header">
                <span class="message-type-badge result error">{ "Error" }</span>
            </div>
            <div class="message-body">
                <div class="error-text">{ message }</div>
            </div>
        </div>
    }
}

fn render_item_error(message: Option<&str>) -> Html {
    let message = message.unwrap_or("Unknown error");
    html! {
        <div class="claude-message error-message-display">
            <div class="message-header">
                <span class="message-type-badge result error">{ "Error" }</span>
            </div>
            <div class="message-body">
                <div class="error-text">{ message }</div>
            </div>
        </div>
    }
}

fn render_turn_diff(diff: Option<&str>) -> Html {
    let diff = diff.unwrap_or("");
    if diff.trim().is_empty() {
        // Empty deltas — don't render an empty block.
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{1f4dd}" }</span>
                        <span class="tool-name">{ "Turn Diff (cumulative)" }</span>
                    </div>
                    { super::diff::render_unified_diff(diff) }
                </div>
            </div>
        </div>
    }
}

fn render_file_change_patch(changes: Option<&[FileUpdateChange]>) -> Html {
    let changes = changes.unwrap_or(&[]);
    let any_diff = changes
        .iter()
        .any(|c| c.diff.as_deref().is_some_and(|d| !d.trim().is_empty()));
    if !any_diff {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{1f4dd}" }</span>
                        <span class="tool-name">{ "File Changes (patch)" }</span>
                    </div>
                    <div class="file-changes-list">
                        { for changes.iter().map(|c| {
                            let path = c.path.as_deref().unwrap_or("(unknown)");
                            let kind = c.kind.as_deref().unwrap_or("update");
                            let kind_class = format!("file-change-kind {}", kind);
                            let diff = c.diff.as_deref().unwrap_or("");
                            html! {
                                <div class="file-change-entry">
                                    <div>
                                        <span class={kind_class}>{ kind }</span>
                                        <span class="file-change-path">{ path }</span>
                                    </div>
                                    {
                                        if diff.trim().is_empty() {
                                            html! {}
                                        } else {
                                            super::diff::render_unified_diff(diff)
                                        }
                                    }
                                </div>
                            }
                        })}
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_turn_plan(plan: Option<&[TurnPlanStep]>, explanation: Option<&str>) -> Html {
    let plan = plan.unwrap_or(&[]);
    let explanation = explanation.unwrap_or("");
    if plan.is_empty() && explanation.trim().is_empty() {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{1f5d2}" }</span>
                        <span class="tool-name">{ "Plan" }</span>
                    </div>
                    {
                        if !explanation.trim().is_empty() {
                            html! { <div class="assistant-text">{ explanation }</div> }
                        } else {
                            html! {}
                        }
                    }
                    {
                        if !plan.is_empty() {
                            html! {
                                <div class="codex-todo-list">
                                    { for plan.iter().enumerate().map(|(i, step)| {
                                        let status = step.status.as_deref().unwrap_or("pending");
                                        let text = step.step.as_deref().unwrap_or("");
                                        let (marker, class) = match status {
                                            "completed" => ("\u{2611}", "codex-todo done"),
                                            "inProgress" | "in_progress" => ("\u{25b6}", "codex-todo"),
                                            _ => ("\u{2610}", "codex-todo"),
                                        };
                                        html! {
                                            <div class={class}>
                                                <span class="codex-todo-marker">{ marker }</span>
                                                <span class="codex-todo-text">
                                                    { format!("{}. {}", i + 1, text) }
                                                </span>
                                            </div>
                                        }
                                    })}
                                </div>
                            }
                        } else {
                            html! {}
                        }
                    }
                </div>
            </div>
        </div>
    }
}

fn render_raw_codex(json: &str) -> Html {
    let display = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| json.to_string());

    html! {
        <div class="claude-message raw-message">
            <div class="message-header">
                <span class="message-type-badge raw">{ "Codex Raw" }</span>
            </div>
            <div class="message-body">
                <pre class="raw-json">{ display }</pre>
            </div>
        </div>
    }
}

/// Check if a Codex message indicates "awaiting" (turn complete or turn failed)
pub fn is_codex_terminal_event(json: &str) -> Option<bool> {
    let val: Value = serde_json::from_str(json).ok()?;
    let event_type = val.get("type")?.as_str()?;
    match event_type {
        "turn.completed" | "turn.failed" => Some(true),
        "item.started" | "item.updated" | "item.completed" | "turn.started" | "thread.started" => {
            Some(false)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- CodexItem snake_case deserialization ---

    #[test]
    fn item_agent_message_snake_case() {
        let json = r#"{"type":"agent_message","id":"m1","text":"hello"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::AgentMessage { ref text, .. } if text.as_deref() == Some("hello"))
        );
    }

    #[test]
    fn item_reasoning_snake_case() {
        let json = r#"{"type":"reasoning","id":"r1","text":"thinking..."}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::Reasoning { ref text, .. } if text.as_deref() == Some("thinking..."))
        );
    }

    #[test]
    fn item_command_execution_snake_case() {
        let json = r#"{"type":"command_execution","id":"c1","command":"ls","aggregated_output":"foo","exit_code":0,"status":"completed"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(matches!(
            item,
            CodexItem::CommandExecution { ref command, ref aggregated_output, exit_code: Some(0), .. }
            if command.as_deref() == Some("ls") && aggregated_output.as_deref() == Some("foo")
        ));
    }

    #[test]
    fn item_file_change_snake_case() {
        let json = r#"{"type":"file_change","id":"f1","changes":[{"path":"a.rs","kind":"update"}],"status":"completed"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::FileChange { ref changes, .. } if changes.as_ref().unwrap().len() == 1)
        );
    }

    #[test]
    fn item_mcp_tool_call_snake_case() {
        let json = r#"{"type":"mcp_tool_call","id":"mcp1","server":"srv","tool":"t","arguments":{},"status":"completed"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::McpToolCall { ref server, ref tool, .. } if server.as_deref() == Some("srv") && tool.as_deref() == Some("t"))
        );
    }

    #[test]
    fn item_web_search_snake_case() {
        let json = r#"{"type":"web_search","id":"w1","query":"rust serde"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::WebSearch { ref query, .. } if query.as_deref() == Some("rust serde"))
        );
    }

    #[test]
    fn item_todo_list_snake_case() {
        let json =
            r#"{"type":"todo_list","id":"t1","items":[{"text":"fix bug","completed":false}]}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::TodoList { ref items, .. } if items.as_ref().unwrap().len() == 1)
        );
    }

    #[test]
    fn item_error_snake_case() {
        let json = r#"{"type":"error","id":"e1","message":"oops"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::Error { ref message, .. } if message.as_deref() == Some("oops"))
        );
    }

    // --- CodexItem camelCase deserialization ---

    #[test]
    fn item_agent_message_camel_case() {
        let json = r#"{"type":"agentMessage","id":"m1","text":"hello"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::AgentMessage { ref text, .. } if text.as_deref() == Some("hello"))
        );
    }

    #[test]
    fn item_reasoning_camel_case() {
        let json = r#"{"type":"reasoning","id":"r1","text":"thinking..."}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::Reasoning { ref text, .. } if text.as_deref() == Some("thinking..."))
        );
    }

    #[test]
    fn item_command_execution_camel_case() {
        let json = r#"{"type":"commandExecution","id":"c1","command":"ls","aggregatedOutput":"foo","exitCode":0,"status":"completed"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(matches!(
            item,
            CodexItem::CommandExecution { ref command, ref aggregated_output, exit_code: Some(0), .. }
            if command.as_deref() == Some("ls") && aggregated_output.as_deref() == Some("foo")
        ));
    }

    #[test]
    fn item_file_change_camel_case() {
        let json = r#"{"type":"fileChange","id":"f1","changes":[{"path":"a.rs","kind":"update"}],"status":"completed"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::FileChange { ref changes, .. } if changes.as_ref().unwrap().len() == 1)
        );
    }

    #[test]
    fn item_mcp_tool_call_camel_case() {
        let json = r#"{"type":"mcpToolCall","id":"mcp1","server":"srv","tool":"t","arguments":{},"status":"completed"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::McpToolCall { ref server, ref tool, .. } if server.as_deref() == Some("srv") && tool.as_deref() == Some("t"))
        );
    }

    #[test]
    fn item_web_search_camel_case() {
        let json = r#"{"type":"webSearch","id":"w1","query":"rust serde"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::WebSearch { ref query, .. } if query.as_deref() == Some("rust serde"))
        );
    }

    #[test]
    fn item_todo_list_camel_case() {
        let json =
            r#"{"type":"todoList","id":"t1","items":[{"text":"fix bug","completed":false}]}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, CodexItem::TodoList { ref items, .. } if items.as_ref().unwrap().len() == 1)
        );
    }

    // --- CodexEvent deserialization ---

    #[test]
    fn event_item_completed_with_camel_case_item() {
        let json =
            r#"{"type":"item.completed","item":{"type":"agentMessage","id":"m1","text":"done"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            CodexEvent::ItemCompleted {
                item: Some(CodexItem::AgentMessage { .. })
            }
        ));
    }

    #[test]
    fn event_item_updated_with_camel_case_command() {
        let json = r#"{"type":"item.updated","item":{"type":"commandExecution","id":"c1","command":"ls","aggregatedOutput":"out","exitCode":1,"status":"failed"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            CodexEvent::ItemUpdated {
                item: Some(CodexItem::CommandExecution {
                    exit_code: Some(1),
                    ..
                })
            }
        ));
    }

    #[test]
    fn event_unknown_type_falls_through() {
        let json = r#"{"type":"some.future.event","data":123}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, CodexEvent::Unknown));
    }

    #[test]
    fn item_unknown_type_falls_through() {
        let json = r#"{"type":"some_new_item_type","id":"x"}"#;
        let item: CodexItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, CodexItem::Unknown));
    }

    // --- Round-trip: serialize then deserialize ---

    #[test]
    fn round_trip_agent_message() {
        let item = CodexItem::AgentMessage {
            id: Some("m1".into()),
            text: Some("hello".into()),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: CodexItem = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, CodexItem::AgentMessage { ref text, .. } if text.as_deref() == Some("hello"))
        );
    }

    #[test]
    fn round_trip_command_execution() {
        let item = CodexItem::CommandExecution {
            id: Some("c1".into()),
            command: Some("echo hi".into()),
            aggregated_output: Some("hi\n".into()),
            exit_code: Some(0),
            status: Some("completed".into()),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: CodexItem = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            CodexItem::CommandExecution {
                exit_code: Some(0),
                ..
            }
        ));
    }

    #[test]
    fn round_trip_codex_event() {
        let event = CodexEvent::TurnCompleted {
            usage: Some(CodexUsage {
                input_tokens: Some(100),
                cached_input_tokens: Some(50),
                output_tokens: Some(200),
            }),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: CodexEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, CodexEvent::TurnCompleted { usage: Some(ref u) } if u.output_tokens == Some(200))
        );
    }

    // --- Terminal event detection ---

    #[test]
    fn terminal_event_turn_completed() {
        let json = r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":20}}"#;
        assert_eq!(is_codex_terminal_event(json), Some(true));
    }

    #[test]
    fn terminal_event_turn_failed() {
        let json = r#"{"type":"turn.failed","error":{"message":"oops"}}"#;
        assert_eq!(is_codex_terminal_event(json), Some(true));
    }

    #[test]
    fn terminal_event_item_completed_is_not_terminal() {
        let json =
            r#"{"type":"item.completed","item":{"type":"agent_message","id":"m1","text":"hi"}}"#;
        assert_eq!(is_codex_terminal_event(json), Some(false));
    }

    #[test]
    fn terminal_event_unknown_returns_none() {
        let json = r#"{"type":"something.else"}"#;
        assert_eq!(is_codex_terminal_event(json), None);
    }

    // --- Streaming-delta / plan / diff variants ---

    #[test]
    fn event_turn_diff_updated() {
        let json = r#"{"type":"turn/diff/updated","params":{"diff":"--- a/foo\n+++ b/foo\n@@ -1 +1 @@\n-bar\n+baz\n","threadId":"x","turnId":"y"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnDiffUpdated { params: Some(p) } => {
                assert!(p.diff.as_deref().unwrap().contains("+baz"));
                assert_eq!(p.thread_id.as_deref(), Some("x"));
                assert_eq!(p.turn_id.as_deref(), Some("y"));
            }
            other => panic!("expected TurnDiffUpdated, got {:?}", other),
        }
    }

    #[test]
    fn event_file_change_patch_updated_camel_case() {
        let json = r#"{"type":"item/fileChange/patchUpdated","params":{"changes":[{"path":"a.rs","kind":"update","diff":"--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n"}],"itemId":"i","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::FileChangePatchUpdated { params: Some(p) } => {
                let changes = p.changes.unwrap();
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].path.as_deref(), Some("a.rs"));
                assert!(changes[0].diff.as_deref().unwrap().contains("+new"));
            }
            other => panic!("expected FileChangePatchUpdated, got {:?}", other),
        }
    }

    #[test]
    fn event_turn_plan_updated() {
        let json = r#"{"type":"turn/plan/updated","params":{"plan":[{"status":"completed","step":"first"},{"status":"inProgress","step":"second"}],"explanation":"so far","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnPlanUpdated { params: Some(p) } => {
                let plan = p.plan.unwrap();
                assert_eq!(plan.len(), 2);
                assert_eq!(plan[0].status.as_deref(), Some("completed"));
                assert_eq!(plan[1].status.as_deref(), Some("inProgress"));
                assert_eq!(p.explanation.as_deref(), Some("so far"));
            }
            other => panic!("expected TurnPlanUpdated, got {:?}", other),
        }
    }

    #[test]
    fn event_plan_delta_typed_no_op() {
        let json = r#"{"type":"item/plan/delta","params":{"delta":"chunk","itemId":"i","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, CodexEvent::PlanDelta { .. }));
    }

    #[test]
    fn event_reasoning_summary_part_added_typed_no_op() {
        let json = r#"{"type":"item/reasoning/summaryPartAdded","params":{"itemId":"i","summaryIndex":0,"threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            CodexEvent::ReasoningSummaryPartAdded { .. }
        ));
    }

    #[test]
    fn event_reasoning_text_delta_typed_no_op() {
        let json = r#"{"type":"item/reasoning/textDelta","params":{"contentIndex":0,"delta":"...","itemId":"i","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, CodexEvent::ReasoningTextDelta { .. }));
    }
}

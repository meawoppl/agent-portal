use super::tool_card::tool_card;
use crate::components::expandable::ExpandableText;
use codex_codes::io::items::{
    CommandExecutionItem, CommandExecutionStatus, McpToolCallItem, McpToolCallStatus, TodoItem,
};
use codex_codes::protocol::{CollabAgentState, ReasoningEffort};
use serde_json::Value;
use std::collections::BTreeMap;
use yew::prelude::*;

/// Build the command-execution status meta: an outcome-colored label
/// (palette: `--success` / `--error` / `--warning`) with a ✓/✗ glyph that
/// pops in on completion. Styling + the fade live in `messages.css`
/// (`.codex-cmd-status` / `.codex-cmd-glyph`).
fn command_status_meta(it: &CommandExecutionItem, completed: bool) -> Html {
    // "running" with a CSS-animated ellipsis (. .. ... cycling); the empty
    // `.codex-running-dots` span is filled by the `codex-running-dots` keyframes.
    let running = || {
        html! {
            <span class="codex-cmd-status running">
                { "running" }<span class="codex-running-dots"></span>
            </span>
        }
    };
    if !completed {
        return running();
    }
    let (kind, glyph, label) = match it.exit_code {
        Some(0) => ("ok", "\u{2713}", "completed".to_string()),
        Some(code) => ("err", "\u{2717}", format!("exit {code}")),
        None => match it.status {
            CommandExecutionStatus::Completed => ("ok", "\u{2713}", "completed".to_string()),
            CommandExecutionStatus::Failed => ("err", "\u{2717}", "failed".to_string()),
            CommandExecutionStatus::Declined => ("err", "\u{2717}", "declined".to_string()),
            CommandExecutionStatus::InProgress => return running(),
        },
    };
    // `done` drives the pop-in; `ok`/`err` the palette color.
    html! {
        <span class={classes!("codex-cmd-status", "done", kind)}>
            <span class="codex-cmd-glyph">{ glyph }</span>
            { label }
        </span>
    }
}

pub(super) fn render_command_execution(it: &CommandExecutionItem, completed: bool) -> Html {
    let cmd = if it.command.is_empty() {
        "(unknown command)"
    } else {
        it.command.as_str()
    };
    let out = it.aggregated_output.as_deref().unwrap_or("");

    let is_error = it.exit_code.is_some_and(|c| c != 0);

    let body = html! {
        <>
            <ExpandableText
                full_text={cmd.to_string()}
                max_len=500
                tag="pre"
                class={classes!("tool-input-content")}
            />
            {
                if !out.is_empty() {
                    let class = if is_error { "tool-result error" } else { "tool-result" };
                    html! {
                        <div class={class}>
                            <ExpandableText
                                full_text={out.to_string()}
                                max_len=500
                                tag="pre"
                                class={classes!("tool-result-content")}
                            />
                        </div>
                    }
                } else {
                    html! {}
                }
            }
        </>
    };

    let status = command_status_meta(it, completed);
    tool_card("$", "Bash".into(), Some(status), body, completed)
}

pub(super) fn render_mcp_tool_call(it: &McpToolCallItem, completed: bool) -> Html {
    let server = if it.server.is_empty() {
        "(unknown)"
    } else {
        it.server.as_str()
    };
    let tool = if it.tool.is_empty() {
        "(unknown)"
    } else {
        it.tool.as_str()
    };
    let status = mcp_status_label(&it.status).to_string();
    tool_card(
        "\u{1f50c}",
        format!("{} / {}", server, tool),
        Some(html! { { status } }),
        html! {},
        completed,
    )
}

fn mcp_status_label(status: &McpToolCallStatus) -> &'static str {
    match status {
        McpToolCallStatus::InProgress => "in_progress",
        McpToolCallStatus::Completed => "completed",
        McpToolCallStatus::Failed => "failed",
    }
}

pub(super) fn render_web_search(query: &str, completed: bool) -> Html {
    let query = if query.is_empty() {
        "(no query)"
    } else {
        query
    };
    let body = html! { <pre class="tool-input-content">{ query }</pre> };
    tool_card("\u{1f50d}", "Web Search".into(), None, body, completed)
}

pub(super) fn render_todo_list(items: &[TodoItem], completed: bool) -> Html {
    if items.is_empty() {
        return html! {};
    }
    let body = html! {
        <div class="codex-todo-list">
            { for items.iter().map(|item| {
                let text = item.text.as_str();
                let done = item.completed;
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
    };
    tool_card("\u{2611}", "Todo List".into(), None, body, completed)
}

pub(super) fn render_collab_agent_tool_call(
    tool: &Value,
    model: Option<&str>,
    reasoning_effort: Option<&ReasoningEffort>,
    status: &Value,
    prompt: Option<&str>,
    agents_states: &BTreeMap<String, CollabAgentState>,
    completed: bool,
) -> Html {
    // Card title: "Spawn Agent" for the common spawnAgent tool, otherwise
    // surface the raw tool name so unrecognized collaboration tools still read.
    let tool_name = value_label(tool);
    let name = match tool_name.as_deref() {
        Some("spawnAgent") | None => "Spawn Agent".to_string(),
        Some(other) => format!("Agent: {}", other),
    };

    // Status line mirrors render_command_execution's composition: the item
    // status plus model + reasoning-effort meta when present.
    let mut status_text = value_label(status).unwrap_or_else(|| "running...".to_string());
    let mut meta_bits: Vec<String> = Vec::new();
    if let Some(model) = model.filter(|s| !s.is_empty()) {
        meta_bits.push(model.to_string());
    }
    if let Some(effort) = reasoning_effort
        .map(|effort| effort.0.as_str())
        .filter(|s| !s.is_empty())
    {
        meta_bits.push(format!("effort: {}", effort));
    }
    if !meta_bits.is_empty() {
        status_text = format!("{} \u{00b7} {}", status_text, meta_bits.join(" \u{00b7} "));
    }

    let prompt = prompt.unwrap_or("");

    let body = html! {
        <>
            {
                if !prompt.is_empty() {
                    html! {
                        <ExpandableText
                            full_text={prompt.to_string()}
                            max_len=500
                            tag="pre"
                            class={classes!("tool-input-content")}
                        />
                    }
                } else {
                    html! {}
                }
            }
            {
                if !agents_states.is_empty() {
                    html! {
                        <div class="codex-todo-list">
                            { for agents_states.iter().map(|(thread_id, state)| {
                                let state_label = serde_json::to_value(&state.status)
                                    .ok()
                                    .and_then(|value| value_label(&value))
                                    .unwrap_or_else(|| format!("{:?}", state.status));
                                html! {
                                    <div class="codex-todo">
                                        <span class="codex-todo-marker">{ "\u{1F916}" }</span>
                                        <span class="codex-todo-text">
                                            { format!("{} \u{2014} {}", thread_id, state_label) }
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
        </>
    };

    tool_card(
        "\u{1F916}",
        name,
        Some(html! { { status_text } }),
        body,
        completed,
    )
}

fn value_label(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Null => None,
        value => Some(value.to_string()),
    }
}

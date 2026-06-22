use super::events::CollabAgentToolCallItem;
use super::item_card_classes;
use crate::components::diff::{DiffCard, DiffSource};
use crate::components::expandable::ExpandableText;
use codex_codes::io::items::{
    CommandExecutionItem, CommandExecutionStatus, FileChangeItem, FileUpdateChange,
    McpToolCallItem, McpToolCallStatus, PatchApplyStatus, PatchChangeKind, TodoItem,
};
use yew::prelude::*;

/// Wraps a per-variant body in the standard tool-style card chrome:
/// card wrapper (with in-progress styling), message-body, tool-use-section,
/// and a tool-use-header with icon + name + optional `status` meta line.
/// Returns `html! {}` when `body` is empty so callers can short-circuit
/// empty-data cases by handing in a no-op body.
fn tool_card(icon: &str, name: String, status: Option<Html>, body: Html, completed: bool) -> Html {
    html! {
        <div class={item_card_classes(completed)}>
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ icon }</span>
                        <span class="tool-name">{ name }</span>
                        { if let Some(s) = status {
                            html! { <span class="tool-meta">{ s }</span> }
                        } else {
                            html! {}
                        } }
                    </div>
                    { body }
                </div>
            </div>
        </div>
    }
}

/// Build the command-execution status meta: an outcome-colored label
/// (palette: `--success` / `--error` / `--warning`) with a ✓/✗ glyph that
/// pops in on completion. Styling + the fade live in `messages.css`
/// (`.codex-cmd-status` / `.codex-cmd-glyph`).
fn command_status_meta(it: &CommandExecutionItem, completed: bool) -> Html {
    if !completed {
        return html! { <span class="codex-cmd-status running">{ "running\u{2026}" }</span> };
    }
    let (kind, glyph, label) = match it.exit_code {
        Some(0) => ("ok", "\u{2713}", "completed".to_string()),
        Some(code) => ("err", "\u{2717}", format!("exit {code}")),
        None => match it.status {
            CommandExecutionStatus::Completed => ("ok", "\u{2713}", "completed".to_string()),
            CommandExecutionStatus::Failed => ("err", "\u{2717}", "failed".to_string()),
            CommandExecutionStatus::Declined => ("err", "\u{2717}", "declined".to_string()),
            CommandExecutionStatus::InProgress => ("running", "", "running\u{2026}".to_string()),
        },
    };
    html! {
        <span class={classes!("codex-cmd-status", kind)}>
            { if glyph.is_empty() {
                html! {}
            } else {
                html! { <span class="codex-cmd-glyph">{ glyph }</span> }
            } }
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

fn patch_status_label(status: &PatchApplyStatus) -> &'static str {
    match status {
        PatchApplyStatus::InProgress => "in progress",
        PatchApplyStatus::Completed => "completed",
        PatchApplyStatus::Failed => "failed",
        PatchApplyStatus::Declined => "declined",
    }
}

/// Tagged-enum `PatchChangeKind` to a CSS suffix that pairs with the existing
/// `.diff-card-kind.{add,update,delete}` styles from #823's unified DiffCard.
fn patch_kind_css(kind: &PatchChangeKind) -> &'static str {
    match kind {
        PatchChangeKind::Add => "add",
        PatchChangeKind::Delete => "delete",
        PatchChangeKind::Update { .. } => "update",
    }
}

pub(super) fn render_file_change(it: &FileChangeItem, completed: bool) -> Html {
    if it.changes.is_empty() {
        return html! {};
    }
    let status_label = patch_status_label(&it.status).to_string();
    // Closes #827 part 2 — render the actual diff bodies through the unified
    // `<DiffCard>` from #823 instead of just chip + path. Each per-file
    // change becomes its own framed card with kind chip + path + diff body,
    // matching the layout of `render_file_change_patch`.
    let body = html! {
        <>
            { for it.changes.iter().map(render_diff_card) }
        </>
    };
    tool_card(
        "\u{1f4dd}",
        "File Changes".into(),
        Some(html! { { status_label } }),
        body,
        completed,
    )
}

/// Render one `FileUpdateChange` through the shared `<DiffCard>`. Returns
/// the bare path + kind chip (without a diff body) for empty-diff entries
/// — `item.started{file_change}` events typically carry the diff text
/// already, but a defensive empty-diff path still shows the file path.
pub(super) fn render_diff_card(c: &FileUpdateChange) -> Html {
    let kind_css = AttrValue::from(patch_kind_css(&c.kind));
    let path = AttrValue::from(c.path.clone());
    if c.diff.trim().is_empty() {
        return html! {
            <div class="diff-card">
                <div class="diff-card-header">
                    <span class="tool-icon">{ "\u{1f4dd}" }</span>
                    <span class={classes!("diff-card-kind", kind_css.to_string())}>{ kind_css.clone() }</span>
                    <span class="diff-card-path">{ path }</span>
                </div>
            </div>
        };
    }
    let source = DiffSource::Unified {
        text: c.diff.clone(),
    };
    html! {
        <DiffCard {source} file_path={path} kind={kind_css} />
    }
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

pub(super) fn render_collab_agent_tool_call(it: &CollabAgentToolCallItem, completed: bool) -> Html {
    // Card title: "Spawn Agent" for the common spawnAgent tool, otherwise
    // surface the raw tool name so unrecognized collaboration tools still read.
    let name = match it.tool.as_deref() {
        Some("spawnAgent") | None => "Spawn Agent".to_string(),
        Some(other) => format!("Agent: {}", other),
    };

    // Status line mirrors render_command_execution's composition: the item
    // status plus model + reasoning-effort meta when present.
    let mut status_text = it
        .status
        .clone()
        .unwrap_or_else(|| "running...".to_string());
    let mut meta_bits: Vec<String> = Vec::new();
    if let Some(model) = it.model.as_deref().filter(|s| !s.is_empty()) {
        meta_bits.push(model.to_string());
    }
    if let Some(effort) = it.reasoning_effort.as_deref().filter(|s| !s.is_empty()) {
        meta_bits.push(format!("effort: {}", effort));
    }
    if !meta_bits.is_empty() {
        status_text = format!("{} \u{00b7} {}", status_text, meta_bits.join(" \u{00b7} "));
    }

    let prompt = it.prompt.as_deref().unwrap_or("");

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
                if !it.agents_states.is_empty() {
                    html! {
                        <div class="codex-todo-list">
                            { for it.agents_states.iter().map(|(thread_id, state)| {
                                html! {
                                    <div class="codex-todo">
                                        <span class="codex-todo-marker">{ "\u{1F916}" }</span>
                                        <span class="codex-todo-text">
                                            { format!("{} \u{2014} {}", thread_id, state.status) }
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

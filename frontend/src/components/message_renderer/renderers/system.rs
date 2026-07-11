use super::super::shorten_model_name;
use crate::components::markdown::render_markdown;
use shared::fmt::format_duration;
use yew::prelude::*;

pub fn render_system_message(msg: &shared::SystemMessage, timestamp: Option<&str>) -> Html {
    let subtype = msg.subtype.as_str();

    if is_compaction_beginning(msg) {
        return render_compaction_beginning();
    }

    if subtype == "compact_boundary" {
        return render_compaction_completed(msg);
    }

    if subtype == "summary" || subtype == "compaction" || subtype == "context_compaction" {
        return render_compaction_completed(msg);
    }

    if subtype == "task_started" {
        return render_task_started(msg, timestamp);
    }

    if subtype == "task_progress" {
        return html! {};
    }

    if subtype == "task_notification" {
        return render_task_notification(msg, timestamp);
    }

    if subtype == "init" {
        return render_init_bar(msg, timestamp);
    }

    if subtype == "status" {
        return html! {};
    }

    // `thinking_tokens` markers are collapsed into a single counted chip by the
    // grouping layer (`GroupCategory::Thinking`), so they never reach here via a
    // group. Suppress any that slip through as a standalone `Single` rather than
    // rendering the bare-subtype fallback badge ("THINKING_TOKENS" × N noise).
    if subtype == "thinking_tokens" {
        return html! {};
    }

    html! {
        <div class="claude-message system-message compact" title={timestamp.unwrap_or_default().to_string()}>
            <span class="message-type-badge system">{ subtype }</span>
        </div>
    }
}

fn is_compaction_beginning(msg: &shared::SystemMessage) -> bool {
    msg.as_status()
        .and_then(|status| status.status)
        .is_some_and(|status| status.as_str() == "compacting")
}

fn render_init_bar(msg: &shared::SystemMessage, timestamp: Option<&str>) -> Html {
    let init = msg.as_init();
    let model_short = init
        .as_ref()
        .and_then(|m| m.model.as_deref())
        .and_then(shorten_model_name)
        .unwrap_or_default();
    let version = init
        .as_ref()
        .and_then(|m| m.claude_code_version.as_deref())
        .unwrap_or("");
    let tool_count = init.as_ref().map(|m| m.tools.len()).unwrap_or(0);
    let mcp_count = init.as_ref().map(|m| m.mcp_servers.len()).unwrap_or(0);
    let fast_mode = init
        .as_ref()
        .and_then(|m| m.fast_mode_state.as_deref())
        .unwrap_or("off");

    html! {
        <div class="claude-message system-message compact" title={timestamp.unwrap_or_default().to_string()}>
            <span class="message-type-badge system">{ "Session" }</span>
            if !model_short.is_empty() {
                <span class="init-badge">{ &model_short }</span>
            }
            if !version.is_empty() {
                <span class="init-badge">{ format!("v{}", version) }</span>
            }
            if fast_mode == "on" {
                <span class="init-badge fast">{ "Fast" }</span>
            }
            if mcp_count > 0 {
                <span class="init-badge">{ format!("{} MCP", mcp_count) }</span>
            }
            if tool_count > 0 {
                <span class="init-badge">{ format!("{} tools", tool_count) }</span>
            }
        </div>
    }
}

fn render_compaction_beginning() -> Html {
    html! {
        <div class="claude-message compaction-message compact">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Compaction Beginning" }</span>
            </div>
        </div>
    }
}

fn render_compaction_completed(msg: &shared::SystemMessage) -> Html {
    let compact = msg.as_compact_boundary();
    let summary_text = compact.as_ref().and_then(|c| c.summary.as_deref());
    let leaf_count = compact.as_ref().and_then(|c| c.leaf_message_count);
    let duration = compact.as_ref().and_then(|c| c.duration_ms);

    html! {
        <div class="claude-message compaction-message">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Compaction Completed" }</span>
                {
                    if let Some(count) = leaf_count {
                        html! {
                            <span class="compaction-stat" title="Messages summarized">
                                { format!("{} messages", count) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(ms) = duration {
                        html! {
                            <span class="compaction-stat" title="Compaction duration">
                                { format_duration(ms) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                <div class="compaction-content">
                    <div class="compaction-icon">{ "📦" }</div>
                    <div class="compaction-text">
                        {
                            if let Some(summary) = summary_text {
                                html! {
                                    <div class="compaction-summary">
                                        <div class="summary-label">{ "Summary:" }</div>
                                        <div class="summary-text">{ render_markdown(summary) }</div>
                                    </div>
                                }
                            } else {
                                html! {
                                    <div class="compaction-description">
                                        { "The conversation context has been summarized to free up space. Previous messages have been condensed while preserving important context." }
                                    </div>
                                }
                            }
                        }
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_task_started(msg: &shared::SystemMessage, timestamp: Option<&str>) -> Html {
    let task = msg.as_task_started();
    let description = task
        .as_ref()
        .map(|t| t.description.as_str())
        .unwrap_or("Background task");
    let task_id = task.as_ref().map(|t| t.task_id.as_str()).unwrap_or("");

    let type_label = task
        .as_ref()
        .and_then(|t| t.task_type.clone())
        .map(|tt| match tt {
            shared::TaskType::LocalAgent => "Sub-agent",
            shared::TaskType::LocalBash => "Background Bash",
            // Open enum (2.1.160): unrecognized task types render generically.
            _ => "Task",
        })
        .unwrap_or("Task");

    html! {
        <div class="claude-message task-message compact" title={format!("Task ID: {}", task_id)}>
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge task">{ "Task Started" }</span>
                <span class="task-type-badge">{ type_label }</span>
                <span class="task-description-inline">{ description }</span>
            </div>
        </div>
    }
}

fn render_task_notification(msg: &shared::SystemMessage, timestamp: Option<&str>) -> Html {
    let notif = msg.as_task_notification();

    let summary_text = notif.as_ref().map(|n| n.summary.as_str());
    let task_id = notif.as_ref().map(|n| n.task_id.as_str()).unwrap_or("");
    let duration = notif
        .as_ref()
        .and_then(|n| n.usage.as_ref())
        .map(|u| u.duration_ms);
    let tool_uses = notif
        .as_ref()
        .and_then(|n| n.usage.as_ref())
        .map(|u| u.tool_uses);
    let total_tokens = notif
        .as_ref()
        .and_then(|n| n.usage.as_ref())
        .map(|u| u.total_tokens);

    let is_failed = matches!(
        notif.as_ref().map(|n| &n.status),
        Some(shared::TaskStatus::Failed)
    );
    let status_class = if is_failed { "failed" } else { "completed" };

    html! {
        <div class={classes!("claude-message", "task-message", status_class)}
             title={format!("Task ID: {}", task_id)}>
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class={classes!("message-type-badge", "task", status_class)}>
                    { if is_failed { "Task Failed" } else { "Task Completed" } }
                </span>
                {
                    if let Some(ms) = duration {
                        html! { <span class="task-stat">{ format_duration(ms) }</span> }
                    } else { html! {} }
                }
                {
                    if let Some(tools) = tool_uses {
                        html! { <span class="task-stat" title="Tool calls">{ format!("{} tools", tools) }</span> }
                    } else { html! {} }
                }
                {
                    if let Some(tokens) = total_tokens {
                        html! { <span class="task-stat" title="Total tokens">{ format!("{}k tokens", tokens / 1000) }</span> }
                    } else { html! {} }
                }
            </div>
            {
                if let Some(summary) = summary_text {
                    html! {
                        <div class="message-body">
                            <div class="task-summary">{ render_markdown(summary) }</div>
                        </div>
                    }
                } else { html! {} }
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system_message(json: serde_json::Value) -> shared::SystemMessage {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn compaction_beginning_uses_typed_status_accessor() {
        let msg = system_message(serde_json::json!({
            "type": "system",
            "subtype": "status",
            "status": "compacting",
            "session_id": "s-1"
        }));

        assert!(is_compaction_beginning(&msg));
    }

    #[test]
    fn non_compacting_status_is_not_compaction_beginning() {
        let msg = system_message(serde_json::json!({
            "type": "system",
            "subtype": "status",
            "status": null,
            "session_id": "s-1"
        }));

        assert!(!is_compaction_beginning(&msg));
    }

    #[test]
    fn compact_boundary_is_not_compaction_beginning() {
        let msg = system_message(serde_json::json!({
            "type": "system",
            "subtype": "compact_boundary",
            "summary": "trimmed",
            "compact_metadata": {
                "trigger": "auto",
                "pre_tokens": 123
            }
        }));

        assert!(!is_compaction_beginning(&msg));
    }
}

use super::super::types::SystemMessage;
use super::super::{format_duration, shorten_model_name};
use crate::components::markdown::render_markdown;
use yew::prelude::*;

pub fn render_system_message(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    let subtype = msg.subtype.as_deref().unwrap_or("system");

    // Check if this is a compaction-related message via subtype or status field
    let status_value = msg
        .extra
        .as_ref()
        .and_then(|v| v.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("");

    if status_value == "compacting" {
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

    html! {
        <div class="claude-message system-message compact" title={timestamp.unwrap_or_default().to_string()}>
            <span class="message-type-badge system">{ subtype }</span>
        </div>
    }
}

fn render_init_bar(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    let model_short = msg
        .model
        .as_deref()
        .and_then(shorten_model_name)
        .unwrap_or_default();
    let version = msg.claude_code_version.as_deref().unwrap_or("");
    let tool_count = msg.tools.as_ref().map(|t| t.len()).unwrap_or(0);
    let mcp_count = msg.mcp_servers.as_ref().map(|s| s.len()).unwrap_or(0);
    // Typed dispatch over `extra` (closes #752). `InitExtra::fast_mode_state`
    // mirrors `claude_codes::InitMessage::fast_mode_state` (already typed
    // upstream); we use a narrow local mirror because the SDK's full
    // `InitMessage` has many required fields and a partial frame here would
    // otherwise fail to deserialize.
    let init_extra: shared::InitExtra = msg
        .extra
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let fast_mode = init_extra.fast_mode_state.as_deref().unwrap_or("off");

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

fn render_compaction_completed(msg: &SystemMessage) -> Html {
    // Typed dispatch over `extra` (closes #752). The SDK's
    // `CompactBoundaryMessage` currently only exposes
    // `compact_metadata { pre_tokens, trigger }` - not the `summary` /
    // `leaf_message_count` / `duration_ms` fields the renderer needs.
    // TODO(SDK rust-code-agent-sdks#141): drop the local `CompactionExtra`
    // mirror once upstream adds these fields to `CompactBoundaryMessage`,
    // and switch to `CCSystemMessage::as_compact_boundary()` here.
    let compact_extra: shared::CompactionExtra = msg
        .extra
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let summary_text = msg
        .summary
        .as_deref()
        .or_else(|| compact_extra.summary_text());
    let leaf_count = msg
        .leaf_message_count
        .or_else(|| compact_extra.message_count());
    let duration = msg.duration_ms.or(compact_extra.duration_ms);

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

fn render_task_started(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    let extra = msg.extra.as_ref();
    let description = extra
        .and_then(|v| v.get("description").and_then(|d| d.as_str()))
        .unwrap_or("Background task");
    let task_id = extra
        .and_then(|v| v.get("task_id").and_then(|t| t.as_str()))
        .unwrap_or("");

    let type_label = extra
        .and_then(|v| v.get("task_type"))
        .and_then(|v| serde_json::from_value::<shared::CCTaskType>(v.clone()).ok())
        .map(|tt| match tt {
            shared::CCTaskType::LocalAgent => "Sub-agent",
            shared::CCTaskType::LocalBash => "Background Bash",
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

fn render_task_notification(msg: &SystemMessage, timestamp: Option<&str>) -> Html {
    // Typed dispatch over `extra` (closes #752). `TaskNotificationExtra`
    // mirrors the renderable subset of `claude_codes::TaskNotificationMessage`
    // (the SDK type's required `session_id` / `summary` are already consumed
    // by the outer lenient `SystemMessage`'s typed top-level fields and would
    // not appear in the flattened `extra` Value).
    let notif: shared::TaskNotificationExtra = msg
        .extra
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let summary_text = msg.summary.as_deref();
    let task_id = notif.task_id.as_deref().unwrap_or("");
    let duration = notif.usage.as_ref().map(|u| u.duration_ms);
    let tool_uses = notif.usage.as_ref().map(|u| u.tool_uses);
    let total_tokens = notif.usage.as_ref().map(|u| u.total_tokens);

    let is_failed = matches!(notif.status, Some(shared::CCTaskStatus::Failed));
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

use super::events::{ContextCompactedParams, TurnPlanStep};
use yew::prelude::*;

pub(super) fn render_turn_plan(plan: Option<&[TurnPlanStep]>, explanation: Option<&str>) -> Html {
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

pub(super) fn render_context_compacted(params: Option<&ContextCompactedParams>) -> Html {
    let title = params
        .and_then(|p| p.turn_id.as_deref())
        .map(|turn_id| format!("Codex compacted context for turn {}", turn_id))
        .unwrap_or_else(|| "Codex compacted the conversation context".to_string());

    html! {
        <div class="claude-message compaction-message">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Context Compacted" }</span>
            </div>
            <div class="message-body">
                <div class="compaction-content">
                    <div class="compaction-icon">{ "\u{1f4e6}" }</div>
                    <div class="compaction-text">
                        <div class="compaction-description">{ title }</div>
                    </div>
                </div>
            </div>
        </div>
    }
}

pub(super) fn render_context_compaction_item(completed: bool) -> Html {
    let title = if completed {
        "Codex compacted the conversation context"
    } else {
        "Codex is compacting the conversation context"
    };

    html! {
        <div class="claude-message compaction-message">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Context Compaction" }</span>
            </div>
            <div class="message-body">
                <div class="compaction-content">
                    <div class="compaction-icon">{ "\u{1f4e6}" }</div>
                    <div class="compaction-text">
                        <div class="compaction-description">{ title }</div>
                    </div>
                </div>
            </div>
        </div>
    }
}

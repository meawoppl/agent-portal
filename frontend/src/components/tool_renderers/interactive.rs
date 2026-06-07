use serde_json::Value;
use shared::{AllowedPrompt, AskUserQuestionInput, ExitPlanModeInput, TodoStatus, TodoWriteInput};
use yew::prelude::*;

use crate::components::tool_renderers::extract_tool_input;

pub fn render_todowrite_tool(input: &Value) -> Html {
    let todos = extract_tool_input::<TodoWriteInput>(input)
        .map(|tw| tw.todos)
        .unwrap_or_default();

    html! {
        <div class="tool-use todowrite-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "📋" }</span>
                <span class="tool-name">{ "TodoWrite" }</span>
                <span class="tool-meta">{ format!("({} items)", todos.len()) }</span>
            </div>
            <div class="todo-list">
                {
                    todos.iter().map(|todo| {
                        let (icon, class) = match &todo.status {
                            TodoStatus::Completed => ("✓", "completed"),
                            TodoStatus::InProgress => ("→", "in-progress"),
                            TodoStatus::Pending | TodoStatus::Unknown(_) => ("○", "pending"),
                        };
                        html! {
                            <div class={format!("todo-item {}", class)}>
                                <span class="todo-status">{ icon }</span>
                                <span class="todo-content">{ &todo.content }</span>
                            </div>
                        }
                    }).collect::<Html>()
                }
            </div>
        </div>
    }
}

pub fn render_askuserquestion_tool(input: &Value) -> Html {
    let parsed =
        extract_tool_input::<AskUserQuestionInput>(input).unwrap_or(AskUserQuestionInput {
            questions: Vec::new(),
            answers: None,
            metadata: None,
        });

    let answers = parsed.answers.as_ref();
    if !has_askuserquestion_answers(&parsed) {
        return html! {};
    }

    let questions = &parsed.questions;

    html! {
        <div class="tool-use askuserquestion-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "❓" }</span>
                <span class="tool-name">{ "AskUserQuestion" }</span>
                <span class="tool-meta">{ format!("({} question{})", questions.len(), if questions.len() == 1 { "" } else { "s" }) }</span>
            </div>
            <div class="question-list">
                {
                    questions.iter().map(|q| {
                        let header = q.header.as_str();
                        let question = q.question.as_str();
                        let multi_select = q.multi_select;
                        let options = &q.options;

                        let answer = answers
                            .and_then(|a| a.get(question))
                            .or_else(|| answers.and_then(|a| a.get(header)))
                            .map(|s| s.as_str());

                        html! {
                            <div class="question-card">
                                <div class="question-header">
                                    {
                                        if !header.is_empty() {
                                            html! { <span class="question-badge">{ header }</span> }
                                        } else {
                                            html! {}
                                        }
                                    }
                                    {
                                        if multi_select {
                                            html! { <span class="multi-select-badge">{ "multi-select" }</span> }
                                        } else {
                                            html! {}
                                        }
                                    }
                                </div>
                                <div class="question-text">{ question }</div>
                                <div class="question-options">
                                    {
                                        options.iter().map(|opt| {
                                            let label = opt.label.as_str();
                                            let description = opt.description.as_deref().unwrap_or("");

                                            let is_selected = answer.map(|a| {
                                                a.split(',').map(|s| s.trim()).any(|s| s == label)
                                            }).unwrap_or(false);

                                            let option_class = if is_selected { "option-item selected" } else { "option-item" };
                                            let icon = if is_selected {
                                                if multi_select { "☑" } else { "●" }
                                            } else if multi_select {
                                                "☐"
                                            } else {
                                                "○"
                                            };

                                            html! {
                                                <div class={option_class}>
                                                    <span class="option-icon">{ icon }</span>
                                                    <div class="option-content">
                                                        <span class="option-label">{ label }</span>
                                                        {
                                                            if !description.is_empty() {
                                                                html! { <span class="option-description">{ description }</span> }
                                                            } else {
                                                                html! {}
                                                            }
                                                        }
                                                    </div>
                                                </div>
                                            }
                                        }).collect::<Html>()
                                    }
                                </div>
                                {
                                    if let Some(ans) = answer {
                                        html! {
                                            <div class="question-answer">
                                                <span class="answer-label">{ "Answer: " }</span>
                                                <span class="answer-value">{ ans }</span>
                                            </div>
                                        }
                                    } else {
                                        html! {}
                                    }
                                }
                            </div>
                        }
                    }).collect::<Html>()
                }
            </div>
        </div>
    }
}

pub fn render_askuserquestion_result(input: &AskUserQuestionInput) -> Html {
    if !has_askuserquestion_answers(input) {
        return html! {};
    }

    let value = serde_json::to_value(input).unwrap_or(Value::Null);
    render_askuserquestion_tool(&value)
}

pub fn has_askuserquestion_answers(input: &AskUserQuestionInput) -> bool {
    input
        .answers
        .as_ref()
        .map(|answers| !answers.is_empty())
        .unwrap_or(false)
}

pub fn render_exitplanmode_tool(input: &Value) -> Html {
    // Decode the per-tool input typed instead of JSON-poking field names.
    // `input` stays a `serde_json::Value` envelope (per-tool inputs have
    // different shapes); typed decoding happens at this dispatch site and
    // pulls the `ExitPlanMode` variant's `allowed_prompts` directly. Mirrors
    // the dispatch in `frontend/src/pages/dashboard/permission_dialog.rs`
    // (landed in #740).
    let allowed_prompts: Vec<AllowedPrompt> = extract_tool_input::<ExitPlanModeInput>(input)
        .and_then(|epm| epm.allowed_prompts)
        .unwrap_or_default();

    html! {
        <div class="tool-use exitplanmode-tool">
            <div class="tool-use-header">
                <span class="tool-icon">{ "📋" }</span>
                <span class="tool-name">{ "Plan Complete" }</span>
            </div>
            {
                if !allowed_prompts.is_empty() {
                    html! {
                        <div class="permissions-section">
                            <div class="permissions-header">{ "Requested Permissions:" }</div>
                            <div class="permissions-list">
                                {
                                    allowed_prompts.iter().map(|p| {
                                        html! {
                                            <div class="permission-item">
                                                <span class="permission-bullet">{ "•" }</span>
                                                <span class="permission-tool">{ &p.tool }</span>
                                                <span class="permission-separator">{ ": " }</span>
                                                <span class="permission-prompt">{ &p.prompt }</span>
                                            </div>
                                        }
                                    }).collect::<Html>()
                                }
                            </div>
                        </div>
                    }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

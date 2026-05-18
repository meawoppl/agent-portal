use serde_json::Value;
use shared::{AskUserQuestionInput, TodoItem, TodoStatus, ToolInput};
use yew::prelude::*;

pub fn render_todowrite_tool(input: &Value) -> Html {
    let todos: Vec<TodoItem> = serde_json::from_value::<ToolInput>(input.clone())
        .ok()
        .and_then(|t| match t {
            ToolInput::TodoWrite(tw) => Some(tw.todos),
            _ => None,
        })
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
    let parsed: AskUserQuestionInput = serde_json::from_value::<ToolInput>(input.clone())
        .ok()
        .and_then(|t| match t {
            ToolInput::AskUserQuestion(a) => Some(a),
            _ => None,
        })
        .unwrap_or(AskUserQuestionInput {
            questions: Vec::new(),
            answers: None,
            metadata: None,
        });

    let answers = parsed.answers.as_ref();
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

pub fn render_exitplanmode_tool(input: &Value) -> Html {
    let allowed_prompts = input
        .get("allowedPrompts")
        .and_then(|v| v.as_array())
        .cloned()
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
                                        let tool = p.get("tool").and_then(|t| t.as_str()).unwrap_or("Unknown");
                                        let prompt = p.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
                                        html! {
                                            <div class="permission-item">
                                                <span class="permission-bullet">{ "•" }</span>
                                                <span class="permission-tool">{ tool }</span>
                                                <span class="permission-separator">{ ": " }</span>
                                                <span class="permission-prompt">{ prompt }</span>
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

//! Permission dialog components for tool authorization and user questions

use std::collections::{HashMap, HashSet};
use web_sys::{HtmlInputElement, KeyboardEvent};
use yew::prelude::*;

use shared::{AllowedPrompt, ToolInput};

use super::types::{
    format_permission_input, parse_ask_user_question, AskUserQuestionInput, PendingPermission,
    QuestionAnswers,
};

/// Props for the PermissionDialog component
#[derive(Properties, PartialEq)]
pub struct PermissionDialogProps {
    /// The pending permission request to display
    pub permission: PendingPermission,
    /// Currently selected option index (for single-question or standard permissions)
    pub selected: usize,
    /// For multi-select questions: which options are selected (per question)
    /// Key is question index, value is set of selected option indices
    #[prop_or_default]
    pub multi_select_options: HashMap<usize, HashSet<usize>>,
    /// Answers for each question (for multi-question AskUserQuestion)
    /// Key is question index, value is the selected answer
    #[prop_or_default]
    pub question_answers: QuestionAnswers,
    /// Reference to the dialog for focus management
    pub dialog_ref: NodeRef,
    /// Callback when user navigates up
    pub on_select_up: Callback<()>,
    /// Callback when user navigates down
    pub on_select_down: Callback<()>,
    /// Callback when user confirms selection
    pub on_confirm: Callback<()>,
    /// Callback when user selects and confirms an option by index (for click)
    pub on_select_and_confirm: Callback<usize>,
    /// Callback when user submits all answers (sends HashMap of question->answer)
    pub on_submit_answers: Callback<QuestionAnswers>,
    /// Callback when user selects an answer for a specific question
    /// (question_index, answer)
    pub on_set_answer: Callback<(usize, String)>,
    /// Callback to toggle a multi-select option for a specific question
    /// (question_index, option_index)
    pub on_toggle_option: Callback<(usize, usize)>,
}

/// Permission dialog component - handles both regular permissions and AskUserQuestion
#[function_component(PermissionDialog)]
pub fn permission_dialog(props: &PermissionDialogProps) -> Html {
    let perm = &props.permission;

    // Check if this is an AskUserQuestion
    if perm.tool_name == "AskUserQuestion" {
        if let Some(parsed) = parse_ask_user_question(&perm.input) {
            return render_ask_user_question(props, &parsed);
        }
    }

    // Check if this is ExitPlanMode
    if perm.tool_name == "ExitPlanMode" {
        return render_exitplanmode_permission(props);
    }

    // Regular permission dialog
    render_standard_permission(props)
}

/// Build the keyboard navigation callback shared by permission dialogs
/// (arrow/vim keys to move the selection, Enter/Space to confirm)
fn nav_keydown(props: &PermissionDialogProps) -> Callback<KeyboardEvent> {
    let on_select_up = props.on_select_up.clone();
    let on_select_down = props.on_select_down.clone();
    let on_confirm = props.on_confirm.clone();

    Callback::from(move |e: KeyboardEvent| match e.key().as_str() {
        "ArrowUp" | "k" => {
            e.prevent_default();
            on_select_up.emit(());
        }
        "ArrowDown" | "j" => {
            e.prevent_default();
            on_select_down.emit(());
        }
        "Enter" | " " => {
            e.prevent_default();
            on_confirm.emit(());
        }
        _ => {}
    })
}

/// Render the selectable option rows shared by permission dialogs
fn render_options(props: &PermissionDialogProps, options: &[(&str, &str)]) -> Html {
    options
        .iter()
        .enumerate()
        .map(|(i, (class, label))| {
            let is_selected = i == props.selected;
            let cursor = if is_selected { ">" } else { " " };
            let item_class = if is_selected {
                format!("permission-option selected {}", class)
            } else {
                format!("permission-option {}", class)
            };
            let on_select_and_confirm = props.on_select_and_confirm.clone();
            let onclick = Callback::from(move |_| {
                on_select_and_confirm.emit(i);
            });
            html! {
                <div class={item_class} {onclick}>
                    <span class="option-cursor">{ cursor }</span>
                    <span class="option-label">{ *label }</span>
                </div>
            }
        })
        .collect::<Html>()
}

/// Render the standard permission dialog (Allow/Deny)
fn render_standard_permission(props: &PermissionDialogProps) -> Html {
    let perm = &props.permission;
    let input_preview = format_permission_input(&perm.tool_name, &perm.input);
    let has_suggestions = !perm.permission_suggestions.is_empty();

    let onkeydown = nav_keydown(props);

    // Build options list
    let options: Vec<(&str, &str)> = if has_suggestions {
        vec![
            ("allow", "Allow"),
            ("remember", "Allow & Remember"),
            ("deny", "Deny"),
        ]
    } else {
        vec![("allow", "Allow"), ("deny", "Deny")]
    };

    html! {
        <div
            class="permission-prompt"
            ref={props.dialog_ref.clone()}
            tabindex="0"
            {onkeydown}
        >
            <div class="permission-header">
                <span class="permission-icon">{ "⚠️" }</span>
                <span class="permission-title">{ "Permission Required" }</span>
            </div>
            <div class="permission-body">
                <div class="permission-tool">
                    <span class="tool-label">{ "Tool:" }</span>
                    <span class="tool-name">{ &perm.tool_name }</span>
                </div>
                <div class="permission-input">
                    <pre>{ input_preview }</pre>
                </div>
            </div>
            <div class="permission-options">
                { render_options(props, &options) }
            </div>
            <div class="permission-hint">
                { "↑↓ or tap to select" }
            </div>
        </div>
    }
}

/// Render the AskUserQuestion specialized UI - supports multiple questions
/// Free-text "Other" answer row for [`render_ask_user_question`].
///
/// Deliberately its own keyed component. The field used to live inline in
/// `render_ask_user_question` — a plain `fn -> Html` the parent re-renders on
/// every keystroke. As a controlled `<input value={…}>` re-derived from parent
/// props each render, the DOM node was rebuilt per keystroke, so focus (and the
/// caret) bounced out of the field.
///
/// Owning the draft in local `use_state` keeps the node stable: keystrokes
/// touch local state only, the parent is notified via `on_input`, and the field
/// re-seeds from `initial` solely when the answer changes from the outside
/// (e.g. the user clicks a preset option, which clears the draft).
#[derive(Properties, PartialEq)]
struct CustomAnswerInputProps {
    /// The custom answer text, or empty when a preset option/multi-select join
    /// is the active selection. Re-seeds the field only on external changes.
    initial: String,
    /// Whether the typed answer is the active selection (drives the icon).
    selected: bool,
    /// Emits the typed text upward (the parent binds the question index).
    on_input: Callback<String>,
}

#[function_component(CustomAnswerInput)]
fn custom_answer_input(props: &CustomAnswerInputProps) -> Html {
    let draft = use_state(|| props.initial.clone());

    // Re-seed only when `initial` changes from the outside (selecting a preset
    // clears it). While typing, `draft` and `initial` stay in lockstep, so this
    // is a no-op on keystrokes and never fights the user's caret.
    {
        let draft = draft.clone();
        use_effect_with(props.initial.clone(), move |initial| {
            if *draft != *initial {
                draft.set(initial.clone());
            }
            || ()
        });
    }

    let oninput = {
        let draft = draft.clone();
        let on_input = props.on_input.clone();
        Callback::from(move |e: InputEvent| {
            let value = e.target_unchecked_into::<HtmlInputElement>().value();
            draft.set(value.clone());
            on_input.emit(value);
        })
    };

    let row_class = if props.selected {
        "question-option custom selected"
    } else {
        "question-option custom"
    };
    let icon = if props.selected { "●" } else { "○" };

    html! {
        <div class={row_class}>
            <span class="option-icon">{ icon }</span>
            <div class="option-content">
                <input
                    type="text"
                    class="question-custom-input"
                    placeholder="Something else…"
                    value={(*draft).clone()}
                    {oninput}
                />
            </div>
        </div>
    }
}

fn render_ask_user_question(props: &PermissionDialogProps, parsed: &AskUserQuestionInput) -> Html {
    let total_questions = parsed.questions.len();
    let answers_count = props.question_answers.len();

    // Check if all questions have been answered
    let all_answered = answers_count >= total_questions;

    // For keyboard navigation, we don't use the standard up/down since we have multiple questions
    let on_submit = props.on_submit_answers.clone();
    let answers_for_submit = props.question_answers.clone();

    let onkeydown = Callback::from(move |e: KeyboardEvent| {
        // Only handle Enter to submit when all answered
        if e.key() == "Enter" && answers_for_submit.len() >= total_questions {
            e.prevent_default();
            on_submit.emit(answers_for_submit.clone());
        }
    });

    // Prepare submit button callback
    let on_submit_click = props.on_submit_answers.clone();
    let answers_for_button = props.question_answers.clone();
    let submit_onclick = Callback::from(move |_| {
        on_submit_click.emit(answers_for_button.clone());
    });
    let button_text = if all_answered {
        format!(
            "Submit {} Answer{}",
            answers_count,
            if answers_count == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "Answer {} more question{}",
            total_questions - answers_count,
            if total_questions - answers_count == 1 {
                ""
            } else {
                "s"
            }
        )
    };

    html! {
        <div
            class="permission-prompt ask-user-question"
            ref={props.dialog_ref.clone()}
            tabindex="0"
            {onkeydown}
        >
            {
                parsed.questions.iter().enumerate().map(|(q_idx, q)| {
                    let is_multi = q.multi_select;
                    let current_answer = props.question_answers.get(&q_idx);
                    let is_answered = current_answer.is_some();
                    let multi_selected = props.multi_select_options.get(&q_idx).cloned().unwrap_or_default();

                    let question_class = if is_answered {
                        "question-container answered"
                    } else {
                        "question-container"
                    };

                    html! {
                        <div class={question_class}>
                            <div class="question-header-badge">
                                {
                                    if !q.header.is_empty() {
                                        html! { <span class="badge">{ &q.header }</span> }
                                    } else {
                                        html! {}
                                    }
                                }
                                {
                                    if is_multi {
                                        html! { <span class="multi-badge">{ "multi-select" }</span> }
                                    } else {
                                        html! {}
                                    }
                                }
                                {
                                    if let Some(answer) = current_answer {
                                        html! { <span class="answer-badge">{ format!("✓ {}", answer) }</span> }
                                    } else {
                                        html! {}
                                    }
                                }
                            </div>
                            <div class="question-text">{ &q.question }</div>
                            <div class="question-options">
                                {
                                    q.options.iter().enumerate().map(|(opt_idx, opt)| {
                                        let is_selected = if is_multi {
                                            multi_selected.contains(&opt_idx)
                                        } else {
                                            // For single-select, check if this is the current answer
                                            current_answer.map(|a| a == &opt.label).unwrap_or(false)
                                        };
                                        let item_class = if is_selected {
                                            "question-option selected"
                                        } else {
                                            "question-option"
                                        };
                                        let label_clone = opt.label.clone();
                                        let on_set_answer = props.on_set_answer.clone();
                                        let on_toggle = props.on_toggle_option.clone();
                                        let onclick = if is_multi {
                                            Callback::from(move |_| on_toggle.emit((q_idx, opt_idx)))
                                        } else {
                                            Callback::from(move |_| on_set_answer.emit((q_idx, label_clone.clone())))
                                        };
                                        let icon = if is_selected {
                                            if is_multi { "☑" } else { "●" }
                                        } else if is_multi {
                                            "☐"
                                        } else {
                                            "○"
                                        };

                                        html! {
                                            <div class={item_class} onclick={onclick}>
                                                <span class="option-icon">{ icon }</span>
                                                <div class="option-content">
                                                    <span class="option-label">{ &opt.label }</span>
                                                    {
                                                        if !opt.description.is_empty() {
                                                            html! { <span class="option-description">{ &opt.description }</span> }
                                                        } else {
                                                            html! {}
                                                        }
                                                    }
                                                </div>
                                            </div>
                                        }
                                    }).collect::<Html>()
                                }
                                { {
                                    // Free-text escape hatch so users aren't railroaded into
                                    // the preset options (mirrors AskUserQuestion's built-in
                                    // "Other"). Covers Claude and Codex, which share this frame.
                                    // The answer is whatever is typed; it's "selected" when the
                                    // current answer matches no option (single) or the toggled
                                    // multi-select join. Rendered via the keyed CustomAnswerInput
                                    // so typing doesn't tear down the field (see its doc comment).
                                    let joined_multi: String = multi_selected
                                        .iter()
                                        .filter_map(|&i| q.options.get(i).map(|o| o.label.clone()))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    let matches_option =
                                        q.options.iter().any(|o| current_answer == Some(&o.label));
                                    let matches_multi = !joined_multi.is_empty()
                                        && current_answer.map(|a| a == &joined_multi).unwrap_or(false);
                                    let custom_value = match current_answer {
                                        Some(a) if !matches_option && !matches_multi => a.clone(),
                                        _ => String::new(),
                                    };
                                    let custom_selected = !custom_value.is_empty();
                                    let on_set_answer = props.on_set_answer.clone();
                                    let on_input = Callback::from(move |value: String| {
                                        on_set_answer.emit((q_idx, value));
                                    });
                                    html! {
                                        <CustomAnswerInput
                                            key={format!("custom-{q_idx}")}
                                            initial={custom_value}
                                            selected={custom_selected}
                                            {on_input}
                                        />
                                    }
                                } }
                            </div>
                            {
                                // For multi-select questions, show a "Set Answer" button
                                if is_multi && !multi_selected.is_empty() {
                                    let options_clone = q.options.clone();
                                    let multi_select_clone = multi_selected.clone();
                                    let on_set_answer = props.on_set_answer.clone();
                                    let onclick = Callback::from(move |_| {
                                        // Build comma-separated answer from selected indices
                                        let answer: String = multi_select_clone
                                            .iter()
                                            .filter_map(|&idx| options_clone.get(idx).map(|o| o.label.clone()))
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        on_set_answer.emit((q_idx, answer));
                                    });
                                    html! {
                                        <button class="set-answer-btn" {onclick}>
                                            { "Set Answer" }
                                        </button>
                                    }
                                } else {
                                    html! {}
                                }
                            }
                        </div>
                    }
                }).collect::<Html>()
            }
            <div class="question-submit-section">
                <button
                    class="submit-all-answers"
                    onclick={submit_onclick}
                    disabled={!all_answered}
                >
                    { button_text }
                </button>
                <div class="question-hint">
                    { "Click options to answer each question, then submit" }
                </div>
            </div>
        </div>
    }
}

/// Render the ExitPlanMode permission dialog
fn render_exitplanmode_permission(props: &PermissionDialogProps) -> Html {
    let perm = &props.permission;

    let onkeydown = nav_keydown(props);

    // Decode the per-tool input typed instead of JSON-poking field names.
    // `perm.input` stays a `serde_json::Value` envelope (it carries different shapes per tool);
    // we parse to the typed `ToolInput` enum at this dispatch site and pull the
    // `ExitPlanMode` variant's `allowed_prompts` directly.
    let allowed_prompts: Vec<AllowedPrompt> =
        serde_json::from_value::<ToolInput>(perm.input.clone())
            .ok()
            .and_then(|t| match t {
                ToolInput::ExitPlanMode(epm) => epm.allowed_prompts,
                _ => None,
            })
            .unwrap_or_default();

    let options: Vec<(&str, &str)> = vec![("allow", "Allow"), ("deny", "Deny")];

    html! {
        <div
            class="permission-prompt exitplanmode-permission"
            ref={props.dialog_ref.clone()}
            tabindex="0"
            {onkeydown}
        >
            <div class="permission-header">
                <span class="permission-icon">{ "📋" }</span>
                <span class="permission-title">{ "Plan Ready" }</span>
            </div>
            <div class="permission-body">
                {
                    if !allowed_prompts.is_empty() {
                        html! {
                            <div class="exitplan-permissions">
                                <div class="exitplan-permissions-header">{ "Requested permissions:" }</div>
                                {
                                    allowed_prompts.iter().map(|p| html! {
                                        <div class="exitplan-permission-item">
                                            <span class="permission-tool-name">{ &p.tool }</span>
                                            <span class="permission-separator">{ ": " }</span>
                                            <span class="permission-description">{ &p.prompt }</span>
                                        </div>
                                    }).collect::<Html>()
                                }
                            </div>
                        }
                    } else {
                        html! {
                            <div class="exitplan-no-permissions">
                                { "No additional permissions requested." }
                            </div>
                        }
                    }
                }
            </div>
            <div class="permission-options">
                { render_options(props, &options) }
            </div>
            <div class="permission-hint">
                { "↑↓ or tap to select" }
            </div>
        </div>
    }
}

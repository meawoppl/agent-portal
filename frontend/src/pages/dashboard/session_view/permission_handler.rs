//! `PermissionHandler` — sub-component owning permission-request UI state.
//!
//! Pulled out of `SessionView` so the parent component no longer carries the
//! selection index / multi-select set / per-question answer map. The parent
//! keeps the WebSocket plumbing: when a `ServerToClient::PermissionRequest`
//! arrives, it forwards the typed `PendingPermission` into this handler via
//! the dispatcher callback registered at mount; when the user answers, this
//! handler emits a typed `PermissionResponseKind` and the parent translates
//! that into the matching `ClientToServer::PermissionResponse` frame.

use std::collections::{HashMap, HashSet};
use yew::prelude::*;

use crate::pages::dashboard::permission_dialog::PermissionDialog;
use crate::pages::dashboard::types::{parse_ask_user_question, PendingPermission, QuestionAnswers};

/// Typed answer the handler emits to the parent. The parent translates each
/// variant into a `ClientToServer::PermissionResponse` frame; keeping the
/// shape typed (rather than emitting raw `PermissionResponseFields`) means
/// the handler doesn't need to know about the wire enum.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionResponseKind {
    /// Standard "Allow" — `remember == false` sends no persisted suggestions.
    Approve { remember: bool },
    /// Standard "Deny".
    Deny,
    /// AskUserQuestion submission — typed per-question answers keyed by
    /// question index.
    AnswerQuestions(QuestionAnswers),
}

/// Inputs to the handler.
#[derive(Properties, PartialEq)]
pub struct PermissionHandlerProps {
    /// Whether the parent session is currently focused. When `true`, the
    /// handler grabs focus on the dialog so keyboard shortcuts work without
    /// a stray click.
    pub focused: bool,
    /// Fired exactly once on `create`, handing the parent a callback it can
    /// invoke to push a new permission request into the handler. This is the
    /// child→parent half of the wiring pattern that lets the parent stay a
    /// thin WebSocket router without storing any permission state itself.
    pub on_register: Callback<Callback<PendingPermission>>,
    /// Fired when the handler's pending state transitions. The parent uses
    /// this to feed the `is_awaiting` computation.
    pub on_pending_changed: Callback<bool>,
    /// Fired when the user answers a permission. Carries both the
    /// originating `request_id` (so the parent doesn't need to remember it)
    /// and the typed answer.
    pub on_response: Callback<(String, PermissionResponseKind)>,
    /// Yielded after the handler clears its pending state — the parent uses
    /// this to refocus the textarea so the user can keep typing without a
    /// click. Decoupled from `on_response` because the focus side effect is
    /// independent of which WS frame is sent.
    pub on_refocus_input: Callback<()>,
}

/// Internal messages.
pub enum PermissionHandlerMsg {
    /// A new permission request arrived from the wire.
    Receive(PendingPermission),
    /// Keyboard navigation upward through the option list.
    SelectUp,
    /// Keyboard navigation downward through the option list.
    SelectDown,
    /// Activate the currently selected option (Enter / Space).
    Confirm,
    /// Click on a specific option index.
    SelectAndConfirm(usize),
    /// User picked "Approve" (with optional `remember`).
    Approve { remember: bool },
    /// User picked "Deny".
    Deny,
    /// Per-question single-answer pick for AskUserQuestion.
    SetQuestionAnswer(usize, String),
    /// Toggle a multi-select option for AskUserQuestion.
    ToggleQuestionOption(usize, usize),
    /// Submit the AskUserQuestion form.
    SubmitAllAnswers(QuestionAnswers),
}

pub struct PermissionHandler {
    request: Option<PendingPermission>,
    selected: usize,
    multi_select_options: HashMap<usize, HashSet<usize>>,
    question_answers: QuestionAnswers,
    dialog_ref: NodeRef,
    /// Focus the dialog container exactly once when it appears, never on the
    /// re-renders that typing triggers. Without this gate, `rendered()` called
    /// `el.focus()` after *every* render, so each keystroke in the "Other"
    /// field yanked focus back to the container — you could only enter one
    /// character at a time. Set when a request arrives or the session regains
    /// focus; cleared once consumed in `rendered()`.
    needs_focus: bool,
}

impl Component for PermissionHandler {
    type Message = PermissionHandlerMsg;
    type Properties = PermissionHandlerProps;

    fn create(ctx: &Context<Self>) -> Self {
        // Hand the parent a callback it can invoke to push new requests at
        // us. The parent stores this and calls it from its `WsEvent::Permission`
        // arm — so the parent never has to model permission state itself.
        let dispatcher = ctx.link().callback(PermissionHandlerMsg::Receive);
        ctx.props().on_register.emit(dispatcher);

        Self {
            request: None,
            selected: 0,
            multi_select_options: HashMap::new(),
            question_answers: QuestionAnswers::new(),
            dialog_ref: NodeRef::default(),
            needs_focus: false,
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old_props: &Self::Properties) -> bool {
        // Re-grab keyboard-nav focus only when the session transitions *into*
        // focus with a dialog already pending — not on unrelated prop changes
        // (which would re-introduce the focus-steal during typing).
        if ctx.props().focused && !old_props.focused && self.request.is_some() {
            self.needs_focus = true;
        }
        true
    }

    fn rendered(&mut self, ctx: &Context<Self>, _first_render: bool) {
        // Only when a dialog just appeared (or the session regained focus),
        // and only while this session is focused — clear the flag once we've
        // actually grabbed focus, so typing-triggered re-renders never refocus.
        if self.needs_focus && ctx.props().focused {
            if let Some(el) = self.dialog_ref.cast::<web_sys::HtmlElement>() {
                let _ = el.focus();
            }
            self.needs_focus = false;
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            PermissionHandlerMsg::Receive(perm) => {
                let was_empty = self.request.is_none();
                self.request = Some(perm);
                self.selected = 0;
                self.question_answers.clear();
                self.multi_select_options.clear();
                // A dialog just appeared — grab keyboard-nav focus once on the
                // next render (consumed in `rendered`), not on every keystroke.
                self.needs_focus = true;
                if was_empty {
                    ctx.props().on_pending_changed.emit(true);
                }
                true
            }
            PermissionHandlerMsg::SelectUp => self.navigate(-1),
            PermissionHandlerMsg::SelectDown => self.navigate(1),
            PermissionHandlerMsg::Confirm => {
                self.confirm(ctx);
                false
            }
            PermissionHandlerMsg::SelectAndConfirm(index) => {
                self.selected = index;
                self.confirm(ctx);
                false
            }
            PermissionHandlerMsg::Approve { remember } => {
                self.dispatch_response(ctx, PermissionResponseKind::Approve { remember });
                true
            }
            PermissionHandlerMsg::Deny => {
                self.dispatch_response(ctx, PermissionResponseKind::Deny);
                true
            }
            PermissionHandlerMsg::SetQuestionAnswer(question_idx, answer) => {
                // A blank answer (e.g. a cleared "something else" field)
                // un-answers the question rather than counting an empty string
                // as a valid answer.
                if answer.trim().is_empty() {
                    self.question_answers.remove(&question_idx);
                } else {
                    self.question_answers.insert(question_idx, answer);
                }
                self.multi_select_options.remove(&question_idx);
                true
            }
            PermissionHandlerMsg::ToggleQuestionOption(question_idx, option_idx) => {
                let options = self.multi_select_options.entry(question_idx).or_default();
                if !options.insert(option_idx) {
                    options.remove(&option_idx);
                }
                true
            }
            PermissionHandlerMsg::SubmitAllAnswers(answers) => {
                self.dispatch_response(ctx, PermissionResponseKind::AnswerQuestions(answers));
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let Some(ref perm) = self.request else {
            return html! {};
        };

        let link = ctx.link();
        let on_select_up = link.callback(|_| PermissionHandlerMsg::SelectUp);
        let on_select_down = link.callback(|_| PermissionHandlerMsg::SelectDown);
        let on_confirm = link.callback(|_| PermissionHandlerMsg::Confirm);
        let on_select_and_confirm = link.callback(PermissionHandlerMsg::SelectAndConfirm);
        let on_submit_answers = link.callback(PermissionHandlerMsg::SubmitAllAnswers);
        let on_set_answer =
            link.callback(|(q_idx, answer)| PermissionHandlerMsg::SetQuestionAnswer(q_idx, answer));
        let on_toggle_option = link.callback(|(q_idx, opt_idx)| {
            PermissionHandlerMsg::ToggleQuestionOption(q_idx, opt_idx)
        });

        html! {
            <PermissionDialog
                permission={perm.clone()}
                selected={self.selected}
                multi_select_options={self.multi_select_options.clone()}
                question_answers={self.question_answers.clone()}
                dialog_ref={self.dialog_ref.clone()}
                {on_select_up}
                {on_select_down}
                {on_confirm}
                {on_select_and_confirm}
                {on_submit_answers}
                {on_set_answer}
                {on_toggle_option}
            />
        }
    }
}

impl PermissionHandler {
    fn navigate(&mut self, delta: i32) -> bool {
        let Some(ref perm) = self.request else {
            return false;
        };
        let max = max_option_index(perm);
        self.selected = next_selection(self.selected, max, delta);
        true
    }

    fn confirm(&mut self, ctx: &Context<Self>) {
        let Some(ref perm) = self.request else {
            return;
        };
        if perm.tool_name == "AskUserQuestion" {
            if !self.question_answers.is_empty() {
                let answers = self.question_answers.clone();
                ctx.link()
                    .send_message(PermissionHandlerMsg::SubmitAllAnswers(answers));
            }
            return;
        }

        let has_suggestions = !perm.permission_suggestions.is_empty();
        let msg = match resolve_standard_choice(self.selected, has_suggestions) {
            StandardChoice::Approve => PermissionHandlerMsg::Approve { remember: false },
            StandardChoice::ApproveAndRemember => PermissionHandlerMsg::Approve { remember: true },
            StandardChoice::Deny => PermissionHandlerMsg::Deny,
        };
        ctx.link().send_message(msg);
    }

    fn dispatch_response(&mut self, ctx: &Context<Self>, kind: PermissionResponseKind) {
        let Some(perm) = self.request.take() else {
            return;
        };
        self.multi_select_options.clear();
        self.question_answers.clear();
        ctx.props().on_response.emit((perm.request_id, kind));
        ctx.props().on_pending_changed.emit(false);
        ctx.props().on_refocus_input.emit(());
    }
}

/// Pure helper: which standard option does `index` map to, given whether the
/// permission carries persistable suggestions?
///
/// The mapping mirrors the standard-permission dialog's option ordering:
/// `[Allow, Allow & Remember, Deny]` when suggestions are present and
/// `[Allow, Deny]` when they're not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandardChoice {
    Approve,
    ApproveAndRemember,
    Deny,
}

fn resolve_standard_choice(selected: usize, has_suggestions: bool) -> StandardChoice {
    match (selected, has_suggestions) {
        (0, _) => StandardChoice::Approve,
        (1, true) => StandardChoice::ApproveAndRemember,
        (1, false) => StandardChoice::Deny,
        (2, true) => StandardChoice::Deny,
        _ => StandardChoice::Approve,
    }
}

/// Pure helper: maximum (inclusive) selectable option index for a permission.
///
/// AskUserQuestion shows one row per option in the first question;
/// permissions with persistable suggestions add a third "Allow & Remember"
/// option so the cap is 2; standard Allow/Deny caps at 1.
fn max_option_index(perm: &PendingPermission) -> usize {
    if perm.tool_name == "AskUserQuestion" {
        return parse_ask_user_question(&perm.input)
            .and_then(|p| {
                p.questions
                    .first()
                    .map(|q| q.options.len().saturating_sub(1))
            })
            .unwrap_or(0);
    }
    if !perm.permission_suggestions.is_empty() {
        2
    } else {
        1
    }
}

/// Pure helper: compute the next selected index when the user presses
/// up (delta < 0) or down (delta >= 0), wrapping at both ends.
fn next_selection(current: usize, max: usize, delta: i32) -> usize {
    if delta < 0 {
        if current == 0 {
            max
        } else {
            current - 1
        }
    } else if current < max {
        current + 1
    } else {
        0
    }
}

/// Translate a typed `PermissionResponseKind` plus the originating
/// `PendingPermission` into the wire frame the parent sends over the
/// WebSocket. Kept here (instead of in the parent) so the wire-translation
/// stays adjacent to the typed enum it serializes.
pub fn build_permission_response(
    request_id: String,
    kind: PermissionResponseKind,
    perm: &PendingPermission,
) -> shared::PermissionResponseFields {
    match kind {
        PermissionResponseKind::Approve { remember } => shared::PermissionResponseFields {
            request_id,
            allow: true,
            input: Some(perm.input.clone()),
            permissions: if remember {
                perm.permission_suggestions.clone()
            } else {
                vec![]
            },
            reason: None,
        },
        PermissionResponseKind::Deny => shared::PermissionResponseFields {
            request_id,
            allow: false,
            input: None,
            permissions: vec![],
            reason: Some("User denied".to_string()),
        },
        PermissionResponseKind::AnswerQuestions(answers) => {
            // The Claude CLI echoes this `updatedInput` back into the
            // `tool_use_result` it emits; its frontend reads
            // `tool_use_result.questions` and calls `questions.map(...)`.
            // Returning a bare `{answers: …}` drops `questions` and crashes
            // that frontend with `undefined is not an object (evaluating
            // 'q.map')` — so echo the original input (which carries
            // `questions` and any `metadata`) verbatim and merge the
            // answers in.
            //
            // Answer key is the question **text** (`q.question`), not the
            // header. The CLI's `mapToolResultToToolResultBlockParam`
            // destructures `({question: z})` from each question object and
            // looks up `answers[z]`, so the canonical key is whatever sits
            // in the `question` field. See #873 — using `header` causes the
            // CLI to read every answer as `undefined`, and the sub-agent
            // never sees the user's choices.
            let mut updated_input = perm.input.clone();
            if let Some(obj) = updated_input.as_object_mut() {
                let mut answer_map = serde_json::Map::new();
                if let Some(parsed) = parse_ask_user_question(&perm.input) {
                    for (idx, answer) in answers.iter() {
                        if let Some(q) = parsed.questions.get(*idx) {
                            answer_map.insert(
                                q.question.clone(),
                                serde_json::Value::String(answer.clone()),
                            );
                        }
                    }
                }
                obj.insert("answers".to_string(), serde_json::Value::Object(answer_map));
            }
            shared::PermissionResponseFields {
                request_id,
                allow: true,
                input: Some(updated_input),
                permissions: vec![],
                reason: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_perm(tool: &str, suggestions: usize) -> PendingPermission {
        // `PermissionSuggestion`'s constructor fields aren't re-exported via
        // `shared`, so build via JSON to dodge the orphan import. The wire
        // shape mirrors `claude-codes::ToolPermissionRequest.permissions`.
        let suggestions: Vec<shared::PermissionSuggestion> = (0..suggestions)
            .map(|_| {
                serde_json::from_value(serde_json::json!({
                    "type": "addRules",
                    "destination": "session",
                }))
                .unwrap()
            })
            .collect();
        PendingPermission {
            request_id: "rid".to_string(),
            tool_name: tool.to_string(),
            input: serde_json::json!({}),
            permission_suggestions: suggestions,
        }
    }

    fn mk_ask_user_question(options_per_question: &[usize]) -> PendingPermission {
        let questions: Vec<serde_json::Value> = options_per_question
            .iter()
            .map(|n| {
                serde_json::json!({
                    "question": "Q?",
                    "options": (0..*n).map(|i| serde_json::json!({ "label": format!("opt-{i}") })).collect::<Vec<_>>(),
                })
            })
            .collect();
        PendingPermission {
            request_id: "rid".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            input: serde_json::json!({ "questions": questions }),
            permission_suggestions: vec![],
        }
    }

    // --- max_option_index ---

    #[test]
    fn max_index_standard_permission_caps_at_one() {
        assert_eq!(max_option_index(&mk_perm("Bash", 0)), 1);
    }

    #[test]
    fn max_index_with_suggestions_caps_at_two() {
        // Allow / Allow & Remember / Deny → top index is 2.
        assert_eq!(max_option_index(&mk_perm("Bash", 1)), 2);
    }

    #[test]
    fn max_index_ask_user_question_uses_first_question_options() {
        let perm = mk_ask_user_question(&[3, 5]);
        // Three options → indices 0..=2 → cap is 2.
        assert_eq!(max_option_index(&perm), 2);
    }

    #[test]
    fn max_index_ask_user_question_with_no_questions_caps_at_zero() {
        let perm = PendingPermission {
            request_id: "rid".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            input: serde_json::json!({ "questions": [] }),
            permission_suggestions: vec![],
        };
        assert_eq!(max_option_index(&perm), 0);
    }

    #[test]
    fn max_index_ask_user_question_unparseable_input_caps_at_zero() {
        let perm = PendingPermission {
            request_id: "rid".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            input: serde_json::json!("not-an-object"),
            permission_suggestions: vec![],
        };
        assert_eq!(max_option_index(&perm), 0);
    }

    // --- next_selection ---

    #[test]
    fn next_selection_down_advances_and_wraps() {
        // 0 → 1 → 2 → 0 (with max == 2)
        assert_eq!(next_selection(0, 2, 1), 1);
        assert_eq!(next_selection(1, 2, 1), 2);
        assert_eq!(next_selection(2, 2, 1), 0);
    }

    #[test]
    fn next_selection_up_retreats_and_wraps() {
        // 2 → 1 → 0 → 2 (with max == 2)
        assert_eq!(next_selection(2, 2, -1), 1);
        assert_eq!(next_selection(1, 2, -1), 0);
        assert_eq!(next_selection(0, 2, -1), 2);
    }

    #[test]
    fn next_selection_single_option_stays_put() {
        // With max == 0 the only valid index is 0; both directions must
        // collapse to 0 so the keyboard handler can't drift out of range.
        assert_eq!(next_selection(0, 0, -1), 0);
        assert_eq!(next_selection(0, 0, 1), 0);
    }

    // --- resolve_standard_choice ---

    #[test]
    fn standard_choice_two_option_layout() {
        // [Allow, Deny] — selecting 0 approves, 1 denies, anything past the
        // list falls back to approve (defensive against an out-of-range index).
        assert_eq!(resolve_standard_choice(0, false), StandardChoice::Approve);
        assert_eq!(resolve_standard_choice(1, false), StandardChoice::Deny);
        assert_eq!(resolve_standard_choice(2, false), StandardChoice::Approve);
    }

    #[test]
    fn standard_choice_three_option_layout() {
        // [Allow, Allow & Remember, Deny] when suggestions exist.
        assert_eq!(resolve_standard_choice(0, true), StandardChoice::Approve);
        assert_eq!(
            resolve_standard_choice(1, true),
            StandardChoice::ApproveAndRemember
        );
        assert_eq!(resolve_standard_choice(2, true), StandardChoice::Deny);
        // Out-of-range fall-through.
        assert_eq!(resolve_standard_choice(99, true), StandardChoice::Approve);
    }

    // --- build_permission_response ---

    #[test]
    fn build_response_approve_without_remember_drops_suggestions() {
        let perm = mk_perm("Bash", 2);
        let frame = build_permission_response(
            "rid-1".to_string(),
            PermissionResponseKind::Approve { remember: false },
            &perm,
        );
        assert_eq!(frame.request_id, "rid-1");
        assert!(frame.allow);
        assert_eq!(frame.permissions.len(), 0);
        assert!(frame.reason.is_none());
        assert!(frame.input.is_some());
    }

    #[test]
    fn build_response_approve_with_remember_keeps_suggestions() {
        let perm = mk_perm("Bash", 2);
        let frame = build_permission_response(
            "rid-1".to_string(),
            PermissionResponseKind::Approve { remember: true },
            &perm,
        );
        assert!(frame.allow);
        assert_eq!(frame.permissions.len(), 2);
    }

    #[test]
    fn build_response_deny_clears_input_and_sets_reason() {
        let perm = mk_perm("Bash", 0);
        let frame =
            build_permission_response("rid-1".to_string(), PermissionResponseKind::Deny, &perm);
        assert!(!frame.allow);
        assert!(frame.input.is_none());
        assert_eq!(frame.reason.as_deref(), Some("User denied"));
        assert_eq!(frame.permissions.len(), 0);
    }

    #[test]
    fn build_response_answer_questions_preserves_questions_and_keys_by_question_text() {
        // Regression guard for two coupled crashes in the Claude CLI:
        //
        // 1. The CLI echoes `updatedInput` into the `tool_use_result` it
        //    emits and its frontend does `questions.map(...)` — dropping
        //    `questions` crashes it with
        //    `undefined is not an object (evaluating 'q.map')`.
        //
        // 2. The CLI's `mapToolResultToToolResultBlockParam` destructures
        //    `({question: z})` and reads `answers[z]`, so the answer-map
        //    key MUST be the question `question` text. Keying by `header`
        //    (the regression #831 introduced, #873 reports) makes every
        //    lookup return `undefined` and the sub-agent never sees the
        //    user's choices.
        let perm = PendingPermission {
            request_id: "rid".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            input: serde_json::json!({
                "questions": [
                    { "question": "first?", "header": "One", "options": [{ "label": "a" }] },
                    { "question": "second?", "header": "Two", "options": [{ "label": "b" }] },
                ]
            }),
            permission_suggestions: vec![],
        };
        let mut answers = QuestionAnswers::new();
        answers.insert(0, "a".to_string());
        answers.insert(1, "b".to_string());

        let frame = build_permission_response(
            "rid-1".to_string(),
            PermissionResponseKind::AnswerQuestions(answers),
            &perm,
        );
        assert!(frame.allow);
        let input = frame.input.expect("answers payload missing");

        // `questions` MUST survive in the echoed input — q.map crash guard.
        let questions = input.get("questions").and_then(|v| v.as_array());
        assert_eq!(questions.map(|q| q.len()), Some(2));

        // `answers` is keyed by question text — the value of the
        // `question` field, NOT `header` or array index.
        let answers_obj = input
            .get("answers")
            .and_then(|v| v.as_object())
            .expect("answers object missing");
        assert_eq!(
            answers_obj.get("first?").and_then(|v| v.as_str()),
            Some("a")
        );
        assert_eq!(
            answers_obj.get("second?").and_then(|v| v.as_str()),
            Some("b")
        );
        // And specifically NOT by header — guard against the #831 regression.
        assert!(answers_obj.get("One").is_none());
        assert!(answers_obj.get("Two").is_none());
    }

    #[test]
    fn build_response_answer_questions_keys_by_question_text_even_with_no_header() {
        // The presence or absence of `header` must not change the key
        // choice — the CLI never reads `header` for answer lookup.
        let perm = PendingPermission {
            request_id: "rid".to_string(),
            tool_name: "AskUserQuestion".to_string(),
            input: serde_json::json!({
                "questions": [
                    { "question": "headerless?", "options": [{ "label": "x" }] },
                ]
            }),
            permission_suggestions: vec![],
        };
        let mut answers = QuestionAnswers::new();
        answers.insert(0, "x".to_string());

        let frame = build_permission_response(
            "rid-1".to_string(),
            PermissionResponseKind::AnswerQuestions(answers),
            &perm,
        );
        let input = frame.input.expect("answers payload missing");
        let answers_obj = input
            .get("answers")
            .and_then(|v| v.as_object())
            .expect("answers object missing");
        assert_eq!(
            answers_obj.get("headerless?").and_then(|v| v.as_str()),
            Some("x")
        );
    }
}

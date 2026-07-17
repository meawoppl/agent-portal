//! Shared model picker fed by the SDK model catalogs, used by both the launch
//! and schedule dialogs.
//!
//! The catalog options, the `agent_type` -> CLI-argument mapping, and the
//! `extract_model_arg` routine that recognizes those args when editing an
//! existing task all live together so they can't drift apart: `extract_model_arg`
//! must keep recognizing exactly what `model_cli_args` emits, and it only treats
//! a value as a model when the picker actually offers it as an option.

use claude_codes::ClaudeModel;
use codex_codes::CodexModel;
use shared::AgentType;
use web_sys::HtmlSelectElement;
use yew::prelude::*;

/// Build the CLI args that select `model_value` for the given agent. An empty
/// value means "agent default" and emits no args. Mirrors the mapping used when
/// launching a session so the two paths stay identical.
pub fn model_cli_args(agent_type: AgentType, model_value: &str) -> Vec<String> {
    if model_value.is_empty() {
        return Vec::new();
    }
    match agent_type {
        AgentType::Claude => vec!["--model".to_string(), model_value.to_string()],
        AgentType::Codex => vec!["-c".to_string(), format!("model={model_value}")],
    }
}

/// `(cli_arg, display_name, is_alias)` tuples that populate the picker for
/// `agent_type`. The Codex auto-review pseudo-model is hidden, mirroring Codex's
/// own picker.
fn model_catalog(agent_type: AgentType) -> Vec<(&'static str, &'static str, bool)> {
    match agent_type {
        AgentType::Claude => ClaudeModel::known()
            .iter()
            .map(|m| (m.cli_arg(), m.display_name(), m.is_alias()))
            .collect(),
        AgentType::Codex => CodexModel::known()
            .iter()
            .filter(|m| !matches!(m, CodexModel::CodexAutoReview))
            .map(|m| (m.cli_arg(), m.display_name(), false))
            .collect(),
    }
}

/// Whether `value` is a model the picker for `agent_type` offers as an option.
pub fn is_known_model(agent_type: AgentType, value: &str) -> bool {
    !value.is_empty()
        && model_catalog(agent_type)
            .iter()
            .any(|(cli, _, _)| *cli == value)
}

/// Pull a picker-selectable model argument out of `args`, returning the model
/// value (its CLI arg) and the remaining args with that model argument removed.
///
/// Recognizes only the forms the picker emits, and only when the value is a
/// model the picker offers:
///   * Claude: `--model <value>`
///   * Codex:  `-c model=<value>`
///
/// A malformed form (`--model` with no following value), an unknown model value
/// (`--model some-future-model`), or the absence of any model argument all yield
/// `(None, args unchanged)` so the arg is left in the extra-args field untouched
/// instead of being silently dropped.
pub fn extract_model_arg(args: &[String], agent_type: AgentType) -> (Option<String>, Vec<String>) {
    let mut i = 0;
    while i < args.len() {
        let matched: Option<String> = match agent_type {
            AgentType::Claude => {
                if args[i] == "--model"
                    && i + 1 < args.len()
                    && is_known_model(agent_type, &args[i + 1])
                {
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            }
            AgentType::Codex => {
                if args[i] == "-c" && i + 1 < args.len() {
                    args[i + 1]
                        .strip_prefix("model=")
                        .filter(|v| is_known_model(agent_type, v))
                        .map(|v| v.to_string())
                } else {
                    None
                }
            }
        };

        if let Some(value) = matched {
            let mut rest = Vec::with_capacity(args.len() - 2);
            rest.extend_from_slice(&args[..i]);
            rest.extend_from_slice(&args[i + 2..]);
            return (Some(value), rest);
        }
        i += 1;
    }
    (None, args.to_vec())
}

#[derive(Properties, PartialEq)]
pub struct ModelSelectProps {
    pub agent_type: AgentType,
    /// Currently selected model CLI arg, or `""` for the agent's own default.
    pub value: String,
    /// Fires with the newly selected model CLI arg (`""` = agent default).
    pub on_change: Callback<String>,
    /// CSS class for the underlying `<select>`. Defaults to the launcher-select
    /// styling used by the launch dialog; the schedule dialog passes `""` so it
    /// inherits `.sched-field select`.
    #[prop_or_else(|| "launcher-select".to_string())]
    pub class: String,
}

/// A `<select>` whose options are the SDK model catalog for `agent_type`.
/// `""` (the leading "Agent default" option) means the agent's own default.
///
/// A `<select>` is inherently focus-stable (no controlled-text-input caret
/// churn), so no keyed sub-component is needed here.
#[function_component(ModelSelect)]
pub fn model_select(props: &ModelSelectProps) -> Html {
    let onchange = {
        let on_change = props.on_change.clone();
        Callback::from(move |e: Event| {
            if let Some(select) = e.target_dyn_into::<HtmlSelectElement>() {
                on_change.emit(select.value());
            }
        })
    };

    let selected = props.value.clone();
    let option = move |cli: &str, label: &str| -> Html {
        html! {
            <option value={cli.to_string()} selected={selected == cli}>{ label }</option>
        }
    };

    let catalog = model_catalog(props.agent_type);
    let options: Html = match props.agent_type {
        AgentType::Claude => html! {
            <>
                <optgroup label="Aliases — track the newest model">
                    { for catalog.iter().filter(|(_, _, alias)| *alias)
                        .map(|(cli, label, _)| option(cli, label)) }
                </optgroup>
                <optgroup label="Pinned models">
                    { for catalog.iter().filter(|(_, _, alias)| !*alias)
                        .map(|(cli, label, _)| option(cli, label)) }
                </optgroup>
            </>
        },
        AgentType::Codex => html! {
            { for catalog.iter().map(|(cli, label, _)| option(cli, label)) }
        },
    };

    html! {
        <select class={props.class.clone()} {onchange}>
            <option value="" selected={props.value.is_empty()}>{ "Agent default" }</option>
            { options }
        </select>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// A model that appears in the Claude catalog (used so the tests don't hard
    /// code a specific value that may churn as the SDK updates).
    fn a_known_claude_model() -> String {
        ClaudeModel::known()[0].cli_arg().to_string()
    }

    /// A model that appears in the Codex catalog and is offered by the picker.
    fn a_known_codex_model() -> String {
        CodexModel::known()
            .iter()
            .find(|m| !matches!(m, CodexModel::CodexAutoReview))
            .unwrap()
            .cli_arg()
            .to_string()
    }

    #[test]
    fn extract_claude_model_and_strips_it() {
        let model = a_known_claude_model();
        let (found, rest) =
            extract_model_arg(&args(&["--model", &model, "--verbose"]), AgentType::Claude);
        assert_eq!(found.as_deref(), Some(model.as_str()));
        assert_eq!(rest, args(&["--verbose"]));
    }

    #[test]
    fn extract_codex_model_and_strips_it() {
        let model = a_known_codex_model();
        let (found, rest) = extract_model_arg(
            &args(&["-c", &format!("model={model}"), "-c", "foo=bar"]),
            AgentType::Codex,
        );
        assert_eq!(found.as_deref(), Some(model.as_str()));
        assert_eq!(rest, args(&["-c", "foo=bar"]));
    }

    #[test]
    fn extract_absent_leaves_args_untouched() {
        let input = args(&["--verbose", "-c", "foo=bar"]);
        let (found, rest) = extract_model_arg(&input, AgentType::Claude);
        assert_eq!(found, None);
        assert_eq!(rest, input);
    }

    #[test]
    fn extract_unrecognized_claude_value_is_left_in_args() {
        // A value the picker doesn't offer must NOT be stripped — it stays in
        // the extra-args field untouched instead of being silently dropped.
        let input = args(&["--model", "some-future-model", "--verbose"]);
        let (found, rest) = extract_model_arg(&input, AgentType::Claude);
        assert_eq!(found, None);
        assert_eq!(rest, input);
    }

    #[test]
    fn extract_unrecognized_codex_value_is_left_in_args() {
        let input = args(&["-c", "model=some-future-model"]);
        let (found, rest) = extract_model_arg(&input, AgentType::Codex);
        assert_eq!(found, None);
        assert_eq!(rest, input);
    }

    #[test]
    fn extract_dangling_model_flag_is_left_in_args() {
        // `--model` in trailing position with no value (unrecognized position).
        let input = args(&["--verbose", "--model"]);
        let (found, rest) = extract_model_arg(&input, AgentType::Claude);
        assert_eq!(found, None);
        assert_eq!(rest, input);
    }

    #[test]
    fn round_trips_what_model_cli_args_emits() {
        let claude = a_known_claude_model();
        let (found, rest) = extract_model_arg(
            &model_cli_args(AgentType::Claude, &claude),
            AgentType::Claude,
        );
        assert_eq!(found.as_deref(), Some(claude.as_str()));
        assert!(rest.is_empty());

        let codex = a_known_codex_model();
        let (found, rest) =
            extract_model_arg(&model_cli_args(AgentType::Codex, &codex), AgentType::Codex);
        assert_eq!(found.as_deref(), Some(codex.as_str()));
        assert!(rest.is_empty());
    }

    #[test]
    fn model_cli_args_empty_is_agent_default() {
        assert!(model_cli_args(AgentType::Claude, "").is_empty());
        assert!(model_cli_args(AgentType::Codex, "").is_empty());
    }
}

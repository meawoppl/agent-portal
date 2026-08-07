//! A self-contained card for a shell-command execution: intent, the command
//! line, and its (collapsible) output — styled like the Claude/Codex tool
//! results (green success / red failure). Extracted so any agent whose wire
//! shape carries a command + its output in one place can render it the same
//! way; muse's `bash`/command results are the first consumer.

use super::super::expandable::ExpandableText;
use crate::components::markdown::linkify_urls;
use yew::prelude::*;

/// How much output to show before the `ExpandableText` toggle collapses it —
/// matches the "show a bit, click for the rest" behavior of the other agents'
/// tool output.
const OUTPUT_PREVIEW_CHARS: usize = 600;

#[derive(Properties, PartialEq)]
pub struct CommandResultCardProps {
    pub command: AttrValue,
    /// The agent's one-line rationale ("intent"), shown above the command.
    #[prop_or_default]
    pub description: Option<AttrValue>,
    /// Combined stdout/stderr. Rendered as ANSI-styled, collapsible output.
    #[prop_or_default]
    pub output: Option<AttrValue>,
    /// `None` treated as success (0). Non-zero flips the card to the failure
    /// (red) treatment and shows an `exit N` badge.
    #[prop_or_default]
    pub exit_code: Option<i64>,
}

#[function_component(CommandResultCard)]
pub fn command_result_card(props: &CommandResultCardProps) -> Html {
    let exit = props.exit_code.unwrap_or(0);
    let failed = exit != 0;
    let card_class = classes!("command-result", failed.then_some("failed"));

    html! {
        <div class={card_class}>
            if let Some(desc) = props.description.as_ref().filter(|d| !d.trim().is_empty()) {
                <div class="command-result-intent">{ desc }</div>
            }
            <div class="command-result-command">
                <span class="command-result-prompt">{ "$" }</span>
                <code class="command-result-line">{ linkify_urls(&props.command) }</code>
                if failed {
                    <span class="command-result-exit">{ format!("exit {exit}") }</span>
                }
            </div>
            if let Some(out) = props.output.as_ref().filter(|o| !o.is_empty()) {
                <ExpandableText
                    full_text={out.clone()}
                    max_len={OUTPUT_PREVIEW_CHARS}
                    class={classes!("command-result-output")}
                    ansi={true}
                />
            }
        </div>
    }
}

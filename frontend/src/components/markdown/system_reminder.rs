//! `<system-reminder>` blocks, rendered as a collapsed bar you can click into.
//!
//! These blocks are out-of-band context for the agent — the portal-features
//! reminder, the inter-agent "reply to that agent, not the user" bumper, the
//! CLI's own injected notices. They are genuinely useful to *see* (they explain
//! why an agent did something), but they are noise inline: several are longer
//! than the message carrying them.
//!
//! The splitting itself lives in [`shared::system_reminder`] (#1654), because
//! the backend has to strip the same blocks before classifying message text and
//! a splitter that disagreed with itself across the wire would show the user one
//! thing and feed the classifier another. This module is only the rendering half.
//!
//! Splitting happens in [`MarkdownView`](super::MarkdownView), which every
//! session type's text already flows through, so claude, codex and muse all get
//! this from one pathway rather than three per-renderer special cases.
//!
//! The bar deliberately reuses the `.portal-reminder*` classes rather than
//! minting a parallel set: that treatment (subtle teal, collapsed by default,
//! header toggles) is already the established "notice you can click into" idiom
//! in the transcript, and its `#7dcfff` matches the portal tick color.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct SystemReminderBarProps {
    /// Reminder contents, tags already stripped.
    pub body: AttrValue,
}

#[function_component(SystemReminderBar)]
pub(super) fn system_reminder_bar(props: &SystemReminderBarProps) -> Html {
    let expanded = use_state(|| false);
    let on_toggle = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(!*expanded))
    };
    let header_class = if *expanded {
        "portal-reminder-header expanded"
    } else {
        "portal-reminder-header"
    };

    html! {
        <div class="portal-reminder system-reminder-bar">
            <button type="button" class={header_class} onclick={on_toggle}>
                <span class="portal-reminder-icon">{ "\u{24D8}" }</span>
                <span class="portal-reminder-title">{ "System reminder" }</span>
                <span class="portal-reminder-toggle">
                    { if *expanded { "\u{25BE}" } else { "\u{25B8}" } }
                </span>
            </button>
            if *expanded {
                // The body is rendered as plain text, not markdown: reminders
                // are machine-authored and often contain literal angle
                // brackets, backticks and paths that markdown would mangle.
                <div class="portal-reminder-body system-reminder-body">{ &props.body }</div>
            }
        </div>
    }
}

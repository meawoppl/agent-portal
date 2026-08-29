//! Machine-authored notice blocks, rendered as collapsed bars you can click.
//!
//! These blocks are out-of-band context for the agent — the portal-features
//! reminder, the inter-agent "reply to that agent, not the user" bumper, the
//! CLI's own injected notices. They are genuinely useful to *see* (they explain
//! why an agent did something), but they are noise inline: several are longer
//! than the message carrying them.
//!
//! System reminders and Claude's `<task-notification>` injections share this
//! treatment. The splitting itself lives in [`shared::system_reminder`]
//! (#1654), because
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
    html! {
        <CollapsibleNoticeBar title="System reminder" body={props.body.clone()} />
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct TaskNotificationBarProps {
    /// Notification contents, outer tags already stripped.
    pub body: AttrValue,
}

#[function_component(TaskNotificationBar)]
pub(super) fn task_notification_bar(props: &TaskNotificationBarProps) -> Html {
    let status = xml_field(&props.body, "status").unwrap_or("updated");
    let title = xml_field(&props.body, "summary")
        .map(str::to_string)
        .unwrap_or_else(|| format!("Background task {status}"));
    html! {
        <CollapsibleNoticeBar {title} body={props.body.clone()} />
    }
}

fn xml_field<'a>(body: &'a str, field: &str) -> Option<&'a str> {
    let open = format!("<{field}>");
    let close = format!("</{field}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim())
}

#[derive(Properties, PartialEq)]
struct CollapsibleNoticeBarProps {
    title: AttrValue,
    body: AttrValue,
}

#[function_component(CollapsibleNoticeBar)]
fn collapsible_notice_bar(props: &CollapsibleNoticeBarProps) -> Html {
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
                <span class="portal-reminder-title">{ &props.title }</span>
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

#[cfg(test)]
mod tests {
    use super::xml_field;

    #[test]
    fn extracts_task_notification_summary_and_status() {
        let body = "<task-id>bmq5w2fik</task-id> <status>completed</status> <summary>Background command &quot;Watch CI checks&quot; completed (exit code 0)</summary>";
        assert_eq!(xml_field(body, "status"), Some("completed"));
        assert_eq!(
            xml_field(body, "summary"),
            Some("Background command &quot;Watch CI checks&quot; completed (exit code 0)")
        );
        assert_eq!(xml_field(body, "output-file"), None);
    }
}

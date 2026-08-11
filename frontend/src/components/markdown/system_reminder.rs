//! `<system-reminder>` blocks, rendered as a collapsed bar you can click into.
//!
//! These blocks are out-of-band context for the agent — the portal-features
//! reminder, the inter-agent "reply to that agent, not the user" bumper, the
//! CLI's own injected notices. They are genuinely useful to *see* (they explain
//! why an agent did something), but they are noise inline: several are longer
//! than the message carrying them.
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

const OPEN_TAG: &str = "<system-reminder>";
const CLOSE_TAG: &str = "</system-reminder>";

/// One piece of a message: ordinary prose, or a reminder to collapse.
#[derive(Debug, PartialEq)]
pub(super) enum Segment {
    Text(String),
    Reminder(String),
}

/// Split `text` on `<system-reminder>` blocks.
///
/// An unterminated open tag is treated as prose, not as a reminder running to
/// the end of the message: a truncated or malformed block should read as the
/// literal text it is rather than swallowing everything after it into a bar.
pub(super) fn split_system_reminders(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find(OPEN_TAG) {
        let after_open = open + OPEN_TAG.len();
        let Some(close_rel) = rest[after_open..].find(CLOSE_TAG) else {
            break;
        };
        let close = after_open + close_rel;

        let before = &rest[..open];
        if !before.trim().is_empty() {
            segments.push(Segment::Text(before.to_string()));
        }
        segments.push(Segment::Reminder(
            rest[after_open..close].trim().to_string(),
        ));
        rest = &rest[close + CLOSE_TAG.len()..];
    }

    if !rest.trim().is_empty() {
        segments.push(Segment::Text(rest.to_string()));
    }
    segments
}

/// True when `text` holds at least one complete reminder block — lets the
/// caller skip the split entirely for the overwhelmingly common case.
pub(super) fn has_system_reminder(text: &str) -> bool {
    text.find(OPEN_TAG)
        .is_some_and(|open| text[open + OPEN_TAG.len()..].contains(CLOSE_TAG))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_prose_from_reminder() {
        let segments =
            split_system_reminders("before\n<system-reminder>\nnote\n</system-reminder>");
        assert_eq!(
            segments,
            vec![
                Segment::Text("before\n".to_string()),
                Segment::Reminder("note".to_string()),
            ]
        );
    }

    #[test]
    fn keeps_trailing_prose_and_handles_several_blocks() {
        let segments = split_system_reminders(
            "a<system-reminder>one</system-reminder>b<system-reminder>two</system-reminder>c",
        );
        assert_eq!(
            segments,
            vec![
                Segment::Text("a".to_string()),
                Segment::Reminder("one".to_string()),
                Segment::Text("b".to_string()),
                Segment::Reminder("two".to_string()),
                Segment::Text("c".to_string()),
            ]
        );
    }

    /// A truncated block must read as literal text. Treating an unterminated
    /// open tag as "reminder to end of message" would silently swallow real
    /// content into a collapsed bar.
    #[test]
    fn unterminated_block_stays_prose() {
        let text = "visible <system-reminder> never closed";
        assert!(!has_system_reminder(text));
        assert_eq!(
            split_system_reminders(text),
            vec![Segment::Text(text.to_string())]
        );
    }

    #[test]
    fn plain_text_is_untouched() {
        assert!(!has_system_reminder("just prose"));
        assert_eq!(
            split_system_reminders("just prose"),
            vec![Segment::Text("just prose".to_string())]
        );
    }

    /// A message that is nothing but a reminder yields no empty prose segment.
    #[test]
    fn reminder_only_message_has_no_empty_text_segment() {
        assert_eq!(
            split_system_reminders("<system-reminder>solo</system-reminder>"),
            vec![Segment::Reminder("solo".to_string())]
        );
    }
}

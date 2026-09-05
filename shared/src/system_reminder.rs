//! Splitting machine-authored notice blocks out of message text.
//!
//! These blocks are out-of-band context for the agent — the portal-features
//! reminder, the inter-agent "reply to that agent, not the user" bumper, the
//! CLI's own injected notices. Two consumers need to separate them from the
//! surrounding prose, for opposite reasons:
//!
//! - the **frontend** collapses them into a clickable bar, because they are
//!   useful to *see* (they explain why an agent did something) but are noise
//!   inline — several are longer than the message carrying them;
//! - the **backend** strips them before classifying or summarizing message
//!   text, because since #1649 the portal reminder is folded into the *first
//!   user input* of every session rather than sent standalone, so anything
//!   that treats a user message as prose would otherwise ingest the whole
//!   reminder body.
//!
//! This lives in `shared` so those two stay in lockstep: a splitter that
//! disagrees with itself across the wire would show the user one thing and
//! feed the classifier another.

/// One piece of a message: ordinary prose, or a reminder to collapse.
#[derive(Debug, PartialEq)]
pub enum Segment {
    Text(String),
    Reminder(String),
    TaskNotification(String),
}

const OPEN_TAG: &str = "<system-reminder>";
const CLOSE_TAG: &str = "</system-reminder>";
const TASK_OPEN_TAG: &str = "<task-notification>";
const TASK_CLOSE_TAG: &str = "</task-notification>";

#[derive(Clone, Copy)]
enum NoticeKind {
    Reminder,
    TaskNotification,
}

impl NoticeKind {
    fn tags(self) -> (&'static str, &'static str) {
        match self {
            Self::Reminder => (OPEN_TAG, CLOSE_TAG),
            Self::TaskNotification => (TASK_OPEN_TAG, TASK_CLOSE_TAG),
        }
    }

    fn segment(self, body: String) -> Segment {
        match self {
            Self::Reminder => Segment::Reminder(body),
            Self::TaskNotification => Segment::TaskNotification(body),
        }
    }
}

fn next_notice(text: &str, include_task_notifications: bool) -> Option<(usize, NoticeKind)> {
    [NoticeKind::Reminder, NoticeKind::TaskNotification]
        .into_iter()
        .filter(|kind| include_task_notifications || matches!(kind, &NoticeKind::Reminder))
        .filter_map(|kind| text.find(kind.tags().0).map(|offset| (offset, kind)))
        .min_by_key(|(offset, _)| *offset)
}

/// Split `text` on `<system-reminder>` blocks.
///
/// An unterminated open tag is treated as prose, not as a reminder running to
/// the end of the message: a truncated or malformed block should read as the
/// literal text it is rather than swallowing everything after it into a bar.
pub fn split_system_reminders(text: &str) -> Vec<Segment> {
    split_notices(text, false)
}

/// Split prose from complete system-reminder and task-notification blocks.
///
/// Claude injects both shapes into otherwise ordinary message text. Keeping
/// their parsing together ensures the frontend never renders raw XML while
/// classifier-side consumers can remove the same out-of-band material.
pub fn split_collapsible_notices(text: &str) -> Vec<Segment> {
    split_notices(text, true)
}

fn split_notices(text: &str, include_task_notifications: bool) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut rest = text;

    while let Some((open, kind)) = next_notice(rest, include_task_notifications) {
        let (open_tag, close_tag) = kind.tags();
        let after_open = open + open_tag.len();
        let Some(close_rel) = rest[after_open..].find(close_tag) else {
            break;
        };
        let close = after_open + close_rel;

        let before = &rest[..open];
        if !before.trim().is_empty() {
            segments.push(Segment::Text(before.to_string()));
        }
        segments.push(kind.segment(rest[after_open..close].trim().to_string()));
        rest = &rest[close + close_tag.len()..];
    }

    if !rest.trim().is_empty() {
        segments.push(Segment::Text(rest.to_string()));
    }
    segments
}

/// True when `text` holds a complete collapsible notice of either kind.
pub fn has_collapsible_notice(text: &str) -> bool {
    [NoticeKind::Reminder, NoticeKind::TaskNotification]
        .into_iter()
        .any(|kind| {
            let (open_tag, close_tag) = kind.tags();
            text.find(open_tag)
                .is_some_and(|open| text[open + open_tag.len()..].contains(close_tag))
        })
}

/// True when `text` holds at least one complete reminder block — lets the
/// caller skip the split entirely for the overwhelmingly common case.
pub fn has_system_reminder(text: &str) -> bool {
    text.find(OPEN_TAG)
        .is_some_and(|open| text[open + OPEN_TAG.len()..].contains(CLOSE_TAG))
}

/// The prose of `text` with every complete reminder block removed.
///
/// The classifier-side counterpart to [`split_system_reminders`]: callers that
/// only want "what did the human/agent actually say" get it without walking
/// segments themselves.
pub fn strip_system_reminders(text: &str) -> String {
    if !has_system_reminder(text) {
        return text.to_string();
    }
    split_system_reminders(text)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Text(text) => Some(text),
            Segment::Reminder(_) => None,
            Segment::TaskNotification(_) => unreachable!("system-reminder-only split"),
        })
        .collect::<String>()
}

/// The prose of `text` with every complete collapsible notice removed.
pub fn strip_collapsible_notices(text: &str) -> String {
    if !has_collapsible_notice(text) {
        return text.to_string();
    }
    split_collapsible_notices(text)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Text(text) => Some(text),
            Segment::Reminder(_) | Segment::TaskNotification(_) => None,
        })
        .collect::<String>()
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

    #[test]
    fn splits_task_notification_from_surrounding_prose() {
        let notification = "<task-notification> <task-id>bmq5w2fik</task-id> <status>completed</status> <summary>Background command completed</summary> </task-notification>";
        assert_eq!(
            split_collapsible_notices(&format!("before {notification} after")),
            vec![
                Segment::Text("before ".to_string()),
                Segment::TaskNotification(
                    "<task-id>bmq5w2fik</task-id> <status>completed</status> <summary>Background command completed</summary>".to_string(),
                ),
                Segment::Text(" after".to_string()),
            ]
        );
    }

    #[test]
    fn strips_both_notice_kinds_from_prose() {
        let text = "say <system-reminder>context</system-reminder> hello <task-notification><status>completed</status></task-notification> now";
        assert_eq!(strip_collapsible_notices(text), "say  hello  now");
    }

    #[test]
    fn unterminated_task_notification_stays_prose() {
        let text = "visible <task-notification><status>running</status>";
        assert!(!has_collapsible_notice(text));
        assert_eq!(
            split_collapsible_notices(text),
            vec![Segment::Text(text.to_string())]
        );
    }

    /// The shape #1649 actually produces: reminder folded onto the front of
    /// the user's own first message. Stripping must leave the user's words.
    #[test]
    fn strip_leaves_the_users_words_from_a_folded_first_input() {
        let folded = "<system-reminder>\nAgent Portal version 2.13.0.\n\nbody\n</system-reminder>\n\ndo the thing";
        assert_eq!(strip_system_reminders(folded).trim(), "do the thing");
    }

    #[test]
    fn strip_is_identity_without_a_reminder() {
        assert_eq!(strip_system_reminders("just prose"), "just prose");
    }

    /// An unterminated block is prose, so stripping must not eat it.
    #[test]
    fn strip_keeps_an_unterminated_block() {
        let text = "visible <system-reminder> never closed";
        assert_eq!(strip_system_reminders(text), text);
    }
}

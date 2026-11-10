//! Only scheduler fires explain a new conversation turn. Ordinary command
//! queue/ack lifecycle frames remain transport bookkeeping.

use yew::prelude::*;

pub(crate) fn command_wakeup_label(
    message: &shared::CommandLifecycleMessage,
) -> Option<&'static str> {
    if message.state != shared::CommandLifecycleState::Started {
        return None;
    }
    message.wakeup_source.as_ref().map(|source| match source {
        claude_codes::CommandWakeupSource::LoopWakeup => "Woken by /loop",
        claude_codes::CommandWakeupSource::ScheduleWakeup => "Woken by scheduled task",
        claude_codes::CommandWakeupSource::Unknown(_) => "Automatic wakeup",
    })
}

pub(crate) fn render_command_wakeup(message: &shared::CommandLifecycleMessage) -> Html {
    let Some(label) = command_wakeup_label(message) else {
        return html! {};
    };
    // scheduled_for is epoch milliseconds, not seconds and not the actual
    // fire time. It is only meaningful on a dynamic /loop wakeup.
    let scheduled = matches!(
        message.wakeup_source,
        Some(claude_codes::CommandWakeupSource::LoopWakeup)
    )
    .then_some(message.scheduled_for)
    .flatten()
    .filter(|ms| ms.is_finite())
    .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
    .map(|time| format!("Scheduled for {}", time.format("%Y-%m-%d %H:%M:%S UTC")));
    html! {
        <div class="claude-message system-message compact wakeup-marker" role="status">
            <span class="message-type-badge system">{ label }</span>
            if let Some(scheduled) = scheduled {
                <span class="init-badge">{ scheduled }</span>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_started_scheduler_fires_make_markers() {
        let mut message: shared::CommandLifecycleMessage = serde_json::from_str(r#"{"command_uuid":"cmd","state":"started","wakeup_source":"loop_wakeup","scheduled_for":1893456000000,"uuid":"frame","session_id":"session"}"#).unwrap();
        assert_eq!(command_wakeup_label(&message), Some("Woken by /loop"));
        let text = super::super::visible_text(&render_command_wakeup(&message));
        assert!(text.contains("Scheduled for 2030-01-01 00:00:00 UTC"));
        message.state = shared::CommandLifecycleState::Completed;
        assert_eq!(command_wakeup_label(&message), None);
        message.state = shared::CommandLifecycleState::Queued;
        assert_eq!(command_wakeup_label(&message), None);
        message.state = shared::CommandLifecycleState::Started;
        message.wakeup_source = None;
        assert_eq!(command_wakeup_label(&message), None);
    }
}

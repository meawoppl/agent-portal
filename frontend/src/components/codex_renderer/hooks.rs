//! Hook diagnostics are plain text, never model context or executable markup.

use codex_codes::{HookCompletedNotification, HookEventName, HookOutputEntryKind, HookRunStatus};
use yew::prelude::*;

pub(super) fn render_hook_failure(notification: &HookCompletedNotification) -> Option<Html> {
    let run = &notification.run;
    if matches!(
        run.event_name,
        HookEventName::PreModelRequest | HookEventName::PostModelRequest
    ) || !matches!(run.status, HookRunStatus::Failed | HookRunStatus::Blocked)
    {
        return None;
    }
    let title = if run.status == HookRunStatus::Blocked {
        "Codex hook blocked work"
    } else {
        "Codex hook failed"
    };
    Some(html! {
        <div class="claude-message error-message-display codex-hook-failure" role="status">
            <div class="message-header">
                <span class="message-type-badge result error">{ title }</span>
                <span class="init-badge">{ format!("{:?}", run.event_name) }</span>
            </div>
            <div class="message-body">
                if !run.source_path.0.is_empty() {
                    <div class="error-recovery-hint">{ "Hook source: " }<code>{ &run.source_path.0 }</code></div>
                }
                if let Some(message) = &run.status_message {
                    <pre class="error-text">{ message }</pre>
                }
                { for run.entries.iter().filter(|entry| !matches!(entry.kind, HookOutputEntryKind::Context)).map(|entry| html! {
                    <pre class="error-text">{ &entry.text }</pre>
                }) }
                <div class="error-recovery-hint">{ "Review the hook's diagnostics before retrying the blocked work." }</div>
            </div>
        </div>
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(event: &str, status: &str) -> HookCompletedNotification {
        let json = format!(
            r#"{{"threadId":"thread","run":{{"eventName":"{event}","status":"{status}","executionMode":"sync","handlerType":"command","sourcePath":"/hooks/check","scope":"turn","entries":[{{"kind":"context","text":"private model context"}},{{"kind":"error","text":"![not an image](https://example.com/image.png)"}}]}}}}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    fn text(node: &Html) -> String {
        use yew::virtual_dom::VNode;
        match node {
            VNode::VText(text) => text.text.to_string(),
            VNode::VTag(tag) => tag.children().map(text).unwrap_or_default(),
            VNode::VList(children) => children.iter().map(text).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        }
    }

    #[test]
    fn failed_hook_card_keeps_diagnostics_plain_and_model_context_private() {
        let notification = notification("userPromptSubmit", "blocked");
        let card = render_hook_failure(&notification).unwrap();
        let body = text(&card);
        assert!(body.contains("Codex hook blocked work"));
        assert!(body.contains("![not an image](https://example.com/image.png)"));
        assert!(!body.contains("private model context"));
        let frame = super::super::CodexEvent::HookCompleted {
            params: Box::new(notification),
        };
        assert!(super::super::render_codex_frame_content(&frame, uuid::Uuid::nil()).is_some());
    }

    #[test]
    fn successful_and_model_request_hooks_have_no_card_even_in_old_transcripts() {
        assert!(render_hook_failure(&notification("preToolUse", "completed")).is_none());
        assert!(render_hook_failure(&notification("preModelRequest", "failed")).is_none());
        assert!(render_hook_failure(&notification("postModelRequest", "blocked")).is_none());
    }
}

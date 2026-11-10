//! User-facing task reports. With a handback discriminator, `summary` is
//! model-facing harness text and must never be used as the user report.

pub(crate) struct TaskReport<'a> {
    pub text: Option<&'a str>,
    pub warning: Option<&'a str>,
    pub label: Option<&'a str>,
}

impl<'a> TaskReport<'a> {
    pub fn from_notification(message: &'a shared::TaskNotificationMessage) -> Self {
        let handback = message.handback.as_ref();
        let report = message.handback_report.as_ref();
        let withheld = matches!(handback, Some(claude_codes::TaskHandback::Withheld));
        Self {
            text: if withheld {
                None
            } else if let Some(report) = report {
                Some(&report.text)
            } else if handback.is_none() {
                Some(&message.summary)
            } else {
                None
            },
            warning: report.and_then(|report| report.warning.as_deref()),
            label: handback.map(|kind| match kind {
                claude_codes::TaskHandback::Send => "Report delivered",
                claude_codes::TaskHandback::Flagged => "Report flagged",
                claude_codes::TaskHandback::Withheld => "Report withheld",
                claude_codes::TaskHandback::Unknown(_) => "Report status unknown",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification() -> shared::TaskNotificationMessage {
        serde_json::from_str(r#"{"session_id":"session","task_id":"task","status":"completed","summary":"private model notes"}"#).unwrap()
    }

    #[test]
    fn handback_uses_user_text_and_warning_instead_of_model_notes() {
        let mut message = notification();
        message.handback = Some(claude_codes::TaskHandback::Flagged);
        message.handback_report = Some(claude_codes::HandbackReport {
            text: "user report".into(),
            warning: Some("Security review warning".into()),
        });
        let report = TaskReport::from_notification(&message);
        assert_eq!(report.text, Some("user report"));
        assert_eq!(report.warning, Some("Security review warning"));
        assert_eq!(report.label, Some("Report flagged"));
    }

    #[test]
    fn withheld_and_missing_reports_never_expose_model_only_summary() {
        let mut message = notification();
        for kind in [
            claude_codes::TaskHandback::Withheld,
            claude_codes::TaskHandback::Send,
            claude_codes::TaskHandback::Unknown("future".into()),
        ] {
            message.handback = Some(kind);
            assert_eq!(TaskReport::from_notification(&message).text, None);
        }
        message.handback = None;
        assert_eq!(
            TaskReport::from_notification(&message).text,
            Some("private model notes")
        );
    }
}

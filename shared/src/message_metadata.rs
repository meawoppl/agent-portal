//! Helpers for the typed portal metadata sidecar that travels beside raw agent
//! message content.

use crate::{HistoryEntry, MessageSource, PortalMeta};

/// Extract the server-created timestamp from an optional metadata sidecar.
pub fn created_at_iso(meta: Option<&PortalMeta>) -> Option<&str> {
    meta.and_then(PortalMeta::created_at_iso)
}

/// Parse stored message content as JSON, falling back to a typed envelope when
/// the persisted row predates strict JSON storage or contains raw text.
pub fn content_value_or_fallback(role: &str, content: &str) -> serde_json::Value {
    #[derive(serde::Serialize)]
    struct FallbackMessageContent<'a> {
        #[serde(rename = "type")]
        message_type: &'a str,
        content: &'a str,
    }

    serde_json::from_str::<serde_json::Value>(content).unwrap_or_else(|_| {
        serde_json::to_value(FallbackMessageContent {
            message_type: role,
            content,
        })
        .unwrap_or(serde_json::Value::Null)
    })
}

impl PortalMeta {
    /// Server-assigned persisted-row timestamp as an ISO string.
    pub fn created_at_iso(&self) -> Option<&str> {
        self.created_at.as_deref()
    }

    /// Typed origin/attribution for this message, when it is not the session's
    /// own agent output.
    pub fn source(&self) -> Option<&MessageSource> {
        self.source.as_ref()
    }
}

impl HistoryEntry {
    /// Server-assigned timestamp from this historical row's metadata sidecar.
    pub fn created_at_iso(&self) -> Option<&str> {
        created_at_iso(self.meta.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn created_at_iso_reads_present_timestamp() {
        let meta = PortalMeta {
            created_at: Some("2026-07-19T00:00:00.000000".to_string()),
            ..PortalMeta::default()
        };

        assert_eq!(
            created_at_iso(Some(&meta)),
            Some("2026-07-19T00:00:00.000000")
        );
        assert_eq!(meta.created_at_iso(), Some("2026-07-19T00:00:00.000000"));
    }

    #[test]
    fn created_at_iso_handles_missing_metadata() {
        assert_eq!(created_at_iso(None), None);
        assert_eq!(PortalMeta::default().created_at_iso(), None);
    }

    #[test]
    fn source_reads_sender_metadata() {
        let account_id = Uuid::from_u128(42);
        let meta = PortalMeta {
            source: Some(MessageSource::Human {
                account_id,
                name: "Matt".to_string(),
            }),
            ..PortalMeta::default()
        };

        assert_eq!(
            meta.source(),
            Some(&MessageSource::Human {
                account_id,
                name: "Matt".to_string(),
            })
        );
    }

    #[test]
    fn content_value_or_fallback_preserves_valid_json() {
        let value = content_value_or_fallback("assistant", r#"{"type":"assistant","ok":true}"#);
        assert_eq!(value["type"], "assistant");
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn content_value_or_fallback_wraps_malformed_json() {
        let value = content_value_or_fallback("portal", "raw text");
        assert_eq!(value["type"], "portal");
        assert_eq!(value["content"], "raw text");
    }

    #[test]
    fn history_entry_exposes_created_at_iso() {
        let entry = HistoryEntry {
            content: serde_json::Value::Null,
            meta: Some(PortalMeta {
                created_at: Some("2026-07-19T01:02:03.000000".to_string()),
                ..PortalMeta::default()
            }),
        };

        assert_eq!(entry.created_at_iso(), Some("2026-07-19T01:02:03.000000"));
    }
}

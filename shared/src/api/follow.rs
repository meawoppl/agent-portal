//! Durable peer transcript cursors. Connectivity is a live snapshot, not history.
use super::SessionActivityState;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowEvent {
    pub cursor: String,
    pub message_id: Uuid,
    /// Activity observed in this durable row; never historical connectivity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionActivityState>,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowPage {
    pub cursor: String,
    pub events: Vec<FollowEvent>,
    pub state: SessionActivityState,
    pub connected: bool,
    pub more: bool,
}

/// Opaque to callers; scoped to a session to reject accidental cross-peer reuse.
pub fn follow_cursor(session: Uuid, sequence: i64) -> String {
    format!("v1:{session}:{sequence}")
}

pub fn parse_follow_cursor(value: &str, session: Uuid) -> Option<i64> {
    let sequence = value
        .strip_prefix(&format!("v1:{session}:"))?
        .parse::<i64>()
        .ok()?;
    (sequence >= 0).then_some(sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_follow_output_decodes_without_lifecycle_metadata() {
        let record: FollowRecord = serde_json::from_value(serde_json::json!({
            "kind":"batch", "cursor":"v1:old:1", "start_cursor":null,
            "state":"idle", "connected":true,
            "events":[{"cursor":"v1:old:1", "message_id":Uuid::nil(), "kind":"text", "text":"hello"}]
        })).unwrap();
        assert_eq!(record.source, None);
        assert_eq!(record.events[0].activity, None);
    }

    #[test]
    fn cursor_is_scoped_and_nonnegative() {
        let id = Uuid::new_v4();
        assert_eq!(parse_follow_cursor(&follow_cursor(id, 42), id), Some(42));
        assert_eq!(
            parse_follow_cursor(&follow_cursor(id, 42), Uuid::new_v4()),
            None
        );
        assert_eq!(parse_follow_cursor(&follow_cursor(id, -1), id), None);
        assert_eq!(parse_follow_cursor("nonsense", id), None);
    }
}

/// Stable line-oriented CLI output; checkpoint only after consuming the record.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowRecordKind {
    Batch,
    Status,
    Heartbeat,
}
impl FollowRecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Status => "status",
            Self::Heartbeat => "heartbeat",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FollowRecordSource {
    Durable,
    Snapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowRecord {
    /// Events are journal rows; state/connected always describe the current peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FollowRecordSource>,
    pub kind: FollowRecordKind,
    pub cursor: String,
    pub start_cursor: Option<String>,
    pub state: SessionActivityState,
    pub connected: bool,
    pub events: Vec<FollowEvent>,
}

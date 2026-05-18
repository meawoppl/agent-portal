//! Session snapshot types for persistence

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::buffer::BufferedOutput;

/// Configuration for creating a session
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionConfig {
    /// Unique session identifier
    pub session_id: Uuid,
    /// Working directory for the Claude session
    pub working_directory: PathBuf,
    /// Human-readable session name
    pub session_name: String,
    /// Whether to resume an existing Claude session (vs create new)
    pub resume: bool,
    /// Optional path to claude binary (defaults to "claude" in PATH)
    pub claude_path: Option<PathBuf>,
    /// Extra arguments to pass to the claude CLI
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Which agent CLI to use
    #[serde(default)]
    pub agent_type: shared::AgentType,
}

/// A pending permission request that hasn't been responded to
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPermission {
    /// Unique request identifier (string format from Claude)
    pub request_id: String,
    /// Name of the tool requesting permission
    pub tool_name: String,
    /// Tool input parameters
    pub input: serde_json::Value,
    /// When the request was received
    pub requested_at: DateTime<Utc>,
}

/// Serializable session state for persistence
///
/// This captures everything needed to restore a session after
/// a service restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Session identifier
    pub id: Uuid,
    /// Session configuration
    pub config: SessionConfig,
    /// Buffered outputs not yet acknowledged by consumers
    pub pending_outputs: Vec<BufferedOutput>,
    /// Pending permission request (if any)
    pub pending_permission: Option<PendingPermission>,
    /// Timestamp of last activity
    pub last_activity: DateTime<Utc>,
    /// Whether the Claude process was running when snapshot was taken
    pub was_running: bool,
}

impl SessionSnapshot {
    /// Create a new snapshot
    pub fn new(
        id: Uuid,
        config: SessionConfig,
        pending_outputs: Vec<BufferedOutput>,
        pending_permission: Option<PendingPermission>,
        was_running: bool,
    ) -> Self {
        Self {
            id,
            config,
            pending_outputs,
            pending_permission,
            last_activity: Utc::now(),
            was_running,
        }
    }

    /// Serialize snapshot to JSON bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    /// Deserialize snapshot from JSON bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> SessionConfig {
        SessionConfig {
            session_id: Uuid::new_v4(),
            working_directory: PathBuf::from("/tmp/test"),
            session_name: "test-session".to_string(),
            resume: false,
            claude_path: None,
            extra_args: vec![],
            agent_type: Default::default(),
        }
    }

    #[test]
    fn test_session_config_serialization() {
        let config = sample_config();
        let json = serde_json::to_string(&config).unwrap();
        let restored: SessionConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.session_id, restored.session_id);
        assert_eq!(config.working_directory, restored.working_directory);
        assert_eq!(config.session_name, restored.session_name);
        assert_eq!(config.resume, restored.resume);
        assert_eq!(config.claude_path, restored.claude_path);
    }

    #[test]
    fn test_pending_permission_serialization() {
        let perm = PendingPermission {
            request_id: "req-123".to_string(),
            tool_name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls -la"}),
            requested_at: Utc::now(),
        };

        let json = serde_json::to_string(&perm).unwrap();
        let restored: PendingPermission = serde_json::from_str(&json).unwrap();

        assert_eq!(perm.request_id, restored.request_id);
        assert_eq!(perm.tool_name, restored.tool_name);
        assert_eq!(perm.input, restored.input);
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let config = sample_config();
        let id = config.session_id;

        let pending_outputs = vec![
            BufferedOutput {
                seq: 0,
                content: serde_json::json!({"type": "output", "text": "hello"}),
                timestamp: Utc::now(),
            },
            BufferedOutput {
                seq: 1,
                content: serde_json::json!({"type": "output", "text": "world"}),
                timestamp: Utc::now(),
            },
        ];

        let pending_permission = Some(PendingPermission {
            request_id: "perm-456".to_string(),
            tool_name: "Write".to_string(),
            input: serde_json::json!({"file_path": "/tmp/test.txt"}),
            requested_at: Utc::now(),
        });

        let snapshot = SessionSnapshot::new(id, config, pending_outputs, pending_permission, true);

        // Serialize to bytes
        let bytes = snapshot.to_bytes().unwrap();

        // Deserialize
        let restored = SessionSnapshot::from_bytes(&bytes).unwrap();

        assert_eq!(restored.id, id);
        assert_eq!(restored.pending_outputs.len(), 2);
        assert!(restored.pending_permission.is_some());
        assert!(restored.was_running);
        assert_eq!(restored.pending_permission.unwrap().tool_name, "Write");
    }

    #[test]
    fn test_snapshot_without_pending_permission() {
        let config = sample_config();
        let id = config.session_id;

        let snapshot = SessionSnapshot::new(id, config, vec![], None, false);

        let bytes = snapshot.to_bytes().unwrap();
        let restored = SessionSnapshot::from_bytes(&bytes).unwrap();

        assert_eq!(restored.id, id);
        assert!(restored.pending_outputs.is_empty());
        assert!(restored.pending_permission.is_none());
        assert!(!restored.was_running);
    }
}

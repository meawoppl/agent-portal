use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub use ws_bridge::WsEndpoint;

use crate::{
    AgentType, DirectoryEntry, PermissionSuggestion, SendMode, SessionCost, SessionStatus,
};

// =============================================================================
// Shared field structs — used by both proxy and client endpoints
// =============================================================================

/// Fields for session registration (shared by proxy and web client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterFields {
    pub session_id: Uuid,
    pub session_name: String,
    pub auth_token: Option<String>,
    pub working_directory: String,
    #[serde(default)]
    pub resuming: bool,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub replay_after: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub replaces_session_id: Option<Uuid>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub launcher_id: Option<Uuid>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub scheduled_task_id: Option<Uuid>,
}

/// Configuration for a scheduled task, sent from backend to launcher via ScheduleSync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTaskConfig {
    pub id: Uuid,
    pub name: String,
    pub cron_expression: String,
    pub timezone: String,
    pub working_directory: String,
    pub prompt: String,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    pub enabled: bool,
    pub max_runtime_minutes: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<Uuid>,
}

/// Fields for a permission response (shared by server-to-proxy and client-to-server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponseFields {
    pub request_id: String,
    pub allow: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PermissionSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Fields for starting a file upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadStartFields {
    pub upload_id: String,
    pub filename: String,
    pub content_type: String,
    pub total_chunks: u32,
    #[serde(default)]
    pub total_size: u64,
}

/// Fields for a single file upload chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadChunkFields {
    pub upload_id: String,
    pub chunk_index: u32,
    pub data: String,
}

// =============================================================================
// Session endpoint: proxy <-> backend (/ws/session)
// =============================================================================

pub struct SessionEndpoint;

impl WsEndpoint for SessionEndpoint {
    const PATH: &'static str = "/ws/session";
    type ServerMsg = ServerToProxy;
    type ClientMsg = ProxyToServer;
}

/// Messages the proxy sends to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProxyToServer {
    /// Register a new session or connect to an existing one
    Register(RegisterFields),

    /// Raw output from Claude Code (unsequenced fallback)
    ClaudeOutput { content: serde_json::Value },

    /// Sequenced output from Claude Code
    SequencedOutput {
        seq: u64,
        content: serde_json::Value,
    },

    /// Keepalive heartbeat
    Heartbeat,

    /// Permission request from Claude (tool wants to execute)
    PermissionRequest {
        request_id: String,
        tool_name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permission_suggestions: Vec<PermissionSuggestion>,
    },

    /// Update session metadata (e.g., git branch changed)
    SessionUpdate {
        session_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_branch: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pr_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        repo_url: Option<String>,
    },

    /// Acknowledge receipt of input messages
    InputAck { session_id: Uuid, ack_seq: i64 },

    /// Session status update
    SessionStatus { status: SessionStatus },
}

/// Messages the backend sends to the proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerToProxy {
    /// Acknowledge session registration
    RegisterAck {
        success: bool,
        session_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        max_image_mb: u32,
    },

    /// Keepalive heartbeat
    Heartbeat,

    /// User input (unsequenced fallback)
    ClaudeInput {
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send_mode: Option<SendMode>,
    },

    /// Sequenced user input
    SequencedInput {
        session_id: Uuid,
        seq: i64,
        content: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        send_mode: Option<SendMode>,
    },

    /// User's permission decision
    PermissionResponse(PermissionResponseFields),

    /// Acknowledge receipt of output messages
    OutputAck { session_id: Uuid, ack_seq: u64 },

    /// Start a chunked file upload to the proxy's working directory
    FileUploadStart(FileUploadStartFields),

    /// A single chunk of a file upload (base64-encoded, ~1KB decoded)
    FileUploadChunk(FileUploadChunkFields),

    /// Server is shutting down (proxy should reconnect after delay)
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },

    /// Session has been terminated (proxy should NOT reconnect)
    SessionTerminated { reason: String },

    /// Interrupt the current Claude response
    Interrupt,
}

// =============================================================================
// Client endpoint: frontend <-> backend (/ws/client)
// =============================================================================

pub struct ClientEndpoint;

impl WsEndpoint for ClientEndpoint {
    const PATH: &'static str = "/ws/client";
    type ServerMsg = ServerToClient;
    type ClientMsg = ClientToServer;
}

/// Messages the frontend sends to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientToServer {
    /// Register to receive updates for a session
    Register(RegisterFields),

    /// User sends input to Claude
    ClaudeInput {
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send_mode: Option<SendMode>,
    },

    /// User's permission decision
    PermissionResponse(PermissionResponseFields),

    /// Start a chunked file upload
    FileUploadStart(FileUploadStartFields),

    /// A single chunk of a file upload
    FileUploadChunk(FileUploadChunkFields),

    /// Interrupt the current Claude response
    Interrupt,
}

/// Messages the backend sends to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerToClient {
    /// Output from Claude Code
    ClaudeOutput {
        content: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        sender_user_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        sender_name: Option<String>,
    },

    /// Batch of historical messages for replay
    HistoryBatch { messages: Vec<serde_json::Value> },

    /// Permission request from Claude (tool wants to execute)
    PermissionRequest {
        request_id: String,
        tool_name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permission_suggestions: Vec<PermissionSuggestion>,
    },

    /// Error message
    Error { message: String },

    /// Session metadata changed
    SessionUpdate {
        session_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_branch: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pr_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        repo_url: Option<String>,
    },

    /// User spend data update
    UserSpendUpdate {
        total_spend_usd: f64,
        session_costs: Vec<SessionCost>,
    },

    /// Server is shutting down
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },

    /// Session status changed
    SessionStatus { status: SessionStatus },

    /// Launch session result (forwarded from launcher)
    LaunchSessionResult {
        request_id: Uuid,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// A proxy process exited (forwarded from launcher)
    SessionExited {
        session_id: Uuid,
        exit_code: Option<i32>,
    },
}

// =============================================================================
// Launcher endpoint: launcher <-> backend (/ws/launcher)
// =============================================================================

pub struct LauncherEndpoint;

impl WsEndpoint for LauncherEndpoint {
    const PATH: &'static str = "/ws/launcher";
    type ServerMsg = ServerToLauncher;
    type ClientMsg = LauncherToServer;
}

/// Messages the launcher sends to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LauncherToServer {
    /// Register a launcher daemon
    LauncherRegister {
        launcher_id: Uuid,
        launcher_name: String,
        auth_token: Option<String>,
        hostname: String,
        #[serde(default)]
        version: Option<String>,
        /// Working directory where the launcher process is running
        #[serde(default)]
        working_directory: Option<String>,
    },

    /// Result of a launch request
    LaunchSessionResult {
        request_id: Uuid,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Launcher heartbeat with running process summary
    LauncherHeartbeat {
        launcher_id: Uuid,
        running_sessions: Vec<Uuid>,
        uptime_secs: u64,
    },

    /// Log output from a proxy process
    ProxyLog {
        session_id: Uuid,
        level: String,
        message: String,
        timestamp: String,
    },

    /// A proxy process exited
    SessionExited {
        session_id: Uuid,
        exit_code: Option<i32>,
    },

    /// Directory listing response
    ListDirectoriesResult {
        request_id: Uuid,
        #[serde(default)]
        entries: Vec<DirectoryEntry>,
        error: Option<String>,
        #[serde(default)]
        resolved_path: Option<String>,
    },

    /// Request the backend to mint a token and launch a session
    RequestLaunch {
        request_id: Uuid,
        working_directory: String,
        #[serde(default)]
        session_name: Option<String>,
        #[serde(default)]
        claude_args: Vec<String>,
        #[serde(default)]
        agent_type: AgentType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_task_id: Option<Uuid>,
    },

    /// Inject input into a session on behalf of the scheduler
    InjectInput { session_id: Uuid, content: String },

    /// Report that a scheduled task run has started
    ScheduledRunStarted { task_id: Uuid, session_id: Uuid },

    /// Report that a scheduled task run has completed
    ScheduledRunCompleted {
        task_id: Uuid,
        session_id: Uuid,
        exit_code: Option<i32>,
        duration_secs: u64,
    },
}

/// Messages the backend sends to the launcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerToLauncher {
    /// Acknowledge launcher registration
    LauncherRegisterAck {
        success: bool,
        launcher_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// If true the launcher must not retry — it should exit immediately
        #[serde(default)]
        fatal: bool,
    },

    /// Request to launch a new proxy instance
    LaunchSession {
        request_id: Uuid,
        user_id: Uuid,
        auth_token: String,
        working_directory: String,
        #[serde(default)]
        session_name: Option<String>,
        #[serde(default)]
        claude_args: Vec<String>,
        #[serde(default)]
        agent_type: AgentType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_task_id: Option<Uuid>,
    },

    /// Request to stop a running session
    StopSession { session_id: Uuid },

    /// Request directory listing
    ListDirectories { request_id: Uuid, path: String },

    /// Server is shutting down
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },

    /// Sync scheduled task definitions to the launcher
    ScheduleSync { tasks: Vec<ScheduledTaskConfig> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_endpoint_path() {
        assert_eq!(SessionEndpoint::PATH, "/ws/session");
    }

    #[test]
    fn client_endpoint_path() {
        assert_eq!(ClientEndpoint::PATH, "/ws/client");
    }

    #[test]
    fn launcher_endpoint_path() {
        assert_eq!(LauncherEndpoint::PATH, "/ws/launcher");
    }

    #[test]
    fn proxy_to_server_register_roundtrip() {
        let msg = ProxyToServer::Register(RegisterFields {
            session_id: Uuid::nil(),
            session_name: "test".into(),
            auth_token: None,
            working_directory: "/tmp".into(),
            resuming: false,
            git_branch: None,
            replay_after: None,
            client_version: None,
            replaces_session_id: None,
            hostname: None,
            launcher_id: None,
            agent_type: AgentType::Claude,
            repo_url: None,
            scheduled_task_id: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"Register""#));
        let parsed: ProxyToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            ProxyToServer::Register(reg) => {
                assert_eq!(reg.session_name, "test");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_to_proxy_sequenced_input_roundtrip() {
        let msg = ServerToProxy::SequencedInput {
            session_id: Uuid::nil(),
            seq: 5,
            content: serde_json::json!({"text": "hello"}),
            send_mode: Some(SendMode::Wiggum),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"SequencedInput""#));
        let parsed: ServerToProxy = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToProxy::SequencedInput { seq, send_mode, .. } => {
                assert_eq!(seq, 5);
                assert_eq!(send_mode, Some(SendMode::Wiggum));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn client_to_server_claude_input_roundtrip() {
        let msg = ClientToServer::ClaudeInput {
            content: serde_json::json!({"text": "hi"}),
            send_mode: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ClaudeInput""#));
        let parsed: ClientToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientToServer::ClaudeInput { send_mode, .. } => {
                assert!(send_mode.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_to_client_output_roundtrip() {
        let msg = ServerToClient::ClaudeOutput {
            content: serde_json::json!({"type": "assistant", "text": "hello"}),
            sender_user_id: None,
            sender_name: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerToClient = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToClient::ClaudeOutput { content, .. } => {
                assert_eq!(content["text"], "hello");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn launcher_to_server_register_roundtrip() {
        let msg = LauncherToServer::LauncherRegister {
            launcher_id: Uuid::nil(),
            launcher_name: "test-launcher".into(),
            auth_token: Some("tok".into()),
            hostname: "host1".into(),
            version: Some("1.0".into()),
            working_directory: Some("/home/user/project".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"LauncherRegister""#));
        let parsed: LauncherToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            LauncherToServer::LauncherRegister { launcher_name, .. } => {
                assert_eq!(launcher_name, "test-launcher");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_to_launcher_launch_roundtrip() {
        let msg = ServerToLauncher::LaunchSession {
            request_id: Uuid::nil(),
            user_id: Uuid::nil(),
            auth_token: "token".into(),
            working_directory: "/home".into(),
            session_name: Some("my-session".into()),
            claude_args: vec!["--verbose".into()],
            agent_type: AgentType::Claude,
            scheduled_task_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"LaunchSession""#));
        let parsed: ServerToLauncher = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToLauncher::LaunchSession {
                working_directory,
                claude_args,
                ..
            } => {
                assert_eq!(working_directory, "/home");
                assert_eq!(claude_args, vec!["--verbose"]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Verify wire-format compatibility of per-endpoint types.
    #[test]
    fn wire_compat_register() {
        // Register JSON format
        let json = r#"{
            "type": "Register",
            "session_id": "550e8400-e29b-41d4-a716-446655440000",
            "session_name": "test",
            "auth_token": null,
            "working_directory": "/tmp"
        }"#;
        // Must parse as both ProxyToServer and ClientToServer
        let _: ProxyToServer = serde_json::from_str(json).unwrap();
        let _: ClientToServer = serde_json::from_str(json).unwrap();
    }

    #[test]
    fn launcher_request_launch_roundtrip() {
        let msg = LauncherToServer::RequestLaunch {
            request_id: Uuid::nil(),
            working_directory: "/home/user/project".into(),
            session_name: Some("my-project".into()),
            claude_args: vec!["--verbose".into()],
            agent_type: AgentType::Claude,
            scheduled_task_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"RequestLaunch""#));
        let parsed: LauncherToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            LauncherToServer::RequestLaunch {
                working_directory,
                session_name,
                claude_args,
                ..
            } => {
                assert_eq!(working_directory, "/home/user/project");
                assert_eq!(session_name.as_deref(), Some("my-project"));
                assert_eq!(claude_args, vec!["--verbose"]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn wire_compat_server_shutdown() {
        let json = r#"{"type":"ServerShutdown","reason":"update","reconnect_delay_ms":5000}"#;
        // Must parse in all three server->X enums
        let _: ServerToProxy = serde_json::from_str(json).unwrap();
        let _: ServerToClient = serde_json::from_str(json).unwrap();
        let _: ServerToLauncher = serde_json::from_str(json).unwrap();
    }

    #[test]
    fn wire_compat_session_terminated() {
        let json = r#"{"type":"SessionTerminated","reason":"Session stopped by user"}"#;
        let msg: ServerToProxy = serde_json::from_str(json).unwrap();
        match msg {
            ServerToProxy::SessionTerminated { reason } => {
                assert_eq!(reason, "Session stopped by user");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn schedule_sync_roundtrip() {
        let msg = ServerToLauncher::ScheduleSync {
            tasks: vec![ScheduledTaskConfig {
                id: Uuid::nil(),
                name: "nightly audit".into(),
                cron_expression: "0 3 * * *".into(),
                timezone: "UTC".into(),
                working_directory: "/home/user/project".into(),
                prompt: "Check deps".into(),
                claude_args: vec![],
                agent_type: AgentType::Claude,
                enabled: true,
                max_runtime_minutes: 30,
                last_session_id: None,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ScheduleSync""#));
        let parsed: ServerToLauncher = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToLauncher::ScheduleSync { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].name, "nightly audit");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn inject_input_roundtrip() {
        let msg = LauncherToServer::InjectInput {
            session_id: Uuid::nil(),
            content: "Check for updates".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"InjectInput""#));
        let _: LauncherToServer = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn scheduled_run_started_roundtrip() {
        let msg = LauncherToServer::ScheduledRunStarted {
            task_id: Uuid::nil(),
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ScheduledRunStarted""#));
        let _: LauncherToServer = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn scheduled_run_completed_roundtrip() {
        let msg = LauncherToServer::ScheduledRunCompleted {
            task_id: Uuid::nil(),
            session_id: Uuid::nil(),
            exit_code: Some(0),
            duration_secs: 120,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ScheduledRunCompleted""#));
        let _: LauncherToServer = serde_json::from_str(&json).unwrap();
    }
}

mod client;
mod launcher;
mod session;
mod tunnel;
mod types;
// The `_tests` suffix matters: scripts/check-no-json-macro.py allows `json!`
// only in `*_tests.rs` files, `tests/` dirs, or in-file `#[cfg(test)] mod`
// wrappers — it cannot see this declaration-site gate.
#[cfg(test)]
mod wire_goldens_tests;

pub use client::*;
pub use launcher::*;
pub use session::*;
pub use tunnel::*;
pub use types::*;
pub use ws_bridge::WsEndpoint;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentType, SessionMode};
    use uuid::Uuid;

    #[test]
    fn session_exited_reason_defaults_for_old_launchers() {
        // An older launcher omits `reason`; it must deserialize as the neutral
        // default rather than failing, so a mixed-version fleet keeps working.
        let json = r#"{"type":"SessionExited","session_id":"00000000-0000-0000-0000-000000000000","exit_code":0}"#;
        let msg: LauncherToServer = serde_json::from_str(json).unwrap();
        match msg {
            LauncherToServer::SessionExited { reason, .. } => {
                assert_eq!(reason, SessionExitReason::Completed);
            }
            _ => panic!("expected SessionExited"),
        }
    }

    #[test]
    fn session_exit_reason_roundtrips() {
        let json = serde_json::to_string(&SessionExitReason::CrashedEarly).unwrap();
        assert_eq!(json, "\"crashed_early\"");
        let back: SessionExitReason = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SessionExitReason::CrashedEarly);
    }

    /// Wire compat for the capability/ticket plumbing (#1506): an older proxy
    /// sends no `capabilities`, and an older backend sends no
    /// `tunnel_data_ticket`. Both must parse as "feature absent" rather than
    /// failing the frame and wedging registration.
    #[test]
    fn tunnel_data_plane_fields_default_for_older_peers() {
        let legacy_register = r#"{"type":"Register","session_id":"00000000-0000-0000-0000-000000000000","session_name":"s","auth_token":null,"working_directory":"/tmp"}"#;
        match serde_json::from_str::<ProxyToServer>(legacy_register).unwrap() {
            ProxyToServer::Register(reg) => assert!(reg.capabilities.is_empty()),
            _ => panic!("expected Register"),
        }

        let legacy_ack = r#"{"type":"RegisterAck","success":true,"session_id":"00000000-0000-0000-0000-000000000000","max_image_mb":10}"#;
        match serde_json::from_str::<ServerToProxy>(legacy_ack).unwrap() {
            ServerToProxy::RegisterAck {
                tunnel_data_ticket, ..
            } => assert!(tunnel_data_ticket.is_none()),
            _ => panic!("expected RegisterAck"),
        }

        // And the ticket + sizing are omitted from the wire entirely when
        // absent, so an older proxy sees a byte-identical ack to what it got
        // before.
        let ack = ServerToProxy::RegisterAck {
            success: true,
            session_id: Uuid::nil(),
            error: None,
            max_image_mb: 10,
            retryable: false,
            tunnel_data_ticket: None,
            tunnel_sizing: None,
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert!(!json.contains("tunnel_data_ticket"));
        assert!(!json.contains("tunnel_sizing"));
    }

    /// The sizing negotiation (#1511): the backend picks the largest profile the
    /// proxy advertised, and a v2-capable proxy still advertises v1 so a v1
    /// backend (which never sees v2) keeps issuing V1.
    #[test]
    fn tunnel_sizing_negotiation() {
        use crate::{
            TunnelSizing, PROXY_CAPABILITY_TUNNEL_BINARY_V1, PROXY_CAPABILITY_TUNNEL_BINARY_V2,
        };

        // No capabilities (pre-#1506 proxy) → V1.
        assert_eq!(TunnelSizing::negotiate(&[]), TunnelSizing::V1);
        // v1 only → V1.
        assert_eq!(
            TunnelSizing::negotiate(&[PROXY_CAPABILITY_TUNNEL_BINARY_V1.to_string()]),
            TunnelSizing::V1
        );
        // v1 + v2 (the shape a v2 proxy actually sends) → V2.
        assert_eq!(
            TunnelSizing::negotiate(&[
                PROXY_CAPABILITY_TUNNEL_BINARY_V1.to_string(),
                PROXY_CAPABILITY_TUNNEL_BINARY_V2.to_string(),
            ]),
            TunnelSizing::V2
        );
        // V2 is strictly larger, and both keep the 4×-window ratio. Bind to
        // runtime locals so this isn't a compile-time-const assertion
        // (clippy::assertions_on_constants).
        let (v1, v2) = (TunnelSizing::V1, TunnelSizing::V2);
        assert!(v2.max_chunk > v1.max_chunk);
        assert_eq!(v1.initial_window / v1.max_chunk, 4);
        assert_eq!(v2.initial_window / v2.max_chunk, 4);
    }

    /// A v2 sizing survives the RegisterAck round-trip; older backends omit it
    /// and the proxy reads `None` (→ V1).
    #[test]
    fn register_ack_carries_tunnel_sizing() {
        let ack = ServerToProxy::RegisterAck {
            success: true,
            session_id: Uuid::nil(),
            error: None,
            max_image_mb: 10,
            retryable: false,
            tunnel_data_ticket: Some("tok".to_string()),
            tunnel_sizing: Some(crate::TunnelSizing::V2),
        };
        let json = serde_json::to_string(&ack).unwrap();
        match serde_json::from_str::<ServerToProxy>(&json).unwrap() {
            ServerToProxy::RegisterAck { tunnel_sizing, .. } => {
                assert_eq!(tunnel_sizing, Some(crate::TunnelSizing::V2));
            }
            _ => panic!("expected RegisterAck"),
        }

        let legacy = r#"{"type":"RegisterAck","success":true,"session_id":"00000000-0000-0000-0000-000000000000","max_image_mb":10}"#;
        match serde_json::from_str::<ServerToProxy>(legacy).unwrap() {
            ServerToProxy::RegisterAck { tunnel_sizing, .. } => assert!(tunnel_sizing.is_none()),
            _ => panic!("expected RegisterAck"),
        }
    }

    #[test]
    fn message_source_is_a_tagged_sum() {
        use crate::endpoints::client::MessageSource;
        let human = MessageSource::Human {
            account_id: Uuid::nil(),
            name: "Matt".to_string(),
        };
        let json = serde_json::to_string(&human).unwrap();
        assert!(json.contains(r#""kind":"human""#));
        assert_eq!(
            serde_json::to_string(&MessageSource::Portal).unwrap(),
            r#"{"kind":"portal"}"#
        );
        let back: MessageSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, human);
    }

    #[test]
    fn delivery_meta_pending_is_derived_from_stage() {
        use crate::endpoints::client::DeliveryMeta;
        use crate::InputDeliveryStage;

        let d = |stage| DeliveryMeta {
            client_msg_id: Uuid::nil(),
            stage,
            message: None,
        };
        assert!(d(None).pending()); // submitted, no ack yet
        assert!(d(Some(InputDeliveryStage::ServerReceived)).pending());
        assert!(d(Some(InputDeliveryStage::ProxyReceived)).pending());
        assert!(!d(Some(InputDeliveryStage::AgentAccepted)).pending());
        assert!(!d(Some(InputDeliveryStage::Failed)).pending());
    }

    #[test]
    fn agent_output_meta_is_back_compatible() {
        use crate::endpoints::client::ServerToClient;

        // An old-backend frame without `meta`/`message_meta` still parses.
        // (The variant serializes with the legacy `ClaudeOutput` tag.)
        let legacy = r#"{"type":"ClaudeOutput","content":{"type":"portal"}}"#;
        let parsed: ServerToClient = serde_json::from_str(legacy).unwrap();
        match parsed {
            ServerToClient::AgentOutput { meta, .. } => assert!(meta.is_none()),
            _ => panic!("Wrong variant"),
        }

        // A pre-#1139 HistoryBatch (carried `messages`, not `entries`) degrades
        // to an empty batch rather than failing the whole frame (`entries` is
        // serde(default)). The frontend recovers on the next batch / refresh.
        let legacy_history = r#"{"type":"HistoryBatch","messages":[{"type":"portal"}]}"#;
        let parsed: ServerToClient = serde_json::from_str(legacy_history).unwrap();
        match parsed {
            ServerToClient::HistoryBatch { entries, .. } => assert!(entries.is_empty()),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn wire_compat_pre_2_5_42_omits_agent_type() {
        let json = r#"{"type":"SequencedOutput","seq":3,"content":{"hello":"world"}}"#;
        let parsed: ProxyToServer = serde_json::from_str(json).unwrap();
        match parsed {
            ProxyToServer::SequencedOutput { agent_type, .. } => {
                assert_eq!(agent_type, AgentType::Claude);
            }
            _ => panic!("Wrong variant"),
        }

        let json = r#"{"type":"ClaudeOutput","content":{"hello":"world"}}"#;
        let parsed: ProxyToServer = serde_json::from_str(json).unwrap();
        match parsed {
            ProxyToServer::AgentOutput { agent_type, .. } => {
                assert_eq!(agent_type, AgentType::Claude);
            }
            _ => panic!("Wrong variant"),
        }

        let json = r#"{"type":"ClaudeOutput","content":{"hello":"world"}}"#;
        let parsed: ServerToClient = serde_json::from_str(json).unwrap();
        match parsed {
            ServerToClient::AgentOutput { agent_type, .. } => {
                assert_eq!(agent_type, AgentType::Claude);
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
            capabilities: vec![crate::LAUNCHER_CAPABILITY_CREATE_WORKTREE.to_string()],
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
            resume_session_id: None,
            resume: None,
            create_worktree: false,
            worktree_branch: None,
            fork_from_session_id: None,
            fork_point_turn_id: None,
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
            last_session_id: None,
            continuation_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"RequestLaunch""#));
        let parsed: LauncherToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            LauncherToServer::RequestLaunch {
                working_directory,
                session_name,
                claude_args,
                continuation_id,
                ..
            } => {
                assert_eq!(working_directory, "/home/user/project");
                assert_eq!(session_name.as_deref(), Some("my-project"));
                assert_eq!(claude_args, vec!["--verbose"]);
                assert_eq!(continuation_id, None);
            }
            _ => panic!("Wrong variant"),
        }
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
                fields: ScheduledTaskFields {
                    name: "nightly audit".into(),
                    cron_expression: "0 3 * * *".into(),
                    timezone: "UTC".into(),
                    working_directory: "/home/user/project".into(),
                    prompt: "Check deps".into(),
                    claude_args: vec![],
                    agent_type: AgentType::Claude,
                    max_runtime_minutes: 30,
                    session_mode: SessionMode::Fresh,
                },
                enabled: true,
                last_session_id: None,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ScheduleSync""#));
        let parsed: ServerToLauncher = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToLauncher::ScheduleSync { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].fields.name, "nightly audit");
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pins the ScheduledTaskConfig wire shape: flattened fields must produce
    /// the same keys/values as the pre-flatten struct, and JSON in the old
    /// field order (as emitted by older backends) must still deserialize.
    #[test]
    fn scheduled_task_config_wire_compat() {
        let config = ScheduledTaskConfig {
            id: Uuid::nil(),
            fields: ScheduledTaskFields {
                name: "nightly audit".into(),
                cron_expression: "0 3 * * *".into(),
                timezone: "UTC".into(),
                working_directory: "/home/user/project".into(),
                prompt: "Check deps".into(),
                claude_args: vec!["--verbose".into()],
                agent_type: AgentType::Claude,
                max_runtime_minutes: 30,
                session_mode: SessionMode::Continue,
            },
            enabled: true,
            last_session_id: None,
        };
        let expected: serde_json::Value = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "nightly audit",
                "cron_expression": "0 3 * * *",
                "timezone": "UTC",
                "working_directory": "/home/user/project",
                "prompt": "Check deps",
                "claude_args": ["--verbose"],
                "agent_type": "claude",
                "enabled": true,
                "max_runtime_minutes": 30,
                "session_mode": "continue"
            }"#,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&config).unwrap(), expected);

        // Old wire order (enabled before max_runtime_minutes) still parses.
        let old_wire = r#"{
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "nightly audit",
            "cron_expression": "0 3 * * *",
            "timezone": "UTC",
            "working_directory": "/home/user/project",
            "prompt": "Check deps",
            "claude_args": ["--verbose"],
            "agent_type": "claude",
            "enabled": true,
            "max_runtime_minutes": 30,
            "last_session_id": "11111111-1111-1111-1111-111111111111"
        }"#;
        let parsed: ScheduledTaskConfig = serde_json::from_str(old_wire).unwrap();
        assert_eq!(parsed.fields.name, "nightly audit");
        assert_eq!(parsed.fields.max_runtime_minutes, 30);
        // Omitted session_mode defaults to Fresh (old-launcher wire compat).
        assert_eq!(parsed.fields.session_mode, SessionMode::Fresh);
        assert!(parsed.enabled);
        assert_eq!(
            parsed.last_session_id,
            Some(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap())
        );
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

mod client;
mod launcher;
mod session;
mod types;

pub use client::*;
pub use launcher::*;
pub use session::*;
pub use types::*;
pub use ws_bridge::WsEndpoint;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentType, SendMode, SessionMode};
    use uuid::Uuid;

    #[test]
    fn session_endpoint_path() {
        assert_eq!(SessionEndpoint::PATH, "/ws/session");
    }

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
            claude_args: Vec::new(),
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
            client_msg_id: None,
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
    fn portal_meta_round_trips_and_omits_empty_fields() {
        use crate::endpoints::client::{DeliveryMeta, MessageSource, PortalMeta};
        use crate::InputDeliveryStage;

        // Default (all-none) serializes to `{}` — no stray keys on the wire.
        assert_eq!(serde_json::to_string(&PortalMeta::default()).unwrap(), "{}");

        let meta = PortalMeta {
            created_at: Some("2026-06-26T12:00:00.000000".to_string()),
            source: Some(MessageSource::Agent {
                session_id: Uuid::nil(),
                agent_type: "codex".to_string(),
            }),
            delivery: Some(DeliveryMeta {
                client_msg_id: Uuid::nil(),
                stage: Some(InputDeliveryStage::ProxyReceived),
                message: None,
            }),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: PortalMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back, meta);
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
    fn client_to_server_claude_input_roundtrip() {
        let id = uuid::Uuid::from_u128(7);
        let msg = ClientToServer::AgentInput {
            content: serde_json::json!({"text": "hi"}),
            send_mode: None,
            client_msg_id: Some(id),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ClaudeInput""#));
        let parsed: ClientToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientToServer::AgentInput {
                send_mode,
                client_msg_id,
                ..
            } => {
                assert!(send_mode.is_none());
                assert_eq!(client_msg_id, Some(id));
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Older clients omit `client_msg_id`; the field must default to `None`.
    #[test]
    fn client_input_without_client_msg_id_defaults_none() {
        let json = r#"{"type":"ClaudeInput","content":{"text":"hi"}}"#;
        let parsed: ClientToServer = serde_json::from_str(json).unwrap();
        match parsed {
            ClientToServer::AgentInput { client_msg_id, .. } => {
                assert_eq!(client_msg_id, None);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn input_progress_roundtrips_with_stage() {
        use crate::InputDeliveryStage;
        let id = uuid::Uuid::from_u128(42);
        for (stage, tag) in [
            (InputDeliveryStage::ServerReceived, "server_received"),
            (InputDeliveryStage::ProxyReceived, "proxy_received"),
            (InputDeliveryStage::AgentAccepted, "agent_accepted"),
            (InputDeliveryStage::Failed, "failed"),
        ] {
            let msg = ServerToClient::InputProgress {
                client_msg_id: id,
                stage,
                message: None,
            };
            let json = serde_json::to_string(&msg).unwrap();
            assert!(json.contains(&format!(r#""stage":"{tag}""#)), "{json}");
            match serde_json::from_str::<ServerToClient>(&json).unwrap() {
                ServerToClient::InputProgress {
                    client_msg_id,
                    stage: parsed_stage,
                    ..
                } => {
                    assert_eq!(client_msg_id, id);
                    assert_eq!(parsed_stage, stage);
                }
                _ => panic!("Wrong variant"),
            }
        }
    }

    #[test]
    fn server_to_client_output_roundtrip() {
        let msg = ServerToClient::AgentOutput {
            content: serde_json::json!({"type": "assistant", "text": "hello"}),
            agent_type: AgentType::Codex,
            meta: Some(crate::PortalMeta {
                created_at: Some("2026-05-18T12:34:56.789012".to_string()),
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerToClient = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToClient::AgentOutput {
                content,
                agent_type,
                meta,
            } => {
                assert_eq!(content["text"], "hello");
                assert_eq!(agent_type, AgentType::Codex);
                assert_eq!(
                    meta.and_then(|m| m.created_at).as_deref(),
                    Some("2026-05-18T12:34:56.789012")
                );
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pre-#784 wire JSON for `ServerToClient::ClaudeOutput` (no `meta`) must
    /// still parse — `meta` defaults to `None` and the frontend keeps its prior
    /// reconnect watermark. Same backward-compat slack we extend for `agent_type`.
    #[test]
    fn wire_compat_pre_784_omits_meta() {
        let json = r#"{"type":"ClaudeOutput","content":{"hello":"world"}}"#;
        let parsed: ServerToClient = serde_json::from_str(json).unwrap();
        match parsed {
            ServerToClient::AgentOutput { meta, .. } => {
                assert!(meta.is_none());
            }
            _ => panic!("Wrong variant"),
        }

        // Empty HistoryBatch without `last_created_at` must also parse.
        let json = r#"{"type":"HistoryBatch"}"#;
        let parsed: ServerToClient = serde_json::from_str(json).unwrap();
        match parsed {
            ServerToClient::HistoryBatch {
                entries,
                last_created_at,
            } => {
                assert!(entries.is_empty());
                assert!(last_created_at.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// `HistoryBatch` carries the latest server-assigned timestamp in
    /// `last_created_at` so the frontend sets its reconnect watermark directly
    /// (closes #784). Each `entry` is raw content + its typed `meta` sidecar.
    #[test]
    fn history_batch_roundtrip_with_last_created_at() {
        let msg = ServerToClient::HistoryBatch {
            entries: vec![crate::endpoints::client::HistoryEntry {
                content: serde_json::json!({"type": "assistant", "text": "hi"}),
                meta: None,
            }],
            last_created_at: Some("2026-05-18T00:00:00.000000".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerToClient = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToClient::HistoryBatch {
                entries,
                last_created_at,
            } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].content["text"], "hi");
                assert_eq!(
                    last_created_at.as_deref(),
                    Some("2026-05-18T00:00:00.000000")
                );
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pre-2.5.42 wire JSON for `SequencedOutput` (no `agent_type`) must still
    /// parse, defaulting to `AgentType::Claude`. Same for `ClaudeOutput` and
    /// the backend → frontend `ServerToClient::ClaudeOutput` shape.
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

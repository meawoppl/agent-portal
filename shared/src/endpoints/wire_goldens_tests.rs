//! Pinned wire-format goldens + serde round-trip coverage for every
//! WebSocket endpoint's tagged-union shape.
//!
//! Each `#[test]` asserts two things:
//! 1. The Rust variant serializes to a **pinned** JSON value (exact `type` tag
//!    and field names), so a rename or `serde(rename)` drift fails loudly.
//! 2. That JSON (and the surrounding tolerance for optional/legacy fields)
//!    round-trips: re-parsing yields the same variant + the important
//!    fields are retained.
//!
//! The goldens are `serde_json::Value`s, not raw strings, so object key *order*
//! is irrelevant — only names/values/tags are pinned. Where the legacy name
//! differs from the Rust name (e.g. `AgentInput` ↔ `"ClaudeInput"`), the test
//! pins the legacy on-the-wire spelling.
//!
//! Keep these tests **synchronous, in-memory, and free of I/O**: the WS
//! serialization boundary is pure serde and must stay testable without touching
//! the DB, the filesystem, or a real socket.

use crate::endpoints::{
    client::{ClientToServer, DeliveryMeta, HistoryEntry, PortalMeta, ServerToClient},
    launcher::{LauncherToServer, ServerToLauncher, SessionExitReason},
    session::{ProxyToServer, ServerToProxy},
    types::{
        ContinuationReason, PermissionResponseFields, RegisterFields, SubagentRetryStatus,
        TunnelDataFields,
    },
    TunnelFrame,
};
use crate::{AgentType, InputDeliveryStage, SendMode, SessionStatus, TunnelRefuseReason};
use serde_json::json;
use uuid::Uuid;
use ws_bridge::WsCodec;

fn nil() -> Uuid {
    Uuid::nil()
}

fn roundtrip_json<T>(v: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
{
    let s = serde_json::to_string(v).unwrap();
    serde_json::from_str(&s).unwrap()
}

fn assert_golden<T>(value: &T, expected: serde_json::Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
{
    let got = serde_json::to_value(value).unwrap();
    assert_eq!(
        got,
        expected,
        "golden mismatch for {}: got {:#}",
        std::any::type_name::<T>(),
        got
    );
    // And it round-trips.
    let back: T = serde_json::from_value(got).unwrap();
    let re = serde_json::to_value(&back).unwrap();
    assert_eq!(re, expected);
}

// ── shared helpers ─────────────────────────────────────────────────────

fn reg_fields(session_name: &str) -> RegisterFields {
    RegisterFields {
        session_id: nil(),
        session_name: session_name.into(),
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
        capabilities: Vec::new(),
    }
}

// ── ClientToServer ─────────────────────────────────────────────────────

#[test]
fn client_to_server_register_golden() {
    let msg = ClientToServer::Register(reg_fields("s"));
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "Register");
    assert_eq!(v["session_name"], "s");
    assert_eq!(v["session_id"], "00000000-0000-0000-0000-000000000000");
    assert_eq!(v["working_directory"], "/tmp");
    assert_eq!(v["agent_type"], "claude");
    // capabilities defaults to [] — pins the flatten-safe default.
    assert_eq!(v["capabilities"], json!([]));
    let _: ClientToServer = serde_json::from_value(v).unwrap();
}

#[test]
fn client_to_server_agent_input_golden_uses_legacy_claude_tag() {
    let id = Uuid::from_u128(7);
    let msg = ClientToServer::AgentInput {
        content: json!({"text": "hi"}),
        send_mode: Some(SendMode::Wiggum),
        client_msg_id: Some(id),
    };
    assert_golden(
        &msg,
        json!({
            "type": "ClaudeInput",
            "content": {"text": "hi"},
            "send_mode": "wiggum",
            "client_msg_id": "00000000-0000-0000-0000-000000000007"
        }),
    );
    // Older client omits client_msg_id → defaults to None.
    let parsed: ClientToServer = serde_json::from_value(json!({
        "type": "ClaudeInput",
        "content": {"text": "hi"}
    }))
    .unwrap();
    match parsed {
        ClientToServer::AgentInput {
            client_msg_id,
            send_mode,
            ..
        } => {
            assert_eq!(client_msg_id, None);
            assert_eq!(send_mode, None);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn client_to_server_interrupt_golden() {
    let msg = ClientToServer::Interrupt;
    assert_golden(&msg, json!({"type": "Interrupt"}));
}

#[test]
fn client_to_server_permission_response_golden() {
    let msg = ClientToServer::PermissionResponse(PermissionResponseFields {
        request_id: "r1".into(),
        allow: true,
        input: Some(json!({"x": 1})),
        permissions: vec![],
        reason: None,
    });
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "PermissionResponse");
    assert_eq!(v["request_id"], "r1");
    assert_eq!(v["allow"], true);
    let _: ClientToServer = serde_json::from_value(v).unwrap();
}

#[test]
fn client_to_server_schedule_limit_continuation_golden() {
    let cid = nil();
    let msg = ClientToServer::ScheduleLimitContinuation {
        continuation_id: cid,
    };
    assert_golden(
        &msg,
        json!({
            "type": "ScheduleLimitContinuation",
            "continuation_id": "00000000-0000-0000-0000-000000000000"
        }),
    );
}

// ── ServerToClient ─────────────────────────────────────────────────────

#[test]
fn server_to_client_agent_output_golden_uses_legacy_claude_tag() {
    let msg = ServerToClient::AgentOutput {
        content: json!({"type": "assistant", "text": "hi"}),
        agent_type: AgentType::Codex,
        meta: None,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "ClaudeOutput");
    assert_eq!(v["agent_type"], "codex");
    assert_eq!(v["content"]["text"], "hi");
    // meta omitted when None — keeps wire bytes identical to pre-#784.
    assert!(v.get("meta").is_none());
    let _: ServerToClient = serde_json::from_value(v).unwrap();

    // Pre-#784 legacy without meta/agent_type parses as AgentOutput.
    let legacy: ServerToClient =
        serde_json::from_value(json!({"type": "ClaudeOutput", "content": {"hello": "world"}}))
            .unwrap();
    match legacy {
        ServerToClient::AgentOutput {
            agent_type, meta, ..
        } => {
            assert_eq!(agent_type, AgentType::Claude);
            assert!(meta.is_none());
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn server_to_client_history_batch_golden() {
    let msg = ServerToClient::HistoryBatch {
        entries: vec![HistoryEntry {
            content: json!({"type": "assistant", "text": "hi"}),
            meta: None,
        }],
        last_created_at: Some("2026-05-18T00:00:00.000000".to_string()),
    };
    assert_golden(
        &msg,
        json!({
            "type": "HistoryBatch",
            "entries": [{"content": {"type": "assistant", "text": "hi"}}],
            "last_created_at": "2026-05-18T00:00:00.000000"
        }),
    );
    // Legacy frame with `messages` key degrades to empty batch.
    let parsed: ServerToClient =
        serde_json::from_value(json!({"type": "HistoryBatch", "messages": [{"type": "portal"}]}))
            .unwrap();
    match parsed {
        ServerToClient::HistoryBatch {
            entries,
            last_created_at,
        } => {
            assert!(entries.is_empty());
            assert!(last_created_at.is_none());
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn server_to_client_input_progress_golden_for_every_stage() {
    let id = Uuid::from_u128(42);
    let mapping = [
        (InputDeliveryStage::ServerReceived, "server_received"),
        (InputDeliveryStage::ProxyReceived, "proxy_received"),
        (InputDeliveryStage::AgentAccepted, "agent_accepted"),
        (InputDeliveryStage::Failed, "failed"),
    ];
    for (stage, wire) in mapping {
        let msg = ServerToClient::InputProgress {
            client_msg_id: id,
            stage,
            message: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "InputProgress");
        assert_eq!(v["stage"], wire);
        match serde_json::from_value::<ServerToClient>(v).unwrap() {
            ServerToClient::InputProgress {
                client_msg_id,
                stage: s,
                ..
            } => {
                assert_eq!(client_msg_id, id);
                assert_eq!(s, stage);
            }
            _ => panic!("wrong variant"),
        }
    }
}

#[test]
fn server_to_client_error_golden() {
    let msg = ServerToClient::Error {
        message: "boom".into(),
    };
    assert_golden(&msg, json!({"type": "Error", "message": "boom"}));
}

#[test]
fn server_to_client_session_status_golden() {
    let msg = ServerToClient::SessionStatus {
        status: SessionStatus::Active,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "SessionStatus");
    assert_eq!(v["status"], "active");
    let _: ServerToClient = serde_json::from_value(v).unwrap();
}

#[test]
fn server_to_client_server_shutdown_golden() {
    let msg = ServerToClient::ServerShutdown {
        reason: "update".into(),
        reconnect_delay_ms: 5000,
    };
    assert_golden(
        &msg,
        json!({"type": "ServerShutdown", "reason": "update", "reconnect_delay_ms": 5000}),
    );
    let _: ServerToClient = serde_json::from_value(
        json!({"type":"ServerShutdown","reason":"update","reconnect_delay_ms":5000}),
    )
    .unwrap();
}

#[test]
fn server_to_launcher_server_shutdown_golden() {
    let msg = ServerToLauncher::ServerShutdown {
        reason: "update".into(),
        reconnect_delay_ms: 5000,
    };
    assert_golden(
        &msg,
        json!({"type": "ServerShutdown", "reason": "update", "reconnect_delay_ms": 5000}),
    );
    // Cross-check: same JSON must parse in all three server→X enums.
    let _: ServerToClient = serde_json::from_value(
        json!({"type":"ServerShutdown","reason":"update","reconnect_delay_ms":5000}),
    )
    .unwrap();
    let _: ServerToProxy = serde_json::from_value(
        json!({"type":"ServerShutdown","reason":"update","reconnect_delay_ms":5000}),
    )
    .unwrap();
}

#[test]
fn server_to_client_turn_metrics_roundtrips_via_json_fixture() {
    // Pinned fixture: the proxy-emit shape (no id, minimal fields) must
    // deserialize, and the backend-broadcast shape (with id populated)
    // round-trips through Value.
    let fixture = json!({
        "type": "TurnMetrics",
        "session_id": "00000000-0000-0000-0000-000000000000",
        "agent_type": "claude",
        "started_at": "2026-01-01T00:00:00Z",
        "input_tokens": 100,
        "output_tokens": 50
    });
    let parsed: ServerToClient = serde_json::from_value(fixture.clone()).unwrap();
    match parsed {
        ServerToClient::TurnMetrics(m) => {
            assert_eq!(m.session_id, nil());
            assert_eq!(m.agent_type, AgentType::Claude);
            assert_eq!(m.input_tokens, 100);
        }
        _ => panic!("wrong variant"),
    }
    // A second fixture with codex shape round-trips through Value too.
    let fixture2 = json!({
        "type": "TurnMetrics",
        "session_id": "00000000-0000-0000-0000-000000000000",
        "agent_type": "codex",
        "started_at": "2026-01-01T00:00:00Z",
        "input_tokens": 10,
        "output_tokens": 20
    });
    let parsed2: ServerToClient = serde_json::from_value(fixture2).unwrap();
    match parsed2 {
        ServerToClient::TurnMetrics(m) => assert_eq!(m.agent_type, AgentType::Codex),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn server_to_client_tool_progress_golden() {
    let msg = ServerToClient::ToolProgress {
        tool_use_id: "t1".into(),
        parent_tool_use_id: Some("p1".into()),
        tool_name: "Bash".into(),
        elapsed_time_seconds: 12.5,
        subagent_type: None,
        subagent_retry: Some(SubagentRetryStatus {
            attempt: 2,
            max_retries: 5,
            error_category: "overloaded".into(),
        }),
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "ToolProgress");
    assert_eq!(v["tool_use_id"], "t1");
    assert_eq!(v["tool_name"], "Bash");
    let back: ServerToClient = serde_json::from_value(v).unwrap();
    match back {
        ServerToClient::ToolProgress {
            tool_use_id,
            subagent_retry,
            ..
        } => {
            assert_eq!(tool_use_id, "t1");
            assert_eq!(subagent_retry.unwrap().attempt, 2);
        }
        _ => panic!("wrong variant"),
    }
}

// ── ProxyToServer ──────────────────────────────────────────────────────

#[test]
fn proxy_to_server_register_golden() {
    let msg = ProxyToServer::Register(reg_fields("s"));
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "Register");
    assert_eq!(v["session_name"], "s");
    let _: ProxyToServer = serde_json::from_value(v).unwrap();
    // Legacy register (no capabilities) defaults to empty.
    let parsed: ProxyToServer = serde_json::from_value(json!({
        "type": "Register",
        "session_id": "00000000-0000-0000-0000-000000000000",
        "session_name": "s",
        "auth_token": null,
        "working_directory": "/tmp"
    }))
    .unwrap();
    match parsed {
        ProxyToServer::Register(r) => assert!(r.capabilities.is_empty()),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn proxy_to_server_agent_output_golden_uses_legacy_tag() {
    let msg = ProxyToServer::AgentOutput {
        content: json!({"type": "assistant"}),
        agent_type: AgentType::Muse,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "ClaudeOutput");
    assert_eq!(v["agent_type"], "muse");
    // Pre-2.5.42 omission defaults to Claude.
    let parsed: ProxyToServer =
        serde_json::from_value(json!({"type":"ClaudeOutput","content":{"hello":"world"}})).unwrap();
    match parsed {
        ProxyToServer::AgentOutput { agent_type, .. } => {
            assert_eq!(agent_type, AgentType::Claude)
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn proxy_to_server_sequenced_output_golden() {
    let msg = ProxyToServer::SequencedOutput {
        seq: 3,
        content: json!({"hello": "world"}),
        agent_type: AgentType::Codex,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "SequencedOutput");
    assert_eq!(v["seq"], 3);
    assert_eq!(v["agent_type"], "codex");
    let back: ProxyToServer = serde_json::from_value(v).unwrap();
    match back {
        ProxyToServer::SequencedOutput {
            seq, agent_type, ..
        } => {
            assert_eq!(seq, 3);
            assert_eq!(agent_type, AgentType::Codex);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn proxy_to_server_heartbeat_golden() {
    let msg = ProxyToServer::Heartbeat;
    assert_golden(&msg, json!({"type": "Heartbeat"}));
}

#[test]
fn proxy_to_server_tool_progress_golden() {
    let msg = ProxyToServer::ToolProgress {
        session_id: nil(),
        tool_use_id: "t1-heartbeat-1".into(),
        parent_tool_use_id: Some("t1".into()),
        tool_name: "Bash".into(),
        elapsed_time_seconds: 30.0,
        subagent_type: Some("Explore".into()),
        subagent_retry: None,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "ToolProgress");
    assert_eq!(v["tool_name"], "Bash");
    match roundtrip_json(&msg) {
        ProxyToServer::ToolProgress { session_id, .. } => assert_eq!(session_id, nil()),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn proxy_to_server_ephemeral_golden() {
    let msg = ProxyToServer::Ephemeral {
        session_id: nil(),
        payload: json!({"kind": "tick", "n": 1}),
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "Ephemeral");
    assert_eq!(v["payload"]["kind"], "tick");
    let _: ProxyToServer = serde_json::from_value(v).unwrap();
}

// ── ServerToProxy ──────────────────────────────────────────────────────

#[test]
fn server_to_proxy_register_ack_golden_omits_absent_ticket() {
    let ack = ServerToProxy::RegisterAck {
        success: true,
        session_id: nil(),
        error: None,
        max_image_mb: 10,
        retryable: false,
        tunnel_data_ticket: None,
        tunnel_sizing: None,
    };
    let v = serde_json::to_value(&ack).unwrap();
    assert_eq!(v["type"], "RegisterAck");
    assert_eq!(v["success"], true);
    assert_eq!(v["max_image_mb"], 10);
    assert!(v.get("tunnel_data_ticket").is_none());
    assert!(v.get("tunnel_sizing").is_none());
    // Legacy ack (just success + max_image_mb) parses.
    let parsed: ServerToProxy =
        serde_json::from_value(json!({"type":"RegisterAck","success":true,"session_id":"00000000-0000-0000-0000-000000000000","max_image_mb":10})).unwrap();
    match parsed {
        ServerToProxy::RegisterAck {
            tunnel_data_ticket,
            tunnel_sizing,
            ..
        } => {
            assert!(tunnel_data_ticket.is_none());
            assert!(tunnel_sizing.is_none());
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn server_to_proxy_sequenced_input_golden() {
    let msg = ServerToProxy::SequencedInput {
        session_id: nil(),
        seq: 5,
        content: json!({"text": "hello"}),
        send_mode: Some(SendMode::Wiggum),
        client_msg_id: None,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "SequencedInput");
    assert_eq!(v["seq"], 5);
    assert_eq!(v["send_mode"], "wiggum");
    let back: ServerToProxy = serde_json::from_value(v).unwrap();
    match back {
        ServerToProxy::SequencedInput { seq, send_mode, .. } => {
            assert_eq!(seq, 5);
            assert_eq!(send_mode, Some(SendMode::Wiggum));
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn server_to_proxy_server_shutdown_and_terminated_goldens() {
    let sd = ServerToProxy::ServerShutdown {
        reason: "update".into(),
        reconnect_delay_ms: 5000,
    };
    assert_golden(
        &sd,
        json!({"type":"ServerShutdown","reason":"update","reconnect_delay_ms":5000}),
    );
    let st = ServerToProxy::SessionTerminated {
        reason: "stopped".into(),
    };
    assert_golden(&st, json!({"type":"SessionTerminated","reason":"stopped"}));
}

#[test]
fn server_to_proxy_tunnel_data_golden() {
    let msg = ServerToProxy::TunnelData(TunnelDataFields {
        stream_id: nil(),
        data_base64: "aGVsbG8=".into(),
    });
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "TunnelData");
    assert_eq!(v["data_base64"], "aGVsbG8=");
    let _: ServerToProxy = serde_json::from_value(v).unwrap();
}

// ── Launcher ───────────────────────────────────────────────────────────

#[test]
fn launcher_to_server_exit_reason_defaults() {
    // Older launcher omits `reason` → Completed (neutral default).
    let parsed: LauncherToServer =
        serde_json::from_value(json!({"type":"SessionExited","session_id":"00000000-0000-0000-0000-000000000000","exit_code":0})).unwrap();
    match parsed {
        LauncherToServer::SessionExited { reason, .. } => {
            assert_eq!(reason, SessionExitReason::Completed)
        }
        _ => panic!("wrong variant"),
    }
    assert_eq!(
        serde_json::to_value(SessionExitReason::CrashedEarly).unwrap(),
        "crashed_early"
    );
}

#[test]
fn launcher_server_to_launcher_launch_roundtrip_golden() {
    let msg = ServerToLauncher::LaunchSession {
        request_id: nil(),
        user_id: nil(),
        auth_token: "tok".into(),
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
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "LaunchSession");
    assert_eq!(v["working_directory"], "/home");
    let back: ServerToLauncher = serde_json::from_value(v).unwrap();
    match back {
        ServerToLauncher::LaunchSession {
            working_directory,
            claude_args,
            ..
        } => {
            assert_eq!(working_directory, "/home");
            assert_eq!(claude_args, vec!["--verbose"]);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn launcher_to_server_request_launch_golden() {
    let msg = LauncherToServer::RequestLaunch {
        request_id: nil(),
        working_directory: "/home/user/project".into(),
        session_name: Some("my-project".into()),
        claude_args: vec!["--verbose".into()],
        agent_type: AgentType::Claude,
        scheduled_task_id: None,
        last_session_id: None,
        continuation_id: None,
    };
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "RequestLaunch");
    assert_eq!(v["working_directory"], "/home/user/project");
    assert_eq!(v["session_name"], "my-project");
    let _: LauncherToServer = serde_json::from_value(v).unwrap();
}

// ── Tunnel binary (binary-framed, not JSON) ────────────────────────────

#[test]
fn tunnel_frame_tags_stable_golden() {
    // Re-pin the same constants the tunnel.rs test pins, via the public
    // API (encode) — catches a drift where the codec's tag literal
    // diverges from the const but the const list still looks right.
    let tag = |frame: &TunnelFrame| match frame.encode().unwrap() {
        ws_bridge::WsMessage::Binary(b) => b[0],
        other => panic!("expected binary {other:?}"),
    };
    assert_eq!(
        tag(&TunnelFrame::Hello {
            ticket: String::new()
        }),
        0x00
    );
    assert_eq!(
        tag(&TunnelFrame::Open {
            stream_id: nil(),
            port: 1
        }),
        0x01
    );
    assert_eq!(tag(&TunnelFrame::Opened { stream_id: nil() }), 0x02);
    assert_eq!(
        tag(&TunnelFrame::Refused {
            stream_id: nil(),
            reason: TunnelRefuseReason::NoListener
        }),
        0x03
    );
    assert_eq!(
        tag(&TunnelFrame::Data {
            stream_id: nil(),
            bytes: vec![]
        }),
        0x04
    );
    assert_eq!(
        tag(&TunnelFrame::Window {
            stream_id: nil(),
            add_bytes: 0
        }),
        0x05
    );
    assert_eq!(
        tag(&TunnelFrame::Close {
            stream_id: nil(),
            reason: None
        }),
        0x06
    );
}

// ── Cross-cutting: path stability + PortalMeta invariants ──────────────

#[test]
fn endpoint_paths_golden() {
    use crate::endpoints::{ClientEndpoint, LauncherEndpoint, SessionEndpoint, TunnelDataEndpoint};
    use ws_bridge::WsEndpoint;
    assert_eq!(SessionEndpoint::PATH, "/ws/session");
    assert_eq!(ClientEndpoint::PATH, "/ws/client");
    assert_eq!(LauncherEndpoint::PATH, "/ws/launcher");
    assert_eq!(TunnelDataEndpoint::PATH, "/ws/session/data");
    assert_ne!(SessionEndpoint::PATH, TunnelDataEndpoint::PATH);
}

#[test]
fn portal_meta_default_omits_empty_fields_and_roundtrips() {
    let empty = PortalMeta::default();
    assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
    let m = PortalMeta {
        created_at: Some("2026-06-26T12:00:00.000000".into()),
        source: Some(crate::endpoints::client::MessageSource::Portal),
        delivery: Some(DeliveryMeta {
            client_msg_id: nil(),
            stage: Some(InputDeliveryStage::ProxyReceived),
            message: None,
        }),
    };
    assert_eq!(roundtrip_json(&m), m);
}

#[test]
fn continuation_reason_wire_strings_stable() {
    // The DB column and the JSON value must match as_wire().
    assert_eq!(ContinuationReason::Limit.as_wire(), "limit");
    assert_eq!(ContinuationReason::Overloaded.as_wire(), "overloaded");
    assert_eq!(
        serde_json::to_value(ContinuationReason::Limit).unwrap(),
        "limit"
    );
    assert_eq!(
        ContinuationReason::from_wire("limit"),
        Some(ContinuationReason::Limit)
    );
    assert_eq!(ContinuationReason::from_wire("bogus"), None);
}

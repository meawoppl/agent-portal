//! Cross-endpoint protocol contracts, using real sockets and persistence.
use super::*;
use shared::endpoints::ClientEndpoint;
use shared::{ClientToServer, InputDeliveryStage, PermissionResponseFields, ServerToClient};
use ws_bridge::{
    native_client::{connect, Connection},
    WsEndpoint,
};

async fn receive<E: WsEndpoint>(
    socket: &mut Connection<E>,
    predicate: impl Fn(&E::ServerMsg) -> bool,
) -> E::ServerMsg {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = socket
                .recv()
                .await
                .expect("socket closed before expected frame")
                .expect("valid protocol frame");
            if predicate(&frame) {
                return frame;
            }
        }
    })
    .await
    .expect("expected frame within timeout")
}

#[tokio::test]
async fn turn_permission_delivery_and_reconnect_replay() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let url = format!("ws://{}", spawn_test_app().await);
    let fields = test_register_fields();
    let session_id = fields.session_id;
    let mut proxy = connect::<SessionEndpoint>(&url).await.unwrap();
    proxy
        .send(ProxyToServer::Register(fields.clone()))
        .await
        .unwrap();
    receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::RegisterAck { success: true, .. })
    })
    .await;
    let mut browser = connect::<ClientEndpoint>(&url).await.unwrap();
    browser
        .send(ClientToServer::Register(fields.clone()))
        .await
        .unwrap();

    let client_msg_id = uuid::Uuid::new_v4();
    let prompt = serde_json::json!({"type":"user", "message":{"role":"user","content":"inspect the fixture"}});
    browser
        .send(ClientToServer::AgentInput {
            content: prompt.clone(),
            send_mode: None,
            reasoning_effort: None,
            client_msg_id: Some(client_msg_id),
        })
        .await
        .unwrap();
    receive::<ClientEndpoint>(&mut browser, |f| matches!(f, ServerToClient::InputProgress {client_msg_id:id, stage:InputDeliveryStage::ServerReceived, ..} if *id == client_msg_id)).await;
    let input = receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::SequencedInput { .. })
    })
    .await;
    let ServerToProxy::SequencedInput {
        seq,
        content,
        client_msg_id: delivered_id,
        ..
    } = input
    else {
        unreachable!()
    };
    assert_eq!(content, prompt);
    assert_eq!(delivered_id, Some(client_msg_id));
    proxy
        .send(ProxyToServer::InputAck {
            session_id,
            ack_seq: seq,
        })
        .await
        .unwrap();
    for stage in [
        InputDeliveryStage::ProxyReceived,
        InputDeliveryStage::AgentAccepted,
    ] {
        proxy
            .send(ProxyToServer::InputProgressAck {
                session_id,
                client_msg_id,
                stage,
            })
            .await
            .unwrap();
        receive::<ClientEndpoint>(&mut browser, |f| matches!(f, ServerToClient::InputProgress {client_msg_id:id, stage:actual, ..} if *id == client_msg_id && *actual == stage)).await;
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    let tool_input = serde_json::json!({"command":"pwd"});
    proxy
        .send(ProxyToServer::PermissionRequest {
            request_id: request_id.clone(),
            tool_name: "Bash".into(),
            input: tool_input.clone(),
            permission_suggestions: vec![],
        })
        .await
        .unwrap();
    let permission = receive::<ClientEndpoint>(
        &mut browser,
        |f| matches!(f, ServerToClient::PermissionRequest {request_id:id, ..} if id == &request_id),
    )
    .await;
    assert!(
        matches!(permission, ServerToClient::PermissionRequest {input, tool_name, ..} if input == tool_input && tool_name == "Bash")
    );
    browser
        .send(ClientToServer::PermissionResponse(
            PermissionResponseFields {
                request_id: request_id.clone(),
                allow: true,
                input: Some(tool_input.clone()),
                permissions: vec![],
                reason: None,
            },
        ))
        .await
        .unwrap();
    let decision = receive::<SessionEndpoint>(
        &mut proxy,
        |f| matches!(f, ServerToProxy::PermissionResponse(p) if p.request_id == request_id),
    )
    .await;
    assert!(
        matches!(decision, ServerToProxy::PermissionResponse(p) if p.allow && p.input == Some(tool_input))
    );

    let answer = serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"fixture inspected"}]}});
    proxy
        .send(ProxyToServer::SequencedOutput {
            seq: 1,
            content: answer.clone(),
            agent_type: AgentType::Claude,
        })
        .await
        .unwrap();
    let output = receive::<ClientEndpoint>(
        &mut browser,
        |f| matches!(f, ServerToClient::AgentOutput {content, ..} if content == &answer),
    )
    .await;
    let ServerToClient::AgentOutput {
        meta: Some(meta), ..
    } = output
    else {
        panic!("live output requires replay watermark")
    };
    let watermark = meta.created_at.expect("persisted timestamp");
    receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::OutputAck { ack_seq: 1, .. })
    })
    .await;
    browser.close().await.unwrap();

    let result = serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":"done","session_id":session_id.to_string(),"duration_ms":1,"duration_api_ms":1,"num_turns":1,"total_cost_usd":0.0});
    // The browser is offline; ack proves persistence before reconnect, without sleeps.
    proxy
        .send(ProxyToServer::SequencedOutput {
            seq: 2,
            content: result.clone(),
            agent_type: AgentType::Claude,
        })
        .await
        .unwrap();
    receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::OutputAck { ack_seq: 2, .. })
    })
    .await;
    let mut resumed = fields;
    resumed.resuming = true;
    resumed.replay_after = Some(watermark);
    let mut browser = connect::<ClientEndpoint>(&url).await.unwrap();
    browser
        .send(ClientToServer::Register(resumed))
        .await
        .unwrap();
    let replay = receive::<ClientEndpoint>(&mut browser, |f| {
        matches!(f, ServerToClient::HistoryBatch { .. })
    })
    .await;
    let ServerToClient::HistoryBatch {
        entries,
        last_created_at,
    } = replay
    else {
        unreachable!()
    };
    assert_eq!(entries.iter().filter(|e| e.content == result).count(), 1);
    assert!(
        !entries.iter().any(|e| e.content == answer),
        "watermark excludes already seen output"
    );
    assert!(last_created_at.is_some());
}

struct LegacySession;
impl WsEndpoint for LegacySession {
    const PATH: &'static str = "/ws/session";
    type ClientMsg = serde_json::Value;
    type ServerMsg = ServerToProxy;
}

#[tokio::test]
async fn legacy_register_without_capabilities_keeps_control_socket_fallback() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let url = format!("ws://{}", spawn_test_app().await);
    let mut socket = connect::<LegacySession>(&url).await.unwrap();
    // Frozen minimal older wire shape: do not serialize modern RegisterFields.
    socket.send(serde_json::json!({"type":"Register","session_id":uuid::Uuid::new_v4(),"session_name":"legacy-harness","auth_token":null,"working_directory":"/tmp/harness"})).await.unwrap();
    let ack = receive::<LegacySession>(&mut socket, |f| {
        matches!(f, ServerToProxy::RegisterAck { .. })
    })
    .await;
    assert!(matches!(
        ack,
        ServerToProxy::RegisterAck {
            success: true,
            tunnel_data_ticket: None,
            tunnel_sizing: None,
            ..
        }
    ));
}

#[tokio::test]
async fn unacknowledged_input_replays_when_proxy_reconnects() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let url = format!("ws://{}", spawn_test_app().await);
    let mut fields = test_register_fields();
    let mut proxy = connect::<SessionEndpoint>(&url).await.unwrap();
    proxy
        .send(ProxyToServer::Register(fields.clone()))
        .await
        .unwrap();
    receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::RegisterAck { success: true, .. })
    })
    .await;
    let mut browser = connect::<ClientEndpoint>(&url).await.unwrap();
    browser
        .send(ClientToServer::Register(fields.clone()))
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4();
    browser
        .send(ClientToServer::AgentInput {
            content: serde_json::json!("resume this input"),
            send_mode: None,
            reasoning_effort: None,
            client_msg_id: Some(id),
        })
        .await
        .unwrap();
    let first = receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::SequencedInput { .. })
    })
    .await;
    let ServerToProxy::SequencedInput { seq, content, .. } = first else {
        unreachable!()
    };
    // Disconnect before InputAck: the durable queue must keep the same identity.
    proxy.close().await.unwrap();
    fields.resuming = true;
    let mut proxy = connect::<SessionEndpoint>(&url).await.unwrap();
    proxy
        .send(ProxyToServer::Register(fields.clone()))
        .await
        .unwrap();
    receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::RegisterAck { success: true, .. })
    })
    .await;
    let replay = receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::SequencedInput { .. })
    })
    .await;
    assert!(
        matches!(replay, ServerToProxy::SequencedInput {seq:actual, content:body, client_msg_id:Some(actual_id), ..} if actual == seq && body == content && actual_id == id)
    );
    proxy
        .send(ProxyToServer::InputAck {
            session_id: fields.session_id,
            ack_seq: seq,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn incompatible_proxy_is_rejected_before_session_creation() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    use diesel::prelude::*;
    let url = format!("ws://{}", spawn_test_app().await);
    for version in [0, shared::protocol::PROTOCOL_VERSION + 1, u32::MAX] {
        let mut fields = test_register_fields();
        fields.protocol_version = Some(version);
        let session_id = fields.session_id;
        let mut socket = connect::<SessionEndpoint>(&url).await.unwrap();
        socket.send(ProxyToServer::Register(fields)).await.unwrap();
        let ack = receive::<SessionEndpoint>(&mut socket, |f| {
            matches!(f, ServerToProxy::RegisterAck { .. })
        })
        .await;
        let ServerToProxy::RegisterAck {
            success,
            retryable,
            error,
            protocol_version,
            ..
        } = ack
        else {
            unreachable!()
        };
        assert!(!success);
        assert!(!retryable);
        assert_eq!(protocol_version, Some(shared::protocol::PROTOCOL_VERSION));
        assert!(error.unwrap().contains("Update"));
        let count: i64 = backend::schema::sessions::table
            .find(session_id)
            .count()
            .get_result(&mut test_pool().get().unwrap())
            .unwrap();
        assert_eq!(count, 0, "rejected peers must not create sessions");
    }
}

#[tokio::test]
async fn incompatible_launcher_gets_fatal_actionable_ack() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let url = format!("ws://{}", spawn_test_app().await);
    let mut socket = connect::<shared::LauncherEndpoint>(&url).await.unwrap();
    socket
        .send(shared::LauncherToServer::LauncherRegister {
            protocol_version: Some(shared::protocol::PROTOCOL_VERSION + 1),
            launcher_id: uuid::Uuid::new_v4(),
            launcher_name: "incompatible".into(),
            auth_token: None,
            hostname: "test".into(),
            working_directory: Some("/tmp".into()),
            version: Some("test".into()),
            capabilities: Vec::new(),
        })
        .await
        .unwrap();
    let ack = receive::<shared::LauncherEndpoint>(&mut socket, |f| {
        matches!(f, shared::ServerToLauncher::LauncherRegisterAck { .. })
    })
    .await;
    let shared::ServerToLauncher::LauncherRegisterAck {
        success,
        fatal,
        error,
        reject_reason,
        ..
    } = ack
    else {
        unreachable!()
    };
    assert!(!success);
    assert!(fatal);
    assert!(error.unwrap().contains("Update"));
    assert_eq!(
        reject_reason, None,
        "legacy launchers must decode rejection"
    );
}

#[tokio::test]
async fn incompatible_browser_cannot_register_or_send_pipelined_input() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    use diesel::prelude::*;
    let url = format!("ws://{}", spawn_test_app().await);
    let fields = test_register_fields();
    let session_id = fields.session_id;
    let mut proxy = connect::<SessionEndpoint>(&url).await.unwrap();
    proxy
        .send(ProxyToServer::Register(fields.clone()))
        .await
        .unwrap();
    receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::RegisterAck { success: true, .. })
    })
    .await;
    let mut browser = connect::<ClientEndpoint>(&url).await.unwrap();
    browser
        .send(ClientToServer::Hello {
            protocol_version: u32::MAX,
        })
        .await
        .unwrap();
    // Race the server's rejection deliberately. Neither command may dispatch.
    let _ = browser
        .send(ClientToServer::Dashboard {
            request_id: uuid::Uuid::new_v4(),
            request: shared::api::DashboardRequest::RestartLauncher {
                launcher_id: uuid::Uuid::new_v4(),
            },
        })
        .await;
    let _ = browser.send(ClientToServer::Register(fields)).await;
    let _ = browser.send(ClientToServer::AgentInput {
        content: serde_json::json!({"type":"user","message":{"role":"user","content":"must not run"}}),
        send_mode: None, reasoning_effort: None, client_msg_id: Some(uuid::Uuid::new_v4()),
    }).await;
    let ack = receive::<ClientEndpoint>(&mut browser, |f| {
        matches!(f, ServerToClient::HelloAck { .. })
    })
    .await;
    assert!(matches!(
        ack,
        ServerToClient::HelloAck {
            success: false,
            error: Some(_),
            ..
        }
    ));
    // Closure proves all buffered inbound frames have been discarded, avoiding
    // a sleep-based assertion racing a still-running server command handler.
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = browser.recv().await {
            if frame.is_err() {
                break;
            }
        }
    })
    .await
    .unwrap();
    let input_seq: i64 = backend::schema::sessions::table
        .find(session_id)
        .select(backend::schema::sessions::input_seq)
        .first(&mut test_pool().get().unwrap())
        .unwrap();
    assert_eq!(input_seq, 0);
}

#[tokio::test]
async fn browser_hello_accepts_current_and_then_registration() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let url = format!("ws://{}", spawn_test_app().await);
    let fields = test_register_fields();
    let mut proxy = connect::<SessionEndpoint>(&url).await.unwrap();
    proxy
        .send(ProxyToServer::Register(fields.clone()))
        .await
        .unwrap();
    receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(f, ServerToProxy::RegisterAck { success: true, .. })
    })
    .await;
    let mut browser = connect::<ClientEndpoint>(&url).await.unwrap();
    browser
        .send(ClientToServer::Hello {
            protocol_version: shared::protocol::PROTOCOL_VERSION,
        })
        .await
        .unwrap();
    let ack = receive::<ClientEndpoint>(&mut browser, |f| {
        matches!(f, ServerToClient::HelloAck { .. })
    })
    .await;
    assert!(matches!(
        ack,
        ServerToClient::HelloAck {
            success: true,
            error: None,
            ..
        }
    ));
    browser
        .send(ClientToServer::Register(fields))
        .await
        .unwrap();
    receive::<ClientEndpoint>(&mut browser, |f| {
        matches!(f, ServerToClient::AgentProgress { .. })
    })
    .await;
}

#[tokio::test]
async fn browser_register_skew_uses_legacy_error_frame() {
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let url = format!("ws://{}", spawn_test_app().await);
    let mut fields = test_register_fields();
    fields.protocol_version = Some(u32::MAX);
    let mut browser = connect::<ClientEndpoint>(&url).await.unwrap();
    browser
        .send(ClientToServer::Register(fields))
        .await
        .unwrap();
    let frame = receive::<ClientEndpoint>(&mut browser, |f| {
        matches!(
            f,
            ServerToClient::Error { .. } | ServerToClient::HelloAck { .. }
        )
    })
    .await;
    assert!(matches!(frame, ServerToClient::Error { message } if message.contains("Update")));
}

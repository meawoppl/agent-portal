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
                // Only a failure carries a reason; text on a success is dropped.
                message: Some("not shown".into()),
            })
            .await
            .unwrap();
        receive::<ClientEndpoint>(&mut browser, |f| matches!(f, ServerToClient::InputProgress {client_msg_id:id, stage:actual, message: None, ..} if *id == client_msg_id && *actual == stage)).await;
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

/// Input sent while the agent is offline: the sender is told it is queued
/// once it is durably stored, and when the agent registers, this scenario's
/// replay delivers it once and the normal acks settle it. Nothing is resent
/// by the browser or the server in between.
#[tokio::test]
async fn offline_input_is_queued_then_replayed_on_registration() {
    use diesel::prelude::*;
    if std::env::var("DATABASE_URL").is_err() {
        return;
    }
    let url = format!("ws://{}", spawn_test_app().await);
    let mut fields = test_register_fields();
    let session_id = fields.session_id;
    // A session whose agent has never connected, owned by the dev user.
    {
        let mut conn = test_pool().get().unwrap();
        let owner: uuid::Uuid = backend::schema::users::table
            .filter(backend::schema::users::email.eq(backend::auth::DEV_USER_EMAIL))
            .select(backend::schema::users::id)
            .first(&mut conn)
            .unwrap();
        diesel::insert_into(backend::schema::sessions::table)
            .values((
                backend::schema::sessions::id.eq(session_id),
                backend::schema::sessions::user_id.eq(owner),
                backend::schema::sessions::session_name.eq("queued harness"),
                backend::schema::sessions::session_key.eq(session_id.to_string()),
                backend::schema::sessions::working_directory.eq(&fields.working_directory),
                backend::schema::sessions::status.eq("disconnected"),
            ))
            .execute(&mut conn)
            .unwrap();
        diesel::insert_into(backend::schema::session_members::table)
            .values(backend::models::NewSessionMember {
                session_id,
                user_id: owner,
                role: "owner".into(),
            })
            .execute(&mut conn)
            .unwrap();
    }
    let mut browser = connect::<ClientEndpoint>(&url).await.unwrap();
    browser
        .send(ClientToServer::Register(fields.clone()))
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4();
    browser
        .send(ClientToServer::AgentInput {
            content: serde_json::json!("while you were away"),
            send_mode: None,
            reasoning_effort: None,
            client_msg_id: Some(id),
        })
        .await
        .unwrap();
    receive::<ClientEndpoint>(&mut browser, |f| matches!(f, ServerToClient::InputProgress {client_msg_id, stage: InputDeliveryStage::ServerReceived, queued: true, ..} if *client_msg_id == id)).await;

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
    let ServerToProxy::SequencedInput {
        seq,
        client_msg_id: Some(replayed),
        ..
    } = replay
    else {
        panic!("replay carries the input's identity")
    };
    assert_eq!(replayed, id);
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
                client_msg_id: id,
                stage,
                message: None,
            })
            .await
            .unwrap();
        receive::<ClientEndpoint>(&mut browser, |f| matches!(f, ServerToClient::InputProgress {client_msg_id, stage: actual, queued: false, ..} if *client_msg_id == id && *actual == stage)).await;
    }
    // In this scenario the input arrived once: a heartbeat round trip shows
    // nothing else was queued behind it.
    proxy.send(ProxyToServer::Heartbeat).await.unwrap();
    let next = receive::<SessionEndpoint>(&mut proxy, |f| {
        matches!(
            f,
            ServerToProxy::SequencedInput { .. } | ServerToProxy::Heartbeat
        )
    })
    .await;
    assert!(
        matches!(next, ServerToProxy::Heartbeat),
        "replayed twice: {next:?}"
    );
}

/// A proxy's failure reason reaches the browser on the matching input, an
/// absent reason stays absent (the browser falls back to "Delivery failed"),
/// and an ack naming another session is ignored.
#[tokio::test]
async fn failure_reason_reaches_the_matching_input() {
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
    let (with_reason, without_reason) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let failed =
        |session_id, client_msg_id, message: Option<&str>| ProxyToServer::InputProgressAck {
            session_id,
            client_msg_id,
            stage: InputDeliveryStage::Failed,
            message: message.map(str::to_string),
        };

    // Another session's ack is dropped: the next frame for this id is the
    // real one.
    proxy
        .send(failed(
            uuid::Uuid::new_v4(),
            with_reason,
            Some("wrong session"),
        ))
        .await
        .unwrap();
    proxy
        .send(failed(
            session_id,
            with_reason,
            Some("The agent rejected this input\n"),
        ))
        .await
        .unwrap();
    let frame = receive::<ClientEndpoint>(&mut browser, |f| matches!(f, ServerToClient::InputProgress {client_msg_id, ..} if *client_msg_id == with_reason)).await;
    assert!(
        matches!(&frame, ServerToClient::InputProgress {stage: InputDeliveryStage::Failed, message: Some(m), ..} if m == "The agent rejected this input"),
        "{frame:?}"
    );

    proxy
        .send(failed(session_id, without_reason, None))
        .await
        .unwrap();
    let frame = receive::<ClientEndpoint>(&mut browser, |f| matches!(f, ServerToClient::InputProgress {client_msg_id, ..} if *client_msg_id == without_reason)).await;
    assert!(
        matches!(
            frame,
            ServerToClient::InputProgress {
                stage: InputDeliveryStage::Failed,
                message: None,
                ..
            }
        ),
        "{frame:?}"
    );
}

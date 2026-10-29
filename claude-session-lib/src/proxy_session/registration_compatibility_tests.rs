use super::*;
use futures_util::{SinkExt, StreamExt};

/// Real client registration against a minimal peer: a missing major is a
/// legacy server; an explicitly incompatible success ack must still reject.
#[tokio::test]
async fn registration_checks_server_version_even_on_success() {
    for (version, accepted) in [(None, true), (Some(1), true), (Some(2), false)] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let frame = socket.next().await.unwrap().unwrap();
            let register: ProxyToServer = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            let ProxyToServer::Register(fields) = register else {
                panic!("first frame must register")
            };
            assert_eq!(
                fields.protocol_version,
                Some(shared::protocol::PROTOCOL_VERSION)
            );
            let ack = ServerToProxy::RegisterAck {
                protocol_version: version,
                success: true,
                session_id: fields.session_id,
                error: None,
                max_image_mb: 20,
                retryable: false,
                tunnel_data_ticket: None,
                tunnel_sizing: None,
            };
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::to_string(&ack).unwrap().into(),
                ))
                .await
                .unwrap();
        });
        let config = ProxySessionConfig {
            history_restore: None,
            startup_notice: None,
            context_updates: Default::default(),
            backend_url: url.clone(),
            session_id: Uuid::new_v4(),
            session_name: "compatibility".into(),
            auth_token: None,
            working_directory: std::env::temp_dir().to_string_lossy().into_owned(),
            resume: false,
            prime_portal_reminder_on_first_input: true,
            git_branch: None,
            claude_args: vec![],
            replaces_session_id: None,
            launcher_id: None,
            agent_type: shared::AgentType::Claude,
            scheduled_task_id: None,
            codex_thread_id_sink: None,
            claude_conversation_id_sink: None,
            media_display_sink: None,
            fork_from_session_id: None,
            fork_point_turn_id: None,
        };
        let mut connection = ws_bridge::native_client::connect::<shared::SessionEndpoint>(&url)
            .await
            .unwrap();
        let result = register_session(&mut connection, &config).await;
        assert_eq!(result.is_ok(), accepted);
        if !accepted {
            assert!(matches!(result, Err(RegisterError::Rejected)));
        }
        server.await.unwrap();
    }
}

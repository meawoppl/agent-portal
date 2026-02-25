//! WebSocket connection management for SessionView

use crate::utils;
use shared::api::ErrorMessage;
use shared::{ClientEndpoint, ClientToServer, ServerToClient, WsEndpoint};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::Callback;

use super::types::{PendingPermission, WsSender};
use std::cell::RefCell;
use std::rc::Rc;

/// Messages that can be sent from WebSocket handlers
pub enum WsEvent {
    Connected(WsSender),
    Error(String),
    Output(String),
    HistoryBatch(Vec<String>),
    Permission(PendingPermission),
    BranchChanged(Option<String>, Option<String>),
}

/// Connect to WebSocket and start receiving messages.
/// Returns immediately, spawns async task to handle connection.
pub fn connect_websocket(
    session_id: Uuid,
    replay_after: Option<String>,
    resuming: bool,
    on_event: Callback<WsEvent>,
) {
    spawn_local(async move {
        let ws_endpoint = utils::ws_url(ClientEndpoint::PATH);
        match ws_bridge::yew_client::connect_to::<ClientEndpoint>(&ws_endpoint) {
            Ok(conn) => {
                let (mut sender, mut receiver) = conn.split();

                let register_msg = ClientToServer::Register(shared::RegisterFields {
                    session_id,
                    session_name: session_id.to_string(),
                    auth_token: None,
                    working_directory: String::new(),
                    resuming,
                    git_branch: None,
                    replay_after,
                    client_version: None,
                    replaces_session_id: None,
                    hostname: None,
                    launcher_id: None,
                    agent_type: Default::default(),
                });

                if sender.send(register_msg).await.is_err() {
                    on_event.emit(WsEvent::Error("Failed to send registration".to_string()));
                    return;
                }

                let sender = Rc::new(RefCell::new(Some(sender)));
                on_event.emit(WsEvent::Connected(sender));

                while let Some(result) = receiver.recv().await {
                    match result {
                        Ok(msg) => {
                            handle_proxy_message(msg, &on_event);
                        }
                        Err(e) => {
                            log::error!("WebSocket error: {:?}", e);
                            on_event.emit(WsEvent::Error(format!("{:?}", e)));
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to connect WebSocket: {:?}", e);
                on_event.emit(WsEvent::Error(format!("{:?}", e)));
            }
        }
    });
}

/// Handle incoming server message and emit appropriate events
fn handle_proxy_message(msg: ServerToClient, on_event: &Callback<WsEvent>) {
    match msg {
        ServerToClient::ClaudeOutput {
            content,
            sender_user_id,
            sender_name,
        } => {
            // Inject _sender into content for the renderer if sender info is present
            let output = if sender_user_id.is_some() || sender_name.is_some() {
                if let Some(mut obj) = content.as_object().cloned() {
                    obj.insert(
                        "_sender".to_string(),
                        serde_json::json!({
                            "user_id": sender_user_id.unwrap_or_default(),
                            "name": sender_name.unwrap_or_default(),
                        }),
                    );
                    serde_json::Value::Object(obj).to_string()
                } else {
                    content.to_string()
                }
            } else {
                content.to_string()
            };
            on_event.emit(WsEvent::Output(output));
        }
        ServerToClient::HistoryBatch { messages } => {
            let strings: Vec<String> = messages.into_iter().map(|v| v.to_string()).collect();
            on_event.emit(WsEvent::HistoryBatch(strings));
        }
        ServerToClient::PermissionRequest {
            request_id,
            tool_name,
            input,
            permission_suggestions,
        } => {
            on_event.emit(WsEvent::Permission(PendingPermission {
                request_id,
                tool_name,
                input,
                permission_suggestions,
            }));
        }
        ServerToClient::Error { message } => {
            let error_msg = ErrorMessage::new(message);
            let error_json = serde_json::to_string(&error_msg).unwrap_or_default();
            on_event.emit(WsEvent::Output(error_json));
        }
        ServerToClient::SessionUpdate {
            session_id: _,
            git_branch,
            pr_url,
        } => {
            on_event.emit(WsEvent::BranchChanged(git_branch, pr_url));
        }
        _ => {}
    }
}

/// Send a message over WebSocket
pub fn send_message(sender: &WsSender, msg: ClientToServer) {
    let sender_rc = sender.clone();
    spawn_local(async move {
        let maybe_sender = sender_rc.borrow_mut().take();
        if let Some(mut sender) = maybe_sender {
            let _ = sender.send(msg).await;
            *sender_rc.borrow_mut() = Some(sender);
        }
    });
}

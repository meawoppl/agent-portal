//! WebSocket connection management for SessionView

use crate::utils;
use futures_util::StreamExt;
use shared::api::ErrorMessage;
use shared::{ClientEndpoint, ClientToServer, ServerToClient, WsEndpoint};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::Callback;

use super::types::{PendingPermission, WsSender};

/// Messages that can be sent from WebSocket handlers
pub enum WsEvent {
    Connected(WsSender),
    Error(String),
    Output(String),
    HistoryBatch(Vec<String>),
    Permission(PendingPermission),
    BranchChanged(Option<String>, Option<String>, Option<String>),
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
                    repo_url: None,
                    scheduled_task_id: None,
                });

                if sender.send(register_msg).await.is_err() {
                    on_event.emit(WsEvent::Error("Failed to send registration".to_string()));
                    return;
                }

                // Queued sender plumbing: previously the underlying split
                // `Sender` was wrapped in `Rc<RefCell<Option<_>>>` and
                // `send_message` did a `take`/`await`/restore dance. Two
                // concurrent callers could land in the `take()`-returned-`None`
                // gap and silently drop their message (closes #783). The fix:
                // hand callers an `UnboundedSender<ClientToServer>` and own
                // the real sink in a single spawn_local task that pulls from
                // the receiver in order, awaiting each `send` to completion.
                let (queue_tx, mut queue_rx) = futures_channel::mpsc::unbounded::<ClientToServer>();
                on_event.emit(WsEvent::Connected(queue_tx));

                let on_event_for_send = on_event.clone();
                spawn_local(async move {
                    while let Some(msg) = queue_rx.next().await {
                        if let Err(e) = sender.send(msg).await {
                            log::error!("WebSocket send error: {:?}", e);
                            on_event_for_send.emit(WsEvent::Error(format!("{:?}", e)));
                            break;
                        }
                    }
                });

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
            // agent_type is plumbed through the wire (per-message tag) and
            // available here for future multi-agent UI work — see
            // #723. For now the frontend reads agent_type off the
            // historical-read path (`MessageData::agent_type`); live
            // dispatch stays agent-agnostic.
            agent_type: _,
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
            repo_url,
        } => {
            on_event.emit(WsEvent::BranchChanged(git_branch, pr_url, repo_url));
        }
        _ => {}
    }
}

/// Send a message over WebSocket.
///
/// Pushes onto an unbounded mpsc queue drained by a single owner task — see
/// the `connect_websocket` drain loop. This means concurrent callers (e.g.
/// the file-upload chunk loop in `component.rs`) can never race each other
/// into a "sink temporarily missing, drop the message" hole (closes #783).
/// A failure here only means the WebSocket task already exited (e.g. the
/// connection closed); the caller doesn't have anywhere useful to surface
/// that, so we swallow it to match the prior non-erroring signature.
pub fn send_message(sender: &WsSender, msg: ClientToServer) {
    let _ = sender.unbounded_send(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rapidly push N messages through `send_message` and assert the
    /// receiver drains exactly N items in the order they were sent. This
    /// is the regression test for #783: under the old take/restore
    /// `Rc<RefCell<Option<_>>>` sender, two concurrent producers could
    /// silently drop a message. With the queued mpsc sender, every push
    /// must arrive — `try_recv()` returns `Ok(_)` for each enqueued item
    /// without any executor / await indirection because unbounded mpsc
    /// pushes are synchronous.
    #[test]
    fn send_message_enqueues_all_messages_in_order() {
        const N: usize = 64;
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ClientToServer>();

        for i in 0..N {
            let msg = ClientToServer::ClaudeInput {
                content: serde_json::Value::String(format!("msg-{i}")),
                send_mode: None,
            };
            send_message(&tx, msg);
        }
        drop(tx);

        for i in 0..N {
            match rx.try_recv() {
                Ok(ClientToServer::ClaudeInput { content, .. }) => {
                    assert_eq!(
                        content.as_str(),
                        Some(format!("msg-{i}").as_str()),
                        "message {i} arrived out of order",
                    );
                }
                Ok(other) => panic!("unexpected variant at index {i}: {other:?}"),
                Err(e) => panic!("missing message at index {i}: {e:?}"),
            }
        }

        // Channel is closed and drained — `try_recv` returns `Err(Closed)`.
        assert!(rx.try_recv().is_err());
    }

    /// `send_message` is a non-async, non-blocking push. Verify a single
    /// call lets the receiver observe the message immediately, without any
    /// executor spin (the prior `spawn_local` indirection was what created
    /// the drop window).
    #[test]
    fn send_message_is_synchronous_push() {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ClientToServer>();
        send_message(&tx, ClientToServer::Interrupt);

        assert!(matches!(rx.try_recv(), Ok(ClientToServer::Interrupt)));

        drop(tx);
        assert!(
            rx.try_recv().is_err(),
            "channel must report closed after drop"
        );
    }

    /// Bursting many `send_message` calls from "concurrent" producers (here
    /// modeled as interleaved synchronous pushes from cloned senders, which
    /// is exactly the semantics of multiple `spawn_local`-spawned upload
    /// chunk tasks racing into the same queue) must enqueue every message.
    /// The original `take`/`await`/restore implementation dropped messages
    /// in this scenario; the queue cannot.
    #[test]
    fn concurrent_pushes_from_clones_lose_nothing() {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ClientToServer>();
        let tx_a = tx.clone();
        let tx_b = tx.clone();
        drop(tx);

        // Interleave 100 pushes between two cloned senders.
        for i in 0..50 {
            send_message(&tx_a, ClientToServer::Interrupt);
            send_message(
                &tx_b,
                ClientToServer::ClaudeInput {
                    content: serde_json::Value::String(format!("b-{i}")),
                    send_mode: None,
                },
            );
        }
        drop(tx_a);
        drop(tx_b);

        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 100, "every interleaved push must reach the receiver");
    }
}

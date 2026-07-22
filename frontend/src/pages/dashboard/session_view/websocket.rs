//! WebSocket connection management for SessionView

use crate::utils;
use futures_util::StreamExt;
use shared::api::ErrorMessage;
use shared::{
    ClientEndpoint, ClientToServer, InputDeliveryStage, ServerToClient, TurnMetrics, WsEndpoint,
};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::Callback;

use crate::components::message_renderer::RenderedMessage;

use super::types::{PendingPermission, WsSender};

/// Messages that can be sent from WebSocket handlers
pub enum WsEvent {
    Connected(WsSender),
    Error(String),
    /// Live `ClaudeOutput`. Second field is the server-assigned `created_at`
    /// for the persisted DB row (`None` if the backend didn't send one — the
    /// pre-#784 wire shape and the rare error-envelope paths that don't go
    /// through a DB insert). The frontend uses it as the reconnect-replay
    /// watermark rather than `Date.now()` (closes #784).
    Output(RenderedMessage),
    /// History replay batch. Second field is the server-assigned `created_at`
    /// of the latest message in the batch (`None` if the batch is empty or
    /// the backend is pre-#784). Used to set the reconnect watermark on
    /// initial load.
    HistoryBatch(Vec<RenderedMessage>, Option<String>),
    Permission(PendingPermission),
    BranchChanged(
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<shared::PrRef>,
    ),
    ContinuationStatus {
        continuation_id: Uuid,
        status: String,
    },
    /// Live per-turn metrics broadcast for this session (PR 2 of N).
    /// Boxed to keep `WsEvent` compact — `TurnMetrics` is the largest variant
    /// payload by a wide margin (~22 fields).
    TurnMetrics(Box<TurnMetrics>),
    InputProgress {
        client_msg_id: Uuid,
        stage: InputDeliveryStage,
        message: Option<String>,
    },
    /// The session's port-forward set changed; the view refetches
    /// `GET /api/sessions/{id}/forwards` (docs/PORT_FORWARDING.md).
    ForwardsChanged,
    /// Terminal outcome of a file upload (#939 phase 4). The view gates the
    /// prompt referencing the file on this.
    UploadResult(shared::FileUploadResultFields),
    /// Ephemeral live tool-progress heartbeat (`ServerToClient::ToolProgress`).
    /// Drives the per-session "active tool" strip; never persisted, so it is
    /// not part of `Output`/`HistoryBatch` and carries no timestamp.
    ToolProgress {
        tool_use_id: String,
        parent_tool_use_id: Option<String>,
        tool_name: String,
        elapsed_time_seconds: f64,
    },
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
                    claude_args: Vec::new(),
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
        ServerToClient::AgentOutput {
            content,
            // agent_type is plumbed through the wire (per-message tag) for
            // future multi-agent UI work (#723); the frontend renders off the
            // session-level agent_type. All attribution/timestamp/delivery now
            // rides in the typed `meta` sidecar (see docs/PORTAL_META_SIDECAR.md).
            agent_type: _,
            meta,
        } => {
            on_event.emit(WsEvent::Output(RenderedMessage::new(
                content.to_string(),
                meta,
            )));
        }
        ServerToClient::HistoryBatch {
            entries,
            last_created_at,
        } => {
            let rendered: Vec<RenderedMessage> = entries
                .into_iter()
                .map(|entry| RenderedMessage::new(entry.content.to_string(), entry.meta))
                .collect();
            on_event.emit(WsEvent::HistoryBatch(rendered, last_created_at));
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
        ServerToClient::FileUploadResult(fields) => {
            on_event.emit(WsEvent::UploadResult(fields));
        }
        ServerToClient::Error { message } => {
            let error_msg = ErrorMessage::new(message);
            let error_json = serde_json::to_string(&error_msg).unwrap_or_default();
            // Error envelopes don't go through the DB so they have no
            // server-assigned `created_at` — leave the watermark unchanged.
            on_event.emit(WsEvent::Output(RenderedMessage::new(error_json, None)));
        }
        ServerToClient::SessionUpdate {
            session_id: _,
            git_branch,
            pr_url,
            repo_url,
            open_prs,
        } => {
            on_event.emit(WsEvent::BranchChanged(
                git_branch, pr_url, repo_url, open_prs,
            ));
        }
        // PR 2 of N: route the typed per-turn metrics frame into the
        // SessionView instead of silently dropping it. The previous
        // `_ => {}` arm here is gone — every new wire variant must claim
        // an explicit branch or land in `unhandled` below.
        ServerToClient::TurnMetrics(metrics) => {
            on_event.emit(WsEvent::TurnMetrics(metrics));
        }
        ServerToClient::InputProgress {
            client_msg_id,
            stage,
            message,
        } => {
            on_event.emit(WsEvent::InputProgress {
                client_msg_id,
                stage,
                message,
            });
        }
        ServerToClient::ContinuationStatus {
            continuation_id,
            status,
            message,
        } => {
            let _ = message;
            on_event.emit(WsEvent::ContinuationStatus {
                continuation_id,
                status,
            });
        }
        ServerToClient::ForwardsChanged { session_id: _ } => {
            on_event.emit(WsEvent::ForwardsChanged);
        }
        ServerToClient::ToolProgress {
            tool_use_id,
            parent_tool_use_id,
            tool_name,
            elapsed_time_seconds,
        } => {
            on_event.emit(WsEvent::ToolProgress {
                tool_use_id,
                parent_tool_use_id,
                tool_name,
                elapsed_time_seconds,
            });
        }
        unhandled => {
            // Variants we haven't wired a UI route for yet (e.g. new
            // server-pushed frames added since this branch was written).
            // Logged once at warn so it shows up in the browser console
            // without spamming a busy session.
            log::warn!("Unhandled ServerToClient variant: {:?}", unhandled);
        }
    }
}

/// Send a message over WebSocket. Returns whether the frame was accepted.
///
/// Pushes onto an unbounded mpsc queue drained by a single owner task — see
/// the `connect_websocket` drain loop. This means concurrent callers (e.g.
/// the file-upload chunk loop in `component.rs`) can never race each other
/// into a "sink temporarily missing, drop the message" hole (closes #783).
/// A `false` return means the WebSocket task already exited (the connection is
/// closing); the input outbox uses this to keep the frame queued for resend on
/// reconnect instead of dropping it. Callers with nowhere to surface it can
/// ignore the result.
pub fn send_message(sender: &WsSender, msg: ClientToServer) -> bool {
    sender.unbounded_send(msg).is_ok()
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
            let msg = ClientToServer::AgentInput {
                content: serde_json::Value::String(format!("msg-{i}")),
                send_mode: None,
                client_msg_id: None,
            };
            send_message(&tx, msg);
        }
        drop(tx);

        for i in 0..N {
            match rx.try_recv() {
                Ok(ClientToServer::AgentInput { content, .. }) => {
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
                ClientToServer::AgentInput {
                    content: serde_json::Value::String(format!("b-{i}")),
                    send_mode: None,
                    client_msg_id: None,
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

    // -----------------------------------------------------------------
    // #784: server-assigned watermark plumbing
    //
    // The bug: the reconnect-replay path used the browser's `Date.now()`
    // (or the equivalent `js_sys::Date::new_0()`) as `last_message_timestamp`,
    // then sent that value as `replay_after` on reconnect. The backend
    // filters `messages.created_at.gt(after)`, so clock skew between the
    // browser and the server (or messages already in flight at the moment
    // the client picked its watermark) silently dropped messages from the
    // replayed history — the user never saw them and there was no warning.
    //
    // The fix: the server now sends its DB-row `created_at` alongside
    // every `ClaudeOutput`, and on `HistoryBatch` as a `last_created_at`
    // summary field. The frontend stores those values verbatim into the
    // watermark. These tests pin the WS → WsEvent translation so a future
    // refactor can't silently drop the server timestamp again.

    use shared::ServerToClient;
    use std::cell::RefCell;
    use std::rc::Rc;
    use yew::Callback;

    /// Collect emitted `WsEvent`s into a vec. Yew's `Callback::from` runs
    /// the closure synchronously inside `emit`, so we can just stuff each
    /// emission into an `Rc<RefCell<Vec<_>>>` and read it back after the
    /// call returns — no executor, no `spawn_local`, no WASM needed.
    fn capture() -> (Callback<WsEvent>, Rc<RefCell<Vec<WsEvent>>>) {
        let sink: Rc<RefCell<Vec<WsEvent>>> = Rc::new(RefCell::new(vec![]));
        let sink_inner = sink.clone();
        let cb = Callback::from(move |event: WsEvent| {
            sink_inner.borrow_mut().push(event);
        });
        (cb, sink)
    }

    /// A `ServerToClient::ClaudeOutput` carrying a `created_at` must be
    /// translated into a `WsEvent::Output` whose `PortalMeta.created_at`
    /// carries the exact server-assigned timestamp string. This is the value
    /// the component pipes into `last_message_timestamp` (#784); if it ever
    /// falls back to `Date.now()` here the silent-data-loss regression returns.
    #[test]
    fn claude_output_with_created_at_emits_server_timestamp() {
        let (cb, sink) = capture();
        let server_ts = "2026-05-18T12:34:56.789012".to_string();
        let msg = ServerToClient::AgentOutput {
            content: serde_json::json!({"type": "assistant", "text": "hi"}),
            agent_type: shared::AgentType::Claude,
            meta: Some(shared::PortalMeta {
                created_at: Some(server_ts.clone()),
                source: None,
                delivery: None,
            }),
        };

        handle_proxy_message(msg, &cb);

        let events = sink.borrow();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WsEvent::Output(message) => {
                assert_eq!(
                    message.meta.as_ref().and_then(|m| m.created_at.as_deref()),
                    Some(server_ts.as_str()),
                    "watermark must be the server's created_at, never Date.now()"
                );
                let parsed: serde_json::Value = serde_json::from_str(&message.content).unwrap();
                assert_eq!(
                    parsed.get("_created_at"),
                    None,
                    "content must not receive flattened portal metadata"
                );
            }
            other => panic!("expected Output, got {:?}", debug_event(other)),
        }
    }

    /// Inter-agent attribution is carried on the live `AgentOutput.meta`
    /// sidecar. It must stay out of the raw content JSON so renderers read a
    /// typed source instead of `_origin` conventions.
    #[test]
    fn agent_output_with_meta_keeps_origin_in_sidecar() {
        let (cb, sink) = capture();
        let from_session_id =
            uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("uuid");
        let msg = ServerToClient::AgentOutput {
            content: serde_json::json!({
                "type": "portal",
                "content": [{"type": "text", "text": "hello from peer"}],
            }),
            agent_type: shared::AgentType::Codex,
            meta: Some(shared::PortalMeta {
                created_at: None,
                source: Some(shared::MessageSource::Agent {
                    session_id: from_session_id,
                    agent_type: "codex".to_string(),
                }),
                delivery: None,
            }),
        };

        handle_proxy_message(msg, &cb);

        let events = sink.borrow();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WsEvent::Output(message) => {
                assert!(message.content.contains("hello from peer"));
                assert!(!message.content.contains("_origin"));
                assert_eq!(
                    message.meta.as_ref().and_then(|m| m.source.as_ref()),
                    Some(&shared::MessageSource::Agent {
                        session_id: from_session_id,
                        agent_type: "codex".to_string(),
                    })
                );
            }
            other => panic!("expected Output, got {:?}", debug_event(other)),
        }
    }

    /// Backward-compat: a pre-#784 backend doesn't send `created_at`. The
    /// `WsEvent::Output` watermark is then `None`, and the component is
    /// expected to keep its prior watermark (never falling back to
    /// `Date.now()`). The shape that must NOT regress is the second
    /// tuple element being `None` — the existence of the field is what
    /// lets the component keep using the previous server timestamp.
    #[test]
    fn claude_output_without_created_at_emits_none_watermark() {
        let (cb, sink) = capture();
        let msg = ServerToClient::AgentOutput {
            content: serde_json::json!({"type": "assistant"}),
            agent_type: shared::AgentType::Claude,
            meta: None,
        };

        handle_proxy_message(msg, &cb);

        let events = sink.borrow();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WsEvent::Output(message) => assert!(message.meta.is_none()),
            other => panic!("expected Output, got {:?}", debug_event(other)),
        }
    }

    /// `HistoryBatch` must carry the server-supplied `last_created_at`
    /// through to the component verbatim. The component uses this to
    /// initialize `last_message_timestamp` after a replay batch lands
    /// — without it the next reconnect would re-fall-back to whatever
    /// the component had stored from `Date.now()` (the original bug).
    #[test]
    fn history_batch_propagates_server_last_created_at() {
        let (cb, sink) = capture();
        let ts = "2026-05-18T00:00:00.000000".to_string();
        let msg = ServerToClient::HistoryBatch {
            entries: vec![shared::HistoryEntry {
                content: serde_json::json!({"type": "assistant"}),
                meta: Some(shared::PortalMeta {
                    created_at: Some(ts.clone()),
                    source: None,
                    delivery: None,
                }),
            }],
            last_created_at: Some(ts.clone()),
        };

        handle_proxy_message(msg, &cb);

        let events = sink.borrow();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WsEvent::HistoryBatch(batch, last) => {
                assert_eq!(batch.len(), 1);
                assert_eq!(last.as_deref(), Some(ts.as_str()));
                assert_eq!(
                    batch[0].meta.as_ref().and_then(|m| m.created_at.as_deref()),
                    Some(ts.as_str())
                );
            }
            other => panic!("expected HistoryBatch, got {:?}", debug_event(other)),
        }
    }

    /// Empty / pre-#784 `HistoryBatch` (no `last_created_at`) must
    /// translate to `WsEvent::HistoryBatch(_, None)` so the component
    /// keeps any prior watermark instead of being clobbered.
    #[test]
    fn history_batch_without_last_created_at_emits_none() {
        let (cb, sink) = capture();
        let msg = ServerToClient::HistoryBatch {
            entries: vec![],
            last_created_at: None,
        };

        handle_proxy_message(msg, &cb);

        let events = sink.borrow();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WsEvent::HistoryBatch(_, last) => assert!(last.is_none()),
            other => panic!("expected HistoryBatch, got {:?}", debug_event(other)),
        }
    }

    /// `ServerToClient::Error` envelopes don't go through the DB so they
    /// have no server-assigned `created_at`. The translation must emit
    /// `None` for the watermark slot — the component keeps the prior
    /// watermark rather than reaching for `Date.now()` (the exact bug
    /// #784 fixes; this test pins the no-regression contract for the
    /// non-DB live-message path).
    #[test]
    fn error_envelope_emits_none_watermark() {
        let (cb, sink) = capture();
        let msg = ServerToClient::Error {
            message: "boom".to_string(),
        };

        handle_proxy_message(msg, &cb);

        let events = sink.borrow();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WsEvent::Output(message) => assert!(message.meta.is_none()),
            other => panic!("expected Output, got {:?}", debug_event(other)),
        }
    }

    /// `WsEvent` doesn't `Debug` (some fields are non-Debug), so use a
    /// hand-rolled variant tag for assertion failure messages.
    fn debug_event(ev: &WsEvent) -> &'static str {
        match ev {
            WsEvent::Connected(_) => "Connected",
            WsEvent::Error(_) => "Error",
            WsEvent::Output(_) => "Output",
            WsEvent::HistoryBatch(_, _) => "HistoryBatch",
            WsEvent::Permission(_) => "Permission",
            WsEvent::BranchChanged(_, _, _, _) => "BranchChanged",
            WsEvent::ContinuationStatus { .. } => "ContinuationStatus",
            WsEvent::TurnMetrics(_) => "TurnMetrics",
            WsEvent::InputProgress { .. } => "InputProgress",
            WsEvent::ForwardsChanged => "ForwardsChanged",
            WsEvent::UploadResult(_) => "UploadResult",
            WsEvent::ToolProgress { .. } => "ToolProgress",
        }
    }
}

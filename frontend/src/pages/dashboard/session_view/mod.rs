//! SessionView module - Main terminal view for a single session
//!
//! This module is split into:
//! - `component.rs` - Residual `SessionView` orchestrator (WS connect/reconnect,
//!   message-buffer rendering, awaiting-input gate, sub-component glue)
//! - `helpers.rs` - Pure helpers (msg-type classification, metadata injection,
//!   pending-send reconciliation, autoscroll-transition gate)
//! - `input_bar.rs` - Textarea, send-mode dropdown, file upload, voice, history
//! - `permission_handler.rs` - Permission-request UI lifecycle
//! - `tasks_panel.rs` - Sub-agent / background-task drawer + `derive_task_events`
//! - `types.rs` - Types specific to SessionView (re-exports from parent)
//! - `websocket.rs` - WebSocket connection management
//! - `history.rs` - Command history management

mod component;
mod forward_chips;
pub(super) mod helpers;
mod history;
mod input_bar;
mod outbox;
mod permission_handler;
mod state;
mod tasks_panel;
mod types;
mod websocket;

pub use component::SessionView;
pub(crate) use helpers::ActivityTag;

//! SessionView module - Main terminal view for a single session
//!
//! This module is split into:
//! - `component.rs` - Main SessionView Yew component
//! - `input_bar.rs` - Textarea, send-mode dropdown, file upload, voice, history
//! - `permission_handler.rs` - Permission-request UI lifecycle
//! - `tasks_panel.rs` - Sub-agent / background-task drawer
//! - `types.rs` - Types specific to SessionView (re-exports from parent)
//! - `websocket.rs` - WebSocket connection management
//! - `history.rs` - Command history management

mod component;
mod history;
mod input_bar;
mod permission_handler;
mod tasks_panel;
mod types;
mod websocket;

pub use component::SessionView;

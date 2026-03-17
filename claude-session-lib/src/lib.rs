//! Claude Session Library
//!
//! A library for managing Claude Code sessions, designed for use in
//! persistence services that need to manage multiple sessions, handle
//! restarts, and maintain state across service restarts.
//!
//! # Overview
//!
//! The library provides:
//! - `Session` - A managed Claude Code session with event-based API
//! - `SessionSnapshot` - Serializable session state for persistence
//! - `OutputBuffer` - Buffer for replay on session restore
//!
//! # Usage
//!
//! Create a `SessionConfig`, spawn a `Session`, and loop over `SessionEvent`s.
//! Handle `Output`, `PermissionRequest`, `Exited`, and `Error` variants as needed.

pub mod buffer;
pub mod error;
pub mod heartbeat;
pub mod output_buffer;
pub mod proxy_session;
pub mod session;
pub mod snapshot;

// Re-export main types at crate root
pub use buffer::{BufferedOutput, OutputBuffer};
pub use error::SessionError;
pub use session::{PermissionResponse, Session, SessionEvent};
pub use snapshot::{PendingPermission, SessionConfig, SessionSnapshot};

// Re-export proxy session types
pub use proxy_session::{
    run_connection_loop, ConnectionResult, LoopResult, ProxySessionConfig, SessionState,
};

// Re-export claude_codes types that appear in our public API
pub use claude_codes::io::PermissionSuggestion;
pub use claude_codes::{ClaudeOutput, Permission};

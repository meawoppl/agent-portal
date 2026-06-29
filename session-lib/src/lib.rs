// Ratchet for the workspace unwrap/expect deny (#1165 item 8): this crate
// still has production unwrap/expect; remove this allow as it is cleaned.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Session Library (agent-agnostic core)
//!
//! Generic session-management primitives shared by all per-agent crates
//! (`claude-session-lib`, `codex-session-lib`). Each agent crate defines a
//! zero-sized type that implements [`Agent`] and consumers parametrize
//! [`Session`] over it:
//!
//! ```ignore
//! use session_lib::{Session, SessionConfig};
//! use claude_session_lib::ClaudeAgent;
//!
//! let cfg = SessionConfig { /* ... */ };
//! let session: Session<ClaudeAgent> = Session::new(cfg).await?;
//! ```
//!
//! Heterogeneous consumers (e.g. the launcher) wrap the per-agent
//! `Session<A>` in an enum at the dispatch boundary.

pub mod adapter;
pub mod agent;
pub mod buffer;
pub mod error;
pub mod heartbeat;
pub mod io;
pub mod output_buffer;
pub mod probe;
pub mod session;
pub mod snapshot;
pub mod turn_tracker;

pub use adapter::{AgentAdapter, AgentOutput, ClaudeAdapter, PermissionDecision};
pub use agent::Agent;
pub use buffer::{BufferedOutput, OutputBuffer};
pub use error::SessionError;
pub use io::{IoCommand, IoEvent, PermissionResponse, SessionEvent};
pub use session::Session;
pub use snapshot::{PendingPermission, SessionConfig, SessionSnapshot};
pub use turn_tracker::{TurnOutcome, TurnTracker};

// Re-export claude_codes types that appear in our public API. Per-agent
// crates can either reach for these or import claude_codes directly.
pub use claude_codes::io::PermissionSuggestion;
pub use claude_codes::{ClaudeOutput, Permission};

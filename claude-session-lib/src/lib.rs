//! Claude Session Library
//!
//! Claude-specific backend for [`session_lib`]: defines [`ClaudeAgent`] (the
//! per-agent dispatch type) and the `claude_io_task` that owns the `claude`
//! CLI process. Also re-houses the `proxy_session` connection loop used by
//! the `claude-portal` proxy binary — that loop is claude-specific (wiggum
//! mode, portal-reminder injection, image upload, stream-json output
//! forwarding) and isn't getting split until a future PR.

pub mod agent;
pub mod io_task;
pub mod proxy_session;
mod spawn;
pub mod transcript;

pub use agent::ClaudeAgent;
pub use spawn::claude_cli_args;
pub use transcript::{claude_transcript_status, TranscriptStatus};

// Re-export the proxy session helpers used by the proxy binary.
pub use proxy_session::{
    default_session_name, hostname_or_unknown, run_connection_loop, ConnectionResult, LoopResult,
    PortalInput, ProxySessionConfig, SessionState,
};

// Convenience re-exports so existing consumers don't all have to add
// `session-lib` to their Cargo.toml just to grab the basics.
pub use session_lib::buffer::{BufferedOutput, OutputBuffer};
pub use session_lib::error::SessionError;
pub use session_lib::io::{PermissionResponse, SessionEvent};
pub use session_lib::output_buffer;
pub use session_lib::session::Session;
pub use session_lib::snapshot::{PendingPermission, SessionConfig, SessionSnapshot};

// Re-export claude_codes types that appear in our public API.
pub use claude_codes::io::PermissionSuggestion;
pub use claude_codes::{ClaudeOutput, Permission};

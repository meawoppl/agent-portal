//! Antigravity backend for `session-lib`.
//!
//! This is intentionally a preview integration. The harness runs with its
//! read-only built-in tools, launches per turn, and is shut down after every
//! accepted turn so `sessionEndResponse` confirms state was flushed before the
//! portal reports the turn durable.

mod agent;
mod io_task;

pub use agent::AntigravityAgent;

pub const DEFAULT_MODEL: &str = "gemini-flash-latest";

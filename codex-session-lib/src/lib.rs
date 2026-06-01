//! Codex Session Library
//!
//! Codex-specific backend for [`session_lib`]: defines [`CodexAgent`] (the
//! per-agent dispatch type) and the `codex_io_task` that owns the `codex
//! app-server` process. Construct `Session<CodexAgent>` to get a Codex-
//! backed session.

pub mod agent;
mod events;
mod handler;
mod helpers;
mod io_task;

pub use agent::CodexAgent;

//! Google Antigravity backend for `session-lib`.
//!
//! Antigravity's `localharness` bootstraps over binary stdio and then moves to
//! an authenticated loopback WebSocket. The transport and step assembly live
//! in `antigravity-codes`; this crate maps that runtime onto Portal's neutral
//! `Session<A>` boundary.

mod agent;
mod io_task;

pub use agent::AntigravityAgent;

//! In-memory capture of codex-codes raw frames.
//!
//! `codex-codes`'s `AsyncClient::read_message_opt` logs every JSON-RPC frame
//! it reads via `debug!("[CLIENT] Received: {raw}", target = "codex_codes::client_async")`
//! before attempting typed deserialization. When the typed decode then fails
//! (the canonical `missing field 'callId'` mismatch between codex CLI
//! 0.130.0 and codex-codes 0.128.0), the raw JSON is lost — agent-portal
//! sees only a `serde_json::Error` with a `line:column` pointer.
//!
//! This module ships a tracing [`Layer`] that intercepts those debug events
//! at runtime, stashes the raw JSON in a process-wide ring buffer, and lets
//! the proxy retrieve the most-recent frame when it emits its `turn.failed`
//! event. The retrieved frame is then attached to a portal message so the
//! user can copy it for bug reports.
//!
//! The layer declares per-callsite `Interest::always()` for
//! `codex_codes::client_async` DEBUG events, so it sees them even when the
//! root `EnvFilter` (e.g. `RUST_LOG=info,claude_session_lib=debug`) would
//! otherwise suppress them. Other events are explicitly opted out
//! (`Interest::never()`) so the layer adds zero per-event overhead outside
//! its target.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

const TARGET: &str = "codex_codes::client_async";
const PREFIX: &str = "[CLIENT] Received: ";
const RING_CAPACITY: usize = 8;

static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn ring() -> &'static Mutex<VecDeque<String>> {
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_CAPACITY + 1)))
}

/// Pop and return the most-recently captured Codex raw frame, if any.
///
/// Called by the codex I/O loop on a typed-decode failure. The buffer is
/// process-wide; with multiple concurrent codex sessions the most-recent
/// entry may belong to a different session, but in practice frames are
/// captured a few microseconds before the typed decode fails on the same
/// thread, so the immediate predecessor is overwhelmingly the offending
/// frame.
pub fn take_most_recent() -> Option<String> {
    ring().lock().ok()?.pop_back()
}

fn is_codex_received(metadata: &Metadata<'_>) -> bool {
    metadata.target() == TARGET && *metadata.level() == Level::DEBUG
}

/// Tracing layer that captures codex-codes' `[CLIENT] Received: …` frames.
///
/// Install once, alongside (not wrapped in) your `EnvFilter` so the layer
/// can see DEBUG events that the env filter would otherwise drop:
///
/// ```ignore
/// let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
///     .unwrap_or_else(|_| "info".into());
/// tracing_subscriber::registry()
///     .with(tracing_subscriber::fmt::layer().with_filter(env_filter))
///     .with(claude_session_lib::codex_frame_capture::CodexFrameCaptureLayer::new())
///     .init();
/// ```
#[derive(Default, Clone, Copy)]
pub struct CodexFrameCaptureLayer;

impl CodexFrameCaptureLayer {
    pub fn new() -> Self {
        Self
    }
}

struct MessageVisitor<'a> {
    out: &'a mut Option<String>,
}

impl Visit for MessageVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            *self.out = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            *self.out = Some(format!("{:?}", value));
        }
    }
}

impl<S: Subscriber> Layer<S> for CodexFrameCaptureLayer {
    fn register_callsite(&self, metadata: &Metadata<'_>) -> Interest {
        if is_codex_received(metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        is_codex_received(metadata)
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !is_codex_received(meta) {
            return;
        }
        let mut msg = None;
        event.record(&mut MessageVisitor { out: &mut msg });
        let Some(msg) = msg else { return };
        let Some(json) = msg.strip_prefix(PREFIX) else {
            return;
        };
        // `record_debug` of an `fmt::Arguments` re-formats via Debug, which
        // for Arguments is identical to Display — so we get the raw text
        // without escape sequences. Trim trailing whitespace just in case.
        let json = json.trim();
        if json.is_empty() {
            return;
        }
        let Ok(mut ring) = ring().lock() else {
            return;
        };
        while ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(json.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn captures_codex_received_debug() {
        // Don't init globally — the test runner shares state. Use
        // `with_default` for scoped registration.
        let layer = CodexFrameCaptureLayer::new();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(
                target: "codex_codes::client_async",
                "[CLIENT] Received: {{\"jsonrpc\":\"2.0\",\"method\":\"item/commandExecution/requestApproval\",\"params\":{{}}}}"
            );
            tracing::debug!(
                target: "codex_codes::client_async",
                "[CLIENT] Received: {{\"jsonrpc\":\"2.0\",\"id\":7}}"
            );
            // Unrelated events should be ignored.
            tracing::warn!(target: "codex_codes::stderr", "noise");
            tracing::info!(target: "other::module", "[CLIENT] Received: not-mine");
        });

        let newest = take_most_recent();
        assert_eq!(
            newest.as_deref(),
            Some(r#"{"jsonrpc":"2.0","id":7}"#),
            "should return the most-recently captured codex frame"
        );

        let prior = take_most_recent();
        assert!(
            prior
                .as_deref()
                .map(|s| s.contains("commandExecution/requestApproval"))
                .unwrap_or(false),
            "ring buffer should retain a few frames of history, got {:?}",
            prior
        );

        // Drain any remaining test state so other tests in the module
        // (or in cross-test runs) start clean.
        while take_most_recent().is_some() {}
    }
}

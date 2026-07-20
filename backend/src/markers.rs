//! Stable, operationally-alertable log markers.
//!
//! Some failures are deliberately **logged-and-continued** rather than
//! propagated: dropping a durable message or crashing a background sweep would
//! be worse than the failure itself. To stay observable, each such site emits a
//! **stable SCREAMING_SNAKE marker** as the first token of its log line so
//! operators can alert on the substring (e.g. a Loki/CloudWatch match).
//!
//! This module is the **single source of truth** for those markers. Rules:
//!
//! - Every alertable marker is a `pub const &str` here whose **value equals its
//!   name**. Emit sites reference the const (never a bare string literal) so a
//!   rename is a compile-time, repo-wide change — and so the value stays
//!   byte-identical to whatever existing alerts already match on.
//! - Each const carries a doc comment covering (a) **what emits it and when**,
//!   (b) what a **recurring burst vs a one-off** indicates, and (c) the
//!   **operator action**.
//! - Adding a new logged-and-continue failure on a durable-loss path? Add a
//!   const here (with the doc treatment) *and* a row to the marker table in
//!   `CLAUDE.md` ("Operational log markers"). The `markers` unit test enforces
//!   uniqueness and naming.

/// Emitted when a browser/agent input could **not be persisted** to the
/// `pending_inputs` table — either the INSERT failed or no DB connection was
/// available to attempt it. The message may still reach a live proxy, but the
/// durable copy that replays on proxy reconnect is missing, so an input sent
/// while the proxy is bouncing can be silently lost.
///
/// - **Recurring burst right after a deploy:** almost always schema drift — a
///   migration recorded in `__diesel_schema_migrations` but not physically
///   applied (see CLAUDE.md "Migration hygiene"). Every INSERT fails until the
///   column is reconciled.
/// - **One-off:** a transient DB blip (pool exhaustion, failover). Self-heals.
/// - **Action:** on a burst, diff the live schema against the latest migration
///   (`\d pending_inputs`) and apply any missing column with
///   `ALTER TABLE … ADD COLUMN IF NOT EXISTS …`; the migrations table already
///   records it, so restarts stay consistent.
pub const PENDING_INPUT_PERSIST_FAILED: &str = "PENDING_INPUT_PERSIST_FAILED";

/// Emitted when bumping a session's monotonic `input_seq` fails; the enqueue
/// falls back to sequence `1`. A wrong/duplicate sequence number can corrupt
/// replay ordering on the affected session, so this shares the durable-input
/// blast radius with [`PENDING_INPUT_PERSIST_FAILED`].
///
/// - **Recurring:** same schema-drift / DB-degradation signals as
///   `PENDING_INPUT_PERSIST_FAILED`, scoped to the `sessions` table's
///   `input_seq` column; ordering for those sessions is unreliable until fixed.
/// - **One-off:** a transient DB blip on a single enqueue. Tolerable.
/// - **Action:** same as `PENDING_INPUT_PERSIST_FAILED` — reconcile schema
///   drift; otherwise investigate DB health.
pub const INPUT_SEQ_BUMP_FAILED: &str = "INPUT_SEQ_BUMP_FAILED";

/// Emitted by the push dispatcher (`push/dispatcher.rs`) for every real
/// delivery-path failure it swallows: resolving the recipient, loading
/// subscriptions, delivering to an endpoint, or recording delivery state. A
/// missed push is intentionally never fatal to the request that triggered it.
///
/// - **Recurring burst:** a systemic push fault — VAPID/APNs/FCM
///   misconfiguration or credential expiry, or a DB problem resolving
///   recipients. Users stop receiving notifications while it persists.
/// - **One-off:** a single dead/rotated endpoint or a transient network error;
///   dead endpoints are auto-disabled and expected in small numbers.
/// - **Action:** on a burst, verify push credentials
///   (`PORTAL_VAPID_*`, `PORTAL_APNS_*`, `PORTAL_FCM_*`) and DB health; a low
///   trickle needs no action.
pub const PUSH_DISPATCH_FAILED: &str = "PUSH_DISPATCH_FAILED";

/// Emitted when archiving a single eligible session fails during the archive
/// sweep (`background.rs`). Archive-first is the retention invariant, so a
/// failure here also **holds** that session's retention trim (see
/// [`RETENTION_TRIM_HELD`]) — the hot DB keeps its data rather than losing the
/// unarchived delta.
///
/// - **Recurring burst:** the archive backend is unhealthy — bad
///   `PORTAL_SESSION_ARCHIVE_*` config, S3 credential/permission failure, or a
///   full/unwritable local root. Retention is blocked for those sessions and
///   the hot DB grows until it recovers.
/// - **One-off:** a transient backend hiccup on a single session; retried next
///   sweep.
/// - **Action:** on a burst, check archive-backend reachability and
///   credentials/permissions and the `PORTAL_SESSION_ARCHIVE_*` settings.
pub const SESSION_ARCHIVE_FAILED: &str = "SESSION_ARCHIVE_FAILED";

/// Emitted when the whole archive sweep task returns an error before it can
/// process sessions (e.g. it could not get a DB connection). Distinct from
/// [`SESSION_ARCHIVE_FAILED`], which is per-session; this means the sweep as a
/// whole did no useful work this cycle.
///
/// - **Recurring:** a persistent problem reaching the DB or the archive
///   subsystem; no sessions are being archived, so retention is globally held.
/// - **One-off:** a transient DB-pool checkout failure for one cycle; the next
///   scheduled sweep retries.
/// - **Action:** on a burst, check DB-pool health and the archive backend; the
///   sweep is idempotent and self-recovers once the dependency is healthy.
pub const ARCHIVE_SWEEP_FAILED: &str = "ARCHIVE_SWEEP_FAILED";

/// Emitted when the retention cleanup cycle **holds** one or more sessions'
/// trims because their pre-trim archive did not succeed this cycle (archive-first
/// invariant). Held sessions are excluded from both delete paths and retried
/// next cycle, so no unarchived data is lost — but their hot-DB rows keep
/// growing until the archive recovers.
///
/// - **Recurring/growing count:** an archive outage is silently blocking
///   retention; expect this alongside [`SESSION_ARCHIVE_FAILED`] /
///   [`ARCHIVE_SWEEP_FAILED`], and watch hot-DB size.
/// - **One-off / small count:** a session or two briefly behind the archive;
///   clears on its own next cycle.
/// - **Action:** treat a sustained/rising count as an archive-backend incident —
///   restore the backend; retention drains automatically once archiving
///   succeeds. (An empty held set when archiving is disabled is normal.)
pub const RETENTION_TRIM_HELD: &str = "RETENTION_TRIM_HELD";

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry of every stable marker. New markers MUST be added here so
    /// the tests below can enforce uniqueness, naming, and that each is wired to
    /// a real emit site.
    const ALL_MARKERS: &[&str] = &[
        PENDING_INPUT_PERSIST_FAILED,
        INPUT_SEQ_BUMP_FAILED,
        PUSH_DISPATCH_FAILED,
        SESSION_ARCHIVE_FAILED,
        ARCHIVE_SWEEP_FAILED,
        RETENTION_TRIM_HELD,
    ];

    #[test]
    fn markers_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in ALL_MARKERS {
            assert!(seen.insert(*m), "duplicate marker value: {m}");
        }
    }

    #[test]
    fn markers_are_screaming_snake() {
        for m in ALL_MARKERS {
            assert!(!m.is_empty(), "empty marker");
            assert!(
                m.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "marker not SCREAMING_SNAKE: {m}"
            );
            assert!(
                m.starts_with(|c: char| c.is_ascii_uppercase()),
                "marker must start with an uppercase letter: {m}"
            );
            assert!(!m.contains("__"), "marker has doubled underscore: {m}");
            assert!(
                !m.starts_with('_') && !m.ends_with('_'),
                "marker has leading/trailing underscore: {m}"
            );
        }
    }

    /// Guards against a marker defined but never wired to an emit site: every
    /// const's value must appear somewhere in a known emit-site source other
    /// than its own definition in this module. Uses `include_str!` on sibling
    /// files so no build hacks are required.
    #[test]
    fn every_marker_is_referenced_by_an_emit_site() {
        let sources: &[&str] = &[
            include_str!("background.rs"),
            include_str!("push/dispatcher.rs"),
            include_str!("handlers/websocket/session_manager/input_queue.rs"),
        ];
        for m in ALL_MARKERS {
            let referenced = sources.iter().any(|s| s.contains(m));
            assert!(
                referenced,
                "marker {m} is not referenced by any known emit-site source; \
                 wire it up or remove it"
            );
        }
    }
}

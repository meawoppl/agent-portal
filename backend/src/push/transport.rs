//! Push transports (mobile-apps plan §8.3).
//!
//! A [`PushTransport`] turns a resolved [`PushPayload`] into an actual push for
//! one [`PushSubscription`]. The trait is the seam every stage plugs into: v1
//! ships only [`LogTransport`] (logs delivery intent), C3 adds a `web-push`
//! transport, and C7 adds native APNs/FCM — all behind this one contract, all
//! driven by the same dispatcher.

use crate::models::PushSubscription;
use crate::push::PushPayload;

/// Outcome of a single delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// The push provider accepted the payload.
    Delivered,
    /// The endpoint is permanently gone (e.g. a Web Push 404/410). The
    /// dispatcher marks the subscription `disabled_at` so it is skipped until a
    /// re-registration revives it.
    GoneDeadEndpoint,
}

/// A transport-level failure that is *not* a dead endpoint (a transient network
/// error, an auth/config problem, a serialization failure). The dispatcher logs
/// these under the `PUSH_DISPATCH_FAILED` marker and leaves the subscription
/// intact for the next event.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("push transport error: {0}")]
    Transport(String),
}

/// Deliver a [`PushPayload`] to one subscription.
///
/// Implementations must be cheap to hold across the dispatcher loop and safe to
/// call concurrently. The method is spelled as a `-> impl Future + Send` rather
/// than `async fn` so the returned future is guaranteed `Send` (the dispatcher
/// runs on `tokio::spawn`); implementors may still write it as an `async fn`.
pub trait PushTransport {
    fn send(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> impl std::future::Future<Output = Result<SendOutcome, PushError>> + Send;
}

/// v1 transport: log the delivery intent at info level. Lets the whole
/// dispatch pipeline (resolution, suppression, prefs, subscription fan-out,
/// success/dead-endpoint bookkeeping) be exercised end to end before any real
/// push crate is wired in (C3).
pub struct LogTransport;

impl PushTransport for LogTransport {
    async fn send(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<SendOutcome, PushError> {
        tracing::info!(
            "push delivery intent: platform={} subscription={} session={} kind={} title={:?} body={:?} collapse_key={}",
            sub.platform,
            sub.id,
            payload.session_id,
            payload.event_kind,
            payload.title,
            payload.body,
            payload.collapse_key,
        );
        Ok(SendOutcome::Delivered)
    }
}

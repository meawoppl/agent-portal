//! Push transports (mobile-apps plan §8.3).
//!
//! A [`PushTransport`] turns a resolved [`PushPayload`] into an actual push for
//! one [`PushSubscription`]. The trait is the seam every stage plugs into: v1
//! ships only [`LogTransport`] (logs delivery intent), C3 adds a `web-push`
//! transport, and C7 adds native APNs/FCM — all behind this one contract, all
//! driven by the same dispatcher.

use crate::models::PushSubscription;
use crate::push::PushPayload;

/// A push-service status that means the subscription is permanently dead: the
/// endpoint 404s or explicitly reports Gone (HTTP 410). Single home for the
/// Web Push and FCM response mappings (and their tests) so a new dead-arm
/// can't land in one transport and be missed in the other.
pub fn is_dead_endpoint(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn dead_endpoint_covers_gone_and_missing_only() {
        assert!(is_dead_endpoint(StatusCode::NOT_FOUND));
        assert!(is_dead_endpoint(StatusCode::GONE));
        for live in [StatusCode::OK, StatusCode::CREATED, StatusCode::ACCEPTED] {
            assert!(!is_dead_endpoint(live), "{live} is a live endpoint");
        }
        for transient in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(!is_dead_endpoint(transient), "{transient} is not dead");
        }
    }
}

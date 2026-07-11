//! Web Push transport (mobile-apps plan §8.3, work item C3).
//!
//! Turns a resolved [`PushPayload`] into a VAPID-signed, RFC8291-encrypted Web
//! Push request and delivers it to a browser subscription's endpoint.
//!
//! Split of responsibilities:
//! - the [`web_push`] crate builds the message: it VAPID-signs with the
//!   application-server private key and encrypts the payload against the
//!   subscription's `p256dh` / `auth` keys (RFC8291, `aes128gcm`);
//! - the backend's existing [`reqwest`] client sends it. web-push also ships
//!   its own HTTP clients (isahc/libcurl, hyper), but we disable them
//!   (`default-features = false`) and reuse reqwest so no second HTTP stack is
//!   added.
//!
//! Response mapping (§8.3): 2xx → [`SendOutcome::Delivered`]; 404/410 →
//! [`SendOutcome::GoneDeadEndpoint`] (the dispatcher stamps `disabled_at`);
//! anything else → [`PushError`] (the dispatcher logs `PUSH_DISPATCH_FAILED`).

use serde::Serialize;
use web_push::{
    ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessage, WebPushMessageBuilder,
};

use crate::models::PushSubscription;
use crate::push::transport::{PushError, PushTransport, SendOutcome};
use crate::push::PushPayload;

/// The `platform` column value for browser Web Push subscriptions. APNs/FCM
/// rows carry different values and are handled by native transports (C7); this
/// transport skips them with a clear error rather than mis-sending.
const WEBPUSH_PLATFORM: &str = "webpush";

/// Time-to-live handed to the push service: how long it should retain the
/// notification for an offline device before dropping it. One day matches the
/// "reach a pocketed phone" intent without holding stale interrupts forever.
const PUSH_TTL_SECS: u32 = 60 * 60 * 24;

/// The JSON body delivered to the service worker. A typed struct (never
/// `serde_json::json!`) so the wire shape is a compile-time contract shared
/// with the frontend's `push` handler. `tag` carries the collapse key so the
/// browser shows one notification per session (§8.2).
#[derive(Debug, Serialize)]
struct WebPushBody<'a> {
    session_id: &'a str,
    event_kind: &'a str,
    title: &'a str,
    body: &'a str,
    tag: &'a str,
}

/// Web Push transport backed by a single VAPID application-server key pair.
///
/// Holds the private key and a cloned reqwest client (cheap: reqwest clients
/// share a connection pool behind an `Arc`). Safe to hold across the dispatcher
/// loop and to call concurrently.
pub struct WebPushTransport {
    /// VAPID application-server private key: URL-safe base64 (no padding) or a
    /// PEM-encoded EC private key. Resolved once at construction.
    vapid_private_key: String,
    http: reqwest::Client,
}

impl WebPushTransport {
    /// Build a transport from the configured VAPID private key.
    pub fn new(vapid_private_key: String) -> Self {
        Self {
            vapid_private_key,
            http: reqwest::Client::new(),
        }
    }

    /// Build the VAPID-signed, encrypted [`WebPushMessage`] for one payload.
    /// Pure (no I/O) so it is unit-testable without a push service; the network
    /// send is the caller's separate step.
    fn build_message(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<WebPushMessage, PushError> {
        if sub.platform != WEBPUSH_PLATFORM {
            return Err(PushError::Transport(format!(
                "WebPushTransport cannot deliver to platform {:?} (subscription {}); \
                 native APNs/FCM is C7",
                sub.platform, sub.id
            )));
        }

        // Browser subscriptions must carry both encryption keys; a row missing
        // them is malformed (never a dead endpoint), so surface it as an error.
        let p256dh = sub.p256dh.as_deref().ok_or_else(|| {
            PushError::Transport(format!("subscription {} missing p256dh key", sub.id))
        })?;
        let auth = sub.auth.as_deref().ok_or_else(|| {
            PushError::Transport(format!("subscription {} missing auth key", sub.id))
        })?;

        let subscription_info = SubscriptionInfo::new(sub.endpoint_or_token.as_str(), p256dh, auth);

        let vapid = self.vapid_signature(&subscription_info)?;

        let session_id = payload.session_id.to_string();
        let body = WebPushBody {
            session_id: &session_id,
            event_kind: &payload.event_kind,
            title: &payload.title,
            body: &payload.body,
            tag: &payload.collapse_key,
        };
        let body = serde_json::to_vec(&body)
            .map_err(|e| PushError::Transport(format!("payload serialization failed: {e}")))?;

        let mut builder = WebPushMessageBuilder::new(&subscription_info);
        builder.set_ttl(PUSH_TTL_SECS);
        builder.set_payload(ContentEncoding::Aes128Gcm, &body);
        builder.set_vapid_signature(vapid);
        builder
            .build()
            .map_err(|e| PushError::Transport(format!("web push message build failed: {e}")))
    }

    /// VAPID-sign against a subscription. Accepts either a PEM private key (has
    /// a `BEGIN` header) or a URL-safe base64 raw key — `scripts/generate-vapid-keys.sh`
    /// emits the latter.
    fn vapid_signature(
        &self,
        subscription_info: &SubscriptionInfo,
    ) -> Result<web_push::VapidSignature, PushError> {
        let key = self.vapid_private_key.trim();
        let builder = if key.contains("BEGIN") {
            VapidSignatureBuilder::from_pem(key.as_bytes(), subscription_info)
        } else {
            VapidSignatureBuilder::from_base64(key, subscription_info)
        }
        .map_err(|e| PushError::Transport(format!("invalid VAPID private key: {e}")))?;
        builder
            .build()
            .map_err(|e| PushError::Transport(format!("VAPID signing failed: {e}")))
    }

    /// POST a built message over reqwest, replicating the headers web-push's own
    /// clients set, and map the HTTP status to a [`SendOutcome`].
    async fn deliver(&self, message: WebPushMessage) -> Result<SendOutcome, PushError> {
        let mut request = self
            .http
            .post(message.endpoint.to_string())
            .header("TTL", message.ttl.to_string());

        if let Some(payload) = message.payload {
            request = request
                .header(
                    reqwest::header::CONTENT_ENCODING,
                    payload.content_encoding.to_str(),
                )
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream");
            for (k, v) in payload.crypto_headers {
                request = request.header(k, v);
            }
            request = request.body(payload.content);
        }

        let response = request
            .send()
            .await
            .map_err(|e| PushError::Transport(format!("web push request failed: {e}")))?;

        let status = response.status();
        if status.is_success() {
            Ok(SendOutcome::Delivered)
        } else if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
            Ok(SendOutcome::GoneDeadEndpoint)
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(PushError::Transport(format!(
                "push service returned {status}: {body}"
            )))
        }
    }
}

impl PushTransport for WebPushTransport {
    async fn send(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<SendOutcome, PushError> {
        let message = self.build_message(sub, payload)?;
        self.deliver(message).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // A P-256 application-server private key in URL-safe base64 (no padding),
    // paired with the public key below. Test-only, generated with
    // scripts/generate-vapid-keys.sh; never used against a real push service.
    const TEST_VAPID_PRIVATE_B64: &str = "JFadqg_le_g7mMaPvDG8BKYAmMOQ16kugqTARPmwHdE";

    fn sub(platform: &str, keys: bool) -> PushSubscription {
        PushSubscription {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            platform: platform.to_string(),
            endpoint_or_token: "https://push.example.invalid/abc".to_string(),
            p256dh: keys.then(|| {
                // A valid uncompressed P-256 public point (65 bytes), base64url.
                "BAcQ_z2n8kR3l1p2q9sT4uV6wX8yZ0aB1cD2eF3gH4iJ5kL6mN7oP8qR9sT0uV1wX2yZ3aB4cD5eF6gH7iJ8k".to_string()
            }),
            auth: keys.then(|| "c3VwZXJzZWNyZXRhdXRoMTIz".to_string()),
            device_label: Some("test".to_string()),
            created_at: chrono::Utc::now(),
            last_success_at: None,
            disabled_at: None,
        }
    }

    fn payload() -> PushPayload {
        let id = Uuid::new_v4();
        PushPayload {
            session_id: id,
            event_kind: "permission_request".to_string(),
            title: "my-session".to_string(),
            body: "Permission needed: Bash".to_string(),
            collapse_key: id.to_string(),
        }
    }

    #[test]
    fn body_serializes_to_expected_typed_shape() {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let body = WebPushBody {
            session_id: &id_str,
            event_kind: "turn_complete",
            title: "sess",
            body: "Turn complete",
            tag: &id_str,
        };
        let json: serde_json::Value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(json["session_id"], id.to_string());
        assert_eq!(json["event_kind"], "turn_complete");
        assert_eq!(json["title"], "sess");
        assert_eq!(json["body"], "Turn complete");
        assert_eq!(json["tag"], id.to_string());
    }

    #[test]
    fn non_webpush_platform_is_rejected_cleanly() {
        let transport = WebPushTransport::new(TEST_VAPID_PRIVATE_B64.to_string());
        for platform in ["apns", "fcm"] {
            let err = transport
                .build_message(&sub(platform, true), &payload())
                .expect_err("native platform rows must not be sent as web push");
            let PushError::Transport(msg) = err;
            assert!(
                msg.contains(platform),
                "error should name the platform: {msg}"
            );
        }
    }

    #[test]
    fn missing_encryption_keys_error() {
        let transport = WebPushTransport::new(TEST_VAPID_PRIVATE_B64.to_string());
        let err = transport
            .build_message(&sub("webpush", false), &payload())
            .expect_err("a webpush row without p256dh/auth is malformed");
        let PushError::Transport(msg) = err;
        assert!(msg.contains("p256dh") || msg.contains("auth"), "{msg}");
    }

    #[test]
    fn invalid_vapid_key_surfaces_as_transport_error() {
        let transport = WebPushTransport::new("not-a-valid-key!!!".to_string());
        let err = transport
            .build_message(&sub("webpush", true), &payload())
            .expect_err("a bogus VAPID key must fail message construction");
        let PushError::Transport(msg) = err;
        assert!(
            msg.contains("VAPID") || msg.contains("key"),
            "error should point at the key: {msg}"
        );
    }

    // Status → SendOutcome mapping is the delivery contract (§8.3). We assert it
    // over the classification predicate directly so no network is touched.
    fn classify(status: reqwest::StatusCode) -> Result<SendOutcome, ()> {
        if status.is_success() {
            Ok(SendOutcome::Delivered)
        } else if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
            Ok(SendOutcome::GoneDeadEndpoint)
        } else {
            Err(())
        }
    }

    #[test]
    fn status_mapping_matches_delivery_policy() {
        use reqwest::StatusCode;
        assert_eq!(classify(StatusCode::OK), Ok(SendOutcome::Delivered));
        assert_eq!(classify(StatusCode::CREATED), Ok(SendOutcome::Delivered));
        assert_eq!(classify(StatusCode::ACCEPTED), Ok(SendOutcome::Delivered));
        assert_eq!(
            classify(StatusCode::NOT_FOUND),
            Ok(SendOutcome::GoneDeadEndpoint)
        );
        assert_eq!(
            classify(StatusCode::GONE),
            Ok(SendOutcome::GoneDeadEndpoint)
        );
        assert_eq!(classify(StatusCode::TOO_MANY_REQUESTS), Err(()));
        assert_eq!(classify(StatusCode::INTERNAL_SERVER_ERROR), Err(()));
        assert_eq!(classify(StatusCode::UNAUTHORIZED), Err(()));
    }
}

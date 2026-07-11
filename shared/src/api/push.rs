//! Push-notification endpoint request/response types.
//!
//! Wire contract for the push-subscription CRUD surface
//! (`/api/push/subscriptions`) and the per-user notification preferences
//! (`/api/push/prefs`). Kept WASM-compatible: only `serde`, `uuid`, and
//! `String`-encoded timestamps, so the frontend subscribe flow and settings
//! panel can build against the same types the backend serializes from.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Transport a push subscription targets.
///
/// Stage 1 ships `Webpush` (VAPID); `Apns` / `Fcm` are the native-shell
/// transports (M3). Serialized snake_case to match the `push_subscriptions`
/// `platform` column values (`'webpush' | 'apns' | 'fcm'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushPlatform {
    Webpush,
    Apns,
    Fcm,
}

impl PushPlatform {
    /// The wire/DB string for this platform — the same value serde emits, so
    /// the `push_subscriptions.platform` column stays in lockstep with the JSON
    /// representation.
    pub fn as_wire(&self) -> &'static str {
        match self {
            PushPlatform::Webpush => "webpush",
            PushPlatform::Apns => "apns",
            PushPlatform::Fcm => "fcm",
        }
    }

    /// Parse a stored/wire platform string. Returns `None` for an unrecognized
    /// value so callers can decide how to treat a legacy/corrupt row.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "webpush" => Some(PushPlatform::Webpush),
            "apns" => Some(PushPlatform::Apns),
            "fcm" => Some(PushPlatform::Fcm),
            _ => None,
        }
    }
}

/// Response from GET /api/push/vapid-key — the server's VAPID public key.
///
/// The key is the base64url-encoded (unpadded) VAPID application-server public
/// key the browser passes to `pushManager.subscribe({ applicationServerKey })`.
/// The endpoint returns 404 when the deployment has no key configured
/// (`PORTAL_VAPID_PUBLIC_KEY` unset), which the client treats as "push
/// unavailable" and degrades gracefully.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VapidKeyResponse {
    pub public_key: String,
}

/// Register (or re-register) a push subscription for the caller.
///
/// The backend upserts on the `(user_id, endpoint_or_token)` unique key:
/// re-registration refreshes the keys and clears `disabled_at`. For Web Push,
/// `endpoint_or_token` is the push endpoint URL and `p256dh` / `auth` carry the
/// subscription keys; for native transports the token lives in
/// `endpoint_or_token` and the key fields are `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterPushSubscriptionRequest {
    pub platform: PushPlatform,
    pub endpoint_or_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p256dh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
}

/// One push subscription belonging to the caller.
///
/// Timestamps are RFC3339 strings (matching the other `api` wire types) so the
/// type stays WASM-friendly without pulling chrono formatting into the
/// contract. `endpoint_or_token` and the crypto keys are intentionally omitted
/// — the client never needs them back, and they should stay server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushSubscriptionInfo {
    pub id: Uuid,
    pub platform: PushPlatform,
    #[serde(default)]
    pub device_label: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_success_at: Option<String>,
    #[serde(default)]
    pub disabled_at: Option<String>,
}

/// Response from GET /api/push/subscriptions — the caller's own subscriptions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushSubscriptionsResponse {
    #[serde(default)]
    pub subscriptions: Vec<PushSubscriptionInfo>,
}

fn default_true() -> bool {
    true
}

/// Per-user, per-event-kind notification toggles.
///
/// Defaults are `(permission_request, turn_complete, session_disconnected) =
/// true` and `agent_message = false` — the "agent is blocked on you" and
/// turn/disconnect signals are on by default, inter-agent chatter is opt-in.
/// Every field is `#[serde(default)]`-tolerant so a partial or older payload
/// (a client that predates a new toggle) still parses and adopts the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPrefs {
    #[serde(default = "default_true")]
    pub permission_request: bool,
    #[serde(default = "default_true")]
    pub turn_complete: bool,
    #[serde(default = "default_true")]
    pub session_disconnected: bool,
    #[serde(default)]
    pub agent_message: bool,
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self {
            permission_request: true,
            turn_complete: true,
            session_disconnected: true,
            agent_message: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_platform_snake_case_wire() {
        assert_eq!(
            serde_json::to_value(PushPlatform::Webpush).unwrap(),
            "webpush"
        );
        assert_eq!(serde_json::to_value(PushPlatform::Apns).unwrap(), "apns");
        assert_eq!(serde_json::to_value(PushPlatform::Fcm).unwrap(), "fcm");
        let parsed: PushPlatform = serde_json::from_str("\"fcm\"").unwrap();
        assert_eq!(parsed, PushPlatform::Fcm);
    }

    #[test]
    fn push_platform_wire_helpers_match_serde() {
        for p in [PushPlatform::Webpush, PushPlatform::Apns, PushPlatform::Fcm] {
            // as_wire must equal the serde string, and from_wire must round-trip.
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_wire());
            assert_eq!(PushPlatform::from_wire(p.as_wire()), Some(p));
        }
        assert_eq!(PushPlatform::from_wire("nope"), None);
    }

    #[test]
    fn register_request_omits_none_keys() {
        let req = RegisterPushSubscriptionRequest {
            platform: PushPlatform::Apns,
            endpoint_or_token: "device-token".to_string(),
            p256dh: None,
            auth: None,
            device_label: Some("iPhone".to_string()),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["platform"], "apns");
        assert_eq!(json["endpoint_or_token"], "device-token");
        assert!(json.get("p256dh").is_none());
        assert!(json.get("auth").is_none());
        assert_eq!(json["device_label"], "iPhone");
        let parsed: RegisterPushSubscriptionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.platform, PushPlatform::Apns);
        assert_eq!(parsed.endpoint_or_token, "device-token");
    }

    #[test]
    fn subscription_info_roundtrip() {
        let info = PushSubscriptionInfo {
            id: Uuid::nil(),
            platform: PushPlatform::Webpush,
            device_label: Some("Pixel".to_string()),
            created_at: "2026-07-11T00:00:00+00:00".to_string(),
            last_success_at: None,
            disabled_at: None,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["platform"], "webpush");
        let parsed: PushSubscriptionInfo = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, info);
    }

    #[test]
    fn notification_prefs_default_matches_spec() {
        let prefs = NotificationPrefs::default();
        assert!(prefs.permission_request);
        assert!(prefs.turn_complete);
        assert!(prefs.session_disconnected);
        assert!(!prefs.agent_message);
    }

    #[test]
    fn notification_prefs_missing_fields_adopt_defaults() {
        // Empty payload -> all defaults (true, true, true, false).
        let parsed: NotificationPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, NotificationPrefs::default());

        // A partial payload that only disables one toggle keeps the rest at
        // their defaults rather than falling to `false`.
        let parsed: NotificationPrefs =
            serde_json::from_str(r#"{"turn_complete": false}"#).unwrap();
        assert!(parsed.permission_request);
        assert!(!parsed.turn_complete);
        assert!(parsed.session_disconnected);
        assert!(!parsed.agent_message);
    }
}

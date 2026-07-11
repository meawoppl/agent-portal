//! Push-notification dispatch (mobile-apps plan §8, work items C2 + C4).
//!
//! This module is the server-side half of the "reach a pocketed phone" story:
//! event hooks around the codebase [`NotificationSender::emit`] a
//! [`NotificationEvent`]; a background dispatcher task
//! ([`dispatcher::spawn_dispatcher`]) drains those events and, per the delivery
//! policy in §8.2, resolves the owning user, suppresses the push when that user
//! already has a live web client, filters on their notification preferences,
//! and fans the surviving event out to every non-disabled push subscription
//! via a [`PushTransport`].
//!
//! v1 ships only the [`transport::LogTransport`] (logs delivery intent); real
//! Web Push / APNs / FCM transports land in C3 / C7 behind the same trait.

pub mod apns;
pub mod dispatcher;
pub mod fcm;
pub mod transport;
pub mod webpush;

pub use dispatcher::spawn_dispatcher;
pub use transport::{LogTransport, PushError, PushTransport, SendOutcome};
pub use webpush::WebPushTransport;

use crate::models::PushSubscription;
use shared::api::NotificationPrefs;
use std::path::PathBuf;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use uuid::Uuid;

/// APNs provider-token configuration. All fields must be present together; an
/// absent group leaves APNs delivery disabled while the dispatcher still runs.
#[derive(Debug, Clone)]
pub struct ApnsTransportConfig {
    pub key_p8_path: PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub bundle_id: String,
}

/// FCM v1 service-account configuration. Absent = FCM delivery disabled.
#[derive(Debug, Clone)]
pub struct FcmTransportConfig {
    pub service_account_path: PathBuf,
}

/// Native mobile push configuration (C7). Web Push is intentionally separate
/// (C3); this group only owns APNs and FCM.
#[derive(Debug, Clone, Default)]
pub struct NativePushConfig {
    pub apns: Option<ApnsTransportConfig>,
    pub fcm: Option<FcmTransportConfig>,
}

/// Startup-selected transport set. `PushTransport` is not object-safe because
/// it returns `impl Future`, so static dispatch through an enum keeps the
/// dispatcher generic-free while every real transport (Web Push / APNs / FCM)
/// stays individually optional.
pub enum ConfiguredTransport {
    /// Nothing configured: log delivery intent only.
    Log(LogTransport),
    /// At least one real transport configured. Subscriptions route by their
    /// `platform` column; platforms without a configured transport fall back
    /// to the log transport (visible intent, never an error).
    Registry {
        log: LogTransport,
        webpush: Option<Box<WebPushTransport>>,
        apns: Option<Box<apns::ApnsTransport>>,
        fcm: Option<Box<fcm::FcmTransport>>,
    },
}

impl ConfiguredTransport {
    /// Build the transport registry from startup config. `vapid_private_key`
    /// enables Web Push (C3); `native` enables APNs / FCM (C7). Missing config
    /// never panics — it just leaves that platform on the log fallback.
    pub fn from_config(
        vapid_private_key: Option<String>,
        native: NativePushConfig,
    ) -> anyhow::Result<Self> {
        let webpush = vapid_private_key.map(WebPushTransport::new).map(Box::new);
        let apns = native
            .apns
            .map(apns::ApnsTransport::new)
            .transpose()?
            .map(Box::new);
        let fcm = native
            .fcm
            .map(fcm::FcmTransport::new)
            .transpose()?
            .map(Box::new);
        tracing::info!(
            webpush = webpush.is_some(),
            apns = apns.is_some(),
            fcm = fcm.is_some(),
            "push transports configured"
        );
        if webpush.is_some() || apns.is_some() || fcm.is_some() {
            Ok(Self::Registry {
                log: LogTransport,
                webpush,
                apns,
                fcm,
            })
        } else {
            Ok(Self::Log(LogTransport))
        }
    }
}

impl PushTransport for ConfiguredTransport {
    async fn send(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<SendOutcome, PushError> {
        match self {
            ConfiguredTransport::Log(t) => t.send(sub, payload).await,
            ConfiguredTransport::Registry {
                log,
                webpush,
                apns,
                fcm,
            } => match sub.platform.as_str() {
                "webpush" => match webpush {
                    Some(t) => t.send(sub, payload).await,
                    None => log.send(sub, payload).await,
                },
                "apns" => match apns {
                    Some(t) => t.send(sub, payload).await,
                    None => log.send(sub, payload).await,
                },
                "fcm" => match fcm {
                    Some(t) => t.send(sub, payload).await,
                    None => log.send(sub, payload).await,
                },
                _ => log.send(sub, payload).await,
            },
        }
    }
}

/// A user-visible event worth a push. Each variant names the session it
/// concerns so the dispatcher can resolve the owning user and (as a fallback)
/// a display name. `session_name` is filled best-effort by the emitting hook —
/// call sites that already hold it pass it through; others leave it empty and
/// the dispatcher backfills it from the sessions row (see
/// [`dispatcher`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationEvent {
    /// The agent is blocked waiting on a permission decision — the highest
    /// value interrupt (§8.1).
    PermissionRequest {
        session_id: Uuid,
        session_name: String,
        tool_name: String,
    },
    /// A turn finished (a `Result` message was stored); collapse per session.
    TurnComplete {
        session_id: Uuid,
        session_name: String,
    },
    /// A running session dropped unexpectedly (an `active` → `disconnected`
    /// transition, never a user-requested stop/pause).
    SessionDisconnected {
        session_id: Uuid,
        session_name: String,
    },
}

impl NotificationEvent {
    /// The session this event concerns.
    pub fn session_id(&self) -> Uuid {
        match self {
            NotificationEvent::PermissionRequest { session_id, .. }
            | NotificationEvent::TurnComplete { session_id, .. }
            | NotificationEvent::SessionDisconnected { session_id, .. } => *session_id,
        }
    }

    /// Best-effort display name supplied by the emitting hook; empty when the
    /// hook didn't have it cheaply (the dispatcher then uses the DB name).
    pub fn session_name(&self) -> &str {
        match self {
            NotificationEvent::PermissionRequest { session_name, .. }
            | NotificationEvent::TurnComplete { session_name, .. }
            | NotificationEvent::SessionDisconnected { session_name, .. } => session_name,
        }
    }

    /// Stable wire tag for the event kind — mirrors the `NotificationPrefs`
    /// field names and rides in the push payload so clients can theme/route.
    pub fn event_kind(&self) -> &'static str {
        match self {
            NotificationEvent::PermissionRequest { .. } => "permission_request",
            NotificationEvent::TurnComplete { .. } => "turn_complete",
            NotificationEvent::SessionDisconnected { .. } => "session_disconnected",
        }
    }

    /// Whether this event kind is enabled under the given preferences.
    pub fn is_enabled(&self, prefs: &NotificationPrefs) -> bool {
        match self {
            NotificationEvent::PermissionRequest { .. } => prefs.permission_request,
            NotificationEvent::TurnComplete { .. } => prefs.turn_complete,
            NotificationEvent::SessionDisconnected { .. } => prefs.session_disconnected,
        }
    }

    /// Build the transport-agnostic payload. `session_name` is the resolved
    /// name (event name when the hook provided one, else the DB name). Payload
    /// discipline (§8.2): a short preview only — the tap deep-links to the
    /// session and richer content stays server-side.
    pub fn into_payload(self, session_name: String) -> PushPayload {
        let session_id = self.session_id();
        let event_kind = self.event_kind().to_string();
        // Collapse key = session id: one visible notification per session,
        // newest wins (§8.2).
        let collapse_key = session_id.to_string();
        let title = if session_name.is_empty() {
            "Agent Portal".to_string()
        } else {
            session_name
        };
        let body = match self {
            NotificationEvent::PermissionRequest { tool_name, .. } => {
                format!("Permission needed: {tool_name}")
            }
            NotificationEvent::TurnComplete { .. } => "Turn complete".to_string(),
            NotificationEvent::SessionDisconnected { .. } => "Session disconnected".to_string(),
        };
        PushPayload {
            session_id,
            event_kind,
            title,
            body,
            collapse_key,
        }
    }
}

/// A push payload, independent of transport. `collapse_key` is the session id
/// string so a transport maps it to `apns-collapse-id` / FCM `collapse_key` /
/// Web Push `tag` for one-notification-per-session collapsing (§8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPayload {
    pub session_id: Uuid,
    pub event_kind: String,
    pub title: String,
    pub body: String,
    pub collapse_key: String,
}

/// Non-blocking, infallible-from-the-caller's-perspective handle for emitting
/// [`NotificationEvent`]s. Cheap to clone; stored on `AppState` and threaded
/// into the event hooks. A send failure (the dispatcher task is gone) is
/// logged at debug and swallowed — a hook must never block or fail its calling
/// path on notification delivery.
#[derive(Clone)]
pub struct NotificationSender {
    tx: UnboundedSender<NotificationEvent>,
}

impl NotificationSender {
    /// Emit an event. Errors (channel closed) are logged and dropped.
    pub fn emit(&self, event: NotificationEvent) {
        if let Err(e) = self.tx.send(event) {
            tracing::debug!("push notification channel closed, dropping event: {e}");
        }
    }
}

/// Create the notification channel: a [`NotificationSender`] for the hooks and
/// the receiver the dispatcher task drains.
pub fn channel() -> (NotificationSender, UnboundedReceiver<NotificationEvent>) {
    let (tx, rx) = unbounded_channel();
    (NotificationSender { tx }, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perm(name: &str) -> NotificationEvent {
        NotificationEvent::PermissionRequest {
            session_id: Uuid::nil(),
            session_name: name.to_string(),
            tool_name: "Bash".to_string(),
        }
    }

    #[test]
    fn event_kind_matches_prefs_fields() {
        assert_eq!(perm("s").event_kind(), "permission_request");
        assert_eq!(
            NotificationEvent::TurnComplete {
                session_id: Uuid::nil(),
                session_name: "s".into()
            }
            .event_kind(),
            "turn_complete"
        );
        assert_eq!(
            NotificationEvent::SessionDisconnected {
                session_id: Uuid::nil(),
                session_name: "s".into()
            }
            .event_kind(),
            "session_disconnected"
        );
    }

    #[test]
    fn default_prefs_enable_the_three_v1_events() {
        let prefs = NotificationPrefs::default();
        for ev in [
            perm("s"),
            NotificationEvent::TurnComplete {
                session_id: Uuid::nil(),
                session_name: "s".into(),
            },
            NotificationEvent::SessionDisconnected {
                session_id: Uuid::nil(),
                session_name: "s".into(),
            },
        ] {
            assert!(
                ev.is_enabled(&prefs),
                "{} should be on by default",
                ev.event_kind()
            );
        }
    }

    #[test]
    fn is_enabled_honors_a_disabled_toggle() {
        let prefs = NotificationPrefs {
            turn_complete: false,
            ..NotificationPrefs::default()
        };
        assert!(perm("s").is_enabled(&prefs));
        assert!(!NotificationEvent::TurnComplete {
            session_id: Uuid::nil(),
            session_name: "s".into()
        }
        .is_enabled(&prefs));
    }

    #[test]
    fn payload_uses_session_name_as_title_and_session_id_collapse_key() {
        let id = Uuid::new_v4();
        let ev = NotificationEvent::PermissionRequest {
            session_id: id,
            session_name: "my-session".into(),
            tool_name: "Edit".into(),
        };
        let payload = ev.into_payload("my-session".to_string());
        assert_eq!(payload.title, "my-session");
        assert_eq!(payload.body, "Permission needed: Edit");
        assert_eq!(payload.event_kind, "permission_request");
        assert_eq!(payload.collapse_key, id.to_string());
        assert_eq!(payload.session_id, id);
    }

    #[test]
    fn payload_falls_back_to_generic_title_when_name_empty() {
        let payload = NotificationEvent::TurnComplete {
            session_id: Uuid::nil(),
            session_name: String::new(),
        }
        .into_payload(String::new());
        assert_eq!(payload.title, "Agent Portal");
        assert_eq!(payload.body, "Turn complete");
    }

    #[test]
    fn emit_after_receiver_dropped_is_silent() {
        let (sender, rx) = channel();
        drop(rx);
        // Must not panic / must not block.
        sender.emit(perm("s"));
    }
}

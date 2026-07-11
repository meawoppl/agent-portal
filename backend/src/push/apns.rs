//! Native APNs transport (mobile-apps plan C7).
//!
//! Uses direct APNs provider-token auth via the `a2` crate. The shell stores
//! the APNs device token in `push_subscriptions.endpoint_or_token`.

use std::fs::File;

use a2::{
    Client, ClientConfig, CollapseId, DefaultNotificationBuilder, Endpoint, ErrorReason,
    NotificationBuilder, NotificationOptions, PushType,
};
use serde::Serialize;

use crate::models::PushSubscription;
use crate::push::transport::{PushError, PushTransport, SendOutcome};
use crate::push::{ApnsTransportConfig, PushPayload};

const APNS_PLATFORM: &str = "apns";

#[derive(Debug, Serialize)]
struct ApnsCustomData<'a> {
    session_id: &'a str,
    event_kind: &'a str,
}

pub struct ApnsTransport {
    client: Client,
    bundle_id: String,
}

impl ApnsTransport {
    pub fn new(config: ApnsTransportConfig) -> anyhow::Result<Self> {
        let mut key = File::open(&config.key_p8_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to open PORTAL_APNS_KEY_P8_PATH {}: {e}",
                config.key_p8_path.display()
            )
        })?;
        let client = Client::token(
            &mut key,
            config.key_id,
            config.team_id,
            ClientConfig::new(Endpoint::Production),
        )
        .map_err(|e| anyhow::anyhow!("failed to initialize APNs client: {e}"))?;
        Ok(Self {
            client,
            bundle_id: config.bundle_id,
        })
    }

    fn build_payload<'a>(
        &'a self,
        sub: &'a PushSubscription,
        payload: &'a PushPayload,
    ) -> Result<a2::request::payload::Payload<'a>, PushError> {
        if sub.platform != APNS_PLATFORM {
            return Err(PushError::Transport(format!(
                "ApnsTransport cannot deliver to platform {:?} (subscription {})",
                sub.platform, sub.id
            )));
        }

        let collapse_id = CollapseId::new(&payload.collapse_key).map_err(|e| {
            PushError::Transport(format!(
                "invalid APNs collapse id for subscription {}: {e}",
                sub.id
            ))
        })?;
        let options = NotificationOptions {
            apns_topic: Some(self.bundle_id.as_str()),
            apns_push_type: Some(PushType::Alert),
            apns_collapse_id: Some(collapse_id),
            ..Default::default()
        };

        let mut notification = DefaultNotificationBuilder::new()
            .set_title(&payload.title)
            .set_body(&payload.body)
            .set_sound("default")
            .build(&sub.endpoint_or_token, options);
        let session_id = payload.session_id.to_string();
        notification
            .add_custom_data(
                "agent_portal",
                &ApnsCustomData {
                    session_id: &session_id,
                    event_kind: &payload.event_kind,
                },
            )
            .map_err(|e| {
                PushError::Transport(format!(
                    "APNs payload serialization failed for subscription {}: {e}",
                    sub.id
                ))
            })?;
        Ok(notification)
    }

    fn map_send_error(err: a2::Error) -> Result<SendOutcome, PushError> {
        match err {
            a2::Error::ResponseError(response) => {
                let reason = response.error.as_ref().map(|e| &e.reason);
                if response.code == 410
                    || matches!(
                        reason,
                        Some(
                            ErrorReason::Unregistered
                                | ErrorReason::BadDeviceToken
                                | ErrorReason::DeviceTokenNotForTopic
                        )
                    )
                {
                    Ok(SendOutcome::GoneDeadEndpoint)
                } else {
                    Err(PushError::Transport(format!(
                        "APNs returned {}: {:?}",
                        response.code, response.error
                    )))
                }
            }
            other => Err(PushError::Transport(format!(
                "APNs request failed: {other}"
            ))),
        }
    }
}

impl PushTransport for ApnsTransport {
    async fn send(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<SendOutcome, PushError> {
        let notification = self.build_payload(sub, payload)?;
        self.client
            .send(notification)
            .await
            .map(|_| SendOutcome::Delivered)
            .or_else(Self::map_send_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2::{ErrorBody, Response};
    use chrono::Utc;
    use uuid::Uuid;

    const TEST_APNS_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg8g/n6j9roKvnUkwu
lCEIvbDqlUhA5FOzcakkG90E8L+hRANCAATKS2ZExEybUvchRDuKBftotMwVEus3
jDwmlD1Gg0yJt1e38djFwsxsfr5q2hv0Rj9fTEqAPr8H7mGm0wKxZ7iQ
-----END PRIVATE KEY-----";

    fn test_client() -> Client {
        Client::token(
            std::io::Cursor::new(TEST_APNS_KEY.as_bytes()),
            "KEYID",
            "TEAMID",
            ClientConfig::default(),
        )
        .unwrap()
    }

    fn sub(platform: &str) -> PushSubscription {
        PushSubscription {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            platform: platform.to_string(),
            endpoint_or_token: "apns-token".to_string(),
            p256dh: None,
            auth: None,
            device_label: Some("ios".to_string()),
            created_at: Utc::now(),
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

    fn response_error(code: u16, reason: ErrorReason) -> a2::Error {
        a2::Error::ResponseError(Response {
            apns_id: None,
            code,
            error: Some(ErrorBody {
                reason,
                timestamp: None,
            }),
        })
    }

    #[test]
    fn apns_payload_contains_collapse_topic_and_custom_data() {
        let transport = ApnsTransport {
            client: test_client(),
            bundle_id: "io.txcl.agentportal".to_string(),
        };
        let payload = payload();
        let sub = sub(APNS_PLATFORM);
        let notification = transport
            .build_payload(&sub, &payload)
            .expect("payload builds");
        let json = serde_json::to_value(&notification).expect("serialize notification");
        assert_eq!(json["aps"]["alert"]["title"], payload.title);
        assert_eq!(json["aps"]["alert"]["body"], payload.body);
        assert_eq!(
            json["agent_portal"]["session_id"],
            payload.session_id.to_string()
        );
        assert_eq!(json["agent_portal"]["event_kind"], payload.event_kind);
        assert_eq!(notification.options.apns_topic, Some("io.txcl.agentportal"));
        assert_eq!(
            notification
                .options
                .apns_collapse_id
                .as_ref()
                .map(|id| id.value),
            Some(payload.collapse_key.as_str())
        );
    }

    #[test]
    fn non_apns_platform_rejected_before_network() {
        let transport = ApnsTransport {
            client: test_client(),
            bundle_id: "io.txcl.agentportal".to_string(),
        };
        let err = transport
            .build_payload(&sub("fcm"), &payload())
            .expect_err("wrong platform rejected");
        let PushError::Transport(msg) = err;
        assert!(msg.contains("fcm"));
    }

    #[test]
    fn apns_unregistered_maps_to_dead_endpoint() {
        assert_eq!(
            ApnsTransport::map_send_error(response_error(410, ErrorReason::Unregistered))
                .expect("mapped"),
            SendOutcome::GoneDeadEndpoint
        );
        assert_eq!(
            ApnsTransport::map_send_error(response_error(400, ErrorReason::BadDeviceToken))
                .expect("mapped"),
            SendOutcome::GoneDeadEndpoint
        );
    }

    #[test]
    fn apns_non_dead_error_stays_transport_error() {
        let err =
            ApnsTransport::map_send_error(response_error(500, ErrorReason::InternalServerError))
                .expect_err("not dead endpoint");
        let PushError::Transport(msg) = err;
        assert!(msg.contains("APNs returned 500"));
    }
}

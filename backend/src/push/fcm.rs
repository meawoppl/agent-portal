//! Native FCM v1 transport (mobile-apps plan C7).
//!
//! Android shell subscriptions store the FCM registration token in
//! `push_subscriptions.endpoint_or_token`. This transport authenticates with a
//! Google service-account JSON file and sends HTTP v1 messages directly.

use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::models::PushSubscription;
use crate::push::transport::{PushError, PushTransport, SendOutcome};
use crate::push::{FcmTransportConfig, PushPayload};
use shared::api::PushPlatform;

const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const FCM_TOKEN_REFRESH_SLOP: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct ServiceAccountFile {
    project_id: String,
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Debug, Serialize)]
struct ServiceAccountClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    token_type: String,
    expires_in: u64,
}

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

#[derive(Debug, Serialize)]
struct FcmNotification {
    title: String,
    body: String,
}

#[derive(Debug, Serialize)]
struct FcmAndroid {
    collapse_key: String,
}

#[derive(Debug, Serialize)]
struct FcmMessage {
    token: String,
    notification: FcmNotification,
    data: FcmData,
    android: FcmAndroid,
}

#[derive(Debug, Serialize)]
struct FcmData {
    session_id: String,
    event_kind: String,
}

#[derive(Debug, Serialize)]
struct FcmRequest {
    message: FcmMessage,
}

pub struct FcmTransport {
    service_account: ServiceAccountFile,
    encoding_key: EncodingKey,
    http: reqwest::Client,
    token: Mutex<Option<CachedToken>>,
    endpoint: String,
}

impl FcmTransport {
    pub fn new(config: FcmTransportConfig) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(&config.service_account_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to read PORTAL_FCM_SERVICE_ACCOUNT_PATH {}: {e}",
                config.service_account_path.display()
            )
        })?;
        let service_account: ServiceAccountFile = serde_json::from_str(&contents).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse PORTAL_FCM_SERVICE_ACCOUNT_PATH {}: {e}",
                config.service_account_path.display()
            )
        })?;
        let encoding_key = EncodingKey::from_rsa_pem(service_account.private_key.as_bytes())
            .map_err(|e| anyhow::anyhow!("invalid FCM service-account private_key: {e}"))?;
        Ok(Self::from_parts(
            service_account,
            encoding_key,
            reqwest::Client::new(),
        ))
    }

    fn from_parts(
        service_account: ServiceAccountFile,
        encoding_key: EncodingKey,
        http: reqwest::Client,
    ) -> Self {
        let endpoint = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            service_account.project_id
        );
        Self {
            service_account,
            encoding_key,
            http,
            token: Mutex::new(None),
            endpoint,
        }
    }

    fn build_request(
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<FcmRequest, PushError> {
        // Reject non-FCM rows (and any legacy/unknown platform) before building
        // the request. `platform_kind()` is the typed read boundary.
        if sub.platform_kind() != Some(PushPlatform::Fcm) {
            return Err(PushError::Transport(format!(
                "FcmTransport cannot deliver to platform {:?} (subscription {})",
                sub.platform, sub.id
            )));
        }
        Ok(FcmRequest {
            message: FcmMessage {
                token: sub.endpoint_or_token.clone(),
                notification: FcmNotification {
                    title: payload.title.clone(),
                    body: payload.body.clone(),
                },
                data: FcmData {
                    session_id: payload.session_id.to_string(),
                    event_kind: payload.event_kind.clone(),
                },
                android: FcmAndroid {
                    collapse_key: payload.collapse_key.clone(),
                },
            },
        })
    }

    async fn access_token(&self) -> Result<String, PushError> {
        let mut guard = self.token.lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.expires_at > Instant::now() + FCM_TOKEN_REFRESH_SLOP {
                return Ok(cached.access_token.clone());
            }
        }

        let token = self.fetch_access_token().await?;
        let expires_at = Instant::now()
            .checked_add(Duration::from_secs(token.expires_in))
            .unwrap_or_else(Instant::now);
        let access_token = token.access_token;
        *guard = Some(CachedToken {
            access_token: access_token.clone(),
            expires_at,
        });
        Ok(access_token)
    }

    async fn fetch_access_token(&self) -> Result<TokenResponse, PushError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| PushError::Transport(format!("system clock before epoch: {e}")))?
            .as_secs();
        let claims = ServiceAccountClaims {
            iss: &self.service_account.client_email,
            scope: FCM_SCOPE,
            aud: &self.service_account.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let assertion = encode(&Header::new(Algorithm::RS256), &claims, &self.encoding_key)
            .map_err(|e| PushError::Transport(format!("FCM service-account JWT failed: {e}")))?;

        let response = self
            .http
            .post(&self.service_account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| PushError::Transport(format!("FCM token request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(PushError::Transport(format!(
                "FCM token endpoint returned {status}: {body}"
            )));
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| PushError::Transport(format!("FCM token response parse failed: {e}")))?;
        if !token.token_type.is_empty() && !token.token_type.eq_ignore_ascii_case("bearer") {
            return Err(PushError::Transport(format!(
                "FCM token endpoint returned unsupported token_type {:?}",
                token.token_type
            )));
        }
        Ok(token)
    }

    fn map_send_response(status: StatusCode, body: &str) -> Result<SendOutcome, PushError> {
        if status.is_success() {
            Ok(SendOutcome::Delivered)
        } else if status == StatusCode::NOT_FOUND
            || status == StatusCode::GONE
            || body.contains("UNREGISTERED")
        {
            Ok(SendOutcome::GoneDeadEndpoint)
        } else {
            Err(PushError::Transport(format!(
                "FCM returned {status}: {body}"
            )))
        }
    }
}

impl PushTransport for FcmTransport {
    async fn send(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<SendOutcome, PushError> {
        let request = Self::build_request(sub, payload)?;
        let access_token = self.access_token().await?;
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(access_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| PushError::Transport(format!("FCM send failed: {e}")))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Self::map_send_response(status, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn sub(platform: &str) -> PushSubscription {
        PushSubscription {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            platform: platform.to_string(),
            endpoint_or_token: "fcm-registration-token".to_string(),
            p256dh: None,
            auth: None,
            device_label: Some("android".to_string()),
            created_at: Utc::now(),
            last_success_at: None,
            disabled_at: None,
        }
    }

    fn payload() -> PushPayload {
        let id = Uuid::new_v4();
        PushPayload {
            session_id: id,
            event_kind: "turn_complete".to_string(),
            title: "my-session".to_string(),
            body: "Turn complete".to_string(),
            collapse_key: id.to_string(),
        }
    }

    #[test]
    fn fcm_request_serializes_typed_payload_and_collapse_key() {
        let payload = payload();
        let request = FcmTransport::build_request(&sub(PushPlatform::Fcm.as_wire()), &payload)
            .expect("request builds");
        let json = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json["message"]["token"], "fcm-registration-token");
        assert_eq!(json["message"]["notification"]["title"], payload.title);
        assert_eq!(json["message"]["notification"]["body"], payload.body);
        assert_eq!(
            json["message"]["data"]["session_id"],
            payload.session_id.to_string()
        );
        assert_eq!(json["message"]["data"]["event_kind"], payload.event_kind);
        assert_eq!(
            json["message"]["android"]["collapse_key"],
            payload.collapse_key
        );
    }

    #[test]
    fn non_fcm_platform_rejected_before_network() {
        let err = FcmTransport::build_request(&sub("apns"), &payload())
            .expect_err("wrong platform rejected");
        let PushError::Transport(msg) = err;
        assert!(msg.contains("apns"));
    }

    #[test]
    fn fcm_status_mapping_matches_dead_endpoint_contract() {
        assert_eq!(
            FcmTransport::map_send_response(StatusCode::OK, "{}").expect("mapped"),
            SendOutcome::Delivered
        );
        assert_eq!(
            FcmTransport::map_send_response(StatusCode::NOT_FOUND, "{}").expect("mapped"),
            SendOutcome::GoneDeadEndpoint
        );
        assert_eq!(
            FcmTransport::map_send_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":{"details":[{"errorCode":"UNREGISTERED"}]}}"#,
            )
            .expect("mapped"),
            SendOutcome::GoneDeadEndpoint
        );
    }

    #[test]
    fn fcm_non_dead_error_stays_transport_error() {
        let err = FcmTransport::map_send_response(StatusCode::UNAUTHORIZED, "bad auth")
            .expect_err("not dead endpoint");
        let PushError::Transport(msg) = err;
        assert!(msg.contains("401"));
    }
}

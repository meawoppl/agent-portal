//! Native APNs transport over the backend's maintained HTTP/TLS stack.
//!
//! Apple requires HTTP/2, an ES256 provider token no older than one hour, and
//! token reuse (refreshing more often than 20 minutes can be throttled).
//! See https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns
//! Reusing reqwest removes a2's separate, obsolete rustls 0.22 dependency.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::models::PushSubscription;
use crate::push::transport::{PushError, PushTransport, SendOutcome};
use crate::push::{ApnsTransportConfig, PushPayload};
use shared::api::PushPlatform;

const PROVIDER_TOKEN_LIFETIME: u64 = 50 * 60;
const MAX_PAYLOAD_BYTES: usize = 4096;
const MAX_ERROR_BYTES: usize = 8192;

#[derive(Serialize)]
struct ProviderClaims<'a> {
    iss: &'a str,
    iat: u64,
}

struct CachedToken {
    issued_at: u64,
    token: String,
}

#[derive(Debug, Serialize)]
struct ApnsCustomData<'a> {
    session_id: String,
    event_kind: &'a str,
}

#[derive(Debug, Serialize)]
struct ApnsAlert<'a> {
    title: &'a str,
    body: &'a str,
}

#[derive(Debug, Serialize)]
struct ApnsAps<'a> {
    alert: ApnsAlert<'a>,
    sound: &'static str,
}

#[derive(Debug, Serialize)]
struct ApnsBody<'a> {
    aps: ApnsAps<'a>,
    agent_portal: ApnsCustomData<'a>,
}

#[derive(Deserialize)]
struct ApnsErrorBody {
    reason: String,
}

pub struct ApnsTransport {
    http: reqwest::Client,
    bundle_id: String,
    key_id: String,
    team_id: String,
    encoding_key: EncodingKey,
    token: Mutex<Option<CachedToken>>,
    endpoint: String,
}

impl ApnsTransport {
    pub fn new(config: ApnsTransportConfig) -> anyhow::Result<Self> {
        let key = std::fs::read(&config.key_p8_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to read PORTAL_APNS_KEY_P8_PATH {}: {e}",
                config.key_p8_path.display()
            )
        })?;
        let encoding_key = EncodingKey::from_ec_pem(&key)
            .map_err(|e| anyhow::anyhow!("invalid APNs EC private key: {e}"))?;
        let http = Self::http_client()?;
        Ok(Self {
            http,
            bundle_id: config.bundle_id,
            key_id: config.key_id,
            team_id: config.team_id,
            encoding_key,
            token: Mutex::new(None),
            endpoint: "https://api.push.apple.com".into(),
        })
    }

    fn http_client() -> Result<reqwest::Client, reqwest::Error> {
        reqwest::Client::builder()
            .http2_prior_knowledge()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
    }

    fn provider_token(&self, now: u64) -> Result<String, PushError> {
        let mut guard = self
            .token
            .lock()
            .map_err(|_| PushError::Transport("APNs token cache poisoned".into()))?;
        if let Some(cached) = guard.as_ref() {
            if now >= cached.issued_at && now - cached.issued_at < PROVIDER_TOKEN_LIFETIME {
                return Ok(cached.token.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let token = encode(
            &header,
            &ProviderClaims {
                iss: &self.team_id,
                iat: now,
            },
            &self.encoding_key,
        )
        .map_err(|e| PushError::Transport(format!("APNs provider-token signing failed: {e}")))?;
        *guard = Some(CachedToken {
            issued_at: now,
            token: token.clone(),
        });
        Ok(token)
    }

    fn build_payload(sub: &PushSubscription, payload: &PushPayload) -> Result<Vec<u8>, PushError> {
        if sub.platform_kind() != Some(PushPlatform::Apns) {
            return Err(PushError::Transport(format!(
                "ApnsTransport cannot deliver to platform {:?} (subscription {})",
                sub.platform, sub.id
            )));
        }
        // Device tokens are hexadecimal bytes, not arbitrary URL paths. Keeping
        // validation here prevents a stored token from changing the request URL.
        if sub.endpoint_or_token.is_empty()
            || !sub.endpoint_or_token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(PushError::Transport("invalid APNs device token".into()));
        }
        if payload.collapse_key.len() > 64 {
            return Err(PushError::Transport(
                "APNs collapse id exceeds 64 bytes".into(),
            ));
        }
        let body = ApnsBody {
            aps: ApnsAps {
                alert: ApnsAlert {
                    title: &payload.title,
                    body: &payload.body,
                },
                sound: "default",
            },
            agent_portal: ApnsCustomData {
                session_id: payload.session_id.to_string(),
                event_kind: &payload.event_kind,
            },
        };
        let bytes = serde_json::to_vec(&body)
            .map_err(|e| PushError::Transport(format!("APNs payload serialization failed: {e}")))?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(PushError::Transport(
                "APNs payload exceeds 4096 bytes".into(),
            ));
        }
        Ok(bytes)
    }

    fn map_response(status: reqwest::StatusCode, body: &[u8]) -> Result<SendOutcome, PushError> {
        if status.is_success() {
            return Ok(SendOutcome::Delivered);
        }
        let reason = serde_json::from_slice::<ApnsErrorBody>(body)
            .ok()
            .map(|b| b.reason);
        if status == reqwest::StatusCode::GONE
            || matches!(
                reason.as_deref(),
                Some("Unregistered" | "BadDeviceToken" | "DeviceTokenNotForTopic")
            )
        {
            return Ok(SendOutcome::GoneDeadEndpoint);
        }
        Err(PushError::Transport(format!(
            "APNs returned {}: {}",
            status.as_u16(),
            reason.as_deref().unwrap_or("invalid error response")
        )))
    }
}

impl PushTransport for ApnsTransport {
    async fn send(
        &self,
        sub: &PushSubscription,
        payload: &PushPayload,
    ) -> Result<SendOutcome, PushError> {
        let body = Self::build_payload(sub, payload)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| PushError::Transport(format!("APNs clock error: {e}")))?
            .as_secs();
        let token = self.provider_token(now)?;
        let mut response = self
            .http
            .post(format!(
                "{}/3/device/{}",
                self.endpoint, sub.endpoint_or_token
            ))
            .bearer_auth(token)
            .header("apns-topic", &self.bundle_id)
            .header("apns-push-type", "alert")
            .header("apns-collapse-id", &payload.collapse_key)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| PushError::Transport(format!("APNs request failed: {e}")))?;
        let status = response.status();
        if status.is_success() {
            return Ok(SendOutcome::Delivered);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| PushError::Transport(format!("APNs response failed: {e}")))?
        {
            if body.len() + chunk.len() > MAX_ERROR_BYTES {
                return Err(PushError::Transport(
                    "APNs error response exceeds size limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Self::map_response(status, &body)
    }
}

#[cfg(test)]
mod tests;

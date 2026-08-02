//! Google Cloud Speech-to-Text v1 (`speech:recognize`).
//!
//! The synchronous endpoint, which takes the audio inline as base64 and is
//! capped at roughly a minute — the same ceiling the voice UI already imposes
//! on a single utterance.
//!
//! Biasing is real here: `speechContexts.phrases` is exactly a keyterm list.
//!
//! Auth is a service-account JWT exchanged for an access token, the same dance
//! `push::fcm` does for FCM. Tokens are cached until shortly before expiry —
//! minting one per utterance would add a round trip to every recording.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::config::{resolve_language, Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::{SttError, TranscribeRequest};

const ENDPOINT: &str = "https://speech.googleapis.com/v1/speech:recognize";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const DEFAULT_LANGUAGE: &str = "en-US";
/// Short-form model, tuned for utterances rather than long recordings.
const DEFAULT_MODEL: &str = "latest_short";
/// How much boost to give keyterms. Google's scale runs 0-20; the docs warn
/// that a high boost makes false positives more likely, so this sits mid-range.
const PHRASE_BOOST: f32 = 12.0;
/// Refresh this long before the token actually expires, so a request never
/// races the boundary.
const TOKEN_SKEW: Duration = Duration::from_secs(60);

#[derive(Clone, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Serialize)]
struct JwtClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Deserialize)]
struct RecognizeBody {
    #[serde(default)]
    results: Vec<RecognizeResult>,
}

#[derive(Deserialize)]
struct RecognizeResult {
    #[serde(default)]
    alternatives: Vec<RecognizeAlternative>,
}

#[derive(Deserialize)]
struct RecognizeAlternative {
    #[serde(default)]
    transcript: String,
}

#[derive(Clone)]
pub(crate) struct GoogleStt {
    service_account: ServiceAccount,
    language: Option<String>,
    model: String,
    http: reqwest::Client,
    cached_token: std::sync::Arc<tokio::sync::Mutex<Option<CachedToken>>>,
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: SystemTime,
}

impl GoogleStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        let path = env.require(Field::ServiceAccountPath, "google")?;
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("could not read {path}: {e}"))?;
        let service_account: ServiceAccount = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("{path} is not a service-account JSON file: {e}"))?;

        Ok(Self {
            service_account,
            language: env.language.clone(),
            model: env.model_or(DEFAULT_MODEL),
            http: reqwest::Client::new(),
            cached_token: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub(crate) async fn transcribe(
        &self,
        request: TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        let token = self.access_token().await?;
        let language =
            resolve_language(request.language, self.language.as_deref(), DEFAULT_LANGUAGE);

        let mut config = serde_json::json!({
            "languageCode": language,
            "enableAutomaticPunctuation": true,
            "model": self.model,
        });
        if let Some(encoding) = encoding_for(request.content_type) {
            config["encoding"] = serde_json::json!(encoding);
        }
        if !request.keyterms.is_empty() {
            config["speechContexts"] = serde_json::json!([{
                "phrases": request.keyterms,
                "boost": PHRASE_BOOST,
            }]);
        }

        let body = serde_json::json!({
            "config": config,
            "audio": { "content": base64::engine::general_purpose::STANDARD.encode(&request.audio) },
        });

        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(transport)?;

        let body: RecognizeBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(joined_transcript(&body))
    }

    /// A cached access token, minting a new one when absent or near expiry.
    async fn access_token(&self) -> Result<String, SttError> {
        let mut cached = self.cached_token.lock().await;
        if let Some(token) = cached.as_ref() {
            if SystemTime::now() < token.expires_at {
                return Ok(token.value.clone());
            }
        }

        let assertion = self.signed_assertion()?;
        let response = self
            .http
            .post(&self.service_account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &assertion),
            ])
            .send()
            .await
            .map_err(transport)?;
        let token: TokenResponse = ensure_ok(response).await?.json().await.map_err(decode)?;

        let lifetime = Duration::from_secs(token.expires_in.max(60));
        *cached = Some(CachedToken {
            value: token.access_token.clone(),
            expires_at: SystemTime::now() + lifetime.saturating_sub(TOKEN_SKEW),
        });
        Ok(token.access_token)
    }

    fn signed_assertion(&self) -> Result<String, SttError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SttError::Provider(format!("system clock before epoch: {e}")))?
            .as_secs();
        let claims = JwtClaims {
            iss: &self.service_account.client_email,
            scope: SCOPE,
            aud: &self.service_account.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let key =
            jsonwebtoken::EncodingKey::from_rsa_pem(self.service_account.private_key.as_bytes())
                .map_err(|e| {
                    SttError::Provider(format!("service-account private key is unusable: {e}"))
                })?;
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map_err(|e| SttError::Provider(format!("could not sign the auth assertion: {e}")))
    }
}

/// Google wants the container named when it cannot infer one. Formats whose
/// headers already describe the stream are left unset, which is what the API
/// documents as the safe default.
fn encoding_for(content_type: &str) -> Option<&'static str> {
    match content_type.split(';').next().unwrap_or("").trim() {
        "audio/webm" => Some("WEBM_OPUS"),
        "audio/ogg" => Some("OGG_OPUS"),
        "audio/mpeg" => Some("MP3"),
        "audio/flac" => Some("FLAC"),
        _ => None,
    }
}

/// Google splits long audio into consecutive results; the transcript is their
/// concatenation, each with its own best alternative.
fn joined_transcript(body: &RecognizeBody) -> String {
    body.results
        .iter()
        .filter_map(|r| r.alternatives.first())
        .map(|a| a.transcript.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> RecognizeBody {
        serde_json::from_str(json).expect("valid Google body")
    }

    #[test]
    fn concatenates_consecutive_results() {
        let body = parse(
            r#"{"results":[
                {"alternatives":[{"transcript":"run cargo clippy"}]},
                {"alternatives":[{"transcript":"across the workspace"}]}]}"#,
        );
        assert_eq!(
            joined_transcript(&body),
            "run cargo clippy across the workspace"
        );
    }

    #[test]
    fn takes_only_the_best_alternative_of_each_result() {
        let body = parse(
            r#"{"results":[{"alternatives":[
                {"transcript":"clippy"},{"transcript":"clippie"}]}]}"#,
        );
        assert_eq!(joined_transcript(&body), "clippy");
    }

    #[test]
    fn silence_yields_an_empty_transcript() {
        assert_eq!(joined_transcript(&parse(r#"{"results":[]}"#)), "");
        assert_eq!(joined_transcript(&parse(r#"{}"#)), "");
    }

    #[test]
    fn browser_containers_are_named_explicitly() {
        assert_eq!(encoding_for("audio/webm;codecs=opus"), Some("WEBM_OPUS"));
        assert_eq!(encoding_for("audio/ogg"), Some("OGG_OPUS"));
        // WAV carries its own header; Google infers it.
        assert_eq!(encoding_for("audio/wav"), None);
    }

    #[test]
    fn google_requires_a_service_account() {
        let env = SttEnv {
            api_key: Some("k".into()),
            ..Default::default()
        };
        let err = crate::config_error(GoogleStt::from_env(&env));
        assert!(err.contains("PORTAL_STT_SERVICE_ACCOUNT_PATH"), "{err}");
    }

    #[test]
    fn a_missing_service_account_file_is_reported_with_its_path() {
        let env = SttEnv {
            service_account_path: Some("/nonexistent/sa.json".into()),
            ..Default::default()
        };
        let err = crate::config_error(GoogleStt::from_env(&env));
        assert!(err.contains("/nonexistent/sa.json"), "{err}");
    }
}

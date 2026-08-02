//! Simplismart / SimpliScribe — a finetuned Whisper behind a JSON endpoint.
//!
//! Audio goes up base64-encoded inside the JSON body rather than as bytes or a
//! multipart part. The host is deployment-specific, so `PORTAL_STT_ENDPOINT`
//! overrides the published default — self-hosted and dedicated deployments each
//! get their own.
//!
//! **Keyterms are not sent**: the inference API exposes no vocabulary or
//! initial-prompt parameter, so there is nowhere to put them.

use base64::Engine;
use serde::Deserialize;

use crate::config::{resolve_language, Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::{SttError, TranscribeRequest};

const DEFAULT_ENDPOINT: &str = "https://http.whisper.proxy.prod.s9t.link";
const PATH: &str = "/model/infer/whisper";
/// Whisper takes a bare language code, not a BCP-47 tag with a region.
const DEFAULT_LANGUAGE: &str = "en";

#[derive(Clone)]
pub(crate) struct SimplismartStt {
    api_key: String,
    endpoint: String,
    language: Option<String>,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct InferBody {
    /// Segments of the transcript, in order.
    #[serde(default)]
    transcription: Vec<String>,
}

impl SimplismartStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: env.require(Field::ApiKey, "simplismart")?,
            endpoint: env.endpoint_or(DEFAULT_ENDPOINT),
            language: env.language.clone(),
            http: reqwest::Client::new(),
        })
    }

    pub(crate) async fn transcribe(
        &self,
        request: TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        let language =
            resolve_language(request.language, self.language.as_deref(), DEFAULT_LANGUAGE);
        let body = serde_json::json!({
            "audio_data": base64::engine::general_purpose::STANDARD.encode(&request.audio),
            "language": bare_language(&language),
            "task": "transcribe",
        });

        let response = self
            .http
            .post(format!("{}{PATH}", self.endpoint))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(transport)?;

        let body: InferBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(joined_transcription(&body))
    }
}

/// `en-US` → `en`. Whisper rejects region-qualified tags.
fn bare_language(language: &str) -> String {
    language
        .split(['-', '_'])
        .next()
        .unwrap_or(language)
        .to_ascii_lowercase()
}

fn joined_transcription(body: &InferBody) -> String {
    body.transcription
        .iter()
        .map(|segment| segment.trim())
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> InferBody {
        serde_json::from_str(json).expect("valid Simplismart body")
    }

    #[test]
    fn joins_transcription_segments() {
        let body = parse(r#"{"transcription":["run cargo clippy","across the workspace"]}"#);
        assert_eq!(
            joined_transcription(&body),
            "run cargo clippy across the workspace"
        );
    }

    #[test]
    fn silence_yields_an_empty_transcript() {
        assert_eq!(joined_transcription(&parse(r#"{"transcription":[]}"#)), "");
        assert_eq!(joined_transcription(&parse(r#"{"language":"en"}"#)), "");
    }

    /// The rest of the portal speaks BCP-47; Whisper does not.
    #[test]
    fn region_qualified_tags_are_reduced_to_the_language() {
        assert_eq!(bare_language("en-US"), "en");
        assert_eq!(bare_language("pt_BR"), "pt");
        assert_eq!(bare_language("FR"), "fr");
    }

    #[test]
    fn the_published_host_is_the_default_and_is_overridable() {
        let default = SimplismartStt::from_env(&SttEnv {
            api_key: Some("k".into()),
            ..Default::default()
        })
        .expect("configured");
        assert_eq!(default.endpoint, DEFAULT_ENDPOINT);

        let custom = SimplismartStt::from_env(&SttEnv {
            api_key: Some("k".into()),
            endpoint: Some("https://my-deployment.invalid".into()),
            ..Default::default()
        })
        .expect("configured");
        assert_eq!(custom.endpoint, "https://my-deployment.invalid");
    }
}

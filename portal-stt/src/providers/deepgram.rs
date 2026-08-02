//! Deepgram pre-recorded transcription (`/v1/listen`).
//!
//! Deliberately the opposite request shape from the OpenAI provider — raw body
//! rather than multipart, query parameters rather than form fields, a nested
//! response rather than a flat one — which is what makes the pair a real test
//! of the abstraction rather than two spellings of the same call.
//!
//! Bias goes in as repeated `keyterm` parameters. That is a Nova-3 feature; on
//! older models the equivalent parameter is `keywords`, so overriding
//! `PORTAL_STT_MODEL` to a pre-Nova-3 model silently loses biasing (the request
//! still succeeds — Deepgram ignores unknown parameters).

use serde::Deserialize;

use crate::config::{Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::{SttError, TranscribeRequest};

const DEFAULT_ENDPOINT: &str = "https://api.deepgram.com";
const DEFAULT_MODEL: &str = "nova-3";

#[derive(Clone)]
pub(crate) struct DeepgramStt {
    api_key: String,
    model: String,
    /// Overridable for Deepgram's self-hosted deployment.
    endpoint: String,
    language: Option<String>,
    /// Owned per provider, matching the push transports — a `Client` is a
    /// connection pool, so it must outlive individual requests.
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct ListenBody {
    results: Option<ListenResults>,
}

#[derive(Deserialize)]
struct ListenResults {
    channels: Vec<ListenChannel>,
}

#[derive(Deserialize)]
struct ListenChannel {
    alternatives: Vec<ListenAlternative>,
}

#[derive(Deserialize)]
struct ListenAlternative {
    transcript: String,
}

impl DeepgramStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: env.require(Field::ApiKey, "deepgram")?,
            model: env.model_or(DEFAULT_MODEL),
            endpoint: env.endpoint_or(DEFAULT_ENDPOINT),
            language: env.language.clone(),
            http: reqwest::Client::new(),
        })
    }

    pub(crate) async fn transcribe(
        &self,
        request: TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        // `smart_format` supplies punctuation and capitalization, which the
        // transcript needs to read as a prompt rather than a wall of words.
        let mut query: Vec<(&str, String)> = vec![
            ("model", self.model.clone()),
            ("smart_format", "true".to_string()),
        ];
        if let Some(language) = request.language.or(self.language.as_deref()) {
            query.push(("language", language.to_string()));
        }
        for term in request.keyterms {
            query.push(("keyterm", term.clone()));
        }

        let response = self
            .http
            .post(format!("{}/v1/listen", self.endpoint))
            .query(&query)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Token {}", self.api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, request.content_type)
            .body(request.audio)
            .send()
            .await
            .map_err(transport)?;

        let body: ListenBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(first_transcript(&body))
    }
}

/// Pull the best alternative out of the nested response.
///
/// Silence is a successful transcription of nothing — Deepgram answers with an
/// empty `transcript`, or occasionally no channels at all — so a missing value
/// is an empty string rather than an error.
fn first_transcript(body: &ListenBody) -> String {
    body.results
        .as_ref()
        .and_then(|r| r.channels.first())
        .and_then(|c| c.alternatives.first())
        .map(|a| a.transcript.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> ListenBody {
        serde_json::from_str(json).expect("valid Deepgram body")
    }

    #[test]
    fn reads_the_first_alternative() {
        let body = parse(
            r#"{"results":{"channels":[{"alternatives":[
                {"transcript":"run cargo clippy"},{"transcript":"run cargo clippie"}]}]}}"#,
        );
        assert_eq!(first_transcript(&body), "run cargo clippy");
    }

    /// Recording silence must read as "nothing said", not as a failure.
    #[test]
    fn silence_yields_an_empty_transcript() {
        assert_eq!(
            first_transcript(&parse(
                r#"{"results":{"channels":[{"alternatives":[{"transcript":""}]}]}}"#
            )),
            ""
        );
        assert_eq!(
            first_transcript(&parse(r#"{"results":{"channels":[]}}"#)),
            ""
        );
        assert_eq!(first_transcript(&parse(r#"{"metadata":{}}"#)), "");
    }

    #[test]
    fn the_default_model_is_used_when_none_is_configured() {
        let env = SttEnv {
            api_key: Some("k".into()),
            ..Default::default()
        };
        assert_eq!(
            DeepgramStt::from_env(&env).expect("configured").model,
            DEFAULT_MODEL
        );
    }

    #[test]
    fn deepgram_requires_a_key() {
        let err = crate::config_error(DeepgramStt::from_env(&SttEnv::default()));
        assert!(err.contains("PORTAL_STT_API_KEY"), "{err}");
    }
}

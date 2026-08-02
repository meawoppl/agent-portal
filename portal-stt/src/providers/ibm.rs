//! IBM Watson Speech to Text (`/v1/recognize`).
//!
//! Raw audio body, HTTP Basic auth with the literal username `apikey`. The
//! service URL is per-instance, so `PORTAL_STT_ENDPOINT` is required — there is
//! no global host to default to.
//!
//! **Keyterms are not sent.** Watson's `keywords` parameter is keyword
//! *spotting* — it reports where listed words occur, it does not bias decoding
//! — so passing keyterms there would cost request size and change nothing about
//! the transcript. Watson's real biasing is a trained custom language model,
//! selected through `PORTAL_STT_MODEL`.

use base64::Engine;
use serde::Deserialize;

use crate::config::{resolve_language, Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::{SttError, TranscribeRequest};

/// Watson names models `{language}_{variant}`, so the language selection is
/// folded into the model rather than being a separate parameter.
const DEFAULT_MODEL_SUFFIX: &str = "_Multimedia";
const DEFAULT_LANGUAGE: &str = "en-US";

#[derive(Clone)]
pub(crate) struct IbmStt {
    api_key: String,
    endpoint: String,
    model: Option<String>,
    language: Option<String>,
    http: reqwest::Client,
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

impl IbmStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: env.require(Field::ApiKey, "ibm")?,
            endpoint: env
                .require(Field::Endpoint, "ibm")?
                .trim_end_matches('/')
                .to_string(),
            model: env.model.clone(),
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
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| format!("{language}{DEFAULT_MODEL_SUFFIX}"));

        let response = self
            .http
            .post(format!("{}/v1/recognize", self.endpoint))
            .query(&[("model", model.as_str()), ("smart_formatting", "true")])
            .header(reqwest::header::AUTHORIZATION, self.basic_auth())
            .header(reqwest::header::CONTENT_TYPE, request.content_type)
            .body(request.audio)
            .send()
            .await
            .map_err(transport)?;

        let body: RecognizeBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(joined_transcript(&body))
    }

    /// Watson uses Basic auth with a fixed username and the API key as the
    /// password.
    fn basic_auth(&self) -> String {
        let raw = format!("apikey:{}", self.api_key);
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }
}

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
        serde_json::from_str(json).expect("valid Watson body")
    }

    #[test]
    fn concatenates_result_alternatives() {
        let body = parse(
            r#"{"results":[
                {"final":true,"alternatives":[{"transcript":"run cargo clippy "}]},
                {"final":true,"alternatives":[{"transcript":"on the branch"}]}]}"#,
        );
        assert_eq!(joined_transcript(&body), "run cargo clippy on the branch");
    }

    #[test]
    fn silence_yields_an_empty_transcript() {
        assert_eq!(joined_transcript(&parse(r#"{"results":[]}"#)), "");
    }

    #[test]
    fn ibm_requires_its_per_instance_url() {
        let env = SttEnv {
            api_key: Some("k".into()),
            ..Default::default()
        };
        let err = crate::config_error(IbmStt::from_env(&env));
        assert!(err.contains("PORTAL_STT_ENDPOINT"), "{err}");
    }

    #[test]
    fn credentials_are_sent_as_basic_auth_under_the_apikey_user() {
        let provider = IbmStt::from_env(&SttEnv {
            api_key: Some("secret".into()),
            endpoint: Some("https://api.example.invalid/instances/1/".into()),
            ..Default::default()
        })
        .expect("configured");

        assert_eq!(provider.endpoint, "https://api.example.invalid/instances/1");
        let header = provider.basic_auth();
        let encoded = header.strip_prefix("Basic ").expect("basic scheme");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("valid base64");
        assert_eq!(String::from_utf8(decoded).unwrap(), "apikey:secret");
    }
}

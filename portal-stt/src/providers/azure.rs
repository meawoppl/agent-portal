//! Microsoft Azure AI Speech — Fast Transcription.
//!
//! One multipart call: the audio as a file part, and a `definition` JSON part
//! carrying the locale. Regional, so `PORTAL_STT_REGION` is required.
//!
//! **Keyterms are not used here.** Azure's biasing story is a trained Custom
//! Speech model, not an inline phrase list, so there is nowhere to put them on
//! this endpoint. Point `PORTAL_STT_MODEL` at a Custom Speech deployment to get
//! domain vocabulary.

use serde::{Deserialize, Serialize};

use crate::config::{resolve_language, Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::{extension_for, SttError, TranscribeRequest};

const API_VERSION: &str = "2024-11-15";
const DEFAULT_LOCALE: &str = "en-US";

#[derive(Clone)]
pub(crate) struct AzureStt {
    api_key: String,
    endpoint: String,
    language: Option<String>,
    /// Custom Speech deployment id, when one is configured.
    model: Option<String>,
    http: reqwest::Client,
}

/// The `definition` part of the multipart request.
#[derive(Serialize)]
struct Definition<'a> {
    locales: [&'a str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<ModelRef<'a>>,
}

/// A Custom Speech deployment reference. The field really is named `self`.
#[derive(Serialize)]
struct ModelRef<'a> {
    #[serde(rename = "self")]
    self_uri: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FastTranscriptionBody {
    #[serde(default)]
    combined_phrases: Vec<CombinedPhrase>,
}

#[derive(Deserialize)]
struct CombinedPhrase {
    #[serde(default)]
    text: String,
}

impl AzureStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        let api_key = env.require(Field::ApiKey, "azure")?;
        let region = env.require(Field::Region, "azure")?;
        Ok(Self {
            api_key,
            endpoint: env.endpoint_or(&format!("https://{region}.api.cognitive.microsoft.com")),
            language: env.language.clone(),
            model: env.model.clone(),
            http: reqwest::Client::new(),
        })
    }

    pub(crate) async fn transcribe(
        &self,
        request: TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        let locale = resolve_language(request.language, self.language.as_deref(), DEFAULT_LOCALE);
        let definition = Definition {
            locales: [&locale],
            model: self.model.as_deref().map(|self_uri| ModelRef { self_uri }),
        };
        let definition = serde_json::to_string(&definition)
            .map_err(|e| SttError::Provider(format!("could not encode request: {e}")))?;

        let audio = reqwest::multipart::Part::bytes(request.audio.to_vec())
            .file_name(format!("audio.{}", extension_for(request.content_type)))
            .mime_str(request.content_type)
            .map_err(|e| SttError::Provider(format!("unsupported audio content type: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .part("audio", audio)
            .text("definition", definition);

        let response = self
            .http
            .post(format!(
                "{}/speechtotext/transcriptions:transcribe?api-version={API_VERSION}",
                self.endpoint
            ))
            .header("Ocp-Apim-Subscription-Key", &self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(transport)?;

        let body: FastTranscriptionBody =
            ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(combined_text(&body))
    }
}

/// Azure returns the flat transcript as `combinedPhrases`, one entry per
/// channel. Mono audio yields one; joining is the safe general case.
fn combined_text(body: &FastTranscriptionBody) -> String {
    body.combined_phrases
        .iter()
        .map(|p| p.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> FastTranscriptionBody {
        serde_json::from_str(json).expect("valid Azure body")
    }

    #[test]
    fn reads_the_combined_transcript() {
        let body = parse(
            r#"{"durationMilliseconds":1200,
                "combinedPhrases":[{"text":"run cargo clippy"}]}"#,
        );
        assert_eq!(combined_text(&body), "run cargo clippy");
    }

    #[test]
    fn joins_multiple_channels() {
        let body = parse(r#"{"combinedPhrases":[{"text":"one"},{"text":"two"}]}"#);
        assert_eq!(combined_text(&body), "one two");
    }

    #[test]
    fn silence_yields_an_empty_transcript() {
        assert_eq!(combined_text(&parse(r#"{"combinedPhrases":[]}"#)), "");
        assert_eq!(combined_text(&parse(r#"{}"#)), "");
    }

    #[test]
    fn azure_requires_a_region() {
        let env = SttEnv {
            api_key: Some("k".into()),
            ..Default::default()
        };
        let err = crate::config_error(AzureStt::from_env(&env));
        assert!(err.contains("PORTAL_STT_REGION"), "{err}");
    }

    #[test]
    fn the_endpoint_defaults_to_the_configured_region() {
        let env = SttEnv {
            api_key: Some("k".into()),
            region: Some("westus2".into()),
            ..Default::default()
        };
        let provider = AzureStt::from_env(&env).expect("configured");
        assert_eq!(
            provider.endpoint,
            "https://westus2.api.cognitive.microsoft.com"
        );
    }
}

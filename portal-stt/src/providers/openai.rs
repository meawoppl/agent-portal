//! OpenAI transcription (`/v1/audio/transcriptions`).
//!
//! Audio goes up as a multipart form. Vocabulary bias is expressed through the
//! `prompt` field, which is free text rather than a term list — the documented
//! way to steer it is a sentence that *uses* or names the expected vocabulary,
//! so [`bias_prompt`] renders the keyterms into one.

use serde::Deserialize;

use crate::config::{Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::{extension_for, SttError, TranscribeRequest};

const DEFAULT_ENDPOINT: &str = "https://api.openai.com";
const DEFAULT_MODEL: &str = "gpt-4o-transcribe";

#[derive(Clone)]
pub(crate) struct OpenAiStt {
    api_key: String,
    model: String,
    /// Overridable for OpenAI-compatible gateways and self-hosted Whisper.
    endpoint: String,
    language: Option<String>,
    /// Owned per provider, matching the push transports — a `Client` is a
    /// connection pool, so it must outlive individual requests.
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct TranscriptionBody {
    text: String,
}

impl OpenAiStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: env.require(Field::ApiKey, "openai")?,
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
        let filename = format!("audio.{}", extension_for(request.content_type));
        let part = reqwest::multipart::Part::bytes(request.audio.to_vec())
            .file_name(filename)
            .mime_str(request.content_type)
            .map_err(|e| SttError::Provider(format!("unsupported audio content type: {e}")))?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part("file", part);
        if let Some(language) = request.language.or(self.language.as_deref()) {
            form = form.text("language", language.to_string());
        }
        if let Some(prompt) = bias_prompt(request.keyterms) {
            form = form.text("prompt", prompt);
        }

        let response = self
            .http
            .post(format!("{}/v1/audio/transcriptions", self.endpoint))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(transport)?;

        let body: TranscriptionBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(body.text.trim().to_string())
    }
}

/// Render keyterms into the sentence-shaped hint the `prompt` field expects.
///
/// `None` when there is nothing to bias with, so we don't send an empty prompt
/// that the model would try to treat as context.
fn bias_prompt(keyterms: &[String]) -> Option<String> {
    if keyterms.is_empty() {
        return None;
    }
    Some(format!(
        "The speaker is a software engineer dictating an instruction to a coding \
         agent. Expect terminology such as: {}.",
        keyterms.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_keyterms_means_no_prompt() {
        assert!(bias_prompt(&[]).is_none());
    }

    #[test]
    fn keyterms_are_rendered_into_the_hint() {
        let prompt = bias_prompt(&["clippy".to_string(), "agent-portal".to_string()])
            .expect("some keyterms yield a prompt");
        assert!(prompt.contains("clippy"));
        assert!(prompt.contains("agent-portal"));
    }

    #[test]
    fn the_default_model_is_used_when_none_is_configured() {
        let env = SttEnv {
            api_key: Some("k".into()),
            ..Default::default()
        };
        assert_eq!(
            OpenAiStt::from_env(&env).expect("configured").model,
            DEFAULT_MODEL
        );

        let overridden = SttEnv {
            model: Some("whisper-1".into()),
            ..env.clone()
        };
        assert_eq!(
            OpenAiStt::from_env(&overridden).expect("configured").model,
            "whisper-1"
        );
    }

    #[test]
    fn openai_requires_a_key() {
        let err = crate::config_error(OpenAiStt::from_env(&SttEnv::default()));
        assert!(err.contains("PORTAL_STT_API_KEY"), "{err}");
    }
}

//! OpenAI transcription (`/v1/audio/transcriptions`).
//!
//! Audio goes up as a multipart form. Vocabulary bias is expressed through the
//! `prompt` field, which is free text rather than a term list — the documented
//! way to steer it is a sentence that *uses* or names the expected vocabulary,
//! so [`bias_prompt`] renders the keyterms into one.

use serde::Deserialize;

use super::{extension_for, SttError, TranscribeRequest};

const ENDPOINT: &str = "https://api.openai.com/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "gpt-4o-transcribe";

#[derive(Clone)]
pub struct OpenAiStt {
    api_key: String,
    model: String,
    /// Owned per provider, matching the push transports — a `Client` is a
    /// connection pool, so it must outlive individual requests.
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct TranscriptionBody {
    text: String,
}

impl OpenAiStt {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            api_key,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe(&self, request: TranscribeRequest<'_>) -> Result<String, SttError> {
        let filename = format!("audio.{}", extension_for(request.content_type));
        let part = reqwest::multipart::Part::bytes(request.audio.to_vec())
            .file_name(filename)
            .mime_str(request.content_type)
            .map_err(|e| SttError::Provider(format!("unsupported audio content type: {e}")))?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part("file", part);
        if let Some(language) = request.language {
            form = form.text("language", language.to_string());
        }
        if let Some(prompt) = bias_prompt(request.keyterms) {
            form = form.text("prompt", prompt);
        }

        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| SttError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SttError::Provider(format!(
                "HTTP {status}: {}",
                body.chars().take(400).collect::<String>()
            )));
        }

        let body: TranscriptionBody = response
            .json()
            .await
            .map_err(|e| SttError::Decode(e.to_string()))?;
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
        let provider = OpenAiStt::new("k".to_string(), None);
        assert_eq!(provider.model, DEFAULT_MODEL);
        let overridden = OpenAiStt::new("k".to_string(), Some("whisper-1".to_string()));
        assert_eq!(overridden.model, "whisper-1");
    }
}

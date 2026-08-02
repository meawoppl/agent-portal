//! AssemblyAI — upload, submit, poll.
//!
//! Three calls: the audio is uploaded to get a URL, a transcript job is created
//! against that URL, then the job is polled. Keyterms map onto `word_boost`.

use serde::Deserialize;

use crate::config::{Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::poll::{poll_job, JobState, DEFAULT_TIMEOUT};
use crate::{SttError, TranscribeRequest};

const DEFAULT_ENDPOINT: &str = "https://api.assemblyai.com";
/// AssemblyAI caps `word_boost` at 1000 entries; our keyterm list is far
/// shorter, but the cap is documented here so a future change notices it.
const MAX_WORD_BOOST: usize = 1000;

#[derive(Clone)]
pub(crate) struct AssemblyAiStt {
    api_key: String,
    endpoint: String,
    language: Option<String>,
    model: Option<String>,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct UploadBody {
    upload_url: String,
}

#[derive(Deserialize)]
struct TranscriptBody {
    id: String,
}

#[derive(Deserialize)]
struct TranscriptStatus {
    status: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl AssemblyAiStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: env.require(Field::ApiKey, "assemblyai")?,
            endpoint: env.endpoint_or(DEFAULT_ENDPOINT),
            language: env.language.clone(),
            model: env.model.clone(),
            http: reqwest::Client::new(),
        })
    }

    pub(crate) async fn transcribe(
        &self,
        request: TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        let audio_url = self.upload(request.audio).await?;
        let job_id = self.submit(&audio_url, request.keyterms).await?;

        poll_job("assemblyai", DEFAULT_TIMEOUT, || async {
            let status = self.job_status(&job_id).await?;
            Ok(classify(&status))
        })
        .await
    }

    async fn upload(&self, audio: crate::Bytes) -> Result<String, SttError> {
        let response = self
            .http
            .post(format!("{}/v2/upload", self.endpoint))
            .header(reqwest::header::AUTHORIZATION, &self.api_key)
            .body(audio)
            .send()
            .await
            .map_err(transport)?;
        let body: UploadBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(body.upload_url)
    }

    async fn submit(&self, audio_url: &str, keyterms: &[String]) -> Result<String, SttError> {
        let mut payload = serde_json::json!({ "audio_url": audio_url });
        if !keyterms.is_empty() {
            let boosted: Vec<&String> = keyterms.iter().take(MAX_WORD_BOOST).collect();
            payload["word_boost"] = serde_json::json!(boosted);
            payload["boost_param"] = serde_json::json!("high");
        }
        if let Some(language) = &self.language {
            payload["language_code"] = serde_json::json!(language);
        }
        if let Some(model) = &self.model {
            payload["speech_model"] = serde_json::json!(model);
        }

        let response = self
            .http
            .post(format!("{}/v2/transcript", self.endpoint))
            .header(reqwest::header::AUTHORIZATION, &self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(transport)?;
        let body: TranscriptBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(body.id)
    }

    async fn job_status(&self, job_id: &str) -> Result<TranscriptStatus, SttError> {
        let response = self
            .http
            .get(format!("{}/v2/transcript/{job_id}", self.endpoint))
            .header(reqwest::header::AUTHORIZATION, &self.api_key)
            .send()
            .await
            .map_err(transport)?;
        ensure_ok(response).await?.json().await.map_err(decode)
    }
}

/// Map AssemblyAI's status string onto a poll decision.
///
/// An unrecognized status is treated as still-running rather than an error, so
/// a newly introduced intermediate state degrades into waiting (and eventually
/// the timeout) instead of failing a transcription that was going to succeed.
fn classify(status: &TranscriptStatus) -> JobState<String> {
    match status.status.as_str() {
        "completed" => JobState::Done(status.text.clone().unwrap_or_default().trim().to_string()),
        "error" => JobState::Failed(
            status
                .error
                .clone()
                .unwrap_or_else(|| "no reason given".to_string()),
        ),
        _ => JobState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> TranscriptStatus {
        serde_json::from_str(json).expect("valid AssemblyAI status")
    }

    fn done_text(state: JobState<String>) -> Option<String> {
        match state {
            JobState::Done(text) => Some(text),
            _ => None,
        }
    }

    #[test]
    fn a_completed_job_yields_its_text() {
        let state = classify(&parse(
            r#"{"status":"completed","text":" run cargo clippy "}"#,
        ));
        assert_eq!(done_text(state).as_deref(), Some("run cargo clippy"));
    }

    #[test]
    fn an_errored_job_carries_the_reason() {
        let state = classify(&parse(r#"{"status":"error","error":"audio too short"}"#));
        match state {
            JobState::Failed(reason) => assert_eq!(reason, "audio too short"),
            _ => panic!("expected failure"),
        }
    }

    #[test]
    fn queued_and_processing_keep_polling() {
        assert!(matches!(
            classify(&parse(r#"{"status":"queued"}"#)),
            JobState::Pending
        ));
        assert!(matches!(
            classify(&parse(r#"{"status":"processing"}"#)),
            JobState::Pending
        ));
    }

    /// A status this build has never heard of must not fail a job that is
    /// simply still running.
    #[test]
    fn an_unknown_status_is_treated_as_still_running() {
        assert!(matches!(
            classify(&parse(r#"{"status":"reprocessing"}"#)),
            JobState::Pending
        ));
    }

    /// Silence completes with no `text` at all rather than an empty string.
    #[test]
    fn a_completed_job_without_text_is_empty_not_an_error() {
        let state = classify(&parse(r#"{"status":"completed"}"#));
        assert_eq!(done_text(state).as_deref(), Some(""));
    }
}

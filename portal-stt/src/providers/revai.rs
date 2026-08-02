//! Rev AI — submit a job, poll it, fetch the transcript.
//!
//! Unlike AssemblyAI the audio is posted directly with the job (multipart), and
//! the finished transcript is a separate fetch rather than a field on the
//! status. Keyterms map onto `custom_vocabularies`.

use serde::Deserialize;

use crate::config::{Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::poll::{poll_job, JobState, DEFAULT_TIMEOUT};
use crate::{extension_for, SttError, TranscribeRequest};

const DEFAULT_ENDPOINT: &str = "https://api.rev.ai";

#[derive(Clone)]
pub(crate) struct RevAiStt {
    api_key: String,
    endpoint: String,
    language: Option<String>,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct JobBody {
    id: String,
}

#[derive(Deserialize)]
struct JobStatusBody {
    status: String,
    #[serde(default)]
    failure_detail: Option<String>,
    #[serde(default)]
    failure: Option<String>,
}

impl RevAiStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: env.require(Field::ApiKey, "revai")?,
            endpoint: env.endpoint_or(DEFAULT_ENDPOINT),
            language: env.language.clone(),
            http: reqwest::Client::new(),
        })
    }

    pub(crate) async fn transcribe(
        &self,
        request: TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        let job_id = self.submit(&request).await?;

        poll_job("revai", DEFAULT_TIMEOUT, || async {
            let status = self.job_status(&job_id).await?;
            Ok(classify(&status))
        })
        .await?;

        self.fetch_transcript(&job_id).await
    }

    async fn submit(&self, request: &TranscribeRequest<'_>) -> Result<String, SttError> {
        let mut options = serde_json::json!({ "skip_diarization": true });
        if !request.keyterms.is_empty() {
            options["custom_vocabularies"] = serde_json::json!([{ "phrases": request.keyterms }]);
        }
        if let Some(language) = &self.language {
            options["language"] = serde_json::json!(language);
        }

        let media = reqwest::multipart::Part::bytes(request.audio.to_vec())
            .file_name(format!("audio.{}", extension_for(request.content_type)))
            .mime_str(request.content_type)
            .map_err(|e| SttError::Provider(format!("unsupported audio content type: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .part("media", media)
            .text("options", options.to_string());

        let response = self
            .http
            .post(format!("{}/speechtotext/v1/jobs", self.endpoint))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(transport)?;
        let body: JobBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(body.id)
    }

    async fn job_status(&self, job_id: &str) -> Result<JobStatusBody, SttError> {
        let response = self
            .http
            .get(format!("{}/speechtotext/v1/jobs/{job_id}", self.endpoint))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(transport)?;
        ensure_ok(response).await?.json().await.map_err(decode)
    }

    /// Rev AI serves the transcript as JSON by default; `text/plain` asks for
    /// the flat text, which is all we want.
    async fn fetch_transcript(&self, job_id: &str) -> Result<String, SttError> {
        let response = self
            .http
            .get(format!(
                "{}/speechtotext/v1/jobs/{job_id}/transcript",
                self.endpoint
            ))
            .bearer_auth(&self.api_key)
            .header(reqwest::header::ACCEPT, "text/plain")
            .send()
            .await
            .map_err(transport)?;
        let text = ensure_ok(response).await?.text().await.map_err(decode)?;
        Ok(text.trim().to_string())
    }
}

/// `transcribed` is Rev AI's success state; `failed` is terminal. Anything else
/// (`in_progress`, or a state added later) means keep waiting.
fn classify(status: &JobStatusBody) -> JobState<()> {
    match status.status.as_str() {
        "transcribed" => JobState::Done(()),
        "failed" => JobState::Failed(
            status
                .failure_detail
                .clone()
                .or_else(|| status.failure.clone())
                .unwrap_or_else(|| "no reason given".to_string()),
        ),
        _ => JobState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> JobStatusBody {
        serde_json::from_str(json).expect("valid Rev AI status")
    }

    #[test]
    fn a_transcribed_job_is_done() {
        assert!(matches!(
            classify(&parse(r#"{"status":"transcribed"}"#)),
            JobState::Done(())
        ));
    }

    #[test]
    fn in_progress_keeps_polling() {
        assert!(matches!(
            classify(&parse(r#"{"status":"in_progress"}"#)),
            JobState::Pending
        ));
    }

    /// Rev AI reports the reason in `failure_detail`, falling back to the
    /// coarser `failure` code when the detail is absent.
    #[test]
    fn a_failed_job_prefers_the_detailed_reason() {
        let state = classify(&parse(
            r#"{"status":"failed","failure":"download_failure",
                "failure_detail":"could not decode audio"}"#,
        ));
        match state {
            JobState::Failed(reason) => assert_eq!(reason, "could not decode audio"),
            _ => panic!("expected failure"),
        }

        let coarse = classify(&parse(
            r#"{"status":"failed","failure":"internal_processing"}"#,
        ));
        match coarse {
            JobState::Failed(reason) => assert_eq!(reason, "internal_processing"),
            _ => panic!("expected failure"),
        }
    }

    #[test]
    fn a_failure_with_no_reason_still_reports_something() {
        match classify(&parse(r#"{"status":"failed"}"#)) {
            JobState::Failed(reason) => assert!(!reason.is_empty()),
            _ => panic!("expected failure"),
        }
    }
}

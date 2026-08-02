//! Speechmatics — submit a job, poll it, fetch the transcript.
//!
//! Same three-step shape as Rev AI, with the job configuration supplied as a
//! JSON part alongside the audio. Keyterms map onto `additional_vocab`.

use serde::{Deserialize, Serialize};

use crate::config::{resolve_language, Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::poll::{poll_job, JobState, DEFAULT_TIMEOUT};
use crate::{extension_for, SttError, TranscribeRequest};

const DEFAULT_ENDPOINT: &str = "https://asr.api.speechmatics.com";
/// Speechmatics takes a bare language code.
const DEFAULT_LANGUAGE: &str = "en";
/// `additional_vocab` is capped at 1000 entries by the API.
const MAX_ADDITIONAL_VOCAB: usize = 1000;

#[derive(Clone)]
pub(crate) struct SpeechmaticsStt {
    api_key: String,
    endpoint: String,
    language: Option<String>,
    /// Operating point (`standard` / `enhanced`), when configured.
    model: Option<String>,
    http: reqwest::Client,
}

/// The `config` part of the multipart submit.
#[derive(Serialize)]
struct JobConfig<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    transcription_config: TranscriptionConfig<'a>,
}

#[derive(Serialize)]
struct TranscriptionConfig<'a> {
    language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    operating_point: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_vocab: Option<Vec<AdditionalVocab<'a>>>,
}

#[derive(Serialize)]
struct AdditionalVocab<'a> {
    content: &'a str,
}

#[derive(Deserialize)]
struct CreateJobBody {
    id: String,
}

#[derive(Deserialize)]
struct JobDetailsBody {
    job: JobDetails,
}

#[derive(Deserialize)]
struct JobDetails {
    status: String,
    #[serde(default)]
    errors: Vec<JobError>,
}

#[derive(Deserialize)]
struct JobError {
    #[serde(default)]
    message: String,
}

impl SpeechmaticsStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        Ok(Self {
            api_key: env.require(Field::ApiKey, "speechmatics")?,
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
        let job_id = self.submit(&request).await?;

        poll_job("speechmatics", DEFAULT_TIMEOUT, || async {
            let details = self.job_details(&job_id).await?;
            Ok(classify(&details.job))
        })
        .await?;

        self.fetch_transcript(&job_id).await
    }

    async fn submit(&self, request: &TranscribeRequest<'_>) -> Result<String, SttError> {
        let language =
            resolve_language(request.language, self.language.as_deref(), DEFAULT_LANGUAGE);
        let config = JobConfig {
            kind: "transcription",
            transcription_config: TranscriptionConfig {
                language: bare_language(&language),
                operating_point: self.model.as_deref(),
                additional_vocab: (!request.keyterms.is_empty()).then(|| {
                    request
                        .keyterms
                        .iter()
                        .take(MAX_ADDITIONAL_VOCAB)
                        .map(|term| AdditionalVocab {
                            content: term.as_str(),
                        })
                        .collect()
                }),
            },
        };
        let config = serde_json::to_string(&config)
            .map_err(|e| SttError::Provider(format!("could not encode request: {e}")))?;

        let data_file = reqwest::multipart::Part::bytes(request.audio.to_vec())
            .file_name(format!("audio.{}", extension_for(request.content_type)))
            .mime_str(request.content_type)
            .map_err(|e| SttError::Provider(format!("unsupported audio content type: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .part("data_file", data_file)
            .text("config", config);

        let response = self
            .http
            .post(format!("{}/v2/jobs", self.endpoint))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(transport)?;
        let body: CreateJobBody = ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(body.id)
    }

    async fn job_details(&self, job_id: &str) -> Result<JobDetailsBody, SttError> {
        let response = self
            .http
            .get(format!("{}/v2/jobs/{job_id}", self.endpoint))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(transport)?;
        ensure_ok(response).await?.json().await.map_err(decode)
    }

    async fn fetch_transcript(&self, job_id: &str) -> Result<String, SttError> {
        let response = self
            .http
            .get(format!("{}/v2/jobs/{job_id}/transcript", self.endpoint))
            .query(&[("format", "txt")])
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(transport)?;
        let text = ensure_ok(response).await?.text().await.map_err(decode)?;
        Ok(text.trim().to_string())
    }
}

fn bare_language(language: &str) -> String {
    language
        .split(['-', '_'])
        .next()
        .unwrap_or(language)
        .to_ascii_lowercase()
}

/// `done` is success; `rejected` and `expired` are terminal. `running` — and
/// any state added later — means keep waiting.
fn classify(job: &JobDetails) -> JobState<()> {
    match job.status.as_str() {
        "done" => JobState::Done(()),
        "rejected" | "expired" => JobState::Failed(job_error(job)),
        _ => JobState::Pending,
    }
}

fn job_error(job: &JobDetails) -> String {
    let joined = job
        .errors
        .iter()
        .map(|e| e.message.trim())
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if joined.is_empty() {
        format!("job {}", job.status)
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> JobDetailsBody {
        serde_json::from_str(json).expect("valid Speechmatics body")
    }

    #[test]
    fn a_done_job_is_complete() {
        assert!(matches!(
            classify(&parse(r#"{"job":{"id":"x","status":"done"}}"#).job),
            JobState::Done(())
        ));
    }

    #[test]
    fn running_keeps_polling() {
        assert!(matches!(
            classify(&parse(r#"{"job":{"id":"x","status":"running"}}"#).job),
            JobState::Pending
        ));
    }

    #[test]
    fn a_rejected_job_reports_its_errors() {
        let body = parse(
            r#"{"job":{"id":"x","status":"rejected",
                "errors":[{"message":"unsupported audio"}]}}"#,
        );
        match classify(&body.job) {
            JobState::Failed(reason) => assert_eq!(reason, "unsupported audio"),
            _ => panic!("expected failure"),
        }
    }

    /// A rejection with no error list still has to say something actionable.
    #[test]
    fn a_rejection_without_errors_names_the_status() {
        let body = parse(r#"{"job":{"id":"x","status":"rejected"}}"#);
        match classify(&body.job) {
            JobState::Failed(reason) => assert_eq!(reason, "job rejected"),
            _ => panic!("expected failure"),
        }
    }

    #[test]
    fn expired_jobs_are_terminal_rather_than_polled_forever() {
        let body = parse(r#"{"job":{"id":"x","status":"expired"}}"#);
        assert!(matches!(classify(&body.job), JobState::Failed(_)));
    }

    #[test]
    fn region_qualified_tags_are_reduced() {
        assert_eq!(bare_language("en-US"), "en");
    }
}

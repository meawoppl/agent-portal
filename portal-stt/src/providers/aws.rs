//! Amazon Transcribe.
//!
//! The awkward one. Transcribe has no synchronous "bytes in, text out" call:
//! audio must be staged in S3, a job started against that object, the job
//! polled, and the transcript fetched from a presigned URL. That is why this
//! provider — alone among the ten — needs `PORTAL_STT_BUCKET`.
//!
//! Requests are SigV4-signed with the standard `AWS_ACCESS_KEY_ID` /
//! `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` variables, the same ones the
//! session archive's S3 backend uses, so a deploy already using S3 needs no new
//! credentials. They are read per request so rotation is picked up without a
//! restart.
//!
//! **Keyterms cannot be passed inline.** Transcribe's vocabulary is a
//! pre-created named resource, not a per-request list; set
//! `PORTAL_STT_VOCABULARY_NAME` to use one.

use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    sign, PayloadChecksumKind, SignableBody, SignableRequest, SigningSettings,
};
use aws_sigv4::sign::v4;
use serde::{Deserialize, Serialize};

use crate::config::{resolve_language, Field, SttEnv};
use crate::http::{decode, ensure_ok, transport};
use crate::poll::{poll_job, JobState, DEFAULT_TIMEOUT};
use crate::{extension_for, SttError, TranscribeRequest};

const DEFAULT_LANGUAGE: &str = "en-US";
const JSON_1_1: &str = "application/x-amz-json-1.1";

#[derive(Clone)]
pub(crate) struct AwsStt {
    region: String,
    bucket: String,
    language: Option<String>,
    vocabulary_name: Option<String>,
    http: reqwest::Client,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartJobRequest<'a> {
    transcription_job_name: &'a str,
    language_code: &'a str,
    media: Media,
    /// Omitted when the container is not one Transcribe names — see
    /// [`media_format_for`].
    #[serde(skip_serializing_if = "Option::is_none")]
    media_format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<JobSettings<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Media {
    media_file_uri: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct JobSettings<'a> {
    vocabulary_name: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct GetJobRequest<'a> {
    transcription_job_name: &'a str,
}

#[derive(Deserialize)]
struct GetJobBody {
    #[serde(rename = "TranscriptionJob")]
    transcription_job: TranscriptionJob,
}

#[derive(Deserialize)]
struct TranscriptionJob {
    #[serde(rename = "TranscriptionJobStatus")]
    status: String,
    #[serde(default, rename = "Transcript")]
    transcript: Option<Transcript>,
    #[serde(default, rename = "FailureReason")]
    failure_reason: Option<String>,
}

#[derive(Deserialize)]
struct Transcript {
    #[serde(default, rename = "TranscriptFileUri")]
    transcript_file_uri: Option<String>,
}

#[derive(Deserialize)]
struct TranscriptDocument {
    #[serde(default)]
    results: TranscriptResults,
}

#[derive(Default, Deserialize)]
struct TranscriptResults {
    #[serde(default)]
    transcripts: Vec<TranscriptText>,
}

#[derive(Deserialize)]
struct TranscriptText {
    #[serde(default)]
    transcript: String,
}

impl AwsStt {
    pub(crate) fn from_env(env: &SttEnv) -> anyhow::Result<Self> {
        let region = env
            .get(Field::Region)
            .or_else(|| std::env::var("AWS_REGION").ok())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}=aws requires {} (or AWS_REGION) to be set",
                    crate::config::BACKEND_VAR,
                    Field::Region.var_name()
                )
            })?;
        Ok(Self {
            region,
            bucket: env.require(Field::Bucket, "aws")?,
            language: env.language.clone(),
            vocabulary_name: env.get(Field::VocabularyName),
            http: reqwest::Client::new(),
        })
    }

    pub(crate) async fn transcribe(
        &self,
        request: TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        let job_name = format!("agent-portal-{}", uuid::Uuid::new_v4());
        let key = format!(
            "agent-portal-stt/{job_name}.{}",
            extension_for(request.content_type)
        );

        self.put_object(&key, request.audio.clone(), request.content_type)
            .await?;

        // Whatever happens next, don't leave the staged audio behind.
        let outcome = self.run_job(&job_name, &key, &request).await;
        if let Err(error) = self.delete_object(&key).await {
            tracing::warn!(
                target: "stt",
                event = "aws_stage_cleanup_failed",
                key = %key,
                error = %error,
            );
        }
        outcome
    }

    async fn run_job(
        &self,
        job_name: &str,
        key: &str,
        request: &TranscribeRequest<'_>,
    ) -> Result<String, SttError> {
        self.start_job(job_name, key, request).await?;

        let uri = poll_job("aws transcribe", DEFAULT_TIMEOUT, || async {
            let body = self.get_job(job_name).await?;
            Ok(classify(&body.transcription_job))
        })
        .await?;

        // Presigned by AWS when no output bucket is configured, so this fetch
        // is deliberately unsigned.
        let response = self.http.get(&uri).send().await.map_err(transport)?;
        let document: TranscriptDocument =
            ensure_ok(response).await?.json().await.map_err(decode)?;
        Ok(joined_transcript(&document))
    }

    async fn start_job(
        &self,
        job_name: &str,
        key: &str,
        request: &TranscribeRequest<'_>,
    ) -> Result<(), SttError> {
        let language =
            resolve_language(request.language, self.language.as_deref(), DEFAULT_LANGUAGE);
        let payload = StartJobRequest {
            transcription_job_name: job_name,
            language_code: &language,
            media: Media {
                media_file_uri: format!("s3://{}/{key}", self.bucket),
            },
            media_format: media_format_for(request.content_type),
            settings: self
                .vocabulary_name
                .as_deref()
                .map(|vocabulary_name| JobSettings { vocabulary_name }),
        };

        self.transcribe_call("StartTranscriptionJob", &payload)
            .await
            .map(|_| ())
    }

    async fn get_job(&self, job_name: &str) -> Result<GetJobBody, SttError> {
        let payload = GetJobRequest {
            transcription_job_name: job_name,
        };
        let text = self
            .transcribe_call("GetTranscriptionJob", &payload)
            .await?;
        serde_json::from_str(&text).map_err(decode)
    }

    /// One signed call to the Transcribe JSON-RPC endpoint.
    async fn transcribe_call(
        &self,
        target: &str,
        payload: &impl Serialize,
    ) -> Result<String, SttError> {
        let url = format!("https://transcribe.{}.amazonaws.com/", self.region);
        let body = serde_json::to_vec(payload).map_err(decode)?;
        let headers = vec![
            ("content-type".to_string(), JSON_1_1.to_string()),
            ("x-amz-target".to_string(), format!("Transcribe.{target}")),
        ];

        let request = self.signed_request("POST", &url, &headers, body, "transcribe")?;
        let response = self.http.execute(request).await.map_err(transport)?;
        ensure_ok(response).await?.text().await.map_err(decode)
    }

    async fn put_object(
        &self,
        key: &str,
        audio: crate::Bytes,
        content_type: &str,
    ) -> Result<(), SttError> {
        let url = self.object_url(key);
        let headers = vec![("content-type".to_string(), content_type.to_string())];
        let request = self.signed_request("PUT", &url, &headers, audio.to_vec(), "s3")?;
        let response = self.http.execute(request).await.map_err(transport)?;
        ensure_ok(response).await.map(|_| ())
    }

    async fn delete_object(&self, key: &str) -> Result<(), SttError> {
        let url = self.object_url(key);
        let request = self.signed_request("DELETE", &url, &[], Vec::new(), "s3")?;
        let response = self.http.execute(request).await.map_err(transport)?;
        ensure_ok(response).await.map(|_| ())
    }

    fn object_url(&self, key: &str) -> String {
        format!(
            "https://{}.s3.{}.amazonaws.com/{key}",
            self.bucket, self.region
        )
    }

    /// Build a SigV4-signed request. Credentials are read here, per call, so a
    /// rotation lands without a restart.
    fn signed_request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
        service: &str,
    ) -> Result<reqwest::Request, SttError> {
        let access_key = std::env::var("AWS_ACCESS_KEY_ID")
            .map_err(|_| SttError::Provider("AWS_ACCESS_KEY_ID is not set".to_string()))?;
        let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .map_err(|_| SttError::Provider("AWS_SECRET_ACCESS_KEY is not set".to_string()))?;
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();

        let identity =
            Credentials::new(access_key, secret_key, session_token, None, "portal-stt").into();

        let mut settings = SigningSettings::default();
        // S3 rejects a signed request that omits the payload hash header.
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;

        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name(service)
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|e| SttError::Provider(format!("could not build signing params: {e}")))?;

        let signable = SignableRequest::new(
            method,
            url,
            headers.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            SignableBody::Bytes(&body),
        )
        .map_err(|e| SttError::Provider(format!("could not prepare request for signing: {e}")))?;

        let (instructions, _signature) = sign(signable, &params.into())
            .map_err(|e| SttError::Provider(format!("could not sign request: {e}")))?
            .into_parts();

        let mut builder = http::Request::builder().method(method).uri(url);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let mut http_request = builder
            .body(body)
            .map_err(|e| SttError::Provider(format!("could not build request: {e}")))?;
        instructions.apply_to_request_http1x(&mut http_request);

        reqwest::Request::try_from(http_request)
            .map_err(|e| SttError::Provider(format!("could not convert signed request: {e}")))
    }
}

/// Transcribe wants the container named. Unknown types are left unset so it can
/// infer, rather than being told something wrong.
fn media_format_for(content_type: &str) -> Option<&'static str> {
    match content_type.split(';').next().unwrap_or("").trim() {
        "audio/webm" => Some("webm"),
        "audio/ogg" => Some("ogg"),
        "audio/mpeg" => Some("mp3"),
        "audio/mp4" | "audio/x-m4a" => Some("mp4"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/flac" => Some("flac"),
        _ => None,
    }
}

/// A completed job carries the URL of its transcript; anything else is either
/// terminal failure or still running.
fn classify(job: &TranscriptionJob) -> JobState<String> {
    match job.status.as_str() {
        "COMPLETED" => match job
            .transcript
            .as_ref()
            .and_then(|t| t.transcript_file_uri.clone())
        {
            Some(uri) => JobState::Done(uri),
            // Completed with nowhere to read the result from is a failure we
            // cannot recover by waiting.
            None => JobState::Failed("completed without a transcript URI".to_string()),
        },
        "FAILED" => JobState::Failed(
            job.failure_reason
                .clone()
                .unwrap_or_else(|| "no reason given".to_string()),
        ),
        _ => JobState::Pending,
    }
}

fn joined_transcript(document: &TranscriptDocument) -> String {
    document
        .results
        .transcripts
        .iter()
        .map(|t| t.transcript.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(json: &str) -> TranscriptionJob {
        serde_json::from_str::<GetJobBody>(json)
            .expect("valid Transcribe body")
            .transcription_job
    }

    #[test]
    fn a_completed_job_yields_its_transcript_url() {
        let state = classify(&job(
            r#"{"TranscriptionJob":{"TranscriptionJobStatus":"COMPLETED",
                "Transcript":{"TranscriptFileUri":"https://s3.invalid/t.json"}}}"#,
        ));
        match state {
            JobState::Done(uri) => assert_eq!(uri, "https://s3.invalid/t.json"),
            _ => panic!("expected completion"),
        }
    }

    #[test]
    fn in_progress_keeps_polling() {
        assert!(matches!(
            classify(&job(
                r#"{"TranscriptionJob":{"TranscriptionJobStatus":"IN_PROGRESS"}}"#
            )),
            JobState::Pending
        ));
        assert!(matches!(
            classify(&job(
                r#"{"TranscriptionJob":{"TranscriptionJobStatus":"QUEUED"}}"#
            )),
            JobState::Pending
        ));
    }

    #[test]
    fn a_failed_job_carries_the_reason() {
        let state = classify(&job(
            r#"{"TranscriptionJob":{"TranscriptionJobStatus":"FAILED",
                "FailureReason":"The audio format is not supported."}}"#,
        ));
        match state {
            JobState::Failed(reason) => assert!(reason.contains("not supported"), "{reason}"),
            _ => panic!("expected failure"),
        }
    }

    /// Completing with no URI would otherwise poll to the timeout and report a
    /// misleading "did not finish".
    #[test]
    fn completed_without_a_uri_fails_rather_than_hanging() {
        let state = classify(&job(
            r#"{"TranscriptionJob":{"TranscriptionJobStatus":"COMPLETED"}}"#,
        ));
        assert!(matches!(state, JobState::Failed(_)));
    }

    #[test]
    fn reads_the_transcript_document() {
        let document: TranscriptDocument = serde_json::from_str(
            r#"{"jobName":"x","results":{"transcripts":[{"transcript":"run cargo clippy"}]}}"#,
        )
        .expect("valid transcript document");
        assert_eq!(joined_transcript(&document), "run cargo clippy");
    }

    #[test]
    fn silence_yields_an_empty_transcript() {
        let document: TranscriptDocument =
            serde_json::from_str(r#"{"results":{"transcripts":[]}}"#).expect("valid");
        assert_eq!(joined_transcript(&document), "");
    }

    #[test]
    fn browser_containers_map_to_transcribe_media_formats() {
        assert_eq!(media_format_for("audio/webm;codecs=opus"), Some("webm"));
        assert_eq!(media_format_for("audio/mp4"), Some("mp4"));
        assert_eq!(media_format_for("application/octet-stream"), None);
    }

    #[test]
    fn aws_requires_a_staging_bucket() {
        let env = SttEnv {
            region: Some("us-east-1".into()),
            ..Default::default()
        };
        let err = crate::config_error(AwsStt::from_env(&env));
        assert!(err.contains("PORTAL_STT_BUCKET"), "{err}");
    }

    #[test]
    fn the_object_url_is_region_and_bucket_scoped() {
        let provider = AwsStt::from_env(&SttEnv {
            region: Some("eu-west-1".into()),
            bucket: Some("my-bucket".into()),
            ..Default::default()
        })
        .expect("configured");
        assert_eq!(
            provider.object_url("agent-portal-stt/x.webm"),
            "https://my-bucket.s3.eu-west-1.amazonaws.com/agent-portal-stt/x.webm"
        );
    }
}

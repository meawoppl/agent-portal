//! Driver for providers that transcribe as an asynchronous *job*.
//!
//! AssemblyAI, Rev AI, Speechmatics and Amazon Transcribe all answer a submit
//! with an id and make you ask again until the work is done. That shape is
//! identical across the four, so it lives here once: the provider supplies a
//! closure that reports the job's state, and this decides how often to ask and
//! when to give up.
//!
//! The wait is bounded because it happens inside an HTTP request the browser is
//! blocking on. A voice utterance is seconds of audio and these providers
//! typically finish in a few seconds; [`DEFAULT_TIMEOUT`] is the point past
//! which something is wrong and a clear error beats a hung request.

use std::time::Duration;

use crate::SttError;

/// First gap between polls. Short, because a short utterance is often ready
/// almost immediately and the first poll frequently succeeds.
const INITIAL_INTERVAL: Duration = Duration::from_millis(400);
/// Ceiling for the backoff, so a slow job doesn't get polled ever-less-often
/// into the timeout.
const MAX_INTERVAL: Duration = Duration::from_secs(2);
/// Total budget before giving up.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);

/// What one probe of a job found.
pub(crate) enum JobState<T> {
    /// Still working — ask again.
    Pending,
    Done(T),
    /// The provider says this job will never finish.
    Failed(String),
}

/// Poll `probe` until it reports done, fails, or the budget runs out.
///
/// `label` names the provider in the timeout message; without it a timeout is
/// indistinguishable across providers in a log.
pub(crate) async fn poll_job<T, F, Fut>(
    label: &str,
    timeout: Duration,
    mut probe: F,
) -> Result<T, SttError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<JobState<T>, SttError>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let mut interval = INITIAL_INTERVAL;

    loop {
        match probe().await? {
            JobState::Done(value) => return Ok(value),
            JobState::Failed(reason) => {
                return Err(SttError::Provider(format!("{label} job failed: {reason}")))
            }
            JobState::Pending => {}
        }

        let now = tokio::time::Instant::now();
        if now + interval >= deadline {
            return Err(SttError::Provider(format!(
                "{label} did not finish within {}s",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(interval).await;
        interval = (interval * 2).min(MAX_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(start_paused = true)]
    async fn returns_the_value_once_the_job_completes() {
        let calls = AtomicUsize::new(0);
        let result = poll_job("test", DEFAULT_TIMEOUT, || async {
            Ok(if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                JobState::Pending
            } else {
                JobState::Done("transcript".to_string())
            })
        })
        .await
        .expect("job completes");

        assert_eq!(result, "transcript");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "should poll until done");
    }

    /// A provider-reported failure must surface immediately rather than being
    /// retried until the timeout.
    #[tokio::test(start_paused = true)]
    async fn a_failed_job_stops_at_once() {
        let calls = AtomicUsize::new(0);
        let err = poll_job("test", DEFAULT_TIMEOUT, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<JobState<String>, SttError>(JobState::Failed("bad audio".to_string()))
        })
        .await
        .expect_err("failure propagates");

        assert!(err.to_string().contains("bad audio"), "{err}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "must not retry a failure");
    }

    #[tokio::test(start_paused = true)]
    async fn a_job_that_never_finishes_times_out() {
        let err = poll_job("test", Duration::from_secs(5), || async {
            Ok::<JobState<String>, SttError>(JobState::Pending)
        })
        .await
        .expect_err("times out");
        assert!(err.to_string().contains("did not finish"), "{err}");
    }

    /// A transport error inside the probe is the caller's, not a job failure.
    #[tokio::test(start_paused = true)]
    async fn probe_errors_propagate_unchanged() {
        let err = poll_job("test", DEFAULT_TIMEOUT, || async {
            Err::<JobState<String>, _>(SttError::Transport("connection reset".to_string()))
        })
        .await
        .expect_err("probe error propagates");
        assert!(matches!(err, SttError::Transport(_)), "{err}");
    }
}
